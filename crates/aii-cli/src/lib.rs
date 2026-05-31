//! # aii-cli (library surface)
//!
//! Pure-function command runners that the `aii` binary wires together.
//! Extracting them as a library lets us unit-test each subcommand against
//! a live RPC server without spawning a subprocess.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// Convenience re-export of the release-signing primitives. The
/// authoritative module lives in `aii-crypto::release` so the RPC
/// layer + node binary can share the same types.
pub use aii_crypto::release;

use aii_onboarding::{detect, recommend_tier, score, Tier};
use aii_wallet::{EncryptedKeystore, LocalWallet, MnemonicPhrase, ScryptParams};
use alloy_rlp::Encodable;
use jsonrpsee::core::client::ClientT;
use jsonrpsee::http_client::{HttpClient, HttpClientBuilder};
use jsonrpsee::rpc_params;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors produced by the CLI's command runners.
#[derive(Debug, Error)]
pub enum CliError {
    /// JSON-RPC transport / call failure.
    #[error("rpc: {0}")]
    Rpc(#[from] jsonrpsee::core::ClientError),

    /// Wallet error.
    #[error("wallet: {0}")]
    Wallet(#[from] aii_wallet::WalletError),

    /// JSON formatting failure.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    /// Generic transport (URL parse, etc.).
    #[error("client: {0}")]
    Client(String),
}

/// Build an HTTP client from the user-supplied RPC URL.
fn client(url: &str) -> Result<HttpClient, CliError> {
    HttpClientBuilder::default()
        .build(url)
        .map_err(|e| CliError::Client(e.to_string()))
}

/// Output of the `status` subcommand.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StatusReport {
    /// EIP-155 chain id (decimal).
    pub chain_id: u64,
    /// Network name (e.g. "aii-mainnet").
    pub network: String,
    /// Head block number.
    pub head_block_number: u64,
}

/// Run `aii status --rpc URL`.
pub async fn run_status(rpc: &str) -> Result<StatusReport, CliError> {
    let c = client(rpc)?;
    let r: aii_rpc::AiiStatus = c.request("aii_status", rpc_params![]).await?;
    Ok(StatusReport {
        chain_id: r.chain_id,
        network: r.network,
        head_block_number: r.head_block_number,
    })
}

/// Run `aii chain-id --rpc URL`. Returns chain id as `u64`.
pub async fn run_chain_id(rpc: &str) -> Result<u64, CliError> {
    let c = client(rpc)?;
    let hex: String = c.request("eth_chainId", rpc_params![]).await?;
    parse_hex_u64(&hex).ok_or_else(|| CliError::Client(format!("bad eth_chainId hex: {hex}")))
}

/// Run `aii block --rpc URL <number|hash>`. Returns the block header
/// as a `HeaderView`, or `None` if unknown.
pub async fn run_get_block_header(
    rpc: &str,
    query: &str,
) -> Result<Option<aii_rpc::HeaderView>, CliError> {
    let c = client(rpc)?;
    let r: Option<aii_rpc::HeaderView> =
        c.request("aii_getBlockHeader", rpc_params![query]).await?;
    Ok(r)
}

/// Run `aii recent --rpc URL --limit N`. Returns the N most-recent
/// block headers, newest first. `limit` is server-capped at 100.
pub async fn run_recent_blocks(
    rpc: &str,
    limit: u64,
) -> Result<Vec<aii_rpc::HeaderView>, CliError> {
    let c = client(rpc)?;
    let r: Vec<aii_rpc::HeaderView> = c.request("aii_recentBlocks", rpc_params![limit]).await?;
    Ok(r)
}

/// Single-tx submission via `eth_sendRawTransaction`. Returns the
/// transaction hash returned by the node.
pub async fn run_send_raw_tx(rpc: &str, raw_hex: &str) -> Result<String, CliError> {
    let c = client(rpc)?;
    let h: String = c
        .request("eth_sendRawTransaction", rpc_params![raw_hex])
        .await?;
    Ok(h)
}

/// Run `aii release rollback --rpc URL` (v0.0.80).
///
/// Hits the target node's `aii_rollbackRelease` to restore the
/// pre-install snapshot at `<data-dir>/releases/.previous` over
/// the running binary and `execve` self into it. Returns the
/// node's `InstallReleaseResult` envelope verbatim.
///
/// # Errors
///
/// Transport failure or a non-success JSON-RPC reply.
pub async fn run_rollback_release(rpc: &str) -> Result<aii_rpc::InstallReleaseResult, CliError> {
    let c = client(rpc)?;
    let r: aii_rpc::InstallReleaseResult = c.request("aii_rollbackRelease", rpc_params![]).await?;
    Ok(r)
}

// ──────────────────────── Sub-chain runner (v0.0.38) ────────────────────────

/// One sub-chain → parent-chain flush record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlushRecord {
    /// Sub-chain height at the time of the flush.
    pub sub_block_number: u64,
    /// Sub-chain block hash being anchored (`0x…` hex).
    pub sub_block_hash: String,
    /// Parent-chain tx hash returned by `eth_sendRawTransaction`
    /// (`0x…` hex). If the parent rejected, this is the error string
    /// prefixed by `err:`.
    pub parent_tx: String,
}

/// Final report from a sub-chain run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubchainRunReport {
    /// Sub-chain id we ran.
    pub sub_chain_id: u64,
    /// Parent-chain id we anchored into.
    pub parent_chain_id: u64,
    /// Total sub-chain blocks produced.
    pub sub_blocks_produced: u64,
    /// Sub-chain head hash at exit.
    pub sub_head_hash: String,
    /// All flushes that were attempted (newest last).
    pub flushes: Vec<FlushRecord>,
}

/// Run an in-process PoA sub-chain and flush anchors to the parent
/// chain on schedule.
///
/// The sub-chain uses a fresh secp256k1 authority key (the
/// "operator") for both block signing (via PoA `authorities[0]`)
/// and for signing the EIP-155 flush tx to the parent. The flush tx
/// is a self-transfer (value=0, gas=21000) whose calldata is
/// `sub_block_hash ‖ sub_block_number_be8` so the parent block
/// explorer can witness which sub-chain block was anchored.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub async fn run_subchain(
    sub_chain_id: u64,
    parent_chain_id: u64,
    parent_rpc: &str,
    slot_seconds: u64,
    flush_interval_blocks: u64,
    duration_blocks: u64,
) -> Result<SubchainRunReport, CliError> {
    use aii_block::tx::{Tx, TxLegacy};
    use aii_block::{
        Block, BlockBody, Bloom, Hashable as _, Header, EMPTY_LIST_HASH, EMPTY_TRIE_HASH,
    };
    use aii_consensus_poa::{PoaConfig, PoaEngine};
    use aii_crypto::keccak::keccak256;
    use aii_crypto::secp::{sign, SecretKey};
    use aii_types::{AlgoId, H256, U256};

    if sub_chain_id == parent_chain_id {
        return Err(CliError::Client(
            "sub_chain_id must differ from parent_chain_id".into(),
        ));
    }
    // Operator key — fresh, in-memory only. Generate before the
    // async loop so the non-Send `ThreadRng` is dropped before any
    // `.await`.
    let sk = {
        let mut rng = rand::thread_rng();
        let mut bytes = [0u8; 32];
        loop {
            rng.fill_bytes(&mut bytes);
            if let Ok(s) = SecretKey::from_bytes(&bytes) {
                break s;
            }
        }
    };
    let coinbase = sk.public_key().address();

    let genesis = Block {
        header: Header {
            parent_hash: H256::ZERO,
            ommers_hash: EMPTY_LIST_HASH,
            beneficiary: coinbase,
            state_root: EMPTY_TRIE_HASH,
            transactions_root: EMPTY_TRIE_HASH,
            receipts_root: EMPTY_TRIE_HASH,
            logs_bloom: Bloom::ZERO,
            difficulty: U256::ZERO,
            number: 0,
            gas_limit: 30_000_000,
            gas_used: 0,
            timestamp: 1_700_000_000,
            extra_data: format!("aii-sub-{sub_chain_id}").into_bytes(),
            mix_hash: H256::ZERO,
            nonce: [0u8; 8],
            base_fee_per_gas: U256::from(1_000_000_000u64),
            withdrawals_root: EMPTY_TRIE_HASH,
            blob_gas_used: None,
            excess_blob_gas: None,
            parent_beacon_block_root: None,
        },
        body: BlockBody::default(),
    };
    let cfg = PoaConfig {
        authorities: vec![coinbase],
        coinbase,
        slot_seconds,
        gas_limit: 30_000_000,
        base_fee_per_gas: U256::from(1_000_000_000u64),
        signer_sk: None,
    };
    let engine =
        PoaEngine::new(cfg, &genesis).map_err(|e| CliError::Client(format!("poa engine: {e}")))?;

    let parent_client = client(parent_rpc)?;

    let mut flushes: Vec<FlushRecord> = Vec::new();
    let mut sub_head_hash = genesis.hash();
    let mut parent_nonce: u64 = 0;

    for _ in 1..=duration_blocks {
        tokio::time::sleep(std::time::Duration::from_secs(slot_seconds)).await;
        let (h, n, _block) = engine
            .produce_block()
            .map_err(|e| CliError::Client(format!("produce: {e}")))?;
        sub_head_hash = h;

        if n % flush_interval_blocks == 0 {
            // Build flush tx: self-transfer with calldata =
            //   b"AII_FLUSH" (9)
            // ‖ sub_chain_id_be4    (4)
            // ‖ sub_block_hash      (32)
            // ‖ sub_block_number_be8(8)
            // = 53 bytes. The 9-byte ASCII magic lets the parent's
            // anchor-decoder identify a sub-chain flush among
            // ordinary self-transfers, and the embedded id removes
            // any ambiguity when multiple sub-chains anchor into
            // the same parent.
            let mut data = Vec::with_capacity(9 + 4 + 32 + 8);
            data.extend_from_slice(aii_microchain::FLUSH_TX_MAGIC);
            #[allow(clippy::cast_possible_truncation)]
            let sub_id_be4 = (sub_chain_id as u32).to_be_bytes();
            data.extend_from_slice(&sub_id_be4);
            data.extend_from_slice(h.as_bytes());
            data.extend_from_slice(&n.to_be_bytes());
            let mut tx = TxLegacy {
                nonce: parent_nonce,
                gas_price: U256::from(1_000_000_000u64),
                gas_limit: 100_000,
                to: Some(coinbase),
                value: U256::ZERO,
                data,
                v: 0,
                r: H256::ZERO,
                s: H256::ZERO,
                algo_id: AlgoId::Secp256k1,
            };
            let hash = compute_legacy_eip155_hash(&tx, parent_chain_id);
            let sig = sign(&sk, &hash).map_err(|e| CliError::Client(format!("sign: {e}")))?;
            let raw = sig.to_bytes();
            tx.r = H256::new(raw[..32].try_into().unwrap());
            tx.s = H256::new(raw[32..64].try_into().unwrap());
            tx.v = parent_chain_id * 2 + 35 + u64::from(raw[64]);

            let mut out = alloy_rlp::bytes::BytesMut::new();
            Tx::Legacy(tx).encode(&mut out);
            let raw_hex = format!("0x{}", hex::encode(out));
            let parent_tx_result = parent_client
                .request::<String, _>("eth_sendRawTransaction", rpc_params![raw_hex])
                .await;
            let parent_tx = match parent_tx_result {
                Ok(s) => {
                    parent_nonce += 1;
                    s
                }
                Err(e) => format!("err: {e}"),
            };
            // Compute keccak256 of the encoded tx purely for the
            // sub-block-hash field of the FlushRecord.
            let _ = keccak256;
            flushes.push(FlushRecord {
                sub_block_number: n,
                sub_block_hash: format!("0x{}", hex::encode(h.as_bytes())),
                parent_tx,
            });
        }
    }
    Ok(SubchainRunReport {
        sub_chain_id,
        parent_chain_id,
        sub_blocks_produced: duration_blocks,
        sub_head_hash: format!("0x{}", hex::encode(sub_head_hash.as_bytes())),
        flushes,
    })
}

/// Persistent sub-chain operator state.
///
/// Kept on disk in `<data_dir>/state.json` so a restart resumes with
/// the same operator address, the same height counter, and the same
/// parent-chain nonce sequence. Without this, every restart of
/// `aii subchain run` got a fresh operator key, a fresh head, and
/// re-collided with the parent on the very next flush nonce.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubchainPersistentState {
    /// Sub-chain id (kept here so the JSON file is self-describing).
    pub sub_chain_id: u64,
    /// Consensus engine the sub-chain runs. `"poa"` (single
    /// authority) is fully implemented; `"bft"` requires a multi-
    /// operator validator set and is currently rejected at startup
    /// pending the engine wire-up.
    #[serde(default = "default_consensus_label")]
    pub consensus: String,
    /// Operator secp256k1 secret key, hex (32 bytes).
    pub operator_sk_hex: String,
    /// Last sub-chain block number produced before shutdown.
    pub head_number: u64,
    /// Last sub-chain block hash produced (`0x…` hex). Empty string
    /// on a fresh state file.
    pub head_hash: String,
    /// Next parent-chain nonce to submit a flush tx under.
    pub parent_nonce: u64,
    /// Cumulative flushes performed so far (informational).
    pub flush_count: u64,
}

fn default_consensus_label() -> String {
    "poa".to_string()
}

/// Currently-implemented sub-chain consensus engines.
///
/// `Poa` is the only fully-implemented variant; `Bft` is parsed +
/// persisted but rejected at startup until the engine wire-up lands
/// in a follow-up release. Keeping the enum here means today's
/// `state.json` already records the operator's chosen consensus so a
/// future binary can pick it up without a fresh keygen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubchainConsensus {
    /// Proof-of-Authority with a single operator. Round-robin authority
    /// list `[operator]` — every block produced by the operator.
    Poa,
    /// VRF-PoS BFT. The operator becomes validator 0 of a single-
    /// validator set; future releases extend to multi-operator.
    Bft,
}

