//! # aii-consensus-poa
//!
//! Proof-of-Authority consensus engine. Fixed set of authority
//! addresses; for height `H`, the proposer is
//! `authorities[H % authorities.len()]`. The local node only produces
//! a block when its `coinbase` matches the elected authority.
//!
//! No voting, no quorum, no slashing. The chain is final as soon as
//! the authority signs.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::significant_drop_tightening)]

use std::sync::Arc;

use aii_block::{Block, BlockBody, Bloom, Hashable, Header, EMPTY_LIST_HASH, EMPTY_TRIE_HASH};
use aii_consensus_iface::{ConsensusError, Engine, EngineProgress};
use aii_types::{Address, H256, U256};
use parking_lot::Mutex;
use thiserror::Error;

/// Errors from the PoA engine.
#[derive(Debug, Error)]
pub enum PoaError {
    /// `authorities` was empty.
    #[error("authority set must be non-empty")]
    EmptyAuthorities,
    /// Block number would overflow `u64`.
    #[error("block number overflow")]
    Overflow,
}

/// Configuration for a [`PoaEngine`].
#[derive(Debug, Clone)]
pub struct PoaConfig {
    /// Ordered authority list. Position determines round-robin slot.
    pub authorities: Vec<Address>,
    /// Address operating this node. Only blocks where
    /// `authorities[height % len] == coinbase` will be produced locally.
    pub coinbase: Address,
    /// Slot duration in seconds — drives `block.header.timestamp`.
    pub slot_seconds: u64,
    /// Gas limit applied to every produced block.
    pub gas_limit: u64,
    /// EIP-1559 base fee.
    pub base_fee_per_gas: U256,
}

/// PoA consensus engine.
pub struct PoaEngine {
    config: PoaConfig,
    state: Arc<Mutex<PoaState>>,
}

#[allow(clippy::struct_field_names)]
struct PoaState {
    head_hash: H256,
    head_number: u64,
    head_timestamp: u64,
}

impl PoaEngine {
    /// Construct from config + genesis block. Returns
    /// [`PoaError::EmptyAuthorities`] if `config.authorities` is empty.
    pub fn new(config: PoaConfig, genesis: &Block) -> Result<Self, PoaError> {
        if config.authorities.is_empty() {
            return Err(PoaError::EmptyAuthorities);
        }
        let state = PoaState {
            head_hash: genesis.hash(),
            head_number: genesis.header.number,
            head_timestamp: genesis.header.timestamp,
        };
        Ok(Self {
            config,
            state: Arc::new(Mutex::new(state)),
        })
    }

    /// Snapshot the chain head.
    #[must_use]
    pub fn head(&self) -> (H256, u64) {
        let g = self.state.lock();
        (g.head_hash, g.head_number)
    }

    /// Index of the authority elected for the next height.
    #[must_use]
    pub fn next_authority_index(&self) -> usize {
        let h = self.state.lock();
        let next = h.head_number.wrapping_add(1);
        let len = u64::try_from(self.config.authorities.len()).unwrap_or(1);
        usize::try_from(next % len).unwrap_or(0)
    }

    /// `true` iff this node's `coinbase` is the elected authority for
    /// the next height.
    #[must_use]
    pub fn is_my_turn(&self) -> bool {
        self.config.authorities[self.next_authority_index()] == self.config.coinbase
    }

    /// Build and commit the next block. Only callable when
    /// [`is_my_turn`](Self::is_my_turn) returns `true`; otherwise
    /// returns `EngineProgress::Idle` via `step()`.
    pub fn produce_block(&self) -> Result<(H256, u64, Block), PoaError> {
        let mut g = self.state.lock();
        let new_number = g.head_number.checked_add(1).ok_or(PoaError::Overflow)?;
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
            extra_data: b"aii-poa".to_vec(),
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
        let hash = block.hash();
        g.head_hash = hash;
        g.head_number = new_number;
        g.head_timestamp = new_timestamp;
        Ok((hash, new_number, block))
    }
}

impl Engine for PoaEngine {
    fn init(&mut self, genesis: &Block) -> Result<H256, ConsensusError> {
        let mut g = self.state.lock();
        g.head_hash = genesis.hash();
        g.head_number = genesis.header.number;
        g.head_timestamp = genesis.header.timestamp;
        Ok(g.head_hash)
    }

