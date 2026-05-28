//! # aii-node (library surface)
//!
//! The `aii-node` crate is primarily a binary (`aiid`) — but a small
//! library surface lets integration tests boot a node in-process and
//! exercise it without subprocesses.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::significant_drop_tightening)]

pub mod bft_bootstrap;
pub mod bft_p2p;
pub mod bft_state;
pub mod dpos;
pub mod governance;
pub mod peer_cache;
pub mod precompile;
#[cfg(unix)]
pub mod release_install;
pub mod release_store;
pub mod staking;
pub mod sync;

pub use dpos::{elect_active_set, latest_validator_set, ValidatorEntry};
pub use governance::{Governance, Proposal, ProposalStatus, Vote};
pub use precompile::{dispatch as precompile_dispatch, PrecompileOutcome, PRECOMPILE_ADDR};
pub use staking::{StakeRecord, StakeTable};
pub use sync::bootstrap_sync_from_peer;

/// `BlockExecutor` adapter built on top of a live [`NodeState`].
///
/// Today's impl is the half-step "consult-current-state" mode.
/// The engine asks for post-execution roots **before** consensus,
/// but the oracle returns the current `state_root` (i.e. the
/// post-block-(N-1) state, not the post-block-N state). Hash
/// stability still wins — both leader and followers compute the
/// same answer because every node starts the round at the same
/// head state.
///
/// A future iteration applies the body against a state snapshot
/// before answering, so the header truly locks to the post-block-N
/// state. The trait surface stays unchanged.
pub struct NodeStateExecutor {
    state: Arc<NodeState>,
}

impl NodeStateExecutor {
    /// Wrap a node-state handle.
    #[must_use]
    pub const fn new(state: Arc<NodeState>) -> Self {
        Self { state }
    }
}

impl aii_consensus_iface::BlockExecutor for NodeStateExecutor {
    fn execute_for_proposal(
        &self,
        body: &BlockBody,
        _coinbase: Address,
        _block_number: u64,
    ) -> Result<aii_consensus_iface::PostBlockRoots, aii_consensus_iface::ConsensusError> {
        let state_root = self
            .state
            .state()
            .state_root()
            .map_err(|e| aii_consensus_iface::ConsensusError::Io(e.to_string()))?;
        // Pre-execution receipts_root = empty (no receipts have been
        // computed for this body yet). Same for the bloom.
        Ok(aii_consensus_iface::PostBlockRoots {
            state_root,
            receipts_root: aii_block::EMPTY_TRIE_HASH,
            logs_bloom: [0u8; 256],
            gas_used: (body.transactions.len() as u64) * 21_000,
        })
    }
}

use aii_block::tx::Tx;
use aii_block::{Block, BlockBody, Bloom, Hashable, Header, Receipt, TxType, EMPTY_TRIE_HASH};
use aii_config::ChainSpec;
use aii_net_txpool::{effective_gas_price, AddOutcome, PoolEntry, TxPool};
use aii_rpc::{
    AccountView, ActiveValidatorsView, ForkView, HeaderView, LogEntryView, LogFilter, LogView,
    PostRootsView, ProposalView, ReceiptView, RpcState, SlashView, StakeView, SubchainAnchorView,
    SubmitTxError, TxView, ValidatorEntryView,
};
use aii_state::StateDb;
use aii_storage::{ColumnFamily, KvBackend, RocksDbBackend, WriteBatch};
use aii_types::{Address, H256, U256};
use alloy_rlp::{Decodable, Encodable};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// Meta-CF key holding the canonical chain head as a big-endian u64.
const META_KEY_HEAD: &[u8] = b"head_block_number";
/// Meta-CF key holding the canonical chain head hash (raw 32 bytes).
const META_KEY_HEAD_HASH: &[u8] = b"head_block_hash";
/// Meta-CF key prefix for persisted slashing records.
/// Layout: `b"slash:" ‖ validator_index_be4 ‖ height_be8 ‖ phase_byte`.
/// `phase_byte` is `0` for prevote, `1` for precommit. Yields one entry
/// per slashed `(validator, height, phase)` triple, listable by prefix.
const META_KEY_SLASH_PREFIX: &[u8] = b"slash:";
/// Meta-CF key prefix for persisted fork-detection records.
/// Layout: `b"fork:" ‖ height_be8 ‖ fork_hash[32]`. Multiple records
/// per height are allowed — every conflicting hash seen lands here so
/// the operator can audit re-org candidates.
const META_KEY_FORK_PREFIX: &[u8] = b"fork:";
/// Meta-CF key prefix for post-block root sidecar records.
/// Layout: `b"postroot:" ‖ block_hash[32]` → 32+32+256-byte value
/// (state_root ‖ receipts_root ‖ logs_bloom). Lets light clients
/// fetch the Yellow-Paper roots that *should* have been in the
/// header. The header itself still embeds the v0.0.39-compatible
/// placeholder so block hashes don't drift.
const META_KEY_POSTROOT_PREFIX: &[u8] = b"postroot:";

/// Bundle of post-block Yellow-Paper roots persisted as a sidecar
/// to the block header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostBlockRoots {
    /// `keccak256(rlp(world_state_mpt))` after applying every tx in
    /// the block.
    pub state_root: H256,
    /// `keccak256(rlp(receipts_mpt))` over the block's receipts.
    pub receipts_root: H256,
    /// 256-byte aggregate logs bloom (Yellow Paper §4.4.2).
    pub logs_bloom: Bloom,
}

/// One observed competing block at a given height.
///
/// Recorded by `commit_block` when a new block lands whose height
/// already has a different canonical block. Re-org execution is
/// intentionally deferred (state rollback requires the engine
/// apply-then-hash refactor); this primitive is observability-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkRecord {
    /// Height at which the fork was detected.
    pub height: u64,
    /// Hash currently recorded as the local canonical block at this
    /// height.
    pub canonical_hash: H256,
    /// Conflicting hash that was rejected.
    pub fork_hash: H256,
}

/// Persistent slashing record. Built by `NodeState::record_slashing` from
/// an `aii_consensus_bft::EquivocationEvidence`; queryable via the new
/// `aii_listSlashings` RPC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashRecord {
    /// Validator index that signed both conflicting votes.
    pub validator_index: u32,
    /// Height where the equivocation occurred.
    pub height: u64,
    /// BFT phase: `"prevote"` or `"precommit"`.
    pub phase: &'static str,
    /// Both conflicting block hashes — the smoking gun.
    pub block_hashes: [H256; 2],
}

/// In-process node state.
///
/// Owns a `ChainSpec`, a head-block counter, a persistent `StateDb`, and a
/// persistent header/body/tx index — all backed by RocksDB so that a
/// restart restores the chain head, every block, every tx lookup, and
/// every account state.
pub struct NodeState {
    spec: ChainSpec,
    head: AtomicU64,
    backend: Arc<RocksDbBackend>,
    state: Arc<StateDb<RocksDbBackend>>,
    /// In-memory index of the persistent header/body/tx store. Rebuilt
    /// from disk on startup via [`NodeState::recover`].
    blocks: RwLock<BlockStore>,
    /// Mempool for incoming signed transactions (v0.0.37).
    tx_pool: TxPool,
    /// Latest signed release manifest accepted via the v0.0.75
    /// `aii_announceRelease` RPC. `None` on a fresh node.
    latest_release: RwLock<Option<aii_crypto::release::ReleaseManifest>>,
    /// Directory the node was launched with. Used by v0.0.76
    /// release-binary store helpers to resolve
    /// `<data-dir>/releases/<version>`. `None` until the host
    /// (`aiid` main) calls [`NodeState::set_data_dir`]; in that
    /// state release-store ops fail-soft (RPC returns
    /// `accepted: false`) rather than panicking.
    data_dir: RwLock<Option<std::path::PathBuf>>,
    /// HTTP-RPC URLs of peer nodes used for cross-node release
    /// propagation (v0.0.77). Populated from `aiid --update-peers`
    /// at startup. Empty means "this node accepts announcements
    /// but never re-broadcasts" — useful for leaf clients.
    update_peers: RwLock<Vec<String>>,
    /// When `true`, accepting a release manifest whose binary is
    /// already in `<data-dir>/releases/<version>` triggers an
    /// atomic install + execve self-restart (v0.0.78). Operator
    /// opt-in via `aiid --auto-install-releases`; default `false`
    /// because in-place restarts are disruptive and most
    /// production operators want to schedule the swap.
    auto_install_releases: AtomicBool,
    /// Test-only override (v0.0.78): when `Some`,
    /// [`RpcState::install_release`] uses this path as the
    /// install target instead of `/proc/self/exe`, and skips
    /// the `execve` self-restart spawn. Required because
    /// otherwise integration tests that exercise install would
    /// actually swap the test-runner binary and `exec` it,
    /// which is catastrophic for `cargo test`. Production
    /// always leaves this `None`.
    install_target_override: RwLock<Option<std::path::PathBuf>>,
}

/// Headers + bodies keyed by hash and number, plus an insertion-order
/// vector used to serve "recent N blocks" and a tx-hash → (block_number,
/// in-block index) map so `aii_getTransaction` can do single-hop lookups.
#[derive(Default)]
struct BlockStore {
    by_hash: HashMap<H256, Header>,
    by_number: HashMap<u64, H256>,
    /// Insertion order for `recent_headers` — push on commit, scan tail.
    order: Vec<H256>,
    /// Block bodies keyed by block hash. Empty `BlockBody` for genesis-
    /// like blocks with no txs is stored normally (always present once
    /// indexed). Needed for `aii_getBlockTransactions` / per-tx lookups.
    body_by_hash: HashMap<H256, BlockBody>,
    /// `tx_hash → (block_number, in-block index)`. Lets a single
    /// `aii_getTransaction` call resolve a transaction without scanning
    /// every block. Populated on commit alongside `body_by_hash`.
    tx_index: HashMap<H256, (u64, usize)>,
}

impl NodeState {
    /// Construct a fresh node on top of `backend`. Starting head is 0
    /// (genesis); no blocks indexed. Use [`NodeState::recover`] when
    /// reopening an existing data directory.
    pub fn new(spec: ChainSpec, backend: Arc<RocksDbBackend>) -> Arc<Self> {
        Arc::new(Self {
            spec,
            head: AtomicU64::new(0),
            state: Arc::new(StateDb::new(Arc::clone(&backend))),
            backend,
            blocks: RwLock::new(BlockStore::default()),
            tx_pool: TxPool::new(100_000),
            latest_release: RwLock::new(None),
            data_dir: RwLock::new(None),
            update_peers: RwLock::new(Vec::new()),
            auto_install_releases: AtomicBool::new(false),
            install_target_override: RwLock::new(None),
        })
    }

    /// Open a temporary RocksDB backend (test-only) and return a fresh
    /// `NodeState` bound to it. The tempdir is leaked — the OS reaps it
    /// once the process exits.
    ///
    /// # Panics
    /// Panics if RocksDB cannot open a tempdir (filesystem error).
    #[must_use]
    pub fn new_for_tests(spec: ChainSpec) -> Arc<Self> {
        let backend = Arc::new(
            RocksDbBackend::open_in_temp().expect("RocksDbBackend::open_in_temp for tests"),
        );
        Self::new(spec, backend)
    }

