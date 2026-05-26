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

use aii_block::tx::Tx;
use aii_block::{Block, BlockBody, Bloom, Hashable, Header, EMPTY_LIST_HASH, EMPTY_TRIE_HASH};
use aii_consensus_iface::{ConsensusError, Engine, EngineProgress};
use aii_types::{Address, H256, U256};
use parking_lot::Mutex;
use thiserror::Error;

/// Gas cost charged per included tx in the v0.0.37 placeholder
/// pipeline (no actual EVM execution — every tx is treated as a
/// 21,000-gas transfer).
pub const PLACEHOLDER_TX_GAS: u64 = 21_000;

/// Errors from the PoA engine.
#[derive(Debug, Error)]
pub enum PoaError {
    /// `authorities` was empty.
    #[error("authority set must be non-empty")]
    EmptyAuthorities,
    /// Block number would overflow `u64`.
    #[error("block number overflow")]
    Overflow,
    /// secp256k1 signing failed (corrupt key, etc.).
    #[error("PoA seal sign failed")]
    SealSignFailed,
}

/// Configuration for a [`PoaEngine`].
#[derive(Clone)]
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
    /// Optional secp256k1 signer. When `Some`, every produced block
    /// is signed via [`produce_block`](PoaEngine::produce_block_signed)
    /// and the signature can be verified by any peer using
    /// [`verify_poa_seal`]. When `None`, blocks are produced without a
    /// seal — used by tests and by non-authority observers that only
    /// run the engine to compute next-slot predictions.
    pub signer_sk: Option<aii_crypto::secp::SecretKey>,
}

/// A 65-byte recoverable secp256k1 PoA seal signature over `block.hash()`.
///
/// Sealed blocks ship the signature out-of-band (alongside the block
/// body) rather than embedding it in `extra_data`; this keeps the
/// canonical Ethereum-compatible block layout intact. Verifiers pass
/// the sig + the block hash into [`verify_poa_seal`].
pub type PoaSeal = aii_crypto::secp::Signature;

/// Verify a PoA seal signature against the expected authority for `height`.
///
/// Returns `Ok(true)` iff:
/// 1. `sig` was produced over `block_hash`,
/// 2. the recovered signer's Ethereum address equals
///    `authorities[height % authorities.len()]`.
///
/// # Errors
/// Propagates [`aii_crypto::CryptoError`] from the underlying
/// signature recovery (malformed sig bytes, etc.).
pub fn verify_poa_seal(
    block_hash: &H256,
    sig: &PoaSeal,
    authorities: &[Address],
    height: u64,
) -> Result<bool, aii_crypto::CryptoError> {
    if authorities.is_empty() {
        return Ok(false);
    }
    let recovered = aii_crypto::secp::recover(sig, block_hash)?;
    let expected = authorities[(height as usize) % authorities.len()];
    Ok(recovered.address() == expected)
}

/// PoA consensus engine.
pub struct PoaEngine {
    config: PoaConfig,
    state: Arc<Mutex<PoaState>>,
    pending_txs: Mutex<Vec<Tx>>,
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
            pending_txs: Mutex::new(Vec::new()),
        })
    }

    /// Stage transactions to include in the next produced block.
    /// Overwrites any previously-staged batch.
    pub fn set_pending_txs(&self, txs: Vec<Tx>) {
        *self.pending_txs.lock() = txs;
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

        // Drain pending txs up to the block's gas budget.
        let max_txs = (self.config.gas_limit / PLACEHOLDER_TX_GAS) as usize;
        let mut pending = self.pending_txs.lock();
        let take = pending.len().min(max_txs);
        let txs: Vec<Tx> = pending.drain(..take).collect();
        drop(pending);
        let gas_used = (txs.len() as u64) * PLACEHOLDER_TX_GAS;

        let body = BlockBody {
            transactions: txs,
            ommers: Vec::new(),
            withdrawals: Vec::new(),
        };
        let tx_root = aii_state::transactions_root(&body);
        let header = Header {
            parent_hash: g.head_hash,
            ommers_hash: EMPTY_LIST_HASH,
            beneficiary: self.config.coinbase,
            state_root: EMPTY_TRIE_HASH,
            transactions_root: tx_root,
            receipts_root: EMPTY_TRIE_HASH,
            logs_bloom: Bloom::ZERO,
            difficulty: U256::ZERO,
            number: new_number,
            gas_limit: self.config.gas_limit,
            gas_used,
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
        let block = Block { header, body };
        let hash = block.hash();
        g.head_hash = hash;
        g.head_number = new_number;
        g.head_timestamp = new_timestamp;
        Ok((hash, new_number, block))
    }

    /// Produce the next block and additionally sign its hash with the
    /// configured PoA signer key. Returns the signature alongside the
    /// usual `(hash, number, block)` tuple. Returns `None` for the
    /// sig when no signer is configured.
    ///
    /// # Errors
    /// Returns [`PoaError::Overflow`] if the height counter would wrap,
    /// or [`PoaError::SealSignFailed`] if signing fails (only possible
    /// with a corrupt key).
    pub fn produce_block_signed(&self) -> Result<(H256, u64, Block, Option<PoaSeal>), PoaError> {
        let (hash, number, block) = self.produce_block()?;
        let sig = match &self.config.signer_sk {
            Some(sk) => {
                Some(aii_crypto::secp::sign(sk, &hash).map_err(|_| PoaError::SealSignFailed)?)
            }
            None => None,
        };
        Ok((hash, number, block, sig))
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
            signer_sk: None,
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

    #[test]
    fn produce_block_signed_returns_recoverable_seal() {
        use rand::RngCore;
        let mut sk_bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut sk_bytes);
        let sk = aii_crypto::secp::SecretKey::from_bytes(&sk_bytes).unwrap();
        let pk = sk.public_key();
        let signer_addr = pk.address();
        let mut c = cfg(vec![signer_addr], signer_addr);
        c.signer_sk = Some(sk);
        let e = PoaEngine::new(c, &genesis()).unwrap();
        let (hash, height, _block, sig) = e.produce_block_signed().unwrap();
        let sig = sig.expect("signer_sk set, sig must be present");
        // Verify roundtrip.
        let ok = verify_poa_seal(&hash, &sig, &[signer_addr], height).unwrap();
        assert!(ok, "PoA seal must verify against the producer's authority");
    }

    #[test]
    fn verify_poa_seal_rejects_wrong_authority() {
        use rand::RngCore;
        let mut a_bytes = [0u8; 32];
        let mut b_bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut a_bytes);
        rand::thread_rng().fill_bytes(&mut b_bytes);
        let sk_a = aii_crypto::secp::SecretKey::from_bytes(&a_bytes).unwrap();
        let sk_b = aii_crypto::secp::SecretKey::from_bytes(&b_bytes).unwrap();
        let addr_a = sk_a.public_key().address();
        let addr_b = sk_b.public_key().address();
        // Authority set = [a], producer = b (impostor).
        let mut c = cfg(vec![addr_a], addr_b);
        c.signer_sk = Some(sk_b);
        let e = PoaEngine::new(c, &genesis()).unwrap();
        // Force the slot — engine.is_my_turn() is false but produce_block ignores it.
        let (hash, height, _block, sig) = e.produce_block_signed().unwrap();
        let ok = verify_poa_seal(&hash, &sig.unwrap(), &[addr_a], height).unwrap();
        assert!(
            !ok,
            "PoA seal signed by an impostor must not verify against the legit authority"
        );
    }
}
