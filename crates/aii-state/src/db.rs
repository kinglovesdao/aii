//! `StateDb` — `Address → Account` store backed by [`aii_storage::KvBackend`].

use crate::{account::Account, error::StateError, trie::mpt_root};
use aii_crypto::keccak::keccak256;
use aii_storage::{ColumnFamily, KvBackend};
use aii_types::{Address, H256};
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

    /// Fetch contract bytecode by `code_hash`. Returns `None` if no
    /// code has ever been stored under that hash.
    ///
    /// AII stores bytecode in the [`ColumnFamily::Code`] CF — content-
    /// addressed by `keccak256(bytecode)`, naturally deduplicated.
    pub fn code_get(&self, code_hash: &aii_types::H256) -> Result<Option<Vec<u8>>, StateError> {
        Ok(self.backend.get(ColumnFamily::Code, code_hash.as_bytes())?)
    }

    /// Persist contract bytecode under `code_hash`. Idempotent: the
    /// same bytecode under the same hash is a no-op write.
    pub fn code_put(&self, code_hash: &aii_types::H256, bytes: &[u8]) -> Result<(), StateError> {
        self.backend
            .put(ColumnFamily::Code, code_hash.as_bytes(), bytes)?;
        Ok(())
    }

    /// Read one storage slot for a given contract address. Returns
    /// `H256::ZERO` for any slot that was never written (matches EVM
    /// semantics — unset slots read as zero).
    pub fn storage_get(
        &self,
        addr: &Address,
        slot: &aii_types::H256,
    ) -> Result<aii_types::H256, StateError> {
        let key = storage_key(addr, slot);
        let bytes = self.backend.get(ColumnFamily::AccountStorage, &key)?;
        match bytes {
            Some(b) if b.len() == 32 => {
                let mut out = [0u8; 32];
                out.copy_from_slice(&b);
                Ok(aii_types::H256::new(out))
            }
            Some(b) => Err(StateError::Decode(format!(
                "storage slot value has unexpected length {}",
                b.len()
            ))),
            None => Ok(aii_types::H256::ZERO),
        }
    }

    /// Write one storage slot for a given contract address. Storing
    /// `H256::ZERO` clears the entry to match EVM semantics — readers
    /// of an unset slot get zero regardless of whether the row was
    /// deleted or never existed.
    pub fn storage_put(
        &self,
        addr: &Address,
        slot: &aii_types::H256,
        value: &aii_types::H256,
    ) -> Result<(), StateError> {
        let key = storage_key(addr, slot);
        if value == &aii_types::H256::ZERO {
            self.backend.delete(ColumnFamily::AccountStorage, &key)?;
        } else {
            self.backend
                .put(ColumnFamily::AccountStorage, &key, value.as_bytes())?;
        }
        Ok(())
    }

    /// Compute the Yellow-Paper-style world-state root by iterating
    /// every persisted account and folding it into an MPT over
    /// `(keccak256(address) → rlp(account))`.
    ///
    /// The store already keys each account by `keccak256(address)`, so
    /// the iteration order matches MPT ingestion order without any
    /// transformation.
    ///
    /// This is O(n) in the account count — fine for v0.0.41's testnet
    /// scale (hundreds of accounts); incremental per-block deltas land
    /// in the B-series releases.
    ///
    /// # Errors
    /// Returns [`StateError`] if the backend iterator yields an error
    /// or if a stored account fails to RLP-decode.
    pub fn state_root(&self) -> Result<H256, StateError> {
        let mut pairs: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        for kv in self.backend.iter(ColumnFamily::State) {
            let (k, v) = kv?;
            // Re-encode the account so the MPT value bytes are the
            // canonical RLP of `Account` rather than whatever the
            // backend chose to store.
            let mut s: &[u8] = &v;
            let acc = Account::decode(&mut s)?;
            let mut buf = alloy_rlp::bytes::BytesMut::new();
            acc.encode(&mut buf);
            pairs.push((k, buf.to_vec()));
        }
        Ok(mpt_root(pairs))
    }
}