impl SubchainConsensus {
    /// Parse the on-disk label.
    ///
    /// # Errors
    /// Returns `Err` for any unrecognised label.
    pub fn parse(s: &str) -> Result<Self, CliError> {
        match s {
            "poa" => Ok(Self::Poa),
            "bft" => Ok(Self::Bft),
            other => Err(CliError::Client(format!(
                "unknown sub-chain consensus '{other}' (expected: poa, bft)"
            ))),
        }
    }

    /// Canonical lowercase label used in the on-disk JSON.
    #[must_use]
    pub const fn as_label(self) -> &'static str {
        match self {
            Self::Poa => "poa",
            Self::Bft => "bft",
        }
    }
}

impl SubchainPersistentState {
    /// Build the canonical filename inside `data_dir`.
    fn path(data_dir: &std::path::Path) -> std::path::PathBuf {
        data_dir.join("state.json")
    }

    /// Bootstrap state for a brand-new sub-chain — generates a fresh
    /// operator key and writes the initial state file.
    ///
    /// `consensus` records the operator's chosen engine; today only
    /// `Poa` is fully implemented but the label persists so a future
    /// binary can honour the original choice without forcing a fresh
    /// keygen.
    ///
    /// # Errors
    /// Returns I/O / JSON errors.
    pub fn create_fresh(
        sub_chain_id: u64,
        consensus: SubchainConsensus,
        data_dir: &std::path::Path,
    ) -> Result<Self, CliError> {
        use aii_crypto::secp::SecretKey;
        use rand::RngCore;
        std::fs::create_dir_all(data_dir)
            .map_err(|e| CliError::Client(format!("subchain data_dir: {e}")))?;
        let sk = {
            let mut rng = rand::thread_rng();
            let mut bytes = [0u8; 32];
            loop {
                rng.fill_bytes(&mut bytes);
                if let Ok(s) = SecretKey::from_bytes(&bytes) {
                    break s;
                }
            }
        };
        let st = Self {
            sub_chain_id,
            consensus: consensus.as_label().to_string(),
            operator_sk_hex: format!("0x{}", hex::encode(sk.to_bytes())),
            head_number: 0,
            head_hash: String::new(),
            parent_nonce: 0,
            flush_count: 0,
        };
        st.save(data_dir)?;
        Ok(st)
    }

    /// Decode the on-disk `consensus` label into a typed enum.
    ///
    /// # Errors
    /// Returns `Err` if the label is unknown.
    pub fn consensus_kind(&self) -> Result<SubchainConsensus, CliError> {
        SubchainConsensus::parse(&self.consensus)
    }

    /// Load the persistent state from `data_dir/state.json`, or
    /// return `Ok(None)` if no such file exists.
    ///
    /// # Errors
    /// Returns I/O / JSON errors.
    pub fn load(data_dir: &std::path::Path) -> Result<Option<Self>, CliError> {
        let path = Self::path(data_dir);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&path)
            .map_err(|e| CliError::Client(format!("read {}: {e}", path.display())))?;
        let st = serde_json::from_slice::<Self>(&bytes)
            .map_err(|e| CliError::Client(format!("parse {}: {e}", path.display())))?;
        Ok(Some(st))
    }

    /// Persist atomically: write to `state.json.tmp` then rename.
    /// Crash-safe — the rename is atomic on POSIX, so a torn write
    /// cannot leave a corrupted `state.json`.
    ///
    /// # Errors
    /// Returns I/O / JSON errors.
    pub fn save(&self, data_dir: &std::path::Path) -> Result<(), CliError> {
        std::fs::create_dir_all(data_dir)
            .map_err(|e| CliError::Client(format!("subchain data_dir: {e}")))?;
        let path = Self::path(data_dir);
        let tmp = data_dir.join("state.json.tmp");
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| CliError::Client(format!("serialize state.json: {e}")))?;
        std::fs::write(&tmp, &bytes)
            .map_err(|e| CliError::Client(format!("write {}: {e}", tmp.display())))?;
        std::fs::rename(&tmp, &path)
            .map_err(|e| CliError::Client(format!("rename → {}: {e}", path.display())))?;
        Ok(())
    }

    /// Recover the secp256k1 SecretKey from the hex-encoded form.
    ///
    /// # Errors
    /// Returns a `CliError::Client` if the hex is malformed.
    pub fn operator_secret_key(&self) -> Result<aii_crypto::secp::SecretKey, CliError> {
        let s = self
            .operator_sk_hex
            .strip_prefix("0x")
            .unwrap_or(&self.operator_sk_hex);
        let raw = hex::decode(s).map_err(|e| CliError::Client(format!("operator_sk hex: {e}")))?;
        if raw.len() != 32 {
            return Err(CliError::Client(format!(
                "operator_sk: expected 32 bytes, got {}",
                raw.len()
            )));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&raw);
        aii_crypto::secp::SecretKey::from_bytes(&arr)
            .map_err(|e| CliError::Client(format!("operator_sk parse: {e}")))
    }
}

fn compute_legacy_eip155_hash(t: &aii_block::tx::TxLegacy, chain_id: u64) -> aii_types::H256 {
    use aii_crypto::keccak::keccak256;
    use aii_types::U256;
    let mut buf = alloy_rlp::bytes::BytesMut::new();
    let zero = U256::ZERO;
    let payload_length = t.nonce.length()
        + u256_len(&t.gas_price)
        + t.gas_limit.length()
        + 21
        + u256_len(&t.value)
        + t.data.as_slice().length()
        + chain_id.length()
        + u256_len(&zero) * 2;
    alloy_rlp::Header {
        list: true,
        payload_length,
    }
    .encode(&mut buf);
    t.nonce.encode(&mut buf);
    encode_u256_local(&t.gas_price, &mut buf);
    t.gas_limit.encode(&mut buf);
    t.to.as_ref().expect("self-transfer").encode(&mut buf);
    encode_u256_local(&t.value, &mut buf);
    t.data.as_slice().encode(&mut buf);
    chain_id.encode(&mut buf);
    encode_u256_local(&zero, &mut buf);
    encode_u256_local(&zero, &mut buf);
    aii_types::H256::new(keccak256(&buf).0)
}

// ──────────────────────── Local transfer load (test only) ──────────────────

/// Per-account balance report from [`run_local_transfer_load`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalTransferAccountReport {
    /// Stable test-account index.
    pub index: usize,
    /// ETH-compatible account address (`0x...`).
    pub address: String,
    /// Starting balance in Wei, decimal encoded.
    pub initial_balance_wei: String,
    /// Ending balance in Wei, decimal encoded.
    pub final_balance_wei: String,
    /// Account nonce after the test run.
    pub nonce: u64,
}

/// Summary from `aii local-transfer-load`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalTransferLoadReport {
    /// Chain id used in EIP-155 signing.
    pub chain_id: u64,
    /// Test account reports.
    pub accounts: Vec<LocalTransferAccountReport>,
    /// Requested transaction count.
    pub total_requested: u64,
    /// Transactions successfully executed against local state.
    pub executed: u64,
    /// Transactions that failed signing, recovery, or execution.
    pub failed: u64,
    /// Synthetic block count, calculated from `txs_per_block`.
    pub simulated_blocks: u64,
    /// Synthetic transaction capacity per simulated block.
    pub txs_per_block: u64,
    /// Minimum transfer amount in AII as supplied by the caller.
    pub min_value_aii: String,
    /// Maximum transfer amount in AII as supplied by the caller.
    pub max_value_aii: String,
    /// Minimum transfer amount in Wei, decimal encoded.
    pub min_value_wei: String,
    /// Maximum transfer amount in Wei, decimal encoded.
    pub max_value_wei: String,
    /// Sum of all successfully executed transfer values in Wei.
    pub total_value_wei: String,
    /// Sum of gas used by successful transfers.
    pub total_gas_used: u64,
    /// Gas price used in the signed test transactions.
    pub gas_price_wei: String,
    /// Wall-clock execution time in milliseconds.
    pub elapsed_ms: u128,
}

/// Per-account report from [`run_live_transfer_load`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LiveTransferAccountReport {
    /// Stable account index from the supplied key-file order.
    pub index: usize,
    /// ETH-compatible account address (`0x...`).
    pub address: String,
    /// Nonce read before submission.
    pub initial_nonce: u64,
    /// Balance read before submission, in Wei.
    pub initial_balance_wei: String,
    /// Balance read after the optional settle wait, in Wei.
    pub final_balance_wei: String,
    /// How many transactions this account signed.
    pub signed_txs: u64,
    /// Maximum pre-funded outgoing value required for this account.
    pub planned_outgoing_value_wei: String,
    /// Planned gas budget for this account.
    pub planned_gas_budget_wei: String,
}

/// Summary from a real-chain transfer load.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LiveTransferLoadReport {
    /// JSON-RPC endpoint used.
    pub rpc: String,
    /// Chain id used in EIP-155 signing.
    pub chain_id: u64,
    /// Test account reports.
    pub accounts: Vec<LiveTransferAccountReport>,
    /// Requested transaction count.
    pub total_requested: u64,
    /// Transactions submitted to RPC.
    pub submitted: u64,
    /// Transactions accepted by RPC.
    pub accepted: u64,
    /// Transactions rejected by RPC.
    pub rejected: u64,
    /// Synthetic block count, calculated from `txs_per_block`.
    pub simulated_blocks: u64,
    /// Synthetic transaction capacity per simulated block.
    pub txs_per_block: u64,
    /// Minimum transfer amount in AII as supplied by the caller.
    pub min_value_aii: String,
    /// Maximum transfer amount in AII as supplied by the caller.
    pub max_value_aii: String,
    /// Minimum transfer amount in Wei.
    pub min_value_wei: String,
    /// Maximum transfer amount in Wei.
    pub max_value_wei: String,
    /// Sum of all submitted transfer values in Wei.
    pub total_value_wei: String,
    /// Gas price used in signed transactions.
    pub gas_price_wei: String,
    /// Seconds waited before reading final balances.
    pub settle_sec: u64,
    /// Accepted transaction hashes.
    pub tx_hashes: Vec<String>,
    /// Rejection summaries. Secrets are never included.
    pub errors: Vec<String>,
    /// Wall-clock execution time in milliseconds.
    pub elapsed_ms: u128,
}

/// One recipient funded by [`run_fund_addresses`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FundRecipientReport {
    /// Recipient index in command-line order.
    pub index: usize,
    /// Recipient address.
    pub address: String,
    /// Funded amount in Wei.
    pub amount_wei: String,
    /// Transaction hash if accepted by RPC.
    pub tx_hash: Option<String>,
    /// Error if rejected by RPC.
    pub error: Option<String>,
}

/// Summary from `aii fund-addresses`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FundAddressesReport {
    /// JSON-RPC endpoint used.
    pub rpc: String,
    /// Chain id used in EIP-155 signing.
    pub chain_id: u64,
    /// Funding account address.
    pub from_address: String,
    /// Funding account nonce before submission.
    pub initial_nonce: u64,
    /// Funding account balance before submission, in Wei.
    pub initial_balance_wei: String,
    /// Funding account balance after settle, in Wei.
    pub final_balance_wei: String,
    /// Amount sent to each recipient, in AII text form.
    pub amount_aii: String,
    /// Amount sent to each recipient, in Wei.
    pub amount_wei: String,
    /// Gas price used.
    pub gas_price_wei: String,
    /// Number of submitted funding txs.
    pub submitted: u64,
    /// Number of accepted funding txs.
    pub accepted: u64,
    /// Number of rejected funding txs.
    pub rejected: u64,
    /// Per-recipient result.
    pub recipients: Vec<FundRecipientReport>,
    /// Seconds waited before reading final balance.
    pub settle_sec: u64,
    /// Wall-clock execution time in milliseconds.
    pub elapsed_ms: u128,
}

/// One account credited by [`run_state_credit`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StateCreditAccountReport {
    /// Recipient index in command-line order.
    pub index: usize,
    /// Recipient address.
    pub address: String,
    /// Balance before credit, in Wei.
    pub before_balance_wei: String,
    /// Balance after credit, in Wei.
    pub after_balance_wei: String,
    /// Account nonce preserved by the credit operation.
    pub nonce: u64,
}

/// Summary from `aii state-credit`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StateCreditReport {
    /// RocksDB data directory opened.
    pub data_dir: String,
    /// Amount credited to each address, in AII text form.
    pub amount_aii: String,
    /// Amount credited to each address, in Wei.
    pub amount_wei: String,
    /// Per-account result.
    pub accounts: Vec<StateCreditAccountReport>,
}

const WEI_PER_AII: u128 = 1_000_000_000_000_000_000;

fn parse_aii_decimal_to_wei(input: &str) -> Result<u128, CliError> {
    let s = input.trim();
    if s.is_empty() || s.starts_with('-') || s.starts_with('+') {
        return Err(CliError::Client(format!("bad AII amount: {input}")));
    }
    let mut parts = s.split('.');
    let whole = parts
        .next()
        .ok_or_else(|| CliError::Client(format!("bad AII amount: {input}")))?;
    let frac = parts.next();
    if parts.next().is_some() || whole.is_empty() {
        return Err(CliError::Client(format!("bad AII amount: {input}")));
    }
    if !whole.bytes().all(|b| b.is_ascii_digit()) {
        return Err(CliError::Client(format!("bad AII amount: {input}")));
    }
    let whole_wei = whole
        .parse::<u128>()
        .map_err(|e| CliError::Client(format!("bad AII amount: {e}")))?
        .checked_mul(WEI_PER_AII)
        .ok_or_else(|| CliError::Client("AII amount overflows u128".into()))?;
    let frac_wei = if let Some(frac) = frac {
        if frac.len() > 18 || !frac.bytes().all(|b| b.is_ascii_digit()) {
            return Err(CliError::Client(format!("bad AII amount: {input}")));
        }
        let mut padded = frac.to_string();
        while padded.len() < 18 {
            padded.push('0');
        }
        padded
            .parse::<u128>()
            .map_err(|e| CliError::Client(format!("bad AII amount: {e}")))?
    } else {
        0
    };
    whole_wei
        .checked_add(frac_wei)
        .ok_or_else(|| CliError::Client("AII amount overflows u128".into()))
}

