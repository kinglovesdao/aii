//! # aii-consensus-iface
//!
//! Trait surface that every AII consensus engine (main-chain BFT-PoS,
//! sub-chain PoS/PBFT/DPoS) implements. No engine logic lives here.
//!
//! ## Public API
//! - [`Engine`] — top-level: bootstrap, advance the chain by one round
//! - [`Proposer`] — propose blocks
//! - [`Voter`] — pre-commit voting (for BFT engines) or attestations
//! - [`Validation`] — header / block validation contract
//! - [`ConsensusError`] — umbrella error

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use aii_block::{Block, Header};
use aii_types::{Address, H256};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Tag identifying which consensus algorithm a chain (main or sub) runs.
///
/// Used by:
/// - `aii_microchain::MicroChainSpec::consensus` to mark a sub-chain's
///   algorithm in genesis;
/// - The `aiid` `--consensus` CLI flag;
/// - Factory code that instantiates the right `Engine` impl.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConsensusKind {
    /// VRF-PoS BFT (the AII main chain): two-phase prevote/precommit +
    /// ⅔ BLS aggregation. Implementation in `aii-consensus-bft`.
    Bft,
    /// Proof-of-Authority: fixed authority list, round-robin signing.
    /// Implementation in `aii-consensus-poa`.
    Poa,
}

impl ConsensusKind {
    /// Stable on-disk / on-wire string. Used in CLI args and genesis JSON.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Bft => "bft",
            Self::Poa => "poa",
        }
    }

    /// Parse from a CLI / JSON identifier. Case-insensitive on the
    /// accepted spellings.
    ///
    /// # Errors
    ///
    /// Returns `Err(s.to_owned())` if the string doesn't match a
    /// known kind — callers typically wrap this in their CLI error
    /// type.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "bft" | "bft-pos" => Ok(Self::Bft),
            "poa" | "proof-of-authority" => Ok(Self::Poa),
            other => Err(other.to_string()),
        }
    }
}

impl core::fmt::Display for ConsensusKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Top-level consensus engine.
pub trait Engine: Send + Sync {
    /// Initialise from genesis. Returns the genesis block's hash.
    fn init(&mut self, genesis: &Block) -> Result<H256, ConsensusError>;

    /// Drive the chain forward by one round. Implementations are free to
    /// block, sleep, or run async — the caller wraps in a task.
    fn step(&mut self) -> Result<EngineProgress, ConsensusError>;

    /// Return the current head block hash.
    fn head(&self) -> H256;

    /// Return the address operating this node, if any.
    fn coinbase(&self) -> Option<Address>;
}

/// Outcome of a single `Engine::step` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineProgress {
    /// No new block; engine is idle (waiting for peers, timers, etc.).
    Idle,
    /// A new block was finalised. The hash is included for callers that
    /// want to subscribe.
    NewBlock(H256),
}

/// Per-engine block proposer hook.
pub trait Proposer: Send + Sync {
    /// Propose a new block on top of `parent`.
    fn propose(&self, parent: &Header) -> Result<Block, ConsensusError>;
}

/// Per-engine voter hook (for BFT engines).
pub trait Voter: Send + Sync {
    /// Cast a pre-commit vote on `block`.
    fn pre_commit(&self, block: &Header) -> Result<Vote, ConsensusError>;
}

/// A pre-commit / attestation vote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vote {
    /// Voter (the V-node's address).
    pub voter: Address,
    /// Block being voted on.
    pub target: H256,
    /// Opaque signature bytes — interpreted per engine.
    pub signature: Vec<u8>,
}

/// Block validation contract. Engines invoke this on every block before
/// applying it to local state.
pub trait Validation: Send + Sync {
    /// Validate the header alone (signature, gas, timestamp, etc.).
    fn validate_header(&self, header: &Header) -> Result<(), ConsensusError>;

    /// Validate the full block (header + body consistency, transactions
    /// well-formed, etc.).
    fn validate_block(&self, block: &Block) -> Result<(), ConsensusError>;
}

/// Errors returned by consensus engines.
#[derive(Debug, Error)]
pub enum ConsensusError {
    /// Header invariant violated.
    #[error("invalid header: {0}")]
    InvalidHeader(&'static str),

    /// Block-body invariant violated.
    #[error("invalid block: {0}")]
    InvalidBlock(&'static str),

    /// Signature did not verify.
    #[error("bad signature")]
    BadSignature,

    /// Voter / proposer not in the active V-set.
    #[error("not in v-set")]
    NotInVSet,

    /// I/O or storage error.
    #[error("io: {0}")]
    Io(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyEngine {
        head: H256,
    }

    impl Engine for DummyEngine {
        fn init(&mut self, genesis: &Block) -> Result<H256, ConsensusError> {
            self.head = aii_block::Hashable::hash(&genesis.header);
            Ok(self.head)
        }
        fn step(&mut self) -> Result<EngineProgress, ConsensusError> {
            Ok(EngineProgress::Idle)
        }
        fn head(&self) -> H256 {
            self.head
        }
        fn coinbase(&self) -> Option<Address> {
            None
        }
    }

    #[test]
    fn consensus_kind_round_trips_via_str() {
        assert_eq!(ConsensusKind::Bft.as_str(), "bft");
        assert_eq!(ConsensusKind::Poa.as_str(), "poa");
        assert_eq!(ConsensusKind::parse("bft").unwrap(), ConsensusKind::Bft);
        assert_eq!(ConsensusKind::parse("BFT").unwrap(), ConsensusKind::Bft);
        assert_eq!(ConsensusKind::parse("poa").unwrap(), ConsensusKind::Poa);
        assert_eq!(
            ConsensusKind::parse("proof-of-authority").unwrap(),
            ConsensusKind::Poa,
        );
        assert!(ConsensusKind::parse("nonsense").is_err());
    }

    #[test]
    fn consensus_kind_serialises_as_lowercase_json() {
        let json = serde_json::to_string(&ConsensusKind::Poa).unwrap();
        assert_eq!(json, "\"poa\"");
        let back: ConsensusKind = serde_json::from_str("\"bft\"").unwrap();
        assert_eq!(back, ConsensusKind::Bft);
    }

    #[test]
    fn trait_is_object_safe() {
        let _: Option<Box<dyn Engine>> = None;
        let _: Option<Box<dyn Proposer>> = None;
        let _: Option<Box<dyn Voter>> = None;
        let _: Option<Box<dyn Validation>> = None;
    }

    #[test]
    fn dummy_engine_default_progress_is_idle() {
        let mut e = DummyEngine { head: H256::ZERO };
        assert_eq!(e.step().unwrap(), EngineProgress::Idle);
    }

    #[test]
    fn consensus_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ConsensusError>();
    }

    #[test]
    fn engine_progress_round_trips_via_match() {
        let p = EngineProgress::NewBlock(H256::new([0x11; 32]));
        match p {
            EngineProgress::NewBlock(h) => assert_eq!(h.0[0], 0x11),
            EngineProgress::Idle => panic!("wrong variant"),
        }
    }
}