/// `(address ‖ slot)` — 52-byte key for the [`ColumnFamily::AccountStorage`] CF.
///
/// A flat key is sufficient for now; future per-account storage tries
/// will replace this with a per-account root, but the public API stays
/// the same.
fn storage_key(addr: &Address, slot: &aii_types::H256) -> [u8; 52] {
    let mut out = [0u8; 52];
    out[..20].copy_from_slice(addr.as_bytes());
    out[20..].copy_from_slice(slot.as_bytes());
    out
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

    #[test]
    fn code_get_missing_returns_none() {
        let db = fresh_db();
        assert_eq!(db.code_get(&H256::new([0xaa; 32])).unwrap(), None);
    }

    #[test]
    fn code_put_then_get_round_trip() {
        let db = fresh_db();
        let bytecode = vec![0x60, 0x42, 0x60, 0x00, 0x55, 0x00];
        let hash = H256::new([0xab; 32]);
        db.code_put(&hash, &bytecode).unwrap();
        assert_eq!(db.code_get(&hash).unwrap(), Some(bytecode));
    }

    #[test]
    fn code_under_different_hashes_isolated() {
        let db = fresh_db();
        let h1 = H256::new([0x01; 32]);
        let h2 = H256::new([0x02; 32]);
        db.code_put(&h1, b"contract-1").unwrap();
        db.code_put(&h2, b"contract-2-longer").unwrap();
        assert_eq!(db.code_get(&h1).unwrap(), Some(b"contract-1".to_vec()));
        assert_eq!(
            db.code_get(&h2).unwrap(),
            Some(b"contract-2-longer".to_vec())
        );
    }

    #[test]
    fn storage_unset_slot_reads_zero() {
        let db = fresh_db();
        let addr = Address::new([0xcd; 20]);
        let slot = H256::new([0x77; 32]);
        assert_eq!(db.storage_get(&addr, &slot).unwrap(), H256::ZERO);
    }

    #[test]
    fn storage_put_then_get_round_trip() {
        let db = fresh_db();
        let addr = Address::new([0xcd; 20]);
        let slot = H256::new([0; 32]);
        let value = H256::new([0xaa; 32]);
        db.storage_put(&addr, &slot, &value).unwrap();
        assert_eq!(db.storage_get(&addr, &slot).unwrap(), value);
    }

    #[test]
    fn storage_zero_value_clears_slot() {
        let db = fresh_db();
        let addr = Address::new([0xcd; 20]);
        let slot = H256::new([0; 32]);
        db.storage_put(&addr, &slot, &H256::new([0xaa; 32]))
            .unwrap();
        db.storage_put(&addr, &slot, &H256::ZERO).unwrap();
        assert_eq!(db.storage_get(&addr, &slot).unwrap(), H256::ZERO);
    }

    #[test]
    fn state_root_empty_equals_empty_trie_hash() {
        let db = fresh_db();
        assert_eq!(db.state_root().unwrap(), crate::EMPTY_TRIE_HASH);
    }

    #[test]
    fn state_root_changes_when_account_changes() {
        let db = fresh_db();
        let alice = Address::new([0xa1; 20]);
        db.set_account(&alice, &sample_account()).unwrap();
        let r1 = db.state_root().unwrap();
        let mut updated = sample_account();
        updated.nonce += 1;
        db.set_account(&alice, &updated).unwrap();
        let r2 = db.state_root().unwrap();
        assert_ne!(r1, r2, "state_root must shift on account mutation");
    }

    #[test]
    fn state_root_independent_of_insert_order() {
        let a = Address::new([0xa1; 20]);
        let b = Address::new([0xb2; 20]);
        let db1 = fresh_db();
        db1.set_account(&a, &sample_account()).unwrap();
        db1.set_account(&b, &sample_account()).unwrap();
        let db2 = fresh_db();
        db2.set_account(&b, &sample_account()).unwrap();
        db2.set_account(&a, &sample_account()).unwrap();
        assert_eq!(db1.state_root().unwrap(), db2.state_root().unwrap());
    }

    #[test]
    fn storage_isolated_per_address_and_slot() {
        let db = fresh_db();
        let a = Address::new([0x01; 20]);
        let b = Address::new([0x02; 20]);
        let slot_0 = H256::new([0; 32]);
        let slot_1 = H256::new([1; 32]);
        let v1 = H256::new([0x11; 32]);
        let v2 = H256::new([0x22; 32]);
        let v3 = H256::new([0x33; 32]);
        db.storage_put(&a, &slot_0, &v1).unwrap();
        db.storage_put(&a, &slot_1, &v2).unwrap();
        db.storage_put(&b, &slot_0, &v3).unwrap();
        assert_eq!(db.storage_get(&a, &slot_0).unwrap(), v1);
        assert_eq!(db.storage_get(&a, &slot_1).unwrap(), v2);
        assert_eq!(db.storage_get(&b, &slot_0).unwrap(), v3);
    }
}
