//! BFT-PoS stage 6: chain-level [`BftEngine`] (v0.0.29).
//!
//! Wraps a [`RoundCoordinator`] and the bookkeeping needed to integrate
//! with [`aii_consensus_iface::Engine`] — chain head, timestamp,
//! cross-height seed. The host can either:
//!
//! - Run the engine in **single-validator** mode (the validator set has
//!   one entry with all the stake): [`BftEngine::step`] auto-advances
//!   one full BFT round per call. This is the smallest test of the
//!   full lifecycle through real proposal + vote + tally + cert.
//! - Run **multi-validator** mode: [`BftEngine::step`] returns
//!   [`EngineProgress::Idle`] — the host drives the inner coordinator
//!   via its own network layer (event injection lands in v0.0.30+).
//!
//! ## Scope
//!
//! Single-validator mode end-to-end:
//! 1. Build a leader proof for `(height+1, 0, seed)`.
//! 2. Build a fresh block on top of the current head.
//! 3. Push proposal + own PRE-VOTE + own PRE-COMMIT into a
//!    [`RoundCoordinator`].
//! 4. With one validator the lone vote alone reaches quorum, so the
//!    coordinator emits a [`PrecommitCertificate`].
//! 5. Update head + advance seed via the leader proof's VRF output.
//!
//! Multi-validator drive (peer message ingest, gossip, real timeout
//! scheduling) is an explicit non-goal here — it will land alongside
//! the gossip layer.

#![allow(clippy::significant_drop_tightening)]

use std::sync::Arc;

use parking_lot::Mutex;

use aii_block::{Block, BlockBody, Bloom, Hashable, Header, EMPTY_LIST_HASH, EMPTY_TRIE_HASH};
use aii_consensus_iface::{ConsensusError, Engine, EngineProgress};
use aii_crypto::{bls, vrf};
use aii_types::{Address, H256, U256};

use crate::bft::{LeaderProof, PrecommitCertificate, PrecommitVote, PrevoteVote, ValidatorSet};
use crate::coordinator::RoundCoordinator;
use crate::BftError;

/// Per-validator configuration for [`BftEngine`].
pub struct BftConfig {
    /// Validator set in force for this run.
    pub validator_set: ValidatorSet,
    /// Index of this node's validator inside [`Self::validator_set`].
    pub my_index: u32,
    /// This node's BLS key (used to sign PRE-VOTES / PRE-COMMITS).
    pub my_bls_sk: bls::SecretKey,
    /// This node's VRF key (used to sign leader proofs).
    pub my_vrf_sk: vrf::SecretKey,
    /// Cross-height seed at startup (chain-genesis randomness).
    pub initial_seed: [u8; 32],
    /// Block-producing coinbase address.
    pub coinbase: Address,
    /// Per-block gas limit.
    pub gas_limit: u64,
    /// EIP-1559 base fee.
    pub base_fee_per_gas: U256,
    /// Slot duration (seconds).
    pub slot_seconds: u64,
}

impl BftConfig {
    /// Build a [`BftConfig`] from a genesis file plus this node's
    /// private keys + coinbase. The validator set, chain-spec
    /// parameters (gas limit, base fee, slot time), and the initial
    /// seed are sourced from the genesis; the secret keys and coinbase
    /// stay node-local.
    ///
    /// Returns:
    /// - [`BftError::EmptyValidatorSet`] if `genesis.validators` is empty
    /// - [`BftError::InvalidValidatorPubkey`] if any entry has an
    ///   undecodable BLS or VRF pubkey
    /// - [`BftError::ValidatorIndexOutOfBounds`] if `my_index >=
    ///   genesis.validators.len()` (`size` is the validator-set size)
    pub fn from_genesis(
        genesis: &aii_config::Genesis,
        my_index: u32,
        my_bls_sk: bls::SecretKey,
        my_vrf_sk: vrf::SecretKey,
        coinbase: Address,
    ) -> Result<Self, BftError> {
        if genesis.validators.is_empty() {
            return Err(BftError::EmptyValidatorSet);
        }
        let mut runtime = Vec::with_capacity(genesis.validators.len());
        for (i, gv) in genesis.validators.iter().enumerate() {
            let bls_pubkey = bls::PublicKey::from_compressed(&gv.bls_pubkey.0).map_err(|_| {
                BftError::InvalidValidatorPubkey {
                    index: i,
                    kind: "bls",
                }
            })?;
            let vrf_pubkey = vrf::PublicKey::from_bytes(&gv.vrf_pubkey.0).map_err(|_| {
                BftError::InvalidValidatorPubkey {
                    index: i,
                    kind: "vrf",
                }
            })?;
            runtime.push(crate::bft::Validator {
                bls_pubkey,
                vrf_pubkey,
                stake: gv.stake,
            });
        }
        let validator_set = ValidatorSet::new(runtime)?;
        if (my_index as usize) >= validator_set.size() {
            return Err(BftError::ValidatorIndexOutOfBounds {
                index: my_index,
                size: validator_set.size(),
            });
        }
        Ok(Self {
            validator_set,
            my_index,
            my_bls_sk,
            my_vrf_sk,
            initial_seed: genesis.initial_seed,
            coinbase,
            gas_limit: genesis.chain_spec.initial_gas_limit,
            base_fee_per_gas: U256::from(genesis.chain_spec.min_base_fee_per_gas),
            slot_seconds: genesis.chain_spec.block_time_seconds,
        })
    }
}

/// Outcome of a single-validator advance.
pub struct AdvanceOutput {
    /// The block that was just committed.
    pub block: Block,
    /// Hash of [`Self::block`].
    pub block_hash: H256,
    /// The finality certificate over `block`.
    pub certificate: PrecommitCertificate,
}

/// BFT chain engine. Single-node compatible, multi-validator ready.
pub struct BftEngine {
    config: BftConfig,
    state: Arc<Mutex<BftEngineState>>,
}

struct BftEngineState {
    head_hash: H256,
    head_number: u64,
    head_timestamp: u64,
    /// Seed rolled forward for the next height's leader selection.
    seed: [u8; 32],
    /// Coordinator driving the next-height round, if one is active.
    /// Lazily created when the first event for the height arrives;
    /// reset to `None` after `Committed` is harvested into a new head.
    coordinator: Option<RoundCoordinator>,
    /// `(block, leader_proof)` accepted in the current round — held so
    /// we can commit the full block when the cert forms and roll the
    /// seed forward via the proof's VRF output.
    proposal: Option<(Block, LeaderProof)>,
}

