//! BFT-PoS stage 1: validator set, VRF leader selection, single-phase
//! PRE-COMMIT tally + BLS-aggregated finality certificate (v0.0.23).
//!
//! This module is the **pure on-chain finality state machine**. It does
//! not own a network, does not produce blocks, and is not (yet) wired
//! into [`crate::DevModeEngine`] — that path remains the single-node
//! demo path. Integration happens in a later release once the gossip
//! and round-change layers land.
//!
//! ## Lifecycle of one height
//!
//! 1. `ValidatorSet::select_leader(H, seed_H)` picks the proposer with
//!    stake-weighted determinism.
//! 2. The leader produces a block AND a [`LeaderProof`] that commits to
//!    a VRF output. That output becomes `seed_{H+1}`, so the next
//!    leader is unpredictable to anyone but the next chosen proposer.
//! 3. Validators sign [`PrecommitVote`]s over `(block_hash, height)`.
//! 4. A [`PrecommitTallier`] collects votes, validates them, and tracks
//!    accumulated stake.
//! 5. Once ⅔ + 1 stake worth of valid distinct votes have been
//!    submitted, [`PrecommitTallier::try_finalize`] yields a
//!    [`PrecommitCertificate`] — the block is final.
//!
//! ## Non-goals (deferred)
//!
//! - PRE-VOTE phase (this module uses a single PRE-COMMIT phase).
//! - Networking / gossip.
//! - Round changes, locking, POL.
//! - Equivocation slashing.
//! - Integration with [`crate::DevModeEngine`].

use aii_crypto::keccak::keccak256;
use aii_crypto::{bls, vrf};
use aii_types::H256;
use std::collections::BTreeSet;

use crate::BftError;

/// Maximum size of a validator set. Bounded by the `u128` signer bitmap
/// used in [`PrecommitCertificate`].
pub const MAX_VALIDATORS: usize = 128;

/// One member of the consensus validator set.
///
/// Each validator runs **two** keys: a BLS key for PRE-COMMIT votes
/// (which aggregate cheaply) and a VRF key for the leader-seed beacon.
#[derive(Clone, Debug)]
pub struct Validator {
    /// BLS public key used to verify this validator's PRE-COMMIT votes.
    pub bls_pubkey: bls::PublicKey,
    /// VRF public key used to verify leader-proof beacons.
    pub vrf_pubkey: vrf::PublicKey,
    /// Stake weight (uint, not normalized). Zero stake is forbidden at
    /// the set level (see [`ValidatorSet::new`]).
    pub stake: u64,
}

/// A frozen set of validators for one or more heights.
///
/// The set is immutable after construction; rotation lands in a later
/// release. `total_stake` is checked at construction so all later
/// arithmetic on stake is overflow-free.
#[derive(Clone, Debug)]
pub struct ValidatorSet {
    validators: Vec<Validator>,
    total_stake: u64,
}

impl ValidatorSet {
    /// Build a new validator set.
    ///
    /// Rejects:
    /// - empty input → [`BftError::EmptyValidatorSet`]
    /// - more than [`MAX_VALIDATORS`] entries → [`BftError::ValidatorSetTooLarge`]
    /// - `Σ stake` overflows `u64` → [`BftError::TotalStakeOverflow`]
    /// - `Σ stake == 0` → [`BftError::ZeroTotalStake`]
    pub fn new(validators: Vec<Validator>) -> Result<Self, BftError> {
        if validators.is_empty() {
            return Err(BftError::EmptyValidatorSet);
        }
        if validators.len() > MAX_VALIDATORS {
            return Err(BftError::ValidatorSetTooLarge {
                got: validators.len(),
                max: MAX_VALIDATORS,
            });
        }
        let mut total: u64 = 0;
        for v in &validators {
            total = total
                .checked_add(v.stake)
                .ok_or(BftError::TotalStakeOverflow)?;
        }
        if total == 0 {
            return Err(BftError::ZeroTotalStake);
        }
        Ok(Self {
            validators,
            total_stake: total,
        })
    }

