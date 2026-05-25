//! # aii-consensus-bft
//!
//! Main-chain BFT-PoS engine. v0.0.15 ships a **single-node dev-mode
//! producer**: each [`DevModeEngine::step`] builds an empty block on top
//! of the current head, hashes the header, and advances the height.
//!
//! ## Public API
//! - [`EngineConfig`] — slot interval + chain id; sensible defaults via
//!   `Default::default()`.
//! - [`DevModeEngine`] — single-node block producer; implements
//!   `aii_consensus_iface::Engine` so embedders can swap to a real
//!   multi-validator engine later without API churn.
//! - [`BftError`] — thin wrapper around `ConsensusError`.
//!
//! Real multi-validator BFT (VRF proposer + PRE-VOTE / PRE-COMMIT gossip
//! + ⅔ stake aggregation + BLS finality cert) lands in v0.0.16+.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod bft;
pub mod coordinator;
pub mod wire;
pub use bft::{
    LeaderProof, Phase, PolcCertificate, PrecommitCertificate, PrecommitTallier, PrecommitVote,
    PrevoteTallier, PrevoteVote, TallyState, Validator, ValidatorSet, MAX_VALIDATORS,
    PRECOMMIT_DOMAIN, PREVOTE_DOMAIN,
};
pub use coordinator::RoundCoordinator;
pub use wire::{
    BftMessage, CodecError, PROPOSAL_LEN, TAG_PRECOMMIT, TAG_PREVOTE, TAG_PROPOSAL, VOTE_LEN,
};

use aii_block::{Block, BlockBody, Bloom, Hashable, Header, EMPTY_LIST_HASH, EMPTY_TRIE_HASH};
use aii_consensus_iface::{ConsensusError, Engine, EngineProgress};
use aii_types::{Address, H256, U256};
use parking_lot::Mutex;
use std::sync::Arc;
use thiserror::Error;

/// Default slot duration (seconds). Spec target: 3 s; configurable for
/// faster CI / dev cycles.
pub const DEFAULT_SLOT_SECONDS: u64 = 3;

/// Engine configuration.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Slot interval in seconds (informational in dev mode — the engine
    /// produces a block per `step()` call; callers throttle).
    pub slot_seconds: u64,
    /// Block-producing coinbase address (the dev validator).
    pub coinbase: Address,
    /// EIP-1559 base fee for produced blocks (Wei).
    pub base_fee_per_gas: U256,
    /// Gas limit applied to every produced block.
    pub gas_limit: u64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            slot_seconds: DEFAULT_SLOT_SECONDS,
            coinbase: Address::ZERO,
            base_fee_per_gas: U256::from(1_000_000_000u64),
            gas_limit: 30_000_000,
        }
    }
}

/// Single-node dev-mode BFT engine.
///
/// Tracks the current head locally; each `step()` produces an empty
/// child block and updates the head. Thread-safe via `parking_lot::Mutex`.
pub struct DevModeEngine {
    config: EngineConfig,
    state: Arc<Mutex<EngineState>>,
}

#[allow(clippy::struct_field_names)]
struct EngineState {
    head_hash: H256,
    head_number: u64,
    head_timestamp: u64,
}

impl DevModeEngine {
    /// Construct from a config + genesis block.
    pub fn new(config: EngineConfig, genesis: &Block) -> Self {
        Self {
            config,
            state: Arc::new(Mutex::new(EngineState {
                head_hash: genesis.hash(),
                head_number: genesis.header.number,
                head_timestamp: genesis.header.timestamp,
            })),
        }
    }

    /// Snapshot the current head.
    pub fn head(&self) -> (H256, u64) {
        let g = self.state.lock();
        (g.head_hash, g.head_number)
    }

    /// Produce the next block (empty body) and advance the head.
    /// Returns the new head hash and number.
    #[allow(clippy::significant_drop_tightening)]
    pub fn produce_block(&self) -> Result<(H256, u64, Block), BftError> {
        let mut g = self.state.lock();
        let new_number = g.head_number.checked_add(1).ok_or(BftError::Overflow)?;
        let new_timestamp = g.head_timestamp + self.config.slot_seconds;
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
            extra_data: b"aii-dev-mode".to_vec(),
            mix_hash: H256::ZERO,
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
        let new_hash = block.hash();
        g.head_hash = new_hash;
        g.head_number = new_number;
        g.head_timestamp = new_timestamp;
        Ok((new_hash, new_number, block))
    }
}

impl Engine for DevModeEngine {
    fn init(&mut self, genesis: &Block) -> Result<H256, ConsensusError> {
        let mut g = self.state.lock();
        g.head_hash = genesis.hash();
        g.head_number = genesis.header.number;
        g.head_timestamp = genesis.header.timestamp;
        Ok(g.head_hash)
    }

    fn step(&mut self) -> Result<EngineProgress, ConsensusError> {
        let (hash, _, _block) = self
            .produce_block()
            .map_err(|e| ConsensusError::InvalidBlock(static_msg(&e)))?;
        Ok(EngineProgress::NewBlock(hash))
    }

    fn head(&self) -> H256 {
        self.state.lock().head_hash
    }

    fn coinbase(&self) -> Option<Address> {
        Some(self.config.coinbase)
    }
}

const fn static_msg(e: &BftError) -> &'static str {
    match e {
        BftError::Overflow => "block number overflowed u64",
        // The block-producer path only ever raises `Overflow`. The bft
        // submodule's errors are surfaced through its own API, never
        // through `static_msg`.
        _ => "bft error (see BftError variant for details)",
    }
}

