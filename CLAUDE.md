# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

AII is a from-scratch L1 blockchain written in pure Rust, distributed as a Cargo workspace of ~26 crates that together produce three binaries:

- **`aiid`** (crate `aii-node`) — the node daemon: RPC server + block producer + P2P transport.
- **`aii`** (crate `aii-cli`) — user-facing CLI: wallet, chain queries, stress testing, sub-chain runner.
- **`aii-mcp`** (crate `aii-mcp`) — Model Context Protocol stdio server; exposes chain queries + wallet helpers as tools for Claude / Cursor / Cline.

A live testnet runs on `aii.allfund.xyz` (chain id `9999`). The README has the topology.

## Build / test / lint

The workspace pins `rust-version = "1.85"` but the user's local toolchain is **pinned to 1.94.1** to avoid a `rustup` auto-sync hang. Use:

```bash
PATH=/home/jack/.rustup/toolchains/1.94.1-x86_64-unknown-linux-gnu/bin:$PATH \
    cargo test --workspace
```

Commands you'll use:

```bash
# Run the full suite. Should print "X tests" with no failures. Current baseline: 653.
cargo test --workspace

# Run one crate's tests.
cargo test -p aii-consensus-bft

# Run one test by substring.
cargo test -p aii-block signer

# Lints — must be clean before any commit. The workspace runs `clippy::all=deny`
# + `pedantic=warn` + `nursery=warn` (see [workspace.lints.clippy] in root Cargo.toml).
cargo clippy --workspace --all-targets -- -D warnings

# Format. CI/local style is single rustfmt config (no rustfmt.toml — defaults).
cargo fmt --all

# Release binaries used by deploy + stress runs.
cargo build --release -p aii-node -p aii-cli -p aii-mcp
```

When a clippy lint fires that is genuinely worth an exception, add a narrow `#[allow(clippy::xxx)]` with a one-line comment on *why*. Do not edit the workspace lint table.

## Workspace layout (dependency order)

The crates form a clean dependency DAG. When changing a primitive, expect downstream crates to need updates in this order:

```
aii-types ─┬─► aii-codec ──► aii-block ──┬─► aii-state ──► aii-consensus-iface ──┬─► aii-consensus-bft ──┐
           │                              │                                       │                       ├─► aii-node (aiid)
           │                              │                                       └─► aii-consensus-poa ──┤
           ├─► aii-crypto ───────────────►┘                                                               │
           │                                                                                              │
           ├─► aii-storage ─► aii-state                                                                   │
           ├─► aii-config ─► aii-microchain                                                               │
           └─► aii-wallet ─► aii-cli (aii) ◄──── aii-rpc ◄────────────────────────────────────────────────┘
                                  ▲
                                  └── aii-mcp (talks to aiid via RPC)
```

Layered roles:

- **`aii-types`** — H256, Address, U256, AlgoId, BlsPubKey/Signature, VrfPubKey. Foundational; almost every other crate depends on it.
- **`aii-crypto`** — keccak256, secp256k1 (sign/verify/recover, ETH address derivation), BLS12-381 (G1 pubkey + G2 sig), schnorrkel VRF.
- **`aii-codec`** — RLP / SSZ / JSON-RPC framing helpers.
- **`aii-storage`** — `KvBackend` trait with `MemoryBackend` + `RocksDbBackend` impls. Closed-enum `ColumnFamily` (Headers, Bodies, State, etc.) — adding a CF requires a spec revision.
- **`aii-block`** — `Block` / `Header` / `BlockBody` / `Tx` (EIP-2718 envelope: Legacy + EIP-1559 + EIP-4844) / `Receipt`. Byte-perfect with Ethereum mainnet by default; PQ algorithms (`AlgoId::MlDsa65`) are wire-additive.
- **`aii-state`** — `StateDb<B>` + `Account`. Full MPT is on the roadmap; current impl is a flat KV.
- **`aii-config`** — `ChainSpec` (mainnet/testnet) + `Genesis` JSON.
- **`aii-consensus-iface`** — the `Engine` trait every consensus impl satisfies + `ConsensusKind` enum (`Bft | Poa`).
- **`aii-consensus-bft`** — VRF-PoS BFT engine, two-phase PRE-VOTE / PRE-COMMIT, ⅔-stake BLS aggregate finality, `BftGossip` driver, `BftMessage` wire format (`Proposal | Prevote | Precommit`).
- **`aii-consensus-poa`** — Proof-of-Authority, fixed authority list, round-robin signer by `H % authorities.len()`.
- **`aii-net-p2p`** — TCP transport with length-prefixed RLP framing. `Message::{Hello, Ping, Pong, Disconnect, Bft(Vec<u8>)}`.
- **`aii-net-txpool`** — `(sender, nonce)`-keyed mempool with capacity + gas-price eviction + `drain_up_to(n)`.
- **`aii-rpc`** — `eth_*` and `aii_*` namespaces over jsonrpsee HTTP. `RpcState` trait — node impls override default `None`/empty methods.
- **`aii-node`** — `aiid` binary. `NodeState` owns the in-memory block index + mempool. `bft_p2p::TcpBftTransport` wraps `aii-net-p2p` for BFT gossip.
- **`aii-microchain`** — Sub-chain registry + `FlushAnchor`.
- **`aii-evm`** — `execute_transfer` placeholder; real `revm` integration deferred.

## How consensus + producer + tx pipeline fit together

The `aiid` `main()` is one large dispatch that selects a producer mode by CLI:

- `--bft --genesis G --keystore K` + single-validator set → `BftEngine::advance_single()` loop.
- `--bft …` + multi-validator set + `--peers ADDR1,ADDR2,…` → `BftGossip::tick()` loop over `TcpBftTransport`.
- `--consensus poa --authorities A1,A2,… --coinbase A` → `PoaEngine::produce_block()` loop.

All three loops follow the same pattern: every `--slot-seconds`, drain up to `gas_limit / PLACEHOLDER_TX_GAS` (= 1,428) txs from `NodeState::tx_pool()`, call `engine.set_pending_txs(txs)`, then produce. The block lands in the in-memory `BlockStore` and head bumps.

`eth_sendRawTransaction` is implemented in `aii-node::NodeState::submit_raw_tx`: hex → EIP-2718 decode → `Tx::recover_signer(chain_id)` (in `aii-block::tx::signer`) → `TxPool::add` keyed by `(sender, nonce)`. Returns the keccak256 hash.

**Multi-validator BFT does not yet gossip block bodies** — `BftMessage::Proposal` only carries `(hash, leader_proof)` and peers reconstruct an empty block. Tx-bearing blocks therefore require single-validator BFT or PoA today. Body gossip is on the roadmap.

## TDD + release pattern

The repository has been driven by strict RED → GREEN → REFACTOR throughout. When adding a feature:

1. Branch from `master`: `git checkout -b feat/aii-<topic>`.
2. Write the failing test first in the relevant crate.
3. Make it pass with the minimal change.
4. `cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings` all green.
5. Bump version in workspace `Cargo.toml` (`sed -i 's/"0.0.X"/"0.0.Y"/g' Cargo.toml`) — there are 27 occurrences across `[workspace.package].version` + every `aii-* = { …, version = "…" }` line.
6. Write the `## [0.0.Y]` block in `CHANGELOG.md` under `## [Unreleased]`. Lead with **what changed and why anyone cares**, then a "Scope discipline" section listing what was explicitly out of scope.
7. `git commit` (single squashed commit per release is fine); `git checkout master`; `git merge --no-ff feat/aii-<topic>`; `git tag -a v0.0.Y -m "v0.0.Y — <one-line>"`; delete the feature branch.
8. Push: see "Pushing to GitHub" below.

Every release on `master` should leave: `cargo test --workspace` all-green, `cargo clippy -D warnings` clean, CHANGELOG updated, version bumped, `v0.0.Y` tag.