impl BftEngine {
    /// Construct from config + genesis block.
    pub fn new(config: BftConfig, genesis: &Block) -> Self {
        let state = BftEngineState {
            head_hash: genesis.hash(),
            head_number: genesis.header.number,
            head_timestamp: genesis.header.timestamp,
            seed: config.initial_seed,
            coordinator: None,
            proposal: None,
        };
        Self {
            config,
            state: Arc::new(Mutex::new(state)),
        }
    }

    /// Snapshot the chain head.
    #[must_use]
    pub fn head(&self) -> (H256, u64) {
        let g = self.state.lock();
        (g.head_hash, g.head_number)
    }

    /// `true` iff this engine is configured with a single-validator set.
    #[must_use]
    pub fn is_single_validator(&self) -> bool {
        self.config.validator_set.size() == 1
    }

    /// Current `(height, round, Phase)` if a coordinator is active.
    #[must_use]
    pub fn current_round_state(&self) -> Option<(u64, u32, crate::bft::Phase)> {
        let g = self.state.lock();
        g.coordinator
            .as_ref()
            .map(|c| (c.height(), c.round(), c.phase()))
    }

    /// Leader index for the active round (if any).
    #[must_use]
    pub fn current_leader_index(&self) -> Option<usize> {
        let g = self.state.lock();
        g.coordinator.as_ref().map(RoundCoordinator::leader_index)
    }

    /// This node's index inside its `validator_set`.
    #[must_use]
    pub const fn my_index(&self) -> usize {
        self.config.my_index as usize
    }

    /// `true` iff this node would be the elected leader for the
    /// *next* height's round-0 proposal, given the current head and
    /// rolled-forward seed. Used by the gossip layer to bootstrap a
    /// round when no coordinator exists yet.
    #[must_use]
    pub fn would_be_leader_next_height(&self) -> bool {
        let g = self.state.lock();
        let next_h = g.head_number.saturating_add(1);
        let leader = self.config.validator_set.select_leader(next_h, 0, &g.seed);
        leader == self.config.my_index as usize
    }

    /// Validator-set size in force.
    #[must_use]
    pub fn validator_set_size(&self) -> usize {
        self.config.validator_set.size()
    }

    /// Reconstruct the exact block the engine would `cast_proposal` at
    /// `height` against its current head + `leader_proof`. Used by the
    /// gossip layer to recover the full block from a `Proposal` wire
    /// message that only carries `block_hash + leader_proof`.
    #[must_use]
    pub fn reconstruct_proposed_block(&self, height: u64, leader_proof: &LeaderProof) -> Block {
        let g = self.state.lock();
        // `build_block` reads head_hash + head_timestamp, which mirrors
        // what the leader did when they signed the proposal.
        self.build_block(g.head_hash, g.head_timestamp, height, leader_proof)
    }

    /// `&self` harvest: if the coordinator is in `Committed`, commit
    /// the captured block, advance the head, roll the seed, and clear
    /// the coordinator. Returns `Some(new_head_number)` on harvest,
    /// `None` if there is nothing to commit yet.
    ///
    /// Useful for gossip / network drivers that hold the engine in an
    /// `Arc<BftEngine>` and cannot call the `&mut`-flavoured
    /// [`aii_consensus_iface::Engine::step`].
    pub fn try_harvest_committed(&self) -> Option<u64> {
        let mut g = self.state.lock();
        let committed = g
            .coordinator
            .as_ref()
            .is_some_and(|c| c.phase() == crate::bft::Phase::Committed);
        if !committed {
            return None;
        }
        let (block, proof) = g.proposal.clone()?;
        let block_hash = block.hash();
        g.head_hash = block_hash;
        g.head_number = block.header.number;
        g.head_timestamp = block.header.timestamp;
        g.seed = proof.vrf_output;
        g.coordinator = None;
        g.proposal = None;
        Some(block.header.number)
    }

    /// Build a proposal for the current round and feed it to our own
    /// coordinator. Caller is responsible for broadcasting the returned
    /// `(Block, LeaderProof)` to peers. Only valid when this node is
    /// the elected leader for the round.
    pub fn cast_proposal(&self) -> Result<(Block, LeaderProof), BftError> {
        let mut g = self.state.lock();
        self.ensure_coordinator(&mut g);
        let coord = g.coordinator.as_mut().expect("ensured");
        let leader_idx = coord.leader_index();
        if leader_idx != self.config.my_index as usize {
            return Err(BftError::NotLeader {
                round: coord.round(),
                expected: u32::try_from(leader_idx).unwrap_or(u32::MAX),
            });
        }
        let height = coord.height();
        let round = coord.round();
        let seed = g.seed;
        let head_hash = g.head_hash;
        let head_ts = g.head_timestamp;
        let proof = LeaderProof::produce(&self.config.my_vrf_sk, height, round, &seed);
        let block = self.build_block(head_hash, head_ts, height, &proof);
        let block_hash = block.hash();
        g.coordinator
            .as_mut()
            .unwrap()
            .submit_proposal(block_hash, &proof)?;
        g.proposal = Some((block.clone(), proof.clone()));
        Ok((block, proof))
    }

    /// Sign + submit my own PRE-VOTE for whatever block the coordinator
    /// is currently in `Prevoting` over. Returns the signed vote for
    /// the host to broadcast.
    pub fn cast_prevote(&self) -> Result<PrevoteVote, BftError> {
        let mut g = self.state.lock();
        let coord = g
            .coordinator
            .as_mut()
            .ok_or(BftError::NoActiveCoordinator)?;
        let phase = coord.phase();
        let block_hash = coord.proposed_block().ok_or(BftError::WrongPhase {
            expected: crate::bft::Phase::Prevoting,
            actual: phase,
        })?;
        let vote = PrevoteVote::sign(
            &self.config.my_bls_sk,
            block_hash,
            coord.height(),
            coord.round(),
            self.config.my_index,
        );
        coord.submit_prevote(vote.clone())?;
        Ok(vote)
    }

    /// Sign + submit my own PRE-COMMIT. Returns the signed vote for
    /// the host to broadcast.
    pub fn cast_precommit(&self) -> Result<PrecommitVote, BftError> {
        let mut g = self.state.lock();
        let coord = g
            .coordinator
            .as_mut()
            .ok_or(BftError::NoActiveCoordinator)?;
        if coord.phase() != crate::bft::Phase::Precommitting {
            return Err(BftError::WrongPhase {
                expected: crate::bft::Phase::Precommitting,
                actual: coord.phase(),
            });
        }
        let block_hash = coord
            .proposed_block()
            .expect("invariant: block set by Precommitting");
        let vote = PrecommitVote::sign(
            &self.config.my_bls_sk,
            block_hash,
            coord.height(),
            coord.round(),
            self.config.my_index,
        );
        coord.submit_precommit(vote.clone())?;
        Ok(vote)
    }

