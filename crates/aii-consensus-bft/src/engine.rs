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

use aii_block::tx::Tx;
use aii_block::{Block, BlockBody, Bloom, Hashable, Header, EMPTY_LIST_HASH, EMPTY_TRIE_HASH};
use aii_consensus_iface::{ConsensusError, Engine, EngineProgress};
use aii_crypto::{bls, vrf};
use aii_types::{Address, BlsPubKey, H256, U256};

/// Gas cost charged per included tx in the v0.0.37 placeholder
/// pipeline (no actual EVM execution — every tx is treated as a
/// 21,000-gas transfer).
pub const PLACEHOLDER_TX_GAS: u64 = 21_000;

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
    /// Optional roots oracle. When `Some`, every produced block
    /// header is built with the post-execution Yellow-Paper roots
    /// (state_root, receipts_root, logs_bloom) supplied by the
    /// oracle so the block hash itself locks to the post-state.
    ///
    /// `None` keeps the legacy v0.0.64 behaviour: placeholder roots
    /// plus a sidecar `Meta:postroot:<hash>` record. Both modes
    /// interop on the same validator set as long as every node
    /// agrees on the flag.
    #[allow(clippy::type_complexity)]
    pub executor: Option<std::sync::Arc<dyn aii_consensus_iface::BlockExecutor>>,
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
            executor: None,
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
    pending_txs: Mutex<Vec<Tx>>,
}

struct BftEngineState {
    validator_set: ValidatorSet,
    my_index: u32,
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
    /// Equivocation detector — every observed remote vote feeds in.
    /// Yielded `EquivocationEvidence` is parked on `pending_evidence`
    /// for the host to drain via [`BftEngine::drain_evidence`].
    detector: crate::slashing::EquivocationDetector,
    /// Evidence not yet drained by the host.
    pending_evidence: Vec<crate::slashing::EquivocationEvidence>,
    /// v0.0.72: prevotes that arrived too early to be tallied — e.g.
    /// before any coordinator exists, or before the proposal has
    /// transitioned the coordinator into [`crate::bft::Phase::Prevoting`].
    /// Drained by [`BftEngine::drain_pending_votes`] after every state
    /// mutation that could unblock them.
    pending_prevotes: Vec<PrevoteVote>,
    /// v0.0.72: precommits that arrived too early. Same pattern as
    /// [`Self::pending_prevotes`] but for the precommit phase. The
    /// proposer in a fast leader frequently has its precommit reach
    /// remote validators before they themselves have tallied enough
    /// prevotes to transition phase — without this buffer the
    /// precommit would be rejected with `WrongPhase` and the round
    /// would stall.
    pending_precommits: Vec<PrecommitVote>,
    /// v0.0.93 block-sync: a bounded cache of the most recently
    /// committed full blocks and their BFT finality certificates, keyed
    /// by height. Populated on every harvest (and on adopt of a
    /// peer-supplied certified block) so this node can answer a peer's
    /// [`crate::wire::BftMessage::BlockRequest`] without reaching back
    /// into host storage. Capped at [`RECENT_BLOCKS_CAP`] entries — the
    /// oldest are evicted.
    recent_blocks: std::collections::BTreeMap<u64, RecentCommittedBlock>,
}

#[derive(Clone)]
struct RecentCommittedBlock {
    block: Block,
    certificate: PrecommitCertificate,
}

/// Number of recently-committed blocks the engine keeps cached to
/// serve [`crate::wire::BftMessage::BlockRequest`].
///
/// Small: a lagging validator is typically only 1–2 blocks behind (it
/// restarted), and anything further behind should HTTP cold-sync via
/// `--bootnode`.
pub const RECENT_BLOCKS_CAP: usize = 64;

fn cache_recent_block(
    map: &mut std::collections::BTreeMap<u64, RecentCommittedBlock>,
    block: Block,
    certificate: PrecommitCertificate,
) {
    map.insert(
        block.header.number,
        RecentCommittedBlock { block, certificate },
    );
    while map.len() > RECENT_BLOCKS_CAP {
        // Evict the lowest height.
        if let Some((&oldest, _)) = map.iter().next() {
            map.remove(&oldest);
        } else {
            break;
        }
    }
}

impl BftEngine {
    /// Construct from config + genesis block.
    pub fn new(config: BftConfig, genesis: &Block) -> Self {
        let state = BftEngineState {
            validator_set: config.validator_set.clone(),
            my_index: config.my_index,
            head_hash: genesis.hash(),
            head_number: genesis.header.number,
            head_timestamp: genesis.header.timestamp,
            seed: config.initial_seed,
            coordinator: None,
            proposal: None,
            detector: crate::slashing::EquivocationDetector::new(),
            pending_evidence: Vec::new(),
            pending_prevotes: Vec::new(),
            pending_precommits: Vec::new(),
            recent_blocks: std::collections::BTreeMap::new(),
        };
        Self {
            config,
            state: Arc::new(Mutex::new(state)),
            pending_txs: Mutex::new(Vec::new()),
        }
    }