    /// Reopen a previously-persisted node from `backend`. Reads:
    /// * `Meta:head_block_number` → restored head counter,
    /// * every `Headers` entry → `(hash → Header)` + `(number → hash)`,
    /// * every `Bodies` entry → `body_by_hash` + tx-hash index,
    /// * `order` is rebuilt by sorting headers by `header.number` so that
    ///   `recent_headers` returns newest-first across restarts.
    ///
    /// State accounts are already on disk and need no replay.
    ///
    /// # Errors
    /// Returns a storage-level error if reading any column family fails
    /// or if a persisted header/body fails to RLP-decode.
    pub fn recover(
        spec: ChainSpec,
        backend: Arc<RocksDbBackend>,
    ) -> Result<Arc<Self>, Box<dyn std::error::Error + Send + Sync>> {
        let mut by_hash: HashMap<H256, Header> = HashMap::new();
        let mut by_number: HashMap<u64, H256> = HashMap::new();
        let mut body_by_hash: HashMap<H256, BlockBody> = HashMap::new();
        let mut tx_index: HashMap<H256, (u64, usize)> = HashMap::new();

        for kv in backend.iter(ColumnFamily::Headers) {
            let (k, v) = kv?;
            if k.len() != 32 {
                continue;
            }
            let mut h_arr = [0u8; 32];
            h_arr.copy_from_slice(&k);
            let hash = H256::new(h_arr);
            let mut s: &[u8] = &v;
            let header = Header::decode(&mut s)?;
            by_number.insert(header.number, hash);
            by_hash.insert(hash, header);
        }

        for kv in backend.iter(ColumnFamily::Bodies) {
            let (k, v) = kv?;
            if k.len() != 32 {
                continue;
            }
            let mut h_arr = [0u8; 32];
            h_arr.copy_from_slice(&k);
            let hash = H256::new(h_arr);
            let mut s: &[u8] = &v;
            let body = BlockBody::decode(&mut s)?;
            if let Some(header) = by_hash.get(&hash) {
                for (idx, tx) in body.transactions.iter().enumerate() {
                    tx_index.insert(tx.hash(), (header.number, idx));
                }
            }
            body_by_hash.insert(hash, body);
        }

        // Rebuild `order` (insertion-order vec for recent_headers) by
        // sorting headers by number ascending — same observable order as
        // the live commit path produces.
        let mut sorted: Vec<(u64, H256)> = by_number.iter().map(|(n, h)| (*n, *h)).collect();
        sorted.sort_unstable_by_key(|(n, _)| *n);
        let order: Vec<H256> = sorted.into_iter().map(|(_, h)| h).collect();

        let head = backend
            .get(ColumnFamily::Meta, META_KEY_HEAD)?
            .filter(|b| b.len() == 8)
            .map_or(0, |b| {
                let mut arr = [0u8; 8];
                arr.copy_from_slice(&b);
                u64::from_be_bytes(arr)
            });

        Ok(Arc::new(Self {
            spec,
            head: AtomicU64::new(head),
            state: Arc::new(StateDb::new(Arc::clone(&backend))),
            backend,
            blocks: RwLock::new(BlockStore {
                by_hash,
                by_number,
                order,
                body_by_hash,
                tx_index,
            }),
            tx_pool: TxPool::new(100_000),
            latest_release: RwLock::new(None),
            data_dir: RwLock::new(None),
            update_peers: RwLock::new(Vec::new()),
            auto_install_releases: AtomicBool::new(false),
            install_target_override: RwLock::new(None),
        }))
    }

    /// Borrow the mempool (for the producer drain loop).
    #[must_use]
    pub const fn tx_pool(&self) -> &TxPool {
        &self.tx_pool
    }

    /// Update the head block number — called when a new block is finalised.
    /// Persists the new head to the `Meta` CF so a restart restores it.
    pub fn set_head(&self, n: u64) {
        self.head.store(n, Ordering::Relaxed);
        let _ = self
            .backend
            .put(ColumnFamily::Meta, META_KEY_HEAD, &n.to_be_bytes());
        if let Some(hash) = self
            .blocks
            .read()
            .ok()
            .and_then(|s| s.by_number.get(&n).copied())
        {
            let _ = self
                .backend
                .put(ColumnFamily::Meta, META_KEY_HEAD_HASH, hash.as_bytes());
        }
    }

    /// Index a finalised block so RPC clients can look it up via
    /// `aii_getBlockHeader` / `aii_recentBlocks`, and execute every
    /// transaction in the block body against the world-state.
    ///
    /// Idempotent on the same hash — a re-applied block is skipped
    /// before any state mutation, so node1 and node2 each apply each
    /// finalised block exactly once even though both nodes call
    /// `commit_block` independently from their own harvest loops.
    ///
    /// Tx execution today goes through [`aii_evm::execute_transfer`] —
    /// the fast-path EOA-to-EOA value-transfer executor. Failing txs
    /// (bad nonce, insufficient balance, contract-call attempt) are
    /// logged at `warn` and skipped; the block-hash agreement between
    /// validators is unaffected because gas accounting is per-tx-count,
    /// not per-tx-receipt.
    pub fn commit_block(&self, block: &Block) {
        let hash = block.hash();
        let mut fork_detected: Option<H256> = None;
        {
            let mut s = self.blocks.write().expect("BlockStore lock not poisoned");
            if s.by_hash.contains_key(&hash) {
                return;
            }
            // Fork detection: same height, different hash → record the
            // conflict and skip the canonical-update path. Real
            // re-org execution (rollback + re-apply) lands in a future
            // release once state-checkpointing is in place.
            if let Some(existing) = s.by_number.get(&block.header.number).copied() {
                if existing != hash {
                    fork_detected = Some(existing);
                }
            }
            if fork_detected.is_none() {
                s.by_hash.insert(hash, block.header.clone());
                s.by_number.insert(block.header.number, hash);
                s.order.push(hash);
                // Index every tx by hash so `aii_getTransaction` is O(1).
                for (idx, tx) in block.body.transactions.iter().enumerate() {
                    s.tx_index.insert(tx.hash(), (block.header.number, idx));
                }
                s.body_by_hash.insert(hash, block.body.clone());
            }
        }
        if let Some(canonical) = fork_detected {
            self.record_fork(block.header.number, canonical, hash);
            tracing::warn!(
                height = block.header.number,
                canonical = ?canonical,
                fork = ?hash,
                "fork detected — recorded for audit, not re-orged",
            );
            return;
        }
        self.persist_block(block, hash);
        self.execute_block_txs(block);
        self.scan_microchain_anchors(block, hash);
        self.maybe_elect_validator_set(block.header.number);
    }

    /// Persist a fork-detection record. Key: `b"fork:" ‖ height_be8 ‖
    /// fork_hash[32]`; value: `canonical_hash[32]`. Multiple records
    /// per height are kept so the operator can see every rejected
    /// candidate.
    fn record_fork(&self, height: u64, canonical: H256, fork: H256) {
        let mut key = Vec::with_capacity(META_KEY_FORK_PREFIX.len() + 8 + 32);
        key.extend_from_slice(META_KEY_FORK_PREFIX);
        key.extend_from_slice(&height.to_be_bytes());
        key.extend_from_slice(fork.as_bytes());
        if let Err(e) = self
            .backend
            .put(ColumnFamily::Meta, &key, canonical.as_bytes())
        {
            tracing::error!(error = %e, "record_fork: write failed");
        }
    }

    /// List every persisted fork record. Used by `aii_listForks` RPC
    /// + ops tooling.
    ///
    /// # Errors
    /// Propagates backend errors / decode failures.
    pub fn list_forks(&self) -> Result<Vec<ForkRecord>, Box<dyn std::error::Error + Send + Sync>> {
        let mut out = Vec::new();
        for kv in self
            .backend
            .iter_prefix(ColumnFamily::Meta, META_KEY_FORK_PREFIX)
        {
            let (k, v) = kv?;
            let suffix = &k[META_KEY_FORK_PREFIX.len()..];
            if suffix.len() != 8 + 32 || v.len() != 32 {
                continue;
            }
            let mut h_arr = [0u8; 8];
            h_arr.copy_from_slice(&suffix[..8]);
            let height = u64::from_be_bytes(h_arr);
            let mut fork_arr = [0u8; 32];
            fork_arr.copy_from_slice(&suffix[8..40]);
            let mut canonical_arr = [0u8; 32];
            canonical_arr.copy_from_slice(&v);
            out.push(ForkRecord {
                height,
                canonical_hash: H256::new(canonical_arr),
                fork_hash: H256::new(fork_arr),
            });
        }
        Ok(out)
    }

    /// If `block_number` is a multiple of the chain's
    /// `epoch_length_blocks`, run a fresh DPoS election against the
    /// persistent stake table and persist the result under the new
    /// epoch index. No-op at every non-boundary height.
    ///
    /// `block_number == 0` (genesis) is intentionally skipped so the
    /// genesis validator set isn't overwritten by an empty election.
    fn maybe_elect_validator_set(&self, block_number: u64) {
        let epoch_len = self.spec.epoch_length_blocks;
        if block_number == 0 || epoch_len == 0 || block_number % epoch_len != 0 {
            return;
        }
        let epoch = block_number / epoch_len;
        let table = self.stake_table();
        let elected = match elect_active_set(
            &table,
            U256::from(self.spec.min_validator_stake_wei),
            self.spec.validators_per_epoch,
        ) {
            Ok(set) => set,
            Err(e) => {
                tracing::warn!(error = %e, "elect_active_set failed — skipping epoch");
                return;
            }
        };
        if let Err(e) = dpos::persist_validator_set(&self.backend, epoch, &elected) {
            tracing::error!(error = %e, "persist_validator_set failed");
            return;
        }
        tracing::info!(
            epoch,
            block = block_number,
            elected = elected.len(),
            "DPoS validator set re-elected",
        );
    }

    /// Walk every tx in `block` looking for sub-chain flush-anchor
    /// calldata (`AII_FLUSH ‖ sub_chain_id_be4 ‖ sub_block_hash ‖
    /// sub_block_number_be8`). Each match updates the
    /// `ColumnFamily::MicroChain` registry with the new
    /// [`aii_microchain::FlushAnchor`].
    fn scan_microchain_anchors(&self, block: &Block, parent_block_hash: H256) {
        let chain_id = self.spec.chain_id;
        for tx in &block.body.transactions {
            let payload = match tx {
                Tx::Legacy(t) => aii_microchain::parse_flush_anchor(&t.data),
                Tx::Eip1559(t) => aii_microchain::parse_flush_anchor(&t.data),
                Tx::Eip4844(t) => aii_microchain::parse_flush_anchor(&t.data),
            };
            let Some(payload) = payload else { continue };
            // Light sanity: sender == to, value == 0 (per the producer
            // convention in `aii cli run-subchain`).
            let to = match tx {
                Tx::Legacy(t) => t.to,
                Tx::Eip1559(t) => t.to,
                Tx::Eip4844(t) => Some(t.to),
            };
            let Ok(sender) = tx.recover_signer(chain_id) else {
                continue;
            };
            if Some(sender) != to {
                continue;
            }
            let anchor = aii_microchain::FlushAnchor {
                sub_block_hash: payload.sub_block_hash,
                parent_block_hash,
                sub_block_number: payload.sub_block_number,
            };
            self.persist_flush_anchor(payload.sub_chain_id, &anchor);
            tracing::info!(
                sub_chain_id = payload.sub_chain_id.0,
                sub_block_number = payload.sub_block_number,
                parent_block = block.header.number,
                "recorded microchain flush anchor",
            );
        }
    }

    /// Persist a flush anchor for `id` to `ColumnFamily::MicroChain`
    /// under key `b"anchor:" ‖ id_be4`. Overwrites the previous anchor
    /// idempotently — last-flushed wins.
    fn persist_flush_anchor(
        &self,
        id: aii_microchain::MicroChainId,
        anchor: &aii_microchain::FlushAnchor,
    ) {
        let mut key = Vec::with_capacity(7 + 4);
        key.extend_from_slice(b"anchor:");
        key.extend_from_slice(&id.0.to_be_bytes());
        // Value: sub_block_hash[32] ‖ parent_block_hash[32] ‖ sub_block_number_be8
        let mut val = Vec::with_capacity(72);
        val.extend_from_slice(anchor.sub_block_hash.as_bytes());
        val.extend_from_slice(anchor.parent_block_hash.as_bytes());
        val.extend_from_slice(&anchor.sub_block_number.to_be_bytes());
        if let Err(e) = self.backend.put(ColumnFamily::MicroChain, &key, &val) {
            tracing::error!(
                id = id.0,
                error = %e,
                "persist_flush_anchor: write failed",
            );
        }
    }

    /// Read the most recent flush anchor for `id`, or `Ok(None)` if no
    /// flush has been recorded for that sub-chain yet.
    ///
    /// # Errors
    /// Propagates backend errors.
    pub fn last_flush_anchor(
        &self,
        id: aii_microchain::MicroChainId,
    ) -> Result<Option<aii_microchain::FlushAnchor>, Box<dyn std::error::Error + Send + Sync>> {
        let mut key = Vec::with_capacity(7 + 4);
        key.extend_from_slice(b"anchor:");
        key.extend_from_slice(&id.0.to_be_bytes());
        let Some(v) = self.backend.get(ColumnFamily::MicroChain, &key)? else {
            return Ok(None);
        };
        if v.len() != 72 {
            return Ok(None);
        }
        let mut sub = [0u8; 32];
        sub.copy_from_slice(&v[..32]);
        let mut parent = [0u8; 32];
        parent.copy_from_slice(&v[32..64]);
        let mut num = [0u8; 8];
        num.copy_from_slice(&v[64..72]);
        Ok(Some(aii_microchain::FlushAnchor {
            sub_block_hash: H256::new(sub),
            parent_block_hash: H256::new(parent),
            sub_block_number: u64::from_be_bytes(num),
        }))
    }

