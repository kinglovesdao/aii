# Changelog

All notable changes to AII workspace follow [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.22] — 2026-05-24

### Added — sub-chain VM host imports

`aii-wasm` grows the six `env.*` host functions that turn the v0.0.19
wasmtime VM from a pure calculator into a stateful sub-chain contract
runtime. Reads consult a per-call overlay, then fall through to the
chain's persisted state via the new `HostState` trait. Writes, logs and
abort messages accumulate in `HostEffects` and are returned to the
caller only on success — any revert path drops them.

#### aii-wasm

- New `WasmModule` — compiled binary reusable across many host-aware
  calls; pairs with the new `WasmRuntime::compile(wasm)`.
- New `CallContext { caller, callee, block_number, block_timestamp }`
  passed in per call.
- New `HostState` trait — single method `storage_get(addr, slot)` —
  read-only view into persisted chain state. Implementations are
  trivial wrappers over `aii-state::StateDb`; the trait is tiny on
  purpose so tests can mock it without dragging in storage.
- New `HostEffects { storage_writes, logs }` — sorted by slot for
  determinism. Repeat writes to the same slot collapse to the last
  value.
- New `WasmRuntime::call_with_host(module, fuel, name, args, ctx, host)`
  → `HostCallResult { return_value, effects, fuel_remaining }`.
- Six host imports under module `env`:
  - `storage_read(slot_ptr, out_ptr)` — overlay first, then `HostState`.
  - `storage_write(slot_ptr, value_ptr)` — into overlay only.
  - `caller(out_ptr)` / `self_address(out_ptr)` — 20-byte writes.
  - `log(data_ptr, data_len)` — append to effects.
  - `abort(msg_ptr, msg_len)` — record message (≤ 256 bytes) and trap.
- New `WasmError::Aborted(String)` variant for explicit contract revert.
- 14 RED→GREEN tests using hand-written WAT modules covering
  read/write round-trip, host-state fall-through, write collection,
  same-slot last-write-wins, caller/self_address, log (including
  zero-length), abort + truncation, per-call effect isolation,
  out-of-fuel inside a host-call loop, and OOB pointer trapping.

### Scope discipline (unchanged from v0.0.19)

`aii-wasm` is the sub-chain VM only — cross-contract storage access,
native AII transfers, WASI / wasi-preview2, and AOT/cache are explicit
non-goals and remain so. Block-context accessors (`block_number`,
`block_timestamp`) are reserved in `CallContext` but not yet exported
to WASM; they land when the consensus layer plumbs them through.

## [0.0.21] — 2026-05-24

### Added — federated multisig bridge `Vault`

aii-crosschain grows a second cross-chain primitive next to HTLC: a
BLS-aggregated threshold multisig `Vault`. A federation of `n` validators
signs a `LockReceipt` (proof of asset lock on the source chain); the
on-chain `Vault` accepts the release iff at least `t` signers participate,
the aggregated BLS signature verifies over the receipt digest, and the
nonce has not been used before.

#### aii-crosschain

- Module split: existing HTLC content moved from `lib.rs` into a new
  `htlc` submodule. Public path is now `aii_crosschain::htlc::{Htlc, ...}`
  (no external consumers existed; no compat shim).
- New `federation` submodule:
  - `FederationSet { pubkeys, threshold }` — static `t`-of-`n` validator
    set, content-addressed by `keccak256(threshold_be8 ‖ pubkey₁_compressed
    ‖ … ‖ pubkeyₙ_compressed)`. Caps `n ≤ 64` so a `u64` signer bitmap is
    sufficient.
  - `LockReceipt { src_chain_id, asset, amount, recipient, nonce }` —
    `digest(federation_id)` domain-separates by federation so receipts
    cannot be replayed across different federation sets.
  - `AttestationBundle { receipt, aggregated_sig, signer_bitmap }` — what
    the off-chain aggregator submits.
  - `Vault::release(&bundle)` — validates bitmap bounds, threshold,
    nonce replay, and BLS `fast_aggregate_verify`, in that order. On
    success returns `Released { receipt }`; the caller performs the
    actual asset transfer.
- 13 new TDD tests covering construction validation, content-addressed
  id, digest determinism, threshold success/failure, signature forgery
  rejection, replay protection, and bitmap-bounds enforcement.

### Scope discipline (unchanged from HTLC release)

`aii-crosschain` is the on-chain state machine only. Off-chain attester
clients, source-chain listeners, federation set rotation, IBC light
clients, and full XCM adapters remain explicit non-goals — they will
land in later releases.

## [0.0.20] — 2026-05-24

### Added — persistent contract state (bytecode + storage)

aii-evm transactions now persist contract state across calls, which is
the prerequisite for real ERC-20-style contracts to work on AII.

#### aii-storage

- New `ColumnFamily::Code` — `code_hash → bytecode bytes`. Bytecode is
  stored content-addressed by `keccak256(code)`, so identical code
  deployed twice naturally dedups.

#### aii-state

- New `StateDb::code_get(code_hash)` / `code_put(code_hash, bytes)` —
  bytecode storage backed by `ColumnFamily::Code`.
