# AII — AI-Native L1 Public Chain

> **AII** is the first AI-native public blockchain — protocol-level MCP Server + CLI (`aii`) make AI agents (Claude Desktop / Claude Code / Cursor / Cline) zero-SDK first-class citizens.

Pure PoS BFT consensus. Pre-registration fair launch (10,000 addresses × 1,000,000 AII). Total supply **210 billion AII**. No company, no foundation, no DAO governance.

Spec: see `docs/superpowers/specs/2026-05-21-aii-core-design.md` and the 14-document reference set in the original docs repo.

## Status

**v0.0.1 — Workspace bootstrap + `aii-types` primitive types.**

Workspace skeleton is live; downstream crates (consensus/state/EVM/...) coming in subsequent plans.

## Quickstart (developers)

```bash
git clone https://github.com/AII-Network/aii.git
cd aii
cargo test --workspace
```

## License

MIT — see `LICENSE`.
