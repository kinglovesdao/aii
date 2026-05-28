//! aiid — the AII node binary.

use aii_block::{Block, BlockBody, Bloom, Header, EMPTY_LIST_HASH, EMPTY_TRIE_HASH};
use aii_config::{ChainSpec, Genesis};
use aii_consensus_bft::{BftGossip, DevModeEngine, EngineConfig};
use aii_consensus_iface::ConsensusKind;
use aii_consensus_poa::{PoaConfig, PoaEngine};
use aii_node::bft_p2p::TcpBftTransport;
use aii_node::{bft_bootstrap, NodeState};
use aii_storage::{KvBackend, RocksDbBackend};
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
#[allow(clippy::struct_excessive_bools)]
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
    /// In v0.0.15 this is the only way to advance the head. Disable with
    /// `--no-produce-blocks` for pure RPC / observer operation.
    #[arg(
        long,
        default_value_t = true,
        action = clap::ArgAction::Set,
        num_args = 0..=1,
        default_missing_value = "true",
    )]
    produce_blocks: bool,

    /// Run the node as a follow-only observer: every `follow_seconds`
    /// it calls `aii_getRawBlock` on the bootnode and applies every
    /// new block locally. Requires `--bootnode`. Implies
    /// `--no-produce-blocks` so the local node never forks the chain.
    /// `0` disables (default).
    #[arg(long, default_value = "0")]
    follow_seconds: u64,

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

    /// Bootstrap RPC URL of an already-synced node. When set, on
    /// startup the local node walks blocks from `local_head + 1` to
    /// the peer's tip, fetching each via `aii_getRawBlock`, and
    /// commits them into the local backend before opening RPC.
    /// Skipped if the peer is at or behind the local head.
    #[arg(long)]
    bootnode: Option<String>,

    /// Wrap the BFT gossip socket in a Noise XX handshake +
    /// ChaCha20-Poly1305 AEAD transport. All validators in a peer
    /// set must agree on this flag — mixing encrypted + plaintext
    /// peers fails the handshake.
    #[arg(long, default_value = "false")]
    encrypt_gossip: bool,

    /// BTC-style outbound-only BFT (v0.0.68). When set, the BFT
    /// listener is bound to a random loopback port and only the
    /// outbound TCP sockets to `--peers` carry consensus traffic.
    /// Use this on validators that sit behind a home NAT or HTTP
    /// proxy chain (Mihomo / Clash, Cloudflare WARP, corporate VPN)
    /// where the public 30311 port is not reachable from the rest
    /// of the validator set. Implies `--bft-listen 127.0.0.1:0`.
    #[arg(long, default_value = "false")]
    bft_outbound_only: bool,

    /// Comma-separated HTTP-RPC URLs of peer nodes used for
    /// v0.0.77 release-manifest gossip + binary auto-fetch.
    /// Example: `--update-peers http://node-b:8545,http://node-c:8545`.
    /// Empty disables outbound propagation (the node still accepts
    /// announcements but never re-broadcasts).
    #[arg(long, default_value = "", value_parser)]
    update_peers: String,

    /// v0.0.78 opt-in: when set, accepting a signed release
    /// manifest whose binary is already cached at
    /// `<data-dir>/releases/<version>` triggers an atomic install
    /// over the running `aiid` plus an `execve` self-restart.
    /// Default `false` — in-place restarts are disruptive and
    /// most operators want to schedule the swap manually via
    /// `aii_installRelease` once they've reviewed the manifest.
    #[arg(long, default_value = "false")]
    auto_install_releases: bool,

    /// v0.0.81 late-joiner re-poll: every N seconds, query each
    /// `--update-peers` URL's `aii_latestRelease` and catch up
    /// (verify signature, record manifest, pull binary) if a
    /// peer is ahead. Default 60 s. Set to 0 to disable. Has
    /// no effect when `--update-peers` is empty.
    #[arg(long, default_value = "60")]
    release_poll_secs: u64,

    /// v0.0.84 runtime head-stall watchdog: when the head block
    /// number does not advance for this many seconds, the node
    /// calls `exec_self()` for a kernel-level same-PID restart.
    /// The restarted process then cold-syncs via the v0.0.83
    /// implicit-bootnode fallback (`--update-peers[0]`) and
    /// rejoins consensus. Default 0 (disabled). Recommend a
    /// value at least 5× the BFT slot interval so single-slot
    /// hiccups don't trigger restarts.
    #[arg(long, default_value = "0")]
    stall_recover_secs: u64,

    /// v0.0.84 head-watchdog poll cadence. The watchdog wakes
    /// every N seconds to read the head; finer granularity
    /// means faster stall detection but more wakeups. Default
    /// 10 s. Ignored when `--stall-recover-secs` is 0.
    #[arg(long, default_value = "10")]
    stall_poll_secs: u64,

    /// v0.0.85 boot-health confirm window. After an
    /// auto-install, the new process image waits this many
    /// seconds and then checks whether the head advanced past
    /// the pre-install head. If yes, the `.boot-pending`
    /// sentinel is cleared. If no, `aii_rollbackRelease` fires
    /// automatically. Default 0 (disabled). Recommend at
    /// least 5× the BFT slot interval so a slow first round
    /// after restart doesn't trigger an unnecessary rollback.
    #[arg(long, default_value = "0")]
    boot_health_secs: u64,
}

