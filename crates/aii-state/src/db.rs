//! `StateDb` — `Address → Account` store backed by [`aii_storage::KvBackend`].

use crate::{account::Account, error::StateError};
use aii_crypto::keccak::keccak256;
use aii_storage::{ColumnFamily, KvBackend};
use aii_types::Address;
use alloy_rlp::{Decodable, Encodable};
use std::sync::Arc;

/// KV-backed world-state store keyed by `Address`.
///
/// Keys in `ColumnFamily::State` are `keccak256(address)` — matches Ethereum
/// state-trie key derivation.
pub struct StateDb<B: KvBackend> {
    backend: Arc<B>,
}

impl<B: KvBackend> StateDb<B> {
    /// Construct from a shared backend.
    pub const fn new(backend: Arc<B>) -> Self {
        Self { backend }
    }

    fn key(addr: &Address) -> [u8; 32] {
        *keccak256(addr.as_bytes()).as_bytes()
    }

    /// Return the account at `addr`, or `None` if no record exists.
    pub fn account(&self, addr: &Address) -> Result<Option<Account>, StateError> {
        let key = Self::key(addr);
        let Some(bytes) = self.backend.get(ColumnFamily::State, &key)? else {
            return Ok(None);
        };
        let mut s: &[u8] = &bytes;
        let acc = Account::decode(&mut s)?;
        Ok(Some(acc))
    }

    /// Persist `account` at `addr`. Overwrites any existing record.
    pub fn set_account(&self, addr: &Address, account: &Account) -> Result<(), StateError> {
        let key = Self::key(addr);
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        account.encode(&mut buf);
        self.backend.put(ColumnFamily::State, &key, &buf)?;
        Ok(())
    }

    /// Delete the account at `addr` (no-op if it didn't exist).
    pub fn remove_account(&self, addr: &Address) -> Result<(), StateError> {
        let key = Self::key(addr);
        self.backend.delete(ColumnFamily::State, &key)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aii_storage::MemoryBackend;
    use aii_types::{H256, U256};

    fn fresh_db() -> StateDb<MemoryBackend> {
        StateDb::new(Arc::new(MemoryBackend::new()))
    }

    fn sample_account() -> Account {
        Account {
            nonce: 7,
            balance: U256::from(1_000_000u64),
            storage_root: H256::new([0x11; 32]),
            code_hash: H256::new([0x22; 32]),
        }
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let db = fresh_db();
        let addr = Address::new([0xab; 20]);
        assert_eq!(db.account(&addr).unwrap(), None);
    }

    #[test]
    fn set_then_get_round_trip() {
        let db = fresh_db();
        let addr = Address::new([0xab; 20]);
        let acc = sample_account();
        db.set_account(&addr, &acc).unwrap();
        assert_eq!(db.account(&addr).unwrap(), Some(acc));
    }

    #[test]
    fn set_then_remove_clears() {
        let db = fresh_db();
        let addr = Address::new([0xab; 20]);
        db.set_account(&addr, &sample_account()).unwrap();
        db.remove_account(&addr).unwrap();
        assert_eq!(db.account(&addr).unwrap(), None);
    }

    #[test]
    fn two_addresses_isolated() {
        let db = fresh_db();
        let a = Address::new([0x01; 20]);
        let b = Address::new([0x02; 20]);
        let mut acc_a = sample_account();
        acc_a.nonce = 1;
        let mut acc_b = sample_account();
        acc_b.nonce = 2;
        db.set_account(&a, &acc_a).unwrap();
        db.set_account(&b, &acc_b).unwrap();
        assert_eq!(db.account(&a).unwrap().unwrap().nonce, 1);
        assert_eq!(db.account(&b).unwrap().unwrap().nonce, 2);
    }
}
