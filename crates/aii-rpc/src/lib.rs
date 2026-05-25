//! # aii-rpc
//!
//! JSON-RPC + WebSocket server for an AII node.
//!
//! ## Public API
//! - [`RpcState`] — read-only view the RPC layer needs from the node.
//!   Implemented by the embedder (`aii-node`) so this crate stays decoupled
//!   from storage / consensus / state-db.
//! - [`serve`] — bind a `RpcState` to a TCP address.
//! - [`RpcError`] umbrella.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use aii_types::{Address, U256};
use async_trait::async_trait;
use jsonrpsee::core::RpcResult;
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::server::{Server, ServerHandle};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use thiserror::Error;

/// Account record exposed by `aii_getAccount`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountView {
    /// EVM account nonce.
    pub nonce: u64,
    /// Balance in Wei, hex-encoded (`"0x…"`).
    pub balance: String,
    /// Storage trie root hex (`"0x…"`).
    pub storage_root: String,
    /// Bytecode hash hex (`"0x…"`).
    pub code_hash: String,
}

/// JSON-shaped block header (subset suitable for explorers).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HeaderView {
    /// `0x…` hex of the block hash.
    pub hash: String,
    /// `0x…` hex of the parent hash.
    pub parent_hash: String,
    /// `0x…` hex of the block number (Ethereum convention).
    pub number: String,
    /// `0x…` hex of the slot timestamp (unix seconds).
    pub timestamp: String,
    /// `0x…` hex address of the block proposer / coinbase.
    pub beneficiary: String,
    /// `0x…` hex of the gas limit.
    pub gas_limit: String,
    /// `0x…` hex of the gas used.
    pub gas_used: String,
    /// `0x…` hex of the EIP-1559 base fee per gas.
    pub base_fee_per_gas: String,
    /// `0x…` hex of the state root.
    pub state_root: String,
    /// `0x…` hex of the transactions root.
    pub transactions_root: String,
    /// `0x…` hex of the receipts root.
    pub receipts_root: String,
    /// `0x…` hex of the mix hash (BFT: VRF output; PoA: zero).
    pub mix_hash: String,
    /// UTF-8 best-effort decoding of `header.extra_data`. The raw bytes
    /// stay available via their `0x…` hex in `extra_data_hex`.
    pub extra_data_hex: String,
}

/// Read-only state the RPC server consumes.
#[async_trait]
pub trait RpcState: Send + Sync + 'static {
    /// EIP-155 chain id (e.g. 99 for AII mainnet).
    fn chain_id(&self) -> u64;

    /// Human-readable network name.
    fn network(&self) -> String;

    /// Current head block number.
    async fn head_block_number(&self) -> u64;

    /// Minimum / current base-fee suggestion (Wei). Used for `eth_gasPrice`.
    fn gas_price(&self) -> U256;

    /// Return the account view for `addr`, or `None` if no record exists.
    async fn account(&self, addr: &Address) -> Option<AccountView>;

    /// Header by block number, or `None` if the chain has not produced
    /// `n` yet. Default returns `None`; node impls override.
    async fn header_by_number(&self, _n: u64) -> Option<HeaderView> {
        None
    }

    /// Header by block hash (`0x…` 32-byte hex), or `None` if unknown.
    /// Default returns `None`; node impls override.
    async fn header_by_hash(&self, _hash: &str) -> Option<HeaderView> {
        None
    }

    /// The N most-recently-finalised headers, newest first. Default
    /// returns empty; node impls override.
    async fn recent_headers(&self, _limit: usize) -> Vec<HeaderView> {
        Vec::new()
    }

    /// Submit a signed raw transaction. Default rejects; node impls
    /// that own a mempool should override.
    ///
    /// Returns the transaction's hash (`0x…` 32-byte hex) on success.
    async fn submit_raw_tx(&self, _raw_hex: &str) -> Result<String, SubmitTxError> {
        Err(SubmitTxError::Unsupported)
    }
}

/// Errors from `RpcState::submit_raw_tx`.
#[derive(Debug, thiserror::Error)]
pub enum SubmitTxError {
    /// Node was not built with mempool support.
    #[error("eth_sendRawTransaction not supported by this node")]
    Unsupported,
    /// Hex decode failed.
    #[error("invalid hex: {0}")]
    Hex(String),
    /// RLP / EIP-2718 decode failed.
    #[error("invalid tx encoding: {0}")]
    Decode(String),
    /// secp256k1 signer recovery failed.
    #[error("signer recovery: {0}")]
    Signer(String),
    /// Mempool rejected the tx (full, underpriced, etc.).
    #[error("mempool: {0}")]
    Pool(String),
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
    /// `eth_chainId` — chain id as `0x…` hex.
    #[method(name = "chainId")]
    fn chain_id(&self) -> RpcResult<String>;

