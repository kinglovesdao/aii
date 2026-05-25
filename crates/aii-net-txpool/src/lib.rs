//! # aii-net-txpool
//!
//! Capacity-bounded mempool, keyed by `(sender, nonce)`.
//!
//! ## Public API
//! - [`TxPool`] — `add` / `remove` / `len` / `drain_ready` / `evict_to`
//! - [`PoolError`] umbrella
//!
//! ## Semantics
//! - Transactions are stored verbatim — the pool does **not** verify
//!   signatures, gas, or balance. It assumes the caller already screened
//!   the incoming tx through the validation layer.
//! - Insertion is `O(log n)` (BTreeMap on `(sender, nonce)`).
//! - On duplicate `(sender, nonce)`: the entry with the higher effective
//!   gas price wins.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use aii_block::{Tx, TxEip1559, TxEip4844, TxLegacy};
use aii_types::{Address, U256};
use parking_lot::RwLock;
use std::collections::BTreeMap;
use std::sync::Arc;
use thiserror::Error;

/// One entry in the pool.
#[derive(Debug, Clone)]
pub struct PoolEntry {
    /// Sender address (recovered upstream — pool does not verify).
    pub sender: Address,
    /// Transaction nonce (denormalised for fast lookup).
    pub nonce: u64,
    /// Effective gas price (for ordering / eviction).
    pub effective_gas_price: U256,
    /// The transaction itself.
    pub tx: Tx,
}

/// Mempool.
#[derive(Debug, Clone, Default)]
pub struct TxPool {
    inner: Arc<RwLock<Inner>>,
    capacity: usize,
}

#[derive(Debug, Default)]
struct Inner {
    /// Primary index: `(sender, nonce) → entry`.
    entries: BTreeMap<(Address, u64), PoolEntry>,
}

impl TxPool {
    /// Construct a pool with a maximum size.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner::default())),
            capacity,
        }
    }

    /// Current pool size.
    pub fn len(&self) -> usize {
        let g = self.inner.read();
        let n = g.entries.len();
        drop(g);
        n
    }

    /// `true` iff empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Add an entry. Returns `Replaced(old)` if it replaced a same-nonce
    /// entry with lower gas price.
    #[allow(clippy::significant_drop_tightening)]
    pub fn add(&self, entry: PoolEntry) -> Result<AddOutcome, PoolError> {
        let mut g = self.inner.write();
        if g.entries.len() >= self.capacity {
            return Err(PoolError::Full);
        }
        let key = (entry.sender, entry.nonce);
        if let Some(existing) = g.entries.get(&key) {
            if existing.effective_gas_price >= entry.effective_gas_price {
                return Ok(AddOutcome::RejectedUnderpriced);
            }
            let old = g.entries.insert(key, entry).expect("just-checked exists");
            return Ok(AddOutcome::Replaced(Box::new(old)));
        }
        g.entries.insert(key, entry);
        Ok(AddOutcome::Inserted)
    }

    /// Remove the entry at `(sender, nonce)`. No-op if missing.
    pub fn remove(&self, sender: Address, nonce: u64) -> Option<PoolEntry> {
        self.inner.write().entries.remove(&(sender, nonce))
    }

    /// Return the contiguous run of transactions for `sender` starting at
    /// `current_nonce` (inclusive). Caller drains in nonce order to build
    /// the next block.
    #[allow(clippy::significant_drop_tightening)]
    pub fn drain_ready(&self, sender: Address, current_nonce: u64) -> Vec<Tx> {
        let mut g = self.inner.write();
        let mut out = Vec::new();
        let mut next_nonce = current_nonce;
        loop {
            let key = (sender, next_nonce);
            let Some(entry) = g.entries.remove(&key) else {
                break;
            };
            out.push(entry.tx);
            next_nonce = next_nonce.checked_add(1).expect("nonce overflow");
        }
        out
    }

    /// Drain up to `n` entries in BTreeMap key order — used by block
    /// producers to pull a batch without enforcing per-sender nonce
    /// ordering across sender boundaries. (For v0.0.37 stress testing
    /// where each signer files monotonically-increasing nonces, this
    /// is good enough: the BTreeMap order is `(sender, nonce)` so
    /// within one sender the nonces are sequential.)
    #[allow(clippy::significant_drop_tightening)]
    pub fn drain_up_to(&self, n: usize) -> Vec<Tx> {
        let mut g = self.inner.write();
        // Collect keys first (need owned copies before mutating the map).
        let keys: Vec<(Address, u64)> = g.entries.keys().take(n).copied().collect();
        let mut out = Vec::with_capacity(keys.len());
        for k in keys {
            if let Some(e) = g.entries.remove(&k) {
                out.push(e.tx);
            }
        }
        out
    }

    /// Evict lowest-gas-price entries until size ≤ `target`.
    pub fn evict_to(&self, target: usize) {
        let mut g = self.inner.write();
        if g.entries.len() <= target {
            return;
        }
        let mut sorted: Vec<((Address, u64), U256)> = g
            .entries
            .iter()
            .map(|(k, e)| (*k, e.effective_gas_price))
            .collect();
        sorted.sort_by(|a, b| a.1.cmp(&b.1));
        let to_drop = g.entries.len() - target;
        for (key, _) in sorted.into_iter().take(to_drop) {
            g.entries.remove(&key);
        }
    }
}

