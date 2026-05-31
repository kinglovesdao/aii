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

pub mod release_gossip;
pub mod release_poller;

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

/// JSON-shaped transaction record (subset suitable for explorers).
///
/// `value`, `nonce`, `gas_limit`, `max_fee_per_gas`, and `max_priority_fee_per_gas`
/// are stringified hex so JSON consumers never get truncation surprises around
/// `2^53 - 1`. `tx_type` is `"legacy" | "eip1559" | "eip4844"`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TxView {
    /// Keccak-256 hash of the encoded transaction (`0x…` 32-byte hex).
    pub hash: String,
    /// Recovered sender address (`0x…` 20-byte hex). Comes from
    /// `Tx::recover_signer(chain_id)`.
    pub from: String,
    /// Recipient address (`0x…` 20-byte hex), or empty string for contract
    /// creations (`to == None`).
    pub to: String,
    /// Transferred value in Wei (`0x…` hex of a u256).
    pub value: String,
    /// Sender's nonce on this tx (`0x…` hex).
    pub nonce: String,
    /// Gas limit reserved by the tx (`0x…` hex).
    pub gas_limit: String,
    /// EIP-1559 / 4844: max fee per gas (`0x…` hex). Mirrors `gas_price` for legacy.
    pub max_fee_per_gas: String,
    /// EIP-1559 / 4844: priority fee per gas (`0x…` hex). Same as
    /// `max_fee_per_gas` for legacy (priority concept doesn't apply).
    pub max_priority_fee_per_gas: String,
    /// Variant: `"legacy"`, `"eip1559"`, or `"eip4844"`.
    pub tx_type: String,
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

/// Ethereum-JSON-RPC-shaped transaction response (v0.0.90).
///
/// Used by `eth_getTransactionByHash` and as the
/// full-transaction variant inside [`EthBlockResponse`].
/// Carries the same body as [`TxView`] (via flatten) plus the
/// three "where did this tx land" fields a MetaMask /
/// ethers.js client expects: block hash, block number, and
/// in-block index.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EthTxResponse {
    /// Inlined TxView fields (`hash`, `from`, `to`, `value`,
    /// `nonce`, `gas_limit`, fee fields, `tx_type`).
    #[serde(flatten)]
    pub tx: TxView,
    /// `0x…` 32-byte hex of the containing block's hash.
    pub block_hash: String,
    /// `0x…` hex of the containing block's number.
    pub block_number: String,
    /// `0x…` hex of the tx's index within the block.
    pub transaction_index: String,
}

/// Ethereum-JSON-RPC-shaped block response (v0.0.90).
///
/// Used by `eth_getBlockByHash` / `eth_getBlockByNumber`.
/// Carries every field from [`HeaderView`] (via flatten) plus
/// a `transactions` array — either a list of tx hashes
/// (`full = false`, default for cheap explorer queries) or a
/// list of full [`EthTxResponse`] objects (`full = true`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EthBlockResponse {
    /// Inlined HeaderView fields.
    #[serde(flatten)]
    pub header: HeaderView,
    /// Either tx hashes (`["0x…", "0x…", …]`) or full tx
    /// objects, depending on the `full_transactions` arg
    /// passed by the caller.
    pub transactions: EthBlockTxs,
}

/// `transactions` payload of an [`EthBlockResponse`] — either
/// hashes or full objects.
///
/// `#[serde(untagged)]` so the JSON wire shape is just a plain
/// array of strings or a plain array of objects (no enum
/// discriminator). Matches the Ethereum spec.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum EthBlockTxs {
    /// `full_transactions = false`: array of `0x…` tx hashes.
    Hashes(Vec<String>),
    /// `full_transactions = true`: array of full tx objects.
    Full(Vec<EthTxResponse>),
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

    /// Return the runtime bytecode at `addr`, or empty
    /// `Vec::new()` if the account has no code (EOA or
    /// non-existent). Default returns empty; node impls
    /// override. Used by [`AiiRpc::call`]-adjacent
    /// `eth_getCode` (v0.0.89).
    async fn code_at(&self, _addr: &Address) -> Vec<u8> {
        Vec::new()
    }

    /// Return the 32-byte storage value at `(addr, slot)`.
    /// Unset slots return all-zero (matches EVM semantics).
    /// Default returns zero; node impls override. Used by
    /// `eth_getStorageAt` (v0.0.89).
    async fn storage_at(&self, _addr: &Address, _slot: &aii_types::H256) -> aii_types::H256 {
        aii_types::H256::ZERO
    }

    /// Estimate the gas a transaction would consume by
    /// running it through the EVM as a simulation (no state
    /// changes). Default returns `Unsupported`. Node impls
    /// route through `aii_evm::simulate_with_revm` (v0.0.89).
    async fn estimate_gas(&self, _req: SimulateCallParams) -> Result<u64, SimulateCallError> {
        Err(SimulateCallError::Unsupported)
    }

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

    /// Transactions inside the block at `n`, in inclusion order. Default
    /// returns `None` (block not found / not tracked); node impls override.
    /// A returned `Some(Vec::new())` means "block exists but had no txs".
    async fn block_transactions(&self, _n: u64) -> Option<Vec<TxView>> {
        None
    }

    /// Look up a single transaction by hash and return its body view +
    /// the block number it landed in (or `None` if unknown). Default
    /// returns `None`; node impls override.
    async fn transaction_by_hash(&self, _hash: &str) -> Option<(TxView, u64)> {
        None
    }

    /// Ethereum-shaped tx lookup (v0.0.90). Same data as
    /// [`Self::transaction_by_hash`] plus the block hash and
    /// in-block index that an `eth_getTransactionByHash`
    /// caller expects. Default returns `None`.
    async fn eth_transaction_by_hash(&self, _hash: &str) -> Option<EthTxResponse> {
        None
    }

    /// Ethereum-shaped block lookup by number (v0.0.90). When
    /// `full_transactions` is `false`, the response's `transactions`
    /// field is a list of tx hashes; when `true`, it's a list of
    /// full [`EthTxResponse`] objects. Default returns `None`.
    async fn eth_block_by_number(
        &self,
        _n: u64,
        _full_transactions: bool,
    ) -> Option<EthBlockResponse> {
        None
    }

    /// Ethereum-shaped block lookup by hash (v0.0.90). Same shape
    /// as [`Self::eth_block_by_number`]. Default returns `None`.
    async fn eth_block_by_hash(
        &self,
        _hash: &str,
        _full_transactions: bool,
    ) -> Option<EthBlockResponse> {
        None
    }

    /// Submit a signed raw transaction. Default rejects; node impls
    /// that own a mempool should override.
    ///
    /// Returns the transaction's hash (`0x…` 32-byte hex) on success.
    async fn submit_raw_tx(&self, _raw_hex: &str) -> Result<String, SubmitTxError> {
        Err(SubmitTxError::Unsupported)
    }

    /// Look up a transaction's receipt by tx hash. Default returns
    /// `None`; node impls that index receipts should override.
    async fn receipt_by_tx_hash(&self, _hash: &str) -> Option<ReceiptView> {
        None
    }

    /// Scan a block range for logs matching `filter`. Default empty.
    async fn logs_in_range(&self, _filter: &LogFilter) -> Vec<LogEntryView> {
        Vec::new()
    }

    /// Return the RLP-encoded full `Block` (header + body) as `0x…`
    /// hex. Default returns `None`; persistent backends should override.
    async fn raw_block(&self, _query: &str) -> Option<String> {
        None
    }

    /// List every persisted slashing record. Default returns an empty
    /// vector; node impls that index slashings override.
    async fn slashings(&self) -> Vec<SlashView> {
        Vec::new()
    }

    /// Return the most recent flush anchor for sub-chain `id`, or
    /// `None` if no anchor has been recorded.
    async fn subchain_anchor(&self, _id: u32) -> Option<SubchainAnchorView> {
        None
    }

    /// Read one staker's bond + unbond status. Default returns `None`.
    async fn stake_at(&self, _address: &Address) -> Option<StakeView> {
        None
    }

    /// Sum of every currently-bonded stake on the chain in Wei.
    /// Default returns `U256::ZERO`.
    async fn total_bonded_stake(&self) -> U256 {
        U256::ZERO
    }

    /// List every staking record. Default returns an empty vector.
    async fn all_stakers(&self) -> Vec<StakeView> {
        Vec::new()
    }

    /// Most recently elected DPoS validator set, with the epoch it
    /// was elected at. Default returns `None`.
    async fn active_validator_set(&self) -> Option<ActiveValidatorsView> {
        None
    }

    /// List every governance proposal. Default empty.
    async fn governance_proposals(&self) -> Vec<ProposalView> {
        Vec::new()
    }

    /// One governance proposal lookup. Default `None`.
    async fn governance_proposal(&self, _id: u64) -> Option<ProposalView> {
        None
    }

    /// List every fork-detection record. Default empty.
    async fn forks(&self) -> Vec<ForkView> {
        Vec::new()
    }

    /// Read post-block Yellow-Paper sidecar roots. Default `None`.
    async fn post_roots_for(&self, _block_hash: &str) -> Option<PostRootsView> {
        None
    }

    /// Record a verified release manifest gossiped by a peer
    /// (v0.0.75). Default no-ops; nodes that participate in the
    /// auto-update protocol override to persist the manifest into
    /// their in-memory `latest_release` slot.
    ///
    /// Implementations MUST NOT trust the manifest blindly — they
    /// must have already verified the Ed25519 signature against
    /// the pinned project pubkey before calling this. The default
    /// `aii-rpc` dispatcher (see `AiiRpcImpl::announce_release`)
    /// does the verification and only calls this on the happy path.
    async fn record_release_announcement(
        &self,
        _manifest: aii_crypto::release::ReleaseManifest,
    ) -> bool {
        false
    }

    /// Return the latest verified release manifest known to the
    /// node, or `None` on a node that has never received an
    /// announcement. Default `None`.
    async fn latest_release(&self) -> Option<aii_crypto::release::ReleaseManifest> {
        None
    }

    /// Return the bytes of the cached release binary for `version`,
    /// or `None` if the node hasn't stored that version. Default
    /// `None`; embedders that participate in the auto-update
    /// protocol override.
    async fn release_binary_bytes(&self, _version: &str) -> Option<Vec<u8>> {
        None
    }

    /// Accept a peer-supplied binary blob for `version`. The
    /// implementation MUST verify the bytes hash to the SHA-256 in
    /// the most-recently-known manifest for `version` before
    /// persisting; on hash mismatch return
    /// `(false, "<reason>")`. Default no-op rejects everything.
    async fn import_release_binary(&self, _version: &str, _bytes: Vec<u8>) -> (bool, String) {
        (
            false,
            "node does not participate in release binary store".into(),
        )
    }

    /// Peer HTTP-RPC URLs the v0.0.77 release-propagation task
    /// targets after accepting a new manifest. Default empty;
    /// `aii-node::NodeState` overrides to return the operator-
    /// supplied `--update-peers` list.
    async fn update_peers_for_release(&self) -> Vec<String> {
        Vec::new()
    }

    /// Atomically install a previously-imported release binary
    /// over the running `aiid` and schedule an `execve` self-
    /// restart (v0.0.78). The install (file copy + chmod +
    /// rename) happens synchronously inside this call; the
    /// `execve` runs in a spawned task after a short delay so
    /// the JSON-RPC reply flushes back to the caller before the
    /// process is replaced. Default implementation returns
    /// "not supported".
    async fn install_release(&self, _version: &str) -> InstallOutcome {
        InstallOutcome {
            scheduled: false,
            reason: "node does not support in-place install".into(),
            restart_in_secs: 0,
        }
    }

    /// Roll the running binary back to the snapshot saved at
    /// `<data-dir>/releases/.previous` (v0.0.80). Composes the
    /// inverse of [`Self::install_release`]: restore the
    /// snapshot atomically, then `execve` self into it. The
    /// rollback itself is reversible — after the swap,
    /// `.previous` holds the bytes we rolled away from, so a
    /// second `rollback_release` call flips the pair back.
    /// Default returns "not supported".
    async fn rollback_release(&self) -> InstallOutcome {
        InstallOutcome {
            scheduled: false,
            reason: "node does not support release rollback".into(),
            restart_in_secs: 0,
        }
    }

    /// Simulate a transaction via the EVM (v0.0.88 `eth_call`
    /// support). Returns the return bytes on success, an
    /// [`SimulateCallError`] otherwise. The default returns
    /// `Unsupported` — `aii-node::NodeState` overrides this to
    /// route through `aii_evm::simulate_with_revm`.
    async fn simulate_call(&self, _req: SimulateCallParams) -> Result<Vec<u8>, SimulateCallError> {
        Err(SimulateCallError::Unsupported)
    }
}