    /// `eth_blockNumber` — head block number as `0x…` hex.
    #[method(name = "blockNumber")]
    async fn block_number(&self) -> RpcResult<String>;

    /// `eth_gasPrice` — suggested gas price as `0x…` hex (Wei).
    #[method(name = "gasPrice")]
    fn gas_price(&self) -> RpcResult<String>;

    /// `eth_getBalance(address, blockTag)` — balance as `0x…` hex (Wei).
    /// `blockTag` is currently ignored (only the head is supported).
    #[method(name = "getBalance")]
    async fn get_balance(&self, address: String, block_tag: Option<String>) -> RpcResult<String>;

    /// `eth_sendRawTransaction(rawHex)` — accepts an EIP-2718-encoded
    /// signed transaction (`0x…` hex), verifies the signer via
    /// secp256k1 ecrecover, and admits it to the mempool. Returns the
    /// 32-byte transaction hash as `0x…` hex.
    #[method(name = "sendRawTransaction")]
    async fn send_raw_transaction(&self, raw_hex: String) -> RpcResult<String>;
}

#[rpc(server, namespace = "aii")]
pub trait AiiRpc {
    /// `aii_status` — chain id + name + head number.
    #[method(name = "status")]
    async fn status(&self) -> RpcResult<AiiStatus>;

    /// `aii_getAccount(address)` — account view (nonce / balance / roots).
    /// Returns `null` if no account exists at that address.
    #[method(name = "getAccount")]
    async fn get_account(&self, address: String) -> RpcResult<Option<AccountView>>;

    /// `aii_getBlockHeader(numberOrHash)` — accepts either a decimal /
    /// `0x…` hex block number, or a 32-byte `0x…` hex block hash.
    /// Returns `null` if the block is unknown.
    #[method(name = "getBlockHeader")]
    async fn get_block_header(&self, query: String) -> RpcResult<Option<HeaderView>>;

    /// `aii_recentBlocks(limit)` — N most recent block headers, newest
    /// first. `limit` is capped at 100.
    #[method(name = "recentBlocks")]
    async fn recent_blocks(&self, limit: u64) -> RpcResult<Vec<HeaderView>>;
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

    fn gas_price(&self) -> RpcResult<String> {
        let p = self.state.gas_price();
        Ok(format!("0x{p:x}"))
    }

    async fn get_balance(&self, address: String, _block: Option<String>) -> RpcResult<String> {
        let addr = parse_address(&address)?;
        let bal = self
            .state
            .account(&addr)
            .await
            .map_or_else(|| "0x0".to_string(), |a| a.balance);
        Ok(bal)
    }

    async fn send_raw_transaction(&self, raw_hex: String) -> RpcResult<String> {
        match self.state.submit_raw_tx(&raw_hex).await {
            Ok(hash) => Ok(hash),
            Err(e) => Err(jsonrpsee::types::ErrorObjectOwned::owned(
                -32000,
                e.to_string(),
                None::<()>,
            )),
        }
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

    async fn get_account(&self, address: String) -> RpcResult<Option<AccountView>> {
        let addr = parse_address(&address)?;
        Ok(self.state.account(&addr).await)
    }

    async fn get_block_header(&self, query: String) -> RpcResult<Option<HeaderView>> {
        // Block hash is 0x + 64 hex chars; everything else parses as a number.
        let trimmed = query.strip_prefix("0x").unwrap_or(&query);
        if trimmed.len() == 64 {
            return Ok(self.state.header_by_hash(&format!("0x{trimmed}")).await);
        }
        let n = if let Some(rest) = query.strip_prefix("0x") {
            u64::from_str_radix(rest, 16)
        } else {
            query.parse::<u64>()
        }
        .map_err(|e| {
            jsonrpsee::types::ErrorObjectOwned::owned(
                -32602,
                format!("getBlockHeader: '{query}' is neither a number nor a 32-byte hash: {e}"),
                None::<()>,
            )
        })?;
        Ok(self.state.header_by_number(n).await)
    }

    async fn recent_blocks(&self, limit: u64) -> RpcResult<Vec<HeaderView>> {
        let capped = usize::try_from(limit.min(100)).unwrap_or(100);
        Ok(self.state.recent_headers(capped).await)
    }
}

fn parse_address(s: &str) -> RpcResult<Address> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if s.len() != 40 {
        return Err(jsonrpsee::types::ErrorObjectOwned::owned(
            -32602,
            "address must be 0x + 40 hex chars",
            None::<()>,
        ));
    }
    let mut bytes = [0u8; 20];
    hex::decode_to_slice(s, &mut bytes).map_err(|e| {
        jsonrpsee::types::ErrorObjectOwned::owned(-32602, format!("hex decode: {e}"), None::<()>)
    })?;
    Ok(Address::new(bytes))
}

