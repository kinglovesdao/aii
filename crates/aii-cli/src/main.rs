//! `aii` — user-facing CLI for the AII protocol.

use aii_cli::release::{sign_release, verify_release, ReleaseError};
use aii_cli::{
    run_account_from_mnemonic, run_account_mnemonic, run_account_new, run_account_new_encrypted,
    run_account_verify, run_chain_id, run_genesis_init, run_genesis_validate, run_get_block_header,
    run_random_seed_hex, run_recent_blocks, run_status, run_stress, run_subchain, run_tier,
    run_validator_keygen, run_validator_pubkey, CliError, ValidatorEntry, ValidatorPubkeys,
};
use aii_crypto::ed25519::{PublicKey as Ed25519PublicKey, SecretKey as Ed25519SecretKey};
use clap::{Parser, Subcommand};
use rand_core::OsRng;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "aii", version, about = "AII protocol — user-facing CLI")]
struct Cli {
    /// JSON-RPC endpoint for commands that talk to a node.
    #[arg(long, global = true, default_value = "http://127.0.0.1:8545")]
    rpc: String,

    /// Emit machine-readable JSON instead of human-formatted text.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Print the node's chain id, network, and head block number.
    Status,
    /// Print just the chain id (decimal).
    ChainId,
    /// Account management subcommands.
    Account {
        #[command(subcommand)]
        sub: AccountCmd,
    },
    /// Probe local hardware + recommend a node Tier (T1–T7).
    Tier,
    /// Validator (BFT consensus participant) management.
    Validator {
        #[command(subcommand)]
        sub: ValidatorCmd,
    },
    /// Genesis file generation / validation.
    Genesis {
        #[command(subcommand)]
        sub: GenesisCmd,
    },
    /// Look up a block header by number or hash.
    Block {
        /// Decimal or 0x… hex block number, or 0x… 32-byte block hash.
        query: String,
    },
    /// Print the N most recently finalised block headers.
    Recent {
        /// Max headers to return (server-capped at 100).
        #[arg(long, default_value = "10")]
        limit: u64,
    },
    /// Run an in-process PoA sub-chain and periodically flush
    /// anchors to a parent chain via `eth_sendRawTransaction`.
    Subchain {
        #[command(subcommand)]
        sub: SubchainCmd,
    },
    /// Release-signing tools (v0.0.74) — produce and verify
    /// Ed25519-signed binary release manifests.
    Release {
        #[command(subcommand)]
        sub: ReleaseCmd,
    },
    /// Flood the node with signed self-transfers and report
    /// observed txs/block + throughput.
    Stress {
        /// Chain id of the target network (must match node's).
        #[arg(long, default_value = "9999")]
        chain_id: u64,
        /// Total number of txs to submit.
        #[arg(long, default_value = "5000")]
        total: u64,
        /// Distinct signer addresses (more = wider parallel nonce streams).
        #[arg(long, default_value = "32")]
        senders: u32,
        /// Concurrent RPC workers.
        #[arg(long, default_value = "16")]
        parallel: u32,
        /// Seconds to wait after submission before sampling blocks.
        #[arg(long, default_value = "5")]
        settle_sec: u64,
        /// How many recent blocks to sample for the txs/block stats.
        #[arg(long, default_value = "20")]
        sample_blocks: u64,
    },
}

#[derive(Debug, Subcommand)]
enum SubchainCmd {
    /// Spawn a fresh PoA sub-chain in-process and flush anchors to
    /// `--parent-rpc` every `--flush-interval-blocks` blocks.
    Run {
        /// Sub-chain id (must differ from the parent chain's id).
        #[arg(long, default_value = "10000")]
        sub_chain_id: u64,
        /// Parent chain id (used to EIP-155-sign the flush tx).
        #[arg(long, default_value = "9999")]
        parent_chain_id: u64,
        /// Parent chain JSON-RPC endpoint.
        #[arg(long)]
        parent_rpc: String,
        /// Seconds between sub-chain blocks (PoA slot interval).
        #[arg(long, default_value = "1")]
        slot_seconds: u64,
        /// Every N sub-chain blocks, sign + post one flush anchor tx.
        #[arg(long, default_value = "5")]
        flush_interval_blocks: u64,
        /// Total sub-chain blocks to produce before exiting.
        #[arg(long, default_value = "20")]
        duration_blocks: u64,
    },
}

