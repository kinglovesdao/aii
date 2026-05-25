//! BFT-PoS finality state machine.
//!
//! - **Stage 1 (v0.0.23)**: validator set, VRF leader selection, single
//!   PRE-COMMIT phase + BLS-aggregated [`PrecommitCertificate`].
//! - **Stage 2 (v0.0.25)**: two-phase voting — PRE-VOTE phase yielding
//!   a [`PolcCertificate`] (Proof-of-Lock-Change), required before
//!   PRE-COMMIT. Every vote carries an explicit `round` number so the
//!   coordinator can advance rounds on timeout. Both phase digests are
//!   domain-separated by [`PREVOTE_DOMAIN`] / [`PRECOMMIT_DOMAIN`] so
//!   a signature from one phase cannot be replayed as the other.
//!
//! This module is the **pure on-chain finality state machine**. It does
//! not own a network, does not produce blocks, and is not (yet) wired
//! into [`crate::DevModeEngine`] — that path remains the single-node
//! demo path. Integration happens in a later release once the gossip
//! and round-change coordinator land.
//!
//! ## Lifecycle of one `(height, round)`
//!
//! 1. [`ValidatorSet::select_leader`] picks the proposer.
//! 2. Leader produces a block AND a [`LeaderProof`] for the next seed.
//! 3. Validators sign [`PrevoteVote`]s over `(block, height, round)`.
//! 4. [`PrevoteTallier`] collects votes; on ⅔+1 stake →
//!    [`PolcCertificate`]. The block is now LOCKED for this round.
//! 5. Validators sign [`PrecommitVote`]s over the same `(block, height,
//!    round)`.
//! 6. [`PrecommitTallier`] collects votes; on ⅔+1 stake →
//!    [`PrecommitCertificate`]. The block is FINAL.
//!
//! If either phase fails to reach quorum within the round's timeout,
//! the coordinator advances to `round + 1` with a fresh proposal and
//! fresh tallies — votes from round `R` do not carry forward.
//!
//! ## Non-goals (deferred)
//!
//! - Networking / gossip layer.
//! - Round-change coordinator + timeout policy.
//! - Locking-across-rounds policy (POL preservation in higher rounds).
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

/// Domain-separation tag prefixed to every PRE-VOTE digest. Ensures
/// signatures from one phase cannot be replayed as the other.
pub const PREVOTE_DOMAIN: &[u8] = b"AII-PREVOTE";
/// Domain-separation tag prefixed to every PRE-COMMIT digest.
pub const PRECOMMIT_DOMAIN: &[u8] = b"AII-PRECOMMIT";

/// A single PRE-VOTE vote from one validator (stage 2).
///
/// Two-phase BFT: a validator's PRE-COMMIT for a block at round R is
/// only legitimate after a [`PolcCertificate`] has formed from PRE-VOTES
/// in the same round. This module exposes both phases as pure tallies;
/// the protocol-level coordinator chains them.
#[derive(Clone, Debug)]
pub struct PrevoteVote {
    /// Block being voted for.
    pub block_hash: H256,
    /// Height of the vote.
    pub height: u64,
    /// Round number within this height (rounds advance on timeout).
    pub round: u32,
    /// Validator's index in the [`ValidatorSet`].
    pub validator_index: u32,
    /// BLS signature over [`PrevoteVote::digest`].
    pub bls_sig: bls::Signature,
}

impl PrevoteVote {
    /// `keccak256(PREVOTE_DOMAIN ‖ block_hash ‖ height_be8 ‖ round_be4)`.
    /// Domain-separated so the same `(block, height, round)` produces
    /// different bytes than the PRE-COMMIT digest.
    #[must_use]
    pub fn digest(block_hash: &H256, height: u64, round: u32) -> H256 {
        let mut buf = Vec::with_capacity(PREVOTE_DOMAIN.len() + 32 + 8 + 4);
        buf.extend_from_slice(PREVOTE_DOMAIN);
        buf.extend_from_slice(block_hash.as_bytes());
        buf.extend_from_slice(&height.to_be_bytes());
        buf.extend_from_slice(&round.to_be_bytes());
        keccak256(&buf)
    }

