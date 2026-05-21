# Changelog

All notable changes to AII workspace follow [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.1] — 2026-05-21

### Added
- Workspace bootstrap (Cargo.toml, CI, lints)
- `aii-types` crate with primitive types (H256, Address, U256, AlgoId, BlsPubKey, BlsSignature, SignedTx)
- GitHub Actions CI: fmt + clippy + test + deny + audit + llvm-cov on Linux/macOS
- AlgoId enum reserves Day-0 PQ algorithm slots per spec D7

### Notes
- This is the first commit of `aii-core`. All downstream crates (state, EVM, consensus, ...) depend on `aii-types`.
- See spec `docs/superpowers/specs/2026-05-21-aii-core-design.md` §3 for the full 24-crate plan.