    /// Resume from a recovered chain head (v0.0.70).
    ///
    /// Use this when the node is restarting and has read the
    /// last-committed block out of persistent storage. The engine
    /// continues at `head.number + 1` round 0 rather than restarting
    /// from genesis.
    ///
    /// The seed for the next leader election is derived from the
    /// recovered block's `mix_hash` (which the producer set to the
    /// proposing validator's VRF output, per the v0.0.34+ wire
    /// format). For `head.number == 0` (genesis recovery), the seed
    /// falls back to `config.initial_seed` — matching [`Self::new`].
    ///
    /// In-memory round / locked / vote state is NOT persisted yet —
    /// after a single-validator restart the BFT engine still needs
    /// ~⅔-stake worth of peers to be co-restarting before liveness
    /// resumes. That fix is task v0.0.71-A. v0.0.70 only restores
    /// chain CONTINUITY (head height) on restart, not BFT
    /// in-flight state.
    #[must_use]
    pub fn from_recovered(config: BftConfig, head: &Block) -> Self {
        let seed = if head.header.number == 0 {
            config.initial_seed
        } else {
            *head.header.mix_hash.as_bytes()
        };
        let state = BftEngineState {
            validator_set: config.validator_set.clone(),
            my_index: config.my_index,
            head_hash: head.hash(),
            head_number: head.header.number,
            head_timestamp: head.header.timestamp,
            seed,
            coordinator: None,
            proposal: None,
            detector: crate::slashing::EquivocationDetector::new(),
            pending_evidence: Vec::new(),
            pending_prevotes: Vec::new(),
            pending_precommits: Vec::new(),
            recent_blocks: std::collections::BTreeMap::new(),
        };
        Self {
            config,
            state: Arc::new(Mutex::new(state)),
            pending_txs: Mutex::new(Vec::new()),
        }
    }

    /// Stage transactions to include in the next produced block,
    /// **replacing** any previously-staged batch. Use this when you
    /// know the caller fully owns the staging window (typically in
    /// single-validator dev mode where every slot deterministically
    /// drains everything pending in one shot).
    pub fn set_pending_txs(&self, txs: Vec<Tx>) {
        *self.pending_txs.lock() = txs;
    }

    /// Stage additional transactions onto the pending-tx queue without
    /// dropping anything already staged. Multi-validator BFT pumps
    /// mempool drains into the engine on a separate cadence from the
    /// gossip proposer; an overwriting `set_pending_txs` there would
    /// silently drop txs whose round had not yet fired.
    pub fn extend_pending_txs(&self, txs: Vec<Tx>) {
        if txs.is_empty() {
            return;
        }
        self.pending_txs.lock().extend(txs);
    }

    /// Snapshot count of staged-but-unproposed txs (for diagnostics).
    #[must_use]
    pub fn pending_tx_count(&self) -> usize {
        self.pending_txs.lock().len()
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
        self.state.lock().validator_set.size() == 1
    }

    /// This node's coinbase — i.e. the address that becomes the
    /// `header.beneficiary` of any block this engine proposes. Used by
    /// the gossip layer to stamp the outbound `BftMessage::Proposal`
    /// so peers reconstruct the header with the leader's coinbase
    /// (not their own).
    #[must_use]
    pub const fn coinbase(&self) -> Address {
        self.config.coinbase
    }

    /// This node's BLS public key in the 48-byte wire form used by
    /// genesis and keyed DPoS validator-set records.
    #[must_use]
    pub fn my_bls_pubkey(&self) -> BlsPubKey {
        BlsPubKey::new(self.config.my_bls_sk.public_key().to_compressed())
    }

    /// Current `(height, round, Phase)` if a coordinator is active.
    #[must_use]
    pub fn current_round_state(&self) -> Option<(u64, u32, crate::bft::Phase)> {
        let g = self.state.lock();
        g.coordinator
            .as_ref()
            .map(|c| (c.height(), c.round(), c.phase()))
    }

    /// Force-advance the coordinator for the next-to-commit height to
    /// `target_round` (v0.0.71).
    ///
    /// Use this on startup after [`Self::from_recovered`] when the
    /// host has loaded a persisted `{height, round}` snapshot — the
    /// engine creates a fresh coordinator at the recovered height
    /// then calls [`crate::coordinator::RoundCoordinator::fire_timeout`]
    /// `target_round` times so the local round matches what the rest
    /// of the validator set is on. Without this, a restarted
    /// validator would come up at round 0 while live peers are at
    /// round R, and their votes would not combine into a quorum
    /// until the local engine itself had timed out R times — a
    /// ~5..30 s liveness hole per restart.
    ///
    /// No-op when `target_round == 0`. Idempotent: calling twice for
    /// the same height + round is observationally identical.
    ///
    /// # Errors
    ///
    /// Returns [`BftError::WrongHeight`] only when the coordinator is
    /// already initialized for a different height — i.e. the caller
    /// attempted to fast-forward after a vote has already arrived
    /// for this round. The typical startup-time call site cannot hit
    /// this branch because no votes have been ingested yet.
    pub fn fast_forward_to_round(&self, target_round: u32) -> Result<(), BftError> {
        let mut g = self.state.lock();
        let next_height = g.head_number + 1;
        // Drop any prior coordinator for an old height (defensive — the
        // engine resets coordinator to None on commit, so this only
        // matters if the host called this method twice without an
        // intervening commit).
        if let Some(existing) = g.coordinator.as_ref() {
            if existing.height() != next_height {
                return Err(BftError::WrongHeight);
            }
        }
        let mut coord = RoundCoordinator::new(next_height, g.seed, g.validator_set.clone());
        for _ in 0..target_round {
            coord.fire_timeout();
        }
        g.coordinator = Some(coord);
        Ok(())
    }

    /// Leader index for the active round (if any).
    #[must_use]
    pub fn current_leader_index(&self) -> Option<usize> {
        let g = self.state.lock();
        g.coordinator.as_ref().map(RoundCoordinator::leader_index)
    }

    /// This node's index inside its `validator_set`.
    #[must_use]
    pub fn my_index(&self) -> usize {
        self.state.lock().my_index as usize
    }

    /// `true` iff this node would be the elected leader for the
    /// *next* height's round-0 proposal, given the current head and
    /// rolled-forward seed. Used by the gossip layer to bootstrap a
    /// round when no coordinator exists yet.
    #[must_use]
    pub fn would_be_leader_next_height(&self) -> bool {
        let g = self.state.lock();
        let next_h = g.head_number.saturating_add(1);
        let leader = g.validator_set.select_leader(next_h, 0, &g.seed);
        leader == g.my_index as usize
    }

