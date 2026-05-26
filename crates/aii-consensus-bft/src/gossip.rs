//! BFT gossip driver (v0.0.34).
//!
//! Bridges the local [`BftEngine`] to a peer-to-peer transport. The
//! engine owns consensus state; the transport owns network sockets;
//! this module is the glue that turns engine output into broadcast
//! bytes and incoming bytes into engine input.
//!
//! ## Model
//!
//! - [`BftTransport`] — a sync trait. Implementations push outbound
//!   bytes and pull inbound bytes. For tests, an in-memory channel
//!   pair (see [`tests`]). For production, a thin wrapper around
//!   `aii-net-p2p::Peer` that ships `BftMessage::encode()` payloads
//!   inside `aii_net_p2p::Message::Bft`.
//! - [`BftGossip`] — owns an `Arc<BftEngine>` + a `T: BftTransport`.
//!   Per `tick()` it:
//!   1. Drains the transport inbox and routes each [`BftMessage`] into
//!      `engine.submit_remote_*`.
//!   2. Inspects `engine.current_round_state()` and, when appropriate,
//!      calls `engine.cast_proposal / cast_prevote / cast_precommit`,
//!      broadcasting the result.
//!   3. Tracks per-`(height, round)` "already voted" flags so it does
//!      not call `cast_*` twice in the same round.
//!
//! No timers, no async. Hosts drive `tick()` from their own loop.

use std::sync::Arc;

use alloy_rlp::{Decodable, Encodable};
use parking_lot::Mutex;

use crate::bft::{LeaderProof, Phase};
use crate::engine::BftEngine;
use crate::wire::BftMessage;
use crate::BftError;
use aii_block::{BlockBody, Hashable};

/// Sync sink/source of opaque gossip bytes.
///
/// Encoding of those bytes is decided by the gossip driver: today
/// always [`BftMessage::encode`] / [`BftMessage::decode`].
pub trait BftTransport: Send + Sync {
    /// Best-effort fan-out of `bytes` to every connected peer.
    /// Implementations must NOT block.
    fn broadcast(&self, bytes: Vec<u8>);

    /// Pop one pending inbound payload, if any. Returns `None` when
    /// the inbox is empty.
    fn try_recv(&self) -> Option<Vec<u8>>;
}

impl<T: BftTransport + ?Sized> BftTransport for Arc<T> {
    fn broadcast(&self, bytes: Vec<u8>) {
        (**self).broadcast(bytes);
    }
    fn try_recv(&self) -> Option<Vec<u8>> {
        (**self).try_recv()
    }
}

/// Per-round voting bookkeeping, keyed on `(height, round)`.
///
/// We also cache the *bytes* of the last message we emitted in each
/// phase so we can re-broadcast on every tick. This guards against
/// the startup race where a leader broadcasts a Proposal before the
/// TCP handshake with its peer has completed: a stale subscriber
/// would otherwise never see the message and the round would stall.
#[derive(Default)]
struct RoundFlags {
    proposed: Option<(u64, u32)>,
    prevoted: Option<(u64, u32)>,
    precommitted: Option<(u64, u32)>,
    last_proposal: Option<Vec<u8>>,
    last_prevote: Option<Vec<u8>>,
    last_precommit: Option<Vec<u8>>,
}

impl RoundFlags {
    fn already_proposed(&self, h: u64, r: u32) -> bool {
        self.proposed == Some((h, r))
    }
    fn already_prevoted(&self, h: u64, r: u32) -> bool {
        self.prevoted == Some((h, r))
    }
    fn already_precommitted(&self, h: u64, r: u32) -> bool {
        self.precommitted == Some((h, r))
    }
}

/// Drives one validator's BFT participation against a transport.
pub struct BftGossip<T: BftTransport> {
    engine: Arc<BftEngine>,
    transport: T,
    flags: Mutex<RoundFlags>,
}

impl<T: BftTransport> BftGossip<T> {
    /// Construct a new gossip driver.
    pub fn new(engine: Arc<BftEngine>, transport: T) -> Self {
        Self {
            engine,
            transport,
            flags: Mutex::new(RoundFlags::default()),
        }
    }