    /// Persist header + body + tx-index entries for `block` to RocksDB in a
    /// single atomic `WriteBatch`. The block-hash agreement check in
    /// `commit_block` runs first, so this path only fires for novel blocks.
    fn persist_block(&self, block: &Block, hash: H256) {
        let mut header_buf = alloy_rlp::bytes::BytesMut::new();
        block.header.encode(&mut header_buf);
        let mut body_buf = alloy_rlp::bytes::BytesMut::new();
        block.body.encode(&mut body_buf);

        let mut wb = WriteBatch::new();
        wb.put(ColumnFamily::Headers, hash.as_bytes(), &header_buf);
        wb.put(ColumnFamily::Bodies, hash.as_bytes(), &body_buf);
        // `number → hash` reverse map so number-based lookups can be
        // answered from disk if the in-memory cache evicts (future work).
        let nk = number_key(block.header.number);
        wb.put(ColumnFamily::Meta, &nk, hash.as_bytes());
        // `tx_hash → (block_hash ‖ index_be8)` for `aii_getTransaction`.
        for (idx, tx) in block.body.transactions.iter().enumerate() {
            let mut v = Vec::with_capacity(32 + 8);
            v.extend_from_slice(hash.as_bytes());
            v.extend_from_slice(&(idx as u64).to_be_bytes());
            wb.put(ColumnFamily::TxLookup, tx.hash().as_bytes(), &v);
        }
        if let Err(e) = self.backend.write(wb) {
            tracing::error!(
                number = block.header.number,
                error = %e,
                "persist block: WriteBatch failed",
            );
        }
    }

    #[allow(clippy::too_many_lines)]
    fn execute_block_txs(&self, block: &Block) {
        use aii_block::tx::{TxEip1559, TxLegacy};
        let chain_id = self.spec.chain_id;
        let mut cumulative_gas_used: u64 = 0;
        let mut receipts: Vec<(H256, Receipt)> = Vec::new();
        for tx in &block.body.transactions {
            let tx_hash = tx.hash();
            let Ok(sender) = tx.recover_signer(chain_id) else {
                tracing::warn!(
                    block = block.header.number,
                    "tx signer recovery failed — skipping",
                );
                continue;
            };
            let (to, value, data, gas_limit, gas_price, tx_type) = match tx {
                Tx::Legacy(TxLegacy {
                    to,
                    value,
                    data,
                    gas_limit,
                    gas_price,
                    ..
                }) => (
                    *to,
                    *value,
                    data.clone(),
                    *gas_limit,
                    *gas_price,
                    TxType::Legacy,
                ),
                Tx::Eip1559(TxEip1559 {
                    to,
                    value,
                    data,
                    gas_limit,
                    max_fee_per_gas,
                    ..
                }) => (
                    *to,
                    *value,
                    data.clone(),
                    *gas_limit,
                    *max_fee_per_gas,
                    TxType::Eip1559,
                ),
                Tx::Eip4844(_) => {
                    tracing::warn!(
                        block = block.header.number,
                        "EIP-4844 blob txs not yet executable — skipping",
                    );
                    continue;
                }
            };
            // Precompile path: if `to == PRECOMPILE_ADDR` the tx is a
            // staking / governance call. Dispatch against the
            // persistent stores; charge a flat 21 000 gas. We
            // intentionally skip revm — the precompile is pure AII
            // state and doesn't run EVM bytecode.
            if to == Some(precompile::PRECOMPILE_ADDR) {
                let table = self.stake_table();
                let gov = self.governance();
                let outcome = precompile::dispatch(
                    &table,
                    &gov,
                    sender,
                    value,
                    &data,
                    block.header.number,
                    self.spec.unbonding_period_blocks,
                );
                let success = outcome.is_ok();
                if let Err(e) = &outcome {
                    tracing::warn!(
                        block = block.header.number,
                        sender = ?sender,
                        error = %e,
                        "precompile dispatch failed",
                    );
                }
                let gas_charged = 21_000u64;
                cumulative_gas_used = cumulative_gas_used.saturating_add(gas_charged);
                let fee = U256::from(gas_charged).saturating_mul(gas_price);
                if !fee.is_zero() {
                    self.credit(&block.header.beneficiary, fee);
                }
                receipts.push((
                    tx_hash,
                    Receipt {
                        tx_type,
                        status: success,
                        cumulative_gas_used,
                        logs_bloom: Bloom::ZERO,
                        logs: vec![],
                    },
                ));
                continue;
            }
            // Snapshot the sender balance so we can compute the actual
            // fee debited by revm (sender_pre - sender_post - value).
            // revm charges `gas_used * gas_price` from the sender as
            // part of its tx-validation step; we hand that same amount
            // to the block beneficiary below.
            let sender_pre = self
                .state
                .account(&sender)
                .ok()
                .flatten()
                .map_or(U256::ZERO, |a| a.balance);
            match aii_evm::execute_with_revm(
                &self.state,
                sender,
                to,
                value,
                data,
                gas_limit,
                gas_price,
            ) {
                Ok(summary) => {
                    cumulative_gas_used = cumulative_gas_used.saturating_add(summary.gas_used);
                    // Credit gas fee to block beneficiary. Use the
                    // direct gas_used * gas_price product — matches
                    // what revm debited from the sender.
                    let fee = U256::from(summary.gas_used).saturating_mul(gas_price);
                    if !fee.is_zero() {
                        self.credit(&block.header.beneficiary, fee);
                    }
                    let _ = sender_pre; // reserved for future divergence checks
                                        // Per-tx bloom: address + each topic of every log
                                        // gets accrued into a fresh Bloom; this is the
                                        // canonical Yellow-Paper §4.4.3 receipt bloom.
                    let mut tx_bloom = Bloom::ZERO;
                    for log in &summary.logs {
                        tx_bloom.accrue(log.address.as_bytes());
                        for topic in &log.topics {
                            tx_bloom.accrue(topic.as_bytes());
                        }
                    }
                    receipts.push((
                        tx_hash,
                        Receipt {
                            tx_type,
                            status: summary.success,
                            cumulative_gas_used,
                            logs_bloom: tx_bloom,
                            logs: summary.logs,
                        },
                    ));
                }
                Err(e) => {
                    tracing::warn!(
                        block = block.header.number,
                        sender = ?sender,
                        error = %e,
                        "execute_with_revm failed — skipping tx",
                    );
                }
            }
        }
        // Mint block subsidy to the beneficiary. The halving curve is
        // controlled by ChainSpec; testnets can disable halving by
        // setting `block_reward_halving_interval = u64::MAX`.
        let subsidy_wei = self.spec.block_reward_at(block.header.number);
        if subsidy_wei > 0 {
            self.credit(&block.header.beneficiary, U256::from(subsidy_wei));
        }
        self.persist_receipts(block.hash(), &receipts);
        // Compute + persist Yellow-Paper sidecar roots so light
        // clients can validate the block's post-execution state.
        // (Header itself still carries placeholders to keep block
        // hashes stable across this release.)
        let receipts_only: Vec<aii_block::Receipt> =
            receipts.iter().map(|(_, r)| r.clone()).collect();
        let state_root = self.state.state_root().unwrap_or(EMPTY_TRIE_HASH);
        let receipts_root = aii_state::receipts_root(&receipts_only);
        let mut block_bloom = Bloom::ZERO;
        for (_, r) in &receipts {
            for log in &r.logs {
                block_bloom.accrue(log.address.as_bytes());
                for topic in &log.topics {
                    block_bloom.accrue(topic.as_bytes());
                }
            }
        }
        self.persist_post_roots(
            block.hash(),
            &PostBlockRoots {
                state_root,
                receipts_root,
                logs_bloom: block_bloom,
            },
        );
    }

    /// Persist the post-block roots sidecar for `block_hash`.
    fn persist_post_roots(&self, block_hash: H256, roots: &PostBlockRoots) {
        let mut key = Vec::with_capacity(META_KEY_POSTROOT_PREFIX.len() + 32);
        key.extend_from_slice(META_KEY_POSTROOT_PREFIX);
        key.extend_from_slice(block_hash.as_bytes());
        let mut val = Vec::with_capacity(32 + 32 + 256);
        val.extend_from_slice(roots.state_root.as_bytes());
        val.extend_from_slice(roots.receipts_root.as_bytes());
        val.extend_from_slice(&roots.logs_bloom.0);
        if let Err(e) = self.backend.put(ColumnFamily::Meta, &key, &val) {
            tracing::error!(error = %e, "persist_post_roots: write failed");
        }
    }

    /// Read back the post-block roots for `block_hash`, or `Ok(None)`
    /// if no record exists (e.g. block produced before v0.0.58).
    ///
    /// # Errors
    /// Propagates backend errors.
    pub fn post_roots(
        &self,
        block_hash: H256,
    ) -> Result<Option<PostBlockRoots>, Box<dyn std::error::Error + Send + Sync>> {
        let mut key = Vec::with_capacity(META_KEY_POSTROOT_PREFIX.len() + 32);
        key.extend_from_slice(META_KEY_POSTROOT_PREFIX);
        key.extend_from_slice(block_hash.as_bytes());
        let Some(bytes) = self.backend.get(ColumnFamily::Meta, &key)? else {
            return Ok(None);
        };
        if bytes.len() != 32 + 32 + 256 {
            return Ok(None);
        }
        let mut sr = [0u8; 32];
        sr.copy_from_slice(&bytes[..32]);
        let mut rr = [0u8; 32];
        rr.copy_from_slice(&bytes[32..64]);
        let mut bloom = [0u8; 256];
        bloom.copy_from_slice(&bytes[64..320]);
        Ok(Some(PostBlockRoots {
            state_root: H256::new(sr),
            receipts_root: H256::new(rr),
            logs_bloom: Bloom(bloom),
        }))
    }

    /// Add `delta` Wei to the balance of `addr`. Used by the gas-fee
    /// credit path and the block-subsidy mint path inside
    /// `execute_block_txs`. Idempotent under arithmetic — saturates
    /// rather than wrapping to avoid silent overflow.
    fn credit(&self, addr: &Address, delta: U256) {
        let mut acc = self
            .state
            .account(addr)
            .ok()
            .flatten()
            .unwrap_or(aii_state::Account::EMPTY);
        acc.balance = acc.balance.saturating_add(delta);
        if let Err(e) = self.state.set_account(addr, &acc) {
            tracing::error!(
                addr = ?addr,
                error = %e,
                "credit: set_account failed",
            );
        }
    }

    /// Persist every receipt produced for `block_hash` into the
    /// `Receipts` CF (keyed by tx_hash). Writes via a single
    /// `WriteBatch` so partial failure can't leave an inconsistent
    /// index.
    fn persist_receipts(&self, _block_hash: H256, receipts: &[(H256, Receipt)]) {
        if receipts.is_empty() {
            return;
        }
        let mut wb = WriteBatch::new();
        for (tx_hash, r) in receipts {
            let mut buf = alloy_rlp::bytes::BytesMut::new();
            r.encode_2718(&mut buf);
            wb.put(ColumnFamily::Receipts, tx_hash.as_bytes(), &buf);
        }
        if let Err(e) = self.backend.write(wb) {
            tracing::error!(
                count = receipts.len(),
                error = %e,
                "persist receipts: WriteBatch failed",
            );
        }
    }

    /// Persist a slashing record produced by the BFT equivocation
    /// detector. The record is stored under
    /// `Meta:slash:<vidx>:<height>:<phase>`; duplicate records for
    /// the same `(validator, height, phase)` triple overwrite (idempotent).
    ///
    /// Future releases will, in addition to recording the slash, debit
    /// the offending validator's staked balance — that requires the
    /// DPoS stake table from C.6 / E.3. For now the record itself is
    /// the slashing primitive.
    /// Optional slash-debit hook. When invoked together with
    /// [`Self::record_slashing`], this debits the offending
    /// validator's bond by `slash_amount_wei` (saturating at zero).
    /// Returns silently if the validator has no stake record (e.g.
    /// during a testnet where staking isn't wired yet).
    pub fn debit_slash_stake(&self, offender: &Address, slash_amount_wei: U256) {
        let table = self.stake_table();
        if let Ok(Some(_)) = table.get(offender) {
            if let Err(e) = table.slash(offender, slash_amount_wei) {
                tracing::error!(error = %e, "slash debit failed");
            }
        }
    }

