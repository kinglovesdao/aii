//! `aii` — user-facing CLI for the AII protocol.

use aii_cli::{
    run_account_from_mnemonic, run_account_mnemonic, run_account_new, run_account_new_encrypted,
    run_account_verify, run_chain_id, run_genesis_init, run_genesis_validate, run_get_block_header,
    run_random_seed_hex, run_recent_blocks, run_status, run_stress, run_tier, run_validator_keygen,
    run_validator_pubkey, CliError, ValidatorEntry, ValidatorPubkeys,
};
use clap::{Parser, Subcommand};
use std::fs;
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
