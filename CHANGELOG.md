# Changelog

All notable changes to AII workspace follow [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
