//! # aii-wasm
//!
//! WebAssembly sub-chain VM for AII. Backed by `wasmtime`.
//!
//! ## Scope (v0.0.19)
//!
//! - [`WasmRuntime`] — engine wrapper that owns a `wasmtime::Engine`
//!   configured for fuel-metered execution.
//! - [`WasmInstance`] — one instantiated module + its private `Store`,
//!   with a fuel budget set at instantiation.
//! - [`WasmInstance::call_i32`] — invoke an exported function whose
//!   signature is `i32, i32, … → i32`. Fuel is consumed; running out
//!   yields [`WasmError::OutOfFuel`].
//! - Module validation is delegated to wasmtime.
//!
//! ### Out of scope (deferred)
//!
//! - Richer signatures (i64, f32, multi-return) — land in v0.0.20+.
//! - Host imports (state read/write, log, transfer) — v0.0.20+.
//! - WASI / wasi-preview2 — explicitly never on the consensus path;
//!   they'd be added only for off-chain tooling.
//! - Module caching / AOT compilation — performance work, not behavior.
//!
//! ## Gas model
//!
//! AII uses wasmtime's [fuel] mechanism: one fuel unit per executed
//! instruction. Calling code allocates a budget per call, and the
//! instance refuses to make progress when the budget is exhausted. The
//! consensus layer is responsible for translating tx gas → WASM fuel
//! (1 gas = 1 fuel for now; tuneable later).
//!
//! [fuel]: https://docs.wasmtime.dev/api/wasmtime/struct.Config.html#method.consume_fuel

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod host;
pub use host::{
    CallContext, HostCallResult, HostEffects, HostState, WasmModule, MAX_ABORT_MSG_LEN,
};

use thiserror::Error;
use wasmtime::{Config, Engine, Instance, Module, Store, Val};

/// Errors produced by the WASM runtime.
#[derive(Debug, Error)]
pub enum WasmError {
    /// Engine could not be constructed (wasmtime config rejected).
    #[error("wasmtime engine construction failed: {0}")]
    Engine(String),

    /// Module bytes are invalid WebAssembly.
    #[error("invalid WASM module: {0}")]
    BadModule(String),

    /// Failed to instantiate the module (missing imports, link error, …).
    #[error("instantiation failed: {0}")]
    Instantiate(String),

    /// Exported function with the requested name does not exist.
    #[error("no exported function named {0:?}")]
    MissingExport(String),

    /// Exported function exists but does not match the call signature
    /// (e.g. caller passed 1 arg but the function takes 2).
    #[error("signature mismatch for {func}: {detail}")]
    SignatureMismatch {
        /// Function name that was invoked.
        func: String,
        /// What went wrong in plain English.
        detail: String,
    },

    /// The fuel budget for this call ran out mid-execution.
    #[error("out of fuel")]
    OutOfFuel,

    /// Any other trap or runtime failure raised by wasmtime.
    #[error("wasm trap: {0}")]
    Trap(String),

    /// Failed to set fuel on the Store (wasmtime returned an error).
    #[error("fuel control failed: {0}")]
    FuelControl(String),

    /// Contract called `env.abort(msg, len)` and reverted with the
    /// supplied message (truncated to [`MAX_ABORT_MSG_LEN`] bytes).
    #[error("contract aborted: {0}")]
    Aborted(String),
}

/// Owner of the wasmtime engine. Re-use across many modules.
pub struct WasmRuntime {
    engine: Engine,
}

impl WasmRuntime {
    /// Construct a runtime with fuel metering enabled.
    pub fn new() -> Result<Self, WasmError> {
        let mut cfg = Config::new();
        cfg.consume_fuel(true);
        let engine = Engine::new(&cfg).map_err(|e| WasmError::Engine(e.to_string()))?;
        Ok(Self { engine })
    }

    /// Borrow the underlying wasmtime [`Engine`] — used by the host
    /// module to build module-local linkers without re-instantiating
    /// the engine.
    pub(crate) const fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Compile + instantiate a WebAssembly binary with `fuel` units
    /// budgeted for *all* calls made through the returned instance.
    ///
    /// Module bytes must be valid WebAssembly; the binary is validated
    /// by wasmtime during compilation.
    pub fn instantiate(&self, wasm: &[u8], fuel: u64) -> Result<WasmInstance, WasmError> {
        let module =
            Module::new(&self.engine, wasm).map_err(|e| WasmError::BadModule(e.to_string()))?;
        let mut store = Store::new(&self.engine, ());
        store
            .set_fuel(fuel)
            .map_err(|e| WasmError::FuelControl(e.to_string()))?;
        let instance = Instance::new(&mut store, &module, &[])
            .map_err(|e| WasmError::Instantiate(e.to_string()))?;
        Ok(WasmInstance { store, instance })
    }
}

/// One instantiated module bound to its own `Store` + fuel pool.
pub struct WasmInstance {
    store: Store<()>,
    instance: Instance,
}

impl std::fmt::Debug for WasmInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmInstance").finish_non_exhaustive()
    }
}

