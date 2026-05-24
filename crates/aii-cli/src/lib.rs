//! # aii-cli (library surface)
//!
//! Pure-function command runners that the `aii` binary wires together.
//! Extracting them as a library lets us unit-test each subcommand against
//! a live RPC server without spawning a subprocess.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use aii_onboarding::{detect, recommend_tier, score, Tier};
use aii_wallet::{EncryptedKeystore, LocalWallet, MnemonicPhrase, ScryptParams};
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
}