    /// Ingest a peer's proposal. Verifies the leader proof and
    /// transitions to `Prevoting`.
    pub fn submit_remote_proposal(
        &self,
        block: Block,
        leader_proof: LeaderProof,
    ) -> Result<(), BftError> {
        let mut g = self.state.lock();
        self.ensure_coordinator(&mut g);
        let coord = g.coordinator.as_mut().expect("ensured");
        let block_hash = block.hash();
        coord.submit_proposal(block_hash, &leader_proof)?;
        g.proposal = Some((block, leader_proof));
        Ok(())
    }

    /// Ingest a peer's PRE-VOTE. Forwards to inner coordinator.
    pub fn submit_remote_prevote(&self, vote: PrevoteVote) -> Result<(), BftError> {
        let mut g = self.state.lock();
        let coord = g
            .coordinator
            .as_mut()
            .ok_or(BftError::NoActiveCoordinator)?;
        coord.submit_prevote(vote)?;
        Ok(())
    }

    /// Ingest a peer's PRE-COMMIT. Forwards to inner coordinator.
    pub fn submit_remote_precommit(&self, vote: PrecommitVote) -> Result<(), BftError> {
        let mut g = self.state.lock();
        let coord = g
            .coordinator
            .as_mut()
            .ok_or(BftError::NoActiveCoordinator)?;
        coord.submit_precommit(vote)?;
        Ok(())
    }

    /// External clock says the round timed out — advance the coordinator
    /// to the next round and drop the captured proposal.
    pub fn tick_timeout(&self) -> Result<(), BftError> {
        let mut g = self.state.lock();
        self.ensure_coordinator(&mut g);
        let coord = g.coordinator.as_mut().expect("ensured");
        coord.fire_timeout();
        g.proposal = None;
        Ok(())
    }

    /// Lazy: instantiate a fresh `RoundCoordinator` for `head_number + 1`
    /// if none is active.
    fn ensure_coordinator(&self, g: &mut BftEngineState) {
        if g.coordinator.is_none() {
            g.coordinator = Some(RoundCoordinator::new(
                g.head_number + 1,
                g.seed,
                self.config.validator_set.clone(),
            ));
        }
    }

    /// Build the block this node would propose for `height` on top of
    /// `parent_hash` at `parent_timestamp`. The leader's VRF output is
    /// embedded in `mix_hash` so every legitimate proposer for the
    /// same height produces a distinct block.
    fn build_block(
        &self,
        parent_hash: H256,
        parent_timestamp: u64,
        height: u64,
        leader_proof: &LeaderProof,
    ) -> Block {
        let header = Header {
            parent_hash,
            ommers_hash: EMPTY_LIST_HASH,
            beneficiary: self.config.coinbase,
            state_root: EMPTY_TRIE_HASH,
            transactions_root: EMPTY_TRIE_HASH,
            receipts_root: EMPTY_TRIE_HASH,
            logs_bloom: Bloom::ZERO,
            difficulty: U256::ZERO,
            number: height,
            gas_limit: self.config.gas_limit,
            gas_used: 0,
            timestamp: parent_timestamp + self.config.slot_seconds,
            extra_data: b"aii-bft".to_vec(),
            mix_hash: H256::new(leader_proof.vrf_output),
            nonce: [0u8; 8],
            base_fee_per_gas: self.config.base_fee_per_gas,
            withdrawals_root: EMPTY_TRIE_HASH,
            blob_gas_used: None,
            excess_blob_gas: None,
            parent_beacon_block_root: None,
        };
        Block {
            header,
            body: BlockBody::default(),
        }
    }

    /// Run one full BFT round (propose + vote + commit) against
    /// ourselves. Only valid in single-validator mode.
    #[allow(clippy::significant_drop_tightening)]
    pub fn advance_single(&self) -> Result<AdvanceOutput, BftError> {
        let vs_size = self.config.validator_set.size();
        if vs_size != 1 {
            return Err(BftError::NotSingleValidator(vs_size));
        }
        let mut g = self.state.lock();
        let new_number = g.head_number.checked_add(1).ok_or(BftError::Overflow)?;
        let new_timestamp = g.head_timestamp + self.config.slot_seconds;
        let seed = g.seed;

        // Build the leader proof for the new height's round 0.
        let leader_proof = LeaderProof::produce(&self.config.my_vrf_sk, new_number, 0, &seed);

        // Build the block. Carry the VRF output into mix_hash so
        // consecutive blocks differ even with identical bodies.
        let header = Header {
            parent_hash: g.head_hash,
            ommers_hash: EMPTY_LIST_HASH,
            beneficiary: self.config.coinbase,
            state_root: EMPTY_TRIE_HASH,
            transactions_root: EMPTY_TRIE_HASH,
            receipts_root: EMPTY_TRIE_HASH,
            logs_bloom: Bloom::ZERO,
            difficulty: U256::ZERO,
            number: new_number,
            gas_limit: self.config.gas_limit,
            gas_used: 0,
            timestamp: new_timestamp,
            extra_data: b"aii-bft".to_vec(),
            mix_hash: H256::new(leader_proof.vrf_output),
            nonce: [0u8; 8],
            base_fee_per_gas: self.config.base_fee_per_gas,
            withdrawals_root: EMPTY_TRIE_HASH,
            blob_gas_used: None,
            excess_blob_gas: None,
            parent_beacon_block_root: None,
        };
        let block = Block {
            header,
            body: BlockBody::default(),
        };
        let block_hash = block.hash();

        // Drive the coordinator: submit proposal, prevote, precommit
        // — all signed by ourselves.
        let mut coord = RoundCoordinator::new(new_number, seed, self.config.validator_set.clone());
        coord.submit_proposal(block_hash, &leader_proof)?;
        coord.submit_prevote(PrevoteVote::sign(
            &self.config.my_bls_sk,
            block_hash,
            new_number,
            0,
            self.config.my_index,
        ))?;
        coord.submit_precommit(PrecommitVote::sign(
            &self.config.my_bls_sk,
            block_hash,
            new_number,
            0,
            self.config.my_index,
        ))?;
        let certificate = coord
            .certificate()
            .cloned()
            .ok_or(BftError::NotSingleValidator(vs_size))?;

        // Commit head + roll the seed forward via the leader's VRF output.
        g.head_hash = block_hash;
        g.head_number = new_number;
        g.head_timestamp = new_timestamp;
        g.seed = leader_proof.vrf_output;

        Ok(AdvanceOutput {
            block,
            block_hash,
            certificate,
        })
    }
}

