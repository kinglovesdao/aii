//! Deterministic BFT capacity budget checks.
//!
//! These helpers do not replace real公网 stress testing. They encode the
//! protocol-level budget that makes the "tens of millions of nodes, 30 s
//! finality" target plausible: only a capped DPoS committee participates
//! in each BFT round, while the rest of the network observes, syncs, and
//! gossips blocks outside the voting quorum.

use thiserror::Error;

use crate::bft::MAX_VALIDATORS;
use crate::wire::{MAX_PROPOSAL_BODY_LEN, PROPOSAL_HEADER_LEN, VOTE_LEN};

/// Required finality target from the public roadmap: one PoS block
/// should finalise within 30 seconds.
pub const FINALITY_TARGET_SECS: u64 = 30;

/// BFT uses PRE-VOTE and PRE-COMMIT phases for every successful round.
pub const VOTE_PHASES_PER_ROUND: u64 = 2;

/// Capacity budget for one successful height/round.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapacityBudget {
    /// Active validator count in the DPoS/BFT committee.
    pub validators: usize,
    /// Target seconds available for the round.
    pub target_secs: u64,
    /// Proposal bytes emitted by the leader before peer fan-out.
    pub proposal_bytes: usize,
    /// Equal-stake validators required to cross 2/3 + 1 quorum.
    pub equal_stake_quorum_votes: usize,
    /// Committee-wide vote messages in a full-mesh broadcast model.
    pub vote_messages_per_round: u64,
    /// Committee-wide vote payload bytes in a full-mesh broadcast model.
    pub vote_payload_bytes_per_round: u64,
    /// Leader upload bytes for sending the proposal to every other validator.
    pub leader_proposal_fanout_bytes: u64,
    /// Minimum leader upload bandwidth for proposal fan-out within
    /// `target_secs`, in megabits/s.
    pub min_leader_upload_mbps: u64,
}

impl CapacityBudget {
    /// Return `true` when the active committee respects the consensus
    /// bitmap cap and the target is no slower than the roadmap target.
    #[must_use]
    pub const fn satisfies_design_cap(self) -> bool {
        self.validators <= MAX_VALIDATORS && self.target_secs <= FINALITY_TARGET_SECS
    }
}

/// Invalid input for [`capacity_budget`].
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum CapacityError {
    /// Committee is empty.
    #[error("validator count must be > 0")]
    EmptyValidatorSet,
    /// Committee exceeds the BFT bitmap cap.
    #[error("validator count {got} exceeds maximum {max}")]
    TooManyValidators {
        /// Supplied validators.
        got: usize,
        /// Maximum accepted validators.
        max: usize,
    },
    /// Target finality seconds is zero.
    #[error("target seconds must be > 0")]
    ZeroTarget,
    /// Proposal body exceeds the wire codec limit.
    #[error("proposal bytes {got} exceeds maximum {max}")]
    ProposalTooLarge {
        /// Supplied proposal size.
        got: usize,
        /// Maximum accepted proposal size.
        max: usize,
    },
}

