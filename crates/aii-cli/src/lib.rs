//! # aii-cli (library surface)
//!
//! Pure-function command runners that the `aii` binary wires together.
//! Extracting them as a library lets us unit-test each subcommand against
//! a live RPC server without spawning a subprocess.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

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
            // Build flush tx: self-transfer with calldata = sub_hash || u64::be(n)
            let mut data = Vec::with_capacity(40);
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
        let state = NodeState::new(ChainSpec::mainnet());
        let (addr, handle) = aii_rpc::serve("127.0.0.1:0".parse().unwrap(), state)
            .await
            .unwrap();
        (format!("http://{addr}"), handle)
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
