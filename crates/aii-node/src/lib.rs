//! # aii-node (library surface)
//!
//! The `aii-node` crate is primarily a binary (`aiid`) — but a small
//! library surface lets integration tests boot a node in-process and
//! exercise it without subprocesses.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use aii_config::ChainSpec;
use aii_rpc::RpcState;
use async_trait::async_trait;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// In-process node state — minimal v0.0.7 surface that satisfies
/// `aii_rpc::RpcState`. Real consensus / state-machine wiring lands in
/// later releases.
pub struct NodeState {
    spec: ChainSpec,
    head: AtomicU64,
}

impl NodeState {
    /// Construct with a starting head of 0 (genesis).
    pub fn new(spec: ChainSpec) -> Arc<Self> {
        Arc::new(Self {
            spec,
            head: AtomicU64::new(0),
        })
    }

    /// Update the head block number — called when a new block is finalised.
    pub fn set_head(&self, n: u64) {
        self.head.store(n, Ordering::Relaxed);
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
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
