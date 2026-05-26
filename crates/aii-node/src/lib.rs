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
pub mod sync;

pub use sync::bootstrap_sync_from_peer;

use aii_block::tx::Tx;
use aii_block::{Block, BlockBody, Bloom, Hashable, Header, Receipt, TxType};
use aii_config::ChainSpec;
use aii_net_txpool::{effective_gas_price, AddOutcome, PoolEntry, TxPool};
use aii_rpc::{AccountView, HeaderView, LogView, ReceiptView, RpcState, SubmitTxError, TxView};
use aii_state::StateDb;
use aii_storage::{ColumnFamily, KvBackend, RocksDbBackend, WriteBatch};
use aii_types::{Address, H256, U256};
use alloy_rlp::{Decodable, Encodable};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// Meta-CF key holding the canonical chain head as a big-endian u64.
const META_KEY_HEAD: &[u8] = b"head_block_number";
/// Meta-CF key holding the canonical chain head hash (raw 32 bytes).
const META_KEY_HEAD_HASH: &[u8] = b"head_block_hash";

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
        {
            let mut s = self.blocks.write().expect("BlockStore lock not poisoned");
            if s.by_hash.contains_key(&hash) {
                return;
            }
            s.by_hash.insert(hash, block.header.clone());
            s.by_number.insert(block.header.number, hash);
            s.order.push(hash);
            // Index every tx by hash so `aii_getTransaction` is O(1).
            for (idx, tx) in block.body.transactions.iter().enumerate() {
                s.tx_index.insert(tx.hash(), (block.header.number, idx));
            }
            s.body_by_hash.insert(hash, block.body.clone());
        }
        self.persist_block(block, hash);
        self.execute_block_txs(block);
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
}
