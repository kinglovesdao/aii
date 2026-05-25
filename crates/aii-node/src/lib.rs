//! # aii-node (library surface)
//!
//! The `aii-node` crate is primarily a binary (`aiid`) — but a small
//! library surface lets integration tests boot a node in-process and
//! exercise it without subprocesses.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod bft_bootstrap;
pub mod bft_p2p;

use aii_config::ChainSpec;
use aii_rpc::{AccountView, RpcState};
use aii_state::StateDb;
use aii_storage::MemoryBackend;
use aii_types::{Address, U256};
use async_trait::async_trait;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

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
}

impl NodeState {
    /// Construct with a starting head of 0 (genesis) and a fresh in-memory
    /// state database.
    pub fn new(spec: ChainSpec) -> Arc<Self> {
        Arc::new(Self {
            spec,
            head: AtomicU64::new(0),
            state: StateDb::new(Arc::new(MemoryBackend::new())),
        })
    }

    /// Update the head block number — called when a new block is finalised.
    pub fn set_head(&self, n: u64) {
        self.head.store(n, Ordering::Relaxed);
    }

    /// Borrow the world-state for embedders who want to read/write accounts
    /// directly (e.g. apply a genesis allocation).
    pub const fn state(&self) -> &StateDb<MemoryBackend> {
        &self.state
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
}