impl Engine for BftEngine {
    fn init(&mut self, genesis: &Block) -> Result<H256, ConsensusError> {
        let mut g = self.state.lock();
        g.head_hash = genesis.hash();
        g.head_number = genesis.header.number;
        g.head_timestamp = genesis.header.timestamp;
        g.seed = self.config.initial_seed;
        Ok(g.head_hash)
    }

    fn step(&mut self) -> Result<EngineProgress, ConsensusError> {
        if self.is_single_validator() {
            let out = self
                .advance_single()
                .map_err(|_e| ConsensusError::InvalidBlock("BFT advance failed"))?;
            return Ok(EngineProgress::NewBlock(out.block_hash));
        }

        // Multi-validator: harvest if the coordinator has reached Committed.
        let mut g = self.state.lock();
        let committed = g
            .coordinator
            .as_ref()
            .is_some_and(|c| c.phase() == crate::bft::Phase::Committed);
        if !committed {
            return Ok(EngineProgress::Idle);
        }
        let (block, proof) = g
            .proposal
            .clone()
            .ok_or(ConsensusError::InvalidBlock("committed with no proposal"))?;
        let block_hash = block.hash();
        g.head_hash = block_hash;
        g.head_number = block.header.number;
        g.head_timestamp = block.header.timestamp;
        g.seed = proof.vrf_output;
        g.coordinator = None;
        g.proposal = None;
        Ok(EngineProgress::NewBlock(block_hash))
    }

    fn head(&self) -> H256 {
        self.state.lock().head_hash
    }

