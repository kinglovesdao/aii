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

use std::{collections::BTreeSet, sync::Arc};

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
    requested_blocks: BTreeSet<u64>,
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
    /// v0.0.73: blocks the engine committed during `tick()` that the
    /// host hasn't drained yet. The gossip auto-harvests after each
    /// inbound message so that the engine's `head_hash` advances
    /// inline with the inbox processing — otherwise a fast proposer's
    /// next-height proposal arrives at a follower whose head is still
    /// at N-1 and the reconstructed block hashes against the wrong
    /// parent.
    harvested_blocks: Mutex<Vec<aii_block::Block>>,
}

impl<T: BftTransport> BftGossip<T> {
    /// Construct a new gossip driver.
    pub fn new(engine: Arc<BftEngine>, transport: T) -> Self {
        Self {
            engine,
            transport,
            flags: Mutex::new(RoundFlags::default()),
            harvested_blocks: Mutex::new(Vec::new()),
        }
    }

    /// Take ownership of every block that the engine committed during
    /// recent `tick()` calls (v0.0.73). Hosts call this from their
    /// main loop to apply the blocks to their world-state storage.
    /// Returns the blocks in commit order; the internal buffer is
    /// emptied. Cheap when there's nothing to harvest.
    pub fn drain_harvested(&self) -> Vec<aii_block::Block> {
        std::mem::take(&mut *self.harvested_blocks.lock())
    }

