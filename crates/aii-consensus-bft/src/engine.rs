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
}

impl BftEngine {
    /// Construct from config + genesis block.
    pub fn new(config: BftConfig, genesis: &Block) -> Self {
        let state = BftEngineState {
            head_hash: genesis.hash(),
            head_number: genesis.header.number,
            head_timestamp: genesis.header.timestamp,
            seed: config.initial_seed,
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
            Ok(EngineProgress::NewBlock(out.block_hash))
        } else {
            // Multi-validator drive lands in v0.0.30; for now report idle.
            Ok(EngineProgress::Idle)
        }
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
}
