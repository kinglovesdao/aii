//! `aii` — user-facing CLI for the AII protocol.

use aii_cli::{run_account_new, run_chain_id, run_status, run_tier, CliError};
use clap::{Parser, Subcommand};
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
