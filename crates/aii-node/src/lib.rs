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

use aii_block::tx::Tx;
use aii_block::{Block, Hashable, Header};
use aii_config::ChainSpec;
use aii_net_txpool::{effective_gas_price, AddOutcome, PoolEntry, TxPool};
use aii_rpc::{AccountView, HeaderView, RpcState, SubmitTxError};
use aii_state::StateDb;
use aii_storage::MemoryBackend;
use aii_types::{Address, H256, U256};
use alloy_rlp::Decodable;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// In-process node state.
///
/// Owns a `ChainSpec`, a head-block counter, and an in-memory `StateDb` so
/// that the RPC layer can answer `eth_getBalance` / `aii_getAccount`
/// against real state. The persistent RocksDB backend connected by the
/// `aiid` binary stays separate (it stores blocks; v0.0.11 will route
/// `StateDb` to it too).
pub struct NodeState {
    spec: ChainSpec,
    head: AtomicU64,
    state: StateDb<MemoryBackend>,
    /// In-memory header store. Persistent RocksDB lands in v0.0.36+.
    blocks: RwLock<BlockStore>,
    /// Mempool for incoming signed transactions (v0.0.37).
    tx_pool: TxPool,
}

/// Headers keyed by hash + a number→hash index, plus an insertion-order
/// vector used to serve "recent N blocks".
#[derive(Default)]
struct BlockStore {
    by_hash: HashMap<H256, Header>,
    by_number: HashMap<u64, H256>,
    /// Insertion order for `recent_headers` — push on commit, scan tail.
    order: Vec<H256>,
}

impl NodeState {
    /// Construct with a starting head of 0 (genesis) and a fresh in-memory
    /// state database.
    pub fn new(spec: ChainSpec) -> Arc<Self> {
        Arc::new(Self {
            spec,
            head: AtomicU64::new(0),
            state: StateDb::new(Arc::new(MemoryBackend::new())),
            blocks: RwLock::new(BlockStore::default()),
            tx_pool: TxPool::new(100_000),
        })
    }

    /// Borrow the mempool (for the producer drain loop).
    #[must_use]
    pub const fn tx_pool(&self) -> &TxPool {
        &self.tx_pool
    }

    /// Update the head block number — called when a new block is finalised.
    pub fn set_head(&self, n: u64) {
        self.head.store(n, Ordering::Relaxed);
    }

    /// Index a finalised block so RPC clients can look it up via
    /// `aii_getBlockHeader` / `aii_recentBlocks`. Idempotent on the
    /// same hash.
    pub fn commit_block(&self, block: &Block) {
        let hash = block.hash();
        let mut s = self.blocks.write().expect("BlockStore lock not poisoned");
        if s.by_hash.contains_key(&hash) {
            return;
        }
        s.by_hash.insert(hash, block.header.clone());
        s.by_number.insert(block.header.number, hash);
        s.order.push(hash);
    }

    /// Total number of indexed blocks (test-only diagnostic).
    #[must_use]
    pub fn block_count(&self) -> usize {
        self.blocks.read().map_or(0, |s| s.order.len())
    }

    /// Borrow the world-state for embedders who want to read/write accounts
    /// directly (e.g. apply a genesis allocation).
    pub const fn state(&self) -> &StateDb<MemoryBackend> {
        &self.state
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

fn parse_hash_str(s: &str) -> Option<H256> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if s.len() != 64 {
        return None;
    }
    let mut bytes = [0u8; 32];
    hex::decode_to_slice(s, &mut bytes).ok()?;
    Some(H256::new(bytes))
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
        let state = NodeState::new(ChainSpec::mainnet());
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
        let state = NodeState::new(ChainSpec::testnet());
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
        let state = NodeState::new(ChainSpec::testnet());
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
        let state = NodeState::new(ChainSpec::mainnet());
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
        let state = NodeState::new(ChainSpec::mainnet()); // min_base_fee = 1e9
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
        let state = NodeState::new(ChainSpec::mainnet());
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
        let state = NodeState::new(ChainSpec::mainnet());
        let block = fake_block(42, H256::ZERO);
        let block_hash = block.hash();
        state.commit_block(&block);
        let hex_hash = format!("0x{}", hex::encode(block_hash.as_bytes()));
        let view = state.header_by_hash(&hex_hash).await.unwrap();
        assert_eq!(view.number, "0x2a");
    }

    #[tokio::test]
    async fn lookup_unknown_block_returns_none() {
        let state = NodeState::new(ChainSpec::mainnet());
        assert!(state.header_by_number(99).await.is_none());
        assert!(state
            .header_by_hash("0x0000000000000000000000000000000000000000000000000000000000000000")
            .await
            .is_none());
    }

    #[tokio::test]
    async fn recent_headers_returns_newest_first_and_caps_at_limit() {
        let state = NodeState::new(ChainSpec::mainnet());
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
        let state = NodeState::new(ChainSpec::mainnet());
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
        let state = NodeState::new(ChainSpec::mainnet());
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
        let state = NodeState::new(ChainSpec::mainnet());
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
}
