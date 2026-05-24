//! Host imports for sub-chain contracts (v0.0.22).
//!
//! WASM contracts running on AII's sub-chain VM call back into the chain
//! through six `env.*` functions: read/write the contract's own storage,
//! emit a log, learn who called and who I am, and explicitly revert.
//!
//! All writes are **collected, not persisted**: storage writes accumulate
//! in an in-call overlay and are reported via [`HostEffects`] to the
//! caller, which commits or discards them. A contract revert (`abort`
//! host import, OOB pointer, out-of-fuel) drops everything.
//!
//! ## Host imports
//!
//! | Name | Signature | Behaviour |
//! |---|---|---|
//! | `env.storage_read` | `(slot_ptr, out_ptr)` | reads 32 bytes of slot key, writes 32-byte value into `out_ptr` (overlay first, then [`HostState::storage_get`]) |
//! | `env.storage_write` | `(slot_ptr, value_ptr)` | writes a 32-byte value to the overlay under the 32-byte slot key |
//! | `env.caller` | `(out_ptr)` | writes the 20-byte caller address |
//! | `env.self_address` | `(out_ptr)` | writes the 20-byte callee address |
//! | `env.log` | `(data_ptr, data_len)` | appends `data_len` bytes from memory to [`HostEffects::logs`] |
//! | `env.abort` | `(msg_ptr, msg_len)` | records a revert message (≤256 bytes) and traps |
//!
//! Cross-contract reads/writes, native transfers, and block-context
//! accessors are explicit non-goals for v0.0.22 — they land alongside
//! sub-chain ↔ EVM integration in a later release.

use crate::{classify_trap, WasmError, WasmRuntime};
use aii_types::{Address, H256};
use std::collections::BTreeMap;
use wasmtime::{Caller, Extern, Linker, Module, Store, Val};

/// Maximum number of bytes of an `abort` message that are surfaced via
/// [`WasmError::Aborted`]. Longer messages are silently truncated.
pub const MAX_ABORT_MSG_LEN: usize = 256;

/// Execution context made available to a WASM contract via host imports.
///
/// The chain consensus layer constructs this per transaction. `caller`
/// is the address that initiated the call; `callee` is the address of
/// the contract being executed (used as the implicit storage namespace).
#[derive(Clone, Debug)]
pub struct CallContext {
    /// Caller address (`msg.sender`-equivalent).
    pub caller: Address,
    /// Callee address — the contract being executed.
    pub callee: Address,
    /// Block number at the time of the call.
    pub block_number: u64,
    /// Block timestamp (unix seconds) at the time of the call.
    pub block_timestamp: u64,
}

/// Read-only view of persisted chain state from inside a host import.
///
/// Implementations are typically thin wrappers over `aii_state::StateDb`,
/// but the trait is deliberately tiny so tests can mock it without
/// pulling in the storage stack.
pub trait HostState {
    /// Return the value at `(addr, slot)`, or `H256::ZERO` if no record
    /// exists. Implementations MUST NOT mutate.
    fn storage_get(&self, addr: &Address, slot: &H256) -> H256;
}

/// Side-effects accumulated by a successful host call.
///
/// Reverted calls (`abort`, OOB pointer, out-of-fuel) return no effects
/// — the error path drops them.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct HostEffects {
    /// `(callee, slot, value)` triples — the final state of every slot
    /// written during the call. Sorted by `slot` for determinism. Repeat
    /// writes to the same slot collapse to the last value.
    pub storage_writes: Vec<(Address, H256, H256)>,
    /// Log events emitted via `env.log`, in emission order. Each entry
    /// is the raw byte payload; structure is the contract's concern.
    pub logs: Vec<Vec<u8>>,
}

/// Result of [`WasmRuntime::call_with_host`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostCallResult {
    /// The `i32` returned by the exported function.
    pub return_value: i32,
    /// Side-effects to be committed by the caller.
    pub effects: HostEffects,
    /// Fuel remaining after the call.
    pub fuel_remaining: u64,
}

