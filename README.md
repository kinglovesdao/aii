<div align="center">

```
   █████╗ ██╗██╗
  ██╔══██╗██║██║
  ███████║██║██║
  ██╔══██║██║██║
  ██║  ██║██║██║
  ╚═╝  ╚═╝╚═╝╚═╝
```

# AII — AI-Native L1 Public Chain

**The first blockchain where AI agents are protocol-level first-class citizens.**

[![Version](https://img.shields.io/badge/version-v0.0.93-blue?style=for-the-badge&logo=rust&logoColor=white)](https://github.com/kinglovesdao/aii/releases)
[![License](https://img.shields.io/badge/license-MIT-green?style=for-the-badge)](LICENSE)
[![Chain ID](https://img.shields.io/badge/chain--id-9999-purple?style=for-the-badge&logo=ethereum&logoColor=white)](https://aii.allfund.xyz/)
[![Testnet](https://img.shields.io/badge/testnet-LIVE-brightgreen?style=for-the-badge&logo=statuspage&logoColor=white)](https://aii.allfund.xyz/)
[![Rust](https://img.shields.io/badge/built%20with-Rust-orange?style=for-the-badge&logo=rust&logoColor=white)](https://rustup.rs/)
[![BFT-PoS](https://img.shields.io/badge/consensus-BFT--PoS-red?style=for-the-badge)](https://github.com/kinglovesdao/aii)

[![Stars](https://img.shields.io/github/stars/kinglovesdao/aii?style=social)](https://github.com/kinglovesdao/aii/stargazers)
[![Forks](https://img.shields.io/github/forks/kinglovesdao/aii?style=social)](https://github.com/kinglovesdao/aii/network/members)
[![Issues](https://img.shields.io/github/issues/kinglovesdao/aii?color=red)](https://github.com/kinglovesdao/aii/issues)
[![Last Commit](https://img.shields.io/github/last-commit/kinglovesdao/aii?color=blue)](https://github.com/kinglovesdao/aii/commits)

</div>

---

## 📑 Table of Contents

- [✨ Highlights](#-highlights)
- [🏗️ Architecture](#️-architecture)
- [📦 Installation](#-installation)
- [⚙️ Configuration](#️-configuration)
- [💻 Usage](#-usage)
- [🌐 Live Testnet](#-live-testnet)
- [🗺️ Roadmap](#️-roadmap)
- [📊 GitHub Stats](#-github-stats)
- [🌍 Multilingual Docs](#-multilingual-docs)
- [🤝 Contributing](#-contributing)
- [📄 License](#-license)

---

## ✨ Highlights

> **AII** is a pure-Rust Layer-1 blockchain purpose-built for the AI era. Protocol-level MCP Server and CLI make AI agents zero-SDK first-class citizens — no wrappers, no bridges, just native blockchain access.

| Feature | Description |
|---------|-------------|
| 🤖 **AI-Native** | Built-in MCP Server — Claude Desktop / Cursor / Cline get wallet + chain queries natively |
| ⚡ **BFT-PoS Consensus** | VRF leader election + ⅔ BLS aggregate finality, single-block instant finality |
| 🔗 **EVM Compatible** | Full `eth_*` JSON-RPC surface — MetaMask, ethers.js, viem work out of the box |
| 🌐 **Discovery v4** | devp2p UDP Discovery v4, NAT traversal, zero-config peer finding |
| 🦀 **Pure Rust** | 27-crate Cargo workspace, zero unsafe, `clippy::all + pedantic + nursery` |
| 📱 **Mobile Ready** | ARM64 cross-compile verified — 14 MB binary, 13 MB RSS on Android |
| 🔒 **Fair Launch** | 10,000 pre-registered addresses × 1,000,000 AII · No company · No VC · No foundation |
| 🔄 **Self-Healing** | BFT block-sync over gossip — validators restart without manual intervention |

---

## 🏗️ Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                        AII Node (aiid)                          │
│                                                                 │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────────┐   │
│  │ JSON-RPC │  │ BFT-PoS  │  │ Discovery│  │  MCP Server  │   │
│  │eth_* api │  │ Engine   │  │  v4 UDP  │  │  (AI agents) │   │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘  └──────┬───────┘   │
│       │             │              │                │           │
│  ┌────▼─────────────▼──────────────▼────────────────▼──────┐   │
│  │           NodeState · TxPool · BlockStore               │   │
│  └─────────────────────────┬───────────────────────────────┘   │
│                             │                                   │
│  ┌──────────────────────────▼──────────────────────────────┐   │
│  │           RocksDB · EVM (revm) · State MPT              │   │
│  └─────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
```

**Live Testnet Topology:**

```
                ┌──────────────────┐
   ╔══DNS══════►│ aii.allfund.xyz  │  HTTPS · Let's Encrypt
   ║            │  nginx (Ubuntu)  │  /api → 127.0.0.1:8545
   ║            └────────┬─────────┘
   ║                     │
   ║                     ▼
┌──╨─────────────┐                   ┌────────────────────┐
│ JP 8.211.135.. │◄── TCP :30311 ───►│ CN 106.14.223..    │
│ aiid v0.0.93   │   BftMessage      │ aiid v0.0.93       │
│ Ubuntu 24.04   │   gossip + sync   │ Docker ubuntu:24   │
│ validator #0   │                   │ validator #1       │
└────────────────┘                   └────────────────────┘
```

---

## 📦 Installation

### Prerequisites

> Requires **Rust 1.85+** (toolchain pinned to stable). RocksDB builds automatically via `librocksdb-sys`.

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
rustup update stable
```

### Build from Source

```bash
# Clone repository
git clone https://github.com/kinglovesdao/aii.git
cd aii

# Run full test suite (~945 tests, ~60 s)
cargo test --workspace

# Build release binaries
cargo build --release -p aii-node -p aii-cli -p aii-mcp

# Verify
./target/release/aiid --version   # aiid 0.0.93
./target/release/aii  --version   # aii  0.0.93
```

### Pre-built Binaries

> Download the latest release for your platform from [Releases](https://github.com/kinglovesdao/aii/releases).

| Platform | Binary | Notes |
|----------|--------|-------|
| Linux x86-64 | `aiid` | glibc 2.29+ |
| Linux ARM64  | `aiid` | aarch64-linux-gnu, Raspberry Pi / SBC |
| Android ARM64 | `aiid` | Tested on OnePlus 6 (SDM845), 13 MB RSS |

---

## ⚙️ Configuration

### Validator Node (BFT-PoS)

```bash
# Generate validator keypair
./aii validator keygen --out /var/lib/aiid/node.json

# Create genesis (multi-validator)
./aii genesis init \
  --network testnet \
  --validator-pubkey /var/lib/aiid/node-0-pub.json \
  --validator-pubkey /var/lib/aiid/node-1-pub.json \
  --out /var/lib/aiid/genesis.json

# Start validator
./aiid \
  --data-dir /var/lib/aiid/data \
  --rpc 0.0.0.0:8545 \
  --bft \
  --genesis /var/lib/aiid/genesis.json \
  --keystore /var/lib/aiid/node.json \
  --peers <peer-ip>:30311 \
  --discovery-seeds <seed-ip>:30310 \
  --coinbase 0xYOUR_ADDRESS \
  --slot-seconds 1
```

### Observer / RPC Node

```bash
./aiid \
  --data-dir /var/lib/aiid/data \
  --rpc 0.0.0.0:8545 \
  --produce-blocks false \
  --bootnode http://<validator-ip>:8545
```

### MetaMask / Wallet Configuration

| Field | Value |
|-------|-------|
| Network Name | AII Testnet |
| RPC URL | `https://aii.allfund.xyz/api` |
| Chain ID | `9999` |
| Currency Symbol | `AII` |
| Block Explorer | `https://aii.allfund.xyz/` |

---

## 💻 Usage

### Query the Live Testnet

```bash
# Network status
./aii --rpc https://aii.allfund.xyz/api status

# Recent blocks
./aii --rpc https://aii.allfund.xyz/api recent --limit 10

# Account balance
./aii --rpc https://aii.allfund.xyz/api account show \
  --address 0xYOUR_ADDRESS

# Send a transfer
./aii --rpc https://aii.allfund.xyz/api transfer \
  --key-file wallet.hex \
  --to 0xRECIPIENT \
  --amount-aii 1.0 \
  --chain-id 9999
```

### Stress Test

```bash
# Flood the testnet with 10,000 real signed transfers
./aii live-transfer-load \
  --rpc https://aii.allfund.xyz/api \
  --chain-id 9999 \
  --key-file key1.hex --key-file key2.hex \
  --key-file key3.hex --key-file key4.hex \
  --total 10000 --json
```

### AI Agent Integration (MCP)

```json
// Claude Desktop config (~/.claude/claude_desktop_config.json)
{
  "mcpServers": {
    "aii": {
      "command": "/path/to/aii-mcp",
      "args": ["--rpc", "https://aii.allfund.xyz/api"]
    }
  }
}
```

```
# Now Claude can natively:
> "What is the current block number on AII testnet?"
> "Send 5 AII to 0xABCD... from my wallet"
> "Show me the last 10 transactions"
```

### Hardware Tier Detection

```bash
# Probe your hardware and get a validator tier recommendation
./aii tier --json
```

---

## 🌐 Live Testnet

| Endpoint | URL | Region |
|----------|-----|--------|
| 🌍 HTTPS API | `https://aii.allfund.xyz/api` | Global (CDN) |
| 🗾 JP Direct | `http://8.211.135.234:8545` | Tokyo, Japan |
| 🇨🇳 CN Direct | `http://106.14.223.128:8545` | Shanghai, China |
| 🔭 Explorer | `https://aii.allfund.xyz/` | Web UI |

**Quick connectivity check:**

```bash
curl -s -X POST https://aii.allfund.xyz/api \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}' \
  | python3 -m json.tool
```

---

## 🗺️ Roadmap

| Phase | Status | Milestone |
|-------|--------|-----------|
| Phase 0 · Alpha | ✅ Done | Docs, core framework, CI |
| Phase 1 · Beta | 🔄 In Progress | Public testnet live, BFT-PoS, EVM RPC, MCP |
| Phase 2 · Main | ⏳ Pending | Feature freeze, ≥21 validators, audit, genesis dry-run |
| Phase 3 · Launch | 🔜 Planned | Mainnet TGE, sub-chain protocol |
| Phase 4 · Ecosystem | 🔜 Planned | Cross-chain, DEX sub-chain, hackathon |
| Phase 5 · Scale | 🔜 Planned | WASM VM, IPFS storage sub-chains |

---

## 📊 GitHub Stats

<div align="center">

### 👨‍💻 Contributor Activity

[![GitHub Stats](https://github-readme-stats.vercel.app/api?username=kinglovesdao&show_icons=true&theme=tokyonight&hide_border=true&include_all_commits=true&count_private=true&title_color=00b4d8&icon_color=00b4d8&text_color=c9d1d9&bg_color=0d1117)](https://github.com/kinglovesdao)

### 🔥 Contribution Streak

[![GitHub Streak](https://streak-stats.demolab.com?user=kinglovesdao&theme=tokyonight&hide_border=true&background=0D1117&stroke=00b4d8&ring=00b4d8&fire=ff6b35&currStreakLabel=00b4d8)](https://github.com/kinglovesdao)

### 🌐 Top Languages (10 Languages)

[![Top Langs](https://github-readme-stats.vercel.app/api/top-langs/?username=kinglovesdao&layout=compact&theme=tokyonight&hide_border=true&langs_count=10&bg_color=0d1117&title_color=00b4d8&text_color=c9d1d9)](https://github.com/kinglovesdao)

### 📈 Commit Activity

[![Activity Graph](https://github-readme-activity-graph.vercel.app/graph?username=kinglovesdao&theme=tokyo-night&hide_border=true&bg_color=0d1117&color=00b4d8&line=00b4d8&point=ff6b35)](https://github.com/kinglovesdao)

</div>

---

## 🤖 AI Integration Guide

> **Full walkthrough: how to operate AII with Claude, Cursor, Cline and any MCP-compatible AI agent.**

[![AI Guide](https://img.shields.io/badge/📖_Read-AI_Integration_Guide-blueviolet?style=for-the-badge)](docs/ai-guide.md)

| What you'll learn | Link |
|---|---|
| MCP setup for Claude Desktop / Cursor / Cline | [Quick Setup](docs/ai-guide.md#-quick-setup) |
| All 12 MCP tools with example responses | [Tools Reference](docs/ai-guide.md#-mcp-tools-reference) |
| Step-by-step AI walkthroughs (wallet, blocks, BFT planning) | [Walkthroughs](docs/ai-guide.md#-ai-operation-walkthroughs) |
| Copy-paste prompt library | [Prompts](docs/ai-guide.md#-practical-prompts-library) |
| Security model for AI key operations | [Security](docs/ai-guide.md#-security-model) |

---

## 🌍 Multilingual Docs

> Full installation and configuration guide available in 10 languages — see **[INSTALL.md](INSTALL.md)**

| Language | Link |
|----------|------|
| 🇺🇸 English | [INSTALL.md#english](INSTALL.md#-english) |
| 🇨🇳 简体中文 | [INSTALL.md#简体中文](INSTALL.md#-简体中文) |
| 🇯🇵 日本語 | [INSTALL.md#日本語](INSTALL.md#-日本語) |
| 🇰🇷 한국어 | [INSTALL.md#한국어](INSTALL.md#-한국어) |
| 🇷🇺 Русский | [INSTALL.md#русский](INSTALL.md#-русский) |
| 🇩🇪 Deutsch | [INSTALL.md#deutsch](INSTALL.md#-deutsch) |
| 🇫🇷 Français | [INSTALL.md#français](INSTALL.md#-français) |
| 🇧🇷 Português | [INSTALL.md#português](INSTALL.md#-português) |
| 🇮🇳 हिन्दी | [INSTALL.md#हिन्दी](INSTALL.md#-हिन्दी) |
| 🇸🇦 العربية | [INSTALL.md#العربية](INSTALL.md#-العربية) |

---

## 🤝 Contributing

> AII is a fair-launch, community-driven protocol. No company, no VC, no foundation — contributions are welcome from everyone.

```bash
# Fork → branch → commit → PR
git checkout -b feat/your-feature
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git commit -m "feat: describe your change"
```

Please read the architecture overview in [`CLAUDE.md`](CLAUDE.md) before contributing. All changes must pass:
- ✅ `cargo test --workspace` (zero failures)
- ✅ `cargo clippy -- -D warnings` (zero warnings)
- ✅ `cargo fmt --all` (formatted)

---

## 📄 License

```
MIT License — Copyright (c) 2026 AII Network contributors
```

This project is open source under the [MIT License](LICENSE).  
Free to use, modify, and distribute with attribution.

---

<div align="center">

**Built with ❤️ in pure Rust · No company · No foundation · Fair launch**

[![Explorer](https://img.shields.io/badge/🔭_Explorer-aii.allfund.xyz-blue?style=for-the-badge)](https://aii.allfund.xyz/)
[![Chain ID](https://img.shields.io/badge/Chain_ID-9999-purple?style=for-the-badge)](https://aii.allfund.xyz/)

*Join the testnet · Run a validator · Build on AII*

</div>
