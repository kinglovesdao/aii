//! On-chain governance — proposals, stake-weighted voting, execution
//! markers (roadmap E.2).
//!
//! A proposal is a free-form, gov-namespaced parameter-change request
//! (e.g. "set `block_reward_initial_wei = X`"). Each proposal lives
//! in `ColumnFamily::Meta` under key `b"prop:" ‖ id_be8`. Each vote
//! lives under `b"vote:" ‖ id_be8 ‖ voter[20]`. Votes are weighted by
//! `StakeTable.amount_wei` at vote time; quorum is `simple_majority`
//! against `StakeTable.total_bonded`.
//!
//! This module is deliberately storage-only: producing a proposal or
//! casting a vote happens through the node's RPC surface in
//! `aii-rpc`, and "executing" a passed proposal is a separate
//! engine-side action that future releases wire into the block-build
//! path (a passed proposal is a chain-fork instruction).

use crate::staking::StakeTable;
use aii_storage::{ColumnFamily, KvBackend, RocksDbBackend};
use aii_types::{Address, U256};
use std::sync::Arc;

/// CF key prefix for proposal records.
const PROP_PREFIX: &[u8] = b"prop:";
/// CF key prefix for per-(proposal, voter) vote records.
const VOTE_PREFIX: &[u8] = b"vote:";
/// CF key prefix for a `(proposal_id → tally cache)` shortcut. Filled
/// lazily on first read.
const TALLY_PREFIX: &[u8] = b"tally:";

/// A proposal's life-cycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalStatus {
    /// Voting window open.
    Pending = 0,
    /// `> 2/3` of bonded stake voted yes (relative to total bonded
    /// stake at time of tally).
    Passed = 1,
    /// Voting window closed without quorum.
    Rejected = 2,
    /// Passed proposals that have already been executed by the engine.
    Executed = 3,
}

impl ProposalStatus {
    /// `0..3 → ProposalStatus`. Returns `Pending` for any unknown byte
    /// so a corrupt record fails closed (you can re-vote, not
    /// re-execute).
    #[must_use]
    pub const fn from_byte(b: u8) -> Self {
        match b {
            1 => Self::Passed,
            2 => Self::Rejected,
            3 => Self::Executed,
            _ => Self::Pending,
        }
    }
}

/// A proposal record persisted in `ColumnFamily::Meta`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    /// Monotonically-increasing proposal id.
    pub id: u64,
    /// Free-form description of the change being proposed.
    pub title: String,
    /// Block height at which voting ends. After this height, no
    /// further `cast_vote` is accepted; the tally is finalised on
    /// next `tally`.
    pub voting_ends_at: u64,
    /// Current life-cycle status.
    pub status: ProposalStatus,
    /// Address that proposed.
    pub proposer: Address,
}

/// One vote record. `support = true` is yes, false is no.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vote {
    /// Proposal voted on.
    pub proposal_id: u64,
    /// Address casting the vote.
    pub voter: Address,
    /// Yes / no.
    pub support: bool,
    /// Vote weight at the moment of casting (stake at the time, not
    /// at proposal creation — simple `liquid-stake` model).
    pub weight_wei: U256,
}

/// Stake-table-backed governance store.
pub struct Governance {
    backend: Arc<RocksDbBackend>,
}

impl Governance {
    /// Construct from a shared backend.
    #[must_use]
    pub const fn new(backend: Arc<RocksDbBackend>) -> Self {
        Self { backend }
    }

    fn prop_key(id: u64) -> Vec<u8> {
        let mut k = Vec::with_capacity(PROP_PREFIX.len() + 8);
        k.extend_from_slice(PROP_PREFIX);
        k.extend_from_slice(&id.to_be_bytes());
        k
    }

    fn vote_key(id: u64, voter: &Address) -> Vec<u8> {
        let mut k = Vec::with_capacity(VOTE_PREFIX.len() + 8 + 20);
        k.extend_from_slice(VOTE_PREFIX);
        k.extend_from_slice(&id.to_be_bytes());
        k.extend_from_slice(voter.as_bytes());
        k
    }

    fn tally_key(id: u64) -> Vec<u8> {
        let mut k = Vec::with_capacity(TALLY_PREFIX.len() + 8);
        k.extend_from_slice(TALLY_PREFIX);
        k.extend_from_slice(&id.to_be_bytes());
        k
    }

    fn encode_proposal(p: &Proposal) -> Vec<u8> {
        let mut v = Vec::with_capacity(8 + 8 + 1 + 20 + p.title.len() + 4);
        v.extend_from_slice(&p.id.to_be_bytes());
        v.extend_from_slice(&p.voting_ends_at.to_be_bytes());
        v.push(p.status as u8);
        v.extend_from_slice(p.proposer.as_bytes());
        let title_len = u32::try_from(p.title.len()).unwrap_or(u32::MAX);
        v.extend_from_slice(&title_len.to_be_bytes());
        v.extend_from_slice(p.title.as_bytes());
        v
    }