/// A compiled WebAssembly module, reusable across many calls.
pub struct WasmModule {
    pub(crate) inner: Module,
}

impl std::fmt::Debug for WasmModule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmModule").finish_non_exhaustive()
    }
}

/// Internal state attached to the `Store` while a host call executes.
#[allow(dead_code)] // fields are read indirectly via host imports
pub(crate) struct StoreData<S: HostState> {
    pub ctx: CallContext,
    pub overlay: BTreeMap<H256, H256>,
    pub logs: Vec<Vec<u8>>,
    pub aborted: Option<String>,
    pub host: S,
}

impl WasmRuntime {
    /// Compile a WebAssembly binary for repeated host-aware invocation.
    pub fn compile(&self, wasm: &[u8]) -> Result<WasmModule, WasmError> {
        let inner =
            Module::new(self.engine(), wasm).map_err(|e| WasmError::BadModule(e.to_string()))?;
        Ok(WasmModule { inner })
    }

    /// Invoke `name` on `module` with host imports linked.
    ///
    /// `fuel` budgets this single call. `host` is moved into the store
    /// for the duration of the call and dropped on return. On success
    /// returns [`HostCallResult`]; on failure all effects are discarded.
    ///
    /// Revert paths:
    /// - `env.abort(msg_ptr, msg_len)` → [`WasmError::Aborted`] with the
    ///   message (truncated to [`MAX_ABORT_MSG_LEN`] bytes).
    /// - Out-of-fuel → [`WasmError::OutOfFuel`].
    /// - OOB memory access in any host import → [`WasmError::Trap`].
    pub fn call_with_host<S: HostState + Send + Sync + 'static>(
        &self,
        module: &WasmModule,
        fuel: u64,
        name: &str,
        args: &[i32],
        ctx: CallContext,
        host: S,
    ) -> Result<HostCallResult, WasmError> {
        let data = StoreData {
            ctx,
            overlay: BTreeMap::new(),
            logs: Vec::new(),
            aborted: None,
            host,
        };
        let mut store = Store::new(self.engine(), data);
        store
            .set_fuel(fuel)
            .map_err(|e| WasmError::FuelControl(e.to_string()))?;

        let mut linker: Linker<StoreData<S>> = Linker::new(self.engine());
        register_host_imports(&mut linker)?;

        let instance = linker
            .instantiate(&mut store, &module.inner)
            .map_err(|e| WasmError::Instantiate(e.to_string()))?;

        let func = instance
            .get_func(&mut store, name)
            .ok_or_else(|| WasmError::MissingExport(name.to_string()))?;

        let ty = func.ty(&store);
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
        let call_result = func.call(&mut store, &vals, &mut results);
        let fuel_remaining = store.get_fuel().unwrap_or(0);
        let data = store.into_data();

        match call_result {
            Ok(()) => {
                let return_value = match results[0] {
                    Val::I32(v) => v,
                    other => {
                        return Err(WasmError::SignatureMismatch {
                            func: name.to_string(),
                            detail: format!("result was {other:?}, not i32"),
                        });
                    }
                };
                let callee = data.ctx.callee;
                let storage_writes: Vec<(Address, H256, H256)> = data
                    .overlay
                    .into_iter()
                    .map(|(slot, value)| (callee, slot, value))
                    .collect();
                Ok(HostCallResult {
                    return_value,
                    effects: HostEffects {
                        storage_writes,
                        logs: data.logs,
                    },
                    fuel_remaining,
                })
            }
            Err(e) => {
                if let Some(msg) = data.aborted {
                    return Err(WasmError::Aborted(msg));
                }
                Err(classify_trap(name, &e))
            }
        }
    }
}

/// Pull the exported `memory` out of the caller's instance, or trap if
/// the contract did not export one.
fn caller_memory<S: HostState>(
    caller: &mut Caller<'_, StoreData<S>>,
) -> Result<wasmtime::Memory, wasmtime::Error> {
    caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .ok_or_else(|| wasmtime::Error::msg("contract has no exported `memory`"))
}