/// Bind an RPC server to `addr` backed by `state`.
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

    /// Method-registration failure.
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
        gas: U256,
        alice: Address,
        alice_balance: U256,
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
        fn gas_price(&self) -> U256 {
            self.gas
        }
        async fn account(&self, addr: &Address) -> Option<AccountView> {
            if *addr == self.alice {
                Some(AccountView {
                    nonce: 7,
                    balance: format!("0x{:x}", self.alice_balance),
                    storage_root:
                        "0x56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421"
                            .to_string(),
                    code_hash: "0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
                        .to_string(),
                })
            } else {
                None
            }
        }
    }

    fn fixture() -> Arc<TestState> {
        Arc::new(TestState {
            chain_id: 99,
            network: "aii-mainnet".to_string(),
            head: 0xab,
            gas: U256::from(1_000_000_000u64),
            alice: Address::new([0x42; 20]),
            alice_balance: U256::from(1_000u64),
        })
    }

    async fn spawn() -> (String, ServerHandle) {
        let (addr, handle) = serve("127.0.0.1:0".parse().unwrap(), fixture())
            .await
            .unwrap();
        (format!("http://{addr}"), handle)
    }

    #[tokio::test]
    async fn eth_chain_id() {
        let (url, h) = spawn().await;
        let c = HttpClientBuilder::default().build(url).unwrap();
        let r: String = c.request("eth_chainId", rpc_params![]).await.unwrap();
        assert_eq!(r, "0x63");
        h.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_block_number() {
        let (url, h) = spawn().await;
        let c = HttpClientBuilder::default().build(url).unwrap();
        let r: String = c.request("eth_blockNumber", rpc_params![]).await.unwrap();
        assert_eq!(r, "0xab");
        h.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_gas_price() {
        let (url, h) = spawn().await;
        let c = HttpClientBuilder::default().build(url).unwrap();
        let r: String = c.request("eth_gasPrice", rpc_params![]).await.unwrap();
        assert_eq!(r, "0x3b9aca00"); // 1e9
        h.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_get_balance_existing() {
        let (url, h) = spawn().await;
        let c = HttpClientBuilder::default().build(url).unwrap();
        let r: String = c
            .request(
                "eth_getBalance",
                rpc_params!["0x4242424242424242424242424242424242424242", "latest"],
            )
            .await
            .unwrap();
        assert_eq!(r, "0x3e8"); // 1000
        h.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_get_balance_missing_returns_zero() {
        let (url, h) = spawn().await;
        let c = HttpClientBuilder::default().build(url).unwrap();
        let r: String = c
            .request(
                "eth_getBalance",
                rpc_params!["0x1111111111111111111111111111111111111111", "latest"],
            )
            .await
            .unwrap();
        assert_eq!(r, "0x0");
        h.stop().unwrap();
    }

    #[tokio::test]
    async fn eth_get_balance_bad_address_errors() {
        let (url, h) = spawn().await;
        let c = HttpClientBuilder::default().build(url).unwrap();
        let r: Result<String, _> = c
            .request(
                "eth_getBalance",
                rpc_params!["0xnot-a-real-address", "latest"],
            )
            .await;
        assert!(r.is_err());
        h.stop().unwrap();
    }

    #[tokio::test]
    async fn aii_status() {
        let (url, h) = spawn().await;
        let c = HttpClientBuilder::default().build(url).unwrap();
        let r: AiiStatus = c.request("aii_status", rpc_params![]).await.unwrap();
        assert_eq!(r.chain_id, 99);
        h.stop().unwrap();
    }

    #[tokio::test]
    async fn aii_get_account_existing() {
        let (url, h) = spawn().await;
        let c = HttpClientBuilder::default().build(url).unwrap();
        let r: Option<AccountView> = c
            .request(
                "aii_getAccount",
                rpc_params!["0x4242424242424242424242424242424242424242"],
            )
            .await
            .unwrap();
        let view = r.unwrap();
        assert_eq!(view.nonce, 7);
        assert_eq!(view.balance, "0x3e8");
        h.stop().unwrap();
    }

    #[tokio::test]
    async fn aii_get_account_missing_returns_null() {
        let (url, h) = spawn().await;
        let c = HttpClientBuilder::default().build(url).unwrap();
        let r: Option<AccountView> = c
            .request(
                "aii_getAccount",
                rpc_params!["0x1111111111111111111111111111111111111111"],
            )
            .await
            .unwrap();
        assert!(r.is_none());
        h.stop().unwrap();
    }
}