    fn decode_proposal(bytes: &[u8]) -> Option<Proposal> {
        if bytes.len() < 8 + 8 + 1 + 20 + 4 {
            return None;
        }
        let mut id_arr = [0u8; 8];
        id_arr.copy_from_slice(&bytes[..8]);
        let id = u64::from_be_bytes(id_arr);
        let mut ends_arr = [0u8; 8];
        ends_arr.copy_from_slice(&bytes[8..16]);
        let voting_ends_at = u64::from_be_bytes(ends_arr);
        let status = ProposalStatus::from_byte(bytes[16]);
        let mut addr = [0u8; 20];
        addr.copy_from_slice(&bytes[17..37]);
        let proposer = Address::new(addr);
        let mut title_len_arr = [0u8; 4];
        title_len_arr.copy_from_slice(&bytes[37..41]);
        let title_len = u32::from_be_bytes(title_len_arr) as usize;
        if bytes.len() < 41 + title_len {
            return None;
        }
        let title = String::from_utf8(bytes[41..41 + title_len].to_vec()).ok()?;
        Some(Proposal {
            id,
            title,
            voting_ends_at,
            status,
            proposer,
        })
    }

    fn encode_vote(v: &Vote) -> Vec<u8> {
        let mut out = Vec::with_capacity(1 + 32);
        out.push(u8::from(v.support));
        out.extend_from_slice(&v.weight_wei.to_be_bytes::<32>());
        out
    }

    fn decode_vote(proposal_id: u64, voter: Address, bytes: &[u8]) -> Option<Vote> {
        if bytes.len() != 1 + 32 {
            return None;
        }
        let support = bytes[0] != 0;
        let mut w = [0u8; 32];
        w.copy_from_slice(&bytes[1..]);
        Some(Vote {
            proposal_id,
            voter,
            support,
            weight_wei: U256::from_be_bytes(w),
        })
    }