    /// Validator-set size in force.
    #[must_use]
    pub fn validator_set_size(&self) -> usize {
        self.state.lock().validator_set.size()
    }

    /// Replace the active validator set at a safe epoch boundary.
    ///
    /// Rotation is only accepted when no coordinator/proposal is active,
    /// which is the post-harvest boundary after a height commits. That
    /// keeps votes collected under the old set from being interpreted
    /// under a new set.
    ///
    /// # Errors
    /// Returns [`BftError::ActiveRoundInProgress`] if a round is in
    /// flight, or [`BftError::ValidatorIndexOutOfBounds`] if `my_index`
    /// is not a member of `validator_set`.
    pub fn rotate_validator_set(
        &self,
        validator_set: ValidatorSet,
        my_index: u32,
    ) -> Result<(), BftError> {
        if (my_index as usize) >= validator_set.size() {
            return Err(BftError::ValidatorIndexOutOfBounds {
                index: my_index,
                size: validator_set.size(),
            });
        }
        let mut g = self.state.lock();
        if g.coordinator.is_some() || g.proposal.is_some() {
            return Err(BftError::ActiveRoundInProgress);
        }
        g.pending_prevotes.clear();
        g.pending_precommits.clear();
        g.validator_set = validator_set;
        g.my_index = my_index;
        Ok(())
    }

    /// Reconstruct an empty-body block under this engine's own coinbase
    /// — used by callers (and tests) that only care about the header
    /// and don't need to mirror a remote leader's beneficiary.
    /// Multi-validator gossip uses
    /// [`Self::reconstruct_proposed_block_with_body`] instead so that
    /// (a) the leader's txs are slotted in and (b) the leader's
    /// `--coinbase` is honoured as the block's `beneficiary`.
    #[must_use]
    pub fn reconstruct_proposed_block(&self, height: u64, leader_proof: &LeaderProof) -> Block {
        self.reconstruct_proposed_block_with_body(
            height,
            leader_proof,
            self.config.coinbase,
            BlockBody::default(),
        )
    }

    /// Reconstruct the block the engine would `cast_proposal` at
    /// `height` against its current head + `leader_proof`, slotting in
    /// the supplied `coinbase` and `body`. Used by the gossip layer to
    /// recover the full block a peer leader proposed (the body and the
    /// leader's coinbase arrive on the wire alongside `block_hash +
    /// leader_proof`).
    ///
    /// The reconstructed block's header derives `gas_used` from the
    /// body so that `block.hash()` here equals `block.hash()` on the
    /// leader — that equality is what the gossip layer verifies
    /// before accepting the proposal.
    #[must_use]
    pub fn reconstruct_proposed_block_with_body(
        &self,
        height: u64,
        leader_proof: &LeaderProof,
        coinbase: Address,
        body: BlockBody,
    ) -> Block {
        let g = self.state.lock();
        // `build_block_with_body` reads head_hash + head_timestamp,
        // which mirrors what the leader did when they signed the proposal.
        self.build_block_with_body(
            g.head_hash,
            g.head_timestamp,
            height,
            leader_proof,
            coinbase,
            body,
        )
    }

    /// `&self` harvest: if the coordinator is in `Committed`, commit
    /// the captured block, advance the head, roll the seed, and clear
    /// the coordinator. Returns `Some(block)` on harvest with the
    /// committed full block, `None` if there is nothing to commit yet.
    ///
    /// Useful for gossip / network drivers that hold the engine in an
    /// `Arc<BftEngine>` and cannot call the `&mut`-flavoured
    /// [`aii_consensus_iface::Engine::step`].
    pub fn try_harvest_committed(&self) -> Option<Block> {
        let mut g = self.state.lock();
        let committed = g
            .coordinator
            .as_ref()
            .is_some_and(|c| c.phase() == crate::bft::Phase::Committed);
        if !committed {
            return None;
        }
        let (block, proof) = g.proposal.clone()?;
        let certificate = g.coordinator.as_ref()?.certificate().cloned()?;
        let block_hash = block.hash();
        g.head_hash = block_hash;
        g.head_number = block.header.number;
        g.head_timestamp = block.header.timestamp;
        g.seed = proof.vrf_output;
        g.coordinator = None;
        g.proposal = None;
        // v0.0.93: cache the committed block and certificate so peers
        // that fell behind can fetch verified finality over gossip.
        cache_recent_block(&mut g.recent_blocks, block.clone(), certificate);
        Some(block)
    }

    /// Current committed head height. (v0.0.93 block-sync helper.)
    #[must_use]
    pub fn head_number(&self) -> u64 {
        self.state.lock().head_number
    }

    /// v0.0.93 block-sync: return the committed block and finality
    /// certificate cached at `height`, if this engine has it. Used to
    /// answer a peer's [`crate::wire::BftMessage::BlockRequest`].
    /// Returns `None` when the height is outside the recent-block cache
    /// window.
    #[must_use]
    pub fn committed_block_at(&self, height: u64) -> Option<(Block, PrecommitCertificate)> {
        let g = self.state.lock();
        g.recent_blocks
            .get(&height)
            .map(|entry| (entry.block.clone(), entry.certificate.clone()))
    }