fn local_transfer_secret_key(index: usize) -> Result<aii_crypto::secp::SecretKey, CliError> {
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(b"AII-LOAD");
    bytes[24..].copy_from_slice(
        &u64::try_from(index + 1)
            .map_err(|_| CliError::Client("test account index overflow".into()))?
            .to_be_bytes(),
    );
    aii_crypto::secp::SecretKey::from_bytes(&bytes)
        .map_err(|e| CliError::Client(format!("test secret key: {e}")))
}

fn sign_legacy_transfer(
    sk: &aii_crypto::secp::SecretKey,
    chain_id: u64,
    nonce: u64,
    to: aii_types::Address,
    value: aii_types::U256,
    gas_price: aii_types::U256,
) -> Result<aii_block::tx::Tx, CliError> {
    use aii_block::tx::{Tx, TxLegacy};
    use aii_crypto::secp::sign;
    use aii_types::{AlgoId, H256};

    let mut tx = TxLegacy {
        nonce,
        gas_price,
        gas_limit: 21_000,
        to: Some(to),
        value,
        data: vec![],
        v: 0,
        r: H256::ZERO,
        s: H256::ZERO,
        algo_id: AlgoId::Secp256k1,
    };
    let hash = compute_legacy_eip155_hash(&tx, chain_id);
    let sig = sign(sk, &hash).map_err(|e| CliError::Client(e.to_string()))?;
    let raw = sig.to_bytes();
    tx.r = H256::new(raw[..32].try_into().unwrap());
    tx.s = H256::new(raw[32..64].try_into().unwrap());
    tx.v = chain_id
        .checked_mul(2)
        .and_then(|v| v.checked_add(35))
        .and_then(|v| v.checked_add(u64::from(raw[64])))
        .ok_or_else(|| CliError::Client("chain_id too large for EIP-155 v".into()))?;
    Ok(Tx::Legacy(tx))
}

fn read_secp_secret_file(path: &std::path::Path) -> Result<aii_crypto::secp::SecretKey, CliError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| CliError::Client(format!("{}: {e}", path.display())))?;
    let s = raw.trim().strip_prefix("0x").unwrap_or_else(|| raw.trim());
    let bytes = hex::decode(s)
        .map_err(|e| CliError::Client(format!("{}: bad hex: {e}", path.display())))?;
    if bytes.len() != 32 {
        return Err(CliError::Client(format!(
            "{}: expected 32-byte secp256k1 private key, got {} bytes",
            path.display(),
            bytes.len()
        )));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    aii_crypto::secp::SecretKey::from_bytes(&arr)
        .map_err(|e| CliError::Client(format!("{}: {e}", path.display())))
}

/// Derive the public address from a 32-byte secp256k1 private-key hex file.
///
/// This is useful for test funding flows where key material stays on disk and
/// only the address is displayed or passed to RPC commands.
pub fn run_account_from_key_file(path: &std::path::Path) -> Result<aii_types::Address, CliError> {
    Ok(read_secp_secret_file(path)?.public_key().address())
}

/// Credit testnet state directly in a stopped node's RocksDB.
///
/// This is an operator-maintenance escape hatch for testnets where no funded
/// EOA private key is available. Stop every validator first, apply the same
/// credit to every validator's state DB, then restart the network.
#[allow(clippy::missing_panics_doc)]
pub fn run_state_credit(
    data_dir: &std::path::Path,
    recipients: &[aii_types::Address],
    amount_aii: &str,
) -> Result<StateCreditReport, CliError> {
    use aii_state::{Account, StateDb};
    use aii_storage::RocksDbBackend;
    use std::sync::Arc;

    if recipients.is_empty() {
        return Err(CliError::Client(
            "at least one recipient is required".into(),
        ));
    }
    let amount_wei = parse_aii_decimal_to_wei(amount_aii)?;
    if amount_wei == 0 {
        return Err(CliError::Client(
            "amount_aii must be greater than zero".into(),
        ));
    }

    let backend = Arc::new(
        RocksDbBackend::open(data_dir)
            .map_err(|e| CliError::Client(format!("open state db: {e}")))?,
    );
    let state = StateDb::new(backend);
    let amount = aii_types::U256::from(amount_wei);
    let mut reports = Vec::with_capacity(recipients.len());
    for (index, address) in recipients.iter().enumerate() {
        let mut account = state
            .account(address)
            .map_err(|e| CliError::Client(format!("read account: {e}")))?
            .unwrap_or(Account::EMPTY);
        let before = account.balance;
        account.balance = account.balance.saturating_add(amount);
        state
            .set_account(address, &account)
            .map_err(|e| CliError::Client(format!("write account: {e}")))?;
        reports.push(StateCreditAccountReport {
            index,
            address: format!("0x{}", hex::encode(address.as_bytes())),
            before_balance_wei: before.to_string(),
            after_balance_wei: account.balance.to_string(),
            nonce: account.nonce,
        });
    }

    Ok(StateCreditReport {
        data_dir: data_dir.display().to_string(),
        amount_aii: amount_aii.to_string(),
        amount_wei: amount_wei.to_string(),
        accounts: reports,
    })
}

fn parse_hex_u256(s: &str) -> Result<aii_types::U256, CliError> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if s.is_empty() {
        return Ok(aii_types::U256::ZERO);
    }
    let padded_hex = if s.len() % 2 == 0 {
        s.to_string()
    } else {
        format!("0{s}")
    };
    let mut bytes =
        hex::decode(&padded_hex).map_err(|e| CliError::Client(format!("bad hex u256: {e}")))?;
    if bytes.len() > 32 {
        return Err(CliError::Client("hex u256 exceeds 32 bytes".into()));
    }
    let mut padded = [0u8; 32];
    let start = 32 - bytes.len();
    padded[start..].copy_from_slice(&bytes);
    bytes.clear();
    Ok(aii_types::U256::from_be_bytes(padded))
}

fn planned_amount_wei(min_wei: u128, max_wei: u128, total: u64, i: u64) -> u128 {
    if total <= 1 {
        min_wei
    } else {
        min_wei + (((max_wei - min_wei) * u128::from(i)) / u128::from(total - 1))
    }
}

/// Fund recipient addresses from one real on-chain funded private key.
///
/// The funding key file must contain a 32-byte secp256k1 private key as hex.
/// Recipients are plain addresses; callers that have recipient key files can
/// derive their addresses and pass them here. Private keys are never returned.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub async fn run_fund_addresses(
    rpc: &str,
    chain_id: u64,
    from_key_file: &std::path::Path,
    recipients: &[aii_types::Address],
    amount_aii: &str,
    settle_sec: u64,
) -> Result<FundAddressesReport, CliError> {
    use aii_block::Hashable as _;
    use alloy_rlp::Encodable as _;
    use std::time::Instant;

    if recipients.is_empty() {
        return Err(CliError::Client(
            "at least one recipient is required".into(),
        ));
    }
    let amount_wei = parse_aii_decimal_to_wei(amount_aii)?;
    if amount_wei == 0 {
        return Err(CliError::Client(
            "amount_aii must be greater than zero".into(),
        ));
    }

    let client = client(rpc)?;
    let sk = read_secp_secret_file(from_key_file)?;
    let from = sk.public_key().address();
    let from_hex = format!("0x{}", hex::encode(from.as_bytes()));
    let account: Option<aii_rpc::AccountView> = client
        .request("aii_getAccount", rpc_params![from_hex.clone()])
        .await?;
    let initial_nonce = account.as_ref().map_or(0, |a| a.nonce);
    let initial_balance = match account {
        Some(a) => parse_hex_u256(&a.balance)?,
        None => aii_types::U256::ZERO,
    };
    let gas_price_hex: String = client.request("eth_gasPrice", rpc_params![]).await?;
    let gas_price = parse_hex_u256(&gas_price_hex)?;
    let tx_count = u64::try_from(recipients.len())
        .map_err(|_| CliError::Client("too many recipients".into()))?;
    let total_value =
        aii_types::U256::from(amount_wei).saturating_mul(aii_types::U256::from(tx_count));
    let gas_budget = gas_price
        .saturating_mul(aii_types::U256::from(21_000u64))
        .saturating_mul(aii_types::U256::from(tx_count));
    let required = total_value.saturating_add(gas_budget);
    if initial_balance < required {
        return Err(CliError::Client(format!(
            "funding account has insufficient balance: have {initial_balance} wei, need at least {required} wei"
        )));
    }

    let start = Instant::now();
    let mut accepted = 0u64;
    let mut rejected = 0u64;
    let mut reports = Vec::with_capacity(recipients.len());
    for (index, recipient) in recipients.iter().enumerate() {
        let tx = sign_legacy_transfer(
            &sk,
            chain_id,
            initial_nonce + u64::try_from(index).expect("recipient index fits u64"),
            *recipient,
            aii_types::U256::from(amount_wei),
            gas_price,
        )?;
        let tx_hash = tx.hash();
        let mut out = alloy_rlp::bytes::BytesMut::new();
        tx.encode(&mut out);
        let raw_hex = format!("0x{}", hex::encode(out));
        match client
            .request::<String, _>("eth_sendRawTransaction", rpc_params![raw_hex])
            .await
        {
            Ok(hash) => {
                accepted = accepted.saturating_add(1);
                reports.push(FundRecipientReport {
                    index,
                    address: format!("0x{}", hex::encode(recipient.as_bytes())),
                    amount_wei: amount_wei.to_string(),
                    tx_hash: Some(hash),
                    error: None,
                });
            }
            Err(e) => {
                rejected = rejected.saturating_add(1);
                reports.push(FundRecipientReport {
                    index,
                    address: format!("0x{}", hex::encode(recipient.as_bytes())),
                    amount_wei: amount_wei.to_string(),
                    tx_hash: None,
                    error: Some(format!("hash=0x{}: {e}", hex::encode(tx_hash.as_bytes()))),
                });
            }
        }
    }

    if settle_sec > 0 {
        tokio::time::sleep(std::time::Duration::from_secs(settle_sec)).await;
    }
    let account: Option<aii_rpc::AccountView> = client
        .request("aii_getAccount", rpc_params![from_hex.clone()])
        .await?;
    let final_balance = match account {
        Some(a) => parse_hex_u256(&a.balance)?,
        None => aii_types::U256::ZERO,
    };

    Ok(FundAddressesReport {
        rpc: rpc.to_string(),
        chain_id,
        from_address: from_hex,
        initial_nonce,
        initial_balance_wei: initial_balance.to_string(),
        final_balance_wei: final_balance.to_string(),
        amount_aii: amount_aii.to_string(),
        amount_wei: amount_wei.to_string(),
        gas_price_wei: gas_price.to_string(),
        submitted: tx_count,
        accepted,
        rejected,
        recipients: reports,
        settle_sec,
        elapsed_ms: start.elapsed().as_millis(),
    })
}

