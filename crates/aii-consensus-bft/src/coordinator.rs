//! BFT-PoS stage 3: round-change coordinator (v0.0.26).
//!
//! [`RoundCoordinator`] orchestrates one height through the two-phase
//! BFT lifecycle introduced in stage 2, driving rounds forward when
//! the network is too slow to reach quorum.
//!
//! ## What this layer does
//!
//! - Tracks the current `(height, round, phase)`.
//! - Accepts a proposal once per round, verifying the leader's
//!   [`LeaderProof`] against the expected proposer for this round.
//! - Forwards [`PrevoteVote`]s to an inner [`PrevoteTallier`]; on
//!   ⅔+1 stake quorum, captures the [`PolcCertificate`] and transitions
//!   to [`Phase::Precommitting`].
//! - Forwards [`PrecommitVote`]s to an inner [`PrecommitTallier`]; on
//!   ⅔+1 stake quorum, captures the [`PrecommitCertificate`] and
//!   transitions to [`Phase::Committed`].
//! - On [`RoundCoordinator::fire_timeout`], advances to the next round
//!   (unless already [`Phase::Committed`]), resetting tallies and
//!   re-deriving the leader for the new round.
//!
//! ## What this layer does NOT do
//!
//! - Networking / gossip — the host wires events in.
//! - Timeout scheduling — the host fires `fire_timeout()` when its
//!   external clock decides the round is dead.
//! - Locking / POL preservation across rounds (v0.0.27+).
//! - Equivocation detection / slashing (v0.0.27+).
//! - Block production — the leader's block hash arrives via
//!   `submit_proposal` from a higher layer.

use aii_types::H256;

use crate::bft::{
    LeaderProof, Phase, PolcCertificate, PrecommitCertificate, PrecommitTallier, PrecommitVote,
    PrevoteTallier, PrevoteVote, TallyState, ValidatorSet,
};
use crate::BftError;

/// Round-change coordinator. One instance per height.
pub struct RoundCoordinator {
    height: u64,
    round: u32,
    phase: Phase,
    vs: ValidatorSet,
    seed: [u8; 32],
    proposed_block: Option<H256>,
    prevote_tally: Option<PrevoteTallier>,
    precommit_tally: Option<PrecommitTallier>,
    polc: Option<PolcCertificate>,
    final_cert: Option<PrecommitCertificate>,
}

impl RoundCoordinator {
    /// Build a coordinator starting at round 0 of `height` with the
    /// given validator set and cross-height `seed`.
    pub const fn new(height: u64, seed: [u8; 32], vs: ValidatorSet) -> Self {
        Self {
            height,
            round: 0,
            phase: Phase::AwaitingProposal,
            vs,
            seed,
            proposed_block: None,
            prevote_tally: None,
            precommit_tally: None,
            polc: None,
            final_cert: None,
        }
    }

    /// Current phase.
    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    /// Current round number within this height.
    #[must_use]
    pub const fn round(&self) -> u32 {
        self.round
    }

    /// Block height this coordinator is finalising.
    #[must_use]
    pub const fn height(&self) -> u64 {
        self.height
    }

    /// Validator index of the leader for the **current** round.
    #[must_use]
    pub fn leader_index(&self) -> usize {
        self.vs.select_leader(self.height, self.round, &self.seed)
    }

    /// Block hash of the accepted proposal, if any has arrived in this round.
    #[must_use]
    pub const fn proposed_block(&self) -> Option<H256> {
        self.proposed_block
    }

    /// POLC, if PRE-VOTE quorum has been reached in this round.
    #[must_use]
    pub const fn polc(&self) -> Option<&PolcCertificate> {
        self.polc.as_ref()
    }

    /// Final commit certificate, if the block has been committed.
    #[must_use]
    pub const fn certificate(&self) -> Option<&PrecommitCertificate> {
        self.final_cert.as_ref()
    }