/// Parsed `eth_call` request handed to [`RpcState::simulate_call`].
///
/// The wire-shape [`EthCallRequest`] uses hex strings; this is
/// the typed form with everything decoded.
#[derive(Debug, Clone)]
pub struct SimulateCallParams {
    /// Caller address (decoded from `from`, defaults to all-zero).
    pub from: Address,
    /// Target contract address (decoded from `to`).
    pub to: Address,
    /// Wei value.
    pub value: U256,
    /// Call data.
    pub data: Vec<u8>,
    /// Gas limit for the simulation.
    pub gas_limit: u64,
    /// Gas price (mostly cosmetic in a simulation).
    pub gas_price: U256,
}

/// Errors from [`RpcState::simulate_call`].
#[derive(Debug, thiserror::Error)]
pub enum SimulateCallError {
    /// Host has no EVM wired up.
    #[error("eth_call not supported by this node")]
    Unsupported,
    /// Hex decode failure on one of the request fields.
    #[error("invalid hex in eth_call request: {0}")]
    Hex(String),
    /// EVM ran but the transaction reverted or halted.
    #[error("execution reverted: {0}")]
    Reverted(String),
    /// revm internal error (bad transaction shape, state lookup
    /// failure, etc.).
    #[error("evm: {0}")]
    Evm(String),
}

/// Result type carried back from [`RpcState::install_release`].
///
/// `scheduled = true` means the binary was successfully copied
/// into place and a self-restart will fire in `restart_in_secs`
/// seconds. `scheduled = false` means the install was rejected
/// (binary missing, version mismatch, I/O error, etc.) with the
/// reason in `reason`.
#[derive(Debug, Clone)]
pub struct InstallOutcome {
    /// Whether the binary was installed and a restart scheduled.
    pub scheduled: bool,
    /// Human-readable reason — empty on success, error detail
    /// when `scheduled` is `false`.
    pub reason: String,
    /// Seconds the host will wait before `execve`-ing the new
    /// binary. `0` when `scheduled` is `false`.
    pub restart_in_secs: u64,
}

/// JSON-RPC-facing view of an [`aii_block::Receipt`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReceiptView {
    /// Transaction hash this receipt belongs to (`0x…` hex).
    pub transaction_hash: String,
    /// Block number that included the tx (`0x…` hex).
    pub block_number: String,
    /// `0x01` if the tx succeeded, `0x00` otherwise.
    pub status: String,
    /// Cumulative gas used by this tx + all preceding txs in the block.
    pub cumulative_gas_used: String,
    /// 256-byte logs bloom as `0x…` hex.
    pub logs_bloom: String,
    /// Tx type string — `"legacy" | "eip1559" | "eip4844"`.
    pub tx_type: String,
    /// Emitted logs.
    pub logs: Vec<LogView>,
}

/// JSON-RPC-facing view of an [`aii_block::Log`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogView {
    /// Address that emitted the log (`0x…` hex).
    pub address: String,
    /// Indexed topics (`0x…` hex), at most 4.
    pub topics: Vec<String>,
    /// Non-indexed event data (`0x…` hex).
    pub data: String,
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

    /// `eth_getTransactionReceipt(hash)` — receipt for a finalised tx.
    /// Returns `null` if the tx is unknown, has no receipt on file, or
    /// was rejected pre-execution.
    #[method(name = "getTransactionReceipt")]
    async fn get_transaction_receipt(&self, hash: String) -> RpcResult<Option<ReceiptView>>;

    /// `eth_getLogs(filter)` — every log matching `filter` across the
    /// `from_block..=to_block` range. Block-level logs_bloom is the
    /// fast-path prefilter; matching blocks then walk their receipts
    /// linearly. An empty `address` / `topics` matches everything.
    #[method(name = "getLogs")]
    async fn get_logs(&self, filter: LogFilter) -> RpcResult<Vec<LogEntryView>>;

    /// `eth_call(req, blockTag)` — execute a transaction as a
    /// read-only simulation against the head state via revm
    /// (v0.0.88). State changes are discarded. Returns the
    /// hex-encoded return data on success. `blockTag` is
    /// currently ignored (only head-state is supported).
    #[method(name = "call")]
    async fn call(&self, req: EthCallRequest, block_tag: Option<String>) -> RpcResult<String>;

    /// `eth_estimateGas(req, blockTag)` — return the gas a
    /// transaction would consume if submitted, as `0x…` hex
    /// (v0.0.89). Runs the same simulation as `eth_call` but
    /// reports `gas_used` instead of the return data.
    /// `blockTag` ignored (head-state only).
    #[method(name = "estimateGas")]
    async fn estimate_gas(
        &self,
        req: EthCallRequest,
        block_tag: Option<String>,
    ) -> RpcResult<String>;

    /// `eth_getCode(address, blockTag)` — return the runtime
    /// bytecode at `address` as `0x…` hex (v0.0.89). Empty
    /// `0x` for EOAs or non-existent accounts. `blockTag`
    /// ignored (head-state only).
    #[method(name = "getCode")]
    async fn get_code(&self, address: String, block_tag: Option<String>) -> RpcResult<String>;

    /// `eth_getStorageAt(address, slot, blockTag)` — return
    /// the 32-byte storage value at `(address, slot)` as
    /// `0x…` hex (v0.0.89). Unset slots return
    /// `0x000…000`. `blockTag` ignored (head-state only).
    #[method(name = "getStorageAt")]
    async fn get_storage_at(
        &self,
        address: String,
        slot: String,
        block_tag: Option<String>,
    ) -> RpcResult<String>;

    /// `eth_getTransactionByHash(hash)` — return the tx body
    /// + block location for a finalised tx, or `null` if
    /// unknown (v0.0.90).
    #[method(name = "getTransactionByHash")]
    async fn get_transaction_by_hash(&self, hash: String) -> RpcResult<Option<EthTxResponse>>;

    /// `eth_getBlockByNumber(numberOrTag, fullTransactions)` —
    /// returns the block header + transactions for the given
    /// block (v0.0.90). `numberOrTag` accepts hex (`0x…`),
    /// decimal, or `"latest" | "earliest"`. `null` if the
    /// block doesn't exist.
    #[method(name = "getBlockByNumber")]
    async fn get_block_by_number(
        &self,
        number_or_tag: String,
        full_transactions: bool,
    ) -> RpcResult<Option<EthBlockResponse>>;

    /// `eth_getBlockByHash(hash, fullTransactions)` — same
    /// shape as [`Self::get_block_by_number`], looked up by
    /// 32-byte block hash. `null` if unknown.
    #[method(name = "getBlockByHash")]
    async fn get_block_by_hash(
        &self,
        hash: String,
        full_transactions: bool,
    ) -> RpcResult<Option<EthBlockResponse>>;
}

