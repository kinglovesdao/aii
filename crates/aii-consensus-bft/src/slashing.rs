//! BFT-PoS stage 5: equivocation detection (v0.0.28).
//!
//! A validator that signs two different blocks for the same `(height,
//! round, phase)` has equivocated — the on-chain protocol must be able
//! to prove it and trigger a slashing transaction. This module is the
//! **detector** that produces the evidence; the actual slashing
//! transaction lives in `aii-state` and is wired up by the chain
//! executor in a later release.
//!
//! ## What the detector does
//!
//! Keeps the first BLS-signed vote it sees per
//! `(validator_index, height, round)` for each phase. On a second vote
//! from the same validator at the same coordinates BUT for a different
//! block, it returns [`EquivocationEvidence`] — a sealed structure
//! carrying both conflicting votes so any verifier can re-check the
//! signatures and the block-hash mismatch independently.
//!
//! ## What this layer does NOT do
//!
//! - It does NOT punish: it just produces evidence. The slashing
//!   transaction (debiting stake, freezing the validator) is the
//!   chain-state job.
//! - It does NOT detect across-height conflicts (a validator signing
//!   different heights is normal — these are different votes).

use std::collections::BTreeMap;
use thiserror::Error;

use crate::bft::{PrecommitVote, PrevoteVote, ValidatorSet};

/// Key under which votes are deduplicated: `(validator_index, height, round)`.
type VoteKey = (u32, u64, u32);

/// Tracks one validator's first PRE-VOTE / PRE-COMMIT per
/// `(height, round)` so a subsequent conflicting vote becomes a
/// slashable [`EquivocationEvidence`].
#[derive(Default)]
pub struct EquivocationDetector {
    seen_prevote: BTreeMap<VoteKey, PrevoteVote>,
    seen_precommit: BTreeMap<VoteKey, PrecommitVote>,
}

impl EquivocationDetector {
    /// Fresh detector with no recorded votes.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a PRE-VOTE. Returns [`EquivocationEvidence::Prevote`] iff
    /// this validator has previously signed a PRE-VOTE for the same
    /// `(height, round)` over a **different** `block_hash`. An identical
    /// re-submission of the same vote returns `None` (idempotent).
    pub fn record_prevote(&mut self, vote: PrevoteVote) -> Option<EquivocationEvidence> {
        let key = (vote.validator_index, vote.height, vote.round);
        if let Some(prior) = self.seen_prevote.get(&key) {
            if prior.block_hash != vote.block_hash {
                return Some(EquivocationEvidence::Prevote {
                    conflicting: [prior.clone(), vote],
                });
            }
            return None;
        }
        self.seen_prevote.insert(key, vote);
        None
    }

    /// Record a PRE-COMMIT. Same semantics as [`Self::record_prevote`].
    pub fn record_precommit(&mut self, vote: PrecommitVote) -> Option<EquivocationEvidence> {
        let key = (vote.validator_index, vote.height, vote.round);
        if let Some(prior) = self.seen_precommit.get(&key) {
            if prior.block_hash != vote.block_hash {
                return Some(EquivocationEvidence::Precommit {
                    conflicting: [prior.clone(), vote],
                });
            }
            return None;
        }
        self.seen_precommit.insert(key, vote);
        None
    }
}

/// Proof that a single validator signed two conflicting votes at the
/// same `(height, round, phase)`. Both signatures are present so any
/// node can independently re-verify the slashing claim.
#[derive(Clone, Debug)]
pub enum EquivocationEvidence {
    /// Two PRE-VOTES for different blocks at the same `(height, round)`.
    Prevote {
        /// The two conflicting prevotes (block hashes differ).
        conflicting: [PrevoteVote; 2],
    },
    /// Two PRE-COMMITS for different blocks at the same `(height, round)`.
    Precommit {
        /// The two conflicting precommits (block hashes differ).
        conflicting: [PrecommitVote; 2],
    },
}

impl EquivocationEvidence {
    /// Validator index implicated by this evidence (both votes share it).
    #[must_use]
    pub const fn validator_index(&self) -> u32 {
        match self {
            Self::Prevote { conflicting } => conflicting[0].validator_index,
            Self::Precommit { conflicting } => conflicting[0].validator_index,
        }
    }

    /// Heigh of the equivocation (both votes share it).
    #[must_use]
    pub const fn height(&self) -> u64 {
        match self {
            Self::Prevote { conflicting } => conflicting[0].height,
            Self::Precommit { conflicting } => conflicting[0].height,
        }
    }

