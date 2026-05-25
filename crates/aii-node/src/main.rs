//! aiid — the AII node binary.

use aii_block::{Block, BlockBody, Bloom, Header, EMPTY_LIST_HASH, EMPTY_TRIE_HASH};
use aii_config::ChainSpec;
use aii_consensus_bft::{BftGossip, DevModeEngine, EngineConfig};
use aii_consensus_iface::ConsensusKind;
use aii_consensus_poa::{PoaConfig, PoaEngine};
use aii_node::bft_p2p::TcpBftTransport;
use aii_node::{bft_bootstrap, NodeState};
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

    /// Run with the real BFT-PoS engine instead of the dev-mode producer.
    /// Requires `--genesis` and `--keystore`. In single-validator mode
    /// the node produces a fresh block every `--slot-seconds`; in
    /// multi-validator mode the node waits for peer events (network
    /// transport lands in v0.0.34+).
    #[arg(long)]
    bft: bool,

    /// Path to a genesis JSON file (produced by `aii genesis init`).
    /// Required when `--bft` is set.
    #[arg(long)]
    genesis: Option<PathBuf>,

    /// Path to a validator keystore JSON (produced by `aii validator keygen`).
    /// Required when `--bft` is set.
    #[arg(long)]
    keystore: Option<PathBuf>,

    /// Block-producing coinbase address (hex, with or without `0x`).
    /// Defaults to all-zero.
    #[arg(long)]
    coinbase: Option<String>,

    /// TCP address to bind the BFT gossip listener to. Defaults to
    /// `127.0.0.1:30311`. Only used when `--bft` is set.
    #[arg(long, default_value = "127.0.0.1:30311")]
    bft_listen: SocketAddr,

    /// Comma-separated list of `host:port` addresses of peer BFT
    /// nodes. Each entry will be dialed on startup and reconnect on
    /// failure. Example: `--peers 10.0.0.2:30311,10.0.0.3:30311`.
    #[arg(long, value_delimiter = ',')]
    peers: Vec<SocketAddr>,

    /// Consensus algorithm to run for the main chain. `bft` (default)
    /// uses VRF-PoS + BLS finality; `poa` uses a fixed authority list
    /// in round-robin order. `--consensus poa` requires
    /// `--authorities`.
    #[arg(long, default_value = "bft")]
    consensus: String,

    /// Comma-separated PoA authority addresses (hex, with or without
    /// `0x`). Order matters: `authorities[height % N]` is the slot
    /// proposer for height N. Required when `--consensus poa`.
    #[arg(long, value_delimiter = ',')]
    authorities: Vec<String>,
}

