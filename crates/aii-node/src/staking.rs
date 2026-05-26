//! On-chain staking primitive (roadmap E.3).
//!
//! A staking record tracks how many Wei an address has bonded to the
//! validator-set election (C.6 future work) and, once `begin_unbond`
//! has been called, the block height at which those Wei become
//! withdrawable again.
//!
//! Records live in `ColumnFamily::Meta` under the key prefix
//! `b"stake:" ‖ address[20]`. Each record is a fixed 40-byte value
//! (32-byte U256 big-endian amount ‖ 8-byte big-endian unbond_height,
//! `0` meaning "still bonded"). The slashing executor (C.7) lands on
//! top of this primitive once DPoS rotation (C.6) is wired in.

use aii_storage::{ColumnFamily, KvBackend, RocksDbBackend};
use aii_types::{Address, U256};
use std::sync::Arc;

/// Persisted-record column-family key prefix.
const KEY_PREFIX: &[u8] = b"stake:";

/// A single validator's staking position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StakeRecord {
    /// Staker address (the principal who locked the Wei).
    pub staker: Address,
    /// Bonded amount in Wei. After `begin_unbond` this drops to 0
    /// and `unbond_at` tracks when the staker may sweep the original
    /// principal.
    pub amount_wei: U256,
    /// Block height at which the bond becomes withdrawable. `0` means
    /// "still actively bonded — no unbond requested".
    pub unbond_at: u64,
}

impl StakeRecord {
    /// `true` while the stake counts toward the elected validator set.
    #[must_use]
    pub fn is_bonded(&self) -> bool {
        self.unbond_at == 0 && !self.amount_wei.is_zero()
    }
}

/// Stake-table accessor. Wraps an `Arc<RocksDbBackend>` for atomic
/// reads / writes against the persistent `Meta` column family.
pub struct StakeTable {
    backend: Arc<RocksDbBackend>,
}

impl StakeTable {
    /// Construct from a shared backend (typically `NodeState::backend()`).
    #[must_use]
    pub const fn new(backend: Arc<RocksDbBackend>) -> Self {
        Self { backend }
    }

    fn key(addr: &Address) -> Vec<u8> {
        let mut k = Vec::with_capacity(KEY_PREFIX.len() + 20);
        k.extend_from_slice(KEY_PREFIX);
        k.extend_from_slice(addr.as_bytes());
        k
    }

    fn encode_value(amount: U256, unbond_at: u64) -> [u8; 40] {
        let mut v = [0u8; 40];
        v[..32].copy_from_slice(&amount.to_be_bytes::<32>());
        v[32..].copy_from_slice(&unbond_at.to_be_bytes());
        v
    }

    fn decode_value(bytes: &[u8]) -> Option<(U256, u64)> {
        if bytes.len() != 40 {
            return None;
        }
        let mut amt = [0u8; 32];
        amt.copy_from_slice(&bytes[..32]);
        let mut height = [0u8; 8];
        height.copy_from_slice(&bytes[32..]);
        Some((U256::from_be_bytes(amt), u64::from_be_bytes(height)))
    }

    /// Read the current stake record for `addr`, or `Ok(None)` if no
    /// bond has ever been recorded.
    ///
    /// # Errors
    /// Propagates backend errors.
    pub fn get(
        &self,
        addr: &Address,
    ) -> Result<Option<StakeRecord>, Box<dyn std::error::Error + Send + Sync>> {
        let Some(bytes) = self.backend.get(ColumnFamily::Meta, &Self::key(addr))? else {
            return Ok(None);
        };
        let Some((amount_wei, unbond_at)) = Self::decode_value(&bytes) else {
            return Ok(None);
        };
        Ok(Some(StakeRecord {
            staker: *addr,
            amount_wei,
            unbond_at,
        }))
    }