/// Request body for the v0.0.88 `eth_call` JSON-RPC method.
///
/// Mirrors the Ethereum JSON-RPC `eth_call` "transaction
/// object" — every field is optional except `to` (which is
/// required for a CALL; a CREATE simulation is not yet
/// exposed). All hex fields accept `0x…` or bare hex.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EthCallRequest {
    /// Caller address (`0x…` hex, 20 bytes). Defaults to all
    /// zeros — fine for view-function calls that don't read
    /// `msg.sender`.
    #[serde(default)]
    pub from: Option<String>,
    /// Target contract address (`0x…` hex). Required.
    pub to: String,
    /// Wei value (`0x…` hex). Defaults to `0x0`.
    #[serde(default)]
    pub value: Option<String>,
    /// Call data (`0x…` hex). Defaults to empty.
    #[serde(default)]
    pub data: Option<String>,
    /// Gas limit (`0x…` hex). Defaults to a generous
    /// `0x1c9c380` (30 M gas) — `eth_call` doesn't charge gas
    /// against any account, so the cap is just to prevent
    /// runaway execution.
    #[serde(default)]
    pub gas: Option<String>,
    /// Gas price (`0x…` hex Wei). Defaults to `0x0` — revm
    /// won't actually debit anything in a simulation, but
    /// some contracts read `tx.gasprice`.
    #[serde(default, rename = "gasPrice")]
    pub gas_price: Option<String>,
}

/// `eth_getLogs` request filter (subset of the Ethereum spec — block
/// hash filtering and topic-position OR-arrays land in a follow-up).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogFilter {
    /// Starting block number, inclusive. Hex `0x…` or decimal. `None`
    /// defaults to block 0.
    #[serde(default)]
    pub from_block: Option<String>,
    /// Ending block number, inclusive. `None` defaults to the head.
    #[serde(default)]
    pub to_block: Option<String>,
    /// Optional address filter — `None` matches every contract.
    #[serde(default)]
    pub address: Option<String>,
    /// Topic filter, exact-match positionally. Empty vec matches every
    /// log; a topic of `null` is a wildcard in that position (not yet
    /// supported — the value `"null"` is treated as a literal).
    #[serde(default)]
    pub topics: Vec<String>,
}

/// One log entry returned by `eth_getLogs`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogEntryView {
    /// Block number that contained the tx that emitted this log.
    pub block_number: String,
    /// Tx hash that emitted this log.
    pub transaction_hash: String,
    /// Address that emitted the log.
    pub address: String,
    /// Indexed topics.
    pub topics: Vec<String>,
    /// Non-indexed event data.
    pub data: String,
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

    /// `aii_getBlockTransactions(numberOrHash)` — every transaction
    /// inside a finalised block, in inclusion order. Returns `null` if
    /// the block is unknown; returns `[]` if the block exists but
    /// contains no transactions (the common no-traffic case).
    #[method(name = "getBlockTransactions")]
    async fn get_block_transactions(&self, query: String) -> RpcResult<Option<Vec<TxView>>>;

    /// `aii_getTransaction(hash)` — look up a single transaction by its
    /// keccak256 hash. Returns `null` if the hash is unknown; otherwise
    /// `{ tx, block_number }` so explorers can render a tx-detail page
    /// with a link back to its containing block.
    #[method(name = "getTransaction")]
    async fn get_transaction(&self, hash: String) -> RpcResult<Option<TxLookup>>;

    /// `aii_getRawBlock(numberOrHash)` — full RLP-encoded `Block`
    /// (header + body), returned as `0x…` hex. Lets a cold-joining
    /// node reconstruct a byte-identical block (and therefore the
    /// same block hash) without needing every header/body field
    /// shipped through their typed views. Returns `null` if unknown.
    #[method(name = "getRawBlock")]
    async fn get_raw_block(&self, query: String) -> RpcResult<Option<String>>;

    /// `aii_listSlashings` — every persisted slashing record on this
    /// node. Operational tooling uses this to spot equivocating
    /// validators across history. Empty array = no slashings yet.
    #[method(name = "listSlashings")]
    async fn list_slashings(&self) -> RpcResult<Vec<SlashView>>;

    /// `aii_getSubchainAnchor(sub_chain_id)` — most recent flush anchor
    /// recorded by the parent chain for sub-chain `id`. Returns `null`
    /// if no anchor has been flushed yet.
    #[method(name = "getSubchainAnchor")]
    async fn get_subchain_anchor(&self, id: u32) -> RpcResult<Option<SubchainAnchorView>>;

    /// `aii_getStake(address)` — staking record for `address` or `null`
    /// if no bond has ever been recorded. Used by validator dashboards
    /// + governance UIs.
    #[method(name = "getStake")]
    async fn get_stake(&self, address: String) -> RpcResult<Option<StakeView>>;

    /// `aii_totalStake` — sum of every currently-bonded stake on the
    /// chain. Returned as `0x…` hex (Wei). Denominator for any
    /// stake-weighted query.
    #[method(name = "totalStake")]
    async fn total_stake(&self) -> RpcResult<String>;

    /// `aii_listStakers` — every staker on record, in unspecified
    /// order. Empty array on a fresh chain.
    #[method(name = "listStakers")]
    async fn list_stakers(&self) -> RpcResult<Vec<StakeView>>;

    /// `aii_getActiveValidators` — the most recently elected DPoS
    /// validator set. Returns `{ epoch, validators: [...] }` with
    /// every entry hex-encoded. Returns `null` on a chain that has
    /// not yet crossed its first epoch boundary.
    #[method(name = "getActiveValidators")]
    async fn get_active_validators(&self) -> RpcResult<Option<ActiveValidatorsView>>;

    /// `aii_listProposals` — every governance proposal known to this
    /// node. Empty array on a fresh chain.
    #[method(name = "listProposals")]
    async fn list_proposals(&self) -> RpcResult<Vec<ProposalView>>;

    /// `aii_getProposal(id)` — single proposal lookup. Returns `null`
    /// if unknown. Tally fields (`yes_wei` / `no_wei`) are populated
    /// after `tally()` has finalised the vote.
    #[method(name = "getProposal")]
    async fn get_proposal(&self, id: u64) -> RpcResult<Option<ProposalView>>;

    /// `aii_listForks` — every fork-detection record recorded by the
    /// node. Empty on a healthy chain; non-empty when a competing
    /// block has been seen at an already-finalised height. v0.0.54
    /// is observability-only — re-org execution lands later.
    #[method(name = "listForks")]
    async fn list_forks(&self) -> RpcResult<Vec<ForkView>>;

    /// `aii_getPostRoots(block_hash)` — Yellow-Paper sidecar roots
    /// computed after applying every tx in the block. Lets a light
    /// client verify post-execution state without the header itself
    /// carrying these fields (header still embeds placeholders for
    /// backward-compatible block hashing). Returns `null` for an
    /// unknown block hash or one produced before v0.0.58.
    #[method(name = "getPostRoots")]
    async fn get_post_roots(&self, block_hash: String) -> RpcResult<Option<PostRootsView>>;

    /// `aii_announceRelease(manifest)` — gossip-style entry point
    /// for the auto-update protocol (v0.0.75). A peer that has
    /// pulled, verified, and installed a new release-signing
    /// manifest broadcasts it to every other peer via this RPC.
    /// The receiving node:
    ///
    /// 1. Verifies the Ed25519 signature against the pinned project
    ///    release-signing pubkey ([`aii_crypto::release::pinned_release_pubkey`]).
    /// 2. Compares the manifest's version against its currently
    ///    known latest; older or duplicate announcements are
    ///    ignored as `ok: false` without persisting.
    /// 3. Stores the manifest as the new "latest known" so
    ///    operators can query it via [`Self::aii_latestRelease`].
    ///
    /// The actual binary fetch + atomic install lands in v0.0.76;
    /// v0.0.75 ships the announcement-and-discovery wire format
    /// only.
    #[method(name = "announceRelease")]
    async fn announce_release(
        &self,
        manifest: ReleaseManifestView,
    ) -> RpcResult<AnnounceReleaseResult>;

    /// `aii_latestRelease()` — the latest signed release manifest
    /// this node has seen (via gossip or local CLI). Returns
    /// `null` on a node that has never received an announcement.
    #[method(name = "latestRelease")]
    async fn latest_release(&self) -> RpcResult<Option<ReleaseManifestView>>;

    /// `aii_getReleaseBinary(version)` — serve the cached binary
    /// for `version` as a `0x`-prefixed hex string (v0.0.76).
    /// Returns `null` if this node does not have the binary on
    /// disk. The caller is expected to verify the returned bytes
    /// against the SHA-256 in the manifest before trusting them
    /// (the served peer might be lagging on a different release).
    #[method(name = "getReleaseBinary")]
    async fn get_release_binary(&self, version: String) -> RpcResult<Option<String>>;

    /// `aii_importReleaseBinary(version, hex_bytes)` — accept a
    /// binary blob and store it locally **iff** its SHA-256
    /// matches the locally-known latest manifest for `version`
    /// (v0.0.76). The verification step is non-negotiable: this
    /// node will only persist a binary it can prove matches a
    /// signature it has already verified. Returns `{ accepted,
    /// reason }`.
    #[method(name = "importReleaseBinary")]
    async fn import_release_binary(
        &self,
        version: String,
        hex_bytes: String,
    ) -> RpcResult<ImportReleaseResult>;

    /// `aii_installRelease(version)` — atomically install the
    /// release binary cached at `<data-dir>/releases/<version>`
    /// over the running `aiid` and schedule an `execve` self-
    /// restart so the node comes back online on the new binary
    /// (v0.0.78). The install itself happens synchronously; the
    /// restart fires from a spawned task a few seconds later so
    /// the JSON-RPC reply makes it back to the caller.
    #[method(name = "installRelease")]
    async fn install_release(&self, version: String) -> RpcResult<InstallReleaseResult>;

    /// `aii_rollbackRelease()` — restore the binary saved at
    /// `<data-dir>/releases/.previous` over the running `aiid`
    /// and `execve` into it (v0.0.80). The pre-install
    /// snapshot is written by [`Self::install_release`] just
    /// before it clobbers the running binary, so a rollback is
    /// available iff at least one install has happened since
    /// the data directory was created. Reversible: a second
    /// rollback flips back to whatever was running before the
    /// first.
    #[method(name = "rollbackRelease")]
    async fn rollback_release(&self) -> RpcResult<InstallReleaseResult>;
}