    /// Round of the equivocation (both votes share it).
    #[must_use]
    pub const fn round(&self) -> u32 {
        match self {
            Self::Prevote { conflicting } => conflicting[0].round,
            Self::Precommit { conflicting } => conflicting[0].round,
        }
    }

    /// Independently re-check the evidence: both signatures must
    /// verify under the same validator's pubkey, and the two block
    /// hashes must differ. Returns `Ok(())` iff the evidence really is
    /// slashable.
    pub fn verify(&self, vs: &ValidatorSet) -> Result<(), SlashingError> {
        match self {
            Self::Prevote { conflicting } => {
                let [a, b] = conflicting;
                if a.validator_index != b.validator_index {
                    return Err(SlashingError::Mismatch {
                        field: "validator_index",
                    });
                }
                if a.height != b.height {
                    return Err(SlashingError::Mismatch { field: "height" });
                }
                if a.round != b.round {
                    return Err(SlashingError::Mismatch { field: "round" });
                }
                if a.block_hash == b.block_hash {
                    return Err(SlashingError::SameBlock);
                }
                let v = vs
                    .get(a.validator_index as usize)
                    .ok_or(SlashingError::UnknownValidator(a.validator_index))?;
                let da = PrevoteVote::digest(&a.block_hash, a.height, a.round);
                let db = PrevoteVote::digest(&b.block_hash, b.height, b.round);
                a.bls_sig
                    .verify(da.as_bytes(), &v.bls_pubkey)
                    .map_err(|_| SlashingError::InvalidSignature)?;
                b.bls_sig
                    .verify(db.as_bytes(), &v.bls_pubkey)
                    .map_err(|_| SlashingError::InvalidSignature)?;
                Ok(())
            }
            Self::Precommit { conflicting } => {
                let [a, b] = conflicting;
                if a.validator_index != b.validator_index {
                    return Err(SlashingError::Mismatch {
                        field: "validator_index",
                    });
                }
                if a.height != b.height {
                    return Err(SlashingError::Mismatch { field: "height" });
                }
                if a.round != b.round {
                    return Err(SlashingError::Mismatch { field: "round" });
                }
                if a.block_hash == b.block_hash {
                    return Err(SlashingError::SameBlock);
                }
                let v = vs
                    .get(a.validator_index as usize)
                    .ok_or(SlashingError::UnknownValidator(a.validator_index))?;
                let da = PrecommitVote::digest(&a.block_hash, a.height, a.round);
                let db = PrecommitVote::digest(&b.block_hash, b.height, b.round);
                a.bls_sig
                    .verify(da.as_bytes(), &v.bls_pubkey)
                    .map_err(|_| SlashingError::InvalidSignature)?;
                b.bls_sig
                    .verify(db.as_bytes(), &v.bls_pubkey)
                    .map_err(|_| SlashingError::InvalidSignature)?;
                Ok(())
            }
        }
    }
}

