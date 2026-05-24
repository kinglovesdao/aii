//! # aii-vnode
//!
//! V-node (validator) accounting for AII. Maintains the active set
//! (`VSet`), tracks each validator's stake, and provides the reward-split
//! helper required by the consensus engine.
//!
//! ## Public API
//! - [`VNode`] — one validator record (address + BLS pubkey + stake)
//! - [`VSet`] — ordered map of active validators
//! - [`MIN_STAKE_WEI`] (= 100,000 × 1e18 AII)
//! - [`split_reward`] — 80/20 split between validator and treasury
//! - [`VNodeError`] umbrella

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use aii_types::{Address, BlsPubKey, U256};
use std::collections::BTreeMap;
use thiserror::Error;

/// Minimum stake to enter the active set, in Wei (= 100,000 AII × 1e18).
pub const MIN_STAKE_WEI: u128 = 100_000 * 1_000_000_000_000_000_000;

/// Validator share numerator (out of 100).
pub const VALIDATOR_SHARE_PCT: u32 = 80;
/// Treasury share numerator (out of 100).
pub const TREASURY_SHARE_PCT: u32 = 20;

/// One V-node record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VNode {
    /// EOA address used for stake and reward payouts.
    pub address: Address,
    /// BLS12-381 public key used for PRE-COMMIT aggregation.
    pub bls_pubkey: BlsPubKey,
    /// Total stake currently locked under `address`, in Wei.
    pub stake: U256,
    /// `true` while the node is in the active set (not jailed / not exiting).
    pub active: bool,
}

impl VNode {
    /// Returns `true` iff this node meets the active-set stake floor.
    pub fn meets_minimum_stake(&self) -> bool {
        self.stake >= U256::from(MIN_STAKE_WEI)
    }
}

/// Active validator set.
#[derive(Debug, Clone, Default)]
pub struct VSet {
    /// Ordered by address for deterministic iteration.
    nodes: BTreeMap<Address, VNode>,
}

impl VSet {
    /// Construct an empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of validators (including inactive).
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// `true` iff the set is empty.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Iterate all validators in address order.
    pub fn iter(&self) -> impl Iterator<Item = &VNode> {
        self.nodes.values()
    }

    /// Iterate only active validators.
    pub fn active(&self) -> impl Iterator<Item = &VNode> {
        self.nodes.values().filter(|v| v.active)
    }

    /// Total stake across all *active* validators.
    pub fn total_active_stake(&self) -> U256 {
        self.active()
            .fold(U256::ZERO, |acc, v| acc.wrapping_add(v.stake))
    }

    /// Register a new V-node or top up an existing one's stake. If after
    /// the addition the stake meets the floor, the node is marked active.
    pub fn apply_stake(
        &mut self,
        address: Address,
        bls_pubkey: BlsPubKey,
        amount: U256,
    ) -> Result<(), VNodeError> {
        if amount == U256::ZERO {
            return Err(VNodeError::ZeroAmount);
        }
        let entry = self.nodes.entry(address).or_insert(VNode {
            address,
            bls_pubkey,
            stake: U256::ZERO,
            active: false,
        });
        entry.stake = entry.stake.wrapping_add(amount);
        entry.active = entry.meets_minimum_stake();
        Ok(())
    }

    /// Reduce stake for an existing V-node. Removes the entry if the new
    /// stake is zero. Drops `active` if below the floor.
    pub fn apply_unstake(&mut self, address: Address, amount: U256) -> Result<(), VNodeError> {
        let entry = self.nodes.get_mut(&address).ok_or(VNodeError::Unknown)?;
        if amount > entry.stake {
            return Err(VNodeError::Underflow);
        }
        entry.stake -= amount;
        if entry.stake == U256::ZERO {
            self.nodes.remove(&address);
        } else {
            entry.active = entry.meets_minimum_stake();
        }
        Ok(())
    }
}