    /// Add `delta` Wei to `addr`'s bond. Resets `unbond_at` to 0
    /// (re-bonding cancels any pending unbond — the staker is signalling
    /// they want back into the active set).
    ///
    /// # Errors
    /// Propagates backend errors.
    pub fn bond(
        &self,
        addr: &Address,
        delta: U256,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let prev = self.get(addr)?;
        let new_amount = prev
            .as_ref()
            .map_or(U256::ZERO, |r| r.amount_wei)
            .saturating_add(delta);
        self.backend.put(
            ColumnFamily::Meta,
            &Self::key(addr),
            &Self::encode_value(new_amount, 0),
        )?;
        Ok(())
    }

    /// Begin unbonding at block `current_height`. The stake becomes
    /// withdrawable at `current_height + unbonding_period`. Idempotent
    /// — repeated calls reset the unbond timer.
    ///
    /// # Errors
    /// Returns an error if `addr` has no record, or on backend failure.
    pub fn begin_unbond(
        &self,
        addr: &Address,
        current_height: u64,
        unbonding_period: u64,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let Some(prev) = self.get(addr)? else {
            return Err(format!("begin_unbond: no stake at {addr:?}").into());
        };
        let unbond_at = current_height.saturating_add(unbonding_period);
        self.backend.put(
            ColumnFamily::Meta,
            &Self::key(addr),
            &Self::encode_value(prev.amount_wei, unbond_at),
        )?;
        Ok(())
    }

    /// Sweep a fully-unbonded record. Removes it from the table and
    /// returns the amount that was held. Returns `Ok(None)` if the
    /// stake is still bonded or the unbond timer has not yet elapsed.
    ///
    /// # Errors
    /// Propagates backend errors.
    pub fn withdraw(
        &self,
        addr: &Address,
        current_height: u64,
    ) -> Result<Option<U256>, Box<dyn std::error::Error + Send + Sync>> {
        let Some(rec) = self.get(addr)? else {
            return Ok(None);
        };
        if rec.unbond_at == 0 || current_height < rec.unbond_at {
            return Ok(None);
        }
        self.backend.delete(ColumnFamily::Meta, &Self::key(addr))?;
        Ok(Some(rec.amount_wei))
    }

    /// Reduce `addr`'s bonded amount by `delta` (used by the slashing
    /// executor once C.6 / C.7 are wired). Underflow saturates at 0.
    ///
    /// # Errors
    /// Returns an error if `addr` has no record, or on backend failure.
    pub fn slash(
        &self,
        addr: &Address,
        delta: U256,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let Some(prev) = self.get(addr)? else {
            return Err(format!("slash: no stake at {addr:?}").into());
        };
        let new_amount = prev.amount_wei.saturating_sub(delta);
        self.backend.put(
            ColumnFamily::Meta,
            &Self::key(addr),
            &Self::encode_value(new_amount, prev.unbond_at),
        )?;
        Ok(())
    }

    /// Scan every stake record in the table.
    ///
    /// # Errors
    /// Propagates backend errors.
    pub fn list_all(&self) -> Result<Vec<StakeRecord>, Box<dyn std::error::Error + Send + Sync>> {
        let mut out = Vec::new();
        for kv in self.backend.iter_prefix(ColumnFamily::Meta, KEY_PREFIX) {
            let (k, v) = kv?;
            if k.len() != KEY_PREFIX.len() + 20 {
                continue;
            }
            let mut addr_arr = [0u8; 20];
            addr_arr.copy_from_slice(&k[KEY_PREFIX.len()..]);
            let staker = Address::new(addr_arr);
            let Some((amount_wei, unbond_at)) = Self::decode_value(&v) else {
                continue;
            };
            out.push(StakeRecord {
                staker,
                amount_wei,
                unbond_at,
            });
        }
        Ok(out)
    }