/// Run a real-chain transfer load using funded key files.
///
/// The supplied key files must contain 32-byte secp256k1 private keys as hex
/// text. The function derives the four addresses, reads live nonce/balance
/// from RPC, signs real EIP-155 legacy transfers, submits them through
/// `eth_sendRawTransaction`, waits `settle_sec`, then reads balances again.
/// Private keys are never returned in the report.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub async fn run_live_transfer_load(
    rpc: &str,
    chain_id: u64,
    key_files: &[std::path::PathBuf],
    total: u64,
    min_aii: &str,
    max_aii: &str,
    txs_per_block: u64,
    settle_sec: u64,
) -> Result<LiveTransferLoadReport, CliError> {
    use aii_block::tx::Tx;
    use aii_block::Hashable as _;
    use alloy_rlp::Encodable as _;
    use std::time::Instant;

    if total == 0 {
        return Err(CliError::Client("total must be greater than zero".into()));
    }
    if key_files.len() < 2 {
        return Err(CliError::Client(
            "at least two key files are required".into(),
        ));
    }
    if txs_per_block == 0 {
        return Err(CliError::Client(
            "txs_per_block must be greater than zero".into(),
        ));
    }
    let min_wei = parse_aii_decimal_to_wei(min_aii)?;
    let max_wei = parse_aii_decimal_to_wei(max_aii)?;
    if min_wei > max_wei {
        return Err(CliError::Client("min_aii must be <= max_aii".into()));
    }

    let client = client(rpc)?;
    let mut keys = Vec::with_capacity(key_files.len());
    let mut addresses = Vec::with_capacity(key_files.len());
    for path in key_files {
        let sk = read_secp_secret_file(path)?;
        let address = sk.public_key().address();
        keys.push(sk);
        addresses.push(address);
    }

    let gas_price_hex: String = client.request("eth_gasPrice", rpc_params![]).await?;
    let gas_price = parse_hex_u256(&gas_price_hex)?;
    let gas_limit = aii_types::U256::from(21_000u64);
    let per_tx_fee = gas_price.saturating_mul(gas_limit);

    let mut initial_nonces = Vec::with_capacity(addresses.len());
    let mut initial_balances = Vec::with_capacity(addresses.len());
    for address in &addresses {
        let account: Option<aii_rpc::AccountView> = client
            .request(
                "aii_getAccount",
                rpc_params![format!("0x{}", hex::encode(address.as_bytes()))],
            )
            .await?;
        initial_nonces.push(account.as_ref().map_or(0, |a| a.nonce));
        initial_balances.push(match account {
            Some(a) => parse_hex_u256(&a.balance)?,
            None => aii_types::U256::ZERO,
        });
    }

    let accounts = addresses.len();
    let mut planned_counts = vec![0u64; accounts];
    let mut planned_outgoing = vec![aii_types::U256::ZERO; accounts];
    let mut total_value_wei = aii_types::U256::ZERO;
    for i in 0..total {
        let sender_index = (i as usize) % accounts;
        let amount = aii_types::U256::from(planned_amount_wei(min_wei, max_wei, total, i));
        planned_counts[sender_index] = planned_counts[sender_index].saturating_add(1);
        planned_outgoing[sender_index] = planned_outgoing[sender_index].saturating_add(amount);
        total_value_wei = total_value_wei.saturating_add(amount);
    }
    for (index, balance) in initial_balances.iter().enumerate() {
        let required = planned_outgoing[index].saturating_add(
            per_tx_fee.saturating_mul(aii_types::U256::from(planned_counts[index])),
        );
        if *balance < required {
            return Err(CliError::Client(format!(
                "account #{index} has insufficient balance: have {balance} wei, need at least {required} wei"
            )));
        }
    }

    let start = Instant::now();
    let mut next_nonces = initial_nonces.clone();
    let mut submitted = 0u64;
    let mut accepted = 0u64;
    let mut rejected = 0u64;
    let mut tx_hashes = Vec::new();
    let mut errors = Vec::new();

    for i in 0..total {
        let sender_index = (i as usize) % accounts;
        let recipient_index = (sender_index + 1) % accounts;
        let amount = aii_types::U256::from(planned_amount_wei(min_wei, max_wei, total, i));
        let tx = sign_legacy_transfer(
            &keys[sender_index],
            chain_id,
            next_nonces[sender_index],
            addresses[recipient_index],
            amount,
            gas_price,
        )?;
        let recovered = tx
            .recover_signer(chain_id)
            .map_err(|e| CliError::Client(format!("recover signer: {e}")))?;
        if recovered != addresses[sender_index] {
            rejected = rejected.saturating_add(1);
            errors.push(format!("tx #{i}: signer recovery mismatch"));
            continue;
        }
        let tx_hash = match &tx {
            Tx::Legacy(_) | Tx::Eip1559(_) | Tx::Eip4844(_) => tx.hash(),
        };
        let mut out = alloy_rlp::bytes::BytesMut::new();
        tx.encode(&mut out);
        let raw_hex = format!("0x{}", hex::encode(out));
        submitted = submitted.saturating_add(1);
        match client
            .request::<String, _>("eth_sendRawTransaction", rpc_params![raw_hex])
            .await
        {
            Ok(hash) => {
                accepted = accepted.saturating_add(1);
                next_nonces[sender_index] = next_nonces[sender_index].saturating_add(1);
                tx_hashes.push(hash);
            }
            Err(e) => {
                rejected = rejected.saturating_add(1);
                errors.push(format!(
                    "tx #{i} hash=0x{}: {e}",
                    hex::encode(tx_hash.as_bytes())
                ));
            }
        }
    }

    if settle_sec > 0 {
        tokio::time::sleep(std::time::Duration::from_secs(settle_sec)).await;
    }

    let mut reports = Vec::with_capacity(accounts);
    for (index, address) in addresses.iter().enumerate() {
        let account: Option<aii_rpc::AccountView> = client
            .request(
                "aii_getAccount",
                rpc_params![format!("0x{}", hex::encode(address.as_bytes()))],
            )
            .await?;
        let final_balance = match account {
            Some(a) => parse_hex_u256(&a.balance)?,
            None => aii_types::U256::ZERO,
        };
        reports.push(LiveTransferAccountReport {
            index,
            address: format!("0x{}", hex::encode(address.as_bytes())),
            initial_nonce: initial_nonces[index],
            initial_balance_wei: initial_balances[index].to_string(),
            final_balance_wei: final_balance.to_string(),
            signed_txs: planned_counts[index],
            planned_outgoing_value_wei: planned_outgoing[index].to_string(),
            planned_gas_budget_wei: per_tx_fee
                .saturating_mul(aii_types::U256::from(planned_counts[index]))
                .to_string(),
        });
    }

    Ok(LiveTransferLoadReport {
        rpc: rpc.to_string(),
        chain_id,
        accounts: reports,
        total_requested: total,
        submitted,
        accepted,
        rejected,
        simulated_blocks: total.div_ceil(txs_per_block),
        txs_per_block,
        min_value_aii: min_aii.to_string(),
        max_value_aii: max_aii.to_string(),
        min_value_wei: min_wei.to_string(),
        max_value_wei: max_wei.to_string(),
        total_value_wei: total_value_wei.to_string(),
        gas_price_wei: gas_price.to_string(),
        settle_sec,
        tx_hashes,
        errors,
        elapsed_ms: start.elapsed().as_millis(),
    })
}

/// Run a deterministic, in-memory 4-address transfer load.
///
/// This is intended for local workstation and Android-device smoke testing.
/// It signs real EIP-155 legacy transactions, recovers each signer, then
/// executes value transfers against an in-memory state database. It never
/// submits transactions to a live RPC endpoint and never uses real funds.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn run_local_transfer_load(
    chain_id: u64,
    total: u64,
    accounts: usize,
    min_aii: &str,
    max_aii: &str,
    txs_per_block: u64,
) -> Result<LocalTransferLoadReport, CliError> {
    use aii_state::{Account, StateDb};
    use aii_storage::MemoryBackend;
    use aii_types::U256;
    use std::sync::Arc;
    use std::time::Instant;

    if total == 0 {
        return Err(CliError::Client("total must be greater than zero".into()));
    }
    if accounts < 2 {
        return Err(CliError::Client("accounts must be at least 2".into()));
    }
    if txs_per_block == 0 {
        return Err(CliError::Client(
            "txs_per_block must be greater than zero".into(),
        ));
    }

    let min_wei = parse_aii_decimal_to_wei(min_aii)?;
    let max_wei = parse_aii_decimal_to_wei(max_aii)?;
    if min_wei > max_wei {
        return Err(CliError::Client("min_aii must be <= max_aii".into()));
    }

    let state = Arc::new(StateDb::new(Arc::new(MemoryBackend::new())));
    let mut keys = Vec::with_capacity(accounts);
    let mut addresses = Vec::with_capacity(accounts);
    for index in 0..accounts {
        let sk = local_transfer_secret_key(index)?;
        let address = sk.public_key().address();
        keys.push(sk);
        addresses.push(address);
    }

    let initial_balance_wei = U256::from(
        WEI_PER_AII
            .checked_mul(100_000)
            .ok_or_else(|| CliError::Client("initial balance overflow".into()))?,
    );
    for address in &addresses {
        state
            .set_account(
                address,
                &Account {
                    balance: initial_balance_wei,
                    ..Account::EMPTY
                },
            )
            .map_err(|e| CliError::Client(format!("local state: {e}")))?;
    }

    let gas_price = U256::from(1_000_000_000u64);
    let mut nonces = vec![0u64; accounts];
    let mut executed = 0u64;
    let mut failed = 0u64;
    let mut total_value_wei = U256::ZERO;
    let mut total_gas_used = 0u64;
    let range = max_wei - min_wei;
    let denominator = total.saturating_sub(1);
    let start = Instant::now();

    for i in 0..total {
        let sender_index = (i as usize) % accounts;
        let recipient_index = (sender_index + 1) % accounts;
        let step = if denominator == 0 {
            0
        } else {
            (range * u128::from(i)) / u128::from(denominator)
        };
        let amount_wei = min_wei + step;
        let amount = U256::from(amount_wei);
        let tx = sign_legacy_transfer(
            &keys[sender_index],
            chain_id,
            nonces[sender_index],
            addresses[recipient_index],
            amount,
            gas_price,
        )?;
        let recovered = tx
            .recover_signer(chain_id)
            .map_err(|e| CliError::Client(format!("recover signer: {e}")))?;
        if recovered != addresses[sender_index] {
            failed += 1;
            continue;
        }
        match aii_evm::execute_transfer(&state, addresses[sender_index], &tx) {
            Ok(receipt) => {
                nonces[sender_index] = nonces[sender_index].wrapping_add(1);
                executed = executed.wrapping_add(1);
                total_value_wei = total_value_wei.wrapping_add(amount);
                total_gas_used = total_gas_used.wrapping_add(receipt.cumulative_gas_used);
            }
            Err(_) => {
                failed = failed.wrapping_add(1);
            }
        }
    }

    let mut account_reports = Vec::with_capacity(accounts);
    for (index, address) in addresses.iter().enumerate() {
        let account = state
            .account(address)
            .map_err(|e| CliError::Client(format!("local state: {e}")))?
            .unwrap_or(Account::EMPTY);
        account_reports.push(LocalTransferAccountReport {
            index,
            address: format!("0x{}", hex::encode(address.as_bytes())),
            initial_balance_wei: initial_balance_wei.to_string(),
            final_balance_wei: account.balance.to_string(),
            nonce: account.nonce,
        });
    }

    Ok(LocalTransferLoadReport {
        chain_id,
        accounts: account_reports,
        total_requested: total,
        executed,
        failed,
        simulated_blocks: total.div_ceil(txs_per_block),
        txs_per_block,
        min_value_aii: min_aii.to_string(),
        max_value_aii: max_aii.to_string(),
        min_value_wei: min_wei.to_string(),
        max_value_wei: max_wei.to_string(),
        total_value_wei: total_value_wei.to_string(),
        total_gas_used,
        gas_price_wei: gas_price.to_string(),
        elapsed_ms: start.elapsed().as_millis(),
    })
}

// ──────────────────────── Stress harness (v0.0.37) ──────────────────────────

/// Outcome of one stress run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StressReport {
    /// How many txs the harness submitted.
    pub submitted: u64,
    /// How many submissions were accepted by the node (200 OK).
    pub accepted: u64,
    /// How many block headers we sampled at the end.
    pub blocks_observed: u64,
    /// `Σ gas_used` across observed blocks, divided by 21 000 — the
    /// number of (placeholder) txs that actually landed in blocks.
    pub txs_in_blocks: u64,
    /// Peak `gas_used / 21 000` across observed blocks.
    pub peak_txs_per_block: u64,
    /// Mean txs/block over observed blocks (`txs_in_blocks /
    /// blocks_observed`, rounded).
    pub mean_txs_per_block: u64,
    /// Wall-clock submission throughput (txs / s).
    pub submit_tx_per_sec: f64,
    /// Total elapsed seconds.
    pub elapsed_sec: f64,
}