## Pushing to GitHub

The user's local SSH key (`~/.ssh/id_ed25519`) is for a *different* GitHub account (`reikiplanet`). Pushes to `github.com/kinglovesdao/aii` go over HTTPS with a Personal Access Token stored at `/media/jack/drive4/blockchain/githubkey.md` (line 4, after the "： " separator).

The recipe — extracts token, scopes it to a single `git push`, doesn't write it to disk:

```bash
TOKEN=$(sed -n '4p' /media/jack/drive4/blockchain/githubkey.md | sed 's/.*： //')
git -c credential.helper="!f() { echo username=x-access-token; echo password=$TOKEN; }; f" \
    push origin master vX.X.X
```

After any push that read the token, the user expects a "PAT 提醒" — a one-line reminder to revoke/rotate the token if it appeared in any tool output. Do not store the token in `.git-credentials` or any committed file.

## Live testnet ops

Two Aliyun ECS nodes form the testnet (passwords + IPs in `/media/jack/drive4/blockchain/vode.md`):

- **JP** `8.211.135.234` — Ubuntu 24.04, native `aiid` via systemd unit `/etc/systemd/system/aiid.service`, RPC on `0.0.0.0:8545`, BFT gossip on `0.0.0.0:30311`. Logs at `/var/log/aiid.log`. Keystore + genesis under `/var/lib/aiid/`.
- **CN** `106.14.223.128` — CentOS 7 (glibc 2.17 — too old for our Ubuntu 24 binary). `aiid` runs inside an `ubuntu:24.04` Docker container (`docker logs aiid`). Same systemd unit name; `ExecStart` invokes `docker run --rm --network host …`.

Cloud security groups must open `30311/tcp` and `8545/tcp` inbound on both — user does this in the Aliyun console.

The browser-facing explorer at `https://aii.allfund.xyz/` is a single HTML page at `/var/www/aii.allfund.xyz/index.html` served by nginx, which reverse-proxies `/api` → `127.0.0.1:8545`. Cert by Let's Encrypt + certbot-auto-renew. Site config: `/etc/nginx/sites-available/aii.allfund.xyz`.

The user's local network runs Mihomo (Clash fork) on the `198.18.0.0/30` fake-IP range, so DNS queries from `dig` on the laptop return Mihomo's interception address. To check real DNS / connectivity for `*.allfund.xyz`, query from one of the servers, not locally.

## Sub-chain runtime

`aii subchain run` (added in v0.0.38) spawns an in-process PoA sub-chain with a fresh secp256k1 operator key, produces blocks at `--slot-seconds`, and every `--flush-interval-blocks` signs an EIP-155 legacy self-transfer whose calldata is `sub_block_hash ‖ sub_block_number_be8` and submits it to the parent via `eth_sendRawTransaction`. The parent treats it as a normal tx — anchor verification on the parent side is not yet wired (sub-chain → main-chain registry update is a future release).

## Conventions worth knowing

- **`AlgoId::Secp256k1` is the wire-default.** For Ethereum-compatible mode, all RLP encodings emit a *byte-perfect* legacy / EIP-1559 layout with no trailing algorithm byte. The `aii-block::tx::legacy` round-trip tests assert this. PQ modes (e.g. `MlDsa65`) emit one extra trailing byte and are explicitly *not* Ethereum-compatible.
- **Every block currently treats each tx as a fixed-cost 21,000-gas transfer** (`PLACEHOLDER_TX_GAS` in both `aii-consensus-bft` and `aii-consensus-poa`). Real `revm` execution + receipts land in a later release. `header.gas_used = tx_count * 21_000`.
- **Headers in `NodeState` are in-memory only.** RocksDB is opened (so the data dir is reserved) but not yet written to. A restart loses the block index.
- **Module-level docstrings carry the error contract.** `#[error("…")]` on each `thiserror::Error` variant + a `## Errors` section in the module doc — not per-fn `# Errors` comments (workspace clippy disables `missing_errors_doc`).
