# AII — AI-Native L1 Public Chain

> **AII** is the first AI-native public blockchain — protocol-level MCP Server + CLI (`aii`) make AI agents (Claude Desktop / Claude Code / Cursor / Cline) zero-SDK first-class citizens.

Pure PoS BFT consensus. Pre-registration fair launch (10,000 addresses × 1,000,000 AII). Total supply **210 billion AII**. No company, no foundation, no DAO governance.

Spec: see `docs/superpowers/specs/2026-05-21-aii-core-design.md` and the 14-document reference set in the original docs repo.

## Status

**Post-v0.0.92 development branch — live public testnet, mainnet not launched yet.**

| | |
|---|---|
| Chain ID | `9999` (aii-testnet) |
| Explorer | https://aii.allfund.xyz/ |
| Validators | 2 (JP + CN), TCP gossip on port 30311 |
| Consensus | BFT-PoS — VRF leader election + ⅔ BLS aggregate finality |
| Block time | ~1–2 s |
| RPC surface | EVM-compatible `eth_*` reads/writes plus AII explorer RPC |
| Live validation | 300 accepted real AII transfers, 0.1–50 AII each, with receipts |
| Mainnet status | Not launched. Phase 2 items remain: feature freeze, genesis dry run, ≥21 validator readiness, audit closure, 14-day parameter objection window. |

### Public RPC endpoints

| URL | Notes |
|---|---|
| `https://aii.allfund.xyz/api` | HTTPS, Let's Encrypt, reverse-proxied to JP |
| `http://8.211.135.234:8545` | JP node direct (Aliyun Tokyo) |
| `http://106.14.223.128:8545` | CN node direct (Aliyun Shanghai) |

Wallets (MetaMask etc.) can use any of these as the RPC URL with chain id `9999`.

### Testnet topology

```
                ┌──────────────────┐
   ╔══DNS══════►│ aii.allfund.xyz  │  HTTPS + static explorer
   ║            │  nginx (Ubuntu)  │  /api → 127.0.0.1:8545
   ║            └────────┬─────────┘
   ║                     │
   ║                     ▼
┌──╨─────────────┐                   ┌────────────────────┐
│ JP 8.211.135.. │◄── TCP :30311 ───►│ CN 106.14.223..    │
│ aiid (native)  │   BftMessage      │ aiid (docker)      │
│ Ubuntu 24.04   │   gossip          │ CentOS 7 + ubuntu  │
│ validator #0   │                   │ validator #1       │
└────────────────┘                   └────────────────────┘
```

Both nodes finalise the same block at every height — verified by hash agreement on `aii_getBlockHeader` queries. If one node drops, the other halts on quorum (BFT-safe) and resumes after reconnect.

### Release lineage

| Tag | Highlight |
|---|---|
| `v0.0.34` | Multi-host BFT over TCP gossip |
| `v0.0.35` | Pluggable consensus — BFT + PoA engines |
| `v0.0.36` | Block explorer API (`aii_getBlockHeader`, `aii_recentBlocks`) + MCP tools + live public-testnet deployment |
| `v0.0.88`–`v0.0.90` | Wallet-facing EVM RPC: `eth_call`, `eth_estimateGas`, transaction lookup, receipts, block lookup |
| `v0.0.91`–`v0.0.92` | Public-internet discovery, validator registry wiring, capacity budgeting, and BFT liveness hardening |

## Quickstart (developers)

```bash
git clone https://github.com/kinglovesdao/aii.git
cd aii
cargo test --workspace
cargo build --release -p aii-node -p aii-cli

# Talk to the live testnet:
./target/release/aii --rpc https://aii.allfund.xyz/api status
./target/release/aii --rpc https://aii.allfund.xyz/api recent --limit 5
```

## License

MIT — see `LICENSE`.