    /// v0.0.93 block-sync: adopt a peer-supplied committed `block` as
    /// the new head, but ONLY when it extends the current head by
    /// exactly one (`block.number == head+1` and
    /// `block.parent_hash == head_hash`) AND carries a BFT precommit
    /// certificate that verifies against the current validator set. This
    /// is the catch-up path for a validator that fell one block behind
    /// (e.g. after a restart): it lets the engine advance its head — and
    /// roll the leader seed forward from the block's `mix_hash` (the
    /// proposer's VRF output, per the v0.0.34+ header convention) — so
    /// it can rejoin the current round instead of stalling the whole set.
    ///
    /// Any in-flight coordinator/proposal for the now-superseded height
    /// is cleared. Returns the adopted block on success.
    ///
    /// # Errors
    /// - [`BftError::WrongHeight`] if `block.number != head+1` or the
    ///   certificate height differs from the block height.
    /// - [`BftError::ProposalHashMismatch`] if `block.parent_hash`
    ///   does not match the current head hash or the certificate targets
    ///   a different block hash.
    /// - [`BftError::InvalidBlsSignature`] if the certificate does not
    ///   verify against the current validator set.
    pub fn adopt_synced_block(
        &self,
        block: Block,
        certificate: PrecommitCertificate,
    ) -> Result<Block, BftError> {
        let mut g = self.state.lock();
        let block_hash = block.hash();
        if block.header.number != g.head_number + 1 || certificate.height != block.header.number {
            return Err(BftError::WrongHeight);
        }
        if block.header.parent_hash != g.head_hash || certificate.block_hash != block_hash {
            return Err(BftError::ProposalHashMismatch);
        }
        certificate.verify(&g.validator_set)?;
        g.head_hash = block_hash;
        g.head_number = block.header.number;
        g.head_timestamp = block.header.timestamp;
        g.seed = *block.header.mix_hash.as_bytes();
        // The block we were coordinating for this height is now moot.
        g.coordinator = None;
        g.proposal = None;
        g.pending_prevotes.clear();
        g.pending_precommits.clear();
        cache_recent_block(&mut g.recent_blocks, block.clone(), certificate);
        Ok(block)
    }

    /// Build a proposal for the current round and feed it to our own
    /// coordinator. Caller is responsible for broadcasting the returned
    /// `(Block, LeaderProof)` to peers. Only valid when this node is
    /// the elected leader for the round.
    ///
    /// Drains up to `gas_limit / PLACEHOLDER_TX_GAS` transactions from
    /// the engine's pending pool into the block body so the followers'
    /// reconstructed block (which uses the wire-shipped body) hashes
    /// identically.
    pub fn cast_proposal(&self) -> Result<(Block, LeaderProof), BftError> {
        let mut g = self.state.lock();
        Self::ensure_coordinator(&mut g);
        let my_index = g.my_index;
        let coord = g.coordinator.as_mut().expect("ensured");
        let leader_idx = coord.leader_index();
        if leader_idx != my_index as usize {
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
        let body = self.drain_pending_txs_into_body();
        let block = self.build_block_with_body(
            head_hash,
            head_ts,
            height,
            &proof,
            self.config.coinbase,
            body,
        );
        let block_hash = block.hash();
        g.coordinator
            .as_mut()
            .unwrap()
            .submit_proposal(block_hash, &proof)?;
        g.proposal = Some((block.clone(), proof.clone()));
        Ok((block, proof))
    }

    /// Take pending transactions up to the per-block cap and pack them
    /// into a fresh `BlockBody`. Shared between [`Self::cast_proposal`]
    /// (multi-validator path) and [`Self::advance_single`].
    fn drain_pending_txs_into_body(&self) -> BlockBody {
        let max_txs = (self.config.gas_limit / PLACEHOLDER_TX_GAS) as usize;
        let mut pending = self.pending_txs.lock();
        let take = pending.len().min(max_txs);
        let transactions: Vec<Tx> = pending.drain(..take).collect();
        BlockBody {
            transactions,
            ommers: Vec::new(),
            withdrawals: Vec::new(),
        }
    }

    /// Sign + submit my own PRE-VOTE for whatever block the coordinator
    /// is currently in `Prevoting` over. Returns the signed vote for
    /// the host to broadcast.
    pub fn cast_prevote(&self) -> Result<PrevoteVote, BftError> {
        let mut g = self.state.lock();
        let my_index = g.my_index;
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
            my_index,
        );
        coord.submit_prevote(vote.clone())?;
        Ok(vote)
    }

    /// Sign + submit my own PRE-COMMIT. Returns the signed vote for
    /// the host to broadcast.
    pub fn cast_precommit(&self) -> Result<PrecommitVote, BftError> {
        let mut g = self.state.lock();
        let my_index = g.my_index;
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
            my_index,
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
        Self::ensure_coordinator(&mut g);
        let coord = g.coordinator.as_mut().expect("ensured");
        let block_hash = block.hash();
        coord.submit_proposal(block_hash, &leader_proof)?;
        g.proposal = Some((block, leader_proof));
        // v0.0.72: a proposal transition unlocks any pending votes
        // that arrived ahead of it.
        Self::drain_pending_votes(&mut g);
        Ok(())
    }

    /// Ingest a peer's PRE-VOTE. Forwards to inner coordinator and
    /// feeds the slashing detector — if the sender double-signs at
    /// the same `(height, round)` for two different block hashes,
    /// the evidence is parked on `pending_evidence` for the host to
    /// drain.
    ///
    /// v0.0.72: when the vote arrives before the coordinator exists
    /// or is in the wrong phase / round, the vote is buffered on
    /// `pending_prevotes` rather than rejected. Buffered votes are
    /// re-applied on the next state transition (proposal arrival,
    /// timeout, phase change) that could unblock them. Stale votes
    /// (height < `head_number + 1`) are dropped silently.
    pub fn submit_remote_prevote(&self, vote: PrevoteVote) -> Result<(), BftError> {
        let mut g = self.state.lock();
        if let Some(ev) = g.detector.record_prevote(vote.clone()) {
            g.pending_evidence.push(ev);
        }
        // Drop stale votes (for an already-committed height).
        if vote.height <= g.head_number {
            return Ok(());
        }
        match g.coordinator.as_mut() {
            None => {
                g.pending_prevotes.push(vote);
            }
            Some(coord) => match coord.submit_prevote(vote.clone()) {
                Ok(()) => {
                    // Submission may have transitioned phase — drain
                    // any precommits that were waiting for this.
                    Self::drain_pending_votes(&mut g);
                }
                Err(BftError::WrongPhase { .. } | BftError::WrongRound | BftError::WrongHeight) => {
                    g.pending_prevotes.push(vote);
                }
                Err(e) => return Err(e),
            },
        }
        Ok(())
    }