    /// Build a signed PRE-VOTE.
    pub fn sign(
        sk: &bls::SecretKey,
        block_hash: H256,
        height: u64,
        round: u32,
        validator_index: u32,
    ) -> Self {
        let d = Self::digest(&block_hash, height, round);
        let bls_sig = sk.sign(d.as_bytes());
        Self {
            block_hash,
            height,
            round,
            validator_index,
            bls_sig,
        }
    }
}

/// A single PRE-COMMIT vote from one validator (now round-aware).
#[derive(Clone, Debug)]
pub struct PrecommitVote {
    /// Block being voted for.
    pub block_hash: H256,
    /// Height of the vote.
    pub height: u64,
    /// Round number within this height.
    pub round: u32,
    /// Validator's index in the [`ValidatorSet`].
    pub validator_index: u32,
    /// BLS signature over [`PrecommitVote::digest`].
    pub bls_sig: bls::Signature,
}

impl PrecommitVote {
    /// `keccak256(PRECOMMIT_DOMAIN ‖ block_hash ‖ height_be8 ‖ round_be4)`.
    /// Domain-separated from [`PrevoteVote::digest`].
    #[must_use]
    pub fn digest(block_hash: &H256, height: u64, round: u32) -> H256 {
        let mut buf = Vec::with_capacity(PRECOMMIT_DOMAIN.len() + 32 + 8 + 4);
        buf.extend_from_slice(PRECOMMIT_DOMAIN);
        buf.extend_from_slice(block_hash.as_bytes());
        buf.extend_from_slice(&height.to_be_bytes());
        buf.extend_from_slice(&round.to_be_bytes());
        keccak256(&buf)
    }