    /// Append an equivocation record to the slashing index. Idempotent
    /// on the same `(validator, height, phase)` triple.
    pub fn record_slashing(&self, evidence: &aii_consensus_bft::EquivocationEvidence) {
        use aii_consensus_bft::EquivocationEvidence;
        let (phase_byte, phase_str, hashes) = match evidence {
            EquivocationEvidence::Prevote { conflicting } => (
                0u8,
                "prevote",
                [conflicting[0].block_hash, conflicting[1].block_hash],
            ),
            EquivocationEvidence::Precommit { conflicting } => (
                1u8,
                "precommit",
                [conflicting[0].block_hash, conflicting[1].block_hash],
            ),
        };
        let mut key = Vec::with_capacity(META_KEY_SLASH_PREFIX.len() + 4 + 8 + 1);
        key.extend_from_slice(META_KEY_SLASH_PREFIX);
        key.extend_from_slice(&evidence.validator_index().to_be_bytes());
        key.extend_from_slice(&evidence.height().to_be_bytes());
        key.push(phase_byte);

        // Value: `phase_str_len_be1 ‖ phase_str ‖ hash0[32] ‖ hash1[32]`.
        let mut val = Vec::with_capacity(1 + phase_str.len() + 64);
        val.push(phase_str.len() as u8);
        val.extend_from_slice(phase_str.as_bytes());
        val.extend_from_slice(hashes[0].as_bytes());
        val.extend_from_slice(hashes[1].as_bytes());
        if let Err(e) = self.backend.put(ColumnFamily::Meta, &key, &val) {
            tracing::error!(error = %e, "record_slashing: write failed");
        }
    }

    /// Return every persisted slashing record across the whole chain.
    /// Used by `aii_listSlashings` RPC and ops tooling.
    ///
    /// # Errors
    /// Propagates backend errors during the prefix scan.
    pub fn list_slashings(
        &self,
    ) -> Result<Vec<SlashRecord>, Box<dyn std::error::Error + Send + Sync>> {
        let mut out = Vec::new();
        for kv in self
            .backend
            .iter_prefix(ColumnFamily::Meta, META_KEY_SLASH_PREFIX)
        {
            let (k, v) = kv?;
            let suffix = &k[META_KEY_SLASH_PREFIX.len()..];
            if suffix.len() != 4 + 8 + 1 {
                continue;
            }
            let mut vidx_arr = [0u8; 4];
            vidx_arr.copy_from_slice(&suffix[..4]);
            let validator_index = u32::from_be_bytes(vidx_arr);
            let mut h_arr = [0u8; 8];
            h_arr.copy_from_slice(&suffix[4..12]);
            let height = u64::from_be_bytes(h_arr);
            let phase_byte = suffix[12];
            let phase = if phase_byte == 0 {
                "prevote"
            } else {
                "precommit"
            };
            if v.len() < 1 + 64 {
                continue;
            }
            let plen = v[0] as usize;
            if v.len() < 1 + plen + 64 {
                continue;
            }
            let hash_bytes = &v[1 + plen..];
            let mut h0 = [0u8; 32];
            let mut h1 = [0u8; 32];
            h0.copy_from_slice(&hash_bytes[..32]);
            h1.copy_from_slice(&hash_bytes[32..64]);
            out.push(SlashRecord {
                validator_index,
                height,
                phase,
                block_hashes: [H256::new(h0), H256::new(h1)],
            });
        }
        Ok(out)
    }

    /// Look up the receipt for a tx by its keccak256 hash. Returns
    /// `Ok(None)` if no receipt is on file (e.g. the tx is unknown,
    /// was rejected pre-execution, or pre-dates the receipt index).
    ///
    /// # Errors
    /// Propagates backend / decode failures.
    pub fn receipt_by_tx_hash(
        &self,
        tx_hash: H256,
    ) -> Result<Option<Receipt>, Box<dyn std::error::Error + Send + Sync>> {
        let Some(bytes) = self
            .backend
            .get(ColumnFamily::Receipts, tx_hash.as_bytes())?
        else {
            return Ok(None);
        };
        let mut s: &[u8] = &bytes;
        let r = Receipt::decode_2718(&mut s)?;
        Ok(Some(r))
    }

    /// Total number of indexed blocks (test-only diagnostic).
    #[must_use]
    pub fn block_count(&self) -> usize {
        self.blocks.read().map_or(0, |s| s.order.len())
    }

    /// Synchronous read of the head block number — used by startup
    /// logging where the async trait method would force a runtime.
    #[must_use]
    pub fn head_block_number_sync(&self) -> u64 {
        self.head.load(Ordering::Relaxed)
    }

    /// Record the runtime data directory (v0.0.76).
    ///
    /// Called by `aiid` early in `main()` so the
    /// release-binary-store helpers can resolve
    /// `<data-dir>/releases/<version>` paths. Calling this on a
    /// node that already has a data-dir set overwrites the
    /// previous value (idempotent for the common "set once at
    /// startup" call site).
    pub fn set_data_dir(&self, dir: std::path::PathBuf) {
        if let Ok(mut g) = self.data_dir.write() {
            *g = Some(dir);
        }
    }

    /// Record the list of peer HTTP-RPC URLs used for v0.0.77
    /// release-manifest propagation + binary auto-fetch.
    ///
    /// `aiid --update-peers HTTP1,HTTP2,…` calls this once at
    /// startup. Passing an empty `Vec` disables outbound
    /// propagation (the node still accepts announcements but
    /// doesn't re-broadcast).
    pub fn set_update_peers(&self, peers: Vec<String>) {
        if let Ok(mut g) = self.update_peers.write() {
            *g = peers;
        }
    }

    /// Read the current update-peer list. Returns an empty vec on
    /// a node that hasn't called [`Self::set_update_peers`].
    #[must_use]
    pub fn update_peers(&self) -> Vec<String> {
        self.update_peers
            .read()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Toggle automatic in-place upgrade on release acceptance
    /// (v0.0.78).
    ///
    /// When `true`, [`RpcState::record_release_announcement`]
    /// schedules an atomic install + execve self-restart as soon
    /// as the version's binary lands in
    /// `<data-dir>/releases/<version>`. Operator opt-in via
    /// `aiid --auto-install-releases`.
    pub fn set_auto_install_releases(&self, on: bool) {
        self.auto_install_releases.store(on, Ordering::Relaxed);
    }

    /// Read the auto-install flag. Default `false` on a node that
    /// hasn't called [`Self::set_auto_install_releases`].
    #[must_use]
    pub fn auto_install_releases(&self) -> bool {
        self.auto_install_releases.load(Ordering::Relaxed)
    }

    /// Test-only: redirect the v0.0.78 install target away from
    /// `/proc/self/exe` to `path`, and suppress the `execve`
    /// self-restart task. Used by `aii-node` integration tests
    /// so they can exercise install without overwriting the
    /// test runner. Calling this in production is a footgun —
    /// the manual restart path is gone, so an in-place upgrade
    /// would write the new binary to `path` and never restart.
    #[doc(hidden)]
    pub fn set_install_target_for_tests(&self, path: std::path::PathBuf) {
        if let Ok(mut g) = self.install_target_override.write() {
            *g = Some(path);
        }
    }

    /// Trigger the v0.0.78 auto-install path iff the conditions
    /// are met: auto-install is on, `data_dir` is known, the
    /// binary for `version` is cached locally, and the locally-
    /// known latest manifest matches `version`. Returns silently
    /// when any condition fails — fail-closed by design, since
    /// the operator opted in.
    ///
    /// Invoked from `record_release_announcement` and
    /// `import_release_binary` so the auto-install fires the
    /// moment both (manifest, binary) are in hand, regardless of
    /// which arrived first.
    #[cfg(unix)]
    async fn maybe_auto_install_release(&self, version: &str) {
        if !self.auto_install_releases() {
            return;
        }
        let Some(dir) = self.data_dir.read().ok().and_then(|g| g.clone()) else {
            return;
        };
        if !crate::release_store::binary_path(&dir, version).exists() {
            return;
        }
        let manifest = self.latest_release.read().ok().and_then(|g| g.clone());
        let Some(m) = manifest else {
            return;
        };
        if m.version != version {
            return;
        }
        tracing::info!(
            version = version,
            "auto-install conditions met; invoking install_release",
        );
        let outcome = <Self as aii_rpc::RpcState>::install_release(self, version).await;
        tracing::info!(
            scheduled = outcome.scheduled,
            reason = %outcome.reason,
            restart_in_secs = outcome.restart_in_secs,
            "auto-install result",
        );
    }

    /// Read the full [`Block`] at height `n`, reconstructed from the
    /// in-memory `by_number → hash → header / body` indices.
    ///
    /// Added in v0.0.70 so the startup path can pass the recovered
    /// head block into [`aii_consensus_bft::BftEngine::from_recovered`]
    /// — letting the BFT engine resume at `n+1` instead of starting
    /// over from genesis.
    #[must_use]
    pub fn block_by_number(&self, n: u64) -> Option<Block> {
        let guard = self.blocks.read().ok()?;
        let hash = guard.by_number.get(&n).copied()?;
        let header = guard.by_hash.get(&hash).cloned()?;
        let body = guard.body_by_hash.get(&hash).cloned().unwrap_or_default();
        Some(Block { header, body })
    }

    /// Convenience wrapper: full [`Block`] at the current head.
    /// Returns `None` if the chain is empty or the head index is
    /// inconsistent.
    #[must_use]
    pub fn head_block(&self) -> Option<Block> {
        let n = self.head_block_number_sync();
        self.block_by_number(n)
    }

    /// Test-only: peek the in-memory `number → hash` map. Used by the
    /// cold-join sync test to verify byte-identical block
    /// reconstruction across producer/consumer pairs.
    #[doc(hidden)]
    #[must_use]
    pub fn blocks_read_test_hash_by_number(&self, n: u64) -> Option<H256> {
        self.blocks
            .read()
            .ok()
            .and_then(|s| s.by_number.get(&n).copied())
    }

    /// Test-only sync accessor for the latest persisted validator set.
    #[doc(hidden)]
    #[must_use]
    pub fn async_active_validator_set_test_helper(&self) -> Option<(u64, Vec<ValidatorEntry>)> {
        latest_validator_set(&self.backend).ok().flatten()
    }

    /// Borrow the world-state for embedders who want to read/write accounts
    /// directly (e.g. apply a genesis allocation).
    pub const fn state(&self) -> &Arc<StateDb<RocksDbBackend>> {
        &self.state
    }

    /// Borrow the underlying backend (used by sub-systems that index
    /// data in column families outside the `state` keyset).
    #[must_use]
    pub fn backend(&self) -> Arc<RocksDbBackend> {
        Arc::clone(&self.backend)
    }

    /// Construct a fresh `StakeTable` view bound to this node's
    /// backend. Cheap — the inner `RocksDbBackend` Arc is shared.
    #[must_use]
    pub fn stake_table(&self) -> StakeTable {
        StakeTable::new(Arc::clone(&self.backend))
    }

    /// Construct a fresh `Governance` view bound to this node's
    /// backend. Cheap — the inner `RocksDbBackend` Arc is shared.
    #[must_use]
    pub fn governance(&self) -> Governance {
        Governance::new(Arc::clone(&self.backend))
    }
}

/// `"n:" ‖ number_be8` — key form used in the `Meta` CF for the
/// `number → block_hash` reverse map. Kept tiny and prefix-distinct from
/// the head markers so the keyspace stays scannable.
fn number_key(n: u64) -> Vec<u8> {
    let mut k = Vec::with_capacity(10);
    k.extend_from_slice(b"n:");
    k.extend_from_slice(&n.to_be_bytes());
    k
}

fn proposal_to_view(p: &Proposal, tally: Option<(U256, U256)>) -> ProposalView {
    let (yes, no) = tally.unwrap_or((U256::ZERO, U256::ZERO));
    ProposalView {
        id: format!("0x{:x}", p.id),
        title: p.title.clone(),
        voting_ends_at: format!("0x{:x}", p.voting_ends_at),
        status: match p.status {
            ProposalStatus::Pending => "pending".into(),
            ProposalStatus::Passed => "passed".into(),
            ProposalStatus::Rejected => "rejected".into(),
            ProposalStatus::Executed => "executed".into(),
        },
        proposer: format!("0x{}", hex::encode(p.proposer.as_bytes())),
        yes_wei: format!("0x{yes:x}"),
        no_wei: format!("0x{no:x}"),
    }
}

fn stake_record_to_view(r: &StakeRecord) -> StakeView {
    StakeView {
        address: format!("0x{}", hex::encode(r.staker.as_bytes())),
        amount_wei: format!("0x{:x}", r.amount_wei),
        unbond_at: format!("0x{:x}", r.unbond_at),
        is_bonded: r.is_bonded(),
    }
}

fn header_to_view(hash: H256, h: &Header) -> HeaderView {
    HeaderView {
        hash: format!("0x{}", hex::encode(hash.as_bytes())),
        parent_hash: format!("0x{}", hex::encode(h.parent_hash.as_bytes())),
        number: format!("0x{:x}", h.number),
        timestamp: format!("0x{:x}", h.timestamp),
        beneficiary: format!("0x{}", hex::encode(h.beneficiary.as_bytes())),
        gas_limit: format!("0x{:x}", h.gas_limit),
        gas_used: format!("0x{:x}", h.gas_used),
        base_fee_per_gas: format!("0x{:x}", h.base_fee_per_gas),
        state_root: format!("0x{}", hex::encode(h.state_root.as_bytes())),
        transactions_root: format!("0x{}", hex::encode(h.transactions_root.as_bytes())),
        receipts_root: format!("0x{}", hex::encode(h.receipts_root.as_bytes())),
        mix_hash: format!("0x{}", hex::encode(h.mix_hash.as_bytes())),
        extra_data_hex: format!("0x{}", hex::encode(&h.extra_data)),
    }
}

fn parse_block_tag(s: &str) -> Option<u64> {
    let trimmed = s.trim();
    if let Some(stripped) = trimmed.strip_prefix("0x") {
        u64::from_str_radix(stripped, 16).ok()
    } else {
        trimmed.parse::<u64>().ok()
    }
}

fn parse_hash_str(s: &str) -> Option<H256> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if s.len() != 64 {
        return None;
    }
    let mut bytes = [0u8; 32];
    hex::decode_to_slice(s, &mut bytes).ok()?;
    Some(H256::new(bytes))
}