/// Split a block reward 80% to validator / 20% to treasury.
pub fn split_reward(total: U256) -> (U256, U256) {
    let validator = total * U256::from(VALIDATOR_SHARE_PCT) / U256::from(100u8);
    let treasury = total - validator;
    (validator, treasury)
}

/// Errors produced by V-node accounting.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum VNodeError {
    /// Stake amount must be non-zero.
    #[error("stake amount is zero")]
    ZeroAmount,

    /// Unstake target does not exist.
    #[error("unknown v-node")]
    Unknown,

    /// Unstake amount exceeds current stake.
    #[error("unstake exceeds stake")]
    Underflow,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aii_wei(n: u128) -> U256 {
        U256::from(n) * U256::from(1_000_000_000_000_000_000u128)
    }

    fn pk_n(n: u8) -> BlsPubKey {
        BlsPubKey::new([n; 48])
    }

    fn addr(n: u8) -> Address {
        Address::new([n; 20])
    }

    #[test]
    fn min_stake_is_100k_aii() {
        assert_eq!(U256::from(MIN_STAKE_WEI), aii_wei(100_000));
    }

    #[test]
    fn stake_below_floor_is_inactive() {
        let mut set = VSet::new();
        set.apply_stake(addr(1), pk_n(1), aii_wei(50_000)).unwrap();
        assert_eq!(set.len(), 1);
        assert_eq!(set.active().count(), 0);
    }

    #[test]
    fn stake_at_floor_is_active() {
        let mut set = VSet::new();
        set.apply_stake(addr(1), pk_n(1), aii_wei(100_000)).unwrap();
        assert_eq!(set.active().count(), 1);
    }

    #[test]
    fn top_up_promotes_to_active() {
        let mut set = VSet::new();
        set.apply_stake(addr(1), pk_n(1), aii_wei(50_000)).unwrap();
        set.apply_stake(addr(1), pk_n(1), aii_wei(50_000)).unwrap();
        assert_eq!(set.active().count(), 1);
        assert_eq!(set.total_active_stake(), aii_wei(100_000));
    }

    #[test]
    fn unstake_below_floor_deactivates() {
        let mut set = VSet::new();
        set.apply_stake(addr(1), pk_n(1), aii_wei(100_000)).unwrap();
        set.apply_unstake(addr(1), aii_wei(60_000)).unwrap();
        assert_eq!(set.active().count(), 0);
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn full_unstake_removes_entry() {
        let mut set = VSet::new();
        set.apply_stake(addr(1), pk_n(1), aii_wei(100_000)).unwrap();
        set.apply_unstake(addr(1), aii_wei(100_000)).unwrap();
        assert_eq!(set.len(), 0);
    }

    #[test]
    fn unstake_more_than_stake_errors() {
        let mut set = VSet::new();
        set.apply_stake(addr(1), pk_n(1), aii_wei(100_000)).unwrap();
        let err = set.apply_unstake(addr(1), aii_wei(200_000));
        assert_eq!(err, Err(VNodeError::Underflow));
    }

    #[test]
    fn unstake_unknown_errors() {
        let mut set = VSet::new();
        let err = set.apply_unstake(addr(9), aii_wei(1));
        assert_eq!(err, Err(VNodeError::Unknown));
    }

    #[test]
    fn zero_stake_errors() {
        let mut set = VSet::new();
        let err = set.apply_stake(addr(1), pk_n(1), U256::ZERO);
        assert_eq!(err, Err(VNodeError::ZeroAmount));
    }

    #[test]
    fn reward_split_is_80_20() {
        let (v, t) = split_reward(U256::from(100u64));
        assert_eq!(v, U256::from(80u64));
        assert_eq!(t, U256::from(20u64));
    }

    #[test]
    fn reward_split_sums_to_total() {
        let total = U256::from(1_000_000u64);
        let (v, t) = split_reward(total);
        assert_eq!(v + t, total);
    }
}
