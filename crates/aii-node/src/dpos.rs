//! DPoS validator-set election (roadmap C.6).
//!
//! At every epoch boundary [`elect_active_set`] reads the persistent
//! stake table.
//!
//! Bonded records sort by `amount_wei` descending (ties broken by
//! address ascending for determinism), records below
//! `min_validator_stake_wei` drop out, and the top
//! `validators_per_epoch` entries persist under `ColumnFamily::Meta`
//! key `b"validator_set:" ‖ epoch_be8` so any node can later inspect
//! historical sets.
//!
//! If stakers have registered BLS/VRF runtime keys through the validator
//! registry, the persisted set includes those keys. That makes each
//! epoch record sufficient to construct a BFT [`ValidatorSet`] in the
//! engine-rotation follow-up.

use crate::staking::{StakeRecord, StakeTable};
use crate::validator_registry::ValidatorRegistry;
use aii_storage::{ColumnFamily, KvBackend, RocksDbBackend};
use aii_types::{Address, BlsPubKey, VrfPubKey, U256};
use std::sync::Arc;

/// Key prefix for the per-epoch elected validator set.
const KEY_PREFIX: &[u8] = b"validator_set:";

/// One entry in an elected DPoS validator set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatorEntry {
    /// Validator address (the staker that locked the bond).
    pub address: Address,
    /// Bonded amount at election time.
    pub stake_wei: U256,
    /// Registered BLS key for BFT voting, when available.
    pub bls_pubkey: Option<BlsPubKey>,
    /// Registered VRF key for BFT leader selection, when available.
    pub vrf_pubkey: Option<VrfPubKey>,
}

/// Elect the top-N stakers from `table` who clear `min_stake`.
///
/// Sort order is `(stake_wei desc, address asc)` — deterministic
/// across every node so the active set is a function of state, not
/// insertion order. Returns at most `validators_per_epoch` entries;
/// fewer if not enough stakers clear the floor.
///
/// # Errors
/// Propagates the underlying backend scan error.
pub fn elect_active_set(
    table: &StakeTable,
    min_stake: U256,
    validators_per_epoch: u32,
) -> Result<Vec<ValidatorEntry>, Box<dyn std::error::Error + Send + Sync>> {
    let mut all: Vec<StakeRecord> = table
        .list_all()?
        .into_iter()
        .filter(|r| r.is_bonded() && r.amount_wei >= min_stake)
        .collect();
    // Sort: descending by stake, then ascending by address.
    all.sort_by(|a, b| {
        b.amount_wei
            .cmp(&a.amount_wei)
            .then(a.staker.as_bytes().cmp(b.staker.as_bytes()))
    });
    let cap = validators_per_epoch as usize;
    Ok(all
        .into_iter()
        .take(cap)
        .map(|r| ValidatorEntry {
            address: r.staker,
            stake_wei: r.amount_wei,
            bls_pubkey: None,
            vrf_pubkey: None,
        })
        .collect())
}

/// Elect the top-N stakers who have also registered BFT runtime keys.
///
/// This is the production-ready material for DPoS→BFT rotation: every
/// returned entry carries stake, reward/staker address, BLS pubkey, and
/// VRF pubkey. Stake-only records are intentionally skipped because the
/// BFT engine cannot verify votes or leader proofs without both keys.
///
/// # Errors
/// Propagates backend scan/read errors.
pub fn elect_registered_active_set(
    table: &StakeTable,
    registry: &ValidatorRegistry,
    min_stake: U256,
    validators_per_epoch: u32,
) -> Result<Vec<ValidatorEntry>, Box<dyn std::error::Error + Send + Sync>> {
    let mut all: Vec<StakeRecord> = table
        .list_all()?
        .into_iter()
        .filter(|r| r.is_bonded() && r.amount_wei >= min_stake)
        .collect();
    all.sort_by(|a, b| {
        b.amount_wei
            .cmp(&a.amount_wei)
            .then(a.staker.as_bytes().cmp(b.staker.as_bytes()))
    });

    let mut out = Vec::new();
    for r in all {
        if out.len() >= validators_per_epoch as usize {
            break;
        }
        let Some(keys) = registry.get(&r.staker)? else {
            continue;
        };
        out.push(ValidatorEntry {
            address: r.staker,
            stake_wei: r.amount_wei,
            bls_pubkey: Some(keys.bls_pubkey),
            vrf_pubkey: Some(keys.vrf_pubkey),
        });
    }
    Ok(out)
}