    /// Generate the next proposal id by scanning `prop:` prefix and
    /// finding `max + 1`. O(num proposals) — fine for governance.
    fn next_id(&self) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        let mut max: u64 = 0;
        for kv in self.backend.iter_prefix(ColumnFamily::Meta, PROP_PREFIX) {
            let (k, _) = kv?;
            let suffix = &k[PROP_PREFIX.len()..];
            if suffix.len() != 8 {
                continue;
            }
            let mut arr = [0u8; 8];
            arr.copy_from_slice(suffix);
            let id = u64::from_be_bytes(arr);
            if id > max {
                max = id;
            }
        }
        Ok(max + 1)
    }

    /// Submit a new proposal. Returns its assigned id.
    ///
    /// # Errors
    /// Propagates backend errors.
    pub fn propose(
        &self,
        proposer: Address,
        title: String,
        voting_ends_at: u64,
    ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        let id = self.next_id()?;
        let p = Proposal {
            id,
            title,
            voting_ends_at,
            status: ProposalStatus::Pending,
            proposer,
        };
        self.backend.put(
            ColumnFamily::Meta,
            &Self::prop_key(id),
            &Self::encode_proposal(&p),
        )?;
        Ok(id)
    }

    /// Read a proposal by id, or `Ok(None)` if unknown.
    ///
    /// # Errors
    /// Propagates backend errors.
    pub fn get(
        &self,
        id: u64,
    ) -> Result<Option<Proposal>, Box<dyn std::error::Error + Send + Sync>> {
        let Some(bytes) = self.backend.get(ColumnFamily::Meta, &Self::prop_key(id))? else {
            return Ok(None);
        };
        Ok(Self::decode_proposal(&bytes))
    }

    /// Cast a yes / no vote on proposal `id`. Weight is the voter's
    /// currently-bonded stake (looked up from `table`); votes from
    /// addresses with zero bond are rejected. Re-voting overwrites.
    ///
    /// # Errors
    /// Returns an error string if:
    /// - the proposal doesn't exist,
    /// - voting has ended (`block_height >= voting_ends_at`),
    /// - the voter has no bonded stake.
    pub fn cast_vote(
        &self,
        table: &StakeTable,
        id: u64,
        voter: Address,
        support: bool,
        block_height: u64,
    ) -> Result<Vote, Box<dyn std::error::Error + Send + Sync>> {
        let p = self
            .get(id)?
            .ok_or_else(|| format!("proposal {id} not found"))?;
        if block_height >= p.voting_ends_at {
            return Err(
                format!("proposal {id} voting window closed at {}", p.voting_ends_at).into(),
            );
        }
        let stake = table.get(&voter)?.ok_or_else(|| {
            format!(
                "voter {} has no bonded stake",
                hex::encode(voter.as_bytes())
            )
        })?;
        if !stake.is_bonded() {
            return Err("voter is unbonding — no vote weight".into());
        }
        let vote = Vote {
            proposal_id: id,
            voter,
            support,
            weight_wei: stake.amount_wei,
        };
        self.backend.put(
            ColumnFamily::Meta,
            &Self::vote_key(id, &voter),
            &Self::encode_vote(&vote),
        )?;
        Ok(vote)
    }

    /// Finalise a proposal: walk every vote, sum yes / no, mark
    /// `Passed` if `yes > 2/3 * total_bonded` (chain-wide simple
    /// supermajority). Idempotent — repeated calls on a `Passed` /
    /// `Rejected` proposal are no-ops.
    ///
    /// # Errors
    /// Propagates backend / decode errors.
    pub fn tally(
        &self,
        table: &StakeTable,
        id: u64,
        block_height: u64,
    ) -> Result<ProposalStatus, Box<dyn std::error::Error + Send + Sync>> {
        let mut p = self
            .get(id)?
            .ok_or_else(|| format!("proposal {id} not found"))?;
        if matches!(
            p.status,
            ProposalStatus::Passed | ProposalStatus::Rejected | ProposalStatus::Executed
        ) {
            return Ok(p.status);
        }
        if block_height < p.voting_ends_at {
            // Still in voting window.
            return Ok(p.status);
        }
        let mut yes = U256::ZERO;
        let mut no = U256::ZERO;
        let mut vote_prefix = Vec::with_capacity(VOTE_PREFIX.len() + 8);
        vote_prefix.extend_from_slice(VOTE_PREFIX);
        vote_prefix.extend_from_slice(&id.to_be_bytes());
        for kv in self.backend.iter_prefix(ColumnFamily::Meta, &vote_prefix) {
            let (k, v) = kv?;
            let suffix = &k[vote_prefix.len()..];
            if suffix.len() != 20 {
                continue;
            }
            let mut addr = [0u8; 20];
            addr.copy_from_slice(suffix);
            let Some(vote) = Self::decode_vote(id, Address::new(addr), &v) else {
                continue;
            };
            if vote.support {
                yes = yes.saturating_add(vote.weight_wei);
            } else {
                no = no.saturating_add(vote.weight_wei);
            }
        }
        let total = table.total_bonded()?;
        // Pass condition: yes * 3 > total * 2 (i.e. yes > 2/3 of total)
        // AND no must not also exceed 1/3 (handled implicitly by
        // forcing yes > 2/3 of total).
        let pass = yes.saturating_mul(U256::from(3u64)) > total.saturating_mul(U256::from(2u64))
            && !total.is_zero();
        p.status = if pass {
            ProposalStatus::Passed
        } else {
            ProposalStatus::Rejected
        };
        self.backend.put(
            ColumnFamily::Meta,
            &Self::prop_key(id),
            &Self::encode_proposal(&p),
        )?;
        // Cache the tally bytes so explorers don't re-scan: `yes_be32 ‖ no_be32`.
        let mut tally_bytes = [0u8; 64];
        tally_bytes[..32].copy_from_slice(&yes.to_be_bytes::<32>());
        tally_bytes[32..].copy_from_slice(&no.to_be_bytes::<32>());
        self.backend
            .put(ColumnFamily::Meta, &Self::tally_key(id), &tally_bytes)?;
        Ok(p.status)
    }

    /// Read the cached `(yes, no)` tally for `id`. Returns `Ok(None)`
    /// before [`Self::tally`] has finalised the proposal.
    ///
    /// # Errors
    /// Propagates backend / decode errors.
    pub fn tally_of(
        &self,
        id: u64,
    ) -> Result<Option<(U256, U256)>, Box<dyn std::error::Error + Send + Sync>> {
        let Some(bytes) = self.backend.get(ColumnFamily::Meta, &Self::tally_key(id))? else {
            return Ok(None);
        };
        if bytes.len() != 64 {
            return Ok(None);
        }
        let mut yes = [0u8; 32];
        yes.copy_from_slice(&bytes[..32]);
        let mut no = [0u8; 32];
        no.copy_from_slice(&bytes[32..]);
        Ok(Some((U256::from_be_bytes(yes), U256::from_be_bytes(no))))
    }

    /// List every recorded proposal in unspecified order.
    ///
    /// # Errors
    /// Propagates backend errors.
    pub fn list_all(&self) -> Result<Vec<Proposal>, Box<dyn std::error::Error + Send + Sync>> {
        let mut out = Vec::new();
        for kv in self.backend.iter_prefix(ColumnFamily::Meta, PROP_PREFIX) {
            let (_k, v) = kv?;
            if let Some(p) = Self::decode_proposal(&v) {
                out.push(p);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aii_storage::RocksDbBackend;

    fn fresh() -> (Arc<RocksDbBackend>, Governance, StakeTable) {
        let backend = Arc::new(RocksDbBackend::open_in_temp().unwrap());
        let gov = Governance::new(Arc::clone(&backend));
        let table = StakeTable::new(Arc::clone(&backend));
        (backend, gov, table)
    }

    #[test]
    fn propose_assigns_monotonic_id() {
        let (_b, gov, _t) = fresh();
        let p = Address::new([0xa1; 20]);
        let id1 = gov.propose(p, "raise gas limit".into(), 100).unwrap();
        let id2 = gov.propose(p, "lower base fee".into(), 200).unwrap();
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
    }

    #[test]
    fn propose_then_get_round_trips() {
        let (_b, gov, _t) = fresh();
        let p = Address::new([0xa1; 20]);
        let id = gov
            .propose(p, "increase block reward".into(), 1_000)
            .unwrap();
        let back = gov.get(id).unwrap().unwrap();
        assert_eq!(back.id, id);
        assert_eq!(back.title, "increase block reward");
        assert_eq!(back.voting_ends_at, 1_000);
        assert_eq!(back.status, ProposalStatus::Pending);
        assert_eq!(back.proposer, p);
    }

    #[test]
    fn cast_vote_requires_bonded_stake() {
        let (_b, gov, table) = fresh();
        let voter = Address::new([0xa1; 20]);
        let id = gov.propose(voter, "x".into(), 100).unwrap();
        // No stake yet.
        assert!(gov.cast_vote(&table, id, voter, true, 1).is_err());
        // Bond, then vote works.
        table.bond(&voter, U256::from(1_000u64)).unwrap();
        let v = gov.cast_vote(&table, id, voter, true, 1).unwrap();
        assert_eq!(v.weight_wei, U256::from(1_000u64));
        assert!(v.support);
    }

    #[test]
    fn cast_vote_rejected_after_voting_window() {
        let (_b, gov, table) = fresh();
        let voter = Address::new([0xa1; 20]);
        table.bond(&voter, U256::from(1_000u64)).unwrap();
        let id = gov.propose(voter, "x".into(), 50).unwrap();
        // Block 60 > voting_ends_at 50 → rejected.
        assert!(gov.cast_vote(&table, id, voter, true, 60).is_err());
    }

    #[test]
    fn tally_passes_with_2_3_supermajority() {
        let (_b, gov, table) = fresh();
        let proposer = Address::new([0xa1; 20]);
        let big_yes = Address::new([0xb1; 20]);
        let small_no = Address::new([0xb2; 20]);
        // 1000 bonded total: 800 yes, 200 no → yes is 80% → passes.
        table.bond(&big_yes, U256::from(800u64)).unwrap();
        table.bond(&small_no, U256::from(200u64)).unwrap();
        let id = gov.propose(proposer, "x".into(), 50).unwrap();
        gov.cast_vote(&table, id, big_yes, true, 1).unwrap();
        gov.cast_vote(&table, id, small_no, false, 1).unwrap();
        let status = gov.tally(&table, id, 60).unwrap();
        assert_eq!(status, ProposalStatus::Passed);
    }

    #[test]
    fn tally_rejects_below_2_3_supermajority() {
        let (_b, gov, table) = fresh();
        let proposer = Address::new([0xa1; 20]);
        let yes = Address::new([0xb1; 20]);
        let no = Address::new([0xb2; 20]);
        // 1000 bonded total: 500 yes, 500 no → yes is 50% → rejects.
        table.bond(&yes, U256::from(500u64)).unwrap();
        table.bond(&no, U256::from(500u64)).unwrap();
        let id = gov.propose(proposer, "x".into(), 50).unwrap();
        gov.cast_vote(&table, id, yes, true, 1).unwrap();
        gov.cast_vote(&table, id, no, false, 1).unwrap();
        let status = gov.tally(&table, id, 60).unwrap();
        assert_eq!(status, ProposalStatus::Rejected);
    }

    #[test]
    fn tally_caches_yes_no_totals() {
        let (_b, gov, table) = fresh();
        let p = Address::new([0xa1; 20]);
        let yes = Address::new([0xb1; 20]);
        table.bond(&yes, U256::from(1_000u64)).unwrap();
        let id = gov.propose(p, "x".into(), 50).unwrap();
        gov.cast_vote(&table, id, yes, true, 1).unwrap();
        gov.tally(&table, id, 60).unwrap();
        let (y, n) = gov.tally_of(id).unwrap().unwrap();
        assert_eq!(y, U256::from(1_000u64));
        assert_eq!(n, U256::ZERO);
    }
}