fn parse_hash_h256(s: &str) -> Option<H256> {
    parse_hash_str(s)
}

fn tx_to_view(tx: &Tx, chain_id: u64) -> TxView {
    use aii_block::tx::{TxEip1559, TxEip4844, TxLegacy};
    let hash = format!("0x{}", hex::encode(tx.hash().as_bytes()));
    let from = tx
        .recover_signer(chain_id)
        .map(|a| format!("0x{}", hex::encode(a.as_bytes())))
        .unwrap_or_default();
    let (to_opt, value, nonce, gas_limit, max_fee, max_pri, tx_type) = match tx {
        Tx::Legacy(TxLegacy {
            nonce,
            gas_price,
            gas_limit,
            to,
            value,
            ..
        }) => (
            *to, *value, *nonce, *gas_limit, *gas_price, *gas_price, "legacy",
        ),
        Tx::Eip1559(TxEip1559 {
            nonce,
            max_priority_fee_per_gas,
            max_fee_per_gas,
            gas_limit,
            to,
            value,
            ..
        }) => (
            *to,
            *value,
            *nonce,
            *gas_limit,
            *max_fee_per_gas,
            *max_priority_fee_per_gas,
            "eip1559",
        ),
        Tx::Eip4844(TxEip4844 {
            nonce,
            max_priority_fee_per_gas,
            max_fee_per_gas,
            gas_limit,
            to,
            value,
            ..
        }) => (
            Some(*to),
            *value,
            *nonce,
            *gas_limit,
            *max_fee_per_gas,
            *max_priority_fee_per_gas,
            "eip4844",
        ),
    };
    let to = to_opt
        .map(|a| format!("0x{}", hex::encode(a.as_bytes())))
        .unwrap_or_default();
    TxView {
        hash,
        from,
        to,
        value: format!("0x{value:x}"),
        nonce: format!("0x{nonce:x}"),
        gas_limit: format!("0x{gas_limit:x}"),
        max_fee_per_gas: format!("0x{max_fee:x}"),
        max_priority_fee_per_gas: format!("0x{max_pri:x}"),
        tx_type: tx_type.to_string(),
    }
}

#[async_trait]
impl RpcState for NodeState {
    fn chain_id(&self) -> u64 {
        self.spec.chain_id
    }

    fn network(&self) -> String {
        self.spec.network.clone()
    }

    async fn head_block_number(&self) -> u64 {
        self.head.load(Ordering::Relaxed)
    }

    fn gas_price(&self) -> U256 {
        U256::from(self.spec.min_base_fee_per_gas)
    }

    async fn header_by_number(&self, n: u64) -> Option<HeaderView> {
        let s = self.blocks.read().ok()?;
        let hash = *s.by_number.get(&n)?;
        let h = s.by_hash.get(&hash)?;
        Some(header_to_view(hash, h))
    }

    async fn header_by_hash(&self, hash_hex: &str) -> Option<HeaderView> {
        let hash = parse_hash_str(hash_hex)?;
        let s = self.blocks.read().ok()?;
        let h = s.by_hash.get(&hash)?;
        Some(header_to_view(hash, h))
    }

    async fn recent_headers(&self, limit: usize) -> Vec<HeaderView> {
        let Ok(s) = self.blocks.read() else {
            return Vec::new();
        };
        s.order
            .iter()
            .rev()
            .take(limit)
            .filter_map(|h| s.by_hash.get(h).map(|hdr| header_to_view(*h, hdr)))
            .collect()
    }

    async fn block_transactions(&self, n: u64) -> Option<Vec<TxView>> {
        let chain_id = self.spec.chain_id;
        let Ok(s) = self.blocks.read() else {
            return None;
        };
        let hash = *s.by_number.get(&n)?;
        let body = s.body_by_hash.get(&hash)?;
        Some(
            body.transactions
                .iter()
                .map(|tx| tx_to_view(tx, chain_id))
                .collect(),
        )
    }

    async fn receipt_by_tx_hash(&self, hash_hex: &str) -> Option<ReceiptView> {
        let h = parse_hash_h256(hash_hex)?;
        let receipt = self.receipt_by_tx_hash(h).ok()??;
        let block_number = self
            .blocks
            .read()
            .ok()
            .and_then(|s| s.tx_index.get(&h).map(|(n, _)| *n));
        Some(ReceiptView {
            transaction_hash: format!("0x{}", hex::encode(h.as_bytes())),
            block_number: block_number.map_or_else(|| "0x0".to_string(), |n| format!("0x{n:x}")),
            status: if receipt.status {
                "0x1".into()
            } else {
                "0x0".into()
            },
            cumulative_gas_used: format!("0x{:x}", receipt.cumulative_gas_used),
            logs_bloom: format!("0x{}", hex::encode(receipt.logs_bloom.0)),
            tx_type: match receipt.tx_type {
                TxType::Legacy => "legacy".into(),
                TxType::Eip1559 => "eip1559".into(),
                TxType::Eip4844 => "eip4844".into(),
            },
            logs: receipt
                .logs
                .into_iter()
                .map(|l| LogView {
                    address: format!("0x{}", hex::encode(l.address.as_bytes())),
                    topics: l
                        .topics
                        .into_iter()
                        .map(|t| format!("0x{}", hex::encode(t.as_bytes())))
                        .collect(),
                    data: format!("0x{}", hex::encode(l.data)),
                })
                .collect(),
        })
    }

    async fn transaction_by_hash(&self, hash_hex: &str) -> Option<(TxView, u64)> {
        let chain_id = self.spec.chain_id;
        let h = parse_hash_h256(hash_hex)?;
        let Ok(s) = self.blocks.read() else {
            return None;
        };
        let (block_number, idx) = *s.tx_index.get(&h)?;
        let block_hash = *s.by_number.get(&block_number)?;
        let body = s.body_by_hash.get(&block_hash)?;
        let tx = body.transactions.get(idx)?;
        Some((tx_to_view(tx, chain_id), block_number))
    }

    async fn logs_in_range(&self, filter: &LogFilter) -> Vec<LogEntryView> {
        let head = self.head.load(Ordering::Relaxed);
        let from = filter
            .from_block
            .as_deref()
            .map_or(Some(0), parse_block_tag)
            .unwrap_or(0);
        let to = filter
            .to_block
            .as_deref()
            .map_or(Some(head), parse_block_tag)
            .unwrap_or(head);
        let to = to.min(head);
        if from > to {
            return Vec::new();
        }
        let want_addr: Option<Address> = filter.address.as_deref().and_then(|s| {
            let s = s.strip_prefix("0x").unwrap_or(s);
            if s.len() != 40 {
                return None;
            }
            let mut bytes = [0u8; 20];
            hex::decode_to_slice(s, &mut bytes).ok()?;
            Some(Address::new(bytes))
        });
        let want_topics: Vec<H256> = filter
            .topics
            .iter()
            .filter_map(|s| {
                let s = s.strip_prefix("0x").unwrap_or(s);
                if s.len() != 64 {
                    return None;
                }
                let mut bytes = [0u8; 32];
                hex::decode_to_slice(s, &mut bytes).ok()?;
                Some(H256::new(bytes))
            })
            .collect();
        let mut out = Vec::new();
        for n in from..=to {
            let Some(s) = self.blocks.read().ok() else {
                break;
            };
            let Some(block_hash) = s.by_number.get(&n).copied() else {
                drop(s);
                continue;
            };
            let body = s.body_by_hash.get(&block_hash).cloned();
            drop(s);
            // Bloom prefilter on the block-level bloom from post-roots.
            if let Ok(Some(roots)) = self.post_roots(block_hash) {
                let addr_miss = want_addr
                    .as_ref()
                    .is_some_and(|a| !roots.logs_bloom.contains(a.as_bytes()));
                let topic_miss = want_topics
                    .iter()
                    .any(|t| !roots.logs_bloom.contains(t.as_bytes()));
                if addr_miss || topic_miss {
                    continue;
                }
            }
            // Walk the block's receipts via tx_index.
            let Some(body) = body else { continue };
            for tx in &body.transactions {
                let tx_hash = tx.hash();
                let Ok(Some(receipt)) = self.receipt_by_tx_hash(tx_hash) else {
                    continue;
                };
                for log in &receipt.logs {
                    if let Some(addr) = want_addr {
                        if log.address != addr {
                            continue;
                        }
                    }
                    let topics_ok = want_topics
                        .iter()
                        .all(|t| log.topics.iter().any(|lt| lt == t));
                    if !topics_ok {
                        continue;
                    }
                    out.push(LogEntryView {
                        block_number: format!("0x{n:x}"),
                        transaction_hash: format!("0x{}", hex::encode(tx_hash.as_bytes())),
                        address: format!("0x{}", hex::encode(log.address.as_bytes())),
                        topics: log
                            .topics
                            .iter()
                            .map(|t| format!("0x{}", hex::encode(t.as_bytes())))
                            .collect(),
                        data: format!("0x{}", hex::encode(&log.data)),
                    });
                }
            }
        }
        out
    }

    async fn post_roots_for(&self, block_hash_hex: &str) -> Option<PostRootsView> {
        let h = parse_hash_str(block_hash_hex)?;
        let r = self.post_roots(h).ok().flatten()?;
        Some(PostRootsView {
            state_root: format!("0x{}", hex::encode(r.state_root.as_bytes())),
            receipts_root: format!("0x{}", hex::encode(r.receipts_root.as_bytes())),
            logs_bloom: format!("0x{}", hex::encode(r.logs_bloom.0)),
        })
    }

    async fn forks(&self) -> Vec<ForkView> {
        self.list_forks()
            .unwrap_or_default()
            .into_iter()
            .map(|f| ForkView {
                height: format!("0x{:x}", f.height),
                canonical_hash: format!("0x{}", hex::encode(f.canonical_hash.as_bytes())),
                fork_hash: format!("0x{}", hex::encode(f.fork_hash.as_bytes())),
            })
            .collect()
    }

    async fn governance_proposals(&self) -> Vec<ProposalView> {
        let gov = self.governance();
        gov.list_all()
            .unwrap_or_default()
            .into_iter()
            .map(|p| proposal_to_view(&p, gov.tally_of(p.id).ok().flatten()))
            .collect()
    }

    async fn governance_proposal(&self, id: u64) -> Option<ProposalView> {
        let gov = self.governance();
        let p = gov.get(id).ok().flatten()?;
        Some(proposal_to_view(&p, gov.tally_of(p.id).ok().flatten()))
    }

    async fn active_validator_set(&self) -> Option<ActiveValidatorsView> {
        let (epoch, entries) = latest_validator_set(&self.backend).ok().flatten()?;
        Some(ActiveValidatorsView {
            epoch: format!("0x{epoch:x}"),
            validators: entries
                .iter()
                .map(|e| ValidatorEntryView {
                    address: format!("0x{}", hex::encode(e.address.as_bytes())),
                    stake_wei: format!("0x{:x}", e.stake_wei),
                })
                .collect(),
        })
    }

    async fn stake_at(&self, address: &Address) -> Option<StakeView> {
        let table = self.stake_table();
        let rec = table.get(address).ok().flatten()?;
        Some(stake_record_to_view(&rec))
    }

    async fn total_bonded_stake(&self) -> U256 {
        self.stake_table().total_bonded().unwrap_or(U256::ZERO)
    }

    async fn all_stakers(&self) -> Vec<StakeView> {
        self.stake_table()
            .list_all()
            .unwrap_or_default()
            .iter()
            .map(stake_record_to_view)
            .collect()
    }

    async fn subchain_anchor(&self, id: u32) -> Option<SubchainAnchorView> {
        let anchor = self
            .last_flush_anchor(aii_microchain::MicroChainId(id))
            .ok()
            .flatten()?;
        Some(SubchainAnchorView {
            sub_block_hash: format!("0x{}", hex::encode(anchor.sub_block_hash.as_bytes())),
            parent_block_hash: format!("0x{}", hex::encode(anchor.parent_block_hash.as_bytes())),
            sub_block_number: format!("0x{:x}", anchor.sub_block_number),
        })
    }

