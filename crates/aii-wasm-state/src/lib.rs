//! # aii-wasm-state
//!
//! Bridge crate joining the sub-chain VM ([`aii_wasm`]) to the chain's
//! persistent state ([`aii_state::StateDb`]).
//!
//! - [`StateDbHost`] — thin wrapper implementing [`aii_wasm::HostState`].
//!   Pass it to [`aii_wasm::WasmRuntime::call_with_host`] and contracts
//!   reading a slot via `env.storage_read` see the value persisted by
//!   prior transactions.
//! - [`commit_effects`] — apply the [`aii_wasm::HostEffects`] returned by
//!   a successful call back to the underlying `StateDb`. Failed calls
//!   produce no effects (`call_with_host` drops them on the error path),
//!   so reverts are automatic — the caller never sees effects to commit.
//!
//! This crate intentionally has no logic of its own: it is the smallest
//! adapter that lets the VM and the state store cooperate without
//! making either depend on the other.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use aii_state::{StateDb, StateError};
use aii_storage::KvBackend;
use aii_types::{Address, H256};
use aii_wasm::{HostEffects, HostState};
use std::sync::Arc;

/// Adapter wrapping a shared [`StateDb`] and implementing
/// [`HostState`] so contracts can read persisted storage.
///
/// Wraps `Arc<StateDb<B>>` so the same handle can be shared across
/// transactions without copying the underlying store.
pub struct StateDbHost<B: KvBackend> {
    db: Arc<StateDb<B>>,
}

impl<B: KvBackend> StateDbHost<B> {
    /// Construct from a shared `StateDb` handle.
    pub const fn new(db: Arc<StateDb<B>>) -> Self {
        Self { db }
    }

    /// Borrow the underlying `StateDb` for direct access (e.g. to issue
    /// the explicit commit via [`commit_effects`]).
    pub const fn db(&self) -> &Arc<StateDb<B>> {
        &self.db
    }
}

impl<B: KvBackend> HostState for StateDbHost<B> {
    fn storage_get(&self, addr: &Address, slot: &H256) -> H256 {
        // The trait surface returns H256 directly (no error channel).
        // Storage decode errors collapse to ZERO; the chain's
        // verification layer rejects any block whose state introduced
        // a malformed slot before we'd ever see one here.
        self.db.storage_get(addr, slot).unwrap_or(H256::ZERO)
    }
}

