//! `aii` — user-facing CLI for the AII protocol.

use aii_cli::{
    run_account_from_mnemonic, run_account_mnemonic, run_account_new, run_account_new_encrypted,
    run_account_verify, run_chain_id, run_status, run_tier, CliError,
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
    }
    Ok(())
}