/// Build a deterministic capacity budget for one successful round.
///
/// The model is deliberately conservative for votes: every validator's
/// PRE-VOTE and PRE-COMMIT are broadcast to every other active validator.
/// This is a committee-level bound, not a claim about all observer/light
/// nodes in the global network.
///
/// # Errors
/// Rejects empty/oversized validator sets, zero target seconds, or a
/// proposal larger than the wire codec limit.
pub fn capacity_budget(
    validators: usize,
    proposal_bytes: usize,
    target_secs: u64,
) -> Result<CapacityBudget, CapacityError> {
    if validators == 0 {
        return Err(CapacityError::EmptyValidatorSet);
    }
    if validators > MAX_VALIDATORS {
        return Err(CapacityError::TooManyValidators {
            got: validators,
            max: MAX_VALIDATORS,
        });
    }
    if target_secs == 0 {
        return Err(CapacityError::ZeroTarget);
    }
    if proposal_bytes > max_wire_proposal_bytes() {
        return Err(CapacityError::ProposalTooLarge {
            got: proposal_bytes,
            max: max_wire_proposal_bytes(),
        });
    }

    let vote_messages_per_round = vote_messages_per_round(validators);
    let vote_payload_bytes_per_round = vote_messages_per_round * VOTE_LEN as u64;
    let leader_proposal_fanout_bytes =
        (proposal_bytes as u64).saturating_mul(validators.saturating_sub(1) as u64);
    let min_leader_upload_mbps = mbps_required(leader_proposal_fanout_bytes, target_secs).max(1);

    Ok(CapacityBudget {
        validators,
        target_secs,
        proposal_bytes,
        equal_stake_quorum_votes: equal_stake_quorum_votes(validators),
        vote_messages_per_round,
        vote_payload_bytes_per_round,
        leader_proposal_fanout_bytes,
        min_leader_upload_mbps,
    })
}

/// Maximum proposal bytes accepted by the wire codec, including header.
#[must_use]
pub const fn max_wire_proposal_bytes() -> usize {
    PROPOSAL_HEADER_LEN + MAX_PROPOSAL_BODY_LEN
}

/// Equal-stake 2/3 + 1 quorum count for `validators`.
#[must_use]
pub const fn equal_stake_quorum_votes(validators: usize) -> usize {
    (validators * 2) / 3 + 1
}

/// Committee-wide full-mesh vote messages for one successful round.
#[must_use]
pub const fn vote_messages_per_round(validators: usize) -> u64 {
    let n = validators as u64;
    VOTE_PHASES_PER_ROUND * n * n.saturating_sub(1)
}

const fn mbps_required(bytes: u64, seconds: u64) -> u64 {
    let bits = bytes.saturating_mul(8);
    bits.div_ceil(seconds).div_ceil(1_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_and_bft_validator_caps_match() {
        assert_eq!(aii_config::MAX_ACTIVE_VALIDATORS as usize, MAX_VALIDATORS);
    }

    #[test]
    fn max_committee_budget_satisfies_roadmap_target() {
        let budget = capacity_budget(MAX_VALIDATORS, max_wire_proposal_bytes(), 30).unwrap();
        assert!(budget.satisfies_design_cap());
        assert_eq!(budget.equal_stake_quorum_votes, 86);
        assert_eq!(budget.vote_messages_per_round, 32_512);
        assert_eq!(budget.vote_payload_bytes_per_round, 4_714_240);
        assert!(
            budget.min_leader_upload_mbps <= 600,
            "max 16 MiB proposal fan-out to 127 peers should fit under a 600 Mbps leader uplink budget"
        );
    }

    #[test]
    fn default_committee_has_small_vote_payload() {
        let budget = capacity_budget(21, max_wire_proposal_bytes(), FINALITY_TARGET_SECS).unwrap();
        assert_eq!(budget.equal_stake_quorum_votes, 15);
        assert_eq!(budget.vote_messages_per_round, 840);
        assert_eq!(budget.vote_payload_bytes_per_round, 121_800);
    }

    #[test]
    fn oversized_committee_rejected() {
        let err = capacity_budget(MAX_VALIDATORS + 1, 0, FINALITY_TARGET_SECS).unwrap_err();
        assert_eq!(
            err,
            CapacityError::TooManyValidators {
                got: MAX_VALIDATORS + 1,
                max: MAX_VALIDATORS,
            }
        );
    }

    #[test]
    fn oversized_proposal_rejected() {
        let err =
            capacity_budget(1, max_wire_proposal_bytes() + 1, FINALITY_TARGET_SECS).unwrap_err();
        assert_eq!(
            err,
            CapacityError::ProposalTooLarge {
                got: max_wire_proposal_bytes() + 1,
                max: max_wire_proposal_bytes(),
            }
        );
    }
}