impl WasmInstance {
    /// Invoke an exported function with `i32` parameters and a single
    /// `i32` result. Returns [`WasmError::OutOfFuel`] if the call ran
    /// out of fuel mid-execution.
    pub fn call_i32(&mut self, name: &str, args: &[i32]) -> Result<i32, WasmError> {
        let func = self
            .instance
            .get_func(&mut self.store, name)
            .ok_or_else(|| WasmError::MissingExport(name.to_string()))?;

        let ty = func.ty(&self.store);
        if ty.params().len() != args.len() {
            return Err(WasmError::SignatureMismatch {
                func: name.to_string(),
                detail: format!("expected {} args, got {}", ty.params().len(), args.len()),
            });
        }
        if ty.results().len() != 1 {
            return Err(WasmError::SignatureMismatch {
                func: name.to_string(),
                detail: format!("expected 1 result, function has {}", ty.results().len()),
            });
        }

        let vals: Vec<Val> = args.iter().copied().map(Val::I32).collect();
        let mut results = [Val::I32(0)];
        match func.call(&mut self.store, &vals, &mut results) {
            Ok(()) => match results[0] {
                Val::I32(v) => Ok(v),
                other => Err(WasmError::SignatureMismatch {
                    func: name.to_string(),
                    detail: format!("result was {other:?}, not i32"),
                }),
            },
            Err(e) => Err(classify_trap(name, &e)),
        }
    }

    /// Fuel remaining in the store, in wasmtime units.
    pub fn fuel_remaining(&mut self) -> Result<u64, WasmError> {
        self.store
            .get_fuel()
            .map_err(|e| WasmError::FuelControl(e.to_string()))
    }
}

pub(crate) fn classify_trap(func: &str, err: &wasmtime::Error) -> WasmError {
    // wasmtime distinguishes traps via downcast.
    if let Some(trap) = err.downcast_ref::<wasmtime::Trap>() {
        if matches!(trap, wasmtime::Trap::OutOfFuel) {
            return WasmError::OutOfFuel;
        }
        return WasmError::Trap(format!("{func}: {trap}"));
    }
    // Type-mismatch / argument errors land here too.
    let msg = err.to_string();
    if msg.contains("argument type mismatch") || (msg.contains("expected") && msg.contains("type"))
    {
        return WasmError::SignatureMismatch {
            func: func.to_string(),
            detail: msg,
        };
    }
    WasmError::Trap(format!("{func}: {msg}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tiny WAT module: `add(a, b) -> a + b` and `loop_forever() -> i32`.
    const TEST_WAT: &str = r#"
        (module
          (func (export "add") (param i32 i32) (result i32)
            (i32.add (local.get 0) (local.get 1)))
          (func (export "loop_forever") (result i32)
            (loop $l (br $l))
            (i32.const 0)))
    "#;

    fn compiled() -> Vec<u8> {
        wat::parse_str(TEST_WAT).expect("test WAT must parse")
    }

    #[test]
    fn runtime_constructs() {
        let _rt = WasmRuntime::new().unwrap();
    }

    #[test]
    fn instantiate_valid_module() {
        let rt = WasmRuntime::new().unwrap();
        let _inst = rt.instantiate(&compiled(), 1_000_000).unwrap();
    }

    #[test]
    fn instantiate_invalid_module_rejected() {
        let rt = WasmRuntime::new().unwrap();
        let err = rt
            .instantiate(b"\x00\x61\x73\x6dGARBAGE", 1_000)
            .unwrap_err();
        assert!(
            matches!(err, WasmError::BadModule(_)),
            "expected BadModule, got {err:?}",
        );
    }

    #[test]
    fn call_add_returns_sum() {
        let rt = WasmRuntime::new().unwrap();
        let mut inst = rt.instantiate(&compiled(), 1_000_000).unwrap();
        let result = inst.call_i32("add", &[2, 3]).unwrap();
        assert_eq!(result, 5);
    }

    #[test]
    fn call_add_with_negatives() {
        let rt = WasmRuntime::new().unwrap();
        let mut inst = rt.instantiate(&compiled(), 1_000_000).unwrap();
        let result = inst.call_i32("add", &[-10, 3]).unwrap();
        assert_eq!(result, -7);
    }

    #[test]
    fn missing_export_rejected() {
        let rt = WasmRuntime::new().unwrap();
        let mut inst = rt.instantiate(&compiled(), 1_000_000).unwrap();
        let err = inst.call_i32("does_not_exist", &[]).unwrap_err();
        assert!(matches!(&err, WasmError::MissingExport(name) if name == "does_not_exist"));
    }

    #[test]
    fn wrong_arity_rejected() {
        let rt = WasmRuntime::new().unwrap();
        let mut inst = rt.instantiate(&compiled(), 1_000_000).unwrap();
        // `add` takes 2 args; we pass 1.
        let err = inst.call_i32("add", &[1]).unwrap_err();
        assert!(
            matches!(&err, WasmError::SignatureMismatch { func, .. } if func == "add"),
            "expected SignatureMismatch, got {err:?}",
        );
    }

    #[test]
    fn fuel_decreases_with_execution() {
        let rt = WasmRuntime::new().unwrap();
        let mut inst = rt.instantiate(&compiled(), 1_000_000).unwrap();
        let before = inst.fuel_remaining().unwrap();
        inst.call_i32("add", &[1, 2]).unwrap();
        let after = inst.fuel_remaining().unwrap();
        assert!(
            after < before,
            "fuel should decrease (before={before}, after={after})"
        );
    }

    #[test]
    fn infinite_loop_runs_out_of_fuel() {
        let rt = WasmRuntime::new().unwrap();
        let mut inst = rt.instantiate(&compiled(), 10_000).unwrap();
        let err = inst.call_i32("loop_forever", &[]).unwrap_err();
        assert!(
            matches!(err, WasmError::OutOfFuel),
            "expected OutOfFuel, got {err:?}"
        );
    }
}