/// Wire shape for the release manifest exchanged over JSON-RPC.
/// Mirrors `aii_crypto::release::ReleaseManifest` field-for-field;
/// the duplication lets RPC consumers depend only on `aii-rpc`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleaseManifestView {
    /// Semver-style version string identifying this binary build.
    pub version: String,
    /// Lowercase hex SHA-256 of the released binary.
    pub sha256_hex: String,
    /// Unix-seconds timestamp the manifest was signed at.
    pub timestamp_unix: u64,
    /// Lowercase hex Ed25519 signature over the canonical payload.
    pub ed25519_sig_hex: String,
}

impl From<aii_crypto::release::ReleaseManifest> for ReleaseManifestView {
    fn from(m: aii_crypto::release::ReleaseManifest) -> Self {
        Self {
            version: m.version,
            sha256_hex: m.sha256_hex,
            timestamp_unix: m.timestamp_unix,
            ed25519_sig_hex: m.ed25519_sig_hex,
        }
    }
}

impl From<ReleaseManifestView> for aii_crypto::release::ReleaseManifest {
    fn from(v: ReleaseManifestView) -> Self {
        Self {
            version: v.version,
            sha256_hex: v.sha256_hex,
            timestamp_unix: v.timestamp_unix,
            ed25519_sig_hex: v.ed25519_sig_hex,
        }
    }
}

/// Response envelope for [`AiiRpc::announce_release`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnnounceReleaseResult {
    /// `true` if the announcement was accepted (new + valid signature).
    /// `false` if it was rejected (older than the currently-known
    /// latest, or duplicate). Reason in [`Self::reason`].
    pub accepted: bool,
    /// Human-readable reason for `accepted=false`. Empty on success.
    pub reason: String,
}

/// Response envelope for [`AiiRpc::import_release_binary`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImportReleaseResult {
    /// `true` iff the supplied bytes hashed correctly and were
    /// written to `<data-dir>/releases/<version>`.
    pub accepted: bool,
    /// Human-readable reason for `accepted=false`. Empty on success.
    pub reason: String,
}

/// Response envelope for [`AiiRpc::install_release`].
///
/// `scheduled = true` means the binary at
/// `<data-dir>/releases/<version>` has been copied over the
/// running `aiid` and a self-`execve` will fire in
/// `restart_in_secs` seconds. `scheduled = false` means the
/// install was rejected (binary missing, version mismatch,
/// I/O error, not supported on this build) with the reason
/// in [`Self::reason`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstallReleaseResult {
    /// Whether the binary was installed and a restart scheduled.
    pub scheduled: bool,
    /// Human-readable reason — empty on success, error detail on rejection.
    pub reason: String,
    /// Seconds the host will wait before `execve`-ing the new
    /// binary. `0` when `scheduled` is `false`.
    pub restart_in_secs: u64,
}

impl From<InstallOutcome> for InstallReleaseResult {
    fn from(o: InstallOutcome) -> Self {
        Self {
            scheduled: o.scheduled,
            reason: o.reason,
            restart_in_secs: o.restart_in_secs,
        }
    }
}

/// JSON-RPC view of the post-block Yellow-Paper roots.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PostRootsView {
    /// World-state MPT root after applying every tx (`0x…` hex).
    pub state_root: String,
    /// Receipts MPT root over the block's receipts (`0x…` hex).
    pub receipts_root: String,
    /// Aggregate 256-byte logs bloom (`0x…` hex).
    pub logs_bloom: String,
}

/// JSON-RPC view of one fork-detection record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ForkView {
    /// Height at which the fork was detected (`0x…` hex).
    pub height: String,
    /// Local canonical block hash at that height (`0x…` hex).
    pub canonical_hash: String,
    /// Rejected conflicting hash (`0x…` hex).
    pub fork_hash: String,
}

/// JSON-RPC view of a governance proposal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProposalView {
    /// Proposal id (`0x…` hex).
    pub id: String,
    /// Free-form title / description.
    pub title: String,
    /// Block at which voting ends (`0x…` hex).
    pub voting_ends_at: String,
    /// Life-cycle status — `"pending" / "passed" / "rejected" / "executed"`.
    pub status: String,
    /// Proposer address (`0x…` hex).
    pub proposer: String,
    /// Sum of yes-vote weights (`0x…` hex Wei). `"0x0"` before tally.
    pub yes_wei: String,
    /// Sum of no-vote weights (`0x…` hex Wei). `"0x0"` before tally.
    pub no_wei: String,
}

/// JSON-RPC view of the elected validator set at one epoch boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveValidatorsView {
    /// Epoch index this election was recorded at (`0x…` hex).
    pub epoch: String,
    /// Elected entries, in protocol sort order (stake desc, addr asc).
    pub validators: Vec<ValidatorEntryView>,
}

/// JSON-RPC view of one DPoS validator entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidatorEntryView {
    /// Validator address (`0x…` hex).
    pub address: String,
    /// Bonded stake at election time (`0x…` hex Wei).
    pub stake_wei: String,
    /// Registered BLS pubkey, if the epoch record carries runtime keys.
    pub bls_pubkey: Option<String>,
    /// Registered VRF pubkey, if the epoch record carries runtime keys.
    pub vrf_pubkey: Option<String>,
}

/// JSON-RPC view of a single staking record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StakeView {
    /// Staker address (`0x…` hex).
    pub address: String,
    /// Bonded amount in Wei (`0x…` hex).
    pub amount_wei: String,
    /// Block at which the bond becomes withdrawable (`0x…` hex). `0x0`
    /// means "still actively bonded — no unbond requested yet".
    pub unbond_at: String,
    /// `true` while the stake counts toward the elected validator set.
    pub is_bonded: bool,
}

/// JSON-RPC view of a sub-chain flush anchor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubchainAnchorView {
    /// Sub-chain block hash that was checkpointed (`0x…` hex).
    pub sub_block_hash: String,
    /// Parent-chain block hash that carries the checkpoint (`0x…` hex).
    pub parent_block_hash: String,
    /// Sub-chain block number at the time of the checkpoint (`0x…` hex).
    pub sub_block_number: String,
}

/// JSON-RPC view of a slashing record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SlashView {
    /// Validator index that signed both conflicting votes.
    pub validator_index: u32,
    /// Block height of the equivocation (`0x…` hex).
    pub height: String,
    /// BFT phase: `"prevote"` or `"precommit"`.
    pub phase: String,
    /// Two conflicting block hashes (`0x…` hex), in canonical sort order.
    pub block_hashes: [String; 2],
}