- New `StateDb::storage_get(addr, slot)` / `storage_put(addr, slot, val)` —
  per-account EVM storage backed by `ColumnFamily::AccountStorage`.
  Reads of unset slots return `H256::ZERO`. Writing `H256::ZERO`
  deletes the slot (matches EVM semantics). Flat 52-byte `addr ‖ slot`
  key for now; per-account Merkle tries are a later optimization.
- New `StateError::Decode(String)` variant for malformed on-disk
  storage values.

#### aii-evm

- `RevmDb::code_by_hash` now looks the bytecode up via
  `StateDb::code_get` instead of returning empty. Contracts deployed
  in earlier transactions can now be CALLed.
- `RevmDb::storage` now looks the slot up via `StateDb::storage_get`
  instead of returning `U256::ZERO`. `SLOAD` returns the last
  persisted value.
- `execute_with_revm` now commits the full revm state diff per tx:
  account header (nonce/balance/code_hash), newly-deployed bytecode
  (`info.code` → `code_put`), and every changed storage slot
  (`slot.is_changed()` → `storage_put`).

### Tests (3 new, all RED → GREEN)

- `deploy_persists_runtime_bytecode_under_code_hash` — deploys a
  hand-crafted 18-byte contract; verifies that the runtime bytecode is
  retrievable from the Code CF by the resulting `account.code_hash`.
- `calling_writer_persists_storage_slot` — deploys a writer contract
  (`SSTORE(0, 0x42)`), CALLs it in a *separate* `execute_with_revm`
  invocation, and verifies the slot persists. This is the test that
  exercises the cross-tx `code_by_hash` lookup.
- `reader_recovers_persisted_storage` — deploys a reader
  (`SLOAD(0); RETURN`), seeds `storage[reader][0] = 0x77` via
  `StateDb`, then CALLs the reader and verifies it returns the
  pre-seeded value in the 32-byte output buffer.

### Out of scope (deferred)

- Per-account storage trie + storage root in `Account` — flat KV is
  semantically equivalent for revm; the trie matters once we hash
  state roots for headers.
- Block-hash lookup in `RevmDb::block_hash` — still a deterministic
  placeholder; lands once `aii-node` exposes a header index.

## [0.0.19] — 2026-05-24

### Added — aii-wasm scoped sub-chain VM

- New crate `aii-wasm` providing a wasmtime-backed WebAssembly runtime
  for AII sub-chains. This release intentionally exposes only the
  surface needed to validate the gas/fuel model end-to-end — host
  imports, richer signatures, and module caching are deferred.
  - `WasmRuntime::new()` constructs a wasmtime `Engine` with
    `consume_fuel(true)` enabled. The engine is reusable across many
    modules and many calls.
  - `WasmRuntime::instantiate(wasm, fuel)` validates + compiles the
    binary, opens a fresh `Store`, sets the per-call fuel budget, and
    returns a `WasmInstance`. Invalid bytes are rejected with
    `WasmError::BadModule`.
  - `WasmInstance::call_i32(name, args)` invokes an exported
    `i32, … → i32` function. Strict arity / single-i32-result
    checking on entry; trap classification on exit. Out-of-fuel,
    missing export, and signature mismatch surface as discrete error
    variants for clean caller branching.
  - `WasmInstance::fuel_remaining()` reads the store's fuel reserve
    after a call so consensus can charge the actual consumption back
    to the transaction.
- 9 unit tests covering: runtime construction, module validation
  (good + garbage), `add` happy path with positive and negative i32
  arguments, missing-export and wrong-arity rejection, fuel decrease
  after execution, and infinite-loop trapping as `OutOfFuel`.

### Gas model

AII maps `1 tx-gas = 1 wasm-fuel-unit` for now; the consensus layer
allocates the budget per call. This is a parameter that the chain
governance — once defined — can re-tune without touching this crate.

### Out of scope (deferred)

- Richer call signatures (i64, f32, multi-return) — v0.0.20+
- Host imports (state read/write, log, transfer to other addresses) —
  v0.0.20+
- WASI / wasi-preview2 — explicitly never on the consensus path
- Module caching / AOT compilation — performance work, not behavior

### Dependencies

- New: `wasmtime = "26"` (default-features off, `cranelift + runtime`
  only) plus `wat` as a dev-dependency for tests. wasmtime pulls in
  cranelift which adds a one-time ~12 s compile cost the first time
  `cargo build` runs after this update.

## [0.0.18] — 2026-05-24

### Added — aii-crosschain (scoped HTLC)

- New crate `aii-crosschain` providing the on-chain state machine for
  Hash Time-Locked Contracts — the building block for trustless atomic
  swaps between AII and external chains.
  - `Htlc` record (sender, recipient, amount, secret_hash, timeout,
    state) with a `Locked → Claimed | Refunded` finite state machine.
  - `Htlc::claim(preimage)` — transitions iff `keccak256(preimage) ==
    secret_hash`. Wrong preimage rejected; state preserved.
  - `Htlc::refund(now)` — transitions iff `now ≥ timeout`. Early refund
    rejected; state preserved.
  - `Htlc::new()` rejects zero amount and `sender == recipient`.
  - Terminal states are sticky: double-claim, claim-after-refund, and
    refund-after-claim are all rejected via `HtlcError::NotLocked`.
  - `htlc_id(&Htlc)` — content-addressed identifier
    `keccak256(sender ‖ recipient ‖ amount ‖ secret_hash ‖ timeout)`
    used by cross-chain protocols to reference a lock without an
    index. Stable across nodes; independent of lifecycle state.
