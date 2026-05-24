# Changelog

All notable changes to AII workspace follow [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