/// Persist `set` as the elected validator set for `epoch`.
///
/// Legacy value layout:
/// `count_be4 ‖ [address[20] ‖ stake_wei_be32]*count`.
///
/// Runtime-key value layout:
/// `count_be4 ‖ [address[20] ‖ stake_wei_be32 ‖ bls[48] ‖ vrf[32]]*count`.
///
/// # Errors
/// Propagates backend write errors.
pub fn persist_validator_set(
    backend: &Arc<RocksDbBackend>,
    epoch: u64,
    set: &[ValidatorEntry],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut key = Vec::with_capacity(KEY_PREFIX.len() + 8);
    key.extend_from_slice(KEY_PREFIX);
    key.extend_from_slice(&epoch.to_be_bytes());

    let has_keys = set
        .iter()
        .all(|v| v.bls_pubkey.is_some() && v.vrf_pubkey.is_some());
    let entry_len = if has_keys { 20 + 32 + 48 + 32 } else { 20 + 32 };
    let mut val = Vec::with_capacity(4 + set.len() * entry_len);
    let count_u32 = u32::try_from(set.len()).unwrap_or(u32::MAX);
    val.extend_from_slice(&count_u32.to_be_bytes());
    for v in set {
        val.extend_from_slice(v.address.as_bytes());
        val.extend_from_slice(&v.stake_wei.to_be_bytes::<32>());
        if has_keys {
            val.extend_from_slice(v.bls_pubkey.expect("checked above").as_bytes());
            val.extend_from_slice(v.vrf_pubkey.expect("checked above").as_bytes());
        }
    }
    backend.put(ColumnFamily::Meta, &key, &val)?;
    Ok(())
}

/// Read back a persisted validator set for `epoch`, or `Ok(None)` if
/// no election was recorded at that epoch.
///
/// # Errors
/// Propagates backend errors / decode failures.
pub fn read_validator_set(
    backend: &Arc<RocksDbBackend>,
    epoch: u64,
) -> Result<Option<Vec<ValidatorEntry>>, Box<dyn std::error::Error + Send + Sync>> {
    let mut key = Vec::with_capacity(KEY_PREFIX.len() + 8);
    key.extend_from_slice(KEY_PREFIX);
    key.extend_from_slice(&epoch.to_be_bytes());
    let Some(bytes) = backend.get(ColumnFamily::Meta, &key)? else {
        return Ok(None);
    };
    if bytes.len() < 4 {
        return Ok(None);
    }
    let mut count_arr = [0u8; 4];
    count_arr.copy_from_slice(&bytes[..4]);
    let count = u32::from_be_bytes(count_arr) as usize;
    let body = &bytes[4..];
    let legacy_len = count * (20 + 32);
    let keyed_len = count * (20 + 32 + 48 + 32);
    if body.len() != legacy_len && body.len() != keyed_len {
        return Ok(None);
    }
    let has_keys = body.len() == keyed_len;
    let entry_len = if has_keys { 20 + 32 + 48 + 32 } else { 20 + 32 };
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let off = i * entry_len;
        let mut addr = [0u8; 20];
        addr.copy_from_slice(&body[off..off + 20]);
        let mut stake = [0u8; 32];
        stake.copy_from_slice(&body[off + 20..off + 52]);
        let (bls_pubkey, vrf_pubkey) = if has_keys {
            let mut bls = [0u8; 48];
            bls.copy_from_slice(&body[off + 52..off + 100]);
            let mut vrf = [0u8; 32];
            vrf.copy_from_slice(&body[off + 100..off + 132]);
            (Some(BlsPubKey::new(bls)), Some(VrfPubKey::new(vrf)))
        } else {
            (None, None)
        };
        out.push(ValidatorEntry {
            address: Address::new(addr),
            stake_wei: U256::from_be_bytes(stake),
            bls_pubkey,
            vrf_pubkey,
        });
    }
    Ok(Some(out))
}