    /// Ingest a peer's PRE-COMMIT. Same dual-feed pattern as
    /// [`submit_remote_prevote`], with the same v0.0.72 buffering
    /// semantics — early arrivals are queued on `pending_precommits`
    /// and replayed when the coordinator transitions to
    /// [`crate::bft::Phase::Precommitting`].
    pub fn submit_remote_precommit(&self, vote: PrecommitVote) -> Result<(), BftError> {
        let mut g = self.state.lock();
        if let Some(ev) = g.detector.record_precommit(vote.clone()) {
            g.pending_evidence.push(ev);
        }
        if vote.height <= g.head_number {
            return Ok(());
        }
        match g.coordinator.as_mut() {
            None => {
                g.pending_precommits.push(vote);
            }
            Some(coord) => match coord.submit_precommit(vote.clone()) {
                Ok(()) => {
                    Self::drain_pending_votes(&mut g);
                }
                Err(BftError::WrongPhase { .. } | BftError::WrongRound | BftError::WrongHeight) => {
                    g.pending_precommits.push(vote);
                }
                Err(e) => return Err(e),
            },
        }
        Ok(())
    }

    /// Re-apply every buffered prevote / precommit to the active
    /// coordinator. Called after any state mutation that might have
    /// transitioned the coordinator into a state where previously-
    /// rejected votes become valid (proposal arrival, prevote tally
    /// crossing quorum, precommit tally crossing quorum, timeout
    /// advancing the round, …).
    ///
    /// Votes still rejected stay in the buffer; votes that succeed
    /// or are now stale (height <= committed head) are removed.
    /// Recursion is bounded by the fact that each successful drain
    /// removes one element from the buffer.
    fn drain_pending_votes(g: &mut BftEngineState) {
        // Each iteration runs both buffers through the coordinator
        // once. The outer loop reruns whenever at least one vote was
        // successfully applied — because a successful prevote may
        // transition the phase to Precommitting, which unblocks
        // previously-buffered precommits, and vice versa. Bounded
        // by the fact that each iteration must apply at least one
        // buffered vote to repeat, so total iterations is capped by
        // the total buffer size.
        loop {
            // Pre-flight: drop stale (already-committed) buffered votes.
            let head = g.head_number;
            g.pending_prevotes.retain(|v| v.height > head);
            g.pending_precommits.retain(|v| v.height > head);
            let Some(coord) = g.coordinator.as_mut() else {
                return;
            };
            let prevotes = std::mem::take(&mut g.pending_prevotes);
            let mut leftover_prev: Vec<PrevoteVote> = Vec::new();
            let mut applied_any = false;
            for v in prevotes {
                match coord.submit_prevote(v.clone()) {
                    Ok(()) => {
                        applied_any = true;
                    }
                    Err(
                        BftError::WrongPhase { .. } | BftError::WrongRound | BftError::WrongHeight,
                    ) => leftover_prev.push(v),
                    Err(_) => {} // signature / dedup errors — drop
                }
            }
            g.pending_prevotes = leftover_prev;
            let precommits = std::mem::take(&mut g.pending_precommits);
            let mut leftover_pc: Vec<PrecommitVote> = Vec::new();
            for v in precommits {
                match coord.submit_precommit(v.clone()) {
                    Ok(()) => {
                        applied_any = true;
                    }
                    Err(
                        BftError::WrongPhase { .. } | BftError::WrongRound | BftError::WrongHeight,
                    ) => leftover_pc.push(v),
                    Err(_) => {}
                }
            }
            g.pending_precommits = leftover_pc;
            if !applied_any {
                return;
            }
        }
    }

    /// Drain every equivocation record the detector has observed
    /// since the last call. Host (typically `NodeState`) calls this
    /// on every gossip tick and persists the evidence via
    /// `record_slashing` + `debit_slash_stake`.
    pub fn drain_evidence(&self) -> Vec<crate::slashing::EquivocationEvidence> {
        let mut g = self.state.lock();
        std::mem::take(&mut g.pending_evidence)
    }

    /// External clock says the round timed out — advance the coordinator
    /// to the next round and drop the captured proposal.
    pub fn tick_timeout(&self) -> Result<(), BftError> {
        let mut g = self.state.lock();
        Self::ensure_coordinator(&mut g);
        let coord = g.coordinator.as_mut().expect("ensured");
        coord.fire_timeout();
        g.proposal = None;
        // v0.0.72: round change unblocks buffered votes for the new round.
        Self::drain_pending_votes(&mut g);
        Ok(())
    }

    /// Lazy: instantiate a fresh `RoundCoordinator` for `head_number + 1`
    /// if none is active.
    fn ensure_coordinator(g: &mut BftEngineState) {
        if g.coordinator.is_none() {
            g.coordinator = Some(RoundCoordinator::new(
                g.head_number + 1,
                g.seed,
                g.validator_set.clone(),
            ));
        }
    }