    fn coinbase(&self) -> Option<Address> {
        Some(self.config.coinbase)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bft::Validator;
    use aii_crypto::bls::SecretKey as BlsSecretKey;
    use aii_crypto::vrf::SecretKey as VrfSecretKey;

    fn bls_sk(seed: u8) -> BlsSecretKey {
        BlsSecretKey::from_ikm(&[seed; 32], b"AII-BFT-ENGINE-TEST").unwrap()
    }

    fn vrf_sk() -> VrfSecretKey {
        VrfSecretKey::generate()
    }

    fn genesis() -> Block {
        Block {
            header: Header {
                parent_hash: H256::ZERO,
                ommers_hash: EMPTY_LIST_HASH,
                beneficiary: Address::ZERO,
                state_root: EMPTY_TRIE_HASH,
                transactions_root: EMPTY_TRIE_HASH,
                receipts_root: EMPTY_TRIE_HASH,
                logs_bloom: Bloom::ZERO,
                difficulty: U256::ZERO,
                number: 0,
                gas_limit: 30_000_000,
                gas_used: 0,
                timestamp: 1_700_000_000,
                extra_data: vec![],
                mix_hash: H256::ZERO,
                nonce: [0u8; 8],
                base_fee_per_gas: U256::from(1_000_000_000u64),
                withdrawals_root: EMPTY_TRIE_HASH,
                blob_gas_used: None,
                excess_blob_gas: None,
                parent_beacon_block_root: None,
            },
            body: BlockBody::default(),
        }
    }

    fn single_validator_config() -> BftConfig {
        let bls = bls_sk(1);
        let vrf = vrf_sk();
        let v = Validator {
            bls_pubkey: bls.public_key(),
            vrf_pubkey: vrf.public_key(),
            stake: 100,
        };
        let vs = ValidatorSet::new(vec![v]).unwrap();
        BftConfig {
            validator_set: vs,
            my_index: 0,
            my_bls_sk: bls,
            my_vrf_sk: vrf,
            initial_seed: [0x11; 32],
            coinbase: Address::new([0xcc; 20]),
            gas_limit: 30_000_000,
            base_fee_per_gas: U256::from(1_000_000_000u64),
            slot_seconds: 3,
        }
    }

    fn three_validator_config_as_validator_0() -> BftConfig {
        let mut vs_list = Vec::new();
        let bls0 = bls_sk(1);
        let vrf0 = vrf_sk();
        vs_list.push(Validator {
            bls_pubkey: bls0.public_key(),
            vrf_pubkey: vrf0.public_key(),
            stake: 100,
        });
        for i in 1..3u8 {
            let bls = bls_sk(i + 1);
            let vrf = vrf_sk();
            vs_list.push(Validator {
                bls_pubkey: bls.public_key(),
                vrf_pubkey: vrf.public_key(),
                stake: 100,
            });
        }
        BftConfig {
            validator_set: ValidatorSet::new(vs_list).unwrap(),
            my_index: 0,
            my_bls_sk: bls0,
            my_vrf_sk: vrf0,
            initial_seed: [0x22; 32],
            coinbase: Address::new([0xdd; 20]),
            gas_limit: 30_000_000,
            base_fee_per_gas: U256::from(1_000_000_000u64),
            slot_seconds: 3,
        }
    }

    #[test]
    fn engine_new_starts_at_genesis() {
        let g = genesis();
        let engine = BftEngine::new(single_validator_config(), &g);
        let (h, n) = engine.head();
        assert_eq!(h, g.hash());
        assert_eq!(n, 0);
    }

    #[test]
    fn engine_coinbase_returned_from_config() {
        let g = genesis();
        let engine = BftEngine::new(single_validator_config(), &g);
        assert_eq!(
            <BftEngine as Engine>::coinbase(&engine),
            Some(Address::new([0xcc; 20])),
        );
    }

    #[test]
    fn is_single_validator_returns_true_for_size_1() {
        let engine = BftEngine::new(single_validator_config(), &genesis());
        assert!(engine.is_single_validator());
    }

    #[test]
    fn is_single_validator_returns_false_for_size_3() {
        let engine = BftEngine::new(three_validator_config_as_validator_0(), &genesis());
        assert!(!engine.is_single_validator());
    }

    #[test]
    fn advance_single_increments_height() {
        let engine = BftEngine::new(single_validator_config(), &genesis());
        let out = engine.advance_single().unwrap();
        assert_eq!(out.block.header.number, 1);
        assert_eq!(out.block.header.timestamp, 1_700_000_003);
        let (h, n) = engine.head();
        assert_eq!(h, out.block_hash);
        assert_eq!(n, 1);
    }

    #[test]
    fn advance_single_certificate_verifies() {
        let cfg = single_validator_config();
        let vs = cfg.validator_set.clone();
        let engine = BftEngine::new(cfg, &genesis());
        let out = engine.advance_single().unwrap();
        out.certificate.verify(&vs).unwrap();
        assert_eq!(out.certificate.height, 1);
        assert_eq!(out.certificate.round, 0);
        assert_eq!(out.certificate.block_hash, out.block_hash);
    }

    #[test]
    fn advance_single_produces_parent_hash_chain() {
        let engine = BftEngine::new(single_validator_config(), &genesis());
        let out1 = engine.advance_single().unwrap();
        let out2 = engine.advance_single().unwrap();
        assert_eq!(out2.block.header.parent_hash, out1.block_hash);
        assert_eq!(out2.block.header.number, 2);
    }

    #[test]
    fn advance_single_rejected_for_multi_validator() {
        let engine = BftEngine::new(three_validator_config_as_validator_0(), &genesis());
        assert!(engine.advance_single().is_err());
    }

    #[test]
    fn step_in_single_validator_returns_new_block() {
        let mut engine = BftEngine::new(single_validator_config(), &genesis());
        let progress = <BftEngine as Engine>::step(&mut engine).unwrap();
        assert!(matches!(progress, EngineProgress::NewBlock(_)));
        let (_, n) = engine.head();
        assert_eq!(n, 1);
    }

    #[test]
    fn step_in_multi_validator_returns_idle() {
        let mut engine = BftEngine::new(three_validator_config_as_validator_0(), &genesis());
        let progress = <BftEngine as Engine>::step(&mut engine).unwrap();
        assert_eq!(progress, EngineProgress::Idle);
        // Head unchanged.
        let (_, n) = engine.head();
        assert_eq!(n, 0);
    }

    #[test]
    fn engine_init_resets_to_supplied_genesis() {
        let g1 = genesis();
        let mut engine = BftEngine::new(single_validator_config(), &g1);
        let _ = engine.advance_single().unwrap();
        // Re-init to the original genesis.
        let h = <BftEngine as Engine>::init(&mut engine, &g1).unwrap();
        assert_eq!(h, g1.hash());
        let (_, n) = engine.head();
        assert_eq!(n, 0);
    }

    #[test]
    fn engine_trait_head_matches_internal_head() {
        let engine = BftEngine::new(single_validator_config(), &genesis());
        let h_trait = <BftEngine as Engine>::head(&engine);
        let (h_internal, _) = engine.head();
        assert_eq!(h_trait, h_internal);
    }

    #[test]
    fn consecutive_advances_evolve_seed_via_vrf_output() {
        // The seed used for height H+1 must depend on height H's VRF
        // output, otherwise two consecutive advances would re-select
        // the same leader against the same input. We can't read the
        // seed directly but we can assert the two blocks differ even
        // when bodies are identical — header.mix_hash should carry
        // forward the leader's VRF output, or at minimum number/
        // timestamp/parent_hash distinguish them so block.hash() is
        // unique.
        let engine = BftEngine::new(single_validator_config(), &genesis());
        let a = engine.advance_single().unwrap();
        let b = engine.advance_single().unwrap();
        assert_ne!(a.block_hash, b.block_hash);
    }

    /// Cross-method invariant: AdvanceOutput.certificate.height must
    /// match block.header.number for the just-finalised block.
    #[test]
    fn advance_single_block_height_matches_certificate_height() {
        let engine = BftEngine::new(single_validator_config(), &genesis());
        let out = engine.advance_single().unwrap();
        assert_eq!(out.block.header.number, out.certificate.height);
    }

    /// Drive the engine through many heights and check the chain stays
    /// linked + finality certs validate end to end.
    #[test]
    fn advance_single_many_heights_keep_chain_linked() {
        let cfg = single_validator_config();
        let vs = cfg.validator_set.clone();
        let engine = BftEngine::new(cfg, &genesis());
        let mut last_hash = genesis().hash();
        for expected_n in 1..=10 {
            let out = engine.advance_single().unwrap();
            assert_eq!(out.block.header.parent_hash, last_hash);
            assert_eq!(out.block.header.number, expected_n);
            out.certificate.verify(&vs).unwrap();
            last_hash = out.block_hash;
        }
        let (final_head, final_n) = engine.head();
        assert_eq!(final_head, last_hash);
        assert_eq!(final_n, 10);
    }

    /// Construct the bare BftEngine + check we can also use the
    /// pre-existing Validator types we wired the test fixtures around
    /// — flushes out any missed re-export.
    #[test]
    fn types_used_by_advance_are_in_scope() {
        let _ = PrevoteVote::digest(&H256::ZERO, 0, 0);
        let _ = PrecommitVote::digest(&H256::ZERO, 0, 0);
        let _ = LeaderProof::input(0, 0, &[0u8; 32]);
    }

    // ────────────────────────── multi-validator drive (v0.0.30) ──────────────────

    /// Build N validators with identical stake. Returns the shared
    /// validator set, plus per-node `(bls_sk, vrf_sk)`. Each node's
    /// engine config can then be derived by picking `my_index`.
    fn multi_validator_setup(n: u8) -> (ValidatorSet, Vec<(BlsSecretKey, VrfSecretKey)>) {
        let mut keys = Vec::new();
        let mut vs_list = Vec::new();
        for i in 0..n {
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

    /// Construct a `BftEngine` for validator `idx` in the shared set.
    fn engine_for(
        idx: u32,
        vs: &ValidatorSet,
        keys: &[(BlsSecretKey, VrfSecretKey)],
        genesis: &Block,
    ) -> BftEngine {
        let config = BftConfig {
            validator_set: vs.clone(),
            my_index: idx,
            my_bls_sk: keys[idx as usize].0.clone(),
            my_vrf_sk: keys[idx as usize].1.clone(),
            initial_seed: [0x77; 32],
            coinbase: Address::new([0xab; 20]),
            gas_limit: 30_000_000,
            base_fee_per_gas: U256::from(1_000_000_000u64),
            slot_seconds: 3,
        };
        BftEngine::new(config, genesis)
    }

    #[test]
    fn multi_validator_coordinator_lazily_created() {
        let (vs, keys) = multi_validator_setup(3);
        let engine = engine_for(0, &vs, &keys, &genesis());
        assert!(engine.current_round_state().is_none());
    }

    #[test]
    fn cast_proposal_rejected_for_non_leader() {
        let (vs, keys) = multi_validator_setup(3);
        // Find an index that is NOT the leader for height 1 round 0.
        let expected = vs.select_leader(1, 0, &[0x77; 32]);
        let non_leader = (expected + 1) % 3;
        let engine = engine_for(non_leader as u32, &vs, &keys, &genesis());
        let err = engine.cast_proposal().unwrap_err();
        match err {
            BftError::NotLeader { round, expected: e } => {
                assert_eq!(round, 0);
                assert_eq!(e, expected as u32);
            }
            other => panic!("expected NotLeader, got {other:?}"),
        }
    }

    #[test]
    fn cast_prevote_without_proposal_rejected() {
        let (vs, keys) = multi_validator_setup(3);
        let engine = engine_for(0, &vs, &keys, &genesis());
        assert!(engine.cast_prevote().is_err());
    }

    #[test]
    fn submit_remote_proposal_advances_to_prevoting() {
        let (vs, keys) = multi_validator_setup(3);
        let leader = vs.select_leader(1, 0, &[0x77; 32]);
        let leader_engine = engine_for(leader as u32, &vs, &keys, &genesis());
        let (block, proof) = leader_engine.cast_proposal().unwrap();

        // A peer (not the leader) ingests the proposal.
        let peer_idx = (leader + 1) % 3;
        let peer = engine_for(peer_idx as u32, &vs, &keys, &genesis());
        peer.submit_remote_proposal(block, proof).unwrap();
        let (_, _, phase) = peer.current_round_state().unwrap();
        assert_eq!(phase, crate::bft::Phase::Prevoting);
    }

    #[test]
    fn three_node_consensus_produces_same_block() {
        // The killer test: 3 BftEngine instances drive consensus on the
        // same height by exchanging proposals + votes via direct method
        // calls. All three should agree on the new head.
        let (vs, keys) = multi_validator_setup(3);
        let g = genesis();
        let engines: Vec<BftEngine> = (0..3).map(|i| engine_for(i, &vs, &keys, &g)).collect();

        // Leader for height 1 round 0 proposes.
        let leader = vs.select_leader(1, 0, &[0x77; 32]);
        let (block, proof) = engines[leader].cast_proposal().unwrap();
        let block_hash = block.hash();

        // Every non-leader peer ingests the proposal.
        for (i, e) in engines.iter().enumerate() {
            if i != leader {
                e.submit_remote_proposal(block.clone(), proof.clone())
                    .unwrap();
            }
        }

        // Every node casts their own PRE-VOTE.
        let prevotes: Vec<PrevoteVote> =
            engines.iter().map(|e| e.cast_prevote().unwrap()).collect();

        // Broadcast every prevote to every peer.
        for (vi, vote) in prevotes.iter().enumerate() {
            for (ei, e) in engines.iter().enumerate() {
                if ei != vi {
                    e.submit_remote_prevote(vote.clone()).unwrap();
                }
            }
        }

        // Each node casts PRE-COMMIT.
        let precommits: Vec<PrecommitVote> = engines
            .iter()
            .map(|e| e.cast_precommit().unwrap())
            .collect();
        for (vi, vote) in precommits.iter().enumerate() {
            for (ei, e) in engines.iter().enumerate() {
                if ei != vi {
                    e.submit_remote_precommit(vote.clone()).unwrap();
                }
            }
        }

        // Every node now harvests the committed block via step().
        for mut e in engines {
            let progress = <BftEngine as Engine>::step(&mut e).unwrap();
            assert_eq!(progress, EngineProgress::NewBlock(block_hash));
            let (h, n) = e.head();
            assert_eq!(h, block_hash);
            assert_eq!(n, 1);
        }
    }

    #[test]
    fn submit_remote_prevote_below_quorum_keeps_prevoting() {
        let (vs, keys) = multi_validator_setup(3);
        let leader = vs.select_leader(1, 0, &[0x77; 32]);
        let leader_e = engine_for(leader as u32, &vs, &keys, &genesis());
        let (block, proof) = leader_e.cast_proposal().unwrap();
        // 1 prevote (the leader's). Still below 2-of-3 quorum.
        leader_e.cast_prevote().unwrap();
        let (_, _, phase) = leader_e.current_round_state().unwrap();
        assert_eq!(phase, crate::bft::Phase::Prevoting);
        // Pull (block, proof) out only to silence the unused warning.
        let _ = (block, proof);
    }

    #[test]
    fn three_node_polc_forms_at_quorum() {
        let (vs, keys) = multi_validator_setup(3);
        let g = genesis();
        let leader = vs.select_leader(1, 0, &[0x77; 32]);
        let mut engines: Vec<BftEngine> = (0..3).map(|i| engine_for(i, &vs, &keys, &g)).collect();
        let (block, proof) = engines[leader].cast_proposal().unwrap();
        for (i, e) in engines.iter_mut().enumerate() {
            if i != leader {
                e.submit_remote_proposal(block.clone(), proof.clone())
                    .unwrap();
            }
        }
        let prevotes: Vec<PrevoteVote> =
            engines.iter().map(|e| e.cast_prevote().unwrap()).collect();
        for (vi, vote) in prevotes.iter().enumerate() {
            for (ei, e) in engines.iter().enumerate() {
                if ei != vi {
                    e.submit_remote_prevote(vote.clone()).unwrap();
                }
            }
        }
        // All three engines now in Precommitting.
        for e in &engines {
            let (_, _, phase) = e.current_round_state().unwrap();
            assert_eq!(phase, crate::bft::Phase::Precommitting);
        }
    }

    #[test]
    fn step_returns_idle_when_no_progress() {
        let (vs, keys) = multi_validator_setup(3);
        let mut engine = engine_for(0, &vs, &keys, &genesis());
        // No proposal seen yet — step() must report idle.
        let p = <BftEngine as Engine>::step(&mut engine).unwrap();
        assert_eq!(p, EngineProgress::Idle);
    }

    #[test]
    fn step_after_commit_advances_head_in_multi_validator() {
        let (vs, keys) = multi_validator_setup(3);
        let g = genesis();
        let leader = vs.select_leader(1, 0, &[0x77; 32]);
        let mut engines: Vec<BftEngine> = (0..3).map(|i| engine_for(i, &vs, &keys, &g)).collect();
        let (block, proof) = engines[leader].cast_proposal().unwrap();
        for (i, e) in engines.iter_mut().enumerate() {
            if i != leader {
                e.submit_remote_proposal(block.clone(), proof.clone())
                    .unwrap();
            }
        }
        let prevotes: Vec<PrevoteVote> =
            engines.iter().map(|e| e.cast_prevote().unwrap()).collect();
        for (vi, v) in prevotes.iter().enumerate() {
            for (ei, e) in engines.iter().enumerate() {
                if ei != vi {
                    e.submit_remote_prevote(v.clone()).unwrap();
                }
            }
        }
        let precommits: Vec<PrecommitVote> = engines
            .iter()
            .map(|e| e.cast_precommit().unwrap())
            .collect();
        for (vi, v) in precommits.iter().enumerate() {
            for (ei, e) in engines.iter().enumerate() {
                if ei != vi {
                    e.submit_remote_precommit(v.clone()).unwrap();
                }
            }
        }
        let mut e0 = engines.remove(0);
        let p = <BftEngine as Engine>::step(&mut e0).unwrap();
        assert!(matches!(p, EngineProgress::NewBlock(_)));
        assert_eq!(e0.head().1, 1);
    }

    #[test]
    fn tick_timeout_advances_round_and_clears_proposal() {
        let (vs, keys) = multi_validator_setup(3);
        let leader = vs.select_leader(1, 0, &[0x77; 32]);
        let leader_e = engine_for(leader as u32, &vs, &keys, &genesis());
        leader_e.cast_proposal().unwrap();
        leader_e.tick_timeout().unwrap();
        let (h, r, phase) = leader_e.current_round_state().unwrap();
        assert_eq!(h, 1);
        assert_eq!(r, 1);
        assert_eq!(phase, crate::bft::Phase::AwaitingProposal);
    }

    #[test]
    fn submit_remote_proposal_with_invalid_leader_proof_rejected() {
        let (vs, keys) = multi_validator_setup(3);
        let leader_idx = vs.select_leader(1, 0, &[0x77; 32]);
        let non_leader = (leader_idx + 1) % 3;
        // Build a proposal using the WRONG VRF key — claiming to be the leader.
        let bad_proof = LeaderProof::produce(&keys[non_leader].1, 1, 0, &[0x77; 32]);
        let block = Block {
            header: Header {
                parent_hash: genesis().hash(),
                ommers_hash: EMPTY_LIST_HASH,
                beneficiary: Address::ZERO,
                state_root: EMPTY_TRIE_HASH,
                transactions_root: EMPTY_TRIE_HASH,
                receipts_root: EMPTY_TRIE_HASH,
                logs_bloom: Bloom::ZERO,
                difficulty: U256::ZERO,
                number: 1,
                gas_limit: 30_000_000,
                gas_used: 0,
                timestamp: 1_700_000_003,
                extra_data: vec![],
                mix_hash: H256::new(bad_proof.vrf_output),
                nonce: [0u8; 8],
                base_fee_per_gas: U256::from(1_000_000_000u64),
                withdrawals_root: EMPTY_TRIE_HASH,
                blob_gas_used: None,
                excess_blob_gas: None,
                parent_beacon_block_root: None,
            },
            body: BlockBody::default(),
        };
        let peer = engine_for(0, &vs, &keys, &genesis());
        assert_eq!(
            peer.submit_remote_proposal(block, bad_proof).unwrap_err(),
            BftError::InvalidVrfProof,
        );
    }

    #[test]
    fn next_height_starts_after_commit() {
        let (vs, keys) = multi_validator_setup(3);
        let g = genesis();
        let leader1 = vs.select_leader(1, 0, &[0x77; 32]);
        let mut engines: Vec<BftEngine> = (0..3).map(|i| engine_for(i, &vs, &keys, &g)).collect();
        let (block, proof) = engines[leader1].cast_proposal().unwrap();
        for (i, e) in engines.iter_mut().enumerate() {
            if i != leader1 {
                e.submit_remote_proposal(block.clone(), proof.clone())
                    .unwrap();
            }
        }
        let prevotes: Vec<PrevoteVote> =
            engines.iter().map(|e| e.cast_prevote().unwrap()).collect();
        for (vi, v) in prevotes.iter().enumerate() {
            for (ei, e) in engines.iter().enumerate() {
                if ei != vi {
                    e.submit_remote_prevote(v.clone()).unwrap();
                }
            }
        }
        let precommits: Vec<PrecommitVote> = engines
            .iter()
            .map(|e| e.cast_precommit().unwrap())
            .collect();
        for (vi, v) in precommits.iter().enumerate() {
            for (ei, e) in engines.iter().enumerate() {
                if ei != vi {
                    e.submit_remote_precommit(v.clone()).unwrap();
                }
            }
        }
        for e in &mut engines {
            <BftEngine as Engine>::step(e).unwrap();
        }
        // All engines at height 1 now. Round-state cleared.
        for e in &engines {
            assert_eq!(e.head().1, 1);
            assert!(
                e.current_round_state().is_none(),
                "coordinator should be cleared post-commit",
            );
        }
    }

    #[test]
    fn cast_precommit_without_polc_rejected() {
        let (vs, keys) = multi_validator_setup(3);
        let leader = vs.select_leader(1, 0, &[0x77; 32]);
        let leader_e = engine_for(leader as u32, &vs, &keys, &genesis());
        leader_e.cast_proposal().unwrap();
        // Only 1 prevote (mine) — below quorum, still in Prevoting.
        leader_e.cast_prevote().unwrap();
        assert!(leader_e.cast_precommit().is_err());
    }

    #[test]
    fn current_leader_index_matches_validator_set() {
        let (vs, keys) = multi_validator_setup(3);
        let leader = vs.select_leader(1, 0, &[0x77; 32]);
        let leader_e = engine_for(leader as u32, &vs, &keys, &genesis());
        // No coordinator yet — index is None.
        assert!(leader_e.current_leader_index().is_none());
        // Touch coordinator via cast_proposal.
        leader_e.cast_proposal().unwrap();
        assert_eq!(leader_e.current_leader_index(), Some(leader));
    }

    // ─────────────────── BftConfig::from_genesis (v0.0.31) ────────────────────

    use aii_config::{ChainSpec, Genesis, GenesisValidator};
    use aii_types::{BlsPubKey, VrfPubKey};

    /// Build a `Genesis` with `n` validators of stake 100 each. Returns
    /// the genesis plus each node's `(bls_sk, vrf_sk)`.
    fn genesis_with_n_validators(n: u8) -> (Genesis, Vec<(BlsSecretKey, VrfSecretKey)>) {
        let mut keys = Vec::new();
        let mut gen_validators = Vec::new();
        for i in 0..n {
            let bls = bls_sk(i + 1);
            let vrf = vrf_sk();
            gen_validators.push(GenesisValidator {
                bls_pubkey: BlsPubKey::new(bls.public_key().to_compressed()),
                vrf_pubkey: VrfPubKey::new(vrf.public_key().to_bytes()),
                stake: 100,
            });
            keys.push((bls, vrf));
        }
        let g = Genesis {
            chain_spec: ChainSpec::testnet(),
            timestamp: 1_700_000_000,
            extra_data: vec![],
            alloc: vec![],
            validators: gen_validators,
            initial_seed: [0x9a; 32],
        };
        (g, keys)
    }

    fn config_err(r: Result<BftConfig, BftError>) -> BftError {
        match r {
            Ok(_) => panic!("expected error but got Ok(BftConfig)"),
            Err(e) => e,
        }
    }

    #[test]
    fn bft_config_from_genesis_empty_validators_rejected() {
        let g = Genesis {
            chain_spec: ChainSpec::testnet(),
            timestamp: 0,
            extra_data: vec![],
            alloc: vec![],
            validators: vec![],
            initial_seed: [0; 32],
        };
        let err = config_err(BftConfig::from_genesis(
            &g,
            0,
            bls_sk(1),
            vrf_sk(),
            Address::ZERO,
        ));
        assert_eq!(err, BftError::EmptyValidatorSet);
    }

    #[test]
    fn bft_config_from_genesis_with_invalid_bls_pubkey_rejected() {
        let (mut g, keys) = genesis_with_n_validators(1);
        // Corrupt the BLS pubkey to all-zero (off-curve).
        g.validators[0].bls_pubkey = BlsPubKey::ZERO;
        let err = config_err(BftConfig::from_genesis(
            &g,
            0,
            keys[0].0.clone(),
            keys[0].1.clone(),
            Address::ZERO,
        ));
        assert_eq!(
            err,
            BftError::InvalidValidatorPubkey {
                index: 0,
                kind: "bls"
            },
        );
    }

    #[test]
    fn bft_config_from_genesis_my_index_out_of_bounds_rejected() {
        let (g, keys) = genesis_with_n_validators(3);
        let err = config_err(BftConfig::from_genesis(
            &g,
            5, // outside the 3-validator set
            keys[0].0.clone(),
            keys[0].1.clone(),
            Address::ZERO,
        ));
        assert!(matches!(
            err,
            BftError::ValidatorIndexOutOfBounds { index: 5, size: 3 },
        ));
    }

    #[test]
    fn bft_config_from_genesis_populates_chain_spec_params() {
        let (g, keys) = genesis_with_n_validators(1);
        let cfg = BftConfig::from_genesis(
            &g,
            0,
            keys[0].0.clone(),
            keys[0].1.clone(),
            Address::new([0xab; 20]),
        )
        .unwrap();
        assert_eq!(cfg.gas_limit, g.chain_spec.initial_gas_limit);
        assert_eq!(
            cfg.base_fee_per_gas,
            U256::from(g.chain_spec.min_base_fee_per_gas),
        );
        assert_eq!(cfg.slot_seconds, g.chain_spec.block_time_seconds);
        assert_eq!(cfg.initial_seed, g.initial_seed);
        assert_eq!(cfg.coinbase, Address::new([0xab; 20]));
        assert_eq!(cfg.validator_set.size(), 1);
    }

    #[test]
    fn bft_engine_from_genesis_single_validator_advances() {
        let (g, keys) = genesis_with_n_validators(1);
        let cfg =
            BftConfig::from_genesis(&g, 0, keys[0].0.clone(), keys[0].1.clone(), Address::ZERO)
                .unwrap();
        let genesis_block = Block {
            header: g.to_header(EMPTY_TRIE_HASH),
            body: BlockBody::default(),
        };
        let vs = cfg.validator_set.clone();
        let engine = BftEngine::new(cfg, &genesis_block);
        let out = engine.advance_single().unwrap();
        out.certificate.verify(&vs).unwrap();
        assert_eq!(out.block.header.number, 1);
    }

    #[test]
    fn three_validator_genesis_drives_e2e_consensus() {
        // Same flow as `three_node_consensus_produces_same_block` but
        // every BftEngine is built from the SAME genesis file.
        let (g, keys) = genesis_with_n_validators(3);
        let genesis_block = Block {
            header: g.to_header(EMPTY_TRIE_HASH),
            body: BlockBody::default(),
        };
        let engines: Vec<BftEngine> = (0..3u32)
            .map(|i| {
                let cfg = BftConfig::from_genesis(
                    &g,
                    i,
                    keys[i as usize].0.clone(),
                    keys[i as usize].1.clone(),
                    Address::ZERO,
                )
                .unwrap();
                BftEngine::new(cfg, &genesis_block)
            })
            .collect();

        // Recompute the leader from genesis (BftEngine intentionally
        // hides its config; the host derives leader externally if
        // needed — same data, same algorithm).
        let mut runtime_validators = Vec::new();
        for gv in &g.validators {
            runtime_validators.push(crate::bft::Validator {
                bls_pubkey: bls::PublicKey::from_compressed(&gv.bls_pubkey.0).unwrap(),
                vrf_pubkey: vrf::PublicKey::from_bytes(&gv.vrf_pubkey.0).unwrap(),
                stake: gv.stake,
            });
        }
        let vs_external = ValidatorSet::new(runtime_validators).unwrap();
        let leader = vs_external.select_leader(1, 0, &g.initial_seed);
        let (block, proof) = engines[leader].cast_proposal().unwrap();
        let block_hash = block.hash();

        for (i, e) in engines.iter().enumerate() {
            if i != leader {
                e.submit_remote_proposal(block.clone(), proof.clone())
                    .unwrap();
            }
        }

        let prevotes: Vec<PrevoteVote> =
            engines.iter().map(|e| e.cast_prevote().unwrap()).collect();
        for (vi, v) in prevotes.iter().enumerate() {
            for (ei, e) in engines.iter().enumerate() {
                if ei != vi {
                    e.submit_remote_prevote(v.clone()).unwrap();
                }
            }
        }
        let precommits: Vec<PrecommitVote> = engines
            .iter()
            .map(|e| e.cast_precommit().unwrap())
            .collect();
        for (vi, v) in precommits.iter().enumerate() {
            for (ei, e) in engines.iter().enumerate() {
                if ei != vi {
                    e.submit_remote_precommit(v.clone()).unwrap();
                }
            }
        }
        for mut e in engines {
            let progress = <BftEngine as Engine>::step(&mut e).unwrap();
            assert_eq!(progress, EngineProgress::NewBlock(block_hash));
            assert_eq!(e.head().0, block_hash);
        }
    }

    #[test]
    fn genesis_with_validators_json_round_trip() {
        let (g, _keys) = genesis_with_n_validators(3);
        let json = serde_json::to_string_pretty(&g).unwrap();
        let parsed: Genesis = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, g);
        // The JSON must surface validator pubkeys as 0x-prefixed hex.
        assert!(json.contains("\"bls_pubkey\""));
        assert!(json.contains("\"vrf_pubkey\""));
        assert!(json.contains("\"0x"));
    }

    #[test]
    fn bft_config_from_genesis_round_trip_via_json() {
        let (g, keys) = genesis_with_n_validators(2);
        let json = serde_json::to_string(&g).unwrap();
        let parsed: Genesis = serde_json::from_str(&json).unwrap();
        // Build engine config from the parsed (not original) Genesis.
        let cfg = BftConfig::from_genesis(
            &parsed,
            1,
            keys[1].0.clone(),
            keys[1].1.clone(),
            Address::new([0xcd; 20]),
        )
        .unwrap();
        assert_eq!(cfg.validator_set.size(), 2);
        assert_eq!(cfg.my_index, 1);
    }
}