/// Effective gas price of a transaction (for pool ordering).
pub const fn effective_gas_price(tx: &Tx) -> U256 {
    match tx {
        Tx::Legacy(TxLegacy { gas_price, .. }) => *gas_price,
        Tx::Eip1559(TxEip1559 {
            max_fee_per_gas, ..
        })
        | Tx::Eip4844(TxEip4844 {
            max_fee_per_gas, ..
        }) => *max_fee_per_gas,
    }
}

/// Result of `TxPool::add`.
#[derive(Debug)]
pub enum AddOutcome {
    /// Brand-new entry added.
    Inserted,
    /// Same `(sender, nonce)` already present with strictly higher gas;
    /// the incoming tx was rejected.
    RejectedUnderpriced,
    /// Same `(sender, nonce)` was replaced; the old entry is returned.
    Replaced(Box<PoolEntry>),
}

/// Errors produced by the pool.
#[derive(Debug, Error)]
pub enum PoolError {
    /// Pool capacity reached. Call `evict_to` before adding more.
    #[error("pool full")]
    Full,
}

#[cfg(test)]
mod tests {
    use super::*;
    use aii_types::{AlgoId, H256};

    fn dummy_tx(gas_price: u64) -> Tx {
        Tx::Legacy(TxLegacy {
            nonce: 0,
            gas_price: U256::from(gas_price),
            gas_limit: 21_000,
            to: Some(Address::new([0x01; 20])),
            value: U256::ZERO,
            data: vec![],
            v: 27,
            r: H256::new([0xaa; 32]),
            s: H256::new([0xbb; 32]),
            algo_id: AlgoId::Secp256k1,
        })
    }

    fn entry(sender_byte: u8, nonce: u64, gas: u64) -> PoolEntry {
        let tx = dummy_tx(gas);
        PoolEntry {
            sender: Address::new([sender_byte; 20]),
            nonce,
            effective_gas_price: U256::from(gas),
            tx,
        }
    }

    #[test]
    fn empty_pool_is_empty() {
        let pool = TxPool::new(8);
        assert!(pool.is_empty());
    }

    #[test]
    fn add_increases_len() {
        let pool = TxPool::new(8);
        pool.add(entry(1, 0, 100)).unwrap();
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn duplicate_lower_price_rejected() {
        let pool = TxPool::new(8);
        pool.add(entry(1, 0, 100)).unwrap();
        match pool.add(entry(1, 0, 50)).unwrap() {
            AddOutcome::RejectedUnderpriced => {}
            other => panic!("unexpected: {other:?}"),
        }
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn duplicate_higher_price_replaces() {
        let pool = TxPool::new(8);
        pool.add(entry(1, 0, 100)).unwrap();
        match pool.add(entry(1, 0, 200)).unwrap() {
            AddOutcome::Replaced(_) => {}
            other => panic!("unexpected: {other:?}"),
        }
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn capacity_enforced() {
        let pool = TxPool::new(2);
        pool.add(entry(1, 0, 10)).unwrap();
        pool.add(entry(2, 0, 10)).unwrap();
        let err = pool.add(entry(3, 0, 10));
        assert!(matches!(err, Err(PoolError::Full)));
    }

    #[test]
    fn drain_ready_returns_contiguous_run() {
        let pool = TxPool::new(8);
        pool.add(entry(1, 5, 10)).unwrap();
        pool.add(entry(1, 6, 10)).unwrap();
        pool.add(entry(1, 7, 10)).unwrap();
        pool.add(entry(1, 9, 10)).unwrap(); // gap
        let run = pool.drain_ready(Address::new([1; 20]), 5);
        assert_eq!(run.len(), 3);
        // entry at nonce 9 still in pool
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn evict_drops_lowest_gas_first() {
        let pool = TxPool::new(8);
        pool.add(entry(1, 0, 100)).unwrap();
        pool.add(entry(2, 0, 10)).unwrap();
        pool.add(entry(3, 0, 50)).unwrap();
        pool.evict_to(1);
        assert_eq!(pool.len(), 1);
        // highest gas (100) should survive
        let remaining = pool.remove(Address::new([1; 20]), 0);
        assert!(remaining.is_some());
    }

    #[test]
    fn remove_nonexistent_is_none() {
        let pool = TxPool::new(8);
        assert!(pool.remove(Address::new([1; 20]), 0).is_none());
    }
}