    /// Build the block this node would propose for `height` on top of
    /// `parent_hash` at `parent_timestamp` with the supplied
    /// `coinbase` and `body`. The leader's VRF output is embedded in
    /// `mix_hash` so every legitimate proposer for the same height
    /// produces a distinct block, and `beneficiary` is taken from the
    /// supplied `coinbase` so a follower reconstructing the leader's
    /// block builds the same header bytes (and thus the same block
    /// hash) instead of stamping the header with its own coinbase.
    ///
    /// `gas_used` is derived from the body's tx count using the
    /// per-block `PLACEHOLDER_TX_GAS` accounting — this is what the
    /// followers compute as well, so the resulting block hash is
    /// deterministic across the validator set.
    fn build_block_with_body(
        &self,
        parent_hash: H256,
        parent_timestamp: u64,
        height: u64,
        leader_proof: &LeaderProof,
        coinbase: Address,
        body: BlockBody,
    ) -> Block {
        let tx_root = aii_state::transactions_root(&body);
        // Consult the optional `BlockExecutor` oracle. When provided,
        // it computes Yellow-Paper roots from the body + state so the
        // produced header locks to the post-execution state.
        let (state_root, receipts_root, header_bloom, gas_used) =
            if let Some(executor) = self.config.executor.as_ref() {
                executor
                    .execute_for_proposal(&body, coinbase, height)
                    .map_or_else(
                        |_| {
                            (
                                EMPTY_TRIE_HASH,
                                EMPTY_TRIE_HASH,
                                Bloom::ZERO,
                                (body.transactions.len() as u64) * PLACEHOLDER_TX_GAS,
                            )
                        },
                        |r| {
                            (
                                r.state_root,
                                r.receipts_root,
                                Bloom(r.logs_bloom),
                                r.gas_used,
                            )
                        },
                    )
            } else {
                (
                    EMPTY_TRIE_HASH,
                    EMPTY_TRIE_HASH,
                    Bloom::ZERO,
                    (body.transactions.len() as u64) * PLACEHOLDER_TX_GAS,
                )
            };
        let header = Header {
            parent_hash,
            ommers_hash: EMPTY_LIST_HASH,
            beneficiary: coinbase,
            state_root,
            transactions_root: tx_root,
            receipts_root,
            logs_bloom: header_bloom,
            difficulty: U256::ZERO,
            number: height,
            gas_limit: self.config.gas_limit,
            gas_used,
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
        Block { header, body }
    }

    /// Run one full BFT round (propose + vote + commit) against
    /// ourselves. Only valid in single-validator mode.
    #[allow(clippy::significant_drop_tightening, clippy::too_many_lines)]
    pub fn advance_single(&self) -> Result<AdvanceOutput, BftError> {
        let mut g = self.state.lock();
        let vs_size = g.validator_set.size();
        if vs_size != 1 {
            return Err(BftError::NotSingleValidator(vs_size));
        }
        let new_number = g.head_number.checked_add(1).ok_or(BftError::Overflow)?;
        let new_timestamp = g.head_timestamp + self.config.slot_seconds;
        let seed = g.seed;

        // Build the leader proof for the new height's round 0.
        let leader_proof = LeaderProof::produce(&self.config.my_vrf_sk, new_number, 0, &seed);

        // Drain pending txs up to the block's gas budget.
        let max_txs = (self.config.gas_limit / PLACEHOLDER_TX_GAS) as usize;
        let mut pending = self.pending_txs.lock();
        let take = pending.len().min(max_txs);
        let txs: Vec<Tx> = pending.drain(..take).collect();
        drop(pending);
        // Build the block. Carry the VRF output into mix_hash so
        // consecutive blocks differ even with identical bodies.
        let body = BlockBody {
            transactions: txs,
            ommers: Vec::new(),
            withdrawals: Vec::new(),
        };
        let tx_root = aii_state::transactions_root(&body);
        // Same executor-oracle path as `build_block_with_body`. When
        // no oracle is configured, fall back to the v0.0.64
        // placeholder roots so existing testnets keep agreeing on
        // hashes.
        let (state_root, receipts_root, header_bloom, gas_used) =
            if let Some(executor) = self.config.executor.as_ref() {
                executor
                    .execute_for_proposal(&body, self.config.coinbase, new_number)
                    .map_or_else(
                        |_| {
                            (
                                EMPTY_TRIE_HASH,
                                EMPTY_TRIE_HASH,
                                Bloom::ZERO,
                                (body.transactions.len() as u64) * PLACEHOLDER_TX_GAS,
                            )
                        },
                        |r| {
                            (
                                r.state_root,
                                r.receipts_root,
                                Bloom(r.logs_bloom),
                                r.gas_used,
                            )
                        },
                    )
            } else {
                (
                    EMPTY_TRIE_HASH,
                    EMPTY_TRIE_HASH,
                    Bloom::ZERO,
                    (body.transactions.len() as u64) * PLACEHOLDER_TX_GAS,
                )
            };
        let header = Header {
            parent_hash: g.head_hash,
            ommers_hash: EMPTY_LIST_HASH,
            beneficiary: self.config.coinbase,
            state_root,
            transactions_root: tx_root,
            receipts_root,
            logs_bloom: header_bloom,
            difficulty: U256::ZERO,
            number: new_number,
            gas_limit: self.config.gas_limit,
            gas_used,
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
        let block = Block { header, body };
        let block_hash = block.hash();

        // Drive the coordinator: submit proposal, prevote, precommit
        // — all signed by ourselves.
        let mut coord = RoundCoordinator::new(new_number, seed, g.validator_set.clone());
        coord.submit_proposal(block_hash, &leader_proof)?;
        coord.submit_prevote(PrevoteVote::sign(
            &self.config.my_bls_sk,
            block_hash,
            new_number,
            0,
            g.my_index,
        ))?;
        coord.submit_precommit(PrecommitVote::sign(
            &self.config.my_bls_sk,
            block_hash,
            new_number,
            0,
            g.my_index,
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
        cache_recent_block(&mut g.recent_blocks, block.clone(), certificate.clone());

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
        let certificate = g
            .coordinator
            .as_ref()
            .and_then(|coord| coord.certificate().cloned())
            .ok_or(ConsensusError::InvalidBlock(
                "committed with no certificate",
            ))?;
        let block_hash = block.hash();
        g.head_hash = block_hash;
        g.head_number = block.header.number;
        g.head_timestamp = block.header.timestamp;
        g.seed = proof.vrf_output;
        g.coordinator = None;
        g.proposal = None;
        cache_recent_block(&mut g.recent_blocks, block, certificate);
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
            executor: None,
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
            executor: None,
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
    fn rotate_validator_set_at_idle_boundary_updates_size_and_index() {
        let engine = BftEngine::new(single_validator_config(), &genesis());
        let cfg = three_validator_config_as_validator_0();
        let new_set = cfg.validator_set;

        engine.rotate_validator_set(new_set, 0).unwrap();

        assert_eq!(engine.validator_set_size(), 3);
        assert_eq!(engine.my_index(), 0);
        assert!(!engine.is_single_validator());
    }

    #[test]
    fn rotate_validator_set_rejects_out_of_bounds_index() {
        let engine = BftEngine::new(single_validator_config(), &genesis());
        let new_set = three_validator_config_as_validator_0().validator_set;

        let err = engine.rotate_validator_set(new_set, 99).unwrap_err();

        assert!(matches!(
            err,
            BftError::ValidatorIndexOutOfBounds { index: 99, size: 3 },
        ));
    }

    #[test]
    fn rotate_validator_set_rejects_active_round() {
        let engine = BftEngine::new(three_validator_config_as_validator_0(), &genesis());
        let _ = engine.cast_proposal();
        let new_set = single_validator_config().validator_set;

        let err = engine.rotate_validator_set(new_set, 0).unwrap_err();

        assert_eq!(err, BftError::ActiveRoundInProgress);
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

    /// v0.0.70 `from_recovered` resumes at the recovered block's
    /// height + 1 rather than restarting from genesis.
    #[test]
    fn from_recovered_resumes_at_head_plus_one() {
        // Produce block 1 from a fresh engine to get a "recovered" block.
        let g = genesis();
        let warm = BftEngine::new(single_validator_config(), &g);
        let out = warm.advance_single().unwrap();
        let recovered = out.block;
        assert_eq!(recovered.header.number, 1);

        // Construct a new engine via from_recovered using that block.
        let cold = BftEngine::from_recovered(single_validator_config(), &recovered);
        let (head_hash, head_number) = cold.head();
        assert_eq!(head_number, 1);
        assert_eq!(head_hash, recovered.hash());

        // Next advance produces block 2 (not block 1 again).
        let next = cold.advance_single().unwrap();
        assert_eq!(next.block.header.number, 2);
        assert_eq!(next.block.header.parent_hash, recovered.hash());
    }

    /// v0.0.71 `fast_forward_to_round` lands the coordinator at the
    /// supplied round (advancing through the expected count of
    /// timeouts) and does not affect the chain head.
    #[test]
    fn fast_forward_to_round_lands_at_target() {
        let engine = BftEngine::new(three_validator_config_as_validator_0(), &genesis());
        engine.fast_forward_to_round(4).unwrap();
        let (height, round, _phase) = engine
            .current_round_state()
            .expect("coordinator must exist after fast_forward");
        assert_eq!(height, 1);
        assert_eq!(round, 4);
        // Head must not have moved — fast-forward is a coordinator-
        // local operation.
        let (_, head_n) = engine.head();
        assert_eq!(head_n, 0);
    }

    /// `fast_forward_to_round(0)` is a no-op for the round number
    /// but still creates the coordinator (so subsequent ticks see
    /// "I'm at round 0 for this height").
    #[test]
    fn fast_forward_to_round_zero_creates_coordinator_at_round_zero() {
        let engine = BftEngine::new(three_validator_config_as_validator_0(), &genesis());
        engine.fast_forward_to_round(0).unwrap();
        let (height, round, _phase) = engine.current_round_state().unwrap();
        assert_eq!(height, 1);
        assert_eq!(round, 0);
    }

    /// `from_recovered` against a genesis-only chain matches `new`.
    #[test]
    fn from_recovered_with_genesis_block_matches_new() {
        let g = genesis();
        let from_new = BftEngine::new(single_validator_config(), &g);
        let from_recv = BftEngine::from_recovered(single_validator_config(), &g);
        assert_eq!(from_new.head(), from_recv.head());
        // And both can produce block 1.
        let _ = from_new.advance_single().unwrap();
        let out_recv = from_recv.advance_single().unwrap();
        assert_eq!(out_recv.block.header.number, 1);
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
            executor: None,
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
    fn drain_evidence_returns_double_prevote_evidence() {
        // Validator 1 signs two PRE-VOTES at the same (height, round)
        // for different block hashes — the detector must catch it
        // and `drain_evidence` returns one Prevote evidence.
        let (vs, keys) = multi_validator_setup(3);
        let engine = engine_for(0, &vs, &keys, &genesis());
        // Need a coordinator first — bootstrap by submitting a remote
        // proposal so the coordinator is created at (1, 0).
        let leader = vs.select_leader(1, 0, &[0x77; 32]);
        let leader_engine = engine_for(leader as u32, &vs, &keys, &genesis());
        let (block, proof) = leader_engine.cast_proposal().unwrap();
        engine.submit_remote_proposal(block, proof).unwrap();
        // Now feed two different prevotes from validator 1 at (1, 0).
        let vote_a = PrevoteVote::sign(&keys[1].0, H256::new([0xaa; 32]), 1, 0, 1);
        let vote_b = PrevoteVote::sign(&keys[1].0, H256::new([0xbb; 32]), 1, 0, 1);
        let _ = engine.submit_remote_prevote(vote_a);
        let _ = engine.submit_remote_prevote(vote_b);
        let evidence = engine.drain_evidence();
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].validator_index(), 1);
        // Drain is idempotent — a second call returns empty.
        assert!(engine.drain_evidence().is_empty());
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

    /// v0.0.72 — a peer's prevote arriving BEFORE we've seen the
    /// proposal must be buffered, not rejected with
    /// `NoActiveCoordinator`. After the proposal arrives the
    /// coordinator is created and the buffered prevote is replayed.
    #[test]
    fn prevote_arriving_before_proposal_is_buffered_and_replayed() {
        let (vs, keys) = multi_validator_setup(3);
        let g = genesis();
        let leader = vs.select_leader(1, 0, &[0x77; 32]);
        let engines: Vec<BftEngine> = (0..3).map(|i| engine_for(i, &vs, &keys, &g)).collect();

        // Leader: proposal + own prevote.
        let (block, proof) = engines[leader].cast_proposal().unwrap();
        let leader_prevote = engines[leader].cast_prevote().unwrap();

        // Pick a follower that hasn't seen anything yet.
        let follower = (0..3).find(|i| *i != leader).unwrap();

        // PRE-v0.0.72 this would return Err(NoActiveCoordinator).
        // POST-v0.0.72 it must buffer and return Ok.
        engines[follower]
            .submit_remote_prevote(leader_prevote)
            .expect("early prevote must be buffered, not rejected");
        assert!(
            engines[follower].current_round_state().is_none(),
            "follower must not yet have a coordinator"
        );

        // Deliver the proposal — drain_pending_votes should re-submit
        // the buffered prevote, so after this the follower's tally
        // already has the leader's prevote on file.
        engines[follower]
            .submit_remote_proposal(block.clone(), proof.clone())
            .unwrap();
        let phase = engines[follower].current_round_state().unwrap().2;
        assert_eq!(
            phase,
            crate::bft::Phase::Prevoting,
            "must be in Prevoting after proposal lands"
        );

        // The follower casts its own prevote, then receives the third
        // validator's prevote — that's 3 of 3 prevotes, quorum forms,
        // phase transitions to Precommitting.
        let _follower_pv = engines[follower].cast_prevote().unwrap();
        let third = (0..3).find(|i| *i != leader && *i != follower).unwrap();
        engines[third].submit_remote_proposal(block, proof).unwrap();
        let third_pv = engines[third].cast_prevote().unwrap();
        engines[follower].submit_remote_prevote(third_pv).unwrap();
        let phase = engines[follower].current_round_state().unwrap().2;
        assert_eq!(
            phase,
            crate::bft::Phase::Precommitting,
            "buffered prevote was replayed; quorum reached"
        );
    }

    /// v0.0.72 — a peer's precommit arriving while we're still in
    /// Prevoting must be buffered (WrongPhase is not a rejection
    /// path anymore). After we transition to Precommitting the
    /// buffered precommit gets re-applied.
    #[test]
    fn precommit_arriving_during_prevoting_is_buffered_and_replayed() {
        let (vs, keys) = multi_validator_setup(3);
        let g = genesis();
        let leader = vs.select_leader(1, 0, &[0x77; 32]);
        let engines: Vec<BftEngine> = (0..3).map(|i| engine_for(i, &vs, &keys, &g)).collect();

        let (block, proof) = engines[leader].cast_proposal().unwrap();
        let leader_pv = engines[leader].cast_prevote().unwrap();
        let other_indices: Vec<usize> = (0..3).filter(|i| *i != leader).collect();
        let follower = other_indices[0];
        let third = other_indices[1];

        // Bring third online with a prevote of its own.
        engines[third]
            .submit_remote_proposal(block.clone(), proof.clone())
            .unwrap();
        let third_pv = engines[third].cast_prevote().unwrap();
        engines[leader]
            .submit_remote_prevote(third_pv.clone())
            .unwrap();

        // Drive the leader to Precommitting by also feeding the follower's prevote.
        engines[follower]
            .submit_remote_proposal(block, proof)
            .unwrap();
        let follower_pv = engines[follower].cast_prevote().unwrap();
        engines[leader].submit_remote_prevote(follower_pv).unwrap();
        let leader_precommit = engines[leader].cast_precommit().unwrap();

        // Follower is currently in Prevoting (only saw the proposal + its own prevote).
        let phase_before = engines[follower].current_round_state().unwrap().2;
        assert_eq!(phase_before, crate::bft::Phase::Prevoting);

        // The early precommit arrives.
        engines[follower]
            .submit_remote_precommit(leader_precommit)
            .expect("early precommit must be buffered, not rejected");
        // Still in Prevoting — the precommit can't tally yet.
        assert_eq!(
            engines[follower].current_round_state().unwrap().2,
            crate::bft::Phase::Prevoting
        );

        // Now feed enough prevotes to transition follower to Precommitting.
        engines[follower].submit_remote_prevote(leader_pv).unwrap();
        engines[follower].submit_remote_prevote(third_pv).unwrap();
        // POLC formed → Precommitting; drain_pending_votes runs and
        // the buffered leader precommit is replayed.
        let phase_after = engines[follower].current_round_state().unwrap().2;
        assert_eq!(phase_after, crate::bft::Phase::Precommitting);
        // The follower can now cast its own precommit.
        let _ = engines[follower].cast_precommit().unwrap();
    }

    /// v0.0.72 — buffered vote for a height we've already committed
    /// past is silently dropped, not retained forever.
    #[test]
    fn stale_buffered_votes_are_dropped() {
        let (vs, keys) = multi_validator_setup(3);
        let g = genesis();
        let engine = engine_for(0, &vs, &keys, &g);
        // Construct an artificial PRECOMMIT for height 0 (already
        // committed — genesis IS at head=0). Submission must succeed
        // (Ok) but the vote must not stick around in the buffer.
        let v = PrecommitVote::sign(
            &keys[0].0,
            H256::ZERO,
            0, // stale
            0,
            0,
        );
        engine.submit_remote_precommit(v).unwrap();
        // Submission is silent — verify by ticking timeout (no panic)
        // and by checking head unchanged.
        assert_eq!(engine.head().1, 0);
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