    /// Build a signed PRE-COMMIT.
    pub fn sign(
        sk: &bls::SecretKey,
        block_hash: H256,
        height: u64,
        round: u32,
        validator_index: u32,
    ) -> Self {
        let d = Self::digest(&block_hash, height, round);
        let bls_sig = sk.sign(d.as_bytes());
        Self {
            block_hash,
            height,
            round,
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

/// Per-`(height, round)` collector of PRE-VOTE votes.
///
/// Mirrors [`PrecommitTallier`] but consumes [`PrevoteVote`]s and
/// emits a [`PolcCertificate`] (Proof-of-Lock-Change) on quorum.
pub struct PrevoteTallier {
    block_hash: H256,
    height: u64,
    round: u32,
    vs: ValidatorSet,
    received: BTreeSet<u32>,
    sigs: Vec<bls::Signature>,
    pubkeys: Vec<bls::PublicKey>,
    signer_bitmap: u128,
    stake_collected: u64,
}

impl PrevoteTallier {
    /// Build a tallier bound to `(block_hash, height, round)` and `vs`.
    pub const fn new(block_hash: H256, height: u64, round: u32, vs: ValidatorSet) -> Self {
        Self {
            block_hash,
            height,
            round,
            vs,
            received: BTreeSet::new(),
            sigs: Vec::new(),
            pubkeys: Vec::new(),
            signer_bitmap: 0,
            stake_collected: 0,
        }
    }

    /// Submit one validator's PRE-VOTE.
    ///
    /// Validations (first match wins; state unchanged on error):
    /// 1. `vote.block_hash == self.block_hash`
    /// 2. `vote.height == self.height`
    /// 3. `vote.round == self.round`
    /// 4. `vote.validator_index < vs.size()`
    /// 5. `received.contains(index)` ⇒ [`BftError::DuplicateVote`]
    /// 6. BLS signature verifies against `vs[index].bls_pubkey`
    pub fn submit(&mut self, vote: PrevoteVote) -> Result<TallyState, BftError> {
        if vote.block_hash != self.block_hash {
            return Err(BftError::WrongBlockHash);
        }
        if vote.height != self.height {
            return Err(BftError::WrongHeight);
        }
        if vote.round != self.round {
            return Err(BftError::WrongRound);
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
        let d = PrevoteVote::digest(&self.block_hash, self.height, self.round);
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

    /// If accumulated stake ≥ quorum threshold, produce the POLC.
    #[must_use]
    pub fn try_form_polc(&self) -> Option<PolcCertificate> {
        if self.stake_collected < self.vs.quorum_threshold() {
            return None;
        }
        let aggregated_sig = bls::aggregate_signatures(&self.sigs).ok()?;
        Some(PolcCertificate {
            block_hash: self.block_hash,
            height: self.height,
            round: self.round,
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

/// Proof-of-Lock-Change — proves ≥ ⅔ + 1 stake PRE-VOTED for this
/// block at `(height, round)`. Required before a validator may issue
/// a PRE-COMMIT in the same round.
#[derive(Clone, Debug)]
pub struct PolcCertificate {
    /// Block being locked.
    pub block_hash: H256,
    /// Height of the lock.
    pub height: u64,
    /// Round in which the lock was formed.
    pub round: u32,
    /// Bit `i` set iff validator `i` contributed a PRE-VOTE.
    pub signer_bitmap: u128,
    /// BLS aggregate over contributors' individual PRE-VOTE signatures.
    pub aggregated_sig: bls::Signature,
}

impl PolcCertificate {
    /// Recompute the prevote digest, gather the corresponding pubkeys
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
        let d = PrevoteVote::digest(&self.block_hash, self.height, self.round);
        bls::fast_aggregate_verify(&self.aggregated_sig, d.as_bytes(), &pks)
            .map_err(|_| BftError::InvalidBlsSignature)?;
        Ok(())
    }
}

/// Per-`(height, round)` collector of PRE-COMMIT votes.
///
/// Holds the validator set, the `(block_hash, height, round)` it tallies
/// for, and the BLS material needed to aggregate at finalisation time.
pub struct PrecommitTallier {
    block_hash: H256,
    height: u64,
    round: u32,
    vs: ValidatorSet,
    received: BTreeSet<u32>,
    sigs: Vec<bls::Signature>,
    pubkeys: Vec<bls::PublicKey>,
    signer_bitmap: u128,
    stake_collected: u64,
}

impl PrecommitTallier {
    /// Build a tallier bound to `(block_hash, height, round)` and `vs`.
    pub const fn new(block_hash: H256, height: u64, round: u32, vs: ValidatorSet) -> Self {
        Self {
            block_hash,
            height,
            round,
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
    /// 3. `vote.round == self.round`
    /// 4. `vote.validator_index < vs.size()`
    /// 5. `received.contains(index)` ⇒ [`BftError::DuplicateVote`]
    /// 6. BLS signature verifies against `vs[index].bls_pubkey`
    pub fn submit(&mut self, vote: PrecommitVote) -> Result<TallyState, BftError> {
        if vote.block_hash != self.block_hash {
            return Err(BftError::WrongBlockHash);
        }
        if vote.height != self.height {
            return Err(BftError::WrongHeight);
        }
        if vote.round != self.round {
            return Err(BftError::WrongRound);
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
        let d = PrecommitVote::digest(&self.block_hash, self.height, self.round);
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
            round: self.round,
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

/// Aggregate proof that ⅔ + 1 stake voted for `(block, height, round)`.
#[derive(Clone, Debug)]
pub struct PrecommitCertificate {
    /// Block being certified.
    pub block_hash: H256,
    /// Height at which the certificate was produced.
    pub height: u64,
    /// Round in which finality was reached.
    pub round: u32,
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
        let d = PrecommitVote::digest(&self.block_hash, self.height, self.round);
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
        assert_eq!(
            PrecommitVote::digest(&h, 5, 0),
            PrecommitVote::digest(&h, 5, 0)
        );
    }

    #[test]
    fn precommit_digest_differs_for_different_height() {
        let h = H256::new([0x11; 32]);
        assert_ne!(
            PrecommitVote::digest(&h, 5, 0),
            PrecommitVote::digest(&h, 6, 0)
        );
    }

    #[test]
    fn precommit_digest_differs_for_different_round() {
        let h = H256::new([0x11; 32]);
        assert_ne!(
            PrecommitVote::digest(&h, 5, 0),
            PrecommitVote::digest(&h, 5, 1)
        );
    }

    #[test]
    fn precommit_sign_verifies_with_signer_pubkey() {
        let sk = bls_sk(7);
        let pk = sk.public_key();
        let block_hash = H256::new([0xcc; 32]);
        let vote = PrecommitVote::sign(&sk, block_hash, 10, 0, 0);
        let d = PrecommitVote::digest(&block_hash, 10, 0);
        vote.bls_sig.verify(d.as_bytes(), &pk).unwrap();
    }

    #[test]
    fn tallier_rejects_wrong_block_hash() {
        let (vs, sks) = three_validators_equal_stake();
        let block = H256::new([0x11; 32]);
        let mut tally = PrecommitTallier::new(block, 1, 0, vs);
        let bogus_vote = PrecommitVote::sign(&sks[0], H256::new([0x22; 32]), 1, 0, 0);
        assert_eq!(
            tally.submit(bogus_vote).unwrap_err(),
            BftError::WrongBlockHash,
        );
    }

    #[test]
    fn tallier_rejects_wrong_height() {
        let (vs, sks) = three_validators_equal_stake();
        let block = H256::new([0x11; 32]);
        let mut tally = PrecommitTallier::new(block, 1, 0, vs);
        let vote = PrecommitVote::sign(&sks[0], block, 999, 0, 0);
        assert_eq!(tally.submit(vote).unwrap_err(), BftError::WrongHeight);
    }

    #[test]
    fn tallier_rejects_wrong_round() {
        let (vs, sks) = three_validators_equal_stake();
        let block = H256::new([0x11; 32]);
        let mut tally = PrecommitTallier::new(block, 1, 0, vs);
        let vote = PrecommitVote::sign(&sks[0], block, 1, 7, 0);
        assert_eq!(tally.submit(vote).unwrap_err(), BftError::WrongRound);
    }

    #[test]
    fn tallier_rejects_out_of_bounds_index() {
        let (vs, sks) = three_validators_equal_stake();
        let block = H256::new([0x11; 32]);
        let mut tally = PrecommitTallier::new(block, 1, 0, vs);
        let vote = PrecommitVote::sign(&sks[0], block, 1, 0, 99);
        assert_eq!(
            tally.submit(vote).unwrap_err(),
            BftError::ValidatorIndexOutOfBounds { index: 99, size: 3 },
        );
    }

    #[test]
    fn tallier_rejects_duplicate_vote() {
        let (vs, sks) = three_validators_equal_stake();
        let block = H256::new([0x11; 32]);
        let mut tally = PrecommitTallier::new(block, 1, 0, vs);
        let vote_a = PrecommitVote::sign(&sks[0], block, 1, 0, 0);
        let vote_b = PrecommitVote::sign(&sks[0], block, 1, 0, 0);
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
        let mut tally = PrecommitTallier::new(block, 1, 0, vs);
        // Use validator 1's secret to sign claiming to be validator 0.
        let imposter = bls_sk(99);
        let vote = PrecommitVote::sign(&imposter, block, 1, 0, 0);
        assert_eq!(
            tally.submit(vote).unwrap_err(),
            BftError::InvalidBlsSignature,
        );
    }

    #[test]
    fn tallier_below_quorum_returns_accepted() {
        let (vs, sks) = three_validators_equal_stake();
        let block = H256::new([0x11; 32]);
        let mut tally = PrecommitTallier::new(block, 1, 0, vs);
        let vote = PrecommitVote::sign(&sks[0], block, 1, 0, 0);
        assert_eq!(tally.submit(vote).unwrap(), TallyState::Accepted);
        assert_eq!(tally.stake_collected(), 100);
        assert!(tally.try_finalize().is_none());
    }

    #[test]
    fn tallier_at_quorum_returns_reached_quorum() {
        let (vs, sks) = three_validators_equal_stake();
        let block = H256::new([0x11; 32]);
        let mut tally = PrecommitTallier::new(block, 1, 0, vs);
        tally
            .submit(PrecommitVote::sign(&sks[0], block, 1, 0, 0))
            .unwrap();
        tally
            .submit(PrecommitVote::sign(&sks[1], block, 1, 0, 1))
            .unwrap();
        let third = tally
            .submit(PrecommitVote::sign(&sks[2], block, 1, 0, 2))
            .unwrap();
        assert_eq!(third, TallyState::ReachedQuorum);
        assert_eq!(tally.stake_collected(), 300);
    }

    #[test]
    fn try_finalize_below_quorum_returns_none() {
        let (vs, sks) = three_validators_equal_stake();
        let block = H256::new([0x11; 32]);
        let mut tally = PrecommitTallier::new(block, 1, 0, vs);
        tally
            .submit(PrecommitVote::sign(&sks[0], block, 1, 0, 0))
            .unwrap();
        assert!(tally.try_finalize().is_none());
    }

    #[test]
    fn try_finalize_at_quorum_returns_certificate() {
        let (vs, sks) = three_validators_equal_stake();
        let block = H256::new([0x11; 32]);
        let mut tally = PrecommitTallier::new(block, 1, 0, vs.clone());
        for (i, sk) in sks.iter().enumerate() {
            tally
                .submit(PrecommitVote::sign(
                    sk,
                    block,
                    1,
                    0,
                    u32::try_from(i).unwrap(),
                ))
                .unwrap();
        }
        let cert = tally.try_finalize().expect("quorum should produce cert");
        assert_eq!(cert.block_hash, block);
        assert_eq!(cert.height, 1);
        assert_eq!(cert.round, 0);
        assert_eq!(cert.signer_bitmap, 0b111);
        cert.verify(&vs).unwrap();
    }

    #[test]
    fn certificate_verify_rejects_wrong_block_hash_claim() {
        let (vs, sks) = three_validators_equal_stake();
        let block = H256::new([0x11; 32]);
        let mut tally = PrecommitTallier::new(block, 1, 0, vs.clone());
        for (i, sk) in sks.iter().enumerate() {
            tally
                .submit(PrecommitVote::sign(
                    sk,
                    block,
                    1,
                    0,
                    u32::try_from(i).unwrap(),
                ))
                .unwrap();
        }
        let mut cert = tally.try_finalize().unwrap();
        cert.block_hash = H256::new([0x99; 32]);
        let err = cert.verify(&vs).unwrap_err();
        assert_eq!(err, BftError::InvalidBlsSignature);
    }

    /// Hash sanity: link the digest formula to the new domain-tagged form.
    #[test]
    fn precommit_digest_uses_domain_tag_hash_height_round() {
        let block = H256::new([0xaa; 32]);
        let height = 0x0102_0304_0506_0708u64;
        let round = 0x0a0b_0c0d_u32;
        let mut buf = Vec::with_capacity(PRECOMMIT_DOMAIN.len() + 44);
        buf.extend_from_slice(PRECOMMIT_DOMAIN);
        buf.extend_from_slice(block.as_bytes());
        buf.extend_from_slice(&height.to_be_bytes());
        buf.extend_from_slice(&round.to_be_bytes());
        assert_eq!(
            PrecommitVote::digest(&block, height, round),
            keccak256(&buf)
        );
    }

    // ─────────────────────────── stage 2: prevote phase ───────────────────────────

    #[test]
    fn prevote_digest_is_deterministic_for_same_inputs() {
        let h = H256::new([0x22; 32]);
        assert_eq!(PrevoteVote::digest(&h, 5, 0), PrevoteVote::digest(&h, 5, 0));
    }

    #[test]
    fn prevote_digest_differs_for_different_round() {
        let h = H256::new([0x22; 32]);
        assert_ne!(PrevoteVote::digest(&h, 5, 0), PrevoteVote::digest(&h, 5, 1));
    }

    #[test]
    fn prevote_digest_is_domain_separated_from_precommit() {
        let h = H256::new([0x22; 32]);
        assert_ne!(
            PrevoteVote::digest(&h, 5, 0),
            PrecommitVote::digest(&h, 5, 0),
            "PRE-VOTE and PRE-COMMIT must produce distinct digests for the same (hash,height,round)",
        );
    }

    #[test]
    fn prevote_sign_verifies_with_signer_pubkey() {
        let sk = bls_sk(11);
        let pk = sk.public_key();
        let block_hash = H256::new([0x33; 32]);
        let vote = PrevoteVote::sign(&sk, block_hash, 10, 4, 0);
        let d = PrevoteVote::digest(&block_hash, 10, 4);
        vote.bls_sig.verify(d.as_bytes(), &pk).unwrap();
    }

    #[test]
    fn prevote_tallier_rejects_wrong_block() {
        let (vs, sks) = three_validators_equal_stake();
        let block = H256::new([0x44; 32]);
        let mut tally = PrevoteTallier::new(block, 2, 0, vs);
        let bogus = PrevoteVote::sign(&sks[0], H256::new([0x55; 32]), 2, 0, 0);
        assert_eq!(tally.submit(bogus).unwrap_err(), BftError::WrongBlockHash);
    }

    #[test]
    fn prevote_tallier_rejects_wrong_height() {
        let (vs, sks) = three_validators_equal_stake();
        let block = H256::new([0x44; 32]);
        let mut tally = PrevoteTallier::new(block, 2, 0, vs);
        let vote = PrevoteVote::sign(&sks[0], block, 999, 0, 0);
        assert_eq!(tally.submit(vote).unwrap_err(), BftError::WrongHeight);
    }

    #[test]
    fn prevote_tallier_rejects_wrong_round() {
        let (vs, sks) = three_validators_equal_stake();
        let block = H256::new([0x44; 32]);
        let mut tally = PrevoteTallier::new(block, 2, 0, vs);
        let vote = PrevoteVote::sign(&sks[0], block, 2, 7, 0);
        assert_eq!(tally.submit(vote).unwrap_err(), BftError::WrongRound);
    }

    #[test]
    fn prevote_tallier_rejects_out_of_bounds_index() {
        let (vs, sks) = three_validators_equal_stake();
        let block = H256::new([0x44; 32]);
        let mut tally = PrevoteTallier::new(block, 2, 0, vs);
        let vote = PrevoteVote::sign(&sks[0], block, 2, 0, 99);
        assert_eq!(
            tally.submit(vote).unwrap_err(),
            BftError::ValidatorIndexOutOfBounds { index: 99, size: 3 },
        );
    }

    #[test]
    fn prevote_tallier_rejects_duplicate_vote() {
        let (vs, sks) = three_validators_equal_stake();
        let block = H256::new([0x44; 32]);
        let mut tally = PrevoteTallier::new(block, 2, 0, vs);
        let a = PrevoteVote::sign(&sks[0], block, 2, 0, 0);
        let b = PrevoteVote::sign(&sks[0], block, 2, 0, 0);
        tally.submit(a).unwrap();
        assert_eq!(tally.submit(b).unwrap_err(), BftError::DuplicateVote(0));
    }

    #[test]
    fn prevote_tallier_rejects_invalid_signature() {
        let (vs, _) = three_validators_equal_stake();
        let block = H256::new([0x44; 32]);
        let mut tally = PrevoteTallier::new(block, 2, 0, vs);
        let imposter = bls_sk(99);
        let vote = PrevoteVote::sign(&imposter, block, 2, 0, 0);
        assert_eq!(
            tally.submit(vote).unwrap_err(),
            BftError::InvalidBlsSignature,
        );
    }

    #[test]
    fn prevote_tallier_below_quorum_returns_accepted() {
        let (vs, sks) = three_validators_equal_stake();
        let block = H256::new([0x44; 32]);
        let mut tally = PrevoteTallier::new(block, 2, 0, vs);
        let vote = PrevoteVote::sign(&sks[0], block, 2, 0, 0);
        assert_eq!(tally.submit(vote).unwrap(), TallyState::Accepted);
        assert_eq!(tally.stake_collected(), 100);
        assert!(tally.try_form_polc().is_none());
    }

    #[test]
    fn prevote_tallier_at_quorum_returns_reached_quorum() {
        let (vs, sks) = three_validators_equal_stake();
        let block = H256::new([0x44; 32]);
        let mut tally = PrevoteTallier::new(block, 2, 0, vs);
        tally
            .submit(PrevoteVote::sign(&sks[0], block, 2, 0, 0))
            .unwrap();
        tally
            .submit(PrevoteVote::sign(&sks[1], block, 2, 0, 1))
            .unwrap();
        let third = tally
            .submit(PrevoteVote::sign(&sks[2], block, 2, 0, 2))
            .unwrap();
        assert_eq!(third, TallyState::ReachedQuorum);
    }

    #[test]
    fn try_form_polc_at_quorum_returns_certificate() {
        let (vs, sks) = three_validators_equal_stake();
        let block = H256::new([0x44; 32]);
        let mut tally = PrevoteTallier::new(block, 2, 3, vs.clone());
        for (i, sk) in sks.iter().enumerate() {
            tally
                .submit(PrevoteVote::sign(
                    sk,
                    block,
                    2,
                    3,
                    u32::try_from(i).unwrap(),
                ))
                .unwrap();
        }
        let polc = tally.try_form_polc().expect("quorum should form POLC");
        assert_eq!(polc.block_hash, block);
        assert_eq!(polc.height, 2);
        assert_eq!(polc.round, 3);
        assert_eq!(polc.signer_bitmap, 0b111);
        polc.verify(&vs).unwrap();
    }

    #[test]
    fn polc_verify_rejects_tampered_block_hash() {
        let (vs, sks) = three_validators_equal_stake();
        let block = H256::new([0x44; 32]);
        let mut tally = PrevoteTallier::new(block, 2, 0, vs.clone());
        for (i, sk) in sks.iter().enumerate() {
            tally
                .submit(PrevoteVote::sign(
                    sk,
                    block,
                    2,
                    0,
                    u32::try_from(i).unwrap(),
                ))
                .unwrap();
        }
        let mut polc = tally.try_form_polc().unwrap();
        polc.block_hash = H256::new([0x99; 32]);
        assert_eq!(polc.verify(&vs).unwrap_err(), BftError::InvalidBlsSignature,);
    }

    #[test]
    fn precommit_signed_for_round0_cannot_be_replayed_at_round1() {
        // Replay-resistance across rounds: a vote signed for round 0
        // must not verify against a tallier listening for round 1, even
        // for the same (block, height).
        let (vs, sks) = three_validators_equal_stake();
        let block = H256::new([0xab; 32]);
        let r0_vote = PrecommitVote::sign(&sks[0], block, 1, 0, 0);
        let mut r1_tally = PrecommitTallier::new(block, 1, 1, vs);
        // The round mismatch fires before the sig check.
        assert_eq!(r1_tally.submit(r0_vote).unwrap_err(), BftError::WrongRound,);
    }
}
