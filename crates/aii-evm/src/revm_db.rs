//! Adapter that lets `revm` read from `aii-state::StateDb`.
//!
//! `revm::Database` is a *mutable* trait — every read can mutate the
//! adapter (typically by populating an internal cache). We implement
//! it directly here; mutations to the underlying `StateDb` happen only
//! when [`crate::execute_with_revm`] explicitly commits the post-tx
//! state back.
//!
//! ## v0.0.20
//!
//! Reads are now backed by real persistent storage:
//!
//! - `code_by_hash` consults [`StateDb::code_get`] — contracts deployed
//!   in earlier transactions can be CALLed in later ones.
//! - `storage` consults [`StateDb::storage_get`] — `SLOAD` returns the
//!   last persisted value for the (address, slot) pair.
//! - `block_hash` still returns a deterministic placeholder. Real
//!   block-hash lookup lands once `aii-node` exposes a header index.

use aii_state::StateDb;
use aii_storage::KvBackend;
use aii_types::{Address as AiiAddress, H256 as AiiH256};
use revm::primitives::{AccountInfo, Address, Bytecode, Bytes, B256, U256};
use revm::Database;
use std::convert::Infallible;
use std::sync::Arc;

/// `revm::Database` adapter backed by `aii-state::StateDb`.
pub struct RevmDb<B: KvBackend> {
    state: Arc<StateDb<B>>,
}

impl<B: KvBackend> RevmDb<B> {
    /// Construct from a shared `StateDb`.
    pub const fn new(state: Arc<StateDb<B>>) -> Self {
        Self { state }
    }

    /// Borrow the underlying `StateDb`.
    pub const fn state(&self) -> &Arc<StateDb<B>> {
        &self.state
    }
}

impl<B: KvBackend> Database for RevmDb<B> {
    type Error = Infallible;

    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        let aii_addr = AiiAddress::new(address.into_array());
        // StateDb errors collapse to None — the adapter must be infallible.
        let Ok(maybe_acc) = self.state.account(&aii_addr) else {
            return Ok(None);
        };
        Ok(maybe_acc.map(|a| AccountInfo {
            balance: a.balance,
            nonce: a.nonce,
            code_hash: B256::new(*a.code_hash.as_bytes()),
            // Bytecode lookup is deferred — revm calls `code_by_hash`
            // when it actually needs the body.
            code: None,
        }))
    }

    fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        let hash = AiiH256::new(*code_hash.as_slice().first_chunk::<32>().unwrap());
        let Ok(Some(bytes)) = self.state.code_get(&hash) else {
            return Ok(Bytecode::new());
        };
        // `new_raw` accepts already-validated bytecode without
        // re-jumpdest analysis at this call site; revm will re-analyse
        // internally as needed.
        Ok(Bytecode::new_raw(Bytes::from(bytes)))
    }

    fn storage(&mut self, address: Address, index: U256) -> Result<U256, Self::Error> {
        let aii_addr = AiiAddress::new(address.into_array());
        let slot = AiiH256::new(index.to_be_bytes::<32>());
        let Ok(value) = self.state.storage_get(&aii_addr, &slot) else {
            return Ok(U256::ZERO);
        };
        Ok(U256::from_be_bytes(*value.as_bytes()))
    }

    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        // Deterministic placeholder; revm only needs this for BLOCKHASH opcode.
        let mut bytes = [0u8; 32];
        bytes[24..32].copy_from_slice(&number.to_be_bytes());
        Ok(B256::new(bytes))
    }
}
