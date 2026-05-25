//! # aii-microchain
//!
//! Sub-chain (microchain) registry and flush bookkeeping. Full lifecycle
//! (consensus plug-in, cross-chain bridges) lives in `aii-consensus-plugins`
//! and `aii-crosschain` — this crate is the registry surface.
//!
//! ## Public API
//! - [`MicroChainId`] — `u32` newtype
//! - [`MicroChainSpec`] — id, name, parent flush interval (blocks)
//! - [`Registry`] — in-memory registry; persistent storage is a node
//!   concern (wires through `ColumnFamily::MicroChain`)
//! - [`FlushAnchor`] — last parent + sub block hashes flushed
//! - [`MicroChainError`] umbrella

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use aii_consensus_iface::ConsensusKind;
use aii_types::H256;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

const fn default_consensus_kind() -> ConsensusKind {
    ConsensusKind::Bft
}

/// Sub-chain identifier (32-bit, reserved 0 for main chain).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MicroChainId(pub u32);

impl MicroChainId {
    /// Reserved id of the main chain (never used as a microchain).
    pub const MAIN: Self = Self(0);
}

/// Specification of one sub-chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MicroChainSpec {
    /// Stable identifier.
    pub id: MicroChainId,
    /// Human-readable name.
    pub name: String,
    /// Every N sub-chain blocks, the sub-chain proposer flushes a checkpoint
    /// to the main chain.
    pub flush_interval_blocks: u64,
    /// Which consensus algorithm this sub-chain runs. Old specs that
    /// did not carry this field deserialise as
    /// [`ConsensusKind::Bft`] for backward compatibility.
    #[serde(default = "default_consensus_kind")]
    pub consensus: ConsensusKind,
}

/// Per-sub-chain "last flushed" marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlushAnchor {
    /// Sub-chain block hash that was checkpointed.
    pub sub_block_hash: H256,
    /// Main-chain block hash that carries the checkpoint.
    pub parent_block_hash: H256,
    /// Sub-chain block number at the time of the checkpoint.
    pub sub_block_number: u64,
}

/// In-memory registry of microchains.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    specs: BTreeMap<MicroChainId, MicroChainSpec>,
    anchors: BTreeMap<MicroChainId, FlushAnchor>,
}

impl Registry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new microchain. Duplicate ids → error.
    pub fn register(&mut self, spec: MicroChainSpec) -> Result<(), MicroChainError> {
        if spec.id == MicroChainId::MAIN {
            return Err(MicroChainError::ReservedId);
        }
        if self.specs.contains_key(&spec.id) {
            return Err(MicroChainError::Duplicate);
        }
        if spec.flush_interval_blocks == 0 {
            return Err(MicroChainError::InvalidFlushInterval);
        }
        self.specs.insert(spec.id, spec);
        Ok(())
    }

    /// Look up a microchain by id.
    pub fn get(&self, id: MicroChainId) -> Option<&MicroChainSpec> {
        self.specs.get(&id)
    }

    /// Iterate all registered specs in id order.
    pub fn iter(&self) -> impl Iterator<Item = &MicroChainSpec> {
        self.specs.values()
    }

    /// Number of microchains.
    pub fn len(&self) -> usize {
        self.specs.len()
    }

    /// `true` iff no microchains are registered.
    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }

    /// Persist a new flush anchor for `id`. Overwrites any prior anchor.
    /// Returns an error if `id` is not registered.
    pub fn record_flush(
        &mut self,
        id: MicroChainId,
        anchor: FlushAnchor,
    ) -> Result<(), MicroChainError> {
        if !self.specs.contains_key(&id) {
            return Err(MicroChainError::Unknown);
        }
        self.anchors.insert(id, anchor);
        Ok(())
    }

    /// Look up the last flush anchor for `id`.
    pub fn last_flush(&self, id: MicroChainId) -> Option<&FlushAnchor> {
        self.anchors.get(&id)
    }
}