    /// Submit the leader's proposal for the current round.
    ///
    /// Validates `leader_proof` against the expected proposer's VRF
    /// pubkey. On success, transitions from
    /// [`Phase::AwaitingProposal`] to [`Phase::Prevoting`] and arms a
    /// fresh [`PrevoteTallier`].
    pub fn submit_proposal(
        &mut self,
        block_hash: H256,
        leader_proof: &LeaderProof,
    ) -> Result<(), BftError> {
        if self.phase != Phase::AwaitingProposal {
            return Err(BftError::WrongPhase {
                expected: Phase::AwaitingProposal,
                actual: self.phase,
            });
        }
        let leader_idx = self.leader_index();
        let leader_vrf_pk = &self.vs.validators()[leader_idx].vrf_pubkey;
        leader_proof.verify(leader_vrf_pk, self.height, self.round, &self.seed)?;
        self.proposed_block = Some(block_hash);
        self.prevote_tally = Some(PrevoteTallier::new(
            block_hash,
            self.height,
            self.round,
            self.vs.clone(),
        ));
        self.phase = Phase::Prevoting;
        Ok(())
    }

    /// Submit a PRE-VOTE in the current round.
    ///
    /// Validates the current phase, then forwards to the inner
    /// [`PrevoteTallier`]. If this vote pushes accumulated stake to
    /// ⅔+1, captures the [`PolcCertificate`] and transitions to
    /// [`Phase::Precommitting`].
    pub fn submit_prevote(&mut self, vote: PrevoteVote) -> Result<(), BftError> {
        if self.phase != Phase::Prevoting {
            return Err(BftError::WrongPhase {
                expected: Phase::Prevoting,
                actual: self.phase,
            });
        }
        let tally = self
            .prevote_tally
            .as_mut()
            .expect("invariant: tally exists in Prevoting");
        let state = tally.submit(vote)?;
        if state == TallyState::ReachedQuorum {
            let polc = tally
                .try_form_polc()
                .expect("invariant: tally at quorum forms POLC");
            let block = self
                .proposed_block
                .expect("invariant: block set in Prevoting");
            self.precommit_tally = Some(PrecommitTallier::new(
                block,
                self.height,
                self.round,
                self.vs.clone(),
            ));
            self.polc = Some(polc);
            self.phase = Phase::Precommitting;
        }
        Ok(())
    }

    /// Submit a PRE-COMMIT in the current round.
    ///
    /// Validates the current phase, then forwards to the inner
    /// [`PrecommitTallier`]. If this vote pushes accumulated stake to
    /// ⅔+1, captures the [`PrecommitCertificate`] and transitions to
    /// [`Phase::Committed`].
    pub fn submit_precommit(&mut self, vote: PrecommitVote) -> Result<(), BftError> {
        if self.phase != Phase::Precommitting {
            return Err(BftError::WrongPhase {
                expected: Phase::Precommitting,
                actual: self.phase,
            });
        }
        let tally = self
            .precommit_tally
            .as_mut()
            .expect("invariant: tally exists in Precommitting");
        let state = tally.submit(vote)?;
        if state == TallyState::ReachedQuorum {
            let cert = tally
                .try_finalize()
                .expect("invariant: tally at quorum forms cert");
            self.final_cert = Some(cert);
            self.phase = Phase::Committed;
        }
        Ok(())
    }