    /// Number of validators.
    #[must_use]
    pub fn size(&self) -> usize {
        self.validators.len()
    }

    /// Sum of all validators' stake (checked at construction).
    #[must_use]
    pub const fn total_stake(&self) -> u64 {
        self.total_stake
    }

    /// `(2 * total_stake) / 3 + 1` — strict ⅔-Byzantine threshold.
    #[must_use]
    pub const fn quorum_threshold(&self) -> u64 {
        (self.total_stake * 2) / 3 + 1
    }

    /// Borrow validator `i`, or `None` if out of bounds.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&Validator> {
        self.validators.get(index)
    }

    /// Borrow all validators in canonical order.
    #[must_use]
    pub fn validators(&self) -> &[Validator] {
        &self.validators
    }

    /// Stake-weighted deterministic leader for `(height, seed)`.
    ///
    /// `pick = u64::from_be_bytes(keccak256(height_be8 ‖ seed)[0..8]) % total_stake`,
    /// then the leader is the first validator whose running stake sum
    /// exceeds `pick`. Distribution is uniform modulo `total_stake`.
    #[must_use]
    pub fn select_leader(&self, height: u64, seed: &[u8; 32]) -> usize {
        let mut buf = [0u8; 40];
        buf[0..8].copy_from_slice(&height.to_be_bytes());
        buf[8..40].copy_from_slice(seed);
        let hash = keccak256(&buf);
        let mut pick_bytes = [0u8; 8];
        pick_bytes.copy_from_slice(&hash.as_bytes()[0..8]);
        let pick = u64::from_be_bytes(pick_bytes) % self.total_stake;
        let mut cum: u64 = 0;
        for (i, v) in self.validators.iter().enumerate() {
            cum = cum.saturating_add(v.stake);
            if pick < cum {
                return i;
            }
        }
        self.validators.len() - 1
    }
}

/// VRF proof produced by the leader at a given height; commits to a
/// 32-byte output that becomes the next leader-selection seed.
#[derive(Clone, Debug)]
pub struct LeaderProof {
    /// The schnorrkel VRF proof.
    pub vrf_proof: vrf::VrfProof,
    /// 32-byte VRF output. Becomes the seed for the next height.
    pub vrf_output: [u8; 32],
}

impl LeaderProof {
    /// VRF input for `(height, seed)`. Same on both signer and verifier.
    #[must_use]
    pub fn input(height: u64, seed: &[u8; 32]) -> [u8; 32] {
        let mut buf = [0u8; 40];
        buf[0..8].copy_from_slice(&height.to_be_bytes());
        buf[8..40].copy_from_slice(seed);
        *keccak256(&buf).as_bytes()
    }

    /// Leader produces the proof + output for the next seed.
    pub fn produce(sk: &vrf::SecretKey, height: u64, seed: &[u8; 32]) -> Self {
        let input = Self::input(height, seed);
        let (vrf_proof, vrf_output) = vrf::prove(sk, &input);
        Self {
            vrf_proof,
            vrf_output,
        }
    }

    /// Verifier confirms this proof was made by `pk` at `(height, seed)`
    /// and that `vrf_output` is the genuine VRF result.
    pub fn verify(
        &self,
        pk: &vrf::PublicKey,
        height: u64,
        seed: &[u8; 32],
    ) -> Result<(), BftError> {
        let input = Self::input(height, seed);
        let recovered =
            vrf::verify(pk, &input, &self.vrf_proof).map_err(|_| BftError::InvalidVrfProof)?;
        if recovered != self.vrf_output {
            return Err(BftError::InvalidVrfProof);
        }
        Ok(())
    }

    /// Borrow the seed for the next height.
    #[must_use]
    pub const fn next_seed(&self) -> &[u8; 32] {
        &self.vrf_output
    }
}