/// Run the stress harness against a live AII node.
///
/// Generates `total` signed transfers across `senders` independent
/// signers and submits them via `eth_sendRawTransaction`. Then
/// sleeps `settle_sec` seconds and samples `sample_blocks` recent
/// blocks to compute throughput.
///
/// `chain_id` must match the target chain's chain id (e.g. 9999 for
/// the AII testnet) so the EIP-155 v field validates on the node.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub async fn run_stress(
    rpc: &str,
    chain_id: u64,
    total: u64,
    senders: u32,
    parallel: u32,
    settle_sec: u64,
    sample_blocks: u64,
) -> Result<StressReport, CliError> {
    use aii_block::tx::{Tx, TxLegacy};
    use aii_crypto::keccak::keccak256;
    use aii_crypto::secp::{sign, SecretKey};
    use aii_types::{AlgoId, U256};
    use alloy_rlp::Encodable as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    let senders = senders.max(1);
    let parallel = parallel.max(1);

    // Generate deterministic-but-fresh signers.
    let mut sks: Vec<SecretKey> = Vec::with_capacity(senders as usize);
    for i in 1..=senders {
        let mut bytes = [0u8; 32];
        bytes[28..].copy_from_slice(&i.to_be_bytes());
        sks.push(SecretKey::from_bytes(&bytes).map_err(|e| CliError::Client(e.to_string()))?);
    }
    let sks = Arc::new(sks);

    // Build the channel of pre-signed raw hex strings.
    let (tx_send, mut tx_recv) =
        tokio::sync::mpsc::channel::<String>(usize::try_from(parallel * 4).unwrap_or(1024));
    let sks_for_signer = sks.clone();
    let signer_handle = tokio::task::spawn_blocking(move || -> Result<(), CliError> {
        for i in 0..total {
            let sender_idx = (i as usize) % sks_for_signer.len();
            let nonce = i / u64::from(senders);
            let sk = &sks_for_signer[sender_idx];
            // Self-transfer of 0 wei — minimum payload.
            let mut tx = TxLegacy {
                nonce,
                gas_price: U256::from(1_000_000_000u64),
                gas_limit: 21_000,
                to: Some(sk.public_key().address()),
                value: U256::from(0u64),
                data: vec![],
                v: 0,
                r: aii_types::H256::ZERO,
                s: aii_types::H256::ZERO,
                algo_id: AlgoId::Secp256k1,
            };
            // EIP-155 signing hash.
            let mut buf = alloy_rlp::bytes::BytesMut::new();
            let zero = U256::ZERO;
            let payload_length = tx.nonce.length()
                + u256_len(&tx.gas_price)
                + tx.gas_limit.length()
                + 21 // 20-byte address envelope
                + u256_len(&tx.value)
                + tx.data.as_slice().length()
                + chain_id.length()
                + u256_len(&zero) * 2;
            alloy_rlp::Header {
                list: true,
                payload_length,
            }
            .encode(&mut buf);
            tx.nonce.encode(&mut buf);
            encode_u256_local(&tx.gas_price, &mut buf);
            tx.gas_limit.encode(&mut buf);
            // to: Some(addr) — encoded as 20-byte string
            tx.to.as_ref().expect("self-transfer").encode(&mut buf);
            encode_u256_local(&tx.value, &mut buf);
            tx.data.as_slice().encode(&mut buf);
            chain_id.encode(&mut buf);
            encode_u256_local(&zero, &mut buf);
            encode_u256_local(&zero, &mut buf);
            let hash = aii_types::H256::new(keccak256(&buf).0);
            let sig = sign(sk, &hash).map_err(|e| CliError::Client(e.to_string()))?;
            let raw = sig.to_bytes();
            tx.r = aii_types::H256::new(raw[..32].try_into().unwrap());
            tx.s = aii_types::H256::new(raw[32..64].try_into().unwrap());
            tx.v = chain_id * 2 + 35 + u64::from(raw[64]);
            // Encode the full signed legacy tx.
            let mut out = alloy_rlp::bytes::BytesMut::new();
            Tx::Legacy(tx).encode(&mut out);
            let hex_str = format!("0x{}", hex::encode(out));
            if tx_send.blocking_send(hex_str).is_err() {
                break;
            }
        }
        Ok(())
    });

    let accepted = Arc::new(AtomicU64::new(0));
    let submitted = Arc::new(AtomicU64::new(0));
    let start = Instant::now();

    let rpc = rpc.to_string();
    // Single-receiver fan-out: collect everything into a Vec, then chunk.
    let mut all: Vec<String> = Vec::new();
    while let Some(s) = tx_recv.recv().await {
        all.push(s);
    }
    let signer_result = signer_handle
        .await
        .map_err(|e| CliError::Client(format!("signer join: {e}")))?;
    signer_result?;

    // Chunk all txs across `parallel` workers.
    let chunk = (all.len() + (parallel as usize) - 1) / (parallel as usize).max(1);
    let mut worker_handles = Vec::new();
    for slice in all.chunks(chunk) {
        let url = rpc.clone();
        let acc = accepted.clone();
        let sub = submitted.clone();
        let payload: Vec<String> = slice.to_vec();
        worker_handles.push(tokio::spawn(async move {
            let Ok(c) = client(&url) else {
                return;
            };
            for raw in payload {
                sub.fetch_add(1, Ordering::Relaxed);
                if c.request::<String, _>("eth_sendRawTransaction", rpc_params![&raw])
                    .await
                    .is_ok()
                {
                    acc.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }
    for h in worker_handles {
        let _ = h.await;
    }
    let elapsed = start.elapsed().as_secs_f64();
    tokio::time::sleep(std::time::Duration::from_secs(settle_sec)).await;

    // Sample blocks.
    let recent = run_recent_blocks(&rpc, sample_blocks).await?;
    let mut tx_total: u64 = 0;
    let mut peak: u64 = 0;
    for h in &recent {
        let gas_used = parse_hex_u64(&h.gas_used).unwrap_or(0);
        let n = gas_used / 21_000;
        tx_total += n;
        peak = peak.max(n);
    }
    let blocks = recent.len() as u64;
    let mean = if blocks > 0 { tx_total / blocks } else { 0 };
    let submitted_n = submitted.load(Ordering::Relaxed);
    let accepted_n = accepted.load(Ordering::Relaxed);
    let throughput = if elapsed > 0.0 {
        submitted_n as f64 / elapsed
    } else {
        0.0
    };
    Ok(StressReport {
        submitted: submitted_n,
        accepted: accepted_n,
        blocks_observed: blocks,
        txs_in_blocks: tx_total,
        peak_txs_per_block: peak,
        mean_txs_per_block: mean,
        submit_tx_per_sec: throughput,
        elapsed_sec: elapsed,
    })
}

// Local helpers to avoid pulling in private items from aii-block.
fn u256_len(v: &aii_types::U256) -> usize {
    // U256 RLP length: number of significant big-endian bytes, with
    // 0 encoded as the empty string (1 byte: 0x80) and any single
    // byte < 0x80 also encoded as one literal byte.
    let be = v.to_be_bytes::<32>();
    let leading = be.iter().take_while(|b| **b == 0).count();
    let n = 32 - leading;
    if n <= 1 && (n == 0 || be[31] < 0x80) {
        1
    } else {
        alloy_rlp::length_of_length(n) + n
    }
}

fn encode_u256_local(v: &aii_types::U256, out: &mut alloy_rlp::bytes::BytesMut) {
    let be = v.to_be_bytes::<32>();
    let leading = be.iter().take_while(|b| **b == 0).count();
    let bytes = &be[leading..];
    bytes.encode(out);
}

/// Run `aii account new`. Generates a fresh secp256k1 wallet from OS RNG
/// and returns its address (the private key is **dropped** before return
/// — v0.0.10 has no keystore yet; users must wait for v0.0.11).
pub fn run_account_new() -> Result<aii_types::Address, CliError> {
    // Generate a fresh secret. Loop on the rare case where the RNG hands us
    // an invalid scalar (probability ~ 2^-128).
    let mut rng = rand::thread_rng();
    for _ in 0..16 {
        let mut bytes = [0u8; 32];
        rng.fill_bytes(&mut bytes);
        if let Ok(w) = LocalWallet::from_secret_bytes(&bytes) {
            return Ok(w.address());
        }
    }
    Err(CliError::Client(
        "RNG produced 16 invalid scalars in a row".into(),
    ))
}

/// Result of `aii tier`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TierReport {
    /// 0–100 hardware score.
    pub score: u32,
    /// Recommended Tier.
    pub tier: Tier,
}

/// Generate a fresh keypair, encrypt it under `password`, and return the
/// keystore as JSON. The plaintext secret never leaves this function.
pub fn run_account_new_encrypted(password: &str) -> Result<String, CliError> {
    let mut rng = rand::thread_rng();
    for _ in 0..16 {
        let mut bytes = [0u8; 32];
        rng.fill_bytes(&mut bytes);
        if let Ok(w) = LocalWallet::from_secret_bytes(&bytes) {
            let ks = EncryptedKeystore::encrypt(&w, password, ScryptParams::light())
                .map_err(|e| CliError::Client(e.to_string()))?;
            return Ok(ks.to_json());
        }
    }
    Err(CliError::Client(
        "RNG produced 16 invalid scalars in a row".into(),
    ))
}

/// Verify that `password` decrypts the supplied keystore JSON and return
/// the embedded address. Used by `aii account verify`.
pub fn run_account_verify(
    keystore_json: &str,
    password: &str,
) -> Result<aii_types::Address, CliError> {
    let ks =
        EncryptedKeystore::from_json(keystore_json).map_err(|e| CliError::Client(e.to_string()))?;
    let w = ks
        .decrypt(password)
        .map_err(|e| CliError::Client(e.to_string()))?;
    Ok(w.address())
}

/// Result of `aii account mnemonic`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MnemonicReport {
    /// Space-separated BIP-39 phrase.
    pub phrase: String,
    /// Number of words (12 / 15 / 18 / 21 / 24).
    pub word_count: usize,
    /// First derived address (BIP-44 path m/44'/60'/0'/0/0, empty passphrase).
    pub address: String,
}

/// Generate a fresh BIP-39 mnemonic + derive its first ETH-compatible
/// address. The phrase is returned; the caller is responsible for
/// recording it somewhere safe.
pub fn run_account_mnemonic(word_count: usize) -> Result<MnemonicReport, CliError> {
    let m = MnemonicPhrase::generate(word_count).map_err(|e| CliError::Client(e.to_string()))?;
    let w = m
        .to_wallet("", 0)
        .map_err(|e| CliError::Client(e.to_string()))?;
    Ok(MnemonicReport {
        phrase: m.to_phrase(),
        word_count: m.word_count(),
        address: format!("0x{}", hex::encode(w.address().as_bytes())),
    })
}

/// Re-derive an address from a known mnemonic + index.
pub fn run_account_from_mnemonic(
    phrase: &str,
    passphrase: &str,
    index: u32,
) -> Result<aii_types::Address, CliError> {
    let m = MnemonicPhrase::from_phrase(phrase).map_err(|e| CliError::Client(e.to_string()))?;
    let w = m
        .to_wallet(passphrase, index)
        .map_err(|e| CliError::Client(e.to_string()))?;
    Ok(w.address())
}

/// Run `aii tier`.
#[must_use]
pub fn run_tier() -> TierReport {
    let profile = detect();
    let s = score(&profile);
    TierReport {
        score: s,
        tier: recommend_tier(s),
    }
}

// ──────────────────────── BFT capacity tooling ─────────────────────────────

/// Deterministic BFT capacity report for one successful height/round.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BftCapacityReport {
    /// Active validator count in the DPoS/BFT committee.
    pub validators: usize,
    /// Total online network nodes in the modeled deployment.
    pub network_nodes: u64,
    /// Nodes participating in the BFT consensus committee.
    pub consensus_nodes: usize,
    /// Online nodes that sync/serve the network but do not add BFT votes.
    pub passive_nodes: u64,
    /// Target seconds available for the round.
    pub target_secs: u64,
    /// Proposal bytes emitted by the leader before peer fan-out.
    pub proposal_bytes: usize,
    /// Equal-stake validators required to cross 2/3 + 1 quorum.
    pub equal_stake_quorum_votes: usize,
    /// Committee-wide vote messages in a full-mesh broadcast model.
    pub vote_messages_per_round: u64,
    /// Committee-wide vote payload bytes in a full-mesh broadcast model.
    pub vote_payload_bytes_per_round: u64,
    /// Leader upload bytes for sending the proposal to every other validator.
    pub leader_proposal_fanout_bytes: u64,
    /// Minimum leader upload bandwidth for proposal fan-out within
    /// `target_secs`, in megabits/s.
    pub min_leader_upload_mbps: u64,
    /// Whether this scenario respects the protocol design cap.
    pub satisfies_design_cap: bool,
    /// Passive/non-committee nodes do not increase all-to-all BFT fanout.
    pub passive_nodes_do_not_increase_bft_fanout: bool,
}

/// Run the BFT capacity budget calculator.
///
/// Defaults to max wire proposal size and the roadmap 30-second finality
/// target. The caller supplies the active DPoS/BFT committee size.
///
/// # Errors
/// Returns an error for empty or oversized committees, zero target
/// seconds, or a proposal larger than the wire codec permits.
pub fn run_bft_capacity(
    validators: usize,
    proposal_bytes: Option<usize>,
    target_secs: Option<u64>,
    network_nodes: Option<u64>,
) -> Result<BftCapacityReport, CliError> {
    let budget = aii_consensus_bft::capacity_budget(
        validators,
        proposal_bytes.unwrap_or_else(aii_consensus_bft::max_wire_proposal_bytes),
        target_secs.unwrap_or(aii_consensus_bft::FINALITY_TARGET_SECS),
    )
    .map_err(|e| CliError::Client(e.to_string()))?;
    let network_nodes = network_nodes.unwrap_or(validators as u64);
    if network_nodes < validators as u64 {
        return Err(CliError::Client(format!(
            "network nodes ({network_nodes}) cannot be less than active validators ({validators})"
        )));
    }
    Ok(BftCapacityReport {
        validators: budget.validators,
        network_nodes,
        consensus_nodes: budget.validators,
        passive_nodes: network_nodes - budget.validators as u64,
        target_secs: budget.target_secs,
        proposal_bytes: budget.proposal_bytes,
        equal_stake_quorum_votes: budget.equal_stake_quorum_votes,
        vote_messages_per_round: budget.vote_messages_per_round,
        vote_payload_bytes_per_round: budget.vote_payload_bytes_per_round,
        leader_proposal_fanout_bytes: budget.leader_proposal_fanout_bytes,
        min_leader_upload_mbps: budget.min_leader_upload_mbps,
        satisfies_design_cap: budget.satisfies_design_cap(),
        passive_nodes_do_not_increase_bft_fanout: true,
    })
}

/// Measured BFT pressure report for the active committee path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BftPressureReport {
    /// Active DPoS/BFT validators in the measured committee.
    pub validators: usize,
    /// Total online network nodes in the modeled deployment.
    pub network_nodes: u64,
    /// Nodes participating in the BFT consensus committee.
    pub consensus_nodes: usize,
    /// Online nodes that sync/serve the network but do not add BFT votes.
    pub passive_nodes: u64,
    /// Heights measured by this pressure run.
    pub heights: u64,
    /// Target seconds available per height.
    pub target_secs: u64,
    /// Equal-stake validators required to cross 2/3 + 1 quorum.
    pub equal_stake_quorum_votes: usize,
    /// PRE-VOTE + PRE-COMMIT votes signed and submitted.
    pub votes_processed: u64,
    /// POLC + PRE-COMMIT certificates formed and verified.
    pub certificates_verified: u64,
    /// Total wall-clock time for the measured section.
    pub elapsed_ms: u128,
    /// Slowest single-height wall-clock time.
    pub max_height_ms: u128,
    /// Average wall-clock time per height.
    pub avg_height_ms: u128,
    /// Whether the slowest measured height fit inside `target_secs`.
    pub satisfies_target: bool,
    /// Passive/non-committee nodes do not increase all-to-all BFT fanout.
    pub passive_nodes_do_not_increase_bft_fanout: bool,
}

/// Run an executable BFT pressure check over quorum vote/certificate work.
///
/// The measured section signs and submits quorum PRE-VOTEs, forms and
/// verifies the POLC, signs and submits quorum PRE-COMMITs, then forms
/// and verifies the finality certificate for each height.
///
/// # Errors
/// Returns an error for invalid capacity inputs or if the pressure path
/// fails to reach/verify quorum.
pub fn run_bft_pressure(
    validators: usize,
    network_nodes: Option<u64>,
    heights: Option<u64>,
    target_secs: Option<u64>,
) -> Result<BftPressureReport, CliError> {
    let capacity = run_bft_capacity(validators, None, target_secs, network_nodes)?;
    let heights = heights.unwrap_or(1);
    if heights == 0 {
        return Err(CliError::Client("heights must be greater than zero".into()));
    }

    let (validator_set, bls_keys) = pressure_validator_set(capacity.validators)?;
    let quorum_votes = capacity.equal_stake_quorum_votes;
    let target_ms = u128::from(capacity.target_secs) * 1_000;
    let mut max_height_ms = 0u128;
    let total_started = std::time::Instant::now();

    for height in 1..=heights {
        let height_started = std::time::Instant::now();
        let block_hash = pressure_block_hash(height);
        let mut prevotes =
            aii_consensus_bft::PrevoteTallier::new(block_hash, height, 0, validator_set.clone());
        for (idx, sk) in bls_keys.iter().take(quorum_votes).enumerate() {
            let vote = aii_consensus_bft::PrevoteVote::sign(sk, block_hash, height, 0, idx as u32);
            prevotes
                .submit(vote)
                .map_err(|e| CliError::Client(e.to_string()))?;
        }
        let polc = prevotes
            .try_form_polc()
            .ok_or_else(|| CliError::Client("prevote quorum did not form POLC".into()))?;
        polc.verify(&validator_set)
            .map_err(|e| CliError::Client(e.to_string()))?;

        let mut precommits =
            aii_consensus_bft::PrecommitTallier::new(block_hash, height, 0, validator_set.clone());
        for (idx, sk) in bls_keys.iter().take(quorum_votes).enumerate() {
            let vote =
                aii_consensus_bft::PrecommitVote::sign(sk, block_hash, height, 0, idx as u32);
            precommits
                .submit(vote)
                .map_err(|e| CliError::Client(e.to_string()))?;
        }
        let certificate = precommits
            .try_finalize()
            .ok_or_else(|| CliError::Client("precommit quorum did not finalize".into()))?;
        certificate
            .verify(&validator_set)
            .map_err(|e| CliError::Client(e.to_string()))?;

        max_height_ms = max_height_ms.max(height_started.elapsed().as_millis());
    }

    let elapsed_ms = total_started.elapsed().as_millis();
    Ok(BftPressureReport {
        validators: capacity.validators,
        network_nodes: capacity.network_nodes,
        consensus_nodes: capacity.consensus_nodes,
        passive_nodes: capacity.passive_nodes,
        heights,
        target_secs: capacity.target_secs,
        equal_stake_quorum_votes: quorum_votes,
        votes_processed: heights * quorum_votes as u64 * 2,
        certificates_verified: heights * 2,
        elapsed_ms,
        max_height_ms,
        avg_height_ms: elapsed_ms / u128::from(heights),
        satisfies_target: max_height_ms <= target_ms,
        passive_nodes_do_not_increase_bft_fanout: true,
    })
}

fn pressure_validator_set(
    validators: usize,
) -> Result<
    (
        aii_consensus_bft::ValidatorSet,
        Vec<aii_crypto::bls::SecretKey>,
    ),
    CliError,
> {
    let mut entries = Vec::with_capacity(validators);
    let mut bls_keys = Vec::with_capacity(validators);
    for i in 0..validators {
        let mut ikm = [0u8; 32];
        ikm[0..8].copy_from_slice(&(i as u64 + 1).to_be_bytes());
        ikm[8..16].copy_from_slice(b"AII-BFTP");
        let bls = aii_crypto::bls::SecretKey::from_ikm(&ikm, b"AII-BFT-PRESSURE")
            .map_err(|e| CliError::Client(e.to_string()))?;
        let vrf = aii_crypto::vrf::SecretKey::generate();
        entries.push(aii_consensus_bft::Validator {
            bls_pubkey: bls.public_key(),
            vrf_pubkey: vrf.public_key(),
            stake: 1,
        });
        bls_keys.push(bls);
    }
    let validator_set = aii_consensus_bft::ValidatorSet::new(entries)
        .map_err(|e| CliError::Client(e.to_string()))?;
    Ok((validator_set, bls_keys))
}

fn pressure_block_hash(height: u64) -> aii_types::H256 {
    let mut bytes = [0u8; 32];
    bytes[0..8].copy_from_slice(&height.to_be_bytes());
    bytes[8..20].copy_from_slice(b"AII-PRESSURE");
    aii_types::H256::new(bytes)
}

// ──────────────────────── Discovery diagnostics ────────────────────────────

/// Default public testnet Discovery v4 seeds used by `aii discovery-probe`.
pub const DEFAULT_DISCOVERY_PROBE_SEEDS: &[&str] = &["8.211.135.234:30310", "106.14.223.128:30310"];
/// Default HTTP bootnodes used as a TCP peer-discovery fallback when
/// UDP Discovery v4 is filtered before packets reach the seed.
pub const DEFAULT_DISCOVERY_PROBE_HTTP_BOOTNODES: &[&str] =
    &["http://8.211.135.234:8545", "http://106.14.223.128:8545"];

/// Result from one Discovery v4 probe window.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiscoveryProbeReport {
    /// Seed specs supplied to the probe before DNS/socket resolution.
    pub seed_specs: Vec<String>,
    /// Seed socket addresses that resolved and were queried.
    pub resolved_seeds: Vec<String>,
    /// BFT TCP peers returned by `Neighbours`.
    pub discovered_bft_peers: Vec<String>,
    /// UDP Discovery v4 peers returned by `Neighbours`.
    pub discovered_discovery_peers: Vec<String>,
    /// HTTP bootnode RPC URLs queried for `aii_peers` fallback data.
    pub http_bootnodes: Vec<String>,
    /// BFT TCP peers returned by HTTP bootnode `aii_peers`.
    pub http_fallback_bft_peers: Vec<String>,
    /// Public UDP endpoint observed by a seed/responder via `Pong.to`.
    pub observed_discovery: Option<String>,
    /// Probe wall-clock duration in milliseconds.
    pub elapsed_ms: u128,
}