/// Errors produced by the microchain registry.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum MicroChainError {
    /// Attempted to register id 0 (reserved for main chain).
    #[error("microchain id 0 is reserved for the main chain")]
    ReservedId,

    /// Attempted to register an existing id.
    #[error("microchain id already registered")]
    Duplicate,

    /// `flush_interval_blocks` was zero.
    #[error("flush interval must be > 0")]
    InvalidFlushInterval,

    /// Referenced an id that was never registered.
    #[error("unknown microchain id")]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(id: u32) -> MicroChainSpec {
        MicroChainSpec {
            id: MicroChainId(id),
            name: format!("sub-{id}"),
            flush_interval_blocks: 100,
            consensus: ConsensusKind::Bft,
        }
    }

    #[test]
    fn empty_registry() {
        let r = Registry::new();
        assert!(r.is_empty());
    }

    #[test]
    fn register_increments_len() {
        let mut r = Registry::new();
        r.register(spec(1)).unwrap();
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn duplicate_id_rejected() {
        let mut r = Registry::new();
        r.register(spec(1)).unwrap();
        assert_eq!(r.register(spec(1)), Err(MicroChainError::Duplicate));
    }

    #[test]
    fn reserved_main_chain_id_rejected() {
        let mut r = Registry::new();
        let s = MicroChainSpec {
            id: MicroChainId::MAIN,
            name: "x".to_string(),
            flush_interval_blocks: 1,
            consensus: ConsensusKind::Bft,
        };
        assert_eq!(r.register(s), Err(MicroChainError::ReservedId));
    }

    #[test]
    fn zero_flush_interval_rejected() {
        let mut r = Registry::new();
        let mut s = spec(1);
        s.flush_interval_blocks = 0;
        assert_eq!(r.register(s), Err(MicroChainError::InvalidFlushInterval));
    }

    #[test]
    fn record_flush_round_trip() {
        let mut r = Registry::new();
        r.register(spec(1)).unwrap();
        let anchor = FlushAnchor {
            sub_block_hash: H256::new([0xaa; 32]),
            parent_block_hash: H256::new([0xbb; 32]),
            sub_block_number: 42,
        };
        r.record_flush(MicroChainId(1), anchor.clone()).unwrap();
        assert_eq!(r.last_flush(MicroChainId(1)), Some(&anchor));
    }

    #[test]
    fn flush_unknown_errors() {
        let mut r = Registry::new();
        let anchor = FlushAnchor {
            sub_block_hash: H256::ZERO,
            parent_block_hash: H256::ZERO,
            sub_block_number: 0,
        };
        assert_eq!(
            r.record_flush(MicroChainId(99), anchor),
            Err(MicroChainError::Unknown)
        );
    }

    #[test]
    fn spec_with_poa_consensus_round_trips_via_json() {
        let s = MicroChainSpec {
            id: MicroChainId(7),
            name: "sub-poa".to_string(),
            flush_interval_blocks: 50,
            consensus: ConsensusKind::Poa,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"consensus\":\"poa\""));
        let back: MicroChainSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(back.consensus, ConsensusKind::Poa);
    }

    #[test]
    fn legacy_spec_without_consensus_field_defaults_to_bft() {
        // Pre-v0.0.35 sub-chain genesis JSON did not have a "consensus"
        // field. Make sure those still parse and pick up Bft.
        let json = r#"{"id":1,"name":"sub-legacy","flush_interval_blocks":100}"#;
        let parsed: MicroChainSpec = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.consensus, ConsensusKind::Bft);
    }

    #[test]
    fn iter_yields_in_id_order() {
        let mut r = Registry::new();
        r.register(spec(3)).unwrap();
        r.register(spec(1)).unwrap();
        r.register(spec(2)).unwrap();
        let ids: Vec<u32> = r.iter().map(|s| s.id.0).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }
}