#[derive(Debug, Subcommand)]
enum ReleaseCmd {
    /// Generate a fresh Ed25519 keypair. Prints the public key
    /// (hex, no prefix) to stdout and writes the secret seed to
    /// `--out` (or also to stdout when omitted).
    Keygen {
        /// File to write the secret seed to. Defaults to stdout.
        /// Treat with the same care as any signing key.
        #[arg(long)]
        out: Option<std::path::PathBuf>,
    },
    /// Hash + sign a binary, producing a `release.json` manifest.
    Sign {
        /// Path to the binary being released.
        #[arg(long)]
        binary: std::path::PathBuf,
        /// Semver-style version string for the manifest.
        #[arg(long)]
        version: String,
        /// Hex-encoded 32-byte secret seed (with or without `0x`).
        /// Read from `--secret-file` to avoid leaking to argv/ps.
        #[arg(long, conflicts_with = "secret_file")]
        secret: Option<String>,
        /// File containing the hex-encoded secret seed.
        #[arg(long)]
        secret_file: Option<std::path::PathBuf>,
        /// Unix-seconds timestamp. Defaults to "now".
        #[arg(long)]
        timestamp: Option<u64>,
        /// Path to write the manifest JSON. Defaults to stdout.
        #[arg(long)]
        out: Option<std::path::PathBuf>,
    },
    /// Verify a `release.json` against a binary + a pinned public key.
    Verify {
        /// Path to the release manifest JSON.
        #[arg(long)]
        manifest: std::path::PathBuf,
        /// Path to the binary the manifest claims to cover.
        #[arg(long)]
        binary: std::path::PathBuf,
        /// Hex-encoded 32-byte Ed25519 public key (with or without `0x`).
        #[arg(long)]
        pubkey: String,
    },
}