/// Response shape for `aii_getTransaction`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TxLookup {
    /// The transaction record itself.
    pub tx: TxView,
    /// Block number that included this transaction (`0x…` hex).
    pub block_number: String,
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

    async fn get_transaction_receipt(&self, hash: String) -> RpcResult<Option<ReceiptView>> {
        Ok(self.state.receipt_by_tx_hash(&hash).await)
    }

    async fn get_logs(&self, filter: LogFilter) -> RpcResult<Vec<LogEntryView>> {
        Ok(self.state.logs_in_range(&filter).await)
    }

    async fn call(&self, req: EthCallRequest, _block_tag: Option<String>) -> RpcResult<String> {
        let params = parse_eth_call_request(&req)?;
        match self.state.simulate_call(params).await {
            Ok(bytes) => Ok(format!("0x{}", hex::encode(bytes))),
            Err(e) => Err(jsonrpsee::types::ErrorObjectOwned::owned(
                -32000,
                e.to_string(),
                None::<()>,
            )),
        }
    }

    async fn estimate_gas(
        &self,
        req: EthCallRequest,
        _block_tag: Option<String>,
    ) -> RpcResult<String> {
        let params = parse_eth_call_request(&req)?;
        match self.state.estimate_gas(params).await {
            Ok(g) => Ok(format!("0x{g:x}")),
            Err(e) => Err(jsonrpsee::types::ErrorObjectOwned::owned(
                -32000,
                e.to_string(),
                None::<()>,
            )),
        }
    }

    async fn get_code(&self, address: String, _block_tag: Option<String>) -> RpcResult<String> {
        let addr = parse_address(&address)?;
        let code = self.state.code_at(&addr).await;
        Ok(format!("0x{}", hex::encode(code)))
    }

    async fn get_storage_at(
        &self,
        address: String,
        slot: String,
        _block_tag: Option<String>,
    ) -> RpcResult<String> {
        let addr = parse_address(&address)?;
        let slot_h = parse_h256(&slot)?;
        let value = self.state.storage_at(&addr, &slot_h).await;
        Ok(format!("0x{}", hex::encode(value.as_bytes())))
    }

    async fn get_transaction_by_hash(&self, hash: String) -> RpcResult<Option<EthTxResponse>> {
        Ok(self.state.eth_transaction_by_hash(&hash).await)
    }

    async fn get_block_by_number(
        &self,
        number_or_tag: String,
        full_transactions: bool,
    ) -> RpcResult<Option<EthBlockResponse>> {
        let head = self.state.head_block_number().await;
        let Some(n) = parse_block_number_or_tag(&number_or_tag, head) else {
            return Err(jsonrpsee::types::ErrorObjectOwned::owned(
                -32602,
                format!("invalid block number/tag: {number_or_tag}"),
                None::<()>,
            ));
        };
        Ok(self.state.eth_block_by_number(n, full_transactions).await)
    }

    async fn get_block_by_hash(
        &self,
        hash: String,
        full_transactions: bool,
    ) -> RpcResult<Option<EthBlockResponse>> {
        Ok(self.state.eth_block_by_hash(&hash, full_transactions).await)
    }
}

/// Parse an Ethereum-JSON-RPC block-number argument.
///
/// Accepts:
/// - `"latest"` / `"pending"` / `"safe"` / `"finalized"` → `head`
/// - `"earliest"` → `0`
/// - `"0x…"` hex → decoded number
/// - decimal digits → parsed as base-10
///
/// Returns `None` on parse failure. Unlike Ethereum, "pending"
/// is treated as "latest" — AII has no separate pending block.
fn parse_block_number_or_tag(s: &str, head: u64) -> Option<u64> {
    match s {
        "latest" | "pending" | "safe" | "finalized" => Some(head),
        "earliest" => Some(0),
        s if s.starts_with("0x") || s.starts_with("0X") => u64::from_str_radix(&s[2..], 16).ok(),
        s => s.parse::<u64>().ok(),
    }
}

/// Parse a `0x…` 32-byte hex string into an `H256`. Accepts
/// shorter hex by zero-padding on the left (matches Ethereum
/// JSON-RPC behavior for `eth_getStorageAt`'s slot arg, where
/// callers commonly pass `"0x0"`).
fn parse_h256(s: &str) -> RpcResult<aii_types::H256> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if s.len() > 64 {
        return Err(jsonrpsee::types::ErrorObjectOwned::owned(
            -32602,
            format!("h256: expected at most 64 hex chars, got {}", s.len()),
            None::<()>,
        ));
    }
    // Left-pad to 64 chars.
    let padded = format!("{s:0>64}");
    let mut out = [0u8; 32];
    hex::decode_to_slice(&padded, &mut out).map_err(|e| {
        jsonrpsee::types::ErrorObjectOwned::owned(-32602, format!("h256 hex: {e}"), None::<()>)
    })?;
    Ok(aii_types::H256::new(out))
}

/// Decode an [`EthCallRequest`] (hex strings) into a typed
/// [`SimulateCallParams`] for the host.
fn parse_eth_call_request(req: &EthCallRequest) -> RpcResult<SimulateCallParams> {
    let to = parse_address(&req.to)?;
    let from = match req.from.as_deref() {
        Some(s) => parse_address(s)?,
        None => Address::new([0u8; 20]),
    };
    let value = parse_hex_u256(req.value.as_deref().unwrap_or("0x0"))?;
    let data = match req.data.as_deref() {
        Some(s) => {
            let s = s.strip_prefix("0x").unwrap_or(s);
            hex::decode(s).map_err(|e| {
                jsonrpsee::types::ErrorObjectOwned::owned(
                    -32602,
                    format!("data: bad hex: {e}"),
                    None::<()>,
                )
            })?
        }
        None => Vec::new(),
    };
    let gas_limit = parse_hex_u64(req.gas.as_deref().unwrap_or("0x1c9c380"))?;
    let gas_price = parse_hex_u256(req.gas_price.as_deref().unwrap_or("0x0"))?;
    Ok(SimulateCallParams {
        from,
        to,
        value,
        data,
        gas_limit,
        gas_price,
    })
}

fn parse_hex_u256(s: &str) -> RpcResult<U256> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let s = if s.is_empty() { "0" } else { s };
    U256::from_str_radix(s, 16).map_err(|e| {
        jsonrpsee::types::ErrorObjectOwned::owned(-32602, format!("u256 hex: {e}"), None::<()>)
    })
}

