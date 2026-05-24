//! # aii-rpc
//!
//! JSON-RPC + WebSocket server for an AII node.
//!
//! ## Public API
//! - [`RpcState`] — read-only view the RPC layer needs from the node
//!   (chain id, head block number, chain name). Implemented by the
//!   embedder (`aii-node`) — `aii-rpc` does not own state.
//! - [`serve`] — bind a `RpcState` to a TCP address and return the running
//!   server handle.
//! - [`RpcError`] — umbrella

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use async_trait::async_trait;
use jsonrpsee::core::RpcResult;
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::server::{Server, ServerHandle};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use thiserror::Error;

/// Read-only state the RPC server consumes. Implemented by the embedding
/// node so the RPC crate stays decoupled from storage / consensus.
#[async_trait]
pub trait RpcState: Send + Sync + 'static {
    /// EIP-155 chain id (e.g. 99 for AII mainnet).
    fn chain_id(&self) -> u64;

    /// Human-readable network name.
    fn network(&self) -> String;

    /// Current head block number.
    async fn head_block_number(&self) -> u64;
}

/// `aii_status` response body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiiStatus {
    /// EIP-155 chain id.
    pub chain_id: u64,
    /// Human-readable network name.
    pub network: String,
    /// Current head block number.
    pub head_block_number: u64,
}

#[rpc(server, namespace = "eth")]
pub trait EthRpc {
    /// `eth_chainId` — returns chain id as 0x-prefixed hex quantity.
    #[method(name = "chainId")]
    fn chain_id(&self) -> RpcResult<String>;

    /// `eth_blockNumber` — returns head block number as hex quantity.
    #[method(name = "blockNumber")]
    async fn block_number(&self) -> RpcResult<String>;
}

#[rpc(server, namespace = "aii")]
pub trait AiiRpc {
    /// `aii_status` — chain id + name + head number.
    #[method(name = "status")]
    async fn status(&self) -> RpcResult<AiiStatus>;
}

struct EthRpcImpl<S: RpcState> {
    state: Arc<S>,
}

#[async_trait]
impl<S: RpcState> EthRpcServer for EthRpcImpl<S> {
    fn chain_id(&self) -> RpcResult<String> {
        Ok(format!("0x{:x}", self.state.chain_id()))
    }

    async fn block_number(&self) -> RpcResult<String> {
        Ok(format!("0x{:x}", self.state.head_block_number().await))
    }
}

struct AiiRpcImpl<S: RpcState> {
    state: Arc<S>,
}

#[async_trait]
impl<S: RpcState> AiiRpcServer for AiiRpcImpl<S> {
    async fn status(&self) -> RpcResult<AiiStatus> {
        Ok(AiiStatus {
            chain_id: self.state.chain_id(),
            network: self.state.network(),
            head_block_number: self.state.head_block_number().await,
        })
    }
}

/// Bind an RPC server to `addr` backed by `state`. Returns the bound socket
/// address and the server handle (drop the handle to stop the server).
pub async fn serve<S: RpcState>(
    addr: SocketAddr,
    state: Arc<S>,
) -> Result<(SocketAddr, ServerHandle), RpcError> {
    let server = Server::builder()
        .build(addr)
        .await
        .map_err(RpcError::Bind)?;
    let bound = server.local_addr().map_err(RpcError::Bind)?;

    let eth = EthRpcImpl {
        state: state.clone(),
    };
    let aii = AiiRpcImpl {
        state: state.clone(),
    };

    let mut module = eth.into_rpc();
    module
        .merge(aii.into_rpc())
        .map_err(|e| RpcError::Register(e.to_string()))?;

    let handle = server.start(module);
    Ok((bound, handle))
}

/// Errors produced when starting or running the RPC server.
#[derive(Debug, Error)]
pub enum RpcError {
    /// Socket bind / accept failure.
    #[error("bind: {0}")]
    Bind(std::io::Error),

    /// Method-registration failure (typically a namespace collision).
    #[error("register: {0}")]
    Register(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonrpsee::core::client::ClientT;
    use jsonrpsee::http_client::HttpClientBuilder;
    use jsonrpsee::rpc_params;

    struct TestState {
        chain_id: u64,
        network: String,
        head: u64,
    }

    #[async_trait]
    impl RpcState for TestState {
        fn chain_id(&self) -> u64 {
            self.chain_id
        }
        fn network(&self) -> String {
            self.network.clone()
        }
        async fn head_block_number(&self) -> u64 {
            self.head
        }
    }

    #[tokio::test]
    async fn eth_chain_id_returns_hex() {
        let state = Arc::new(TestState {
            chain_id: 99,
            network: "aii-mainnet".to_string(),
            head: 0,
        });
        let (addr, handle) = serve("127.0.0.1:0".parse().unwrap(), state).await.unwrap();
        let url = format!("http://{addr}");
        let client = HttpClientBuilder::default().build(url).unwrap();
        let result: String = client.request("eth_chainId", rpc_params![]).await.unwrap();
        assert_eq!(result, "0x63"); // 99
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_block_number_returns_head() {
        let state = Arc::new(TestState {
            chain_id: 99,
            network: "aii-mainnet".to_string(),
            head: 0xdead,
        });
        let (addr, handle) = serve("127.0.0.1:0".parse().unwrap(), state).await.unwrap();
        let url = format!("http://{addr}");
        let client = HttpClientBuilder::default().build(url).unwrap();
        let result: String = client
            .request("eth_blockNumber", rpc_params![])
            .await
            .unwrap();
        assert_eq!(result, "0xdead");
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn aii_status_returns_full_struct() {
        let state = Arc::new(TestState {
            chain_id: 99,
            network: "aii-mainnet".to_string(),
            head: 42,
        });
        let (addr, handle) = serve("127.0.0.1:0".parse().unwrap(), state).await.unwrap();
        let url = format!("http://{addr}");
        let client = HttpClientBuilder::default().build(url).unwrap();
        let result: AiiStatus = client.request("aii_status", rpc_params![]).await.unwrap();
        assert_eq!(
            result,
            AiiStatus {
                chain_id: 99,
                network: "aii-mainnet".to_string(),
                head_block_number: 42,
            }
        );
        handle.stop().unwrap();
    }
}