/// A single PRE-COMMIT vote from one validator.
#[derive(Clone, Debug)]
pub struct PrecommitVote {
    /// Block being voted for.
    pub block_hash: H256,
    /// Height of the vote.
    pub height: u64,
    /// Validator's index in the [`ValidatorSet`].
    pub validator_index: u32,
    /// BLS signature over `digest(block_hash, height)`.
    pub bls_sig: bls::Signature,
}

impl PrecommitVote {
    /// 32-byte digest that validators BLS-sign:
    /// `keccak256(block_hash ‖ height_be8)`.
    #[must_use]
    pub fn digest(block_hash: &H256, height: u64) -> H256 {
        let mut buf = Vec::with_capacity(40);
        buf.extend_from_slice(block_hash.as_bytes());
        buf.extend_from_slice(&height.to_be_bytes());
        keccak256(&buf)
    }

    /// Build a signed vote.
    pub fn sign(sk: &bls::SecretKey, block_hash: H256, height: u64, validator_index: u32) -> Self {
        let d = Self::digest(&block_hash, height);
        let bls_sig = sk.sign(d.as_bytes());
        Self {
            block_hash,
            height,
            validator_index,
            bls_sig,
        }
    }
}

/// State returned by [`PrecommitTallier::submit`] for an accepted vote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TallyState {
    /// Vote accepted; running stake is still below the quorum threshold.
    Accepted,
    /// Vote accepted and pushed the running stake at or above the quorum
    /// threshold — the caller may now call
    /// [`PrecommitTallier::try_finalize`].
    ReachedQuorum,
}

/// Per-height collector of PRE-COMMIT votes.
///
/// Holds the validator set, the `(block_hash, height)` it tallies for,
/// and the BLS material needed to aggregate at finalisation time.
pub struct PrecommitTallier {
    block_hash: H256,
    height: u64,
    vs: ValidatorSet,
    received: BTreeSet<u32>,
    sigs: Vec<bls::Signature>,
    pubkeys: Vec<bls::PublicKey>,
    signer_bitmap: u128,
    stake_collected: u64,
}

impl PrecommitTallier {
    /// Build a tallier bound to `(block_hash, height)` and `vs`.
    pub const fn new(block_hash: H256, height: u64, vs: ValidatorSet) -> Self {
        Self {
            block_hash,
            height,
            vs,
            received: BTreeSet::new(),
            sigs: Vec::new(),
            pubkeys: Vec::new(),
            signer_bitmap: 0,
            stake_collected: 0,
        }
    }

    /// Submit one validator's vote.
    ///
    /// Validations (first match wins, state unchanged on error):
    /// 1. `vote.block_hash == self.block_hash`
    /// 2. `vote.height == self.height`
    /// 3. `vote.validator_index < vs.size()`
    /// 4. `received.contains(index)` ⇒ [`BftError::DuplicateVote`]
    /// 5. BLS signature verifies against `vs[index].bls_pubkey`
    pub fn submit(&mut self, vote: PrecommitVote) -> Result<TallyState, BftError> {
        if vote.block_hash != self.block_hash {
            return Err(BftError::WrongBlockHash);
        }
        if vote.height != self.height {
            return Err(BftError::WrongHeight);
        }
        let idx = vote.validator_index as usize;
        if idx >= self.vs.size() {
            return Err(BftError::ValidatorIndexOutOfBounds {
                index: vote.validator_index,
                size: self.vs.size(),
            });
        }
        if self.received.contains(&vote.validator_index) {
            return Err(BftError::DuplicateVote(vote.validator_index));
        }
        let v = &self.vs.validators()[idx];
        let d = PrecommitVote::digest(&self.block_hash, self.height);
        vote.bls_sig
            .verify(d.as_bytes(), &v.bls_pubkey)
            .map_err(|_| BftError::InvalidBlsSignature)?;

        self.received.insert(vote.validator_index);
        self.sigs.push(vote.bls_sig);
        self.pubkeys.push(v.bls_pubkey.clone());
        self.signer_bitmap |= 1u128 << vote.validator_index;
        self.stake_collected = self.stake_collected.saturating_add(v.stake);

        if self.stake_collected >= self.vs.quorum_threshold() {
            Ok(TallyState::ReachedQuorum)
        } else {
            Ok(TallyState::Accepted)
        }
    }