    async fn slashings(&self) -> Vec<SlashView> {
        self.list_slashings()
            .ok()
            .unwrap_or_default()
            .into_iter()
            .map(|r| SlashView {
                validator_index: r.validator_index,
                height: format!("0x{:x}", r.height),
                phase: r.phase.to_string(),
                block_hashes: [
                    format!("0x{}", hex::encode(r.block_hashes[0].as_bytes())),
                    format!("0x{}", hex::encode(r.block_hashes[1].as_bytes())),
                ],
            })
            .collect()
    }

    async fn raw_block(&self, query: &str) -> Option<String> {
        let s = self.blocks.read().ok()?;
        let hash = if let Ok(n) = query.parse::<u64>() {
            *s.by_number.get(&n)?
        } else if let Some(stripped) = query.strip_prefix("0x") {
            if let Ok(n) = u64::from_str_radix(stripped, 16) {
                *s.by_number.get(&n)?
            } else {
                parse_hash_str(query)?
            }
        } else {
            parse_hash_str(query)?
        };
        let header = s.by_hash.get(&hash)?.clone();
        let body = s.body_by_hash.get(&hash)?.clone();
        drop(s);
        let block = Block { header, body };
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        block.encode(&mut buf);
        Some(format!("0x{}", hex::encode(&buf)))
    }

    async fn submit_raw_tx(&self, raw_hex: &str) -> Result<String, SubmitTxError> {
        let s = raw_hex.strip_prefix("0x").unwrap_or(raw_hex);
        let bytes = hex::decode(s).map_err(|e| SubmitTxError::Hex(format!("hex decode: {e}")))?;
        if bytes.is_empty() {
            return Err(SubmitTxError::Decode("empty body".into()));
        }
        let mut buf: &[u8] = &bytes;
        // EIP-2718: a leading byte < 0xc0 selects the envelope; >= 0xc0
        // is the start of an RLP list (legacy).
        let tx = if bytes[0] < 0xc0 {
            Tx::decode_2718(&mut buf)
                .map_err(|e| SubmitTxError::Decode(format!("EIP-2718: {e}")))?
        } else {
            let mut buf: &[u8] = &bytes;
            let legacy = aii_block::tx::TxLegacy::decode(&mut buf)
                .map_err(|e| SubmitTxError::Decode(format!("legacy RLP: {e}")))?;
            Tx::Legacy(legacy)
        };
        let chain_id = self.spec.chain_id;
        let sender = tx
            .recover_signer(chain_id)
            .map_err(|e| SubmitTxError::Signer(e.to_string()))?;
        let nonce = match &tx {
            Tx::Legacy(t) => t.nonce,
            Tx::Eip1559(t) => t.nonce,
            Tx::Eip4844(t) => t.nonce,
        };
        let gas_price = effective_gas_price(&tx);
        let tx_hash = tx.hash();
        let entry = PoolEntry {
            sender,
            nonce,
            effective_gas_price: gas_price,
            tx,
        };
        match self.tx_pool.add(entry) {
            Ok(AddOutcome::Inserted | AddOutcome::Replaced(_)) => {
                Ok(format!("0x{}", hex::encode(tx_hash.as_bytes())))
            }
            Ok(AddOutcome::RejectedUnderpriced) => Err(SubmitTxError::Pool(
                "rejected: same-nonce tx with equal/lower gas price already in pool".into(),
            )),
            Err(e) => Err(SubmitTxError::Pool(e.to_string())),
        }
    }

    async fn account(&self, addr: &Address) -> Option<AccountView> {
        let acc = self.state.account(addr).ok().flatten()?;
        Some(AccountView {
            nonce: acc.nonce,
            balance: format!("0x{:x}", acc.balance),
            storage_root: format!("0x{}", hex::encode(acc.storage_root.as_bytes())),
            code_hash: format!("0x{}", hex::encode(acc.code_hash.as_bytes())),
        })
    }

    async fn release_binary_bytes(&self, version: &str) -> Option<Vec<u8>> {
        let dir = self.data_dir.read().ok()?.clone()?;
        crate::release_store::load_binary(&dir, version)
            .ok()
            .flatten()
    }

    async fn import_release_binary(&self, version: &str, bytes: Vec<u8>) -> (bool, String) {
        let Some(dir) = self.data_dir.read().ok().and_then(|g| g.clone()) else {
            return (
                false,
                "node has no data_dir configured (set_data_dir not called)".into(),
            );
        };
        // Cross-check against the locally-known latest manifest.
        // We refuse to store a binary whose version doesn't match a
        // manifest we've already verified — otherwise a peer could
        // dump arbitrary bytes into our cache.
        let manifest = self.latest_release.read().ok().and_then(|g| g.clone());
        let Some(m) = manifest else {
            return (
                false,
                "no verified manifest known for any version yet".into(),
            );
        };
        if m.version != version {
            return (
                false,
                format!(
                    "version mismatch: latest known manifest is {} but import is for {}",
                    m.version, version
                ),
            );
        }
        // Hand off to the store, which re-verifies sha256 before writing.
        match crate::release_store::store_verified_binary(&dir, version, &m.sha256_hex, &bytes) {
            Ok(path) => {
                tracing::info!(
                    version = version,
                    path = %path.display(),
                    bytes = bytes.len(),
                    "imported release binary",
                );
                #[cfg(unix)]
                self.maybe_auto_install_release(version).await;
                (true, String::new())
            }
            Err(e) => (false, format!("{e}")),
        }
    }

    async fn record_release_announcement(
        &self,
        manifest: aii_crypto::release::ReleaseManifest,
    ) -> bool {
        let version_for_auto_install = {
            let Ok(mut guard) = self.latest_release.write() else {
                return false;
            };
            // Only accept a strictly newer manifest. Compare on
            // (timestamp_unix, version-string) — timestamp first so a
            // backdated re-sign of the same version cannot displace the
            // live one.
            if let Some(current) = guard.as_ref() {
                let strictly_newer = manifest.timestamp_unix > current.timestamp_unix
                    || (manifest.timestamp_unix == current.timestamp_unix
                        && manifest.version > current.version);
                if !strictly_newer {
                    return false;
                }
            }
            tracing::info!(
                version = %manifest.version,
                ts = manifest.timestamp_unix,
                "accepted release announcement",
            );
            let v = manifest.version.clone();
            *guard = Some(manifest);
            v
        };
        // Auto-install path (v0.0.78): if the binary for the
        // newly-accepted version already lives in our cache (e.g.
        // because gossip pushed it before the manifest landed),
        // fire the install + execve here. Lock guard above is
        // dropped first so install_release can re-read state.
        #[cfg(unix)]
        self.maybe_auto_install_release(&version_for_auto_install)
            .await;
        true
    }

    async fn latest_release(&self) -> Option<aii_crypto::release::ReleaseManifest> {
        self.latest_release.read().ok()?.clone()
    }

    async fn update_peers_for_release(&self) -> Vec<String> {
        self.update_peers()
    }