/// Apply the storage writes from `effects` to `db`.
///
/// `effects.logs` are intentionally NOT consumed here — logs are a
/// chain-event surface, not state, and the caller (typically the EVM
/// or sub-chain executor that owns the receipt index) chooses where
/// they land.
pub fn commit_effects<B: KvBackend>(
    db: &StateDb<B>,
    effects: &HostEffects,
) -> Result<(), StateError> {
    for (addr, slot, value) in &effects.storage_writes {
        db.storage_put(addr, slot, value)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aii_storage::MemoryBackend;
    use aii_wasm::{CallContext, WasmRuntime};
    use std::sync::Arc as StdArc;

    fn fresh_db() -> Arc<StateDb<MemoryBackend>> {
        Arc::new(StateDb::new(StdArc::new(MemoryBackend::new())))
    }

    fn slot_with_last_byte(b: u8) -> H256 {
        let mut s = [0u8; 32];
        s[31] = b;
        H256::new(s)
    }

    fn callee() -> Address {
        Address::new([0xdd; 20])
    }

    fn ctx() -> CallContext {
        CallContext {
            caller: Address::new([0xcc; 20]),
            callee: callee(),
            block_number: 100,
            block_timestamp: 1_700_000_000,
        }
    }

    /// WAT: read slot 0x07 into memory[64..96], return last byte.
    const READ_SLOT7_WAT: &str = r#"
        (module
          (import "env" "storage_read" (func $sr (param i32 i32)))
          (memory (export "memory") 1)
          (func (export "read") (result i32)
            (i32.store8 (i32.const 31) (i32.const 0x07))
            (call $sr (i32.const 0) (i32.const 64))
            (i32.load8_u (i32.const 95))))
    "#;

    /// WAT: write slot 0x03 = 0x55 then 0x04 = 0x66.
    const WRITE_TWO_WAT: &str = r#"
        (module
          (import "env" "storage_write" (func $sw (param i32 i32)))
          (memory (export "memory") 1)
          (func (export "go") (result i32)
            (i32.store8 (i32.const 31) (i32.const 0x03))
            (i32.store8 (i32.const 63) (i32.const 0x55))
            (call $sw (i32.const 0) (i32.const 32))
            (i32.store8 (i32.const 31) (i32.const 0x04))
            (i32.store8 (i32.const 63) (i32.const 0x66))
            (call $sw (i32.const 0) (i32.const 32))
            (i32.const 0)))
    "#;

    #[test]
    fn host_state_returns_zero_for_unwritten_slot() {
        let db = fresh_db();
        let host = StateDbHost::new(db);
        let val = host.storage_get(&callee(), &slot_with_last_byte(0xff));
        assert_eq!(val, H256::ZERO);
    }

    #[test]
    fn host_state_returns_persisted_value() {
        let db = fresh_db();
        let addr = callee();
        let slot = slot_with_last_byte(0x07);
        let value = slot_with_last_byte(0x99);
        db.storage_put(&addr, &slot, &value).unwrap();
        let host = StateDbHost::new(db);
        assert_eq!(host.storage_get(&addr, &slot), value);
    }

    #[test]
    fn host_state_per_address_isolation() {
        let db = fresh_db();
        let a = Address::new([0x01; 20]);
        let b = Address::new([0x02; 20]);
        let slot = slot_with_last_byte(0x01);
        db.storage_put(&a, &slot, &slot_with_last_byte(0xaa))
            .unwrap();
        let host = StateDbHost::new(db);
        assert_eq!(host.storage_get(&a, &slot), slot_with_last_byte(0xaa));
        assert_eq!(host.storage_get(&b, &slot), H256::ZERO);
    }

    #[test]
    fn commit_effects_empty_is_noop() {
        let db = StateDb::new(StdArc::new(MemoryBackend::new()));
        commit_effects(&db, &HostEffects::default()).unwrap();
        assert_eq!(
            db.storage_get(&callee(), &slot_with_last_byte(0x00))
                .unwrap(),
            H256::ZERO,
        );
    }

    #[test]
    fn commit_effects_persists_writes() {
        let db = StateDb::new(StdArc::new(MemoryBackend::new()));
        let addr = callee();
        let mut effects = HostEffects::default();
        effects
            .storage_writes
            .push((addr, slot_with_last_byte(0x01), slot_with_last_byte(0xaa)));
        effects
            .storage_writes
            .push((addr, slot_with_last_byte(0x02), slot_with_last_byte(0xbb)));
        commit_effects(&db, &effects).unwrap();
        assert_eq!(
            db.storage_get(&addr, &slot_with_last_byte(0x01)).unwrap(),
            slot_with_last_byte(0xaa),
        );
        assert_eq!(
            db.storage_get(&addr, &slot_with_last_byte(0x02)).unwrap(),
            slot_with_last_byte(0xbb),
        );
    }

    #[test]
    fn wasm_contract_reads_persisted_value_through_bridge() {
        let db = fresh_db();
        let slot = slot_with_last_byte(0x07);
        db.storage_put(&callee(), &slot, &slot_with_last_byte(0x99))
            .unwrap();

        let rt = WasmRuntime::new().unwrap();
        let module = rt
            .compile(&wat::parse_str(READ_SLOT7_WAT).unwrap())
            .unwrap();
        let host = StateDbHost::new(db);
        let out = rt
            .call_with_host(&module, 1_000_000, "read", &[], ctx(), host)
            .unwrap();
        assert_eq!(out.return_value, 0x99);
    }

    #[test]
    fn wasm_contract_write_then_commit_persists_to_state() {
        let db = fresh_db();
        let rt = WasmRuntime::new().unwrap();
        let module = rt.compile(&wat::parse_str(WRITE_TWO_WAT).unwrap()).unwrap();
        let host = StateDbHost::new(db.clone());
        let out = rt
            .call_with_host(&module, 1_000_000, "go", &[], ctx(), host)
            .unwrap();
        // Before commit, db has nothing.
        assert_eq!(
            db.storage_get(&callee(), &slot_with_last_byte(0x03))
                .unwrap(),
            H256::ZERO,
        );
        // Commit and check both slots persisted under the callee.
        commit_effects(&db, &out.effects).unwrap();
        assert_eq!(
            db.storage_get(&callee(), &slot_with_last_byte(0x03))
                .unwrap(),
            slot_with_last_byte(0x55),
        );
        assert_eq!(
            db.storage_get(&callee(), &slot_with_last_byte(0x04))
                .unwrap(),
            slot_with_last_byte(0x66),
        );
    }

    /// WAT for the round-trip test: read slot 0x03 (the slot the writer
    /// populated) and return its last byte.
    const READ_SLOT3_WAT: &str = r#"
        (module
          (import "env" "storage_read" (func $sr (param i32 i32)))
          (memory (export "memory") 1)
          (func (export "read") (result i32)
            (i32.store8 (i32.const 31) (i32.const 0x03))
            (call $sr (i32.const 0) (i32.const 64))
            (i32.load8_u (i32.const 95))))
    "#;

    #[test]
    fn round_trip_write_commit_read_sees_value_in_second_call() {
        let db = fresh_db();
        let rt = WasmRuntime::new().unwrap();
        // First call: write slot 0x03 = 0x55, slot 0x04 = 0x66.
        let writer = rt.compile(&wat::parse_str(WRITE_TWO_WAT).unwrap()).unwrap();
        let host1 = StateDbHost::new(db.clone());
        let out = rt
            .call_with_host(&writer, 1_000_000, "go", &[], ctx(), host1)
            .unwrap();
        commit_effects(&db, &out.effects).unwrap();

        // Second call reads slot 0x03 — the bridge surfaces the committed value.
        let reader = rt
            .compile(&wat::parse_str(READ_SLOT3_WAT).unwrap())
            .unwrap();
        let host2 = StateDbHost::new(db);
        let out2 = rt
            .call_with_host(&reader, 1_000_000, "read", &[], ctx(), host2)
            .unwrap();
        assert_eq!(out2.return_value, 0x55);
    }
}