    /// Borrow the engine (for tests / diagnostics).
    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    pub fn engine(&self) -> &BftEngine {
        &self.engine
    }

    /// Borrow the transport (for tests / diagnostics).
    #[must_use]
    pub const fn transport(&self) -> &T {
        &self.transport
    }

    /// Drive one iteration: drain inbox, vote / propose if appropriate.
    ///
    /// Returns the number of inbound messages consumed plus the number
    /// of outbound messages broadcast (useful for "did anything happen?"
    /// loops in tests).
    pub fn tick(&self) -> usize {
        let mut activity = 0;
        // 1. Drain inbox.
        while let Some(bytes) = self.transport.try_recv() {
            if let Ok(msg) = BftMessage::decode(&bytes) {
                let _ = self.dispatch_inbound(msg);
                activity += 1;
            }
        }

        // 2. Drive the round forward.
        if let Some((h, r, phase)) = self.engine.current_round_state() {
            activity += self.drive_phase(h, r, phase);
        } else if self.engine.would_be_leader_next_height() {
            // Bootstrap: no coordinator yet, but it's our turn to lead.
            // cast_proposal lazily creates a coordinator at
            // (head_number+1, round 0) and proposes against it.
            activity += self.bootstrap_propose();
        }

        activity
    }

    /// First-mover path: we're the leader and nobody has started a
    /// round yet. `cast_proposal` will instantiate a `RoundCoordinator`
    /// and emit a `Proposal` for `(head+1, 0)`.
    fn bootstrap_propose(&self) -> usize {
        let Ok((block, proof)) = self.engine.cast_proposal() else {
            return 0;
        };
        let (h, r, _) = self
            .engine
            .current_round_state()
            .expect("cast_proposal creates coordinator");
        let body_bytes = encode_block_body(&block.body);
        let msg = BftMessage::Proposal {
            height: h,
            round: r,
            block_hash: block.hash(),
            leader_proof: proof,
            coinbase: self.engine.coinbase(),
            body_bytes,
        };
        let bytes = msg.encode();
        {
            let mut f = self.flags.lock();
            f.proposed = Some((h, r));
            f.last_proposal = Some(bytes.clone());
        }
        self.transport.broadcast(bytes);
        1
    }

    /// Lookup-table per-phase action. Returns activity count (cast + retransmit).
    fn drive_phase(&self, h: u64, r: u32, phase: Phase) -> usize {
        let mut activity = 0;
        match phase {
            Phase::AwaitingProposal => {
                activity += self.maybe_propose(h, r);
                activity += self.retransmit_proposal();
            }
            Phase::Prevoting => {
                activity += self.maybe_prevote(h, r);
                // Help late peers catch up by re-emitting the proposal +
                // our prevote on each tick (idempotent at the receiver).
                activity += self.retransmit_proposal();
                activity += self.retransmit_prevote();
            }
            Phase::Precommitting => {
                activity += self.maybe_precommit(h, r);
                activity += self.retransmit_prevote();
                activity += self.retransmit_precommit();
            }
            Phase::Committed => {}
        }
        activity
    }

    fn maybe_propose(&self, h: u64, r: u32) -> usize {
        // Only the leader proposes, and only once per (h, r).
        if self.flags.lock().already_proposed(h, r) {
            return 0;
        }
        let Some(leader_idx) = self.engine.current_leader_index() else {
            return 0;
        };
        let my_idx = self.engine.my_index();
        if leader_idx != my_idx {
            return 0;
        }
        let Ok((block, proof)) = self.engine.cast_proposal() else {
            return 0;
        };
        let body_bytes = encode_block_body(&block.body);
        let msg = BftMessage::Proposal {
            height: h,
            round: r,
            block_hash: block.hash(),
            leader_proof: proof,
            coinbase: self.engine.coinbase(),
            body_bytes,
        };
        let bytes = msg.encode();
        {
            let mut f = self.flags.lock();
            f.proposed = Some((h, r));
            f.last_proposal = Some(bytes.clone());
        }
        self.transport.broadcast(bytes);
        1
    }