/// Run a one-shot Discovery v4 probe against public or operator-supplied seeds.
///
/// This is a diagnostic command: it binds a temporary UDP socket,
/// pings each seed, asks for neighbours, and reports both discovered
/// BFT peers and the public UDP endpoint the seed observed for us.
///
/// # Errors
/// Returns bind, packet signing, or UDP transport errors from the
/// discovery layer.
#[allow(clippy::too_many_lines)]
pub async fn run_discovery_probe(
    seed_specs: &[String],
    listen: std::net::SocketAddr,
    bft_listen: std::net::SocketAddr,
    timeout_ms: u64,
    http_bootnodes: &[String],
) -> Result<DiscoveryProbeReport, CliError> {
    use aii_net_p2p::discovery::{
        expiration_in, Endpoint, FindNode, Neighbours, Packet, Ping, UdpDiscovery,
        DISCOVERY_VERSION,
    };
    use std::collections::BTreeSet;
    use std::time::{Duration, Instant};

    let resolved_seeds = resolve_probe_seed_specs(seed_specs);
    let started = Instant::now();
    let mut found_bft = BTreeSet::new();
    let mut found_discovery = BTreeSet::new();
    let mut observed_discovery = None;

    if !resolved_seeds.is_empty() {
        let driver = UdpDiscovery::bind(listen, probe_secret_key())
            .await
            .map_err(|e| CliError::Client(format!("discovery: {e}")))?;
        let local = Endpoint {
            ip: driver.local_addr().ip(),
            udp_port: driver.local_addr().port(),
            tcp_port: bft_listen.port(),
        };
        let target = aii_crypto::keccak256(&driver.local_addr().to_string().into_bytes());

        for seed in &resolved_seeds {
            let seed_ep = Endpoint {
                ip: seed.ip(),
                udp_port: seed.port(),
                tcp_port: 0,
            };
            let ping = Packet::Ping(Ping {
                version: DISCOVERY_VERSION,
                from: local.clone(),
                to: seed_ep,
                expiration: expiration_in(60),
            });
            let _ = driver.send(*seed, &ping).await;
            let find = Packet::FindNode(FindNode {
                target,
                expiration: expiration_in(60),
            });
            let _ = driver.send(*seed, &find).await;
        }

        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        while tokio::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let slice = remaining.min(Duration::from_millis(100));
            let Ok((decoded, src)) = driver.recv(slice).await else {
                continue;
            };
            match decoded.packet {
                Packet::Ping(p) => {
                    let observed = Endpoint {
                        ip: src.ip(),
                        udp_port: src.port(),
                        tcp_port: p.from.tcp_port,
                    };
                    let _ = driver
                        .send(
                            src,
                            &Packet::Pong(aii_net_p2p::discovery::Pong {
                                to: observed,
                                ping_hash: decoded.packet_hash,
                                expiration: expiration_in(60),
                            }),
                        )
                        .await;
                }
                Packet::FindNode(_) => {
                    let _ = driver
                        .send(
                            src,
                            &Packet::Neighbours(Neighbours {
                                nodes: Vec::new(),
                                expiration: expiration_in(60),
                            }),
                        )
                        .await;
                }
                Packet::Neighbours(n) => {
                    for node in &n.nodes {
                        if let Some(peer) = endpoint_to_probe_peer(node, false) {
                            found_bft.insert(peer);
                        }
                        if let Some(peer) = endpoint_to_probe_peer(node, true) {
                            found_discovery.insert(peer);
                        }
                    }
                }
                Packet::Pong(p) => {
                    if observed_discovery.is_none() {
                        observed_discovery =
                            endpoint_to_probe_peer(&p.to, true).map(|addr| addr.to_string());
                    }
                }
            }
        }
    }

    let http_fallback_bft_peers = fetch_probe_http_bootnode_peers(http_bootnodes).await;
    for peer in &http_fallback_bft_peers {
        if let Ok(addr) = peer.parse::<std::net::SocketAddr>() {
            found_bft.insert(addr);
        }
    }

    Ok(DiscoveryProbeReport {
        seed_specs: seed_specs.to_vec(),
        resolved_seeds: resolved_seeds
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        discovered_bft_peers: found_bft
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        discovered_discovery_peers: found_discovery
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        http_bootnodes: http_bootnodes.to_vec(),
        http_fallback_bft_peers,
        observed_discovery,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

async fn fetch_probe_http_bootnode_peers(http_bootnodes: &[String]) -> Vec<String> {
    let mut out = std::collections::BTreeSet::new();
    for bootnode in http_bootnodes {
        let Ok(client) = HttpClientBuilder::default().build(bootnode) else {
            continue;
        };
        let Ok(peers) = client
            .request::<Vec<String>, _>("aii_peers", rpc_params![])
            .await
        else {
            continue;
        };
        for peer in peers {
            if let Ok(addr) = peer.parse::<std::net::SocketAddr>() {
                out.insert(addr);
            }
        }
    }
    out.iter().map(ToString::to_string).collect()
}

fn resolve_probe_seed_specs(seed_specs: &[String]) -> Vec<std::net::SocketAddr> {
    use std::net::ToSocketAddrs;

    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for spec in seed_specs {
        if let Ok(addr) = spec.parse::<std::net::SocketAddr>() {
            if seen.insert(addr) {
                out.push(addr);
            }
            continue;
        }
        if let Ok(addrs) = spec.to_socket_addrs() {
            for addr in addrs {
                if seen.insert(addr) {
                    out.push(addr);
                }
            }
        }
    }
    out
}

fn endpoint_to_probe_peer(
    endpoint: &aii_net_p2p::discovery::Endpoint,
    discovery_port: bool,
) -> Option<std::net::SocketAddr> {
    let port = if discovery_port {
        endpoint.udp_port
    } else {
        endpoint.tcp_port
    };
    let peer = std::net::SocketAddr::new(endpoint.ip, port);
    (!peer.ip().is_unspecified() && peer.port() != 0).then_some(peer)
}

fn probe_secret_key() -> aii_crypto::secp::SecretKey {
    let seed = format!(
        "{}:{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    );
    for counter in 0u64.. {
        let mut input = seed.as_bytes().to_vec();
        input.extend_from_slice(&counter.to_be_bytes());
        let bytes = *aii_crypto::keccak256(&input).as_bytes();
        if let Ok(sk) = aii_crypto::secp::SecretKey::from_bytes(&bytes) {
            return sk;
        }
    }
    unreachable!("secp256k1 key generation loop should eventually find a valid scalar")
}

// ──────────────────────── Validator / Genesis tooling (v0.0.32) ─────────────

/// Plaintext validator keystore.
///
/// **Testnet only** — production deployments should store secret keys
/// in an encrypted keystore. This struct exists to bootstrap node-
/// operator workflows; the format is JSON with `0x`-prefixed hex.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidatorKeystore {
    /// BLS12-381 G1 public key (48-byte compressed, hex).
    pub bls_pubkey: String,
    /// BLS12-381 secret key (32-byte big-endian scalar, hex).
    pub bls_secret: String,
    /// VRF (schnorrkel) public key (32 bytes, hex).
    pub vrf_pubkey: String,
    /// VRF (schnorrkel) secret key (64-byte expanded scalar, hex).
    pub vrf_secret: String,
}

/// Pubkeys-only projection for sharing with a genesis builder.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidatorPubkeys {
    /// BLS pubkey hex.
    pub bls_pubkey: String,
    /// VRF pubkey hex.
    pub vrf_pubkey: String,
}

impl ValidatorKeystore {
    /// Public-key projection for embedding into [`aii_config::Genesis`].
    #[must_use]
    pub fn pubkeys(&self) -> ValidatorPubkeys {
        ValidatorPubkeys {
            bls_pubkey: self.bls_pubkey.clone(),
            vrf_pubkey: self.vrf_pubkey.clone(),
        }
    }
}

fn hex_with_prefix(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

fn decode_hex<const N: usize>(s: &str, label: &'static str) -> Result<[u8; N], CliError> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let raw = hex::decode(s).map_err(|e| CliError::Client(format!("{label}: bad hex: {e}")))?;
    let arr: [u8; N] = raw.try_into().map_err(|v: Vec<u8>| {
        CliError::Client(format!("{label}: expected {N} bytes, got {}", v.len()))
    })?;
    Ok(arr)
}

/// Generate a fresh validator keystore.
///
/// Random BLS + VRF keys with matching pubkeys. The caller is
/// responsible for persisting the JSON to a file readable only by the
/// node operator.
pub fn run_validator_keygen() -> Result<ValidatorKeystore, CliError> {
    let mut rng = rand::thread_rng();
    let mut ikm = [0u8; 32];
    rng.fill_bytes(&mut ikm);
    let bls_secret_key = aii_crypto::bls::SecretKey::from_ikm(&ikm, b"aii-validator")
        .map_err(|e| CliError::Client(format!("bls keygen: {e}")))?;
    let bls_public_key = bls_secret_key.public_key();
    let vrf_secret_key = aii_crypto::vrf::SecretKey::generate();
    let vrf_public_key = vrf_secret_key.public_key();
    Ok(ValidatorKeystore {
        bls_pubkey: hex_with_prefix(&bls_public_key.to_compressed()),
        bls_secret: hex_with_prefix(&bls_secret_key.to_bytes()),
        vrf_pubkey: hex_with_prefix(&vrf_public_key.to_bytes()),
        vrf_secret: hex_with_prefix(&vrf_secret_key.to_bytes()),
    })
}

/// Extract just the public keys from a stored keystore JSON. Used by
/// `aii validator pubkey` when assembling a genesis file from many
/// independent operators.
pub fn run_validator_pubkey(keystore_json: &str) -> Result<ValidatorPubkeys, CliError> {
    let ks: ValidatorKeystore = serde_json::from_str(keystore_json)?;
    // Validate that the secret/public pair is internally consistent —
    // catches a swapped or corrupt file early.
    let sk_bytes = decode_hex::<32>(&ks.bls_secret, "bls_secret")?;
    let bls_sk = aii_crypto::bls::SecretKey::from_bytes(&sk_bytes)
        .map_err(|e| CliError::Client(format!("bls_secret: {e}")))?;
    let expected_pk_bytes = bls_sk.public_key().to_compressed();
    let actual_pk_bytes = decode_hex::<48>(&ks.bls_pubkey, "bls_pubkey")?;
    if expected_pk_bytes != actual_pk_bytes {
        return Err(CliError::Client(
            "bls_pubkey does not match the public key derived from bls_secret".into(),
        ));
    }
    let vrf_sk_bytes = decode_hex::<64>(&ks.vrf_secret, "vrf_secret")?;
    let vrf_sk = aii_crypto::vrf::SecretKey::from_bytes(&vrf_sk_bytes)
        .map_err(|e| CliError::Client(format!("vrf_secret: {e}")))?;
    let expected_vrf_pk = vrf_sk.public_key().to_bytes();
    let actual_vrf_pk = decode_hex::<32>(&ks.vrf_pubkey, "vrf_pubkey")?;
    if expected_vrf_pk != actual_vrf_pk {
        return Err(CliError::Client(
            "vrf_pubkey does not match the public key derived from vrf_secret".into(),
        ));
    }
    Ok(ks.pubkeys())
}

/// One validator's entry as supplied to `run_genesis_init`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidatorEntry {
    /// Pubkeys (BLS + VRF).
    #[serde(flatten)]
    pub pubkeys: ValidatorPubkeys,
    /// Initial stake.
    pub stake: u64,
}

/// Build a genesis JSON string from a chain spec and a validator list.
///
/// `network` accepts `"mainnet"` or `"testnet"`. `timestamp` is unix
/// seconds at genesis. `initial_seed_hex` is `0x`-prefixed 32-byte hex
/// (use [`run_random_seed_hex`] to generate one).
pub fn run_genesis_init(
    network: &str,
    timestamp: u64,
    initial_seed_hex: &str,
    validators: &[ValidatorEntry],
) -> Result<String, CliError> {
    let chain_spec = match network {
        "mainnet" => aii_config::ChainSpec::mainnet(),
        "testnet" => aii_config::ChainSpec::testnet(),
        other => {
            return Err(CliError::Client(format!(
                "unknown network {other}; expected mainnet or testnet"
            )));
        }
    };
    let initial_seed = decode_hex::<32>(initial_seed_hex, "initial_seed")?;

    let mut gen_validators = Vec::with_capacity(validators.len());
    for (i, v) in validators.iter().enumerate() {
        let bls_bytes = decode_hex::<48>(&v.pubkeys.bls_pubkey, "bls_pubkey")?;
        let vrf_bytes = decode_hex::<32>(&v.pubkeys.vrf_pubkey, "vrf_pubkey")?;
        // Validate the BLS pubkey decompresses — catches corrupt files.
        aii_crypto::bls::PublicKey::from_compressed(&bls_bytes).map_err(|e| {
            CliError::Client(format!("validator {i} bls_pubkey: invalid point: {e}"))
        })?;
        aii_crypto::vrf::PublicKey::from_bytes(&vrf_bytes).map_err(|e| {
            CliError::Client(format!("validator {i} vrf_pubkey: invalid point: {e}"))
        })?;
        gen_validators.push(aii_config::GenesisValidator {
            bls_pubkey: aii_types::BlsPubKey::new(bls_bytes),
            vrf_pubkey: aii_types::VrfPubKey::new(vrf_bytes),
            stake: v.stake,
        });
    }

    let genesis = aii_config::Genesis {
        chain_spec,
        timestamp,
        extra_data: format!("aii-{network}").into_bytes(),
        alloc: Vec::new(),
        validators: gen_validators,
        initial_seed,
    };
    Ok(serde_json::to_string_pretty(&genesis)?)
}

/// Validate a genesis JSON: chain spec invariants, validator pubkey
/// decompression, non-empty / non-zero-stake set.
pub fn run_genesis_validate(genesis_json: &str) -> Result<aii_config::Genesis, CliError> {
    let g: aii_config::Genesis = serde_json::from_str(genesis_json)?;
    g.chain_spec
        .validate()
        .map_err(|m| CliError::Client(format!("chain spec: {m}")))?;
    if g.validators.is_empty() {
        return Err(CliError::Client(
            "genesis has no validators — multi-validator chain cannot start".into(),
        ));
    }
    let mut total: u64 = 0;
    for (i, v) in g.validators.iter().enumerate() {
        aii_crypto::bls::PublicKey::from_compressed(&v.bls_pubkey.0)
            .map_err(|e| CliError::Client(format!("validator {i}: bls pubkey invalid: {e}")))?;
        aii_crypto::vrf::PublicKey::from_bytes(&v.vrf_pubkey.0)
            .map_err(|e| CliError::Client(format!("validator {i}: vrf pubkey invalid: {e}")))?;
        total = total
            .checked_add(v.stake)
            .ok_or_else(|| CliError::Client(format!("validator {i}: total stake overflow")))?;
    }
    if total == 0 {
        return Err(CliError::Client("total validator stake is zero".into()));
    }
    Ok(g)
}

/// Generate a fresh 32-byte initial seed as `0x`-prefixed hex. Suitable
/// for the `initial_seed_hex` argument to [`run_genesis_init`].
#[must_use]
pub fn run_random_seed_hex() -> String {
    let mut buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut buf);
    hex_with_prefix(&buf)
}