    /// If accumulated stake ≥ quorum threshold, produce the certificate.
    /// Idempotent — calling twice yields the same certificate.
    #[must_use]
    pub fn try_finalize(&self) -> Option<PrecommitCertificate> {
        if self.stake_collected < self.vs.quorum_threshold() {
            return None;
        }
        let aggregated_sig = bls::aggregate_signatures(&self.sigs).ok()?;
        Some(PrecommitCertificate {
            block_hash: self.block_hash,
            height: self.height,
            signer_bitmap: self.signer_bitmap,
            aggregated_sig,
        })
    }

    /// Stake collected so far across all accepted votes.
    #[must_use]
    pub const fn stake_collected(&self) -> u64 {
        self.stake_collected
    }
}

/// Aggregate proof that ⅔ + 1 stake voted for `(block_hash, height)`.
#[derive(Clone, Debug)]
pub struct PrecommitCertificate {
    /// Block being certified.
    pub block_hash: H256,
    /// Height at which the certificate was produced.
    pub height: u64,
    /// Bit `i` set iff validator `i` contributed a vote.
    pub signer_bitmap: u128,
    /// BLS aggregate over each contributor's individual signature.
    pub aggregated_sig: bls::Signature,
}

impl PrecommitCertificate {
    /// Recompute the precommit digest, gather the corresponding pubkeys
    /// from `vs`, verify the aggregated signature, and check that the
    /// signer subset's stake meets the quorum.
    pub fn verify(&self, vs: &ValidatorSet) -> Result<(), BftError> {
        let n = vs.size();
        if self.signer_bitmap != 0 {
            let highest = self.signer_bitmap.ilog2();
            if (highest as usize) >= n {
                return Err(BftError::ValidatorIndexOutOfBounds {
                    index: highest,
                    size: n,
                });
            }
        }
        let mut pks: Vec<bls::PublicKey> = Vec::new();
        let mut signer_stake: u64 = 0;
        for i in 0..n {
            if (self.signer_bitmap >> i) & 1 == 1 {
                pks.push(vs.validators()[i].bls_pubkey.clone());
                signer_stake = signer_stake.saturating_add(vs.validators()[i].stake);
            }
        }
        if signer_stake < vs.quorum_threshold() {
            return Err(BftError::InvalidBlsSignature);
        }
        let d = PrecommitVote::digest(&self.block_hash, self.height);
        bls::fast_aggregate_verify(&self.aggregated_sig, d.as_bytes(), &pks)
            .map_err(|_| BftError::InvalidBlsSignature)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aii_crypto::bls::SecretKey as BlsSecretKey;
    use aii_crypto::vrf::SecretKey as VrfSecretKey;

    fn bls_sk(seed: u8) -> BlsSecretKey {
        BlsSecretKey::from_ikm(&[seed; 32], b"AII-BFT-TEST").unwrap()
    }

    fn vrf_sk() -> VrfSecretKey {
        VrfSecretKey::generate()
    }

    fn validator(stake: u64, seed: u8) -> (Validator, BlsSecretKey, VrfSecretKey) {
        let bk = bls_sk(seed);
        let vk = vrf_sk();
        let v = Validator {
            bls_pubkey: bk.public_key(),
            vrf_pubkey: vk.public_key(),
            stake,
        };
        (v, bk, vk)
    }

    fn three_validators_equal_stake() -> (ValidatorSet, Vec<BlsSecretKey>) {
        let (v1, sk1, _) = validator(100, 1);
        let (v2, sk2, _) = validator(100, 2);
        let (v3, sk3, _) = validator(100, 3);
        let vs = ValidatorSet::new(vec![v1, v2, v3]).unwrap();
        (vs, vec![sk1, sk2, sk3])
    }

    #[test]
    fn validator_set_new_empty_rejected() {
        let err = ValidatorSet::new(vec![]).unwrap_err();
        assert_eq!(err, BftError::EmptyValidatorSet);
    }

    #[test]
    fn validator_set_zero_total_stake_rejected() {
        let (v, _, _) = validator(0, 1);
        let err = ValidatorSet::new(vec![v]).unwrap_err();
        assert_eq!(err, BftError::ZeroTotalStake);
    }

    #[test]
    fn validator_set_too_large_rejected() {
        let mut vs = Vec::new();
        for i in 0..=MAX_VALIDATORS {
            let (v, _, _) = validator(1, u8::try_from(i).unwrap_or(255));
            vs.push(v);
        }
        let err = ValidatorSet::new(vs).unwrap_err();
        assert_eq!(
            err,
            BftError::ValidatorSetTooLarge {
                got: MAX_VALIDATORS + 1,
                max: MAX_VALIDATORS,
            }
        );
    }

    #[test]
    fn total_stake_overflow_rejected() {
        let (a, _, _) = validator(u64::MAX, 1);
        let (b, _, _) = validator(1, 2);
        let err = ValidatorSet::new(vec![a, b]).unwrap_err();
        assert_eq!(err, BftError::TotalStakeOverflow);
    }

    #[test]
    fn total_stake_sums_correctly() {
        let (v1, _, _) = validator(10, 1);
        let (v2, _, _) = validator(20, 2);
        let (v3, _, _) = validator(70, 3);
        let vs = ValidatorSet::new(vec![v1, v2, v3]).unwrap();
        assert_eq!(vs.total_stake(), 100);
        assert_eq!(vs.size(), 3);
    }

    #[test]
    fn quorum_threshold_at_two_thirds_plus_one() {
        let (vs, _) = three_validators_equal_stake();
        // total = 300; 2/3 = 200; threshold = 201
        assert_eq!(vs.quorum_threshold(), 201);
    }

    #[test]
    fn select_leader_is_deterministic() {
        let (vs, _) = three_validators_equal_stake();
        let seed = [0xab; 32];
        let l1 = vs.select_leader(42, &seed);
        let l2 = vs.select_leader(42, &seed);
        assert_eq!(l1, l2);
        assert!(l1 < 3);
    }

    #[test]
    fn select_leader_changes_with_height_and_seed() {
        let (vs, _) = three_validators_equal_stake();
        let seed_a = [0xaa; 32];
        let seed_b = [0xbb; 32];
        let mut seen: BTreeSet<usize> = BTreeSet::new();
        // 100 different (height, seed) tuples should hit at least 2 of
        // the 3 validators, otherwise the picker is constant.
        for h in 0..50 {
            seen.insert(vs.select_leader(h, &seed_a));
            seen.insert(vs.select_leader(h, &seed_b));
        }
        assert!(
            seen.len() >= 2,
            "select_leader should not be constant — got {seen:?}",
        );
    }

    #[test]
    fn select_leader_stake_weighted() {
        let (small, _, _) = validator(1, 1);
        let (big, _, _) = validator(99, 2);
        let vs = ValidatorSet::new(vec![small, big]).unwrap();
        let seed = [0xee; 32];
        let mut big_count = 0;
        for h in 0..1000 {
            if vs.select_leader(h, &seed) == 1 {
                big_count += 1;
            }
        }
        // 99% expected — even with sampling noise this should be ≥ 900.
        assert!(
            big_count >= 900,
            "stake-weighted leader picked big-stake validator only {big_count}/1000 times",
        );
    }

    #[test]
    fn leader_proof_produce_verify_round_trip() {
        let sk = vrf_sk();
        let pk = sk.public_key();
        let seed = [0x33; 32];
        let proof = LeaderProof::produce(&sk, 7, &seed);
        proof.verify(&pk, 7, &seed).unwrap();
    }

    #[test]
    fn leader_proof_verify_with_wrong_height_fails() {
        let sk = vrf_sk();
        let pk = sk.public_key();
        let seed = [0x33; 32];
        let proof = LeaderProof::produce(&sk, 7, &seed);
        let err = proof.verify(&pk, 8, &seed).unwrap_err();
        assert_eq!(err, BftError::InvalidVrfProof);
    }

    #[test]
    fn leader_proof_verify_with_tampered_output_fails() {
        let sk = vrf_sk();
        let pk = sk.public_key();
        let seed = [0x33; 32];
        let mut proof = LeaderProof::produce(&sk, 7, &seed);
        proof.vrf_output[0] ^= 0x01;
        let err = proof.verify(&pk, 7, &seed).unwrap_err();
        assert_eq!(err, BftError::InvalidVrfProof);
    }

    #[test]
    fn precommit_digest_is_deterministic_for_same_inputs() {
        let h = H256::new([0x11; 32]);
        assert_eq!(PrecommitVote::digest(&h, 5), PrecommitVote::digest(&h, 5));
    }

    #[test]
    fn precommit_digest_differs_for_different_height() {
        let h = H256::new([0x11; 32]);
        assert_ne!(PrecommitVote::digest(&h, 5), PrecommitVote::digest(&h, 6));
    }

    #[test]
    fn precommit_sign_verifies_with_signer_pubkey() {
        let sk = bls_sk(7);
        let pk = sk.public_key();
        let block_hash = H256::new([0xcc; 32]);
        let vote = PrecommitVote::sign(&sk, block_hash, 10, 0);
        let d = PrecommitVote::digest(&block_hash, 10);
        vote.bls_sig.verify(d.as_bytes(), &pk).unwrap();
    }

    #[test]
    fn tallier_rejects_wrong_block_hash() {
        let (vs, sks) = three_validators_equal_stake();
        let block = H256::new([0x11; 32]);
        let mut tally = PrecommitTallier::new(block, 1, vs);
        let bogus_vote = PrecommitVote::sign(&sks[0], H256::new([0x22; 32]), 1, 0);
        assert_eq!(
            tally.submit(bogus_vote).unwrap_err(),
            BftError::WrongBlockHash,
        );
    }

    #[test]
    fn tallier_rejects_wrong_height() {
        let (vs, sks) = three_validators_equal_stake();
        let block = H256::new([0x11; 32]);
        let mut tally = PrecommitTallier::new(block, 1, vs);
        let vote = PrecommitVote::sign(&sks[0], block, 999, 0);
        assert_eq!(tally.submit(vote).unwrap_err(), BftError::WrongHeight);
    }

    #[test]
    fn tallier_rejects_out_of_bounds_index() {
        let (vs, sks) = three_validators_equal_stake();
        let block = H256::new([0x11; 32]);
        let mut tally = PrecommitTallier::new(block, 1, vs);
        let vote = PrecommitVote::sign(&sks[0], block, 1, 99);
        assert_eq!(
            tally.submit(vote).unwrap_err(),
            BftError::ValidatorIndexOutOfBounds { index: 99, size: 3 },
        );
    }

    #[test]
    fn tallier_rejects_duplicate_vote() {
        let (vs, sks) = three_validators_equal_stake();
        let block = H256::new([0x11; 32]);
        let mut tally = PrecommitTallier::new(block, 1, vs);
        let vote_a = PrecommitVote::sign(&sks[0], block, 1, 0);
        let vote_b = PrecommitVote::sign(&sks[0], block, 1, 0);
        tally.submit(vote_a).unwrap();
        assert_eq!(
            tally.submit(vote_b).unwrap_err(),
            BftError::DuplicateVote(0)
        );
    }

    #[test]
    fn tallier_rejects_invalid_signature() {
        let (vs, _sks) = three_validators_equal_stake();
        let block = H256::new([0x11; 32]);
        let mut tally = PrecommitTallier::new(block, 1, vs);
        // Use validator 1's secret to sign claiming to be validator 0.
        let imposter = bls_sk(99);
        let vote = PrecommitVote::sign(&imposter, block, 1, 0);
        assert_eq!(
            tally.submit(vote).unwrap_err(),
            BftError::InvalidBlsSignature,
        );
    }

    #[test]
    fn tallier_below_quorum_returns_accepted() {
        let (vs, sks) = three_validators_equal_stake();
        let block = H256::new([0x11; 32]);
        let mut tally = PrecommitTallier::new(block, 1, vs);
        let vote = PrecommitVote::sign(&sks[0], block, 1, 0);
        // 100/300 stake; threshold = 201. Below.
        assert_eq!(tally.submit(vote).unwrap(), TallyState::Accepted);
        assert_eq!(tally.stake_collected(), 100);
        assert!(tally.try_finalize().is_none());
    }

    #[test]
    fn tallier_at_quorum_returns_reached_quorum() {
        let (vs, sks) = three_validators_equal_stake();
        let block = H256::new([0x11; 32]);
        let mut tally = PrecommitTallier::new(block, 1, vs);
        tally
            .submit(PrecommitVote::sign(&sks[0], block, 1, 0))
            .unwrap();
        tally
            .submit(PrecommitVote::sign(&sks[1], block, 1, 1))
            .unwrap();
        // 200/300 — still below 201. Submit the third.
        let third = tally
            .submit(PrecommitVote::sign(&sks[2], block, 1, 2))
            .unwrap();
        assert_eq!(third, TallyState::ReachedQuorum);
        assert_eq!(tally.stake_collected(), 300);
    }

    #[test]
    fn try_finalize_below_quorum_returns_none() {
        let (vs, sks) = three_validators_equal_stake();
        let block = H256::new([0x11; 32]);
        let mut tally = PrecommitTallier::new(block, 1, vs);
        tally
            .submit(PrecommitVote::sign(&sks[0], block, 1, 0))
            .unwrap();
        assert!(tally.try_finalize().is_none());
    }

    #[test]
    fn try_finalize_at_quorum_returns_certificate() {
        let (vs, sks) = three_validators_equal_stake();
        let block = H256::new([0x11; 32]);
        let mut tally = PrecommitTallier::new(block, 1, vs.clone());
        for (i, sk) in sks.iter().enumerate() {
            tally
                .submit(PrecommitVote::sign(sk, block, 1, u32::try_from(i).unwrap()))
                .unwrap();
        }
        let cert = tally.try_finalize().expect("quorum should produce cert");
        assert_eq!(cert.block_hash, block);
        assert_eq!(cert.height, 1);
        assert_eq!(cert.signer_bitmap, 0b111);
        cert.verify(&vs).unwrap();
    }

    #[test]
    fn certificate_verify_rejects_wrong_block_hash_claim() {
        let (vs, sks) = three_validators_equal_stake();
        let block = H256::new([0x11; 32]);
        let mut tally = PrecommitTallier::new(block, 1, vs.clone());
        for (i, sk) in sks.iter().enumerate() {
            tally
                .submit(PrecommitVote::sign(sk, block, 1, u32::try_from(i).unwrap()))
                .unwrap();
        }
        let mut cert = tally.try_finalize().unwrap();
        cert.block_hash = H256::new([0x99; 32]);
        let err = cert.verify(&vs).unwrap_err();
        assert_eq!(err, BftError::InvalidBlsSignature);
    }

    /// Hash sanity: link the digest formula to a known keccak round-trip.
    #[test]
    fn precommit_digest_uses_block_hash_and_height_be8() {
        let block = H256::new([0xaa; 32]);
        let height = 0x0102_0304_0506_0708u64;
        let mut buf = Vec::with_capacity(40);
        buf.extend_from_slice(block.as_bytes());
        buf.extend_from_slice(&height.to_be_bytes());
        assert_eq!(PrecommitVote::digest(&block, height), keccak256(&buf));
    }
}