    fn maybe_prevote(&self, h: u64, r: u32) -> usize {
        if self.flags.lock().already_prevoted(h, r) {
            return 0;
        }
        let Ok(vote) = self.engine.cast_prevote() else {
            return 0;
        };
        let bytes = BftMessage::Prevote(vote).encode();
        {
            let mut f = self.flags.lock();
            f.prevoted = Some((h, r));
            f.last_prevote = Some(bytes.clone());
        }
        self.transport.broadcast(bytes);
        1
    }

    fn maybe_precommit(&self, h: u64, r: u32) -> usize {
        if self.flags.lock().already_precommitted(h, r) {
            return 0;
        }
        let Ok(vote) = self.engine.cast_precommit() else {
            return 0;
        };
        let bytes = BftMessage::Precommit(vote).encode();
        {
            let mut f = self.flags.lock();
            f.precommitted = Some((h, r));
            f.last_precommit = Some(bytes.clone());
        }
        self.transport.broadcast(bytes);
        1
    }

    fn retransmit_proposal(&self) -> usize {
        let bytes = self.flags.lock().last_proposal.clone();
        if let Some(b) = bytes {
            self.transport.broadcast(b);
            1
        } else {
            0
        }
    }

    fn retransmit_prevote(&self) -> usize {
        let bytes = self.flags.lock().last_prevote.clone();
        if let Some(b) = bytes {
            self.transport.broadcast(b);
            1
        } else {
            0
        }
    }

    fn retransmit_precommit(&self) -> usize {
        let bytes = self.flags.lock().last_precommit.clone();
        if let Some(b) = bytes {
            self.transport.broadcast(b);
            1
        } else {
            0
        }
    }

    fn dispatch_inbound(&self, msg: BftMessage) -> Result<(), BftError> {
        match msg {
            BftMessage::Proposal {
                height,
                round,
                block_hash,
                leader_proof,
                coinbase,
                body_bytes,
            } => self.handle_proposal_msg(
                height,
                round,
                block_hash,
                leader_proof,
                coinbase,
                &body_bytes,
            ),
            BftMessage::Prevote(v) => self.engine.submit_remote_prevote(v),
            BftMessage::Precommit(v) => self.engine.submit_remote_precommit(v),
        }
    }

    /// Reconstruct the proposed block from the engine's header view +
    /// the RLP-encoded body the leader sent on the wire, then submit
    /// it to the engine.
    ///
    /// Hash verification is end-to-end: the reconstructed block's hash
    /// (which folds in `transactions_root` derived from the body) must
    /// match the `block_hash` field of the proposal, or the proposal is
    /// rejected. This is what makes multi-validator BFT carry real txs
    /// safely — a peer cannot smuggle in a body that disagrees with the
    /// leader's stated hash.
    fn handle_proposal_msg(
        &self,
        height: u64,
        _round: u32,
        block_hash: aii_types::H256,
        leader_proof: LeaderProof,
        coinbase: aii_types::Address,
        body_bytes: &[u8],
    ) -> Result<(), BftError> {
        let body = decode_block_body(body_bytes)?;
        let block =
            self.engine
                .reconstruct_proposed_block_with_body(height, &leader_proof, coinbase, body);
        if block.hash() != block_hash {
            return Err(BftError::ProposalHashMismatch);
        }
        self.engine.submit_remote_proposal(block, leader_proof)
    }
}

/// Serialize a [`BlockBody`] to RLP bytes for inclusion in a
/// `BftMessage::Proposal`. The leader calls this when broadcasting; the
/// follower calls [`decode_block_body`] on receipt.
fn encode_block_body(body: &BlockBody) -> Vec<u8> {
    let mut buf = Vec::with_capacity(body.length());
    body.encode(&mut buf);
    buf
}