    #[cfg(unix)]
    async fn install_release(&self, version: &str) -> aii_rpc::InstallOutcome {
        const RESTART_DELAY_SECS: u64 = 2;

        let Some(dir) = self.data_dir.read().ok().and_then(|g| g.clone()) else {
            return aii_rpc::InstallOutcome {
                scheduled: false,
                reason: "node has no data_dir configured (set_data_dir not called)".into(),
                restart_in_secs: 0,
            };
        };
        let staged = crate::release_store::binary_path(&dir, version);
        if !staged.exists() {
            return aii_rpc::InstallOutcome {
                scheduled: false,
                reason: format!(
                    "no cached binary for version {version} at {} — run aii_importReleaseBinary first",
                    staged.display()
                ),
                restart_in_secs: 0,
            };
        }
        // Test-only path: when the override is set, install to
        // the override and skip execve so the test runner stays
        // alive.
        let override_target = self
            .install_target_override
            .read()
            .ok()
            .and_then(|g| g.clone());
        let target = if let Some(p) = override_target.clone() {
            p
        } else {
            match crate::release_install::current_aiid_path() {
                Ok(p) => p,
                Err(e) => {
                    return aii_rpc::InstallOutcome {
                        scheduled: false,
                        reason: format!("cannot resolve current_exe: {e}"),
                        restart_in_secs: 0,
                    };
                }
            }
        };
        if let Err(e) = crate::release_install::install_binary(&staged, &target) {
            return aii_rpc::InstallOutcome {
                scheduled: false,
                reason: format!("install failed: {e}"),
                restart_in_secs: 0,
            };
        }
        tracing::info!(
            version = version,
            target = %target.display(),
            restart_in_secs = RESTART_DELAY_SECS,
            test_mode = override_target.is_some(),
            "release installed; self-restart scheduled",
        );
        if override_target.is_none() {
            // IMPORTANT: pass `target` into the spawn instead of
            // re-resolving via current_exe() inside the closure.
            // After `rename(2)`, `/proc/self/exe` carries a
            // literal " (deleted)" suffix that `execve` rejects
            // with ENOENT — see `release_install::exec_self_at`
            // for the full rationale.
            let exec_target = target;
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(RESTART_DELAY_SECS)).await;
                let err = crate::release_install::exec_self_at(&exec_target);
                tracing::error!(
                    error = %err,
                    target = %exec_target.display(),
                    "execve self failed; node continues on old binary",
                );
            });
        }
        aii_rpc::InstallOutcome {
            scheduled: true,
            reason: String::new(),
            restart_in_secs: RESTART_DELAY_SECS,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aii_block::{BlockBody, Bloom, EMPTY_LIST_HASH, EMPTY_TRIE_HASH};
    use aii_state::Account;
    use jsonrpsee::core::client::ClientT;
    use jsonrpsee::http_client::HttpClientBuilder;
    use jsonrpsee::rpc_params;

    #[tokio::test]
    async fn end_to_end_chain_id_query() {
        let state = NodeState::new_for_tests(ChainSpec::mainnet());
        let (addr, handle) = aii_rpc::serve("127.0.0.1:0".parse().unwrap(), state.clone())
            .await
            .unwrap();
        let url = format!("http://{addr}");
        let client = HttpClientBuilder::default().build(url).unwrap();
        let chain_id: String = client.request("eth_chainId", rpc_params![]).await.unwrap();
        assert_eq!(chain_id, "0x63"); // 99
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn head_advances_on_set_head() {
        let state = NodeState::new_for_tests(ChainSpec::testnet());
        let (addr, handle) = aii_rpc::serve("127.0.0.1:0".parse().unwrap(), state.clone())
            .await
            .unwrap();
        let url = format!("http://{addr}");
        let client = HttpClientBuilder::default().build(url).unwrap();

        let initial: String = client
            .request("eth_blockNumber", rpc_params![])
            .await
            .unwrap();
        assert_eq!(initial, "0x0");

        state.set_head(42);
        let after: String = client
            .request("eth_blockNumber", rpc_params![])
            .await
            .unwrap();
        assert_eq!(after, "0x2a");

        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn aii_status_reports_correct_network() {
        let state = NodeState::new_for_tests(ChainSpec::testnet());
        let (addr, handle) = aii_rpc::serve("127.0.0.1:0".parse().unwrap(), state.clone())
            .await
            .unwrap();
        let url = format!("http://{addr}");
        let client = HttpClientBuilder::default().build(url).unwrap();
        let status: aii_rpc::AiiStatus = client.request("aii_status", rpc_params![]).await.unwrap();
        assert_eq!(status.network, "aii-testnet");
        assert_eq!(status.chain_id, aii_config::AII_TESTNET.chain_id);
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_get_balance_via_state_db() {
        let state = NodeState::new_for_tests(ChainSpec::mainnet());
        // Pre-populate Alice's account with 1 AII.
        let alice = Address::new([0xa1; 20]);
        let alice_acc = Account {
            nonce: 3,
            balance: U256::from(1_000_000_000_000_000_000u64),
            ..Account::EMPTY
        };
        state.state().set_account(&alice, &alice_acc).unwrap();

        let (addr, handle) = aii_rpc::serve("127.0.0.1:0".parse().unwrap(), state.clone())
            .await
            .unwrap();
        let url = format!("http://{addr}");
        let client = HttpClientBuilder::default().build(url).unwrap();

        let r: String = client
            .request(
                "eth_getBalance",
                rpc_params!["0xa1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1", "latest"],
            )
            .await
            .unwrap();
        assert_eq!(r, "0xde0b6b3a7640000"); // 1e18

        // Missing account returns 0
        let r0: String = client
            .request(
                "eth_getBalance",
                rpc_params!["0x0000000000000000000000000000000000000000", "latest"],
            )
            .await
            .unwrap();
        assert_eq!(r0, "0x0");

        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_gas_price_uses_chain_spec_floor() {
        let state = NodeState::new_for_tests(ChainSpec::mainnet()); // min_base_fee = 1e9
        let (addr, handle) = aii_rpc::serve("127.0.0.1:0".parse().unwrap(), state.clone())
            .await
            .unwrap();
        let url = format!("http://{addr}");
        let client = HttpClientBuilder::default().build(url).unwrap();
        let r: String = client.request("eth_gasPrice", rpc_params![]).await.unwrap();
        assert_eq!(r, "0x3b9aca00"); // 1_000_000_000
        handle.stop().unwrap();
    }

    fn fake_block(number: u64, parent_hash: H256) -> Block {
        Block {
            header: Header {
                parent_hash,
                ommers_hash: EMPTY_LIST_HASH,
                beneficiary: Address::new([0xcc; 20]),
                state_root: EMPTY_TRIE_HASH,
                transactions_root: EMPTY_TRIE_HASH,
                receipts_root: EMPTY_TRIE_HASH,
                logs_bloom: Bloom::ZERO,
                difficulty: U256::ZERO,
                number,
                gas_limit: 30_000_000,
                gas_used: 0,
                timestamp: 1_700_000_000 + number,
                extra_data: b"aii-test".to_vec(),
                mix_hash: H256::new([0xab; 32]),
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

    #[tokio::test]
    async fn commit_block_lookup_by_number_returns_header() {
        let state = NodeState::new_for_tests(ChainSpec::mainnet());
        let block = fake_block(1, H256::ZERO);
        state.commit_block(&block);
        assert_eq!(state.block_count(), 1);
        let view = state.header_by_number(1).await.unwrap();
        assert_eq!(view.number, "0x1");
        assert_eq!(
            view.beneficiary,
            format!("0x{}", hex::encode([0xcc_u8; 20]))
        );
    }

    #[tokio::test]
    async fn commit_block_lookup_by_hash_returns_header() {
        let state = NodeState::new_for_tests(ChainSpec::mainnet());
        let block = fake_block(42, H256::ZERO);
        let block_hash = block.hash();
        state.commit_block(&block);
        let hex_hash = format!("0x{}", hex::encode(block_hash.as_bytes()));
        let view = state.header_by_hash(&hex_hash).await.unwrap();
        assert_eq!(view.number, "0x2a");
    }

    #[tokio::test]
    async fn lookup_unknown_block_returns_none() {
        let state = NodeState::new_for_tests(ChainSpec::mainnet());
        assert!(state.header_by_number(99).await.is_none());
        assert!(state
            .header_by_hash("0x0000000000000000000000000000000000000000000000000000000000000000")
            .await
            .is_none());
    }

    #[tokio::test]
    async fn recent_headers_returns_newest_first_and_caps_at_limit() {
        let state = NodeState::new_for_tests(ChainSpec::mainnet());
        let mut parent = H256::ZERO;
        for n in 1..=5 {
            let b = fake_block(n, parent);
            parent = b.hash();
            state.commit_block(&b);
        }
        let recent = state.recent_headers(3).await;
        assert_eq!(recent.len(), 3);
        // Newest first.
        assert_eq!(recent[0].number, "0x5");
        assert_eq!(recent[1].number, "0x4");
        assert_eq!(recent[2].number, "0x3");
    }

    #[tokio::test]
    async fn aii_get_block_header_rpc_by_number() {
        let state = NodeState::new_for_tests(ChainSpec::mainnet());
        let b = fake_block(7, H256::ZERO);
        state.commit_block(&b);
        let (addr, handle) = aii_rpc::serve("127.0.0.1:0".parse().unwrap(), state.clone())
            .await
            .unwrap();
        let url = format!("http://{addr}");
        let client = HttpClientBuilder::default().build(url).unwrap();
        let r: Option<aii_rpc::HeaderView> = client
            .request("aii_getBlockHeader", rpc_params!["7"])
            .await
            .unwrap();
        let v = r.unwrap();
        assert_eq!(v.number, "0x7");
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn aii_get_block_header_rpc_by_hash() {
        let state = NodeState::new_for_tests(ChainSpec::mainnet());
        let b = fake_block(7, H256::ZERO);
        let h = b.hash();
        state.commit_block(&b);
        let (addr, handle) = aii_rpc::serve("127.0.0.1:0".parse().unwrap(), state.clone())
            .await
            .unwrap();
        let url = format!("http://{addr}");
        let client = HttpClientBuilder::default().build(url).unwrap();
        let hex_hash = format!("0x{}", hex::encode(h.as_bytes()));
        let r: Option<aii_rpc::HeaderView> = client
            .request("aii_getBlockHeader", rpc_params![hex_hash])
            .await
            .unwrap();
        assert_eq!(r.unwrap().number, "0x7");
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn aii_recent_blocks_rpc_caps_and_orders() {
        let state = NodeState::new_for_tests(ChainSpec::mainnet());
        let mut parent = H256::ZERO;
        for n in 1..=10 {
            let b = fake_block(n, parent);
            parent = b.hash();
            state.commit_block(&b);
        }
        let (addr, handle) = aii_rpc::serve("127.0.0.1:0".parse().unwrap(), state.clone())
            .await
            .unwrap();
        let url = format!("http://{addr}");
        let client = HttpClientBuilder::default().build(url).unwrap();
        let r: Vec<aii_rpc::HeaderView> = client
            .request("aii_recentBlocks", rpc_params![5u64])
            .await
            .unwrap();
        assert_eq!(r.len(), 5);
        assert_eq!(r[0].number, "0xa");
        assert_eq!(r[4].number, "0x6");
        handle.stop().unwrap();
    }

    #[test]
    fn end_to_end_stake_elect_govern_tally() {
        // Builds the full economic + governance loop on a tiny epoch:
        //   1. two stakers bond,
        //   2. commit blocks until an epoch boundary fires the election,
        //   3. the elected set is observable via `latest_validator_set`,
        //   4. one staker proposes a governance change,
        //   5. both stakers vote (yes for the majority staker),
        //   6. tally after the voting window passes the proposal because
        //      yes > 2/3 of total bonded stake.
        let mut spec = ChainSpec::mainnet();
        spec.epoch_length_blocks = 4;
        spec.min_validator_stake_wei = 1;
        spec.validators_per_epoch = 5;
        let backend = Arc::new(aii_storage::RocksDbBackend::open_in_temp().unwrap());
        let state = NodeState::new(spec, backend);

        let big = Address::new([0xb1; 20]);
        let small = Address::new([0xb2; 20]);
        state.stake_table().bond(&big, U256::from(800u64)).unwrap();
        state
            .stake_table()
            .bond(&small, U256::from(200u64))
            .unwrap();

        let mut parent = H256::ZERO;
        for n in 1..=4 {
            let b = fake_block(n, parent);
            parent = b.hash();
            state.commit_block(&b);
            state.set_head(n);
        }

        let (epoch, elected) = state
            .async_active_validator_set_test_helper()
            .expect("epoch boundary must record election");
        assert_eq!(epoch, 1);
        assert_eq!(elected.len(), 2);
        assert_eq!(elected[0].address, big, "biggest staker leads");

        let gov = state.governance();
        let proposal_id = gov.propose(big, "raise block reward".into(), 10).unwrap();
        assert_eq!(proposal_id, 1);

        let table = state.stake_table();
        gov.cast_vote(&table, proposal_id, big, true, 5).unwrap();
        gov.cast_vote(&table, proposal_id, small, true, 5).unwrap();

        let status = gov.tally(&table, proposal_id, 15).unwrap();
        assert_eq!(
            status,
            ProposalStatus::Passed,
            "1000/1000 yes > 2/3 of 1000 bonded → Passed",
        );
        let (yes, no) = gov.tally_of(proposal_id).unwrap().unwrap();
        assert_eq!(yes, U256::from(1_000u64));
        assert_eq!(no, U256::ZERO);
    }

    #[test]
    fn empty_block_post_roots_record_world_state() {
        // After committing an empty block, the sidecar should hold
        // the current state_root (genesis-empty here), an empty
        // receipts_root, and a zero logs_bloom. Persistence + read-
        // back must round-trip.
        let state = NodeState::new_for_tests(ChainSpec::mainnet());
        let b = fake_block(1, H256::ZERO);
        let h = b.hash();
        state.commit_block(&b);
        let roots = state.post_roots(h).unwrap().expect("post-roots persisted");
        // Empty body → receipts_root == EMPTY_TRIE_HASH.
        assert_eq!(roots.receipts_root, EMPTY_TRIE_HASH);
        // No logs → bloom is zero.
        assert_eq!(roots.logs_bloom, Bloom::ZERO);
        // state_root should equal the live state's computed root.
        let live = state.state().state_root().unwrap();
        assert_eq!(roots.state_root, live);
    }

    #[test]
    fn fork_at_same_height_records_evidence() {
        let state = NodeState::new_for_tests(ChainSpec::mainnet());
        let mut canonical = fake_block(1, H256::ZERO);
        canonical.header.extra_data = b"canonical".to_vec();
        let mut fork = fake_block(1, H256::ZERO);
        fork.header.extra_data = b"fork-branch".to_vec();
        let canonical_hash = canonical.hash();
        let fork_hash = fork.hash();
        assert_ne!(canonical_hash, fork_hash);
        // Commit canonical first.
        state.commit_block(&canonical);
        assert_eq!(state.block_count(), 1);
        // Now commit the fork — should be rejected from the index
        // but recorded as a fork record.
        state.commit_block(&fork);
        assert_eq!(state.block_count(), 1, "fork must not advance head");
        let forks = state.list_forks().unwrap();
        assert_eq!(forks.len(), 1);
        assert_eq!(forks[0].height, 1);
        assert_eq!(forks[0].canonical_hash, canonical_hash);
        assert_eq!(forks[0].fork_hash, fork_hash);
    }

    #[test]
    fn slash_debit_reduces_bonded_stake() {
        let state = NodeState::new_for_tests(ChainSpec::mainnet());
        let validator = Address::new([0xa1; 20]);
        state
            .stake_table()
            .bond(&validator, U256::from(1_000u64))
            .unwrap();
        state.debit_slash_stake(&validator, U256::from(250u64));
        let rec = state.stake_table().get(&validator).unwrap().unwrap();
        assert_eq!(rec.amount_wei, U256::from(750u64));
    }

    #[test]
    fn epoch_boundary_block_runs_dpos_election() {
        let mut spec = ChainSpec::mainnet();
        // Tiny epoch so the test doesn't need to commit 4 800 blocks.
        spec.epoch_length_blocks = 3;
        // Low min stake so test bonds clear it.
        spec.min_validator_stake_wei = 100;
        spec.validators_per_epoch = 5;
        let backend = Arc::new(aii_storage::RocksDbBackend::open_in_temp().unwrap());
        let state = NodeState::new(spec, backend);

        // Stake two addresses, different amounts.
        let big = Address::new([0xb1; 20]);
        let small = Address::new([0xb2; 20]);
        state
            .stake_table()
            .bond(&big, U256::from(1_000u64))
            .unwrap();
        state
            .stake_table()
            .bond(&small, U256::from(200u64))
            .unwrap();

        // Commit 3 blocks → block 3 triggers an election (height % 3 == 0).
        let mut parent = H256::ZERO;
        for n in 1..=3 {
            let b = fake_block(n, parent);
            parent = b.hash();
            state.commit_block(&b);
        }
        let latest = state.async_active_validator_set_test_helper();
        assert!(latest.is_some(), "epoch boundary must record election");
        let (epoch, entries) = latest.unwrap();
        assert_eq!(epoch, 1, "block 3 / epoch length 3 → epoch 1");
        assert_eq!(entries.len(), 2);
        // Sort order: big stake first.
        assert_eq!(entries[0].address, big);
        assert_eq!(entries[1].address, small);
    }

    #[test]
    fn subchain_flush_anchor_persists_and_reads_back() {
        let state = NodeState::new_for_tests(ChainSpec::mainnet());
        let id = aii_microchain::MicroChainId(7);
        let anchor = aii_microchain::FlushAnchor {
            sub_block_hash: H256::new([0x11; 32]),
            parent_block_hash: H256::new([0x22; 32]),
            sub_block_number: 99,
        };
        state.persist_flush_anchor(id, &anchor);
        let back = state.last_flush_anchor(id).unwrap().unwrap();
        assert_eq!(back, anchor);
    }

    #[test]
    fn slashing_record_persists_and_lists() {
        use aii_consensus_bft::EquivocationEvidence;
        use aii_consensus_bft::PrevoteVote;
        use aii_crypto::bls::SecretKey;
        let state = NodeState::new_for_tests(ChainSpec::mainnet());
        // Build two conflicting PRE-VOTES at the same (height, round).
        // Both signed by the same validator; different block_hash → equivocation.
        let sk = SecretKey::from_ikm(&[0xab; 32], b"aii-validator-test").expect("bls keygen");
        let vote_a = PrevoteVote::sign(&sk, H256::new([0x11; 32]), 42, 0, 7);
        let vote_b = PrevoteVote::sign(&sk, H256::new([0x22; 32]), 42, 0, 7);
        let evidence = EquivocationEvidence::Prevote {
            conflicting: [vote_a, vote_b],
        };
        state.record_slashing(&evidence);
        let listed = state.list_slashings().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].validator_index, 7);
        assert_eq!(listed[0].height, 42);
        assert_eq!(listed[0].phase, "prevote");
        assert_eq!(listed[0].block_hashes[0], H256::new([0x11; 32]));
        assert_eq!(listed[0].block_hashes[1], H256::new([0x22; 32]));
    }

    #[test]
    fn empty_block_credits_subsidy_to_beneficiary() {
        let state = NodeState::new_for_tests(ChainSpec::mainnet());
        let block = fake_block(1, H256::ZERO);
        let coinbase = block.header.beneficiary;
        state.commit_block(&block);
        let acc = state.state().account(&coinbase).unwrap().unwrap();
        // 2 AII initial subsidy at block 1 (no halving yet).
        let expected = U256::from(2_000_000_000_000_000_000u128);
        assert_eq!(
            acc.balance, expected,
            "block 1 must mint {expected} wei subsidy to beneficiary",
        );
    }

    #[test]
    fn subsidy_halves_at_interval_boundary() {
        let spec = ChainSpec::mainnet();
        let initial = spec.block_reward_initial_wei;
        let h = spec.block_reward_halving_interval;
        assert_eq!(spec.block_reward_at(h - 1), initial);
        assert_eq!(spec.block_reward_at(h), initial / 2);
    }

    #[test]
    fn receipt_round_trip_through_persistent_index() {
        let state = NodeState::new_for_tests(ChainSpec::mainnet());
        let tx_hash = H256::new([0xfe; 32]);
        let r = Receipt {
            tx_type: TxType::Eip1559,
            status: true,
            cumulative_gas_used: 42_000,
            logs_bloom: Bloom::ZERO,
            logs: vec![],
        };
        state.persist_receipts(H256::ZERO, &[(tx_hash, r.clone())]);
        let back = state.receipt_by_tx_hash(tx_hash).unwrap().unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn commit_block_executes_contract_deploy_through_revm() {
        use aii_block::tx::TxLegacy;
        use aii_types::AlgoId;

        let state = NodeState::new_for_tests(ChainSpec::mainnet());

        // Fund alice for the contract deployment.
        let alice = Address::new([0xa1; 20]);
        state
            .state()
            .set_account(
                &alice,
                &Account {
                    nonce: 0,
                    balance: U256::from(1_000_000_000_000_000_000u64), // 1 AII
                    ..Account::EMPTY
                },
            )
            .unwrap();

        // EVM creation bytecode that, when CALLed, SSTOREs 0x42 at slot 0:
        //   deploy:  0x60 0x06 0x60 0x0C 0x60 0x00 0x39 0x60 0x06 0x60 0x00 0xF3
        //   runtime: 0x60 0x42 0x60 0x00 0x55 0x00
        let bytecode = vec![
            0x60, 0x06, 0x60, 0x0C, 0x60, 0x00, 0x39, 0x60, 0x06, 0x60, 0x00, 0xF3, // deploy
            0x60, 0x42, 0x60, 0x00, 0x55, 0x00, // runtime
        ];

        // Synthesize a tx with sender = alice; we skip signature recovery
        // by directly setting the body and shipping the block through
        // commit_block — `execute_block_txs` calls `recover_signer` which
        // would fail. We instead test the execute_with_revm path by
        // calling it directly. (Full submit_raw_tx → commit_block →
        // contract working is exercised by the live testnet smoke test.)
        let _tx = Tx::Legacy(TxLegacy {
            nonce: 0,
            gas_price: U256::from(1_000_000_000u64),
            gas_limit: 200_000,
            to: None,
            value: U256::ZERO,
            data: bytecode.clone(),
            v: 27,
            r: H256::new([0xaa; 32]),
            s: H256::new([0xbb; 32]),
            algo_id: AlgoId::Secp256k1,
        });

        let summary = aii_evm::execute_with_revm(
            state.state(),
            alice,
            None,
            revm::primitives::U256::ZERO,
            bytecode,
            200_000,
            revm::primitives::U256::ZERO,
        )
        .expect("revm execute deploy");
        assert!(summary.success);
        let contract = summary.deployed_contract.expect("CREATE returns address");

        // Verify state_root reflects the contract account.
        let root_before = state.state().state_root().unwrap();
        // Touch storage via a CALL.
        let _ = aii_evm::execute_with_revm(
            state.state(),
            alice,
            Some(contract),
            revm::primitives::U256::ZERO,
            vec![],
            100_000,
            revm::primitives::U256::ZERO,
        )
        .unwrap();
        let root_after = state.state().state_root().unwrap();
        assert_ne!(
            root_before, root_after,
            "state_root must shift after contract call mutates storage"
        );
    }

    #[test]
    fn persistence_round_trip_recovers_state_blocks_and_head() {
        use aii_storage::RocksDbBackend;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().to_path_buf();

        // Phase A: open a fresh data dir, write state + commit blocks,
        // bump head, then drop everything (simulating shutdown).
        let alice = Address::new([0xa1; 20]);
        let alice_acc = Account {
            nonce: 7,
            balance: U256::from(987_654_321u64),
            ..Account::EMPTY
        };
        let mut block_hashes: Vec<H256> = Vec::new();
        {
            let backend = Arc::new(RocksDbBackend::open(&path).unwrap());
            let state = NodeState::new(ChainSpec::mainnet(), backend);
            state.state().set_account(&alice, &alice_acc).unwrap();
            let mut parent = H256::ZERO;
            for n in 1..=5 {
                let b = fake_block(n, parent);
                parent = b.hash();
                block_hashes.push(parent);
                state.commit_block(&b);
            }
            state.set_head(5);
            // state, backend Arcs dropped at end of scope.
        }

        // Phase B: reopen, recover, verify everything came back.
        let backend = Arc::new(RocksDbBackend::open(&path).unwrap());
        let state = NodeState::recover(ChainSpec::mainnet(), backend).unwrap();

        // Account survived.
        let after = state.state().account(&alice).unwrap().unwrap();
        assert_eq!(after, alice_acc, "account state must survive restart");

        // Head counter restored.
        assert_eq!(state.head_block_number_sync(), 5, "head counter restored");

        // All 5 blocks recovered, each indexed by hash + number.
        assert_eq!(state.block_count(), 5);
        for (i, h) in block_hashes.iter().enumerate() {
            let n = (i + 1) as u64;
            let by_n = state.blocks.read().unwrap().by_number.get(&n).copied();
            assert_eq!(
                by_n,
                Some(*h),
                "number → hash map must restore for block {n}"
            );
            assert!(
                state.blocks.read().unwrap().by_hash.contains_key(h),
                "hash → header map must restore for block {n}"
            );
            assert!(
                state.blocks.read().unwrap().body_by_hash.contains_key(h),
                "body must restore for block {n}"
            );
        }
    }

    /// v0.0.78 install path: when both the manifest and binary are
    /// present, and an install-target override is set (test mode),
    /// install_release performs the atomic file swap and returns
    /// `scheduled: true` WITHOUT spawning the `execve` self-task.
    /// The override file ends up holding the staged bytes; the
    /// previous override-file contents are discarded.
    #[cfg(unix)]
    #[tokio::test]
    async fn install_release_swaps_override_target_and_skips_exec() {
        use aii_crypto::ed25519::SecretKey;
        use aii_crypto::release::sign_release;
        use std::io::Write;

        const PINNED_SECRET_HEX: &str =
            "be06b95cb0e2d44ee175cc7a475ea4e9fcab47a784d161c36978b34e28ceeb97";

        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_path_buf();
        let target = data_dir.join("aiid-stub-binary");
        std::fs::write(&target, b"OLD CONTENTS").unwrap();

        let state = NodeState::new_for_tests(ChainSpec::testnet());
        state.set_data_dir(data_dir.clone());
        state.set_install_target_for_tests(target.clone());

        // Sign a manifest with the project pinned secret so
        // record_release_announcement accepts it.
        let payload = b"v0.0.78 atomic install body";
        let mut tmpf = tempfile::NamedTempFile::new().unwrap();
        tmpf.write_all(payload).unwrap();
        let sk = SecretKey::from_hex(PINNED_SECRET_HEX).unwrap();
        let manifest = sign_release(&sk, tmpf.path(), "0.0.78", 1_900_000_078).unwrap();
        let accepted =
            aii_rpc::RpcState::record_release_announcement(&*state, manifest.clone()).await;
        assert!(accepted, "manifest should be accepted on a fresh node");

        // Pre-stage the binary in the release store as if a prior
        // aii_importReleaseBinary had landed it.
        crate::release_store::store_verified_binary(
            &data_dir,
            "0.0.78",
            &manifest.sha256_hex,
            payload,
        )
        .unwrap();

        // Trigger install. Test mode: override path is used, no execve fires.
        let outcome = aii_rpc::RpcState::install_release(&*state, "0.0.78").await;
        assert!(outcome.scheduled, "install should succeed: {outcome:?}");
        assert!(outcome.restart_in_secs > 0);

        // Override target now holds the staged binary contents.
        let installed = std::fs::read(&target).unwrap();
        assert_eq!(
            installed, payload,
            "install_release must atomically replace the target"
        );

        // Stale .new file should not exist after a clean install.
        let staging_path = target.with_extension("new");
        assert!(
            !staging_path.exists(),
            ".new staging file must be consumed by rename"
        );
    }

    /// install_release fails-soft when the binary for the requested
    /// version is not present in the release store. Nothing is
    /// written to the target.
    #[cfg(unix)]
    #[tokio::test]
    async fn install_release_rejects_missing_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_path_buf();
        let target = data_dir.join("aiid-stub-binary");
        std::fs::write(&target, b"UNCHANGED").unwrap();

        let state = NodeState::new_for_tests(ChainSpec::testnet());
        state.set_data_dir(data_dir);
        state.set_install_target_for_tests(target.clone());

        let outcome = aii_rpc::RpcState::install_release(&*state, "0.0.99").await;
        assert!(!outcome.scheduled);
        assert!(outcome.reason.contains("no cached binary"));
        assert_eq!(std::fs::read(&target).unwrap(), b"UNCHANGED");
    }

    /// v0.0.78 auto-install path: when --auto-install-releases is
    /// on, a successful manifest accept followed by a binary import
    /// fires install_release without an explicit RPC call. The
    /// target file ends up holding the imported bytes.
    #[cfg(unix)]
    #[tokio::test]
    async fn auto_install_fires_when_manifest_and_binary_both_present() {
        use aii_crypto::ed25519::SecretKey;
        use aii_crypto::release::sign_release;
        use std::io::Write;

        const PINNED_SECRET_HEX: &str =
            "be06b95cb0e2d44ee175cc7a475ea4e9fcab47a784d161c36978b34e28ceeb97";

        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_path_buf();
        let target = data_dir.join("aiid-stub-binary");
        std::fs::write(&target, b"PREVIOUS").unwrap();

        let state = NodeState::new_for_tests(ChainSpec::testnet());
        state.set_data_dir(data_dir);
        state.set_install_target_for_tests(target.clone());
        state.set_auto_install_releases(true);
        assert!(state.auto_install_releases());

        let payload = b"v0.0.78 auto-install body";
        let mut tmpf = tempfile::NamedTempFile::new().unwrap();
        tmpf.write_all(payload).unwrap();
        let sk = SecretKey::from_hex(PINNED_SECRET_HEX).unwrap();
        let manifest = sign_release(&sk, tmpf.path(), "0.0.78", 1_900_000_079).unwrap();

        // Step 1: announce. Binary not yet present, auto-install skips silently.
        let ok = aii_rpc::RpcState::record_release_announcement(&*state, manifest.clone()).await;
        assert!(ok);
        // Target still untouched.
        assert_eq!(std::fs::read(&target).unwrap(), b"PREVIOUS");

        // Step 2: import binary. NOW both conditions hold, auto-install fires.
        let (imp_ok, reason) =
            aii_rpc::RpcState::import_release_binary(&*state, "0.0.78", payload.to_vec()).await;
        assert!(imp_ok, "binary import should accept: {reason}");

        // Target replaced.
        assert_eq!(std::fs::read(&target).unwrap(), payload);
    }
}
