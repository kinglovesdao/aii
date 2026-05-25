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