fn parse_address(s: &str) -> Result<Address, Box<dyn std::error::Error + Send + Sync>> {
    let s = s.trim_start_matches("0x");
    let raw = hex::decode(s).map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
        format!("coinbase: bad hex: {e}").into()
    })?;
    let arr: [u8; 20] =
        raw.try_into()
            .map_err(|v: Vec<u8>| -> Box<dyn std::error::Error + Send + Sync> {
                format!("coinbase: expected 20 bytes, got {}", v.len()).into()
            })?;
    Ok(Address::new(arr))
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
#[allow(clippy::too_many_lines)]
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
    let consensus_kind = ConsensusKind::parse(&cli.consensus).map_err(
        |s| -> Box<dyn std::error::Error + Send + Sync> {
            format!("--consensus: unknown algorithm '{s}' (expected: bft, poa)").into()
        },
    )?;

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

    // Production path: real BFT engine driven by genesis + keystore.
    let producer_handle =
        if cli.bft {
            let genesis_path = cli.genesis.as_ref().ok_or_else(
                || -> Box<dyn std::error::Error + Send + Sync> {
                    "--bft requires --genesis FILE".into()
                },
            )?;
            let keystore_path = cli.keystore.as_ref().ok_or_else(
                || -> Box<dyn std::error::Error + Send + Sync> {
                    "--bft requires --keystore FILE".into()
                },
            )?;
            let coinbase = cli
                .coinbase
                .as_deref()
                .map(parse_address)
                .transpose()?
                .unwrap_or(Address::ZERO);
            let (engine, genesis) =
                bft_bootstrap::boot_bft_engine(genesis_path, keystore_path, coinbase)?;
            let engine = Arc::new(engine);
            let is_single = engine.is_single_validator();
            tracing::info!(
                single_validator = is_single,
                validators = genesis.validators.len(),
                coinbase = ?coinbase,
                "BftEngine ready"
            );
            let state_for_loop = node_state.clone();
            let interval = Duration::from_secs(cli.slot_seconds);
            if is_single {
                let engine_for_loop = engine.clone();
                Some(tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(interval).await;
                        match engine_for_loop.advance_single() {
                            Ok(out) => {
                                state_for_loop.set_head(out.block.header.number);
                                tracing::info!(
                                    number = out.block.header.number,
                                    hash = ?out.block_hash,
                                    round = out.certificate.round,
                                    "BFT block finalised"
                                );
                            }
                            Err(e) => {
                                tracing::error!(?e, "BFT advance failed");
                                break;
                            }
                        }
                    }
                }))
            } else {
                // Multi-validator: stand up the TCP gossip transport,
                // then loop driving the gossip + harvest pair.
                let transport =
                    Arc::new(TcpBftTransport::new(cli.bft_listen, cli.peers.clone()).await?);
                tracing::info!(
                    listen = %transport.local_addr(),
                    peers = ?cli.peers,
                    "BFT gossip transport listening"
                );
                let gossip = Arc::new(BftGossip::new(engine.clone(), transport));
                let engine_for_loop = engine.clone();
                Some(tokio::spawn(async move {
                    loop {
                        gossip.tick();
                        if let Some(n) = engine_for_loop.try_harvest_committed() {
                            state_for_loop.set_head(n);
                            tracing::info!(number = n, "BFT block finalised (multi)");
                        }
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }))
            }
        } else if consensus_kind == ConsensusKind::Poa {
            // Proof-of-Authority main chain.
            if cli.authorities.is_empty() {
                return Err::<_, Box<dyn std::error::Error + Send + Sync>>(
                    "--consensus poa requires --authorities ADDR1,ADDR2,…".into(),
                );
            }
            let authorities: Vec<Address> = cli
                .authorities
                .iter()
                .map(|s| parse_address(s))
                .collect::<Result<Vec<_>, _>>()?;
            let coinbase = cli
                .coinbase
                .as_deref()
                .map(parse_address)
                .transpose()?
                .unwrap_or_else(|| authorities[0]);
            let poa_cfg = PoaConfig {
                authorities: authorities.clone(),
                coinbase,
                slot_seconds: cli.slot_seconds,
                gas_limit: spec.initial_gas_limit,
                base_fee_per_gas: U256::from(spec.min_base_fee_per_gas),
            };
            let genesis = genesis_block(&spec);
            let engine = PoaEngine::new(poa_cfg, &genesis)?;
            tracing::info!(
                authorities = authorities.len(),
                coinbase = ?coinbase,
                "PoA engine ready"
            );
            let state_for_loop = node_state.clone();
            let interval = Duration::from_secs(cli.slot_seconds);
            Some(tokio::spawn(async move {
                loop {
                    tokio::time::sleep(interval).await;
                    if engine.is_my_turn() {
                        match engine.produce_block() {
                            Ok((hash, number, _block)) => {
                                state_for_loop.set_head(number);
                                tracing::info!(number, ?hash, "PoA block produced");
                            }
                            Err(e) => {
                                tracing::error!(?e, "PoA produce failed");
                                break;
                            }
                        }
                    } else {
                        tracing::trace!(
                            next_slot = engine.next_authority_index(),
                            "not my PoA slot — idle"
                        );
                    }
                }
            }))
        } else if cli.produce_blocks {
            // Legacy dev-mode producer.
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
