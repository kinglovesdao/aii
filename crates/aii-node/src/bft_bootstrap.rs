//! Bootstrap helpers for spinning up a `BftEngine` from on-disk files.
//!
//! Used by the `aiid` binary's `--bft` mode:
//! 1. Load a `Genesis` JSON file.
//! 2. Load a `ValidatorKeystore` JSON file (the operator's secret material).
//! 3. Derive `my_index` by matching the keystore's BLS pubkey against the
//!    validator entries in the genesis.
//! 4. Build a `BftConfig` and a `BftEngine` ready to advance the chain.

use std::path::Path;

use aii_cli::ValidatorKeystore;
use aii_config::Genesis;
use aii_consensus_bft::{BftConfig, BftEngine, BftError};
use aii_types::Address;
use thiserror::Error;

/// Errors raised by the BFT bootstrap helpers.
#[derive(Debug, Error)]
pub enum BootstrapError {
    /// Filesystem read error.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// JSON parse error.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// Hex decode error.
    #[error("hex: {0}")]
    Hex(String),
    /// Keystore secret/public key mismatch, malformed length, etc.
    #[error("keystore: {0}")]
    Keystore(String),
    /// The keystore's BLS pubkey does not match any entry in the genesis
    /// validator set — this operator's key isn't part of this chain.
    #[error("keystore is not a validator in this genesis (bls={0})")]
    NotAValidator(String),
    /// `BftConfig::from_genesis` rejected the supplied material.
    #[error("bft: {0}")]
    Bft(#[from] BftError),
}

/// Load and parse a `Genesis` JSON file.
pub fn load_genesis(path: &Path) -> Result<Genesis, BootstrapError> {
    let text = std::fs::read_to_string(path)?;
    let g: Genesis = serde_json::from_str(&text)?;
    Ok(g)
}

/// Load and parse a `ValidatorKeystore` JSON file.
pub fn load_keystore(path: &Path) -> Result<ValidatorKeystore, BootstrapError> {
    let text = std::fs::read_to_string(path)?;
    let ks: ValidatorKeystore = serde_json::from_str(&text)?;
    Ok(ks)
}

/// Find the index of `keystore.bls_pubkey` inside `genesis.validators`,
/// or `NotAValidator` if it's not present.
pub fn discover_my_index(
    genesis: &Genesis,
    keystore: &ValidatorKeystore,
) -> Result<u32, BootstrapError> {
    let needle = keystore.bls_pubkey.trim_start_matches("0x").to_lowercase();
    for (i, v) in genesis.validators.iter().enumerate() {
        let hay = hex::encode(v.bls_pubkey.0);
        if hay == needle {
            return u32::try_from(i).map_err(|_| {
                BootstrapError::Keystore("validator index does not fit in u32".into())
            });
        }
    }
    Err(BootstrapError::NotAValidator(keystore.bls_pubkey.clone()))
}

fn decode_fixed<const N: usize>(
    hex_str: &str,
    label: &'static str,
) -> Result<[u8; N], BootstrapError> {
    let s = hex_str.trim_start_matches("0x");
    let raw = hex::decode(s).map_err(|e| BootstrapError::Hex(format!("{label}: {e}")))?;
    raw.try_into().map_err(|v: Vec<u8>| {
        BootstrapError::Keystore(format!("{label}: expected {N} bytes, got {}", v.len()))
    })
}

/// Build a `BftConfig` from the genesis + keystore + coinbase.
///
/// `my_index` is discovered automatically by matching the keystore's
/// BLS pubkey against the genesis validator set; pass `Some(n)` to
/// override (e.g. for tests with deliberately-mismatched indices).
pub fn build_bft_config(
    genesis: &Genesis,
    keystore: &ValidatorKeystore,
    coinbase: Address,
    my_index_override: Option<u32>,
) -> Result<BftConfig, BootstrapError> {
    let my_index = match my_index_override {
        Some(i) => i,
        None => discover_my_index(genesis, keystore)?,
    };
    let bls_sk_bytes = decode_fixed::<32>(&keystore.bls_secret, "bls_secret")?;
    let bls_sk = aii_crypto::bls::SecretKey::from_bytes(&bls_sk_bytes)
        .map_err(|e| BootstrapError::Keystore(format!("bls_secret: {e}")))?;
    let vrf_sk_bytes = decode_fixed::<64>(&keystore.vrf_secret, "vrf_secret")?;
    let vrf_sk = aii_crypto::vrf::SecretKey::from_bytes(&vrf_sk_bytes)
        .map_err(|e| BootstrapError::Keystore(format!("vrf_secret: {e}")))?;
    let cfg = BftConfig::from_genesis(genesis, my_index, bls_sk, vrf_sk, coinbase)?;
    Ok(cfg)
}

/// One-shot constructor: load genesis + keystore from disk and produce
/// the engine ready to advance.
///
/// Also returns the genesis-block representation so the caller can
/// pass it to `BftEngine::new`.
pub fn boot_bft_engine(
    genesis_path: &Path,
    keystore_path: &Path,
    coinbase: Address,
) -> Result<(BftEngine, Genesis), BootstrapError> {
    let genesis = load_genesis(genesis_path)?;
    let keystore = load_keystore(keystore_path)?;
    let cfg = build_bft_config(&genesis, &keystore, coinbase, None)?;
    let genesis_block = aii_block::Block {
        header: genesis.to_header(aii_state::EMPTY_TRIE_HASH),
        body: aii_block::BlockBody::default(),
    };
    let engine = BftEngine::new(cfg, &genesis_block);
    Ok((engine, genesis))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aii_cli::{run_genesis_init, run_random_seed_hex, run_validator_keygen, ValidatorEntry};
    use std::io::Write;

    fn write_tmp(contents: &str, suffix: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::Builder::new().suffix(suffix).tempfile().unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        f
    }

    #[test]
    fn discover_my_index_finds_matching_validator() {
        let ks_a = run_validator_keygen().unwrap();
        let ks_b = run_validator_keygen().unwrap();
        let genesis_json = run_genesis_init(
            "testnet",
            1_700_000_000,
            &run_random_seed_hex(),
            &[
                ValidatorEntry {
                    pubkeys: ks_a.pubkeys(),
                    stake: 100,
                },
                ValidatorEntry {
                    pubkeys: ks_b.pubkeys(),
                    stake: 100,
                },
            ],
        )
        .unwrap();
        let g: Genesis = serde_json::from_str(&genesis_json).unwrap();
        assert_eq!(discover_my_index(&g, &ks_a).unwrap(), 0);
        assert_eq!(discover_my_index(&g, &ks_b).unwrap(), 1);
    }

    #[test]
    fn discover_my_index_rejects_unknown_keystore() {
        let ks_known = run_validator_keygen().unwrap();
        let ks_stranger = run_validator_keygen().unwrap();
        let genesis_json = run_genesis_init(
            "testnet",
            0,
            &run_random_seed_hex(),
            &[ValidatorEntry {
                pubkeys: ks_known.pubkeys(),
                stake: 100,
            }],
        )
        .unwrap();
        let g: Genesis = serde_json::from_str(&genesis_json).unwrap();
        assert!(matches!(
            discover_my_index(&g, &ks_stranger).unwrap_err(),
            BootstrapError::NotAValidator(_)
        ));
    }

    #[test]
    fn build_bft_config_from_in_memory_genesis_works() {
        let ks = run_validator_keygen().unwrap();
        let genesis_json = run_genesis_init(
            "testnet",
            0,
            &run_random_seed_hex(),
            &[ValidatorEntry {
                pubkeys: ks.pubkeys(),
                stake: 100,
            }],
        )
        .unwrap();
        let g: Genesis = serde_json::from_str(&genesis_json).unwrap();
        let cfg = build_bft_config(&g, &ks, Address::new([0xab; 20]), None).unwrap();
        assert_eq!(cfg.validator_set.size(), 1);
        assert_eq!(cfg.my_index, 0);
        assert_eq!(cfg.coinbase, Address::new([0xab; 20]));
    }

    #[test]
    fn boot_bft_engine_from_disk_advances_one_block() {
        let ks = run_validator_keygen().unwrap();
        let ks_json = serde_json::to_string_pretty(&ks).unwrap();
        let genesis_json = run_genesis_init(
            "testnet",
            1_700_000_000,
            &run_random_seed_hex(),
            &[ValidatorEntry {
                pubkeys: ks.pubkeys(),
                stake: 100,
            }],
        )
        .unwrap();

        let ks_file = write_tmp(&ks_json, ".keystore.json");
        let g_file = write_tmp(&genesis_json, ".genesis.json");

        let (engine, g) =
            boot_bft_engine(g_file.path(), ks_file.path(), Address::new([0xcd; 20])).unwrap();
        // Validate against the genesis we recovered along the way.
        assert_eq!(g.validators.len(), 1);
        // Single-validator advance produces a block.
        let out = engine.advance_single().unwrap();
        assert_eq!(out.block.header.number, 1);
    }

    #[test]
    fn load_genesis_rejects_invalid_json() {
        let f = write_tmp("not really json", ".bad.json");
        assert!(matches!(
            load_genesis(f.path()).unwrap_err(),
            BootstrapError::Json(_)
        ));
    }
}
