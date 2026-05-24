//! aiid — the AII node binary.

use aii_block::{Block, BlockBody, Bloom, Header, EMPTY_LIST_HASH, EMPTY_TRIE_HASH};
use aii_config::ChainSpec;
use aii_consensus_bft::{DevModeEngine, EngineConfig};
use aii_node::NodeState;
use aii_storage::RocksDbBackend;
use aii_types::{Address, H256, U256};
use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "aiid",
    version,
    about = "AII node — chain bootstrap + RPC server + dev-mode block producer (v0.0.15)"
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

    /// Run the BFT dev-mode block producer (one node, no peers, no votes).
    /// In v0.0.15 this is the only way to advance the head. Disable for
    /// pure RPC-server-only operation.
    #[arg(long, default_value = "true")]
    produce_blocks: bool,

    /// Block-production interval in seconds (when `--produce-blocks` is on).
    #[arg(long, default_value = "3")]
    slot_seconds: u64,
}

fn genesis_block(spec: &ChainSpec) -> Block {
    Block {
        header: Header {
            parent_hash: H256::ZERO,
            ommers_hash: EMPTY_LIST_HASH,
            beneficiary: Address::ZERO,
            state_root: EMPTY_TRIE_HASH,
            transactions_root: EMPTY_TRIE_HASH,
            receipts_root: EMPTY_TRIE_HASH,
            logs_bloom: Bloom::ZERO,
            difficulty: U256::ZERO,
            number: 0,
            gas_limit: spec.initial_gas_limit,
            gas_used: 0,
            timestamp: 1_700_000_000,
            extra_data: format!("aii-{}", spec.network).into_bytes(),
            mix_hash: H256::ZERO,
            nonce: [0u8; 8],
            base_fee_per_gas: U256::from(spec.min_base_fee_per_gas),
            withdrawals_root: EMPTY_TRIE_HASH,
            blob_gas_used: None,
            excess_blob_gas: None,
            parent_beacon_block_root: None,
        },
        body: BlockBody::default(),
    }
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
        produce_blocks = cli.produce_blocks,
        slot_seconds = cli.slot_seconds,
        "starting aiid"
    );

    std::fs::create_dir_all(&cli.data_dir)?;
    let _backend: Arc<RocksDbBackend> = Arc::new(RocksDbBackend::open(&cli.data_dir)?);

    let node_state = NodeState::new(spec.clone());

    // Wire up the dev-mode BFT producer if requested.
    let producer_handle = if cli.produce_blocks {
        let genesis = genesis_block(&spec);
        let engine_cfg = EngineConfig {
            slot_seconds: cli.slot_seconds,
            ..EngineConfig::default()
        };
        let engine = DevModeEngine::new(engine_cfg, &genesis);
        let state_for_loop = node_state.clone();
        let interval = Duration::from_secs(cli.slot_seconds);
        Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                match engine.produce_block() {
                    Ok((hash, number, _block)) => {
                        state_for_loop.set_head(number);
                        tracing::info!(number, ?hash, "block produced");
                    }
                    Err(e) => {
                        tracing::error!(?e, "block production failed");
                        break;
                    }
                }
            }
        }))
    } else {
        None
    };

    let (bound, handle) = aii_rpc::serve(cli.rpc, node_state).await?;
    tracing::info!(addr = %bound, "rpc server listening");

    tokio::signal::ctrl_c().await?;
    tracing::info!("ctrl-c received, stopping");
    handle.stop()?;
    handle.stopped().await;
    if let Some(h) = producer_handle {
        h.abort();
    }
    Ok(())
}