/// `(epoch, validator_entries)` payload returned by
/// [`latest_validator_set`]. Aliased to keep the function signature
/// readable.
pub type LatestEpochSet = (u64, Vec<ValidatorEntry>);

/// Find the highest persisted epoch and return the validator set
/// elected at that epoch boundary. Returns `Ok(None)` if no epoch
/// election has ever been recorded (e.g. genesis-only chain).
///
/// # Errors
/// Propagates backend errors.
pub fn latest_validator_set(
    backend: &Arc<RocksDbBackend>,
) -> Result<Option<LatestEpochSet>, Box<dyn std::error::Error + Send + Sync>> {
    let mut latest_epoch: Option<u64> = None;
    for kv in backend.iter_prefix(ColumnFamily::Meta, KEY_PREFIX) {
        let (k, _) = kv?;
        let suffix = &k[KEY_PREFIX.len()..];
        if suffix.len() != 8 {
            continue;
        }
        let mut arr = [0u8; 8];
        arr.copy_from_slice(suffix);
        let epoch = u64::from_be_bytes(arr);
        if latest_epoch.is_none_or(|c| epoch > c) {
            latest_epoch = Some(epoch);
        }
    }
    let Some(epoch) = latest_epoch else {
        return Ok(None);
    };
    let set = read_validator_set(backend, epoch)?.unwrap_or_default();
    Ok(Some((epoch, set)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aii_storage::RocksDbBackend;

    fn fresh_table() -> (Arc<RocksDbBackend>, StakeTable) {
        let backend = Arc::new(RocksDbBackend::open_in_temp().unwrap());
        let table = StakeTable::new(Arc::clone(&backend));
        (backend, table)
    }

    fn legacy_entry(address: Address, stake_wei: U256) -> ValidatorEntry {
        ValidatorEntry {
            address,
            stake_wei,
            bls_pubkey: None,
            vrf_pubkey: None,
        }
    }

    #[test]
    fn empty_table_elects_nothing() {
        let (_b, t) = fresh_table();
        let set = elect_active_set(&t, U256::from(1u64), 10).unwrap();
        assert!(set.is_empty());
    }

    #[test]
    fn filters_below_min_stake() {
        let (_b, t) = fresh_table();
        let small = Address::new([0xa1; 20]);
        let big = Address::new([0xa2; 20]);
        t.bond(&small, U256::from(50u64)).unwrap();
        t.bond(&big, U256::from(500u64)).unwrap();
        let set = elect_active_set(&t, U256::from(100u64), 10).unwrap();
        assert_eq!(set.len(), 1);
        assert_eq!(set[0].address, big);
    }

    #[test]
    fn caps_at_validators_per_epoch() {
        let (_b, t) = fresh_table();
        for i in 0..20u8 {
            let addr = Address::new([i; 20]);
            t.bond(&addr, U256::from(1_000u64)).unwrap();
        }
        let set = elect_active_set(&t, U256::from(1u64), 5).unwrap();
        assert_eq!(set.len(), 5);
    }

    #[test]
    fn sorts_by_stake_desc_then_addr_asc() {
        let (_b, t) = fresh_table();
        let a = Address::new([0xa1; 20]);
        let b = Address::new([0xa2; 20]);
        let c = Address::new([0xa3; 20]);
        t.bond(&a, U256::from(1_000u64)).unwrap();
        t.bond(&b, U256::from(2_000u64)).unwrap();
        // c has same stake as a — should follow a (addr asc) when stake matches.
        t.bond(&c, U256::from(1_000u64)).unwrap();
        let set = elect_active_set(&t, U256::ZERO, 10).unwrap();
        // Order: b (highest), then a (1000, addr a1<a3), then c.
        assert_eq!(set[0].address, b);
        assert_eq!(set[1].address, a);
        assert_eq!(set[2].address, c);
    }

    #[test]
    fn excludes_unbonding_stakers() {
        let (_b, t) = fresh_table();
        let a = Address::new([0xa1; 20]);
        let b = Address::new([0xa2; 20]);
        t.bond(&a, U256::from(1_000u64)).unwrap();
        t.bond(&b, U256::from(500u64)).unwrap();
        t.begin_unbond(&a, 1, 10).unwrap();
        let set = elect_active_set(&t, U256::ZERO, 10).unwrap();
        assert_eq!(set.len(), 1);
        assert_eq!(set[0].address, b);
    }

    #[test]
    fn persist_then_read_round_trip() {
        let (backend, _t) = fresh_table();
        let set = vec![
            legacy_entry(Address::new([0xa1; 20]), U256::from(1_000u64)),
            legacy_entry(Address::new([0xa2; 20]), U256::from(500u64)),
        ];
        persist_validator_set(&backend, 42, &set).unwrap();
        let back = read_validator_set(&backend, 42).unwrap().unwrap();
        assert_eq!(back, set);
    }

    #[test]
    fn registered_election_includes_runtime_keys() {
        let (backend, t) = fresh_table();
        let registry = ValidatorRegistry::new(Arc::clone(&backend));
        let registered = Address::new([0xa1; 20]);
        let unregistered = Address::new([0xa2; 20]);
        t.bond(&registered, U256::from(1_000u64)).unwrap();
        t.bond(&unregistered, U256::from(2_000u64)).unwrap();

        registry
            .register(
                &registered,
                crate::validator_registry::ValidatorKeys {
                    bls_pubkey: BlsPubKey::new([0xb1; 48]),
                    vrf_pubkey: VrfPubKey::new([0xc1; 32]),
                },
            )
            .unwrap();

        let set = elect_registered_active_set(&t, &registry, U256::from(1u64), 10).unwrap();

        assert_eq!(set.len(), 1);
        assert_eq!(set[0].address, registered);
        assert_eq!(set[0].bls_pubkey, Some(BlsPubKey::new([0xb1; 48])));
        assert_eq!(set[0].vrf_pubkey, Some(VrfPubKey::new([0xc1; 32])));
    }

    #[test]
    fn keyed_validator_set_persists_and_reads_back() {
        let (backend, _t) = fresh_table();
        let set = vec![ValidatorEntry {
            address: Address::new([0xa1; 20]),
            stake_wei: U256::from(1_000u64),
            bls_pubkey: Some(BlsPubKey::new([0xb1; 48])),
            vrf_pubkey: Some(VrfPubKey::new([0xc1; 32])),
        }];

        persist_validator_set(&backend, 42, &set).unwrap();

        let back = read_validator_set(&backend, 42).unwrap().unwrap();
        assert_eq!(back, set);
    }

    #[test]
    fn latest_epoch_picks_highest_recorded() {
        let (backend, _t) = fresh_table();
        let set1 = vec![legacy_entry(Address::new([0x01; 20]), U256::from(1u64))];
        let set2 = vec![legacy_entry(Address::new([0x02; 20]), U256::from(2u64))];
        persist_validator_set(&backend, 10, &set1).unwrap();
        persist_validator_set(&backend, 100, &set2).unwrap();
        persist_validator_set(&backend, 50, &set1).unwrap();
        let (latest_e, latest_s) = latest_validator_set(&backend).unwrap().unwrap();
        assert_eq!(latest_e, 100);
        assert_eq!(latest_s, set2);
    }
}