fn parse_hex_u64(s: &str) -> RpcResult<u64> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let s = if s.is_empty() { "0" } else { s };
    u64::from_str_radix(s, 16).map_err(|e| {
        jsonrpsee::types::ErrorObjectOwned::owned(-32602, format!("u64 hex: {e}"), None::<()>)
    })
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

    async fn get_block_transactions(&self, query: String) -> RpcResult<Option<Vec<TxView>>> {
        // Resolve {decimal | "0x..." hex number | "0x..." 32-byte hash} → block number.
        let trimmed = query.strip_prefix("0x").unwrap_or(&query);
        if trimmed.len() == 64 {
            // Hash → header lookup → number → body lookup.
            let Some(h) = self.state.header_by_hash(&format!("0x{trimmed}")).await else {
                return Ok(None);
            };
            let n_hex = h.number.trim_start_matches("0x");
            let n = u64::from_str_radix(n_hex, 16).map_err(|e| {
                jsonrpsee::types::ErrorObjectOwned::owned(
                    -32603,
                    format!("internal: header.number not parseable: {e}"),
                    None::<()>,
                )
            })?;
            return Ok(self.state.block_transactions(n).await);
        }
        let n = if let Some(rest) = query.strip_prefix("0x") {
            u64::from_str_radix(rest, 16)
        } else {
            query.parse::<u64>()
        }
        .map_err(|e| {
            jsonrpsee::types::ErrorObjectOwned::owned(
                -32602,
                format!(
                    "getBlockTransactions: '{query}' is neither a number nor a 32-byte hash: {e}"
                ),
                None::<()>,
            )
        })?;
        Ok(self.state.block_transactions(n).await)
    }

    async fn get_transaction(&self, hash: String) -> RpcResult<Option<TxLookup>> {
        let trimmed = hash.strip_prefix("0x").unwrap_or(&hash);
        if trimmed.len() != 64 {
            return Err(jsonrpsee::types::ErrorObjectOwned::owned(
                -32602,
                "getTransaction: hash must be 0x + 64 hex chars",
                None::<()>,
            ));
        }
        let normalized = format!("0x{trimmed}");
        Ok(self
            .state
            .transaction_by_hash(&normalized)
            .await
            .map(|(tx, block_number)| TxLookup {
                tx,
                block_number: format!("0x{block_number:x}"),
            }))
    }

    async fn get_raw_block(&self, query: String) -> RpcResult<Option<String>> {
        Ok(self.state.raw_block(&query).await)
    }

    async fn list_slashings(&self) -> RpcResult<Vec<SlashView>> {
        Ok(self.state.slashings().await)
    }

    async fn get_subchain_anchor(&self, id: u32) -> RpcResult<Option<SubchainAnchorView>> {
        Ok(self.state.subchain_anchor(id).await)
    }

    async fn get_stake(&self, address: String) -> RpcResult<Option<StakeView>> {
        let addr = parse_address(&address)?;
        Ok(self.state.stake_at(&addr).await)
    }

    async fn total_stake(&self) -> RpcResult<String> {
        let t = self.state.total_bonded_stake().await;
        Ok(format!("0x{t:x}"))
    }

    async fn list_stakers(&self) -> RpcResult<Vec<StakeView>> {
        Ok(self.state.all_stakers().await)
    }

    async fn get_active_validators(&self) -> RpcResult<Option<ActiveValidatorsView>> {
        Ok(self.state.active_validator_set().await)
    }

    async fn list_proposals(&self) -> RpcResult<Vec<ProposalView>> {
        Ok(self.state.governance_proposals().await)
    }

    async fn get_proposal(&self, id: u64) -> RpcResult<Option<ProposalView>> {
        Ok(self.state.governance_proposal(id).await)
    }

    async fn list_forks(&self) -> RpcResult<Vec<ForkView>> {
        Ok(self.state.forks().await)
    }

    async fn get_post_roots(&self, block_hash: String) -> RpcResult<Option<PostRootsView>> {
        Ok(self.state.post_roots_for(&block_hash).await)
    }

    async fn announce_release(
        &self,
        manifest: ReleaseManifestView,
    ) -> RpcResult<AnnounceReleaseResult> {
        // 1. Build the canonical payload from the manifest body.
        let m: aii_crypto::release::ReleaseManifest = manifest.into();
        // 2. Re-hash the binary? No — we don't have it locally yet.
        // The signature is over (domain || version || nul || sha256
        // || timestamp_be). All four are in the manifest, so we can
        // verify with just the manifest bytes.
        let sha_bytes = match hex::decode(m.sha256_hex.trim_start_matches("0x")) {
            Ok(b) if b.len() == 32 => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&b);
                arr
            }
            _ => {
                return Ok(AnnounceReleaseResult {
                    accepted: false,
                    reason: "manifest.sha256_hex is not 32 bytes of hex".into(),
                });
            }
        };
        let payload =
            aii_crypto::release::canonical_payload(&m.version, &sha_bytes, m.timestamp_unix);
        let sig = match aii_crypto::ed25519::Signature::from_hex(&m.ed25519_sig_hex) {
            Ok(s) => s,
            Err(e) => {
                return Ok(AnnounceReleaseResult {
                    accepted: false,
                    reason: format!("malformed signature: {e}"),
                })
            }
        };
        // v0.0.87: verify against the FULL pinned-pubkey set
        // (multi-key rotation support). A manifest signed by any
        // key in the compile-time list is accepted.
        let pubkeys = aii_crypto::release::pinned_release_pubkeys();
        let mut sig_ok = false;
        for pk in &pubkeys {
            if pk.verify(&payload, &sig).is_ok() {
                sig_ok = true;
                break;
            }
        }
        if !sig_ok {
            return Ok(AnnounceReleaseResult {
                accepted: false,
                reason: format!(
                    "signature does not verify against any of the {} pinned pubkeys",
                    pubkeys.len()
                ),
            });
        }
        // 3. Hand off to the host. Implementations decide whether
        // this is "newer" than what they have and persist it.
        let stored = self.state.record_release_announcement(m.clone()).await;
        if stored {
            // v0.0.77: fire-and-forget propagation to peers. The
            // host's `update_peers_for_release` returns the
            // operator-configured `--update-peers` list. Each peer
            // will re-verify the signature against its own pinned
            // pubkey, so this hop carries no extra trust.
            let peers = self.state.update_peers_for_release().await;
            if !peers.is_empty() {
                let state = self.state.clone();
                let manifest = m;
                tokio::spawn(async move {
                    let outcome =
                        crate::release_gossip::propagate_release(state, manifest, peers).await;
                    tracing::info!(
                        peers = outcome.peers.len(),
                        any_imported = outcome.peers.iter().any(|p| p.binary_imported),
                        "release propagation done"
                    );
                });
            }
            Ok(AnnounceReleaseResult {
                accepted: true,
                reason: String::new(),
            })
        } else {
            Ok(AnnounceReleaseResult {
                accepted: false,
                reason: "not newer than currently known latest".into(),
            })
        }
    }

    async fn latest_release(&self) -> RpcResult<Option<ReleaseManifestView>> {
        Ok(self.state.latest_release().await.map(Into::into))
    }

    async fn get_release_binary(&self, version: String) -> RpcResult<Option<String>> {
        Ok(self
            .state
            .release_binary_bytes(&version)
            .await
            .map(|b| format!("0x{}", hex::encode(b))))
    }

    async fn import_release_binary(
        &self,
        version: String,
        hex_bytes: String,
    ) -> RpcResult<ImportReleaseResult> {
        let stripped = hex_bytes.trim_start_matches("0x");
        let bytes = match hex::decode(stripped) {
            Ok(b) => b,
            Err(e) => {
                return Ok(ImportReleaseResult {
                    accepted: false,
                    reason: format!("hex decode: {e}"),
                });
            }
        };
        let (accepted, reason) = self.state.import_release_binary(&version, bytes).await;
        Ok(ImportReleaseResult { accepted, reason })
    }

    async fn install_release(&self, version: String) -> RpcResult<InstallReleaseResult> {
        let outcome = self.state.install_release(&version).await;
        Ok(outcome.into())
    }

    async fn rollback_release(&self) -> RpcResult<InstallReleaseResult> {
        let outcome = self.state.rollback_release().await;
        Ok(outcome.into())
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
/// Cap on inbound JSON-RPC request body size.
///
/// jsonrpsee's default is 10 MiB, which truncates the
/// hex-encoded `aii_importReleaseBinary` call for any aiid
/// build above ~5 MiB (hex doubles the size). 128 MiB leaves
/// comfortable headroom for the current ~16 MiB release binary
/// while keeping the surface narrow enough to reject obvious
/// abuse.
pub const MAX_REQUEST_BODY_SIZE: u32 = 128 * 1024 * 1024;

/// Cap on outbound JSON-RPC response body size.
///
/// `aii_getReleaseBinary` serves the cached binary as a hex
/// string — the response is roughly 2× the binary size. Keep
/// this in step with [`MAX_REQUEST_BODY_SIZE`].
pub const MAX_RESPONSE_BODY_SIZE: u32 = 128 * 1024 * 1024;

/// Bind an HTTP JSON-RPC server to `addr` backed by `state`.
///
/// Configures jsonrpsee with [`MAX_REQUEST_BODY_SIZE`] /
/// [`MAX_RESPONSE_BODY_SIZE`] to accept the full hex-encoded
/// release-binary payloads used by `aii_importReleaseBinary` and
/// `aii_getReleaseBinary` (v0.0.76 onwards).
///
/// # Errors
///
/// I/O failure binding the socket or registering RPC modules.
pub async fn serve<S: RpcState>(
    addr: SocketAddr,
    state: Arc<S>,
) -> Result<(SocketAddr, ServerHandle), RpcError> {
    let cfg = jsonrpsee::server::ServerConfig::builder()
        .max_request_body_size(MAX_REQUEST_BODY_SIZE)
        .max_response_body_size(MAX_RESPONSE_BODY_SIZE)
        .build();
    let server = Server::builder()
        .set_config(cfg)
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

    // ──────────────── v0.0.90: block-tag parser unit tests ────────────────

    #[test]
    fn block_tag_parser_handles_named_tags() {
        assert_eq!(parse_block_number_or_tag("latest", 100), Some(100));
        assert_eq!(parse_block_number_or_tag("pending", 100), Some(100));
        assert_eq!(parse_block_number_or_tag("safe", 100), Some(100));
        assert_eq!(parse_block_number_or_tag("finalized", 100), Some(100));
        assert_eq!(parse_block_number_or_tag("earliest", 100), Some(0));
    }

    #[test]
    fn block_tag_parser_handles_hex() {
        assert_eq!(parse_block_number_or_tag("0x0", 100), Some(0));
        assert_eq!(parse_block_number_or_tag("0x1", 100), Some(1));
        assert_eq!(parse_block_number_or_tag("0xff", 100), Some(255));
        assert_eq!(parse_block_number_or_tag("0X10", 100), Some(16));
    }

    #[test]
    fn block_tag_parser_handles_decimal() {
        assert_eq!(parse_block_number_or_tag("0", 100), Some(0));
        assert_eq!(parse_block_number_or_tag("42", 100), Some(42));
        assert_eq!(parse_block_number_or_tag("1000", 100), Some(1000));
    }

    #[test]
    fn block_tag_parser_rejects_garbage() {
        assert_eq!(parse_block_number_or_tag("garbage", 100), None);
        assert_eq!(parse_block_number_or_tag("0xZZ", 100), None);
        assert_eq!(parse_block_number_or_tag("", 100), None);
    }

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

    use sha2::Digest as _;

    /// Stateful TestState that actually persists release announcements,
    /// used to exercise the v0.0.75 RPC wiring end-to-end.
    struct ReleaseTestState {
        chain_id: u64,
        network: String,
        head: u64,
        latest: std::sync::Mutex<Option<aii_crypto::release::ReleaseManifest>>,
        /// version → bytes cache for the v0.0.76 import / get tests.
        binaries: std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>,
        /// Peer URLs exposed via `update_peers_for_release` (v0.0.77).
        update_peers: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait]
    impl RpcState for ReleaseTestState {
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
            U256::from(1u64)
        }
        async fn account(&self, _addr: &Address) -> Option<AccountView> {
            None
        }
        async fn record_release_announcement(
            &self,
            m: aii_crypto::release::ReleaseManifest,
        ) -> bool {
            let mut g = self.latest.lock().unwrap();
            if let Some(ref cur) = *g {
                if m.timestamp_unix <= cur.timestamp_unix {
                    return false;
                }
            }
            *g = Some(m);
            true
        }
        async fn latest_release(&self) -> Option<aii_crypto::release::ReleaseManifest> {
            self.latest.lock().unwrap().clone()
        }
        async fn release_binary_bytes(&self, version: &str) -> Option<Vec<u8>> {
            self.binaries.lock().unwrap().get(version).cloned()
        }
        async fn update_peers_for_release(&self) -> Vec<String> {
            self.update_peers.lock().unwrap().clone()
        }
        async fn import_release_binary(&self, version: &str, bytes: Vec<u8>) -> (bool, String) {
            // Verify bytes hash matches the manifest's claimed sha256.
            let snapshot = self.latest.lock().unwrap().clone();
            let Some(manifest) = snapshot else {
                return (false, "no manifest".into());
            };
            if manifest.version != version {
                return (false, "version mismatch".into());
            }
            let mut h = sha2::Sha256::new();
            h.update(&bytes);
            let computed = hex::encode(<[u8; 32]>::from(h.finalize()));
            if computed != manifest.sha256_hex.trim_start_matches("0x").to_lowercase() {
                return (false, "hash mismatch".into());
            }
            self.binaries
                .lock()
                .unwrap()
                .insert(version.to_string(), bytes);
            (true, String::new())
        }
        // v0.0.78: in-test install simulates success when the
        // binary is cached, otherwise rejects. No real file I/O
        // or execve — the fixture just records the call.
        async fn install_release(&self, version: &str) -> InstallOutcome {
            let has = self.binaries.lock().unwrap().contains_key(version);
            if has {
                InstallOutcome {
                    scheduled: true,
                    reason: String::new(),
                    restart_in_secs: 2,
                }
            } else {
                InstallOutcome {
                    scheduled: false,
                    reason: format!("no cached binary for {version}"),
                    restart_in_secs: 0,
                }
            }
        }
        // v0.0.80: in-test rollback simulates success when at
        // least one binary has been cached (treating "binary
        // exists in fixture" as a proxy for "a pre-install
        // snapshot also exists"). Otherwise rejects.
        async fn rollback_release(&self) -> InstallOutcome {
            let any = !self.binaries.lock().unwrap().is_empty();
            if any {
                InstallOutcome {
                    scheduled: true,
                    reason: String::new(),
                    restart_in_secs: 2,
                }
            } else {
                InstallOutcome {
                    scheduled: false,
                    reason: "no pre-install snapshot".into(),
                    restart_in_secs: 0,
                }
            }
        }
    }

    fn release_fixture() -> Arc<ReleaseTestState> {
        Arc::new(ReleaseTestState {
            chain_id: 9999,
            network: "aii-testnet".into(),
            head: 0,
            latest: std::sync::Mutex::new(None),
            binaries: std::sync::Mutex::new(std::collections::HashMap::new()),
            update_peers: std::sync::Mutex::new(Vec::new()),
        })
    }

    async fn spawn_release() -> (String, ServerHandle) {
        let (addr, handle) = serve("127.0.0.1:0".parse().unwrap(), release_fixture())
            .await
            .unwrap();
        (format!("http://{addr}"), handle)
    }

    /// Happy path: sign a release with the pinned-pubkey's secret
    /// (we generate a *different* secret here and override the
    /// pinned constant in-process — actually we can't override the
    /// const, so we sign with a random key and assert the RPC
    /// REJECTS it. The accept-path is covered by a separate test
    /// that uses the same const-derived flow.).
    #[tokio::test]
    async fn aii_announce_release_rejects_unsigned_manifest() {
        use aii_crypto::ed25519::SecretKey;
        use aii_crypto::release::sign_release;
        use rand_core::OsRng;
        use std::io::Write;

        let (url, h) = spawn_release().await;
        let c = HttpClientBuilder::default().build(url).unwrap();

        // Manifest signed by a key OTHER than the pinned one: must be
        // rejected with `accepted: false`.
        let mut rng = OsRng;
        let sk = SecretKey::generate(&mut rng);
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"fake binary").unwrap();
        let manifest = sign_release(&sk, tmp.path(), "0.0.99", 1_716_800_000).unwrap();
        let view = ReleaseManifestView::from(manifest);
        let r: AnnounceReleaseResult = c
            .request("aii_announceRelease", rpc_params![view])
            .await
            .unwrap();
        assert!(!r.accepted, "manifest signed by wrong key must be rejected");
        assert!(
            r.reason.contains("signature"),
            "reason should mention signature: {r:?}"
        );

        // And `aii_latestRelease` should still return null.
        let latest: Option<ReleaseManifestView> =
            c.request("aii_latestRelease", rpc_params![]).await.unwrap();
        assert!(latest.is_none());
        h.stop().unwrap();
    }

    /// `aii_latestRelease` on a fresh node returns `null`.
    #[tokio::test]
    async fn aii_latest_release_fresh_node_returns_null() {
        let (url, h) = spawn_release().await;
        let c = HttpClientBuilder::default().build(url).unwrap();
        let r: Option<ReleaseManifestView> =
            c.request("aii_latestRelease", rpc_params![]).await.unwrap();
        assert!(r.is_none());
        h.stop().unwrap();
    }

    /// v0.0.76 — `aii_getReleaseBinary` returns `null` for a
    /// version the node has never seen.
    #[tokio::test]
    async fn aii_get_release_binary_missing_returns_null() {
        let (url, h) = spawn_release().await;
        let c = HttpClientBuilder::default().build(url).unwrap();
        let r: Option<String> = c
            .request("aii_getReleaseBinary", rpc_params!["0.0.76"])
            .await
            .unwrap();
        assert!(r.is_none());
        h.stop().unwrap();
    }

    /// v0.0.76 — announce → import → get round-trips the binary
    /// bytes via the JSON-RPC layer.
    #[tokio::test]
    async fn aii_import_release_binary_round_trip() {
        use aii_crypto::ed25519::SecretKey;
        use aii_crypto::release::sign_release;
        use std::io::Write;

        const PINNED_SECRET_HEX: &str =
            "be06b95cb0e2d44ee175cc7a475ea4e9fcab47a784d161c36978b34e28ceeb97";
        let sk = SecretKey::from_hex(PINNED_SECRET_HEX).unwrap();

        let (url, h) = spawn_release().await;
        let c = HttpClientBuilder::default().build(url).unwrap();

        // 1. Sign a manifest over real bytes.
        let payload = b"v0.0.76 release binary blob";
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(payload).unwrap();
        let manifest = sign_release(&sk, tmp.path(), "0.0.76", 1_900_000_001).unwrap();

        // 2. Announce it; the in-test state stores it under `latest`.
        let view = ReleaseManifestView::from(manifest.clone());
        let r: AnnounceReleaseResult = c
            .request("aii_announceRelease", rpc_params![view])
            .await
            .unwrap();
        assert!(r.accepted);

        // 3. Import the binary bytes (hex-encoded). The in-test
        //    state hashes them and only stores on match.
        let hex_bytes = format!("0x{}", hex::encode(payload));
        let imp: ImportReleaseResult = c
            .request(
                "aii_importReleaseBinary",
                rpc_params!["0.0.76", hex_bytes.clone()],
            )
            .await
            .unwrap();
        assert!(imp.accepted, "happy-path import rejected: {imp:?}");

        // 4. Fetch the binary back via getReleaseBinary.
        let got: Option<String> = c
            .request("aii_getReleaseBinary", rpc_params!["0.0.76"])
            .await
            .unwrap();
        assert_eq!(got.as_deref(), Some(hex_bytes.as_str()));

        // 5. Import with WRONG bytes is rejected; cache untouched.
        let bad_hex = format!("0x{}", hex::encode(b"tampered"));
        let imp_bad: ImportReleaseResult = c
            .request("aii_importReleaseBinary", rpc_params!["0.0.76", bad_hex])
            .await
            .unwrap();
        assert!(!imp_bad.accepted);
        assert!(imp_bad.reason.contains("hash"));

        // Independent sanity check that the local hash matches.
        let mut hh = sha2::Sha256::new();
        hh.update(payload);
        let _: [u8; 32] = hh.finalize().into();
        h.stop().unwrap();
    }

    /// Happy path: sign a manifest with the *actual* pinned secret
    /// seed (the development-project key whose public half is
    /// compiled in via `RELEASE_SIGNING_PUBKEY_HEX`). The RPC
    /// announce must accept it and the subsequent latest-query must
    /// echo the same manifest back.
    ///
    /// The hex secret here is the project's dev-time release-signing
    /// seed — full rotation flow + secret-management policy lands
    /// before mainnet; on the testnet this is operator key material,
    /// not validator material.
    #[tokio::test]
    async fn aii_announce_release_accepts_pinned_pubkey_signature() {
        use aii_crypto::ed25519::SecretKey;
        use aii_crypto::release::{sign_release, RELEASE_SIGNING_PUBKEY_HEX};
        use std::io::Write;

        const PINNED_SECRET_HEX: &str =
            "be06b95cb0e2d44ee175cc7a475ea4e9fcab47a784d161c36978b34e28ceeb97";

        // Sanity-check: the secret here pairs with the pinned pubkey.
        let sk = SecretKey::from_hex(PINNED_SECRET_HEX).unwrap();
        assert_eq!(sk.public().to_hex(), RELEASE_SIGNING_PUBKEY_HEX);

        let (url, h) = spawn_release().await;
        let c = HttpClientBuilder::default().build(url).unwrap();

        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"some binary contents").unwrap();
        let manifest = sign_release(&sk, tmp.path(), "0.0.75", 1_900_000_000).unwrap();
        let view: ReleaseManifestView = manifest.into();
        let r: AnnounceReleaseResult = c
            .request("aii_announceRelease", rpc_params![view.clone()])
            .await
            .unwrap();
        assert!(r.accepted, "pinned-key signature must be accepted: {r:?}");

        // The follow-up latest-query returns the same manifest.
        let latest: Option<ReleaseManifestView> =
            c.request("aii_latestRelease", rpc_params![]).await.unwrap();
        assert_eq!(latest.as_ref(), Some(&view));

        // Re-announcing the same manifest is a no-op (not strictly
        // newer).
        let r2: AnnounceReleaseResult = c
            .request("aii_announceRelease", rpc_params![view])
            .await
            .unwrap();
        assert!(!r2.accepted, "same-timestamp re-announce must reject");

        h.stop().unwrap();
    }

    /// v0.0.77 — two-node integration: A's `update_peers` points
    /// at B. After A accepts an announcement, A's announce handler
    /// spawns a propagate task that calls B's `aii_announceRelease`
    /// and (if B is missing the binary) B's `aii_importReleaseBinary`.
    /// At the end of the test, B's `latest` and `binaries` slots are
    /// both populated. Multi-thread runtime so the spawned propagate
    /// task can drive its HTTP call while the test loop polls B.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn release_gossip_two_node_propagate() {
        use aii_crypto::ed25519::SecretKey;
        use aii_crypto::release::sign_release;
        use std::io::Write;

        const PINNED_SECRET_HEX: &str =
            "be06b95cb0e2d44ee175cc7a475ea4e9fcab47a784d161c36978b34e28ceeb97";
        let sk = SecretKey::from_hex(PINNED_SECRET_HEX).unwrap();

        // Spin up node B with a blank state — A will gossip to it.
        let state_b = release_fixture();
        let (addr_b, handle_b) = serve("127.0.0.1:0".parse().unwrap(), state_b.clone())
            .await
            .unwrap();
        let url_b = format!("http://{addr_b}");

        // Node A — list B as its update peer.
        let state_a = release_fixture();
        *state_a.update_peers.lock().unwrap() = vec![url_b.clone()];
        let (addr_a, handle_a) = serve("127.0.0.1:0".parse().unwrap(), state_a.clone())
            .await
            .unwrap();
        let url_a = format!("http://{addr_a}");

        // Sign a fresh release manifest using the pinned key.
        let payload = b"v0.0.77 propagated release body";
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(payload).unwrap();
        let manifest = sign_release(&sk, tmp.path(), "0.0.77", 1_900_000_002).unwrap();
        // Pre-load A's binary cache so the propagate task can serve
        // it to B via getReleaseBinary.
        state_a
            .binaries
            .lock()
            .unwrap()
            .insert("0.0.77".to_string(), payload.to_vec());

        // Announce to A — this should trigger background propagation to B.
        let client_a = HttpClientBuilder::default().build(&url_a).unwrap();
        let view: ReleaseManifestView = manifest.into();
        let r: AnnounceReleaseResult = client_a
            .request("aii_announceRelease", rpc_params![view.clone()])
            .await
            .unwrap();
        assert!(r.accepted, "A must accept the announcement");

        // The propagate task runs in the background — give it up to
        // 2 s to call B and import the binary.
        let mut got = false;
        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let b_latest = state_b.latest.lock().unwrap().clone();
            let b_bin = state_b.binaries.lock().unwrap().get("0.0.77").cloned();
            if b_latest.is_some() && b_bin.as_deref() == Some(payload.as_ref()) {
                got = true;
                break;
            }
        }
        assert!(
            got,
            "B should have received both the manifest and the binary from A"
        );

        handle_a.stop().unwrap();
        handle_b.stop().unwrap();
    }

    /// v0.0.78 — aii_installRelease happy path: with the binary
    /// pre-loaded in the fixture's cache, the RPC returns
    /// `scheduled = true` and a non-zero restart_in_secs.
    #[tokio::test]
    async fn aii_install_release_returns_scheduled_when_binary_present() {
        let fixture = release_fixture();
        fixture
            .binaries
            .lock()
            .unwrap()
            .insert("0.0.78".to_string(), b"binary".to_vec());
        let (addr, handle) = serve("127.0.0.1:0".parse().unwrap(), fixture)
            .await
            .unwrap();
        let url = format!("http://{addr}");
        let c = HttpClientBuilder::default().build(url).unwrap();
        let r: InstallReleaseResult = c
            .request("aii_installRelease", rpc_params!["0.0.78"])
            .await
            .unwrap();
        assert!(
            r.scheduled,
            "install must succeed when binary present: {r:?}"
        );
        assert!(r.restart_in_secs > 0);
        assert!(r.reason.is_empty());
        handle.stop().unwrap();
    }

    /// v0.0.78 — aii_installRelease without the binary in cache
    /// returns `scheduled = false` with an explanatory reason.
    #[tokio::test]
    async fn aii_install_release_rejects_missing_binary() {
        let (url, h) = spawn_release().await;
        let c = HttpClientBuilder::default().build(url).unwrap();
        let r: InstallReleaseResult = c
            .request("aii_installRelease", rpc_params!["0.0.99"])
            .await
            .unwrap();
        assert!(!r.scheduled);
        assert!(!r.reason.is_empty());
        assert_eq!(r.restart_in_secs, 0);
        h.stop().unwrap();
    }

    /// v0.0.80 — aii_rollbackRelease happy path: with a binary
    /// pre-loaded in the fixture (proxy for a previous install
    /// having snapshotted), the RPC returns scheduled = true.
    #[tokio::test]
    async fn aii_rollback_release_returns_scheduled_when_snapshot_present() {
        let fixture = release_fixture();
        fixture
            .binaries
            .lock()
            .unwrap()
            .insert("0.0.80".to_string(), b"prior".to_vec());
        let (addr, handle) = serve("127.0.0.1:0".parse().unwrap(), fixture)
            .await
            .unwrap();
        let url = format!("http://{addr}");
        let c = HttpClientBuilder::default().build(url).unwrap();
        let r: InstallReleaseResult = c
            .request("aii_rollbackRelease", rpc_params![])
            .await
            .unwrap();
        assert!(r.scheduled, "rollback must succeed with snapshot: {r:?}");
        assert!(r.restart_in_secs > 0);
        assert!(r.reason.is_empty());
        handle.stop().unwrap();
    }

    /// v0.0.80 — aii_rollbackRelease rejects with reason when no
    /// snapshot has been recorded.
    #[tokio::test]
    async fn aii_rollback_release_rejects_when_no_snapshot() {
        let (url, h) = spawn_release().await;
        let c = HttpClientBuilder::default().build(url).unwrap();
        let r: InstallReleaseResult = c
            .request("aii_rollbackRelease", rpc_params![])
            .await
            .unwrap();
        assert!(!r.scheduled);
        assert!(r.reason.contains("snapshot") || r.reason.contains("rollback"));
        h.stop().unwrap();
    }

    /// v0.0.81 late-joiner re-poll — two-node integration:
    /// Node B is ahead (already has a signed manifest + the
    /// binary). Node A has nothing. After a single `poll_once`
    /// against B's URL, A ends up with the manifest accepted
    /// AND the binary imported, all via re-verification of the
    /// pinned Ed25519 signature on A's side.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn release_poller_pulls_manifest_and_binary_from_peer() {
        use aii_crypto::ed25519::SecretKey;
        use aii_crypto::release::sign_release;
        use std::io::Write;

        const PINNED_SECRET_HEX: &str =
            "be06b95cb0e2d44ee175cc7a475ea4e9fcab47a784d161c36978b34e28ceeb97";
        let sk = SecretKey::from_hex(PINNED_SECRET_HEX).unwrap();

        // Boot peer B with a signed manifest + binary already in place.
        let payload = b"v0.0.81 late-joiner pulled body";
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(payload).unwrap();
        let manifest = sign_release(&sk, tmp.path(), "0.0.81", 1_900_000_081).unwrap();
        let state_b = release_fixture();
        *state_b.latest.lock().unwrap() = Some(manifest.clone());
        state_b
            .binaries
            .lock()
            .unwrap()
            .insert("0.0.81".to_string(), payload.to_vec());
        let (addr_b, handle_b) = serve("127.0.0.1:0".parse().unwrap(), state_b.clone())
            .await
            .unwrap();
        let url_b = format!("http://{addr_b}");

        // Node A starts empty.
        let state_a = release_fixture();
        assert!(state_a.latest.lock().unwrap().is_none());

        // Single-tick poll against B.
        let out =
            crate::release_poller::poll_once(state_a.clone(), std::slice::from_ref(&url_b)).await;
        assert_eq!(out.peers.len(), 1);
        let p = &out.peers[0];
        assert_eq!(p.peer, url_b);
        assert!(
            p.accepted_manifest,
            "A should accept B's manifest after sig verify: {p:?}"
        );
        assert!(p.imported_binary, "A should pull the binary: {p:?}");

        // A's view now matches B.
        let a_latest = state_a.latest.lock().unwrap().clone();
        assert_eq!(a_latest.as_ref(), Some(&manifest));
        let a_bin = state_a.binaries.lock().unwrap().get("0.0.81").cloned();
        assert_eq!(a_bin.as_deref(), Some(payload.as_ref()));

        handle_b.stop().unwrap();
    }

    /// v0.0.81 — second poll over the same peer should be a no-op
    /// (manifest already known, binary already present).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn release_poller_is_idempotent_after_first_catch_up() {
        use aii_crypto::ed25519::SecretKey;
        use aii_crypto::release::sign_release;
        use std::io::Write;

        const PINNED_SECRET_HEX: &str =
            "be06b95cb0e2d44ee175cc7a475ea4e9fcab47a784d161c36978b34e28ceeb97";
        let sk = SecretKey::from_hex(PINNED_SECRET_HEX).unwrap();
        let payload = b"v0.0.81 idempotent body";
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(payload).unwrap();
        let manifest = sign_release(&sk, tmp.path(), "0.0.81", 1_900_000_082).unwrap();

        let state_b = release_fixture();
        *state_b.latest.lock().unwrap() = Some(manifest.clone());
        state_b
            .binaries
            .lock()
            .unwrap()
            .insert("0.0.81".to_string(), payload.to_vec());
        let (addr_b, handle_b) = serve("127.0.0.1:0".parse().unwrap(), state_b)
            .await
            .unwrap();
        let url_b = format!("http://{addr_b}");

        let state_a = release_fixture();
        let peers = std::slice::from_ref(&url_b);
        let _ = crate::release_poller::poll_once(state_a.clone(), peers).await;
        let out = crate::release_poller::poll_once(state_a.clone(), peers).await;
        let p = &out.peers[0];
        assert!(!p.accepted_manifest, "second poll must not re-accept");
        assert!(!p.imported_binary, "second poll must not re-import");
        assert_eq!(p.note, "not newer than local");

        handle_b.stop().unwrap();
    }
}