/// Resolve the effective bootnode URL for cold-sync and follow-loop
/// purposes (v0.0.83).
///
/// Precedence:
/// 1. Explicit `--bootnode URL`.
/// 2. First entry of `--update-peers` (already parsed to HTTP URLs).
/// 3. `None` — no implicit fallback available; bootstrap-sync skips.
///
/// Reusing `--update-peers[0]` means an operator who configured the
/// v0.0.77 release-gossip peer list automatically gets the v0.0.69
/// cold-sync recovery path on every restart, without needing to
/// also pass `--bootnode`. Solves the post-deploy BFT stall pattern
/// observed in v0.0.82 production rollout.
#[must_use]
fn effective_bootnode(explicit: Option<&str>, update_peers: &[String]) -> Option<String> {
    if let Some(b) = explicit {
        return Some(b.to_string());
    }
    update_peers.first().cloned()
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

/// Materialise every [`aii_config::GenesisAlloc`] entry from `genesis`
/// into the node's world-state.
///
/// Called once at startup (after the engine has been constructed,
/// before block production begins). Without this step the alloc lives
/// only in the genesis JSON — RPC `eth_getBalance` would return zero
/// for every pre-funded account and `eth_sendRawTransaction` would
/// accept signed transfers that the producer cannot actually execute
/// against any balance.
fn apply_genesis_alloc(
    node_state: &NodeState,
    genesis: &Genesis,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let state = node_state.state();
    for entry in &genesis.alloc {
        state
            .set_account(&entry.address, &entry.to_account())
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                format!(
                    "apply genesis alloc for 0x{}: {e}",
                    hex::encode(entry.address.as_bytes())
                )
                .into()
            })?;
    }
    if !genesis.alloc.is_empty() {
        tracing::info!(
            allocated_accounts = genesis.alloc.len(),
            "applied genesis allocation to world-state",
        );
    }
    Ok(())
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
    let backend: Arc<RocksDbBackend> = Arc::new(RocksDbBackend::open(&cli.data_dir)?);

    // If the data dir already has a head marker, replay the indexed
    // chain off disk; otherwise stand up a fresh in-memory cache on
    // top of the (possibly empty) backend.
    let has_existing = backend
        .get(aii_storage::ColumnFamily::Meta, b"head_block_number")?
        .is_some();
    let node_state = if has_existing {
        let s = NodeState::recover(spec.clone(), Arc::clone(&backend))?;
        tracing::info!(
            recovered_head = s.head_block_number_sync(),
            blocks = s.block_count(),
            "recovered persisted chain from data_dir",
        );
        s
    } else {
        NodeState::new(spec.clone(), Arc::clone(&backend))
    };
    // v0.0.76: tell the state where to put release-binary cache files.
    node_state.set_data_dir(cli.data_dir.clone());
    // v0.0.77: populate the release-gossip peer list.
    let update_peers = aii_rpc::release_gossip::parse_update_peers(&cli.update_peers);
    if !update_peers.is_empty() {
        tracing::info!(peers = ?update_peers, "release-gossip peers configured");
    }
    node_state.set_update_peers(update_peers.clone());

    // v0.0.78: opt-in atomic install + execve self-restart on
    // release accept. Off by default.
    node_state.set_auto_install_releases(cli.auto_install_releases);
    if cli.auto_install_releases {
        tracing::info!("auto-install of accepted releases is ENABLED");
    }

    // v0.0.83: implicit bootnode fallback. When `--bootnode` is not
    // set but `--update-peers` is, use the first update-peer URL as
    // the cold-sync source. This addresses the post-restart BFT
    // stall observed during the v0.0.82 production deploy: any node
    // that fell behind by even 1 block during a 1-second restart
    // window could not catch up via BFT alone (the engine doesn't
    // re-propose finalised blocks), so a `bootstrap_sync_from_peer`
    // call was required — but it only ran when `--bootnode` was
    // explicitly configured. Any operator who configured
    // `--update-peers` for the v0.0.77 gossip flow already has the
    // right peer URLs handy; reusing them as the implicit
    // bootnode/follow source means an explicit `--bootnode` is
    // only ever needed for asymmetric topologies (bootnode != peer).
    let effective_bootnode = effective_bootnode(cli.bootnode.as_deref(), &update_peers);
    if cli.bootnode.is_none() {
        if let Some(implicit) = effective_bootnode.as_deref() {
            tracing::info!(
                bootnode = %implicit,
                source = "update-peers[0]",
                "no --bootnode set; using first --update-peers URL as implicit cold-sync source (v0.0.83)",
            );
        }
    }

    // Cold-join sync: catch up to the bootnode's head before opening
    // RPC. Skips when no bootnode is set (explicit or implicit) or
    // when the peer is at/below our local head. Each fetched block
    // goes through the same commit_block path as a freshly produced
    // one — so state mutations (including subsidy minting) run as
    // part of catch-up.
    if let Some(boot_url) = effective_bootnode.as_deref() {
        match aii_node::bootstrap_sync_from_peer(&node_state, boot_url).await {
            Ok(synced) => tracing::info!(
                head = node_state.head_block_number_sync(),
                blocks_added = synced,
                "bootstrap sync complete",
            ),
            Err(e) => {
                tracing::error!(error = %e, "bootstrap sync failed — continuing with local head");
            }
        }
    }

    // Production path: real BFT engine driven by genesis + keystore.
    let producer_handle = if cli.bft {
        let genesis_path =
            cli.genesis
                .as_ref()
                .ok_or_else(|| -> Box<dyn std::error::Error + Send + Sync> {
                    "--bft requires --genesis FILE".into()
                })?;
        let keystore_path =
            cli.keystore
                .as_ref()
                .ok_or_else(|| -> Box<dyn std::error::Error + Send + Sync> {
                    "--bft requires --keystore FILE".into()
                })?;
        let coinbase = cli
            .coinbase
            .as_deref()
            .map(parse_address)
            .transpose()?
            .unwrap_or(Address::ZERO);
        // v0.0.70: if persistent storage already has blocks, resume
        // the BFT engine at recovered_head + 1 rather than re-starting
        // from genesis (which silently overwrote the existing chain
        // in v0.0.67–v0.0.69).
        let recovered_head_block = node_state.head_block();
        let recovered_head_number = recovered_head_block.as_ref().map_or(0, |b| b.header.number);
        let (engine, genesis) = if let Some(ref head_block) = recovered_head_block {
            if head_block.header.number > 0 {
                tracing::info!(
                    resume_height = head_block.header.number + 1,
                    "resuming BFT engine from recovered head"
                );
                bft_bootstrap::boot_bft_engine_with_recovered_head(
                    genesis_path,
                    keystore_path,
                    coinbase,
                    head_block,
                )?
            } else {
                bft_bootstrap::boot_bft_engine(genesis_path, keystore_path, coinbase)?
            }
        } else {
            bft_bootstrap::boot_bft_engine(genesis_path, keystore_path, coinbase)?
        };
        apply_genesis_alloc(&node_state, &genesis)?;
        let engine = Arc::new(engine);
        let is_single = engine.is_single_validator();
        tracing::info!(
            single_validator = is_single,
            validators = genesis.validators.len(),
            coinbase = ?coinbase,
            recovered_head = recovered_head_number,
            "BftEngine ready"
        );
        let state_for_loop = node_state.clone();
        let interval = Duration::from_secs(cli.slot_seconds);
        if is_single {
            let engine_for_loop = engine.clone();
            let state_for_pool = node_state.clone();
            let max_txs_per_block =
                (spec.initial_gas_limit / aii_consensus_bft::PLACEHOLDER_TX_GAS) as usize;
            Some(tokio::spawn(async move {
                loop {
                    tokio::time::sleep(interval).await;
                    // Pull a batch of txs from the mempool, stage them on
                    // the engine for inclusion in the next block.
                    let txs = state_for_pool.tx_pool().drain_up_to(max_txs_per_block);
                    let tx_count = txs.len();
                    engine_for_loop.set_pending_txs(txs);
                    match engine_for_loop.advance_single() {
                        Ok(out) => {
                            state_for_loop.commit_block(&out.block);
                            state_for_loop.set_head(out.block.header.number);
                            tracing::info!(
                                number = out.block.header.number,
                                hash = ?out.block_hash,
                                round = out.certificate.round,
                                txs = tx_count,
                                gas_used = out.block.header.gas_used,
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
            //
            // v0.0.69: merge the persistent peer cache with `--peers`
            // so a restart re-dials previously-known validators
            // without operator config. Cache lives at
            // `<data-dir>/peers.json` and is updated by the dialer
            // (best-effort; not on the critical path).
            let cache_path = aii_node::peer_cache::cache_path(&cli.data_dir);
            let cached_peers = aii_node::peer_cache::load(&cache_path).unwrap_or_default();
            let merged_peers = aii_node::peer_cache::merge(&cached_peers, &cli.peers);
            if !cached_peers.is_empty() {
                tracing::info!(
                    cached = cached_peers.len(),
                    cli_peers = cli.peers.len(),
                    merged = merged_peers.len(),
                    path = %cache_path.display(),
                    "loaded persistent peer cache",
                );
            }
            // Persist the merged set so subsequent restarts pick it
            // up even before the dialer succeeds against any single
            // peer. Errors are logged but not fatal.
            if let Err(e) = aii_node::peer_cache::save(&cache_path, &merged_peers) {
                tracing::warn!(?e, path = %cache_path.display(), "peer cache save failed");
            }
            let transport = Arc::new(match (cli.bft_outbound_only, cli.encrypt_gossip) {
                (true, true) => {
                    TcpBftTransport::new_outbound_only_encrypted(merged_peers.clone()).await?
                }
                (true, false) => TcpBftTransport::new_outbound_only(merged_peers.clone()).await?,
                (false, true) => {
                    TcpBftTransport::new_encrypted(cli.bft_listen, merged_peers.clone()).await?
                }
                (false, false) => {
                    TcpBftTransport::new(cli.bft_listen, merged_peers.clone()).await?
                }
            });
            tracing::info!(
                listen = %transport.local_addr(),
                peers = ?merged_peers,
                outbound_only = cli.bft_outbound_only,
                "BFT gossip transport listening"
            );
            // v0.0.71: restore in-flight BFT round state. If the
            // local node previously persisted a `(height, round)`
            // snapshot AND that snapshot is for the height we're
            // about to coordinate (recovered_head + 1), fast-forward
            // the coordinator to the saved round. This closes the
            // "single-validator restart freezes consensus" gap: the
            // restarted node lands at the same round as live peers,
            // their votes combine for quorum immediately.
            let bft_state_path = aii_node::bft_state::state_path(&cli.data_dir);
            if let Ok(Some(snap)) = aii_node::bft_state::load(&bft_state_path) {
                if snap.height == recovered_head_number + 1 && snap.round > 0 {
                    if let Err(e) = engine.fast_forward_to_round(snap.round) {
                        tracing::warn!(
                            ?e,
                            height = snap.height,
                            target_round = snap.round,
                            "BFT fast-forward to persisted round failed"
                        );
                    } else {
                        tracing::info!(
                            height = snap.height,
                            round = snap.round,
                            "restored BFT coordinator from persisted round state"
                        );
                    }
                }
            }
            let gossip = Arc::new(BftGossip::new(engine.clone(), transport));
            let engine_for_loop = engine.clone();
            let state_for_pool = node_state.clone();
            let max_txs_per_block =
                (spec.initial_gas_limit / aii_consensus_bft::PLACEHOLDER_TX_GAS) as usize;
            // Round-state persistence cadence — only write when the
            // tracked tuple actually changes.
            let bft_state_path_for_loop = bft_state_path;
            let last_persisted_height = Arc::new(std::sync::atomic::AtomicU64::new(0));
            let last_persisted_round = Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX));
            // Drain the mempool into the engine's pending-txs queue
            // every tick. `extend_pending_txs` appends without
            // overwriting anything the proposer has not yet packed
            // — overwriting would silently drop txs between slots.
            // The leader's next `cast_proposal` packs whatever has
            // accumulated and gossips the body to peers (since
            // v0.0.39).
            Some(tokio::spawn(async move {
                loop {
                    gossip.tick();
                    let txs = state_for_pool.tx_pool().drain_up_to(max_txs_per_block);
                    if !txs.is_empty() {
                        tracing::debug!(count = txs.len(), "staging mempool txs onto BFT engine",);
                        engine_for_loop.extend_pending_txs(txs);
                    }
                    // Drain any equivocation evidence the BFT detector
                    // surfaced from peer votes and auto-persist via
                    // `record_slashing`. Stake debit needs a
                    // validator-index → stake-address map (not yet
                    // in `GenesisValidator`); auto-debit lands once
                    // DPoS rotation publishes that mapping per epoch.
                    for ev in engine_for_loop.drain_evidence() {
                        state_for_loop.record_slashing(&ev);
                        tracing::warn!(
                            validator = ev.validator_index(),
                            height = ev.height(),
                            "equivocation evidence persisted via auto-trigger",
                        );
                    }
                    // v0.0.73: gossip auto-harvests committed blocks
                    // between inbox messages so the engine head stays
                    // in lockstep with the inbox. Drain them here for
                    // application to world-state + the post-commit
                    // round-state snapshot. Belt-and-braces:
                    // also call engine.try_harvest_committed() in
                    // case a path outside gossip.tick produced a
                    // commit (single-validator mode shares this loop
                    // in some configurations).
                    let mut harvested = gossip.drain_harvested();
                    if let Some(block) = engine_for_loop.try_harvest_committed() {
                        harvested.push(block);
                    }
                    for block in harvested {
                        let n = block.header.number;
                        let tx_count = block.body.transactions.len();
                        let gas_used = block.header.gas_used;
                        state_for_loop.commit_block(&block);
                        state_for_loop.set_head(n);
                        tracing::info!(
                            number = n,
                            txs = tx_count,
                            gas_used,
                            "BFT block finalised (multi)",
                        );
                        // v0.0.71: on every committed block reset the
                        // round snapshot to "(N+1, 0)" — the next
                        // coordinator will be created at round 0 of
                        // height N+1. Persist now so a crash before
                        // any round timeout has fired still recovers
                        // at the right height.
                        let snap = aii_node::bft_state::BftStateSnapshot::new(n + 1, 0);
                        if let Err(e) = aii_node::bft_state::save(&bft_state_path_for_loop, snap) {
                            tracing::warn!(?e, "bft_state.json save (post-commit) failed");
                        }
                    }
                    // v0.0.71: persist the active round whenever it
                    // changes so a single-validator restart can
                    // fast-forward back to it.
                    if let Some((height, round, _phase)) = engine_for_loop.current_round_state() {
                        let now = (height, round);
                        if last_persisted_round.load(std::sync::atomic::Ordering::Relaxed)
                            != round as u64
                            || last_persisted_height.load(std::sync::atomic::Ordering::Relaxed)
                                != height
                        {
                            let snap = aii_node::bft_state::BftStateSnapshot::new(now.0, now.1);
                            if let Err(e) =
                                aii_node::bft_state::save(&bft_state_path_for_loop, snap)
                            {
                                tracing::warn!(?e, "bft_state.json save (round change) failed");
                            } else {
                                last_persisted_height
                                    .store(height, std::sync::atomic::Ordering::Relaxed);
                                last_persisted_round
                                    .store(round as u64, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
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
            // PoA seal signing (v0.0.45) is opt-in: when the operator
            // wants signed blocks they must supply a 32-byte hex
            // signer key via PoaConfig directly; the binary doesn't
            // yet expose a CLI flag for it because the encrypted
            // keystore + loader pair is the actual delivery vehicle.
            signer_sk: None,
        };
        let genesis = genesis_block(&spec);
        let engine = PoaEngine::new(poa_cfg, &genesis)?;
        tracing::info!(
            authorities = authorities.len(),
            coinbase = ?coinbase,
            "PoA engine ready"
        );
        let state_for_loop = node_state.clone();
        let state_for_pool = node_state.clone();
        let interval = Duration::from_secs(cli.slot_seconds);
        let max_txs_per_block =
            (spec.initial_gas_limit / aii_consensus_poa::PLACEHOLDER_TX_GAS) as usize;
        Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                if engine.is_my_turn() {
                    let txs = state_for_pool.tx_pool().drain_up_to(max_txs_per_block);
                    let tx_count = txs.len();
                    engine.set_pending_txs(txs);
                    match engine.produce_block() {
                        Ok((hash, number, block)) => {
                            state_for_loop.commit_block(&block);
                            state_for_loop.set_head(number);
                            tracing::info!(
                                number,
                                ?hash,
                                txs = tx_count,
                                gas_used = block.header.gas_used,
                                "PoA block produced"
                            );
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

    // Optional follow loop — applies new blocks from the bootnode on
    // every tick. Skipped when no bootnode is configured or
    // follow_seconds is 0. Implicitly requires `--no-produce-blocks`
    // (otherwise the local DevMode/BFT producer would fork the chain
    // off the bootnode's head).
    let follow_handle = if cli.follow_seconds > 0 && effective_bootnode.is_some() {
        let url = effective_bootnode.clone().unwrap();
        let interval = Duration::from_secs(cli.follow_seconds);
        let state = Arc::clone(&node_state);
        Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                match aii_node::bootstrap_sync_from_peer(&state, &url).await {
                    Ok(n) if n > 0 => tracing::info!(
                        head = state.head_block_number_sync(),
                        blocks_added = n,
                        "follow tick: applied new blocks from bootnode",
                    ),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "follow tick: bootnode poll failed"),
                }
            }
        }))
    } else {
        None
    };

    // v0.0.81: spawn the late-joiner release poller. Only fires
    // when `--update-peers` is non-empty AND `--release-poll-secs`
    // is non-zero; both checks live inside `start_release_poller`
    // for a single source of truth.
    let poller_handle = {
        let peers = node_state.update_peers();
        if !peers.is_empty() && cli.release_poll_secs > 0 {
            tracing::info!(
                interval_secs = cli.release_poll_secs,
                peers = peers.len(),
                "release poller scheduled",
            );
            Some(aii_rpc::release_poller::start_release_poller(
                Arc::clone(&node_state),
                peers,
                std::time::Duration::from_secs(cli.release_poll_secs),
            ))
        } else {
            None
        }
    };

    // v0.0.84 runtime head watchdog. Off when stall_recover_secs == 0.
    // When armed, the watchdog calls release_install::exec_self()
    // on stall — the new process image cold-syncs via the v0.0.83
    // implicit-bootnode fallback and rejoins consensus.
    let watchdog_handle = aii_node::head_watchdog::start_head_watchdog(
        Arc::clone(&node_state),
        cli.stall_recover_secs,
        cli.stall_poll_secs,
    );

    // v0.0.85 boot-health confirm. Off when boot_health_secs == 0.
    // When armed, reads .boot-pending sentinel (written by
    // install_release before execve); after the grace window,
    // either clears the sentinel (head advanced) or triggers
    // rollback_release (head still stuck → bad new binary).
    let boot_health_handle = aii_node::head_watchdog::start_boot_health_confirm(
        Arc::clone(&node_state),
        cli.data_dir.join(aii_node::release_store::RELEASES_SUBDIR),
        cli.boot_health_secs,
    );

    let (bound, handle) = aii_rpc::serve(cli.rpc, node_state).await?;
    tracing::info!(addr = %bound, "rpc server listening");

    tokio::signal::ctrl_c().await?;
    tracing::info!("ctrl-c received, stopping");
    handle.stop()?;
    handle.stopped().await;
    if let Some(h) = producer_handle {
        h.abort();
    }
    if let Some(h) = follow_handle {
        h.abort();
    }
    if let Some(h) = poller_handle {
        h.abort();
    }
    watchdog_handle.abort();
    boot_health_handle.abort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::effective_bootnode;

    #[test]
    fn explicit_bootnode_wins_over_update_peers() {
        let peers = vec![
            "http://peer-a:8545".to_string(),
            "http://peer-b:8545".to_string(),
        ];
        let got = effective_bootnode(Some("http://explicit:8545"), &peers);
        assert_eq!(got.as_deref(), Some("http://explicit:8545"));
    }

    #[test]
    fn falls_back_to_first_update_peer_when_explicit_is_none() {
        let peers = vec![
            "http://peer-a:8545".to_string(),
            "http://peer-b:8545".to_string(),
        ];
        let got = effective_bootnode(None, &peers);
        assert_eq!(got.as_deref(), Some("http://peer-a:8545"));
    }

    #[test]
    fn returns_none_when_both_empty() {
        let got = effective_bootnode(None, &[]);
        assert_eq!(got, None);
    }

    #[test]
    fn explicit_wins_even_when_update_peers_is_empty() {
        let got = effective_bootnode(Some("http://explicit:8545"), &[]);
        assert_eq!(got.as_deref(), Some("http://explicit:8545"));
    }

    #[test]
    fn fallback_ignores_later_peers_when_first_exists() {
        // Even if peer-a is unreachable later, we still pick it
        // here — connectivity is bootstrap_sync_from_peer's
        // problem, not the selector's.
        let peers = vec![
            "http://peer-a:8545".to_string(),
            "http://peer-b:8545".to_string(),
        ];
        let got = effective_bootnode(None, &peers);
        assert_eq!(got.as_deref(), Some("http://peer-a:8545"));
    }
}