/// Convert wasmtime registration errors into [`WasmError::Instantiate`].
fn reg<T>(r: wasmtime::Result<T>) -> Result<T, WasmError> {
    r.map_err(|e| WasmError::Instantiate(e.to_string()))
}

fn host_storage_read<S: HostState + Send + Sync + 'static>(
    mut caller: Caller<'_, StoreData<S>>,
    slot_ptr: i32,
    out_ptr: i32,
) -> Result<(), wasmtime::Error> {
    let mem = caller_memory(&mut caller)?;
    let mut slot_bytes = [0u8; 32];
    mem.read(&caller, slot_ptr as usize, &mut slot_bytes)
        .map_err(|e| wasmtime::Error::msg(format!("storage_read slot OOB: {e}")))?;
    let slot = H256::new(slot_bytes);
    let value = {
        let data = caller.data();
        data.overlay
            .get(&slot)
            .copied()
            .unwrap_or_else(|| data.host.storage_get(&data.ctx.callee, &slot))
    };
    mem.write(&mut caller, out_ptr as usize, value.as_bytes())
        .map_err(|e| wasmtime::Error::msg(format!("storage_read out OOB: {e}")))?;
    Ok(())
}

fn host_storage_write<S: HostState + Send + Sync + 'static>(
    mut caller: Caller<'_, StoreData<S>>,
    slot_ptr: i32,
    value_ptr: i32,
) -> Result<(), wasmtime::Error> {
    let mem = caller_memory(&mut caller)?;
    let mut slot_bytes = [0u8; 32];
    let mut value_bytes = [0u8; 32];
    mem.read(&caller, slot_ptr as usize, &mut slot_bytes)
        .map_err(|e| wasmtime::Error::msg(format!("storage_write slot OOB: {e}")))?;
    mem.read(&caller, value_ptr as usize, &mut value_bytes)
        .map_err(|e| wasmtime::Error::msg(format!("storage_write value OOB: {e}")))?;
    caller
        .data_mut()
        .overlay
        .insert(H256::new(slot_bytes), H256::new(value_bytes));
    Ok(())
}

fn host_caller_addr<S: HostState + Send + Sync + 'static>(
    mut caller: Caller<'_, StoreData<S>>,
    out_ptr: i32,
) -> Result<(), wasmtime::Error> {
    let mem = caller_memory(&mut caller)?;
    let addr = *caller.data().ctx.caller.as_bytes();
    mem.write(&mut caller, out_ptr as usize, &addr)
        .map_err(|e| wasmtime::Error::msg(format!("caller out OOB: {e}")))?;
    Ok(())
}

fn host_self_addr<S: HostState + Send + Sync + 'static>(
    mut caller: Caller<'_, StoreData<S>>,
    out_ptr: i32,
) -> Result<(), wasmtime::Error> {
    let mem = caller_memory(&mut caller)?;
    let addr = *caller.data().ctx.callee.as_bytes();
    mem.write(&mut caller, out_ptr as usize, &addr)
        .map_err(|e| wasmtime::Error::msg(format!("self_address out OOB: {e}")))?;
    Ok(())
}

fn host_log<S: HostState + Send + Sync + 'static>(
    mut caller: Caller<'_, StoreData<S>>,
    data_ptr: i32,
    data_len: i32,
) -> Result<(), wasmtime::Error> {
    let len = usize::try_from(data_len.max(0)).unwrap_or(0);
    let ptr = usize::try_from(data_ptr.max(0)).unwrap_or(0);
    let mem = caller_memory(&mut caller)?;
    let mut buf = vec![0u8; len];
    if len > 0 {
        mem.read(&caller, ptr, &mut buf)
            .map_err(|e| wasmtime::Error::msg(format!("log data OOB: {e}")))?;
    }
    caller.data_mut().logs.push(buf);
    Ok(())
}