    /// Round timed out without reaching finality — advance to the next
    /// round. No-op if already [`Phase::Committed`]. Clears the proposal,
    /// tallies, and POLC; the new round starts fresh.
    pub fn fire_timeout(&mut self) {
        if self.phase == Phase::Committed {
            return;
        }
        self.round = self.round.saturating_add(1);
        self.phase = Phase::AwaitingProposal;
        self.proposed_block = None;
        self.prevote_tally = None;
        self.precommit_tally = None;
        self.polc = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aii_crypto::bls::SecretKey as BlsSecretKey;
    use aii_crypto::vrf::SecretKey as VrfSecretKey;

    use crate::bft::Validator;

    fn bls_sk(seed: u8) -> BlsSecretKey {
        BlsSecretKey::from_ikm(&[seed; 32], b"AII-COORD-TEST").unwrap()
    }
    fn vrf_sk() -> VrfSecretKey {
        VrfSecretKey::generate()
    }

    /// 3 validators of stake 100 each; total 300 ⇒ quorum 201.
    fn three_equal() -> (ValidatorSet, Vec<(BlsSecretKey, VrfSecretKey)>) {
        let mut keys = Vec::new();
        let mut vs_list = Vec::new();
        for i in 0..3u8 {
            let bls = bls_sk(i + 1);
            let vrf = vrf_sk();
            vs_list.push(Validator {
                bls_pubkey: bls.public_key(),
                vrf_pubkey: vrf.public_key(),
                stake: 100,
            });
            keys.push((bls, vrf));
        }
        (ValidatorSet::new(vs_list).unwrap(), keys)
    }

    const SEED: [u8; 32] = [0x42; 32];

    /// Drive the coordinator from `AwaitingProposal` all the way to
    /// quorum-on-prevote → `Precommitting`, returning the block hash
    /// and all signing keys.
    fn drive_to_precommitting(
        coord: &mut RoundCoordinator,
        keys: &[(BlsSecretKey, VrfSecretKey)],
    ) -> H256 {
        let leader_idx = coord.leader_index();
        let proof = LeaderProof::produce(&keys[leader_idx].1, coord.height(), coord.round(), &SEED);
        let block = H256::new([0xbb; 32]);
        coord.submit_proposal(block, &proof).unwrap();
        for (i, (bls, _)) in keys.iter().enumerate() {
            let vote = PrevoteVote::sign(
                bls,
                block,
                coord.height(),
                coord.round(),
                u32::try_from(i).unwrap(),
            );
            coord.submit_prevote(vote).unwrap();
        }
        block
    }

    #[test]
    fn coordinator_starts_in_awaiting_proposal_round_0() {
        let (vs, _) = three_equal();
        let coord = RoundCoordinator::new(1, SEED, vs);
        assert_eq!(coord.phase(), Phase::AwaitingProposal);
        assert_eq!(coord.round(), 0);
        assert_eq!(coord.height(), 1);
        assert!(coord.proposed_block().is_none());
        assert!(coord.polc().is_none());
        assert!(coord.certificate().is_none());
    }

    #[test]
    fn coordinator_leader_index_matches_validator_set() {
        let (vs, _) = three_equal();
        let coord = RoundCoordinator::new(1, SEED, vs.clone());
        let expected = vs.select_leader(1, 0, &SEED);
        assert_eq!(coord.leader_index(), expected);
    }

    #[test]
    fn submit_proposal_transitions_to_prevoting() {
        let (vs, keys) = three_equal();
        let mut coord = RoundCoordinator::new(1, SEED, vs);
        let leader_idx = coord.leader_index();
        let proof = LeaderProof::produce(&keys[leader_idx].1, 1, 0, &SEED);
        let block = H256::new([0xbb; 32]);
        coord.submit_proposal(block, &proof).unwrap();
        assert_eq!(coord.phase(), Phase::Prevoting);
        assert_eq!(coord.proposed_block(), Some(block));
    }

    #[test]
    fn submit_proposal_rejects_invalid_leader_proof() {
        let (vs, keys) = three_equal();
        let mut coord = RoundCoordinator::new(1, SEED, vs);
        let leader_idx = coord.leader_index();
        // Use the non-leader's VRF SK to forge a proof.
        let imposter_idx = (leader_idx + 1) % 3;
        let bad_proof = LeaderProof::produce(&keys[imposter_idx].1, 1, 0, &SEED);
        let block = H256::new([0xbb; 32]);
        assert_eq!(
            coord.submit_proposal(block, &bad_proof).unwrap_err(),
            BftError::InvalidVrfProof,
        );
        assert_eq!(coord.phase(), Phase::AwaitingProposal);
    }

    #[test]
    fn submit_proposal_rejects_when_already_prevoting() {
        let (vs, keys) = three_equal();
        let mut coord = RoundCoordinator::new(1, SEED, vs);
        let leader_idx = coord.leader_index();
        let proof = LeaderProof::produce(&keys[leader_idx].1, 1, 0, &SEED);
        let block = H256::new([0xbb; 32]);
        coord.submit_proposal(block, &proof).unwrap();
        let err = coord.submit_proposal(block, &proof).unwrap_err();
        assert!(
            matches!(
                err,
                BftError::WrongPhase {
                    expected: Phase::AwaitingProposal,
                    actual: Phase::Prevoting,
                }
            ),
            "got {err:?}",
        );
    }

    #[test]
    fn submit_prevote_during_awaiting_proposal_rejected() {
        let (vs, keys) = three_equal();
        let mut coord = RoundCoordinator::new(1, SEED, vs);
        let vote = PrevoteVote::sign(&keys[0].0, H256::new([0xbb; 32]), 1, 0, 0);
        let err = coord.submit_prevote(vote).unwrap_err();
        assert!(matches!(err, BftError::WrongPhase { .. }));
    }

    #[test]
    fn submit_precommit_during_prevoting_rejected() {
        let (vs, keys) = three_equal();
        let mut coord = RoundCoordinator::new(1, SEED, vs);
        let leader_idx = coord.leader_index();
        let proof = LeaderProof::produce(&keys[leader_idx].1, 1, 0, &SEED);
        let block = H256::new([0xbb; 32]);
        coord.submit_proposal(block, &proof).unwrap();
        let vote = PrecommitVote::sign(&keys[0].0, block, 1, 0, 0);
        let err = coord.submit_precommit(vote).unwrap_err();
        assert!(matches!(err, BftError::WrongPhase { .. }));
    }

    #[test]
    fn polc_formation_transitions_to_precommitting() {
        let (vs, keys) = three_equal();
        let mut coord = RoundCoordinator::new(1, SEED, vs);
        let _block = drive_to_precommitting(&mut coord, &keys);
        assert_eq!(coord.phase(), Phase::Precommitting);
        assert!(coord.polc().is_some());
    }

    #[test]
    fn finality_transitions_to_committed() {
        let (vs, keys) = three_equal();
        let mut coord = RoundCoordinator::new(1, SEED, vs);
        let block = drive_to_precommitting(&mut coord, &keys);
        for (i, (bls, _)) in keys.iter().enumerate() {
            let vote = PrecommitVote::sign(bls, block, 1, 0, u32::try_from(i).unwrap());
            coord.submit_precommit(vote).unwrap();
        }
        assert_eq!(coord.phase(), Phase::Committed);
        assert!(coord.certificate().is_some());
    }

    #[test]
    fn committed_state_certificate_verifies_against_validator_set() {
        let (vs, keys) = three_equal();
        let mut coord = RoundCoordinator::new(1, SEED, vs.clone());
        let block = drive_to_precommitting(&mut coord, &keys);
        for (i, (bls, _)) in keys.iter().enumerate() {
            coord
                .submit_precommit(PrecommitVote::sign(
                    bls,
                    block,
                    1,
                    0,
                    u32::try_from(i).unwrap(),
                ))
                .unwrap();
        }
        let cert = coord.certificate().unwrap();
        assert_eq!(cert.block_hash, block);
        assert_eq!(cert.height, 1);
        assert_eq!(cert.round, 0);
        cert.verify(&vs).unwrap();
    }

    #[test]
    fn timeout_in_awaiting_proposal_advances_round() {
        let (vs, _) = three_equal();
        let mut coord = RoundCoordinator::new(1, SEED, vs);
        coord.fire_timeout();
        assert_eq!(coord.round(), 1);
        assert_eq!(coord.phase(), Phase::AwaitingProposal);
    }

    #[test]
    fn timeout_in_prevoting_advances_round_and_clears_state() {
        let (vs, keys) = three_equal();
        let mut coord = RoundCoordinator::new(1, SEED, vs);
        let leader_idx = coord.leader_index();
        let proof = LeaderProof::produce(&keys[leader_idx].1, 1, 0, &SEED);
        let block = H256::new([0xbb; 32]);
        coord.submit_proposal(block, &proof).unwrap();
        coord.fire_timeout();
        assert_eq!(coord.round(), 1);
        assert_eq!(coord.phase(), Phase::AwaitingProposal);
        assert_eq!(coord.proposed_block(), None);
        assert!(coord.polc().is_none());
    }

    #[test]
    fn timeout_in_precommitting_advances_round_and_clears_polc() {
        let (vs, keys) = three_equal();
        let mut coord = RoundCoordinator::new(1, SEED, vs);
        drive_to_precommitting(&mut coord, &keys);
        assert!(coord.polc().is_some());
        coord.fire_timeout();
        assert_eq!(coord.round(), 1);
        assert_eq!(coord.phase(), Phase::AwaitingProposal);
        assert!(coord.polc().is_none());
    }

    #[test]
    fn timeout_in_committed_is_noop() {
        let (vs, keys) = three_equal();
        let mut coord = RoundCoordinator::new(1, SEED, vs);
        let block = drive_to_precommitting(&mut coord, &keys);
        for (i, (bls, _)) in keys.iter().enumerate() {
            coord
                .submit_precommit(PrecommitVote::sign(
                    bls,
                    block,
                    1,
                    0,
                    u32::try_from(i).unwrap(),
                ))
                .unwrap();
        }
        assert_eq!(coord.phase(), Phase::Committed);
        let cert_before = coord.certificate().unwrap().block_hash;
        coord.fire_timeout();
        // No change.
        assert_eq!(coord.phase(), Phase::Committed);
        assert_eq!(coord.round(), 0);
        assert_eq!(coord.certificate().unwrap().block_hash, cert_before);
    }

    #[test]
    fn round_one_validates_proof_against_round_one() {
        // After a timeout, the proposer for round 1 must sign over
        // (height=1, round=1, seed). A proof for round 0 must NOT
        // satisfy the round-1 coordinator.
        let (vs, keys) = three_equal();
        let mut coord = RoundCoordinator::new(1, SEED, vs);
        coord.fire_timeout();
        assert_eq!(coord.round(), 1);

        let r1_leader = coord.leader_index();
        let proof_for_round0 = LeaderProof::produce(&keys[r1_leader].1, 1, 0, &SEED);
        assert_eq!(
            coord
                .submit_proposal(H256::new([0xcc; 32]), &proof_for_round0)
                .unwrap_err(),
            BftError::InvalidVrfProof,
        );

        let proof_for_round1 = LeaderProof::produce(&keys[r1_leader].1, 1, 1, &SEED);
        coord
            .submit_proposal(H256::new([0xcc; 32]), &proof_for_round1)
            .unwrap();
        assert_eq!(coord.phase(), Phase::Prevoting);
    }

    #[test]
    fn submit_prevote_after_committed_rejected() {
        let (vs, keys) = three_equal();
        let mut coord = RoundCoordinator::new(1, SEED, vs);
        let block = drive_to_precommitting(&mut coord, &keys);
        for (i, (bls, _)) in keys.iter().enumerate() {
            coord
                .submit_precommit(PrecommitVote::sign(
                    bls,
                    block,
                    1,
                    0,
                    u32::try_from(i).unwrap(),
                ))
                .unwrap();
        }
        // Now Committed. Late prevotes rejected.
        let late = PrevoteVote::sign(&keys[0].0, block, 1, 0, 0);
        assert!(matches!(
            coord.submit_prevote(late).unwrap_err(),
            BftError::WrongPhase { .. }
        ));
    }

    #[test]
    fn forwarded_tally_errors_propagate() {
        // A vote with wrong block_hash should surface as WrongBlockHash
        // (forwarded straight from the inner tallier), not WrongPhase.
        let (vs, keys) = three_equal();
        let mut coord = RoundCoordinator::new(1, SEED, vs);
        let leader_idx = coord.leader_index();
        let proof = LeaderProof::produce(&keys[leader_idx].1, 1, 0, &SEED);
        let proposed = H256::new([0xbb; 32]);
        coord.submit_proposal(proposed, &proof).unwrap();
        let bad_vote = PrevoteVote::sign(&keys[0].0, H256::new([0xee; 32]), 1, 0, 0);
        assert_eq!(
            coord.submit_prevote(bad_vote).unwrap_err(),
            BftError::WrongBlockHash,
        );
    }
}