/// Errors produced by the BFT engine.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum BftError {
    /// Block number would overflow `u64` (impossible in practice).
    #[error("block number overflow")]
    Overflow,

    /// `ValidatorSet::new` called with no validators.
    #[error("validator set must be non-empty")]
    EmptyValidatorSet,

    /// `ValidatorSet::new` called with more than [`MAX_VALIDATORS`] entries.
    #[error("validator set size {got} exceeds maximum {max}")]
    ValidatorSetTooLarge {
        /// Validators supplied.
        got: usize,
        /// Maximum permitted.
        max: usize,
    },

    /// `Σ stake` would overflow `u64`.
    #[error("total stake overflows u64")]
    TotalStakeOverflow,

    /// `Σ stake == 0` — no validator has any stake.
    #[error("total stake is zero")]
    ZeroTotalStake,

    /// Vote's `block_hash` does not match the tallier's block.
    #[error("vote targets a different block hash than this tallier")]
    WrongBlockHash,

    /// Vote's `height` does not match the tallier's height.
    #[error("vote targets a different height than this tallier")]
    WrongHeight,

    /// Vote's `round` does not match the tallier's round.
    #[error("vote targets a different round than this tallier")]
    WrongRound,

    /// Vote's `validator_index` is outside the set.
    #[error("validator index {index} out of bounds for set of size {size}")]
    ValidatorIndexOutOfBounds {
        /// Index supplied by the vote.
        index: u32,
        /// Validator set size at tally time.
        size: usize,
    },

    /// Validator at this index has already voted.
    #[error("validator {0} has already voted")]
    DuplicateVote(u32),

    /// BLS signature failed to verify against the validator's pubkey.
    #[error("invalid BLS signature")]
    InvalidBlsSignature,

    /// VRF proof failed to verify.
    #[error("invalid VRF proof")]
    InvalidVrfProof,

    /// Operation attempted in a state that doesn't accept it (e.g.
    /// submitting a PRE-VOTE while the coordinator is `AwaitingProposal`).
    #[error("wrong phase: expected {expected:?}, was {actual:?}")]
    WrongPhase {
        /// Phase the coordinator would have accepted the input in.
        expected: Phase,
        /// Phase the coordinator was actually in.
        actual: Phase,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use aii_block::BlockBody;

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

    #[test]
    fn head_starts_at_genesis() {
        let g = genesis();
        let engine = DevModeEngine::new(EngineConfig::default(), &g);
        assert_eq!(engine.head(), (g.hash(), 0));
    }

    #[test]
    fn produce_block_advances_number_by_one() {
        let engine = DevModeEngine::new(EngineConfig::default(), &genesis());
        let (_, n, _) = engine.produce_block().unwrap();
        assert_eq!(n, 1);
        let (_, n2, _) = engine.produce_block().unwrap();
        assert_eq!(n2, 2);
    }

    #[test]
    fn produced_block_parent_hash_matches_previous_head() {
        let engine = DevModeEngine::new(EngineConfig::default(), &genesis());
        let (h1, _, b1) = engine.produce_block().unwrap();
        let (_, _, b2) = engine.produce_block().unwrap();
        assert_eq!(b1.hash(), h1);
        assert_eq!(b2.header.parent_hash, h1);
    }

    #[test]
    fn timestamp_advances_by_slot_seconds() {
        let cfg = EngineConfig {
            slot_seconds: 5,
            ..EngineConfig::default()
        };
        let engine = DevModeEngine::new(cfg, &genesis());
        let (_, _, b1) = engine.produce_block().unwrap();
        let (_, _, b2) = engine.produce_block().unwrap();
        assert_eq!(b2.header.timestamp - b1.header.timestamp, 5);
    }

    #[test]
    fn engine_trait_step_emits_new_block_progress() {
        let mut engine = DevModeEngine::new(EngineConfig::default(), &genesis());
        let progress = aii_consensus_iface::Engine::step(&mut engine).unwrap();
        assert!(matches!(progress, EngineProgress::NewBlock(_)));
    }

    #[test]
    fn engine_trait_head_matches_local_head() {
        let mut engine = DevModeEngine::new(EngineConfig::default(), &genesis());
        let _ = aii_consensus_iface::Engine::step(&mut engine).unwrap();
        let h_trait = aii_consensus_iface::Engine::head(&engine);
        let (h_local, _) = engine.head();
        assert_eq!(h_trait, h_local);
    }

    #[test]
    fn coinbase_returned_from_config() {
        let cfg = EngineConfig {
            coinbase: Address::new([0x42; 20]),
            ..EngineConfig::default()
        };
        let engine = DevModeEngine::new(cfg, &genesis());
        assert_eq!(
            aii_consensus_iface::Engine::coinbase(&engine),
            Some(Address::new([0x42; 20]))
        );
    }

    #[test]
    fn init_resets_to_supplied_genesis() {
        let g = genesis();
        let mut engine = DevModeEngine::new(EngineConfig::default(), &g);
        let _ = aii_consensus_iface::Engine::step(&mut engine).unwrap();
        // advance, then re-init to the same genesis.
        aii_consensus_iface::Engine::init(&mut engine, &g).unwrap();
        let (_, n) = engine.head();
        assert_eq!(n, 0);
    }
}