fn parse_hex_u64(s: &str) -> Option<u64> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    u64::from_str_radix(s, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aii_config::ChainSpec;
    use aii_node::NodeState;

    async fn spawn_node() -> (String, jsonrpsee::server::ServerHandle) {
        let state = NodeState::new_for_tests(ChainSpec::mainnet());
        let (addr, handle) = aii_rpc::serve("127.0.0.1:0".parse().unwrap(), state)
            .await
            .unwrap();
        (format!("http://{addr}"), handle)
    }

    #[test]
    fn subchain_persistent_state_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let st1 =
            SubchainPersistentState::create_fresh(42, SubchainConsensus::Poa, dir.path()).unwrap();
        // Mutate + save.
        let mut st2 = st1.clone();
        st2.head_number = 7;
        st2.head_hash = "0x".to_string() + &hex::encode([0xab; 32]);
        st2.parent_nonce = 3;
        st2.flush_count = 1;
        st2.save(dir.path()).unwrap();
        // Reload.
        let back = SubchainPersistentState::load(dir.path()).unwrap().unwrap();
        assert_eq!(back.sub_chain_id, 42);
        assert_eq!(back.operator_sk_hex, st1.operator_sk_hex);
        assert_eq!(back.head_number, 7);
        assert_eq!(back.parent_nonce, 3);
        // The operator key roundtrips.
        let sk = back.operator_secret_key().unwrap();
        assert_eq!(
            format!("0x{}", hex::encode(sk.to_bytes())),
            back.operator_sk_hex
        );
    }

    #[test]
    fn subchain_persistent_state_load_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(SubchainPersistentState::load(dir.path()).unwrap().is_none());
    }

    #[test]
    fn subchain_consensus_round_trip_via_state_file() {
        let dir = tempfile::tempdir().unwrap();
        let st =
            SubchainPersistentState::create_fresh(7, SubchainConsensus::Bft, dir.path()).unwrap();
        assert_eq!(st.consensus, "bft");
        let back = SubchainPersistentState::load(dir.path()).unwrap().unwrap();
        assert_eq!(back.consensus_kind().unwrap(), SubchainConsensus::Bft);
    }

    #[test]
    fn subchain_consensus_rejects_unknown_label() {
        assert!(SubchainConsensus::parse("foo").is_err());
        assert_eq!(
            SubchainConsensus::parse("poa").unwrap(),
            SubchainConsensus::Poa
        );
        assert_eq!(
            SubchainConsensus::parse("bft").unwrap(),
            SubchainConsensus::Bft
        );
    }

    #[tokio::test]
    async fn status_returns_chain_and_network() {
        let (url, h) = spawn_node().await;
        let r = run_status(&url).await.unwrap();
        assert_eq!(r.chain_id, 99);
        assert_eq!(r.network, "aii-mainnet");
        assert_eq!(r.head_block_number, 0);
        h.stop().unwrap();
    }

    #[tokio::test]
    async fn chain_id_parses_hex_to_decimal() {
        let (url, h) = spawn_node().await;
        let id = run_chain_id(&url).await.unwrap();
        assert_eq!(id, 99);
        h.stop().unwrap();
    }

    #[test]
    fn account_new_returns_distinct_addresses() {
        let a = run_account_new().unwrap();
        let b = run_account_new().unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn account_new_encrypted_then_verify_round_trip() {
        let json = run_account_new_encrypted("hunter2").unwrap();
        let addr = run_account_verify(&json, "hunter2").unwrap();
        // JSON is canonical and contains the embedded address — make sure
        // both paths agree.
        assert!(json.contains(&hex::encode(addr.as_bytes())));
    }

    #[test]
    fn account_verify_wrong_password_errors() {
        let json = run_account_new_encrypted("right").unwrap();
        let err = run_account_verify(&json, "wrong");
        assert!(err.is_err());
    }

    #[test]
    fn account_mnemonic_returns_12_word_phrase_and_address() {
        let r = run_account_mnemonic(12).unwrap();
        assert_eq!(r.word_count, 12);
        assert_eq!(r.phrase.split_whitespace().count(), 12);
        assert!(r.address.starts_with("0x"));
        assert_eq!(r.address.len(), 42);
    }

    #[test]
    fn account_from_mnemonic_matches_canonical_fixture() {
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let addr = run_account_from_mnemonic(phrase, "", 0).unwrap();
        // Canonical MetaMask / ethers fixture.
        assert_eq!(
            hex::encode(addr.as_bytes()).to_lowercase(),
            "9858effd232b4033e47d90003d41ec34ecaeda94"
        );
    }

    #[test]
    fn account_from_mnemonic_rejects_bad_phrase() {
        assert!(run_account_from_mnemonic("not a real phrase", "", 0).is_err());
    }

    #[test]
    fn tier_runs_and_returns_consistent_tier() {
        let r1 = run_tier();
        let r2 = run_tier();
        assert_eq!(r1.tier, r2.tier);
        assert!(r1.score <= 100);
    }

    #[test]
    fn bft_capacity_reports_default_roadmap_budget() {
        let r = run_bft_capacity(21, None, None, None).unwrap();
        assert_eq!(r.validators, 21);
        assert_eq!(r.network_nodes, 21);
        assert_eq!(r.consensus_nodes, 21);
        assert_eq!(r.passive_nodes, 0);
        assert_eq!(r.target_secs, aii_consensus_bft::FINALITY_TARGET_SECS);
        assert_eq!(
            r.proposal_bytes,
            aii_consensus_bft::max_wire_proposal_bytes()
        );
        assert_eq!(r.equal_stake_quorum_votes, 15);
        assert_eq!(r.vote_messages_per_round, 840);
        assert!(r.satisfies_design_cap);
    }

    #[test]
    fn bft_capacity_rejects_oversized_committee() {
        let err =
            run_bft_capacity(aii_consensus_bft::MAX_VALIDATORS + 1, None, None, None).unwrap_err();
        assert!(err.to_string().contains("exceeds maximum"));
    }

    #[test]
    fn bft_capacity_models_large_network_with_capped_active_committee() {
        let r = run_bft_capacity(128, None, Some(30), Some(21_000_000)).unwrap();
        assert_eq!(r.validators, 128);
        assert_eq!(r.network_nodes, 21_000_000);
        assert_eq!(r.passive_nodes, 20_999_872);
        assert_eq!(r.consensus_nodes, 128);
        assert!(r.satisfies_design_cap);
        assert!(r.passive_nodes_do_not_increase_bft_fanout);
    }

    #[test]
    fn bft_pressure_processes_quorum_votes_and_certificates() {
        let r = run_bft_pressure(4, Some(21_000_000), Some(2), Some(30)).unwrap();
        assert_eq!(r.validators, 4);
        assert_eq!(r.network_nodes, 21_000_000);
        assert_eq!(r.consensus_nodes, 4);
        assert_eq!(r.passive_nodes, 20_999_996);
        assert_eq!(r.heights, 2);
        assert_eq!(r.equal_stake_quorum_votes, 3);
        assert_eq!(r.votes_processed, 12);
        assert_eq!(r.certificates_verified, 4);
        assert!(r.satisfies_target);
        assert!(r.passive_nodes_do_not_increase_bft_fanout);
    }

    #[test]
    fn bft_pressure_rejects_zero_heights() {
        let err = run_bft_pressure(4, None, Some(0), None).unwrap_err();
        assert!(err
            .to_string()
            .contains("heights must be greater than zero"));
    }

    #[test]
    fn parse_aii_decimal_to_wei_accepts_fractional_values() {
        assert_eq!(parse_aii_decimal_to_wei("0.1").unwrap(), WEI_PER_AII / 10);
        assert_eq!(parse_aii_decimal_to_wei("50").unwrap(), WEI_PER_AII * 50);
        assert_eq!(
            parse_aii_decimal_to_wei("1.000000000000000001").unwrap(),
            WEI_PER_AII + 1
        );
        assert!(parse_aii_decimal_to_wei("1.0000000000000000001").is_err());
        assert!(parse_aii_decimal_to_wei("-1").is_err());
    }

    #[test]
    fn local_transfer_load_executes_four_account_value_range() {
        let r = run_local_transfer_load(9999, 40, 4, "0.1", "50", 10).unwrap();
        assert_eq!(r.chain_id, 9999);
        assert_eq!(r.accounts.len(), 4);
        assert_eq!(r.total_requested, 40);
        assert_eq!(r.executed, 40);
        assert_eq!(r.failed, 0);
        assert_eq!(r.simulated_blocks, 4);
        assert_eq!(r.min_value_wei, (WEI_PER_AII / 10).to_string());
        assert_eq!(r.max_value_wei, (WEI_PER_AII * 50).to_string());
        assert_eq!(r.total_gas_used, 40 * 21_000);
        assert_eq!(r.accounts.iter().map(|a| a.nonce).sum::<u64>(), 40);
    }

    #[tokio::test]
    async fn discovery_probe_returns_empty_report_for_unresolvable_seed() {
        let report = run_discovery_probe(
            &["not a socket address".to_string()],
            "127.0.0.1:0".parse().unwrap(),
            "127.0.0.1:30311".parse().unwrap(),
            1,
            &[],
        )
        .await
        .unwrap();
        assert_eq!(report.resolved_seeds, Vec::<String>::new());
        assert_eq!(report.discovered_bft_peers, Vec::<String>::new());
        assert_eq!(report.observed_discovery, None);
    }

    #[tokio::test]
    async fn discovery_probe_reports_neighbours_and_observed_endpoint() {
        use aii_net_p2p::discovery::{
            expiration_in, Endpoint, Neighbours, Packet, Pong, UdpDiscovery,
        };

        fn fixed_secret(byte: u8) -> aii_crypto::secp::SecretKey {
            let mut bytes = [0u8; 32];
            bytes[31] = byte;
            aii_crypto::secp::SecretKey::from_bytes(&bytes).unwrap()
        }

        let seed = UdpDiscovery::bind("127.0.0.1:0".parse().unwrap(), fixed_secret(1))
            .await
            .unwrap();
        let seed_addr = seed.local_addr();
        let observed = "127.0.0.1:43000".parse::<std::net::SocketAddr>().unwrap();
        let advertised_bft = "127.0.0.1:30331".parse::<std::net::SocketAddr>().unwrap();
        let advertised_discovery = "127.0.0.1:30330".parse::<std::net::SocketAddr>().unwrap();

        let responder = tokio::spawn(async move {
            for _ in 0..2 {
                let Ok((decoded, src)) = seed.recv(std::time::Duration::from_secs(2)).await else {
                    continue;
                };
                match decoded.packet {
                    Packet::Ping(p) => {
                        let _ = seed
                            .send(
                                src,
                                &Packet::Pong(Pong {
                                    to: Endpoint {
                                        ip: observed.ip(),
                                        udp_port: observed.port(),
                                        tcp_port: p.from.tcp_port,
                                    },
                                    ping_hash: decoded.packet_hash,
                                    expiration: expiration_in(60),
                                }),
                            )
                            .await;
                    }
                    Packet::FindNode(_) => {
                        let _ = seed
                            .send(
                                src,
                                &Packet::Neighbours(Neighbours {
                                    nodes: vec![Endpoint {
                                        ip: advertised_bft.ip(),
                                        udp_port: advertised_discovery.port(),
                                        tcp_port: advertised_bft.port(),
                                    }],
                                    expiration: expiration_in(60),
                                }),
                            )
                            .await;
                    }
                    Packet::Pong(_) | Packet::Neighbours(_) => {}
                }
            }
        });

        let report = run_discovery_probe(
            &[seed_addr.to_string()],
            "127.0.0.1:0".parse().unwrap(),
            "127.0.0.1:30311".parse().unwrap(),
            1_000,
            &[],
        )
        .await
        .unwrap();
        responder.await.unwrap();
        assert_eq!(report.resolved_seeds, vec![seed_addr.to_string()]);
        assert_eq!(
            report.discovered_bft_peers,
            vec![advertised_bft.to_string()],
        );
        assert_eq!(
            report.discovered_discovery_peers,
            vec![advertised_discovery.to_string()],
        );
        assert_eq!(report.observed_discovery, Some(observed.to_string()));
    }

    #[tokio::test]
    async fn discovery_probe_reports_http_bootnode_peers_when_udp_finds_none() {
        let state = NodeState::new_for_tests(ChainSpec::mainnet());
        state.set_bft_peers(&[
            "127.0.0.1:30311".parse().unwrap(),
            "127.0.0.1:30312".parse().unwrap(),
        ]);
        let (addr, handle) = aii_rpc::serve("127.0.0.1:0".parse().unwrap(), state)
            .await
            .unwrap();
        let bootnode = format!("http://{addr}");

        let report = run_discovery_probe(
            &["127.0.0.1:9".to_string()],
            "127.0.0.1:0".parse().unwrap(),
            "127.0.0.1:30311".parse().unwrap(),
            10,
            std::slice::from_ref(&bootnode),
        )
        .await
        .unwrap();

        handle.stop().unwrap();
        assert_eq!(report.http_bootnodes, vec![bootnode]);
        assert_eq!(
            report.http_fallback_bft_peers,
            vec!["127.0.0.1:30311", "127.0.0.1:30312"],
        );
        assert_eq!(report.discovered_bft_peers, report.http_fallback_bft_peers);
        assert_eq!(report.observed_discovery, None);
    }

    // ───────────────────── validator / genesis tooling tests ─────────────────────

    #[test]
    fn validator_keygen_produces_well_formed_hex() {
        let ks = run_validator_keygen().unwrap();
        assert!(ks.bls_pubkey.starts_with("0x"));
        assert_eq!(ks.bls_pubkey.len(), 2 + 48 * 2);
        assert_eq!(ks.bls_secret.len(), 2 + 32 * 2);
        assert_eq!(ks.vrf_pubkey.len(), 2 + 32 * 2);
        assert_eq!(ks.vrf_secret.len(), 2 + 64 * 2);
    }

    #[test]
    fn validator_keygen_produces_distinct_keys() {
        let a = run_validator_keygen().unwrap();
        let b = run_validator_keygen().unwrap();
        assert_ne!(a.bls_secret, b.bls_secret);
        assert_ne!(a.vrf_secret, b.vrf_secret);
    }

    #[test]
    fn validator_pubkey_extracts_pubkeys_from_keystore() {
        let ks = run_validator_keygen().unwrap();
        let json = serde_json::to_string(&ks).unwrap();
        let pub_only = run_validator_pubkey(&json).unwrap();
        assert_eq!(pub_only.bls_pubkey, ks.bls_pubkey);
        assert_eq!(pub_only.vrf_pubkey, ks.vrf_pubkey);
    }

    #[test]
    fn validator_pubkey_rejects_swapped_pubkey() {
        let mut a = run_validator_keygen().unwrap();
        let b = run_validator_keygen().unwrap();
        a.bls_pubkey = b.bls_pubkey; // forge: pubkey doesn't match secret
        let json = serde_json::to_string(&a).unwrap();
        assert!(run_validator_pubkey(&json).is_err());
    }

    #[test]
    fn validator_pubkey_rejects_malformed_json() {
        assert!(run_validator_pubkey("not json").is_err());
    }

    #[test]
    fn genesis_init_produces_valid_json() {
        let ks = run_validator_keygen().unwrap();
        let seed = run_random_seed_hex();
        let entry = ValidatorEntry {
            pubkeys: ks.pubkeys(),
            stake: 100,
        };
        let json = run_genesis_init("testnet", 1_700_000_000, &seed, &[entry]).unwrap();
        // Genesis must parse back via aii-config.
        let parsed: aii_config::Genesis = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.chain_spec.chain_id, 9999);
        assert_eq!(parsed.validators.len(), 1);
        assert_eq!(parsed.timestamp, 1_700_000_000);
    }

    #[test]
    fn genesis_init_rejects_unknown_network() {
        let r = run_genesis_init("starknet", 0, &"0x".repeat(0), &[]);
        assert!(r.is_err());
    }

    #[test]
    fn genesis_init_rejects_bad_bls_pubkey() {
        let entry = ValidatorEntry {
            pubkeys: ValidatorPubkeys {
                bls_pubkey: "0xdeadbeef".into(), // wrong length
                vrf_pubkey: format!("0x{}", "00".repeat(32)),
            },
            stake: 100,
        };
        assert!(run_genesis_init("testnet", 0, &run_random_seed_hex(), &[entry]).is_err());
    }

    #[test]
    fn genesis_validate_accepts_valid_genesis() {
        let ks = run_validator_keygen().unwrap();
        let entry = ValidatorEntry {
            pubkeys: ks.pubkeys(),
            stake: 100,
        };
        let json = run_genesis_init("testnet", 0, &run_random_seed_hex(), &[entry]).unwrap();
        let g = run_genesis_validate(&json).unwrap();
        assert_eq!(g.validators.len(), 1);
    }

    #[test]
    fn genesis_validate_rejects_empty_validators() {
        let g = aii_config::Genesis {
            chain_spec: aii_config::ChainSpec::testnet(),
            timestamp: 0,
            extra_data: vec![],
            alloc: vec![],
            validators: vec![],
            initial_seed: [0; 32],
        };
        let json = serde_json::to_string(&g).unwrap();
        assert!(run_genesis_validate(&json).is_err());
    }

    #[test]
    fn genesis_validate_rejects_zero_stake_total() {
        let ks = run_validator_keygen().unwrap();
        let g = aii_config::Genesis {
            chain_spec: aii_config::ChainSpec::testnet(),
            timestamp: 0,
            extra_data: vec![],
            alloc: vec![],
            validators: vec![aii_config::GenesisValidator {
                bls_pubkey: aii_types::BlsPubKey::new(
                    decode_hex::<48>(&ks.bls_pubkey, "x").unwrap(),
                ),
                vrf_pubkey: aii_types::VrfPubKey::new(
                    decode_hex::<32>(&ks.vrf_pubkey, "x").unwrap(),
                ),
                stake: 0,
            }],
            initial_seed: [0; 32],
        };
        let json = serde_json::to_string(&g).unwrap();
        assert!(run_genesis_validate(&json).is_err());
    }

    /// End-to-end: 3 fresh validator keystores → genesis JSON → load
    /// the genesis back and confirm BftConfig::from_genesis succeeds
    /// for each operator with their own secret material.
    #[test]
    fn three_validator_workflow_produces_loadable_bft_config() {
        let mut keystores = Vec::new();
        let mut entries = Vec::new();
        for _ in 0..3 {
            let ks = run_validator_keygen().unwrap();
            entries.push(ValidatorEntry {
                pubkeys: ks.pubkeys(),
                stake: 100,
            });
            keystores.push(ks);
        }
        let seed = run_random_seed_hex();
        let genesis_json = run_genesis_init("testnet", 1_700_000_000, &seed, &entries).unwrap();
        let parsed: aii_config::Genesis = serde_json::from_str(&genesis_json).unwrap();
        // Each operator can spin up their own BftConfig from this genesis.
        for (i, ks) in keystores.iter().enumerate() {
            let bls_sk_bytes = decode_hex::<32>(&ks.bls_secret, "x").unwrap();
            let bls_sk = aii_crypto::bls::SecretKey::from_bytes(&bls_sk_bytes).unwrap();
            let vrf_sk_bytes = decode_hex::<64>(&ks.vrf_secret, "x").unwrap();
            let vrf_sk = aii_crypto::vrf::SecretKey::from_bytes(&vrf_sk_bytes).unwrap();
            let cfg = aii_consensus_bft::BftConfig::from_genesis(
                &parsed,
                u32::try_from(i).unwrap(),
                bls_sk,
                vrf_sk,
                aii_types::Address::new([0xab; 20]),
            )
            .unwrap();
            assert_eq!(cfg.validator_set.size(), 3);
            assert_eq!(cfg.my_index, u32::try_from(i).unwrap());
        }
    }
}
