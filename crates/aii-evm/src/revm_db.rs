//! Adapter that lets `revm` read from `aii-state::StateDb`.
//!
//! `revm::Database` is a *mutable* trait — every read can mutate the
//! adapter (typically by populating an internal cache). We implement
//! it directly here; mutations to the underlying `StateDb` happen only
//! when [`crate::execute_with_revm`] explicitly commits the post-tx
//! state back.
//!
//! ## Limitations (v0.0.16)
//! - **Storage trie** is not yet wired (`storage` returns `U256::ZERO`).
//!   This is fine for value-transfer / simple bytecode that doesn't
//!   read EVM storage, but a real ERC-20 etc. will need it.
//! - **Code lookup by hash** is not wired (`code_by_hash` returns empty
//!   bytecode). Contracts deployed in-memory during a single transaction
//!   still work because `revm` caches the deployed bytecode internally.
//! - **`block_hash`** returns a deterministic placeholder.
//!
//! These TODOs all need extra columns in `aii-storage` + per-account
//! storage tries in `aii-state` (planned for v0.0.17+).

use aii_state::StateDb;
use aii_storage::KvBackend;
use aii_types::Address as AiiAddress;
use revm::primitives::{AccountInfo, Address, Bytecode, B256, U256};
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

    fn code_by_hash(&mut self, _code_hash: B256) -> Result<Bytecode, Self::Error> {
        // v0.0.16: no on-chain code lookup yet. Contracts deployed in the
        // current transaction still execute because revm caches the body.
        Ok(Bytecode::new())
    }

    fn storage(&mut self, _address: Address, _index: U256) -> Result<U256, Self::Error> {
        // v0.0.16: per-account storage trie not yet wired. All slots read
        // as zero — correct for newly-created contracts.
        Ok(U256::ZERO)
    }

    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        // Deterministic placeholder; revm only needs this for BLOCKHASH opcode.
        let mut bytes = [0u8; 32];
        bytes[24..32].copy_from_slice(&number.to_be_bytes());
        Ok(B256::new(bytes))
    }
}