#[derive(Debug, Subcommand)]
enum ValidatorCmd {
    /// Generate a fresh BLS + VRF keypair and write the keystore JSON.
    Keygen {
        /// Output file (defaults to stdout).
        #[arg(long)]
        out: Option<std::path::PathBuf>,
    },
    /// Print just the public keys from an existing validator keystore.
    Pubkey {
        /// Keystore JSON path.
        #[arg(long)]
        file: std::path::PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum GenesisCmd {
    /// Build a fresh genesis JSON from a list of validator pubkey files.
    Init {
        /// `mainnet` or `testnet`.
        #[arg(long, default_value = "testnet")]
        network: String,
        /// Unix-seconds timestamp for the genesis block.
        #[arg(long, default_value = "1700000000")]
        timestamp: u64,
        /// 0x-prefixed 32-byte hex; omit to generate a fresh random seed.
        #[arg(long)]
        initial_seed: Option<String>,
        /// Path to a file containing JSON `{ bls_pubkey, vrf_pubkey }` —
        /// repeat once per validator. Each gets `--stake` units of stake.
        #[arg(long = "validator-pubkey")]
        validator_pubkeys: Vec<std::path::PathBuf>,
        /// Per-validator stake (uniform across the set for now).
        #[arg(long, default_value = "100")]
        stake: u64,
        /// Output file (defaults to stdout).
        #[arg(long)]
        out: Option<std::path::PathBuf>,
    },
    /// Load a genesis JSON file and validate it.
    Validate {
        /// Genesis JSON path.
        #[arg(long)]
        file: std::path::PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum AccountCmd {
    /// Generate a fresh secp256k1 keypair and print the address.
    New,
    /// Generate a fresh keypair, encrypt it with `--password`, and write
    /// the JSON keystore to `--out` (or stdout).
    NewEncrypted {
        /// Password to encrypt the keystore under.
        #[arg(long)]
        password: String,
        /// Output file path; defaults to stdout.
        #[arg(long)]
        out: Option<std::path::PathBuf>,
    },
    /// Verify that the password decrypts the keystore JSON at `--file`.
    Verify {
        /// JSON keystore path.
        #[arg(long)]
        file: std::path::PathBuf,
        /// Password to test.
        #[arg(long)]
        password: String,
    },
    /// Generate a fresh BIP-39 mnemonic + first ETH-compatible address.
    Mnemonic {
        /// Word count (12, 15, 18, 21, or 24). Defaults to 12.
        #[arg(long, default_value = "12")]
        words: usize,
    },
    /// Derive an address from an existing mnemonic phrase.
    FromMnemonic {
        /// BIP-39 phrase (quote it on the shell).
        #[arg(long)]
        phrase: String,
        /// Optional BIP-39 passphrase ("25th word"); defaults to empty.
        #[arg(long, default_value = "")]
        passphrase: String,
        /// BIP-44 address index. Defaults to 0.
        #[arg(long, default_value = "0")]
        index: u32,
    },
}

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn main() -> Result<(), CliError> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .try_init();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Status => {
            let s = run_status(&cli.rpc).await?;
            if cli.json {
                println!("{}", serde_json::to_string(&s)?);
            } else {
                println!("chain_id: {}", s.chain_id);
                println!("network:  {}", s.network);
                println!("head:     #{}", s.head_block_number);
            }
        }
        Cmd::ChainId => {
            let id = run_chain_id(&cli.rpc).await?;
            if cli.json {
                println!("{}", serde_json::json!({ "chain_id": id }));
            } else {
                println!("{id}");
            }
        }
        Cmd::Block { query } => {
            let h = run_get_block_header(&cli.rpc, &query).await?;
            match h {
                Some(v) if cli.json => println!("{}", serde_json::to_string(&v)?),
                Some(v) => {
                    println!("number:        {}", v.number);
                    println!("hash:          {}", v.hash);
                    println!("parent_hash:   {}", v.parent_hash);
                    println!("timestamp:     {}", v.timestamp);
                    println!("beneficiary:   {}", v.beneficiary);
                    println!("gas_limit:     {}", v.gas_limit);
                    println!("gas_used:      {}", v.gas_used);
                    println!("base_fee:      {}", v.base_fee_per_gas);
                }
                None if cli.json => println!("null"),
                None => println!("block not found: {query}"),
            }
        }
        Cmd::Subchain {
            sub:
                SubchainCmd::Run {
                    sub_chain_id,
                    parent_chain_id,
                    parent_rpc,
                    slot_seconds,
                    flush_interval_blocks,
                    duration_blocks,
                },
        } => {
            let r = run_subchain(
                sub_chain_id,
                parent_chain_id,
                &parent_rpc,
                slot_seconds,
                flush_interval_blocks,
                duration_blocks,
            )
            .await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                println!("sub_chain_id:    {}", r.sub_chain_id);
                println!("parent_chain_id: {}", r.parent_chain_id);
                println!("sub blocks:      {}", r.sub_blocks_produced);
                println!("sub head:        {}", r.sub_head_hash);
                println!("flushes:");
                for f in &r.flushes {
                    println!(
                        "  sub #{:>4}  hash={}…  parent_tx={}",
                        f.sub_block_number,
                        &f.sub_block_hash[..14],
                        &f.parent_tx
                    );
                }
            }
        }
        Cmd::Release { sub } => {
            handle_release_cmd(sub, cli.json)?;
        }
        Cmd::Stress {
            chain_id,
            total,
            senders,
            parallel,
            settle_sec,
            sample_blocks,
        } => {
            let r = run_stress(
                &cli.rpc,
                chain_id,
                total,
                senders,
                parallel,
                settle_sec,
                sample_blocks,
            )
            .await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                println!("submitted:        {}", r.submitted);
                println!("accepted:         {}", r.accepted);
                println!("blocks observed:  {}", r.blocks_observed);
                println!("txs in blocks:    {}", r.txs_in_blocks);
                println!("peak txs / block: {}", r.peak_txs_per_block);
                println!("mean txs / block: {}", r.mean_txs_per_block);
                println!("submit rate:      {:.0} tx/s", r.submit_tx_per_sec);
                println!("elapsed:          {:.1} s", r.elapsed_sec);
            }
        }
        Cmd::Recent { limit } => {
            let headers = run_recent_blocks(&cli.rpc, limit).await?;
            if cli.json {
                println!("{}", serde_json::to_string(&headers)?);
            } else if headers.is_empty() {
                println!("no blocks yet");
            } else {
                println!("# {:>6}  {:>20}  hash", "number", "timestamp");
                for h in &headers {
                    println!("  {:>6}  {:>20}  {}", h.number, h.timestamp, h.hash);
                }
            }
        }
        Cmd::Account {
            sub: AccountCmd::New,
        } => {
            let addr = run_account_new()?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({ "address": format!("0x{}", hex::encode(addr.as_bytes())) })
                );
            } else {
                println!("address: 0x{}", hex::encode(addr.as_bytes()));
            }
        }
        Cmd::Account {
            sub: AccountCmd::NewEncrypted { password, out },
        } => {
            let json = run_account_new_encrypted(&password)?;
            if let Some(path) = out {
                fs::write(&path, &json).map_err(|e| CliError::Client(e.to_string()))?;
                eprintln!("wrote {}", path.display());
            } else {
                println!("{json}");
            }
        }
        Cmd::Account {
            sub: AccountCmd::Verify { file, password },
        } => {
            let json = fs::read_to_string(&file).map_err(|e| CliError::Client(e.to_string()))?;
            let addr = run_account_verify(&json, &password)?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({ "address": format!("0x{}", hex::encode(addr.as_bytes())), "ok": true })
                );
            } else {
                println!("ok — address: 0x{}", hex::encode(addr.as_bytes()));
            }
        }
        Cmd::Account {
            sub: AccountCmd::Mnemonic { words },
        } => {
            let r = run_account_mnemonic(words)?;
            if cli.json {
                println!("{}", serde_json::to_string(&r)?);
            } else {
                println!("phrase:  {}", r.phrase);
                println!("words:   {}", r.word_count);
                println!("address: {}", r.address);
            }
        }
        Cmd::Account {
            sub:
                AccountCmd::FromMnemonic {
                    phrase,
                    passphrase,
                    index,
                },
        } => {
            let addr = run_account_from_mnemonic(&phrase, &passphrase, index)?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({ "address": format!("0x{}", hex::encode(addr.as_bytes())), "index": index })
                );
            } else {
                println!(
                    "address: 0x{} (index {index})",
                    hex::encode(addr.as_bytes())
                );
            }
        }
        Cmd::Tier => {
            let t = run_tier();
            if cli.json {
                println!("{}", serde_json::to_string(&t)?);
            } else {
                println!("score: {} → {:?}", t.score, t.tier);
            }
        }
        Cmd::Validator {
            sub: ValidatorCmd::Keygen { out },
        } => {
            let ks = run_validator_keygen()?;
            let json = serde_json::to_string_pretty(&ks)?;
            if let Some(path) = out {
                fs::write(&path, &json).map_err(|e| CliError::Client(e.to_string()))?;
                eprintln!("wrote {}", path.display());
            } else {
                println!("{json}");
            }
        }
        Cmd::Validator {
            sub: ValidatorCmd::Pubkey { file },
        } => {
            let json = fs::read_to_string(&file).map_err(|e| CliError::Client(e.to_string()))?;
            let pk = run_validator_pubkey(&json)?;
            if cli.json {
                println!("{}", serde_json::to_string(&pk)?);
            } else {
                println!("bls_pubkey: {}", pk.bls_pubkey);
                println!("vrf_pubkey: {}", pk.vrf_pubkey);
            }
        }
        Cmd::Genesis {
            sub:
                GenesisCmd::Init {
                    network,
                    timestamp,
                    initial_seed,
                    validator_pubkeys,
                    stake,
                    out,
                },
        } => {
            let seed = initial_seed.unwrap_or_else(run_random_seed_hex);
            let mut entries = Vec::new();
            for path in &validator_pubkeys {
                let json = fs::read_to_string(path).map_err(|e| CliError::Client(e.to_string()))?;
                let pk: ValidatorPubkeys = serde_json::from_str(&json)?;
                entries.push(ValidatorEntry { pubkeys: pk, stake });
            }
            let json = run_genesis_init(&network, timestamp, &seed, &entries)?;
            if let Some(path) = out {
                fs::write(&path, &json).map_err(|e| CliError::Client(e.to_string()))?;
                eprintln!("wrote {}", path.display());
            } else {
                println!("{json}");
            }
        }
        Cmd::Genesis {
            sub: GenesisCmd::Validate { file },
        } => {
            let json = fs::read_to_string(&file).map_err(|e| CliError::Client(e.to_string()))?;
            let g = run_genesis_validate(&json)?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "ok": true,
                        "chain_id": g.chain_spec.chain_id,
                        "validators": g.validators.len(),
                    })
                );
            } else {
                println!(
                    "ok — {} validators on chain {}",
                    g.validators.len(),
                    g.chain_spec.chain_id
                );
            }
        }
    }
    Ok(())
}