    /// Sum every currently-bonded stake — the denominator for any
    /// stake-weighted operation (DPoS election, governance quorum).
    ///
    /// # Errors
    /// Propagates backend errors.
    pub fn total_bonded(&self) -> Result<U256, Box<dyn std::error::Error + Send + Sync>> {
        let mut total = U256::ZERO;
        for r in self.list_all()? {
            if r.is_bonded() {
                total = total.saturating_add(r.amount_wei);
            }
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aii_storage::RocksDbBackend;

    fn fresh_table() -> StakeTable {
        let backend = Arc::new(RocksDbBackend::open_in_temp().unwrap());
        StakeTable::new(backend)
    }

    #[test]
    fn bond_then_get_round_trips() {
        let t = fresh_table();
        let addr = Address::new([0xa1; 20]);
        let amount = U256::from(1_000_000u64);
        t.bond(&addr, amount).unwrap();
        let r = t.get(&addr).unwrap().unwrap();
        assert_eq!(r.amount_wei, amount);
        assert_eq!(r.unbond_at, 0);
        assert!(r.is_bonded());
    }

    #[test]
    fn bond_accumulates() {
        let t = fresh_table();
        let addr = Address::new([0xa1; 20]);
        t.bond(&addr, U256::from(100u64)).unwrap();
        t.bond(&addr, U256::from(50u64)).unwrap();
        let r = t.get(&addr).unwrap().unwrap();
        assert_eq!(r.amount_wei, U256::from(150u64));
    }

    #[test]
    fn begin_unbond_freezes_bond() {
        let t = fresh_table();
        let addr = Address::new([0xa1; 20]);
        t.bond(&addr, U256::from(1_000u64)).unwrap();
        t.begin_unbond(&addr, 5, 100).unwrap();
        let r = t.get(&addr).unwrap().unwrap();
        assert_eq!(r.unbond_at, 105);
        assert!(!r.is_bonded()); // unbonding ≠ bonded
    }

    #[test]
    fn withdraw_before_unbond_elapsed_returns_none() {
        let t = fresh_table();
        let addr = Address::new([0xa1; 20]);
        t.bond(&addr, U256::from(1_000u64)).unwrap();
        t.begin_unbond(&addr, 10, 100).unwrap();
        // Current height 50 < unbond_at 110.
        assert!(t.withdraw(&addr, 50).unwrap().is_none());
        // Record still exists.
        assert!(t.get(&addr).unwrap().is_some());
    }

    #[test]
    fn withdraw_after_unbond_elapsed_removes_record() {
        let t = fresh_table();
        let addr = Address::new([0xa1; 20]);
        t.bond(&addr, U256::from(1_000u64)).unwrap();
        t.begin_unbond(&addr, 10, 100).unwrap();
        let out = t.withdraw(&addr, 120).unwrap();
        assert_eq!(out, Some(U256::from(1_000u64)));
        // Record gone.
        assert!(t.get(&addr).unwrap().is_none());
    }

    #[test]
    fn slash_reduces_bond_saturating() {
        let t = fresh_table();
        let addr = Address::new([0xa1; 20]);
        t.bond(&addr, U256::from(1_000u64)).unwrap();
        t.slash(&addr, U256::from(600u64)).unwrap();
        let r = t.get(&addr).unwrap().unwrap();
        assert_eq!(r.amount_wei, U256::from(400u64));
        // Slash beyond available saturates.
        t.slash(&addr, U256::from(10_000u64)).unwrap();
        let r = t.get(&addr).unwrap().unwrap();
        assert_eq!(r.amount_wei, U256::ZERO);
    }

    #[test]
    fn list_all_returns_every_record() {
        let t = fresh_table();
        let a = Address::new([0xa1; 20]);
        let b = Address::new([0xb2; 20]);
        t.bond(&a, U256::from(100u64)).unwrap();
        t.bond(&b, U256::from(200u64)).unwrap();
        let all = t.list_all().unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn total_bonded_skips_unbonding_records() {
        let t = fresh_table();
        let a = Address::new([0xa1; 20]);
        let b = Address::new([0xb2; 20]);
        t.bond(&a, U256::from(100u64)).unwrap();
        t.bond(&b, U256::from(200u64)).unwrap();
        t.begin_unbond(&a, 1, 50).unwrap();
        assert_eq!(t.total_bonded().unwrap(), U256::from(200u64));
    }
}
