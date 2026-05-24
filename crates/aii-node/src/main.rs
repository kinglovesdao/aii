//! aiid — the AII node binary.

use aii_config::ChainSpec;
use aii_node::NodeState;
use aii_storage::RocksDbBackend;
use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "aiid",
    version,
    about = "AII node — chain bootstrap + RPC server (v0.0.7 scaffold)"
)]
struct Cli {
    /// Data directory (used for RocksDB storage). Created if it does not exist.
    #[arg(long, default_value = "/tmp/aiid")]
    data_dir: PathBuf,

    /// RPC bind address.
    #[arg(long, default_value = "127.0.0.1:8545")]
    rpc: SocketAddr,

    /// Use the testnet chain spec instead of mainnet.
    #[arg(long)]
    testnet: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .try_init();

    let cli = Cli::parse();
    let spec = if cli.testnet {
        ChainSpec::testnet()
    } else {
        ChainSpec::mainnet()
    };

    tracing::info!(
        data_dir = ?cli.data_dir,
        rpc = %cli.rpc,
        chain_id = spec.chain_id,
        network = %spec.network,
        "starting aiid"
    );

    // Open the persistent backend so the data-dir is initialised. We
    // don't read or write here yet — that wires up alongside consensus
    // in a later release.
    std::fs::create_dir_all(&cli.data_dir)?;
    let _backend: Arc<RocksDbBackend> = Arc::new(RocksDbBackend::open(&cli.data_dir)?);

    let state = NodeState::new(spec);
    let (bound, handle) = aii_rpc::serve(cli.rpc, state).await?;
    tracing::info!(addr = %bound, "rpc server listening");

    // Wait for SIGINT / SIGTERM, then stop the RPC server.
    tokio::signal::ctrl_c().await?;
    tracing::info!("ctrl-c received, stopping");
    handle.stop()?;
    handle.stopped().await;
    Ok(())
}