    /// v0.0.73: harvest any block the engine has just committed and
    /// stash it on the gossip buffer for the host to drain. Called
    /// inside `tick()` after every inbound message so the engine's
    /// `head_hash` is always up-to-date when the next message lands.
    fn auto_harvest(&self) {
        while let Some(block) = self.engine.try_harvest_committed() {
            self.harvested_blocks.lock().push(block);
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
        // 1. Drain inbox — auto-harvest between messages so the
        // engine's head_hash advances in lockstep with the inbox.
        // Without this, a fast proposer's next-height proposal lands
        // before the receiver has committed the previous block and
        // the reconstructed parent_hash diverges → ProposalHashMismatch
        // (v0.0.73).
        let mut inbox_drained = 0;
        while let Some(bytes) = self.transport.try_recv() {
            inbox_drained += 1;
            if let Ok(msg) = BftMessage::decode(&bytes) {
                let _ = self.dispatch_inbound(msg);
                activity += 1;
            }
            self.auto_harvest();
        }
        if inbox_drained > 0 {
            tracing::debug!(count = inbox_drained, "gossip.tick: drained inbox");
        }

        // 2. Drive the round forward.
        let round_state = self.engine.current_round_state();
        let would_lead = self.engine.would_be_leader_next_height();
        tracing::debug!(
            round_state = ?round_state,
            would_lead_next_height = would_lead,
            "gossip.tick: state"
        );
        if let Some((h, r, phase)) = round_state {
            activity += self.drive_phase(h, r, phase);
        } else if would_lead {
            // Bootstrap: no coordinator yet, but it's our turn to lead.
            // cast_proposal lazily creates a coordinator at
            // (head_number+1, round 0) and proposes against it.
            tracing::debug!("gossip.tick: bootstrap_propose (I'd lead next height)");
            activity += self.bootstrap_propose();
        }
        // Final harvest sweep in case the drive_phase above pushed us
        // to Committed (rare, but possible if our own precommit was
        // the quorum-forming vote and didn't traverse the inbox).
        self.auto_harvest();

        activity
    }

    /// First-mover path: we're the leader and nobody has started a
    /// round yet. `cast_proposal` will instantiate a `RoundCoordinator`
    /// and emit a `Proposal` for `(head+1, 0)`.
    fn bootstrap_propose(&self) -> usize {
        let result = self.engine.cast_proposal();
        let Ok((block, proof)) = result else {
            tracing::warn!(
                err = ?result.err(),
                "bootstrap_propose: cast_proposal failed"
            );
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
        tracing::info!(
            height = h,
            round = r,
            block_hash = ?block.hash(),
            wire_bytes = bytes.len(),
            "bootstrap_propose: broadcasting Proposal"
        );
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
            tracing::debug!(h, r, "maybe_propose: no leader_index");
            return 0;
        };
        let my_idx = self.engine.my_index();
        tracing::debug!(h, r, leader_idx, my_idx, "maybe_propose: leader check");
        if leader_idx != my_idx {
            return 0;
        }
        let cp = self.engine.cast_proposal();
        let Ok((block, proof)) = cp else {
            tracing::warn!(h, r, err = ?cp.err(), "maybe_propose: cast_proposal failed");
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
        let cv = self.engine.cast_prevote();
        let Ok(vote) = cv else {
            tracing::warn!(h, r, err = ?cv.err(), "maybe_prevote: cast_prevote failed");
            return 0;
        };
        tracing::info!(h, r, "maybe_prevote: broadcasting Prevote");
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
            tracing::debug!(h, r, "maybe_precommit: already precommitted");
            return 0;
        }
        let cp = self.engine.cast_precommit();
        let Ok(vote) = cp else {
            tracing::warn!(h, r, err = ?cp.err(), "maybe_precommit: cast_precommit failed");
            return 0;
        };
        let bytes = BftMessage::Precommit(vote).encode();
        tracing::info!(
            h,
            r,
            wire_bytes = bytes.len(),
            "maybe_precommit: broadcasting Precommit"
        );
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
            } => {
                tracing::info!(
                    height,
                    round,
                    block_hash = ?block_hash,
                    body_len = body_bytes.len(),
                    "dispatch_inbound: Proposal"
                );
                if self.request_catchup_if_future_height(height) {
                    return Ok(());
                }
                let r = self.handle_proposal_msg(
                    height,
                    round,
                    block_hash,
                    leader_proof,
                    coinbase,
                    &body_bytes,
                );
                if let Err(ref e) = r {
                    tracing::warn!(?e, "handle_proposal_msg returned err");
                }
                r
            }
            BftMessage::Prevote(v) => {
                tracing::debug!(?v, "dispatch_inbound: Prevote");
                if self.request_catchup_if_future_height(v.height) {
                    return Ok(());
                }
                let r = self.engine.submit_remote_prevote(v);
                if let Err(ref e) = r {
                    tracing::warn!(?e, "submit_remote_prevote returned err");
                }
                r
            }
            BftMessage::Precommit(v) => {
                tracing::debug!(?v, "dispatch_inbound: Precommit");
                if self.request_catchup_if_future_height(v.height) {
                    return Ok(());
                }
                let r = self.engine.submit_remote_precommit(v);
                if let Err(ref e) = r {
                    tracing::warn!(?e, "submit_remote_precommit returned err");
                }
                r
            }
            BftMessage::BlockRequest { height } => {
                tracing::debug!(height, "dispatch_inbound: BlockRequest");
                if let Some((block, certificate)) = self.engine.committed_block_at(height) {
                    let mut block_bytes = Vec::new();
                    block.encode(&mut block_bytes);
                    let bytes = BftMessage::BlockResponse {
                        block_bytes,
                        certificate,
                    }
                    .encode();
                    self.transport.broadcast(bytes);
                }
                Ok(())
            }
            BftMessage::BlockResponse {
                block_bytes,
                certificate,
            } => {
                tracing::debug!(bytes = block_bytes.len(), "dispatch_inbound: BlockResponse");
                self.handle_block_response(&block_bytes, certificate)
            }
        }
    }

    fn request_catchup_if_future_height(&self, remote_height: u64) -> bool {
        let next_height = self.engine.head_number().saturating_add(1);
        if remote_height <= next_height {
            return false;
        }
        let mut flags = self.flags.lock();
        if !flags.requested_blocks.insert(next_height) {
            return true;
        }
        drop(flags);
        tracing::info!(
            remote_height,
            requested_height = next_height,
            "gossip observed future BFT height; requesting missing block"
        );
        self.transport.broadcast(
            BftMessage::BlockRequest {
                height: next_height,
            }
            .encode(),
        );
        true
    }

    fn handle_block_response(
        &self,
        block_bytes: &[u8],
        certificate: crate::bft::PrecommitCertificate,
    ) -> Result<(), BftError> {
        let mut slice = block_bytes;
        let block = aii_block::Block::decode(&mut slice)
            .map_err(|e| BftError::InvalidProposalBody(e.to_string()))?;
        if !slice.is_empty() {
            return Err(BftError::InvalidProposalBody(
                "trailing bytes after synced block".to_string(),
            ));
        }
        let adopted = self.engine.adopt_synced_block(block, certificate)?;
        self.flags
            .lock()
            .requested_blocks
            .remove(&adopted.header.number);
        self.harvested_blocks.lock().push(adopted);
        Ok(())
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
        round: u32,
        block_hash: aii_types::H256,
        leader_proof: LeaderProof,
        coinbase: aii_types::Address,
        body_bytes: &[u8],
    ) -> Result<(), BftError> {
        if height == self.engine.head_number().saturating_add(1) {
            let should_fast_forward = self.engine.current_round_state().is_none_or(
                |(active_height, active_round, _phase)| {
                    active_height == height && round > active_round
                },
            );
            if should_fast_forward && round > 0 {
                self.engine.fast_forward_to_round(round)?;
            }
        }
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
            executor: None,
        };
        Arc::new(BftEngine::new(cfg, g))
    }

    fn single_validator_fixture(seed: u8) -> (ValidatorSet, Vec<(BlsSecretKey, VrfSecretKey)>) {
        let bls = bls_sk(seed);
        let vrf = vrf_sk();
        let vs = ValidatorSet::new(vec![Validator {
            bls_pubkey: bls.public_key(),
            vrf_pubkey: vrf.public_key(),
            stake: 100,
        }])
        .unwrap();
        (vs, vec![(bls, vrf)])
    }

    fn commit_one_block(engine: Arc<BftEngine>) -> Block {
        let (transport, _peer) = MemoryTransport::pair();
        let gossip = BftGossip::new(engine, transport);
        for _ in 0..10 {
            gossip.tick();
            if let Some(block) = gossip.drain_harvested().into_iter().next() {
                return block;
            }
        }
        panic!("single-validator source should commit one block");
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
    fn single_validator_gossip_finalises_without_peers() {
        let bls = bls_sk(1);
        let vrf = vrf_sk();
        let vs = ValidatorSet::new(vec![Validator {
            bls_pubkey: bls.public_key(),
            vrf_pubkey: vrf.public_key(),
            stake: 100,
        }])
        .unwrap();
        let g = genesis();
        let e = build_engine(0, &vs, &[(bls, vrf)], &g);
        e.set_pending_txs(vec![dummy_signed_tx(0)]);
        let (t, _peer) = MemoryTransport::pair();
        let gossip = BftGossip::new(e.clone(), t);

        let mut committed: Option<Block> = None;
        for _ in 0..10 {
            gossip.tick();
            if committed.is_none() {
                committed = gossip
                    .drain_harvested()
                    .into_iter()
                    .next()
                    .or_else(|| e.try_harvest_committed());
            }
            if committed.is_some() {
                break;
            }
        }

        let block = committed.expect("single validator gossip should self-finalise");
        assert_eq!(block.header.number, 1);
        assert_eq!(block.body.transactions.len(), 1);
        assert_eq!(e.head().1, 1);
    }

    #[test]
    fn block_request_response_syncs_certified_missing_block() {
        let (vs, keys) = single_validator_fixture(9);
        let g = genesis();
        let source_engine = build_engine(0, &vs, &keys, &g);
        let target_engine = build_engine(0, &vs, &keys, &g);
        let committed = commit_one_block(source_engine.clone());
        assert_eq!(committed.header.number, 1);
        assert!(source_engine.committed_block_at(1).is_some());

        let (source_transport, target_transport) = MemoryTransport::pair();
        let source_gossip = BftGossip::new(source_engine, source_transport);
        let target_gossip = BftGossip::new(target_engine.clone(), target_transport);

        target_gossip
            .transport()
            .broadcast(BftMessage::BlockRequest { height: 1 }.encode());
        source_gossip.tick();
        target_gossip.tick();

        assert_eq!(target_engine.head().1, 1);
        assert_eq!(target_engine.head().0, committed.hash());
        let synced = target_gossip.drain_harvested();
        assert_eq!(synced.len(), 1);
        assert_eq!(synced[0].hash(), committed.hash());
    }

    #[test]
    fn block_response_with_bad_certificate_is_rejected() {
        let (vs, keys) = single_validator_fixture(10);
        let g = genesis();
        let source_engine = build_engine(0, &vs, &keys, &g);
        let target_engine = build_engine(0, &vs, &keys, &g);
        let committed = commit_one_block(source_engine.clone());
        let mut block_bytes = Vec::new();
        committed.encode(&mut block_bytes);
        let (_, mut certificate) = source_engine
            .committed_block_at(1)
            .expect("source should cache committed certificate");
        certificate.block_hash = H256::new([0x99; 32]);

        let (source_transport, target_transport) = MemoryTransport::pair();
        let source_gossip = BftGossip::new(source_engine, source_transport);
        let target_gossip = BftGossip::new(target_engine.clone(), target_transport);
        source_gossip.transport().broadcast(
            BftMessage::BlockResponse {
                block_bytes,
                certificate,
            }
            .encode(),
        );
        target_gossip.tick();

        assert_eq!(target_engine.head().1, 0);
        assert!(target_gossip.drain_harvested().is_empty());
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
            // v0.0.73: gossip auto-harvests; the host drains via the
            // gossip's buffer. Fall back to direct engine harvest in
            // case the path ran twice (idempotent in v0.0.73).
            if committed_a.is_none() {
                committed_a = gossip_a
                    .drain_harvested()
                    .into_iter()
                    .next()
                    .or_else(|| e_a.try_harvest_committed());
            }
            if committed_b.is_none() {
                committed_b = gossip_b
                    .drain_harvested()
                    .into_iter()
                    .next()
                    .or_else(|| e_b.try_harvest_committed());
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
        // v0.0.73: gossip auto-harvests; engine head advances inside
        // tick() so we no longer need a separate try_harvest_committed
        // call here.
        for _ in 0..50 {
            gossip_a.tick();
            gossip_b.tick();
            let _ = gossip_a.drain_harvested();
            let _ = gossip_b.drain_harvested();
            if e_a.head().1 == 1 && e_b.head().1 == 1 {
                break;
            }
        }
        assert_eq!(e_a.head().1, 1, "node A should finalise height 1");
        assert_eq!(e_b.head().1, 1, "node B should finalise height 1");
        assert_eq!(e_a.head().0, e_b.head().0, "both nodes agree on block hash");
    }
}