    fn step(&mut self) -> Result<EngineProgress, ConsensusError> {
        if !self.is_my_turn() {
            return Ok(EngineProgress::Idle);
        }
        let (hash, _n, _block) = self
            .produce_block()
            .map_err(|_e| ConsensusError::InvalidBlock("PoA produce failed"))?;
        Ok(EngineProgress::NewBlock(hash))
    }

    fn head(&self) -> H256 {
        self.state.lock().head_hash
    }

    fn coinbase(&self) -> Option<Address> {
        Some(self.config.coinbase)
    }
}

impl PoaEngine {
    /// Head block number (convenience for tests + RPC).
    #[must_use]
    pub fn head_number(&self) -> u64 {
        self.state.lock().head_number
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn cfg(authorities: Vec<Address>, coinbase: Address) -> PoaConfig {
        PoaConfig {
            authorities,
            coinbase,
            slot_seconds: 2,
            gas_limit: 30_000_000,
            base_fee_per_gas: U256::from(1_000_000_000u64),
        }
    }

    #[test]
    fn engine_new_starts_at_genesis() {
        let a = Address::new([0xa1; 20]);
        let e = PoaEngine::new(cfg(vec![a], a), &genesis()).unwrap();
        let (h, n) = e.head();
        assert_eq!(h, genesis().hash());
        assert_eq!(n, 0);
    }

    #[test]
    fn empty_authority_set_rejected() {
        let a = Address::new([0xa1; 20]);
        assert!(PoaEngine::new(cfg(vec![], a), &genesis()).is_err());
    }

    #[test]
    fn single_authority_produces_every_block() {
        let a = Address::new([0xa1; 20]);
        let mut e = PoaEngine::new(cfg(vec![a], a), &genesis()).unwrap();
        for expected in 1..=5u64 {
            let p = Engine::step(&mut e).unwrap();
            assert!(matches!(p, EngineProgress::NewBlock(_)));
            assert_eq!(e.head_number(), expected);
        }
    }

    #[test]
    fn non_authority_is_idle() {
        let a = Address::new([0xa1; 20]);
        let stranger = Address::new([0x99; 20]);
        // Authority set = {a}, but our coinbase is stranger.
        let mut e = PoaEngine::new(cfg(vec![a], stranger), &genesis()).unwrap();
        let p = Engine::step(&mut e).unwrap();
        assert_eq!(p, EngineProgress::Idle);
        let (_, n) = e.head();
        assert_eq!(n, 0);
    }

    #[test]
    fn two_authorities_round_robin() {
        // Authorities [a, b]: heights 1,3,5… → b; 2,4,6 → a (since (h % 2)=0 → a, =1 → b).
        let a = Address::new([0xa1; 20]);
        let b = Address::new([0xb1; 20]);
        // Engine A: only proposes on heights where (h % 2) == 0 → 2, 4, 6.
        let mut e_a = PoaEngine::new(cfg(vec![a, b], a), &genesis()).unwrap();
        // First step on engine_a: next_authority = authorities[1 % 2] = b → A is idle.
        assert_eq!(Engine::step(&mut e_a).unwrap(), EngineProgress::Idle);
        assert_eq!(e_a.head_number(), 0);
    }

    #[test]
    fn parent_hash_chain_links() {
        let a = Address::new([0xa1; 20]);
        let e = PoaEngine::new(cfg(vec![a], a), &genesis()).unwrap();
        let (h1, _, b1) = e.produce_block().unwrap();
        let (_, _, b2) = e.produce_block().unwrap();
        assert_eq!(b2.header.parent_hash, h1);
        assert_eq!(b1.header.parent_hash, genesis().hash());
        assert_eq!(b2.header.number, 2);
    }

    #[test]
    fn engine_init_resets_to_genesis() {
        let a = Address::new([0xa1; 20]);
        let mut e = PoaEngine::new(cfg(vec![a], a), &genesis()).unwrap();
        e.produce_block().unwrap();
        assert_eq!(e.head().1, 1);
        let g = genesis();
        Engine::init(&mut e, &g).unwrap();
        assert_eq!(e.head().1, 0);
    }

    #[test]
    fn engine_coinbase_returned() {
        let a = Address::new([0xa1; 20]);
        let e = PoaEngine::new(cfg(vec![a], a), &genesis()).unwrap();
        assert_eq!(Engine::coinbase(&e), Some(a));
    }
}
