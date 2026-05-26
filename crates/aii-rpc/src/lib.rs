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