fn release_err(e: &ReleaseError) -> CliError {
    CliError::Client(format!("release: {e}"))
}

#[allow(clippy::too_many_lines)]
fn handle_release_cmd(sub: ReleaseCmd, json_out: bool) -> Result<(), CliError> {
    match sub {
        ReleaseCmd::Keygen { out } => {
            let mut rng = OsRng;
            let sk = Ed25519SecretKey::generate(&mut rng);
            let pk = sk.public();
            let secret_hex = sk.to_hex();
            let pubkey_hex = pk.to_hex();
            if json_out {
                println!(
                    "{}",
                    serde_json::json!({
                        "ed25519_secret_hex": secret_hex,
                        "ed25519_pubkey_hex": pubkey_hex,
                    })
                );
            } else if let Some(path) = out.as_ref() {
                fs::write(path, &secret_hex).map_err(|e| CliError::Client(e.to_string()))?;
                eprintln!("wrote secret seed to {}", path.display());
                println!("{pubkey_hex}");
            } else {
                println!("ed25519_secret_hex: {secret_hex}");
                println!("ed25519_pubkey_hex: {pubkey_hex}");
            }
        }
        ReleaseCmd::Sign {
            binary,
            version,
            secret,
            secret_file,
            timestamp,
            out,
        } => {
            let secret_hex = match (secret, secret_file) {
                (Some(s), None) => s,
                (None, Some(f)) => fs::read_to_string(&f)
                    .map_err(|e| CliError::Client(e.to_string()))?
                    .trim()
                    .to_string(),
                (None, None) => {
                    return Err(CliError::Client("need --secret or --secret-file".into()))
                }
                (Some(_), Some(_)) => {
                    return Err(CliError::Client(
                        "--secret and --secret-file are mutually exclusive".into(),
                    ))
                }
            };
            let sk = Ed25519SecretKey::from_hex(&secret_hex)
                .map_err(|e| CliError::Client(format!("secret: {e}")))?;
            let ts = timestamp.unwrap_or_else(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            });
            let manifest = sign_release(&sk, &binary, &version, ts).map_err(|e| release_err(&e))?;
            let manifest_json = serde_json::to_string_pretty(&manifest)?;
            if let Some(path) = out.as_ref() {
                fs::write(path, &manifest_json).map_err(|e| CliError::Client(e.to_string()))?;
                eprintln!("wrote manifest to {}", path.display());
            } else {
                println!("{manifest_json}");
            }
        }
        ReleaseCmd::Verify {
            manifest,
            binary,
            pubkey,
        } => {
            let pk = Ed25519PublicKey::from_hex(&pubkey)
                .map_err(|e| CliError::Client(format!("pubkey: {e}")))?;
            let manifest_json =
                fs::read_to_string(&manifest).map_err(|e| CliError::Client(e.to_string()))?;
            let m: aii_cli::release::ReleaseManifest = serde_json::from_str(&manifest_json)?;
            verify_release(&pk, &m, &binary).map_err(|e| release_err(&e))?;
            if json_out {
                println!(
                    "{}",
                    serde_json::json!({
                        "ok": true,
                        "version": m.version,
                        "timestamp_unix": m.timestamp_unix,
                    })
                );
            } else {
                println!("ok — {} signed at {}", m.version, m.timestamp_unix);
            }
        }
    }
    Ok(())
}