- 14 unit tests including TDD RED → GREEN cycle verification.

### Fixed

- `aii-storage` proptest `snapshot_unchanged_under_concurrent_writer`
  no longer fails on duplicate-key inputs. The test now dedups
  `seed_pairs` via `BTreeMap` (last-write-wins) before seeding and
  verifying, matching the backend's actual semantics.

### Scope notes

Out of scope for this release: multi-sig bridge federation (Aii ↔
Ethereum custodial), IBC light clients, Polkadot XCM adapters. These
build on the HTLC primitive and will land in later releases.

## [0.0.17] — 2026-05-24

### Added — devp2p Discovery v4 (UDP) — Ping / Pong

- New module `aii-net-p2p::discovery` implementing the Ethereum
  Discovery v4 wire spec (<https://github.com/ethereum/devp2p/blob/
  master/discv4.md>):
  - **Packet framing** — `hash (32) || signature (65) || type (1) ||
    rlp(data)`. Hash verified end-to-end (tampering detected at decode).
  - **Signature** — secp256k1 over `keccak256(type || data)`. Decoder
    *recovers* the sender's public key + address from the signature
    (matches devp2p's design).
  - **Packet types** — `Ping (0x01)` and `Pong (0x02)`. `FindNode (0x03)`
    + `Neighbours (0x04)` land in v0.0.18 with the Kademlia routing
    table.
  - **`Endpoint`** — IPv4/IPv6 + UDP port + TCP port; RLP round-trips.
  - **`UdpDiscovery`** — async UDP driver (`bind` / `send` / `recv`).
    `recv` carries a per-call timeout.
- 8 unit tests including a real **UDP loopback Ping/Pong exchange**
  between two driver instances + tampered-packet detection + truncated-
  packet rejection + unknown-type-byte rejection + recv-timeout.

### Changed
- Workspace 0.0.16 → 0.0.17.
- `aii-net-p2p` now depends on `aii-crypto` for secp256k1 packet
  signatures.

### Notes
- Packets are size-capped at the spec's 1280-byte UDP ceiling.
- `FindNode` / `Neighbours` need a Kademlia routing table + node-id
  XOR distance bucketing — separate v0.0.18 deliverable. The current
  protocol-version constant (`DISCOVERY_VERSION = 4`) is already
  embedded in `Ping` payloads so peers consider us spec-compliant.

## [0.0.16] — 2026-05-24

### Added — `aii-evm` revm 18 integration (contract execution)

- **`aii-evm::RevmDb`** — `revm::Database` adapter over
  `aii_state::StateDb`. Reads accounts on demand; emits empty bytecode
  / empty storage as a stop-gap (per-account storage trie lands in
  v0.0.17+).
- **`aii-evm::execute_with_revm`** — runs a tx through revm 18 and
  commits the resulting state diff back to `StateDb`. Handles:
  - Value transfer (sender/recipient balance + nonce updates).
  - Contract CALL with arbitrary calldata.
  - Contract CREATE — returns the deployed address.
  - Insufficient balance / invalid signature paths produce
    `ExecError::Revm` from revm's pre-tx validation.
- **`ExecutionSummary`** — `success` / `gas_used` / `output` /
  `deployed_contract`.

### Tests (4 new revm-driven cases, 10 total in `aii-evm`)
- `revm_value_transfer_advances_balances` — balance + nonce diff after
  a 123-Wei transfer.
- `revm_insufficient_balance_returns_failure_or_error` — sender below
  required value rejected by revm's pre-tx validation.
- `revm_empty_create_deploys_an_address` — empty init code lands at
  CREATE-derived address.
- `revm_call_to_eoa_with_zero_value_is_a_no_op_success` — sanity check
  that revm accepts trivial CALLs.

### Changed
- Workspace 0.0.15 → 0.0.16.
- `aii-evm` deps: `revm = "18"`, `derive_more = { version = "1",
  features = ["full"] }` (revm pulls derive_more without enabling any
  feature; force the full set).

### Limitations carried into v0.0.17+
- `RevmDb::storage` returns `U256::ZERO`. Real ERC-20 etc. need a
  per-account storage trie + storage CF in `aii-storage`.
- `RevmDb::code_by_hash` returns empty bytecode. Persistent bytecode
  by `code_hash` is part of the same v0.0.17 work.
- `block_hash` returns a deterministic placeholder; harmless for tests.

## [0.0.15] — 2026-05-24

### Added — `aii-consensus-bft` (scoped) + live block production

- **`aii-consensus-bft`** (M2 #11, scoped) — single-node dev-mode BFT
  engine:
  - `DevModeEngine` implements `aii_consensus_iface::Engine` so embedders
    can swap to a real multi-validator BFT later without API churn.
  - `produce_block()` builds an empty child block per slot, advances
    the head, returns `(hash, number, Block)`.
  - `EngineConfig` (slot_seconds / coinbase / base_fee / gas_limit).
  - 8 unit tests covering head advance, parent hash linkage, timestamp
    increment, Engine trait integration.
- **`aiid` binary** now produces blocks on a background task:
  - `--produce-blocks` (default `true`) starts the dev producer loop.
  - `--slot-seconds N` sets the block interval (default 3 s).
  - `NodeState.set_head` is called every slot — `eth_blockNumber` is no
    longer permanently `0`.
- Live-verified end-to-end: `eth_blockNumber` returned `0x0 → 0x2 → 0x4`
  across 5 seconds at `--slot-seconds 1`; node log emitted
  `block produced` events with monotonically increasing hashes.

### Changed
- Workspace version 0.0.14 → 0.0.15 (note: 0.0.14 release tag also moved
  the workspace.package.version that had drifted to 0.0.13 since 0.0.13
  was the last release that actually bumped both — 0.0.15 syncs all 22
  path-dep version constraints).

### Notes — what's NOT yet in this engine (v0.0.16+ targets)
- VRF-based proposer selection (primitive exists in `aii-crypto::vrf`).
- PRE-VOTE / PRE-COMMIT gossip over `aii-net-p2p`.
- BLS aggregate signature over PRE-COMMIT votes.
- ⅔ stake threshold → single-block instant finality.
- Multi-validator V-set rotation (`aii-vnode` already tracks stake).
- Block-body inclusion of txs from `aii-net-txpool`.

The trait surface (`Engine` / `Proposer` / `Voter` / `Validation`) is
already wired through `aii-consensus-iface`, so each future addition
is additive and the embedder API stays stable.

## [0.0.14] — 2026-05-24

### Added — `aii-mcp` keystore + mnemonic tools (4 new MCP tools)
- `account_new_encrypted(password)` — generate a fresh secp256k1 key
  and return a Web3 v3 keystore JSON encrypted under `password`.
- `account_verify(keystore_json, password)` — confirm a password
  unlocks a keystore; return the embedded address on success.
- `mnemonic_new(words)` — generate a fresh BIP-39 phrase (12 / 15 /
  18 / 21 / 24 words) + derive the first ETH-compatible address.
- `account_from_mnemonic(phrase, passphrase, index)` — re-derive any
  address from a known phrase. Verified live against the canonical
  `0x9858EfFD…` ethers/MetaMask fixture.

These tools let MCP clients (Claude Desktop / Claude Code / Cursor /
Cline) walk a user through creating, securing, and recovering an AII
account *without* ever touching the protocol RPC layer — the keystore
and mnemonic primitives are pure local computation.

### Tests
- 14 lib tests in `aii-mcp` (up from 7) covering all 4 new tools +
  the updated `tools/list` count + arg validation.
- Live stdio smoke verified end-to-end via piped JSON-RPC over
  `target/debug/aii-mcp` (4 tools roundtripped through the stdio
  parser).

### Changed
- Workspace version 0.0.13 → 0.0.14.
- `aii-mcp::handle_tools_call` now reads `arguments` from the MCP
  `tools/call` envelope (was previously ignoring it because the four
  v0.0.10 tools took no args).

### Notes
- Write tools (`send_transaction`, etc.) that *do* require RPC
  submission land in v0.0.15+ once `aii-rpc::eth_sendRawTransaction`
  + a wired mempool exist.

## [0.0.13] — 2026-05-24

### Added — BIP-39 Mnemonic + BIP-32 HD Derivation
- `aii-wallet::MnemonicPhrase` — BIP-39 mnemonics in English wordlist:
  - `generate(word_count)` for 12 / 15 / 18 / 21 / 24-word phrases (from
    OS RNG via `rand::thread_rng`).
  - `from_phrase(s)` validates checksum + wordlist membership.
  - `to_seed(passphrase)` produces the canonical 64-byte BIP-39 seed.
  - `to_wallet(passphrase, index)` derives a `LocalWallet` at BIP-44
    path `m/44'/60'/0'/0/{index}` (the MetaMask + ethers default).
- 11 unit tests including:
  - BIP-39 Trezor official seed test vector (`abandon × 11 about` +
    "TREZOR" → canonical 64-byte seed).
  - **MetaMask interop test**: same phrase + empty passphrase + index 0
    yields `0x9858EfFD232B4033E47d90003D41EC34EcaEda94` — bit-exact
    match with ethers-rs / web3.js / MetaMask.
- `aii-cli`: two new commands
  - `aii account mnemonic [--words 12]`
    → fresh phrase + first ETH-compatible address.
  - `aii account from-mnemonic --phrase "..." [--passphrase X] [--index N]`
    → re-derive any address.
  - 3 new lib tests + live-verified `aii` binary smoke.

### Changed
- Workspace version 0.0.12 → 0.0.13.
- `aii-wallet` deps: `bip39 = "2"`, `bip32 = "0.5"` (RustCrypto).

### Notes
- BIP-44 coin type `60` (Ethereum) is the default for full MetaMask
  interop. An AII-native path (coin type ~9999) can ship later as
  `to_wallet_aii(...)` without breaking the default API.
- The `aii-mcp` write tools (`send_transaction`, `account_import`)
  planned for v0.0.14 can now consume a phrase + index instead of a
  raw secret.

## [0.0.12] — 2026-05-24

### Added — Encrypted Keystore (Web3 Secret Storage v3)
- `aii-wallet::EncryptedKeystore` — full Web3 v3 keystore implementation:
  - **scrypt** KDF (configurable n/r/p; `ScryptParams::light` for tests,
    `::geth_default` for production)
  - **AES-128-CTR** cipher with random IV
  - **Keccak-256 MAC** over `derived_key[16..32] ‖ ciphertext` —
    verified *before* decryption to surface wrong-password errors cleanly
  - JSON serde compatible with `geth account import` / MetaMask:
    `{ version: 3, id: uuid, address, crypto: {...} }`
  - `encrypt(&LocalWallet, password, params)` / `decrypt(password)`
  - `to_json()` / `from_json()` round-trip
  - 8 unit tests (round-trip / wrong-password / JSON / tampered ciphertext
    / distinct ciphertexts on re-encrypt / version + cipher + kdf rejection)
- `aii-cli`: two new commands
  - `aii account new-encrypted --password … --out keystore.json`
  - `aii account verify --file keystore.json --password …`
  - 2 new lib tests; live-verified against `aii` binary.

### Changed
- Workspace version 0.0.11 → 0.0.12.
- `aii-wallet` deps grow: `scrypt`, `aes`, `ctr`, `cipher`, `serde_json`,
  `uuid` (v4 + serde).

### Notes
- BIP-39 mnemonic + BIP-32 HD derivation deferred to v0.0.13 — the
  keystore alone unlocks `aii-cli`'s `account new-encrypted` + the future
  `aii-mcp`'s `send_transaction` write-tool (which will accept a
  keystore + password instead of a raw secret).

## [0.0.11] — 2026-05-24

### Added — RPC extension wired to real StateDb
- `aii-rpc::RpcState` trait gains `gas_price()` and
  `account(addr) -> Option<AccountView>`. `AccountView` is the JSON
  shape returned by `aii_getAccount` (nonce + balance hex + roots hex).
- New methods:
  - `eth_gasPrice` — returns chain-spec floor as `0x…` hex Wei.
  - `eth_getBalance(address, blockTag)` — looks up `StateDb` via
    `RpcState::account`. `blockTag` is accepted but only the head is
    supported in v0.0.11.
  - `aii_getAccount(address)` — returns the full Account record or
    `null`.
- `aii-node::NodeState` now owns an in-memory `StateDb<MemoryBackend>`
  and exposes it via `state()`. Pre-populating accounts before booting
  the RPC server is now a one-liner (`state.state().set_account(...)`).

### Tests
- 4 new lib tests in `aii-rpc` covering all new methods (happy paths
  + missing-account + bad-address error).
- 2 new lib tests in `aii-node` end-to-ending the new methods through
  jsonrpsee.
- Live-verified against `aiid` binary:
  ```
  $ curl … eth_gasPrice     → "0x3b9aca00"
  $ curl … eth_getBalance   → "0x0" / "0xde0b6b3a7640000"
  $ curl … aii_getAccount   → null / {nonce, balance, ...}
  ```

### Changed
- Workspace version 0.0.10 → 0.0.11.

## [0.0.10] — 2026-05-24

### Added — User-facing surfaces + AI integration
- `aii-onboarding` — read-only hardware probe + Tier (T1–T7) recommender
  per 《04 架构设计文档》§14.4. `detect()` reads sysinfo; `score()` collapses
  to 0–100 with calibrated weights; `recommend_tier()` maps to Tier.
  11 unit tests across reference profiles + classify-disk + score-cap.
- `aii-cli` — user-facing CLI **`aii`** built on `clap` v4:
  - `aii status` / `aii chain-id` — query a running node via JSON-RPC
  - `aii account new` — generate a fresh secp256k1 address (key dropped)
  - `aii tier` — run the onboarding probe locally
  - `--rpc <URL>` / `--json` global flags
  4 lib tests + verified live against running aiid binary.
- `aii-mcp` — Model Context Protocol server **`aii-mcp`** over stdio:
  - MCP 2024-11-05 (`initialize` / `tools/list` / `tools/call`
    / `notifications/initialized`)
  - 4 read-only tools: `chain_status` / `chain_id` / `account_new`
    / `tier_recommend`
  - Plugs into Claude Desktop / Claude Code / Cursor / Cline through
    standard `claude_desktop_config.json` `mcpServers` block.
  7 in-process tests + stdio smoke verified end-to-end.

### Notes
- Day-0 footprint v0.0.9 stays intact; v0.0.10 adds **3 user-facing
  crates** described in 《04 架构设计文档》§14 + 《12 AI 集成》but not in the
  Day-0 spec §3 list. They are leaf modules — no Day-0 crate depends
  on them.
- 21 crates total; 54 test groups workspace-wide.
- aii-mcp is the differentiating "AI-native chain" capability called
  out in CLAUDE.md.

## [0.0.9] — 2026-05-24

### Added — Day-0 completion (18 of 18 crates) 🎉
- `aii-state::mpt_root` — **full** Modified Merkle Patricia Tree
  algorithm (hex-prefix encoding + leaf / extension / branch nodes +
  RLP-length pruning per Yellow Paper Appendix D). 11 unit tests
  covering empty / single / multi-key / extension-merge / branch-split
  / 100-key stress. The v0.0.6 `unimplemented!()` placeholder is gone.
- `aii-evm` (M1 #7, scoped) — `execute_transfer` runs value-transfer
  txs against `StateDb`: nonce + balance check, debit, credit, nonce
  bump, returns `Receipt`. EOA→EOA only; contract paths return
  `ExecError::ContractCallsNotYetSupported` until the `revm`
  integration lands. 6 unit tests (happy path, nonce mismatch,
  insufficient balance, CREATE rejection, contract-recipient
  rejection, nonce atomicity).
- `aii-net-p2p` (M1 #8, scoped) — TCP listener + dial + length-prefixed
  RLP frame codec (`u32` BE prefix, ≤ 1 MiB). `Message::{Hello, Ping,
  Pong, Disconnect}`. 6 tests including two real-TCP end-to-end
  exchanges (Hello / Ping-Pong). Full devp2p discovery + RLPx lands
  in a later release.
- `aii-net-sync` (M1 #9) — pure state-machine `SyncEngine`
  (`Idle → Headers → Bodies → Done`) consuming `Event`s and emitting
  `Action`s. Contiguity / hash-order validation. 8 tests covering
  all transitions + error paths.

### Changed
- Workspace version 0.0.8 → 0.0.9.

### Notes — **Day-0 footprint complete (18 of 18 crates)**
- M0 ×4 — aii-types / aii-codec / aii-crypto / aii-storage
- M1 ×5 — aii-block / aii-state (full MPT) / aii-evm / aii-net-p2p
  / aii-net-sync
- M2 ×9 — aii-consensus-iface / aii-microchain / aii-net-txpool /
  aii-rpc / aii-wallet / aii-vnode / aii-config / aii-metrics
  / aii-node (+ `aiid` binary)
- 44 test groups; ~340 tests pass under
  `cargo test --workspace --all-features`.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  clean. `cargo doc --workspace` clean.
- aiid binary verified live: RocksDB opens, RPC listens, eth_chainId
  returns `"0x63"`, aii_status reports `{"chain_id":99,…}`.
- M2 *Day-1+* extension crates (aii-mcp / aii-wasm / aii-consensus-
  plugins / aii-crosschain / aii-bindings / aii-onboarding /
  full aii-consensus-bft engine) intentionally remain post-Day-0
  per spec §3.4. Day-0 footprint is now **frozen**.

## [0.0.8] — 2026-05-24

### Added — RPC + node binary
- `aii-rpc` (M2 #15) — `jsonrpsee` HTTP server with `eth_chainId`,
  `eth_blockNumber`, and `aii_status` methods. `RpcState` trait keeps
  the crate state-free; embedders provide chain id / head / network.
  3 in-process end-to-end tests.
- `aii-node` (M2 #19) — library `NodeState` + binary **`aiid`**.
  `aiid` opens RocksDB at `--data-dir`, serves RPC at `--rpc`, and
  waits for SIGINT. 3 library tests + verified live-binary smoke:
  ```
  $ target/debug/aiid --data-dir /tmp/aiid-smoke --rpc 127.0.0.1:18545 &
  $ curl … aii_status   → {"chain_id":99,"network":"aii-mainnet",…}
  $ curl … eth_chainId  → "0x63"
  ```

### Changed
- Workspace version 0.0.7 → 0.0.8.

### Notes
- Day-0 footprint progress: **15 of 18 crates landed**. Remaining:
  `aii-evm` (revm wrapper), `aii-net-p2p` (devp2p), `aii-net-sync`
  (each is multi-week work and not attemptable as a one-PR scaffold).
- Day-0 binary `aiid` ships and serves real JSON-RPC over HTTP — this
  is the first user-facing deployment artefact in the workspace.

## [0.0.7] — 2026-05-24

### Added — 7 new crates (M2 leaves)
- `aii-config` (M2 #17) — `ChainSpec` (chain id 99 default) + `Genesis`
  (alloc + `to_header(state_root)`). 12 tests.
- `aii-consensus-iface` (M2 #10) — trait-only crate: `Engine`, `Proposer`,
  `Voter`, `Validation`, `ConsensusError`, `EngineProgress`, `Vote`. 4 tests.
- `aii-metrics` (M2 #18) — lock-protected counter/gauge registry +
  Prometheus text render. 6 tests.
- `aii-wallet` (M2 #16) — `LocalWallet` (in-memory secp256k1 + `Address`
  derivation + `sign_message_hash`). 5 tests. Encrypted keystore +
  BIP-39 land later.
- `aii-vnode` (M2 #12) — `VNode` / `VSet` with 100,000 AII stake floor
  + 80/20 reward split (`split_reward`). 11 tests.
- `aii-net-txpool` (M2 #14) — capacity-bounded mempool keyed by
  `(sender, nonce)`; price-replacement; lowest-gas-first eviction.
  `effective_gas_price` helper. 8 tests.
- `aii-microchain` (M2 #13) — `MicroChainId`/`MicroChainSpec` registry
  + `FlushAnchor` bookkeeping. 8 tests.

### Changed
- `aii-types::Address` + `H256`: derive `PartialOrd` + `Ord` (needed for
  BTreeMap keys in vnode / txpool / microchain).
- Workspace version 0.0.6 → 0.0.7.

### Notes
- M0 (4 crates) + M1 (2 crates: block, state) + M2 (7 crates) =
  **13 of 18 Day-0 crates landed**. Remaining Day-0:
  `aii-evm`, `aii-net-p2p`, `aii-net-sync`, `aii-consensus-bft`,
  `aii-rpc`, `aii-node` (binary).
- All 54 new tests passing; workspace clippy clean under `-D warnings`.
- Tags: v0.0.5, v0.0.6, v0.0.7 (local only — push pending remote setup).

## [0.0.6] — 2026-05-24

### Added
- New crate `aii-state` (M1 #5 — narrow scope):
  - `Account` — 4-field RLP `[nonce, balance, storage_root, code_hash]`,
    `Hashable` impl, `Account::EMPTY` constant for fresh EOAs.
  - `EMPTY_CODE_HASH` constant (= `keccak256(b"")`).
  - `StateDb<B: KvBackend>` — `Address → Account` store keyed by
    `keccak256(address)` in `ColumnFamily::State`; `account` / `set_account`
    / `remove_account` methods.
  - `mpt_root` placeholder — empty input returns `EMPTY_TRIE_HASH`;
    non-empty input panics until v0.0.7 lands the full Merkle Patricia
    Tree algorithm.
- 12 unit tests across `account` / `trie` / `db` modules.

### Changed
- Workspace version 0.0.5 → 0.0.6.

### Notes
- Full MPT (hex-prefix + branch / extension / leaf nodes + RLP-pruning)
  is deferred to v0.0.7 to keep this PR reviewable.
- This unblocks `aii-evm` (which needs `Account` and `StateDb` more than
  it needs trie roots — root computation happens at block-commit time).

## [0.0.5] — 2026-05-24

### Added
- New crate `aii-block` (M1 #6 — first M1 crate):
  - `Header` — 20-field EIP-1559 + 4895 + 4844 + 4788 layout with
    forward/back-compatible trailing fields (`blob_gas_used`,
    `excess_blob_gas`, `parent_beacon_block_root` are `Option`).
  - `Tx` enum — EIP-2718 envelope (Legacy / EIP-1559 / EIP-4844
    placeholder). All variants carry an optional `AlgoId` extension
    that defaults to `Secp256k1` and emits byte-perfect Ethereum
    encodings in that case (PQ slots are additive and read by trailing-
    item detection during decode).
  - `Receipt` — single struct + `TxType` discriminator + EIP-2718
    envelope, with helpers `encode_2718` / `decode_2718`.
  - `Block` = `Header` + `BlockBody { transactions, ommers, withdrawals }`;
    `Block::hash()` ≡ `Header::hash()`.
  - `Bloom` (2048-bit Yellow-Paper §4.4.2 accrue/contains), `Log`,
    `Withdrawal` (EIP-4895, Gwei), `AccessListItem` (EIP-2930),
    `Hashable` trait.
  - Constants: `EMPTY_LIST_HASH`, `EMPTY_TRIE_HASH` (Keccak-verified at
    test time).
- 32 unit tests + 5 proptest properties + 10-header byte-perfect
  fixture round-trip with hash self-consistency.

### Changed
- Workspace version 0.0.4 → 0.0.5.
- `aii-types`: `impl alloy_rlp::{Encodable, Decodable}` for `H256` and
  `Address` (unlocks `#[derive(RlpEncodable, RlpDecodable)]` for
  downstream crates' simple structs without orphan-rule contortions).
- `alloy-rlp` workspace dep gains the `derive` feature.
- Workspace clippy config: list of explicit pedantic/nursery sub-lint
  allows (errors-doc / panics-doc / must-use-candidate / doc-markdown /
  numeric-cast family / many-single-char-names / match-same-arms /
  ref-option / option-if-let-else / format-push-string) — matches the
  documented "pedantic = warn" intent under CI's `-- -D warnings` flag.

### Notes
- Per spec §5.3, `aii-block` is **not** published to crates.io until M2.
- Mainnet fixtures in v0.0.5 are synthetic but byte-perfect through
  the encoder; an M1 follow-up swaps in genuine mainnet RLP without
  any API change.
- This unblocks M1 crates `aii-state` and `aii-evm` (both depend on
  `aii-block`).

## [0.0.4] — 2026-05-24

### Added
- New crate `aii-storage` (M0 #4 — final basestone crate):
  - `KvBackend` trait (sync get/put/delete/write/snapshot/iter/iter_prefix)
    and `Snapshot` trait (read-only consistent view).
  - `ColumnFamily` closed enum: 10 variants covering headers / bodies /
    receipts / transactions / state / account_storage / tx_lookup / meta
    / microchain / default. Stable snake_case wire names; adding a
    variant requires a spec revision.
  - `WriteBatch` backend-agnostic op log; cross-CF atomic on commit.
  - `RocksDbBackend` (default feature `rocksdb`) — wraps rocksdb 0.22
    with lz4 compression, opens every CF via `ColumnFamily::ALL`.
  - `MemoryBackend` (always on) — `Arc<RwLock<HashMap<CF, BTreeMap>>>`
    for downstream-crate unit tests; snapshot via `Arc` clone.
  - `StorageError` umbrella (`Backend` / `InvalidColumnFamily` / `Io`).
- 8-test conformance suite parametrised over both backends (16 runs);
  2 property tests (Op-sequence equivalence + snapshot isolation);
  criterion benchmark meeting the M0 >=50k op/s sequential-write gate;
  `scripts/check_storage_perf.sh` CI helper.
- Workspace deps: `rocksdb 0.22`, `tempfile 3`, `criterion 0.5`.

### Changed
- Workspace version 0.0.3 → 0.0.4.

### Notes
- All four M0 basestone crates are now landed (types / codec / crypto /
  storage). M1 (state / EVM / block / net-*) begins next.
- Per spec §5.3, `aii-storage` is **not** published to crates.io until M2.

## [0.0.3] — 2026-05-24

### Added
- New crate `aii-crypto` with the four Day-0 cryptographic primitives:
  - `keccak::keccak256` — Ethereum-style Keccak-256, 3 KAT vectors
    (empty / "abc" / 1M 'a').
  - `secp::{sign, verify, recover}` — secp256k1 ECDSA with 65-byte ETH
    layout (`r ‖ s ‖ v`); `PublicKey::address` matches the known
    constant for `sk = 1`.
  - `bls::{sign, verify, fast_aggregate_verify, aggregate_signatures,
    aggregate_pubkeys}` — BLS12-381 Eth2 `min-pk` scheme over blst.
  - `vrf::{prove, verify}` — Schnorrkel VRF over Ristretto-25519 with
    96-byte wire form (pre-output ‖ proof).
  - `CryptoError` umbrella (`InvalidEncoding` + `BadSignature`).
- 31 unit tests + 5 property tests covering all four primitives.
- Workspace deps: `tiny-keccak 2`, `k256 0.13`, `blst 0.3`,
  `schnorrkel 0.11`, `merlin 3`, `rand_core 0.6`.

### Changed
- Workspace version 0.0.2 → 0.0.3.
- Rust toolchain pin 1.83 → 1.94.1; workspace `rust-version` 1.83 → 1.85
  (ecosystem moved to edition2024 via getrandom 0.4 / indexmap 2.14 /
  ruint 1.18).

### Notes
- Spec D7 (PQ algorithm slots) remains placeholder-only for v0.0.3;
  concrete verifiers will land in `aii-registry` (planned v0.0.4) so
  that `AlgoId`-keyed dispatch is the only call site.
- `aii-crypto` is the 3rd of the 4 M0 basestone crates; remaining M0
  work is `aii-storage` (RocksDB).

## [0.0.2] — 2026-05-22

### Added
- New crate `aii-codec` with RLP / SSZ / JSON-RPC hex codecs.
  - RLP impls for `H256`, `Address`, `AlgoId`, `SignedTx`.
  - SSZ impls for `H256`, `Address`, `AlgoId`, `BlsPubKey`, `BlsSignature`, `SignedTx`.
  - ETH JSON-RPC hex helpers (`bytes_hex` / `quantity` / `hex_h256` / `hex_address` serde modules).
  - Local `SszError` (insulates from ssz_rs non-exhaustive-enum drift).
  - `CodecError` umbrella with `From` for `alloy_rlp::Error` / `SszError` / `serde_json::Error` / `HexError`.
  - 52 unit tests + 11 property tests.
- Workspace deps: `alloy-rlp 0.3`, `ssz_rs 0.9`, `serde_json 1`, `hex 0.4`.

### Changed
- Workspace version 0.0.1 → 0.0.2.

## [0.0.1] — 2026-05-21

### Added
- Workspace bootstrap (Cargo.toml, CI, lints)
- `aii-types` crate with primitive types (H256, Address, U256, AlgoId, BlsPubKey, BlsSignature, SignedTx)
- GitHub Actions CI: fmt + clippy + test + deny + audit + llvm-cov on Linux/macOS
- AlgoId enum reserves Day-0 PQ algorithm slots per spec D7

### Notes
- This is the first commit of `aii-core`. All downstream crates (state, EVM, consensus, ...) depend on `aii-types`.
- See spec `docs/superpowers/specs/2026-05-21-aii-core-design.md` §3 for the full 24-crate plan.