/// Inverse of [`encode_block_body`]. RLP-decode the body bytes received
/// from a peer. Maps an RLP error to [`BftError::InvalidProposalBody`]
/// so the gossip layer can reject malformed proposals without panicking.
fn decode_block_body(bytes: &[u8]) -> Result<BlockBody, BftError> {
    let mut slice = bytes;
    BlockBody::decode(&mut slice).map_err(|e| BftError::InvalidProposalBody(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bft::{Validator, ValidatorSet};
    use crate::engine::BftConfig;
    use aii_block::{Block, BlockBody, Bloom, Header, EMPTY_LIST_HASH, EMPTY_TRIE_HASH};
    use aii_crypto::bls::SecretKey as BlsSecretKey;
    use aii_crypto::vrf::SecretKey as VrfSecretKey;
    use aii_types::{Address, H256, U256};
    use std::collections::VecDeque;
    use std::sync::Arc;

    fn bls_sk(seed: u8) -> BlsSecretKey {
        BlsSecretKey::from_ikm(&[seed; 32], b"AII-BFT-GOSSIP-TEST").unwrap()
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

    /// Pair of in-memory mailboxes. `MemoryTransport::pair()` returns
    /// two transports A and B such that A.broadcast(x) shows up in
    /// B.try_recv() and vice versa.
    struct MemoryTransport {
        outbox: Arc<Mutex<VecDeque<Vec<u8>>>>, // bytes A wrote, B will read
        inbox: Arc<Mutex<VecDeque<Vec<u8>>>>,  // bytes B wrote, A will read
    }

    impl MemoryTransport {
        fn pair() -> (Self, Self) {
            let a_to_b = Arc::new(Mutex::new(VecDeque::new()));
            let b_to_a = Arc::new(Mutex::new(VecDeque::new()));
            (
                Self {
                    outbox: a_to_b.clone(),
                    inbox: b_to_a.clone(),
                },
                Self {
                    outbox: b_to_a,
                    inbox: a_to_b,
                },
            )
        }
    }

    impl BftTransport for MemoryTransport {
        fn broadcast(&self, bytes: Vec<u8>) {
            self.outbox.lock().push_back(bytes);
        }
        fn try_recv(&self) -> Option<Vec<u8>> {
            self.inbox.lock().pop_front()
        }
    }

    fn build_engine(
        idx: u32,
        vs: &ValidatorSet,
        keys: &[(BlsSecretKey, VrfSecretKey)],
        g: &Block,
    ) -> Arc<BftEngine> {
        let cfg = BftConfig {
            validator_set: vs.clone(),
            my_index: idx,
            my_bls_sk: keys[idx as usize].0.clone(),
            my_vrf_sk: keys[idx as usize].1.clone(),
            initial_seed: [0x55; 32],
            coinbase: Address::new([0xab; 20]),
            gas_limit: 30_000_000,
            base_fee_per_gas: U256::from(1_000_000_000u64),
            slot_seconds: 3,
        };
        Arc::new(BftEngine::new(cfg, g))
    }

    /// Build a synthetic EIP-1559 self-transfer tx — enough to be a
    /// real `Tx::Eip1559` variant that RLP-round-trips cleanly. Used
    /// purely to populate non-empty block bodies for the body-gossip
    /// integration test below.
    fn dummy_signed_tx(nonce: u64) -> aii_block::tx::Tx {
        use aii_block::tx::{Tx, TxEip1559};
        use aii_types::AlgoId;
        Tx::Eip1559(TxEip1559 {
            chain_id: 9999,
            nonce,
            max_priority_fee_per_gas: U256::from(1u64),
            max_fee_per_gas: U256::from(2u64),
            gas_limit: 21_000,
            to: Some(Address::new([0x42; 20])),
            value: U256::from(1u64),
            data: Vec::new(),
            access_list: Vec::new(),
            v: 0,
            r: H256::new([0u8; 32]),
            s: H256::new([0u8; 32]),
            algo_id: AlgoId::Secp256k1,
        })
    }

    #[test]
    fn two_node_gossip_finalises_block_carrying_txs() {
        // 2-validator BFT (quorum = 2 of 2). Leader stages two txs;
        // proposal carries the body; follower reconstructs the same
        // block hash and votes; both finalise one block with txs == 2.
        let mut keys = Vec::new();
        let mut vs_list = Vec::new();
        for i in 0..2u8 {
            let bls = bls_sk(i + 1);
            let vrf = vrf_sk();
            vs_list.push(Validator {
                bls_pubkey: bls.public_key(),
                vrf_pubkey: vrf.public_key(),
                stake: 100,
            });
            keys.push((bls, vrf));
        }
        let vs = ValidatorSet::new(vs_list).unwrap();
        let g = genesis();

        let e_a = build_engine(0, &vs, &keys, &g);
        let e_b = build_engine(1, &vs, &keys, &g);

        // Stage txs on BOTH nodes' pools — only the actual leader will
        // drain its pool when it casts the proposal; the other node's
        // pool stays untouched but its body comes from the wire.
        let txs = vec![dummy_signed_tx(0), dummy_signed_tx(1)];
        e_a.set_pending_txs(txs.clone());
        e_b.set_pending_txs(txs);

        let (t_a, t_b) = MemoryTransport::pair();
        let gossip_a = BftGossip::new(e_a.clone(), t_a);
        let gossip_b = BftGossip::new(e_b.clone(), t_b);

        let mut committed_a: Option<Block> = None;
        let mut committed_b: Option<Block> = None;
        for _ in 0..50 {
            gossip_a.tick();
            gossip_b.tick();
            if committed_a.is_none() {
                committed_a = e_a.try_harvest_committed();
            }
            if committed_b.is_none() {
                committed_b = e_b.try_harvest_committed();
            }
            if committed_a.is_some() && committed_b.is_some() {
                break;
            }
        }
        let a = committed_a.expect("node A should commit one block");
        let b = committed_b.expect("node B should commit one block");
        assert_eq!(a.header.number, 1);
        assert_eq!(b.header.number, 1);
        assert_eq!(a.hash(), b.hash(), "both nodes agree on block hash");
        assert_eq!(
            a.body.transactions.len(),
            2,
            "committed block carries leader's txs",
        );
        assert_eq!(
            b.body.transactions.len(),
            2,
            "follower's reconstructed block has the same txs",
        );
        // gas_used must reflect tx count (2 × PLACEHOLDER_TX_GAS).
        assert_eq!(a.header.gas_used, 2 * crate::PLACEHOLDER_TX_GAS);
        assert_eq!(b.header.gas_used, 2 * crate::PLACEHOLDER_TX_GAS);
    }

    #[test]
    fn two_node_gossip_finalises_one_block() {
        // 2 validators (so quorum = ceil(2 * 2 / 3) = 2 → both must agree).
        let mut keys = Vec::new();
        let mut vs_list = Vec::new();
        for i in 0..2u8 {
            let bls = bls_sk(i + 1);
            let vrf = vrf_sk();
            vs_list.push(Validator {
                bls_pubkey: bls.public_key(),
                vrf_pubkey: vrf.public_key(),
                stake: 100,
            });
            keys.push((bls, vrf));
        }
        let vs = ValidatorSet::new(vs_list).unwrap();
        let g = genesis();

        let e_a = build_engine(0, &vs, &keys, &g);
        let e_b = build_engine(1, &vs, &keys, &g);

        let (t_a, t_b) = MemoryTransport::pair();
        let gossip_a = BftGossip::new(e_a.clone(), t_a);
        let gossip_b = BftGossip::new(e_b.clone(), t_b);

        // Tick until both engines have a head > 0 or we exhaust patience.
        for _ in 0..50 {
            gossip_a.tick();
            gossip_b.tick();
            // Harvest committed blocks via the engine's &self helper
            // (the gossip layer doesn't auto-harvest; the host pulls
            // committed blocks on its own cadence).
            let _ = e_a.try_harvest_committed();
            let _ = e_b.try_harvest_committed();
            if e_a.head().1 == 1 && e_b.head().1 == 1 {
                break;
            }
        }
        assert_eq!(e_a.head().1, 1, "node A should finalise height 1");
        assert_eq!(e_b.head().1, 1, "node B should finalise height 1");
        assert_eq!(e_a.head().0, e_b.head().0, "both nodes agree on block hash");
    }
}