fn host_abort<S: HostState + Send + Sync + 'static>(
    mut caller: Caller<'_, StoreData<S>>,
    msg_ptr: i32,
    msg_len: i32,
) -> Result<(), wasmtime::Error> {
    let raw_len = usize::try_from(msg_len.max(0)).unwrap_or(0);
    let len = raw_len.min(MAX_ABORT_MSG_LEN);
    let ptr = usize::try_from(msg_ptr.max(0)).unwrap_or(0);
    let mem = caller_memory(&mut caller)?;
    let mut buf = vec![0u8; len];
    if len > 0 {
        mem.read(&caller, ptr, &mut buf)
            .map_err(|e| wasmtime::Error::msg(format!("abort message OOB: {e}")))?;
    }
    let msg = String::from_utf8_lossy(&buf).into_owned();
    caller.data_mut().aborted = Some(msg);
    Err(wasmtime::Error::msg("contract aborted"))
}

/// Register the six `env.*` host imports against `linker`.
fn register_host_imports<S: HostState + Send + Sync + 'static>(
    linker: &mut Linker<StoreData<S>>,
) -> Result<(), WasmError> {
    reg(linker.func_wrap("env", "storage_read", host_storage_read::<S>))?;
    reg(linker.func_wrap("env", "storage_write", host_storage_write::<S>))?;
    reg(linker.func_wrap("env", "caller", host_caller_addr::<S>))?;
    reg(linker.func_wrap("env", "self_address", host_self_addr::<S>))?;
    reg(linker.func_wrap("env", "log", host_log::<S>))?;
    reg(linker.func_wrap("env", "abort", host_abort::<S>))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// In-memory `HostState` for tests.
    #[derive(Default, Clone)]
    struct MockHostState {
        storage: Arc<Mutex<HashMap<(Address, H256), H256>>>,
    }

    impl MockHostState {
        fn with_entry(addr: Address, slot: H256, value: H256) -> Self {
            let m = Self::default();
            m.storage.lock().unwrap().insert((addr, slot), value);
            m
        }
    }

    impl HostState for MockHostState {
        fn storage_get(&self, addr: &Address, slot: &H256) -> H256 {
            self.storage
                .lock()
                .unwrap()
                .get(&(*addr, *slot))
                .copied()
                .unwrap_or(H256::ZERO)
        }
    }

    fn ctx() -> CallContext {
        CallContext {
            caller: Address::new([0xcc; 20]),
            callee: Address::new([0xdd; 20]),
            block_number: 100,
            block_timestamp: 1_700_000_000,
        }
    }

    fn slot_with_last_byte(b: u8) -> H256 {
        let mut s = [0u8; 32];
        s[31] = b;
        H256::new(s)
    }

    fn value_with_last_byte(b: u8) -> H256 {
        slot_with_last_byte(b)
    }

    /// WAT: write slot 0x01 = 0x42, then read it back into memory, return last byte.
    const STORAGE_RW_WAT: &str = r#"
        (module
          (import "env" "storage_write" (func $sw (param i32 i32)))
          (import "env" "storage_read"  (func $sr (param i32 i32)))
          (memory (export "memory") 1)
          ;; layout: 0..32=slot, 32..64=value, 64..96=readback
          (func (export "write_then_read") (result i32)
            (i32.store8 (i32.const 31) (i32.const 0x01))
            (i32.store8 (i32.const 63) (i32.const 0x42))
            (call $sw (i32.const 0) (i32.const 32))
            (call $sr (i32.const 0) (i32.const 64))
            (i32.load8_u (i32.const 95))))
    "#;

    /// WAT: read a slot WITHOUT writing it first.
    const STORAGE_READ_ONLY_WAT: &str = r#"
        (module
          (import "env" "storage_read" (func $sr (param i32 i32)))
          (memory (export "memory") 1)
          (func (export "read_slot7") (result i32)
            (i32.store8 (i32.const 31) (i32.const 0x07))
            (call $sr (i32.const 0) (i32.const 64))
            (i32.load8_u (i32.const 95))))
    "#;

    /// WAT: write two different slots.
    const STORAGE_TWO_WRITES_WAT: &str = r#"
        (module
          (import "env" "storage_write" (func $sw (param i32 i32)))
          (memory (export "memory") 1)
          (func (export "do_writes") (result i32)
            (i32.store8 (i32.const 31) (i32.const 0x01))
            (i32.store8 (i32.const 63) (i32.const 0x42))
            (call $sw (i32.const 0) (i32.const 32))
            (i32.store8 (i32.const 31) (i32.const 0x02))
            (i32.store8 (i32.const 63) (i32.const 0x88))
            (call $sw (i32.const 0) (i32.const 32))
            (i32.const 0)))
    "#;

    /// WAT: write slot 1 = 0xAA then slot 1 = 0xBB (last-write wins).
    const STORAGE_WRITE_TWICE_SAME_SLOT_WAT: &str = r#"
        (module
          (import "env" "storage_write" (func $sw (param i32 i32)))
          (memory (export "memory") 1)
          (func (export "do") (result i32)
            (i32.store8 (i32.const 31) (i32.const 0x01))
            (i32.store8 (i32.const 63) (i32.const 0xAA))
            (call $sw (i32.const 0) (i32.const 32))
            (i32.store8 (i32.const 63) (i32.const 0xBB))
            (call $sw (i32.const 0) (i32.const 32))
            (i32.const 0)))
    "#;

    /// WAT: write caller into memory at offset 0, return last byte.
    const CALLER_WAT: &str = r#"
        (module
          (import "env" "caller" (func $c (param i32)))
          (memory (export "memory") 1)
          (func (export "who") (result i32)
            (call $c (i32.const 0))
            (i32.load8_u (i32.const 19))))
    "#;

    /// WAT: write self_address into memory at offset 0, return last byte.
    const SELF_ADDR_WAT: &str = r#"
        (module
          (import "env" "self_address" (func $s (param i32)))
          (memory (export "memory") 1)
          (func (export "me") (result i32)
            (call $s (i32.const 0))
            (i32.load8_u (i32.const 19))))
    "#;

    /// WAT: emit one log with bytes "hello".
    const LOG_HELLO_WAT: &str = r#"
        (module
          (import "env" "log" (func $log (param i32 i32)))
          (memory (export "memory") 1)
          (data (i32.const 0) "hello")
          (func (export "emit") (result i32)
            (call $log (i32.const 0) (i32.const 5))
            (i32.const 0)))
    "#;

    /// WAT: emit empty log.
    const LOG_EMPTY_WAT: &str = r#"
        (module
          (import "env" "log" (func $log (param i32 i32)))
          (memory (export "memory") 1)
          (func (export "emit") (result i32)
            (call $log (i32.const 0) (i32.const 0))
            (i32.const 0)))
    "#;

    /// WAT: abort with message "oops" (4 bytes).
    const ABORT_WAT: &str = r#"
        (module
          (import "env" "abort" (func $abort (param i32 i32)))
          (memory (export "memory") 1)
          (data (i32.const 0) "oops")
          (func (export "go") (result i32)
            (call $abort (i32.const 0) (i32.const 4))
            (i32.const 0)))
    "#;

    /// WAT: abort with a 500-byte message (truncation test).
    const ABORT_LONG_WAT: &str = r#"
        (module
          (import "env" "abort" (func $abort (param i32 i32)))
          (memory (export "memory") 1)
          (func (export "go") (result i32)
            ;; fill bytes 0..500 with 0x41 ('A')
            (local $i i32)
            (loop $l
              (i32.store8 (local.get $i) (i32.const 0x41))
              (local.set $i (i32.add (local.get $i) (i32.const 1)))
              (br_if $l (i32.lt_u (local.get $i) (i32.const 500))))
            (call $abort (i32.const 0) (i32.const 500))
            (i32.const 0)))
    "#;

    /// WAT: spin forever calling storage_write to drain fuel.
    const FUEL_DRAIN_WAT: &str = r#"
        (module
          (import "env" "storage_write" (func $sw (param i32 i32)))
          (memory (export "memory") 1)
          (func (export "spin") (result i32)
            (loop $l
              (call $sw (i32.const 0) (i32.const 32))
              (br $l))
            (i32.const 0)))
    "#;

    /// WAT: pass an obviously-OOB pointer to storage_read.
    const OOB_WAT: &str = r#"
        (module
          (import "env" "storage_read" (func $sr (param i32 i32)))
          (memory (export "memory") 1)
          (func (export "go") (result i32)
            (call $sr (i32.const 999999) (i32.const 0))
            (i32.const 0)))
    "#;

    fn wat(text: &str) -> Vec<u8> {
        wat::parse_str(text).expect("test WAT must parse")
    }

    fn rt_and_module(text: &str) -> (WasmRuntime, WasmModule) {
        let rt = WasmRuntime::new().unwrap();
        let m = rt.compile(&wat(text)).unwrap();
        (rt, m)
    }

    #[test]
    fn host_effects_default_is_empty() {
        let e = HostEffects::default();
        assert!(e.storage_writes.is_empty());
        assert!(e.logs.is_empty());
    }

    #[test]
    fn storage_write_then_read_returns_same_value() {
        let (rt, m) = rt_and_module(STORAGE_RW_WAT);
        let out = rt
            .call_with_host(
                &m,
                1_000_000,
                "write_then_read",
                &[],
                ctx(),
                MockHostState::default(),
            )
            .unwrap();
        // last byte of read-back = 0x42
        assert_eq!(out.return_value, 0x42);
    }

    #[test]
    fn storage_read_falls_through_to_host_state() {
        let (rt, m) = rt_and_module(STORAGE_READ_ONLY_WAT);
        // Pre-populate host state at slot 0x07 = 0x99.
        let host = MockHostState::with_entry(
            ctx().callee,
            slot_with_last_byte(0x07),
            value_with_last_byte(0x99),
        );
        let out = rt
            .call_with_host(&m, 1_000_000, "read_slot7", &[], ctx(), host)
            .unwrap();
        assert_eq!(out.return_value, 0x99);
    }

    #[test]
    fn storage_writes_collected_into_effects() {
        let (rt, m) = rt_and_module(STORAGE_TWO_WRITES_WAT);
        let out = rt
            .call_with_host(
                &m,
                1_000_000,
                "do_writes",
                &[],
                ctx(),
                MockHostState::default(),
            )
            .unwrap();
        assert_eq!(out.effects.storage_writes.len(), 2);
        // Sorted by slot ascending.
        assert_eq!(out.effects.storage_writes[0].1, slot_with_last_byte(0x01));
        assert_eq!(out.effects.storage_writes[0].2, value_with_last_byte(0x42));
        assert_eq!(out.effects.storage_writes[1].1, slot_with_last_byte(0x02));
        assert_eq!(out.effects.storage_writes[1].2, value_with_last_byte(0x88));
        // The callee is the implicit storage namespace.
        assert_eq!(out.effects.storage_writes[0].0, ctx().callee);
    }

    #[test]
    fn multiple_writes_to_same_slot_keep_last() {
        let (rt, m) = rt_and_module(STORAGE_WRITE_TWICE_SAME_SLOT_WAT);
        let out = rt
            .call_with_host(&m, 1_000_000, "do", &[], ctx(), MockHostState::default())
            .unwrap();
        assert_eq!(out.effects.storage_writes.len(), 1);
        assert_eq!(out.effects.storage_writes[0].2, value_with_last_byte(0xBB));
    }

    #[test]
    fn caller_writes_20_bytes_into_memory() {
        let (rt, m) = rt_and_module(CALLER_WAT);
        let out = rt
            .call_with_host(&m, 1_000_000, "who", &[], ctx(), MockHostState::default())
            .unwrap();
        // ctx().caller = [0xcc; 20], last byte = 0xcc.
        assert_eq!(out.return_value, 0xcc);
    }

    #[test]
    fn self_address_writes_20_bytes_into_memory() {
        let (rt, m) = rt_and_module(SELF_ADDR_WAT);
        let out = rt
            .call_with_host(&m, 1_000_000, "me", &[], ctx(), MockHostState::default())
            .unwrap();
        // ctx().callee = [0xdd; 20], last byte = 0xdd.
        assert_eq!(out.return_value, 0xdd);
    }

    #[test]
    fn log_appends_bytes_to_effects() {
        let (rt, m) = rt_and_module(LOG_HELLO_WAT);
        let out = rt
            .call_with_host(&m, 1_000_000, "emit", &[], ctx(), MockHostState::default())
            .unwrap();
        assert_eq!(out.effects.logs, vec![b"hello".to_vec()]);
    }

    #[test]
    fn log_with_zero_length_recorded_as_empty() {
        let (rt, m) = rt_and_module(LOG_EMPTY_WAT);
        let out = rt
            .call_with_host(&m, 1_000_000, "emit", &[], ctx(), MockHostState::default())
            .unwrap();
        assert_eq!(out.effects.logs, vec![Vec::<u8>::new()]);
    }

    #[test]
    fn abort_with_message_returns_aborted_error() {
        let (rt, m) = rt_and_module(ABORT_WAT);
        let err = rt
            .call_with_host(&m, 1_000_000, "go", &[], ctx(), MockHostState::default())
            .unwrap_err();
        match err {
            WasmError::Aborted(msg) => assert_eq!(msg, "oops"),
            other => panic!("expected Aborted, got {other:?}"),
        }
    }

    #[test]
    fn abort_message_truncated_at_max_len() {
        let (rt, m) = rt_and_module(ABORT_LONG_WAT);
        let err = rt
            .call_with_host(&m, 10_000_000, "go", &[], ctx(), MockHostState::default())
            .unwrap_err();
        match err {
            WasmError::Aborted(msg) => {
                assert_eq!(msg.len(), MAX_ABORT_MSG_LEN, "got {} bytes", msg.len());
                assert!(msg.chars().all(|c| c == 'A'));
            }
            other => panic!("expected Aborted, got {other:?}"),
        }
    }

    #[test]
    fn effects_isolated_per_call() {
        let (rt, m) = rt_and_module(STORAGE_TWO_WRITES_WAT);
        let a = rt
            .call_with_host(
                &m,
                1_000_000,
                "do_writes",
                &[],
                ctx(),
                MockHostState::default(),
            )
            .unwrap();
        let b = rt
            .call_with_host(
                &m,
                1_000_000,
                "do_writes",
                &[],
                ctx(),
                MockHostState::default(),
            )
            .unwrap();
        // Each call produces its own fresh effects; they don't accumulate.
        assert_eq!(a.effects.storage_writes.len(), 2);
        assert_eq!(b.effects.storage_writes.len(), 2);
        assert_eq!(a.effects.storage_writes, b.effects.storage_writes);
    }

    #[test]
    fn out_of_fuel_during_storage_loop_traps() {
        let (rt, m) = rt_and_module(FUEL_DRAIN_WAT);
        let err = rt
            .call_with_host(&m, 1_000, "spin", &[], ctx(), MockHostState::default())
            .unwrap_err();
        assert!(
            matches!(err, WasmError::OutOfFuel),
            "expected OutOfFuel, got {err:?}"
        );
    }

    #[test]
    fn oob_pointer_traps() {
        let (rt, m) = rt_and_module(OOB_WAT);
        let err = rt
            .call_with_host(&m, 1_000_000, "go", &[], ctx(), MockHostState::default())
            .unwrap_err();
        assert!(
            matches!(err, WasmError::Trap(_)),
            "expected Trap, got {err:?}"
        );
    }
}
