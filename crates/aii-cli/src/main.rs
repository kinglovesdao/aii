//! `aii` — user-facing CLI for the AII protocol.

use aii_cli::release::{sign_release, verify_release, ReleaseError};
use aii_cli::{
    run_account_from_key_file, run_account_from_mnemonic, run_account_mnemonic, run_account_new,
    run_account_new_encrypted, run_account_verify, run_bft_capacity, run_bft_pressure,
    run_chain_id, run_discovery_probe, run_fund_addresses, run_genesis_init, run_genesis_validate,
    run_get_block_header, run_live_transfer_load, run_random_seed_hex, run_recent_blocks,
    run_state_credit, run_status, run_stress, run_subchain, run_tier, run_validator_keygen,
    run_validator_pubkey, CliError, ValidatorEntry, ValidatorPubkeys,
    DEFAULT_DISCOVERY_PROBE_HTTP_BOOTNODES, DEFAULT_DISCOVERY_PROBE_SEEDS,
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
    /// Compute the deterministic BFT committee capacity budget.
    BftCapacity {
        /// Active DPoS/BFT validators in the voting committee.
        #[arg(long, default_value = "21")]
        validators: usize,
        /// Total online network nodes. Consensus fanout still uses only validators.
        #[arg(long)]
        network_nodes: Option<u64>,
        /// Proposal bytes to budget. Defaults to the wire codec maximum.
        #[arg(long)]
        proposal_bytes: Option<usize>,
        /// Finality target seconds. Defaults to the roadmap 30 s target.
        #[arg(long)]
        target_secs: Option<u64>,
    },
    /// Execute quorum vote/certificate pressure for the active BFT committee.
    BftPressure {
        /// Active DPoS/BFT validators in the voting committee.
        #[arg(long, default_value = "128")]
        validators: usize,
        /// Total online network nodes. Consensus fanout still uses only validators.
        #[arg(long)]
        network_nodes: Option<u64>,
        /// Heights to measure.
        #[arg(long, default_value = "1")]
        heights: u64,
        /// Finality target seconds. Defaults to the roadmap 30 s target.
        #[arg(long)]
        target_secs: Option<u64>,
    },
    /// Probe Discovery v4 seeds and report discovered peers/public endpoint.
    DiscoveryProbe {
        /// UDP Discovery v4 seed specs. DNS names and IP host:port are accepted.
        #[arg(long, value_delimiter = ',')]
        seeds: Vec<String>,
        /// Temporary UDP bind address for the probe.
        #[arg(long, default_value = "0.0.0.0:0")]
        listen: std::net::SocketAddr,
        /// BFT listener advertised in the probe Ping's TCP port.
        #[arg(long, default_value = "0.0.0.0:30311")]
        bft_listen: std::net::SocketAddr,
        /// Milliseconds to wait for Discovery v4 replies.
        #[arg(long, default_value = "1500")]
        timeout_ms: u64,
        /// HTTP JSON-RPC bootnodes queried for `aii_peers` if UDP is filtered.
        #[arg(long = "http-bootnode", value_delimiter = ',')]
        http_bootnodes: Vec<String>,
        /// Disable HTTP `aii_peers` fallback and report UDP Discovery v4 only.
        #[arg(long, default_value = "false")]
        no_http_fallback: bool,
    },
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
    /// Submit real funded-account transfers on-chain.
    ///
    /// Each `--key-file` must contain one 32-byte secp256k1 private key
    /// as hex text. For the four-address test, pass exactly four files.
    LiveTransferLoad {
        /// Chain id of the target network (must match node's).
        #[arg(long, default_value = "9999")]
        chain_id: u64,
        /// Private-key hex file. Repeat four times for the full test.
        #[arg(long = "key-file", required = true)]
        key_files: Vec<std::path::PathBuf>,
        /// Total number of real transfers to submit.
        #[arg(long, default_value = "1000")]
        total: u64,
        /// Minimum transfer amount in AII.
        #[arg(long, default_value = "0.1")]
        min_aii: String,
        /// Maximum transfer amount in AII.
        #[arg(long, default_value = "50")]
        max_aii: String,
        /// Expected tx capacity per block, used for reporting only.
        #[arg(long, default_value = "100")]
        txs_per_block: u64,
        /// Seconds to wait after submission before reading final balances.
        #[arg(long, default_value = "10")]
        settle_sec: u64,
    },
    /// Fund test addresses from one real funded private key.
    FundAddresses {
        /// Chain id of the target network (must match node's).
        #[arg(long, default_value = "9999")]
        chain_id: u64,
        /// Funding private-key hex file.
        #[arg(long)]
        from_key_file: std::path::PathBuf,
        /// Recipient private-key file; the address is derived from it.
        #[arg(long = "to-key-file")]
        to_key_files: Vec<std::path::PathBuf>,
        /// Recipient address (`0x...`). Can be mixed with `--to-key-file`.
        #[arg(long = "to-address")]
        to_addresses: Vec<String>,
        /// Amount sent to each recipient.
        #[arg(long, default_value = "7000")]
        amount_aii: String,
        /// Seconds to wait after submission before reading final balance.
        #[arg(long, default_value = "20")]
        settle_sec: u64,
    },
    /// Directly credit accounts in a stopped node's local RocksDB state.
    StateCredit {
        /// Node data directory, e.g. `/var/lib/aiid/data`.
        #[arg(long)]
        data_dir: std::path::PathBuf,
        /// Recipient private-key file; the address is derived from it.
        #[arg(long = "to-key-file")]
        to_key_files: Vec<std::path::PathBuf>,
        /// Recipient address (`0x...`). Can be mixed with `--to-key-file`.
        #[arg(long = "to-address")]
        to_addresses: Vec<String>,
        /// Amount credited to each recipient.
        #[arg(long, default_value = "10000")]
        amount_aii: String,
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
    /// Verify a `release.json` against a binary. Defaults to the
    /// compiled-in pinned release-signing pubkey
    /// (`RELEASE_SIGNING_PUBKEY_HEX`); pass `--pubkey` to verify
    /// against a different key (testing, key rotation drills, etc.).
    Verify {
        /// Path to the release manifest JSON.
        #[arg(long)]
        manifest: std::path::PathBuf,
        /// Path to the binary the manifest claims to cover.
        #[arg(long)]
        binary: std::path::PathBuf,
        /// Hex-encoded 32-byte Ed25519 public key. Omit to use the
        /// pinned project release-signing pubkey.
        #[arg(long)]
        pubkey: Option<String>,
    },
    /// Restore the binary saved at `<data-dir>/releases/.previous`
    /// over the running `aiid` and `execve` into it (v0.0.80).
    /// Reversible: a second `rollback` call flips back to the
    /// previously-running binary.
    Rollback {
        /// JSON-RPC endpoint of the target node.
        #[arg(long, default_value = "http://127.0.0.1:8545")]
        rpc: String,
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
    /// Derive an address from a 32-byte secp256k1 private-key hex file.
    FromKeyFile {
        /// Private-key hex file.
        #[arg(long)]
        file: std::path::PathBuf,
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
            // v0.0.80 — async-only rollback subcommand peels off
            // here; everything else stays in the sync handler.
            if let ReleaseCmd::Rollback { rpc } = &sub {
                let r = aii_cli::run_rollback_release(rpc).await?;
                if cli.json {
                    println!("{}", serde_json::to_string(&r)?);
                } else if r.scheduled {
                    println!(
                        "scheduled rollback to .previous; node will restart in {} s",
                        r.restart_in_secs
                    );
                } else {
                    println!("rollback rejected: {}", r.reason);
                }
            } else {
                handle_release_cmd(sub, cli.json)?;
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
        Cmd::LiveTransferLoad {
            chain_id,
            key_files,
            total,
            min_aii,
            max_aii,
            txs_per_block,
            settle_sec,
        } => {
            let r = run_live_transfer_load(
                &cli.rpc,
                chain_id,
                &key_files,
                total,
                &min_aii,
                &max_aii,
                txs_per_block,
                settle_sec,
            )
            .await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                println!("rpc:              {}", r.rpc);
                println!("chain_id:         {}", r.chain_id);
                println!("requested:        {}", r.total_requested);
                println!("submitted:        {}", r.submitted);
                println!("accepted:         {}", r.accepted);
                println!("rejected:         {}", r.rejected);
                println!(
                    "value range:      {}..{} AII",
                    r.min_value_aii, r.max_value_aii
                );
                println!("total value wei:  {}", r.total_value_wei);
                println!("gas price wei:    {}", r.gas_price_wei);
                println!("sim blocks:       {}", r.simulated_blocks);
                println!("elapsed_ms:       {}", r.elapsed_ms);
                println!("accounts:");
                for a in &r.accounts {
                    println!(
                        "  #{} {} nonce={} signed={} balance {} -> {}",
                        a.index,
                        a.address,
                        a.initial_nonce,
                        a.signed_txs,
                        a.initial_balance_wei,
                        a.final_balance_wei
                    );
                }
                if !r.errors.is_empty() {
                    println!("errors:");
                    for e in &r.errors {
                        println!("  {e}");
                    }
                }
            }
        }
        Cmd::FundAddresses {
            chain_id,
            from_key_file,
            to_key_files,
            to_addresses,
            amount_aii,
            settle_sec,
        } => {
            let mut recipients = Vec::new();
            for path in &to_key_files {
                recipients.push(run_account_from_key_file(path)?);
            }
            for address in &to_addresses {
                recipients.push(parse_cli_address(address)?);
            }
            let r = run_fund_addresses(
                &cli.rpc,
                chain_id,
                &from_key_file,
                &recipients,
                &amount_aii,
                settle_sec,
            )
            .await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                println!("rpc:             {}", r.rpc);
                println!("chain_id:        {}", r.chain_id);
                println!("from:            {}", r.from_address);
                println!("initial_nonce:   {}", r.initial_nonce);
                println!("amount:          {} AII", r.amount_aii);
                println!("amount wei:      {}", r.amount_wei);
                println!("gas price wei:   {}", r.gas_price_wei);
                println!("submitted:       {}", r.submitted);
                println!("accepted:        {}", r.accepted);
                println!("rejected:        {}", r.rejected);
                println!(
                    "balance:         {} -> {}",
                    r.initial_balance_wei, r.final_balance_wei
                );
                println!("recipients:");
                for recipient in &r.recipients {
                    match (&recipient.tx_hash, &recipient.error) {
                        (Some(hash), _) => println!(
                            "  #{} {} amount={} tx={}",
                            recipient.index, recipient.address, recipient.amount_wei, hash
                        ),
                        (_, Some(error)) => println!(
                            "  #{} {} amount={} error={}",
                            recipient.index, recipient.address, recipient.amount_wei, error
                        ),
                        _ => println!(
                            "  #{} {} amount={} pending",
                            recipient.index, recipient.address, recipient.amount_wei
                        ),
                    }
                }
                println!("elapsed_ms:      {}", r.elapsed_ms);
            }
        }
        Cmd::StateCredit {
            data_dir,
            to_key_files,
            to_addresses,
            amount_aii,
        } => {
            let mut recipients = Vec::new();
            for path in &to_key_files {
                recipients.push(run_account_from_key_file(path)?);
            }
            for address in &to_addresses {
                recipients.push(parse_cli_address(address)?);
            }
            let r = run_state_credit(&data_dir, &recipients, &amount_aii)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                println!("data_dir:   {}", r.data_dir);
                println!("amount:     {} AII", r.amount_aii);
                println!("amount wei: {}", r.amount_wei);
                println!("accounts:");
                for account in &r.accounts {
                    println!(
                        "  #{} {} nonce={} balance {} -> {}",
                        account.index,
                        account.address,
                        account.nonce,
                        account.before_balance_wei,
                        account.after_balance_wei
                    );
                }
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
        Cmd::Account {
            sub: AccountCmd::FromKeyFile { file },
        } => {
            let addr = run_account_from_key_file(&file)?;
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
        Cmd::BftCapacity {
            validators,
            network_nodes,
            proposal_bytes,
            target_secs,
        } => {
            let r = run_bft_capacity(validators, proposal_bytes, target_secs, network_nodes)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                println!("validators:              {}", r.validators);
                println!("network_nodes:           {}", r.network_nodes);
                println!("consensus_nodes:         {}", r.consensus_nodes);
                println!("passive_nodes:           {}", r.passive_nodes);
                println!("target_secs:             {}", r.target_secs);
                println!("proposal_bytes:          {}", r.proposal_bytes);
                println!("quorum_votes:            {}", r.equal_stake_quorum_votes);
                println!("vote_messages/round:     {}", r.vote_messages_per_round);
                println!(
                    "vote_payload_bytes/round: {}",
                    r.vote_payload_bytes_per_round
                );
                println!(
                    "leader_fanout_bytes:      {}",
                    r.leader_proposal_fanout_bytes
                );
                println!("min_leader_upload_mbps:  {}", r.min_leader_upload_mbps);
                println!("satisfies_design_cap:    {}", r.satisfies_design_cap);
                println!(
                    "passive_nodes_no_fanout: {}",
                    r.passive_nodes_do_not_increase_bft_fanout
                );
            }
        }
        Cmd::BftPressure {
            validators,
            network_nodes,
            heights,
            target_secs,
        } => {
            let r = run_bft_pressure(validators, network_nodes, Some(heights), target_secs)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                println!("validators:              {}", r.validators);
                println!("network_nodes:           {}", r.network_nodes);
                println!("consensus_nodes:         {}", r.consensus_nodes);
                println!("passive_nodes:           {}", r.passive_nodes);
                println!("heights:                 {}", r.heights);
                println!("target_secs:             {}", r.target_secs);
                println!("quorum_votes:            {}", r.equal_stake_quorum_votes);
                println!("votes_processed:         {}", r.votes_processed);
                println!("certificates_verified:   {}", r.certificates_verified);
                println!("elapsed_ms:              {}", r.elapsed_ms);
                println!("max_height_ms:           {}", r.max_height_ms);
                println!("avg_height_ms:           {}", r.avg_height_ms);
                println!("satisfies_target:        {}", r.satisfies_target);
                println!(
                    "passive_nodes_no_fanout: {}",
                    r.passive_nodes_do_not_increase_bft_fanout
                );
            }
        }
        Cmd::DiscoveryProbe {
            seeds,
            listen,
            bft_listen,
            timeout_ms,
            http_bootnodes,
            no_http_fallback,
        } => {
            let seed_specs = if seeds.is_empty() {
                DEFAULT_DISCOVERY_PROBE_SEEDS
                    .iter()
                    .map(|seed| (*seed).to_string())
                    .collect::<Vec<_>>()
            } else {
                seeds
            };
            let http_bootnodes = if no_http_fallback {
                Vec::new()
            } else if http_bootnodes.is_empty() {
                DEFAULT_DISCOVERY_PROBE_HTTP_BOOTNODES
                    .iter()
                    .map(|bootnode| (*bootnode).to_string())
                    .collect::<Vec<_>>()
            } else {
                http_bootnodes
            };
            let r =
                run_discovery_probe(&seed_specs, listen, bft_listen, timeout_ms, &http_bootnodes)
                    .await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                println!("seeds:                {}", r.seed_specs.join(","));
                println!("resolved:             {}", r.resolved_seeds.join(","));
                println!(
                    "observed_discovery:   {}",
                    r.observed_discovery.as_deref().unwrap_or("-")
                );
                println!("bft_peers:            {}", r.discovered_bft_peers.join(","));
                println!("http_bootnodes:       {}", r.http_bootnodes.join(","));
                println!(
                    "http_bft_peers:       {}",
                    r.http_fallback_bft_peers.join(",")
                );
                println!(
                    "discovery_peers:      {}",
                    r.discovered_discovery_peers.join(",")
                );
                println!("elapsed_ms:           {}", r.elapsed_ms);
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

fn parse_cli_address(s: &str) -> Result<aii_types::Address, CliError> {
    let s = s.trim().strip_prefix("0x").unwrap_or_else(|| s.trim());
    let bytes = hex::decode(s).map_err(|e| CliError::Client(format!("address hex: {e}")))?;
    if bytes.len() != 20 {
        return Err(CliError::Client(format!(
            "address must be 20 bytes, got {}",
            bytes.len()
        )));
    }
    let mut out = [0u8; 20];
    out.copy_from_slice(&bytes);
    Ok(aii_types::Address::new(out))
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
            let pk = match pubkey {
                Some(hex) => Ed25519PublicKey::from_hex(&hex)
                    .map_err(|e| CliError::Client(format!("pubkey: {e}")))?,
                None => aii_cli::release::pinned_release_pubkey(),
            };
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
                        "pubkey": pk.to_hex(),
                    })
                );
            } else {
                println!(
                    "ok — {} signed at {} (key {})",
                    m.version,
                    m.timestamp_unix,
                    &pk.to_hex()[..16]
                );
            }
        }
        // Rollback is handled in the async dispatch in main.rs
        // (needs to await an HTTP RPC call); reaching it here
        // means the dispatch shortcut was bypassed somehow.
        ReleaseCmd::Rollback { .. } => {
            return Err(CliError::Client(
                "release rollback is dispatched in main(); reaching the sync handler is a bug"
                    .into(),
            ));
        }
    }
    Ok(())
}