/// Errors produced by [`EquivocationEvidence::verify`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SlashingError {
    /// The two votes target the same block — they are duplicates, not
    /// an equivocation.
    #[error("not equivocation: both votes target the same block hash")]
    SameBlock,

    /// The two votes disagree on `validator_index` / `height` / `round`.
    /// The evidence is malformed.
    #[error("evidence mismatch: votes disagree on {field}")]
    Mismatch {
        /// Which coordinate disagrees: "validator_index", "height", "round".
        field: &'static str,
    },

    /// `validator_index` is outside the set.
    #[error("validator index {0} out of bounds for set")]
    UnknownValidator(u32),

    /// At least one signature failed to verify against the validator's
    /// pubkey.
    #[error("BLS signature in evidence failed to verify")]
    InvalidSignature,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bft::{PrecommitVote, PrevoteVote, Validator};
    use aii_crypto::bls::SecretKey as BlsSecretKey;
    use aii_crypto::vrf::SecretKey as VrfSecretKey;
    use aii_types::H256;

    fn bls_sk(seed: u8) -> BlsSecretKey {
        BlsSecretKey::from_ikm(&[seed; 32], b"AII-SLASH-TEST").unwrap()
    }

    /// 3-validator set indexed 0..3; returns the set and matching BLS SKs.
    fn three_validators() -> (ValidatorSet, Vec<BlsSecretKey>) {
        let mut keys = Vec::new();
        let mut vs_list = Vec::new();
        for i in 0..3u8 {
            let bls = bls_sk(i + 1);
            let vrf = VrfSecretKey::generate();
            vs_list.push(Validator {
                bls_pubkey: bls.public_key(),
                vrf_pubkey: vrf.public_key(),
                stake: 100,
            });
            keys.push(bls);
        }
        (ValidatorSet::new(vs_list).unwrap(), keys)
    }

    fn block(b: u8) -> H256 {
        H256::new([b; 32])
    }

    #[test]
    fn detector_starts_empty() {
        let _d = EquivocationDetector::new();
        // Implicit: just constructing it should succeed.
    }

    #[test]
    fn first_prevote_returns_no_evidence() {
        let (_, sks) = three_validators();
        let mut d = EquivocationDetector::new();
        let v = PrevoteVote::sign(&sks[0], block(1), 5, 0, 0);
        assert!(d.record_prevote(v).is_none());
    }

    #[test]
    fn duplicate_prevote_for_same_block_no_evidence() {
        let (_, sks) = three_validators();
        let mut d = EquivocationDetector::new();
        let v1 = PrevoteVote::sign(&sks[0], block(1), 5, 0, 0);
        let v2 = PrevoteVote::sign(&sks[0], block(1), 5, 0, 0);
        let _ = d.record_prevote(v1);
        assert!(d.record_prevote(v2).is_none());
    }

    #[test]
    fn conflicting_prevotes_produce_evidence() {
        let (_, sks) = three_validators();
        let mut d = EquivocationDetector::new();
        let v1 = PrevoteVote::sign(&sks[0], block(1), 5, 0, 0);
        let v2 = PrevoteVote::sign(&sks[0], block(2), 5, 0, 0);
        d.record_prevote(v1);
        let ev = d.record_prevote(v2).expect("evidence expected");
        match ev {
            EquivocationEvidence::Prevote { conflicting } => {
                assert_eq!(conflicting[0].block_hash, block(1));
                assert_eq!(conflicting[1].block_hash, block(2));
            }
            EquivocationEvidence::Precommit { .. } => panic!("expected Prevote evidence"),
        }
    }

    #[test]
    fn different_validators_independent() {
        let (_, sks) = three_validators();
        let mut d = EquivocationDetector::new();
        d.record_prevote(PrevoteVote::sign(&sks[0], block(1), 5, 0, 0));
        // Validator 1 signing block 2 at the same (height, round) is NOT
        // equivocation — different validators.
        let v = PrevoteVote::sign(&sks[1], block(2), 5, 0, 1);
        assert!(d.record_prevote(v).is_none());
    }

    #[test]
    fn different_rounds_independent() {
        let (_, sks) = three_validators();
        let mut d = EquivocationDetector::new();
        d.record_prevote(PrevoteVote::sign(&sks[0], block(1), 5, 0, 0));
        // Same validator + height, but different round is fine.
        let v = PrevoteVote::sign(&sks[0], block(2), 5, 1, 0);
        assert!(d.record_prevote(v).is_none());
    }

    #[test]
    fn different_heights_independent() {
        let (_, sks) = three_validators();
        let mut d = EquivocationDetector::new();
        d.record_prevote(PrevoteVote::sign(&sks[0], block(1), 5, 0, 0));
        let v = PrevoteVote::sign(&sks[0], block(2), 6, 0, 0);
        assert!(d.record_prevote(v).is_none());
    }

    #[test]
    fn conflicting_precommits_produce_evidence() {
        let (_, sks) = three_validators();
        let mut d = EquivocationDetector::new();
        d.record_precommit(PrecommitVote::sign(&sks[0], block(1), 5, 0, 0));
        let ev = d
            .record_precommit(PrecommitVote::sign(&sks[0], block(2), 5, 0, 0))
            .expect("evidence expected");
        assert!(matches!(ev, EquivocationEvidence::Precommit { .. }));
    }

    #[test]
    fn prevote_and_precommit_streams_are_independent() {
        // A validator signing the same (h,r) as both PRE-VOTE for one
        // block and PRE-COMMIT for another is NOT equivocation in this
        // detector — phases are tracked separately. Cross-phase
        // contradictions are handled by the digest domain separation
        // (a precommit sig won't verify as a prevote anyway).
        let (_, sks) = three_validators();
        let mut d = EquivocationDetector::new();
        d.record_prevote(PrevoteVote::sign(&sks[0], block(1), 5, 0, 0));
        let precommit = PrecommitVote::sign(&sks[0], block(2), 5, 0, 0);
        assert!(d.record_precommit(precommit).is_none());
    }

    #[test]
    fn evidence_verify_accepts_real_equivocation() {
        let (vs, sks) = three_validators();
        let mut d = EquivocationDetector::new();
        d.record_prevote(PrevoteVote::sign(&sks[0], block(1), 5, 0, 0));
        let ev = d
            .record_prevote(PrevoteVote::sign(&sks[0], block(2), 5, 0, 0))
            .unwrap();
        ev.verify(&vs).unwrap();
    }

    #[test]
    fn evidence_validator_index_height_round_accessors() {
        let (_, sks) = three_validators();
        let mut d = EquivocationDetector::new();
        d.record_prevote(PrevoteVote::sign(&sks[0], block(1), 7, 3, 0));
        let ev = d
            .record_prevote(PrevoteVote::sign(&sks[0], block(2), 7, 3, 0))
            .unwrap();
        assert_eq!(ev.validator_index(), 0);
        assert_eq!(ev.height(), 7);
        assert_eq!(ev.round(), 3);
    }

    #[test]
    fn evidence_verify_rejects_when_same_block() {
        let (vs, sks) = three_validators();
        let v = PrevoteVote::sign(&sks[0], block(1), 5, 0, 0);
        let ev = EquivocationEvidence::Prevote {
            conflicting: [v.clone(), v],
        };
        assert_eq!(ev.verify(&vs).unwrap_err(), SlashingError::SameBlock);
    }

    #[test]
    fn evidence_verify_rejects_mismatched_validator_index() {
        let (vs, sks) = three_validators();
        let a = PrevoteVote::sign(&sks[0], block(1), 5, 0, 0);
        let b = PrevoteVote::sign(&sks[1], block(2), 5, 0, 1);
        let ev = EquivocationEvidence::Prevote {
            conflicting: [a, b],
        };
        assert_eq!(
            ev.verify(&vs).unwrap_err(),
            SlashingError::Mismatch {
                field: "validator_index",
            },
        );
    }

    #[test]
    fn evidence_verify_rejects_mismatched_round() {
        let (vs, sks) = three_validators();
        let a = PrevoteVote::sign(&sks[0], block(1), 5, 0, 0);
        let b = PrevoteVote::sign(&sks[0], block(2), 5, 1, 0);
        let ev = EquivocationEvidence::Prevote {
            conflicting: [a, b],
        };
        assert_eq!(
            ev.verify(&vs).unwrap_err(),
            SlashingError::Mismatch { field: "round" },
        );
    }

    #[test]
    fn evidence_verify_rejects_out_of_bounds_validator() {
        let (vs, sks) = three_validators();
        // Forge with validator_index 99 in both votes.
        let mut a = PrevoteVote::sign(&sks[0], block(1), 5, 0, 99);
        let mut b = PrevoteVote::sign(&sks[0], block(2), 5, 0, 99);
        a.validator_index = 99;
        b.validator_index = 99;
        let ev = EquivocationEvidence::Prevote {
            conflicting: [a, b],
        };
        assert_eq!(
            ev.verify(&vs).unwrap_err(),
            SlashingError::UnknownValidator(99),
        );
    }

    #[test]
    fn evidence_verify_rejects_invalid_signature() {
        // Forge: validator 0 in the metadata, but signatures are by sk_2.
        let (vs, sks) = three_validators();
        let a = PrevoteVote::sign(&sks[2], block(1), 5, 0, 0);
        let b = PrevoteVote::sign(&sks[2], block(2), 5, 0, 0);
        let ev = EquivocationEvidence::Prevote {
            conflicting: [a, b],
        };
        assert_eq!(ev.verify(&vs).unwrap_err(), SlashingError::InvalidSignature,);
    }

    #[test]
    fn precommit_evidence_round_trips_through_verify() {
        let (vs, sks) = three_validators();
        let mut d = EquivocationDetector::new();
        d.record_precommit(PrecommitVote::sign(&sks[0], block(1), 5, 0, 0));
        let ev = d
            .record_precommit(PrecommitVote::sign(&sks[0], block(2), 5, 0, 0))
            .unwrap();
        ev.verify(&vs).unwrap();
    }
}
