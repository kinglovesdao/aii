<div align="center">

# 🤖 AI × AII — Complete Guide

### *The first blockchain where AI agents are protocol-level first-class citizens*

[![MCP](https://img.shields.io/badge/MCP-Model_Context_Protocol-blueviolet?style=for-the-badge&logo=anthropic&logoColor=white)](https://modelcontextprotocol.io/)
[![Claude](https://img.shields.io/badge/Claude_Desktop-Supported-orange?style=for-the-badge&logo=anthropic&logoColor=white)](https://claude.ai/)
[![Cursor](https://img.shields.io/badge/Cursor-Supported-blue?style=for-the-badge)](https://cursor.sh/)
[![Cline](https://img.shields.io/badge/Cline-Supported-green?style=for-the-badge)](https://github.com/cline/cline)

</div>

---

## 📑 Contents

- [Why AI-Native?](#-why-ai-native)
- [Architecture](#-architecture)
- [Quick Setup](#-quick-setup)
  - [Claude Desktop](#claude-desktop)
  - [Claude Code (CLI)](#claude-code-cli)
  - [Cursor / Cline / Other MCP Clients](#cursor--cline--other-mcp-clients)
- [MCP Tools Reference](#-mcp-tools-reference)
- [AI Operation Walkthroughs](#-ai-operation-walkthroughs)
  - [Check chain status](#1-check-chain-status)
  - [Create wallet](#2-create-wallet)
  - [Query blocks](#3-query-blocks)
  - [Hardware tier recommendation](#4-hardware-tier-recommendation)
  - [BFT capacity planning](#5-bft-capacity-planning)
  - [Discovery probe](#6-discovery-probe)
- [CLI Shell Agent Operations](#-cli-shell-agent-operations)
- [Practical Prompts Library](#-practical-prompts-library)
- [Security Model](#-security-model)
- [FAQ](#-faq)

---

## 🌟 Why AI-Native?

> **AII is designed so that an AI agent can operate the entire blockchain stack — from checking chain status to generating wallets and planning validator infrastructure — without writing a single line of SDK code.**

### The Problem with Traditional Blockchains and AI

| Barrier | Traditional Chain | AII |
|---------|------------------|-----|
| SDK learning curve | Agent must learn complex ABIs | **Zero — use CLI or MCP** |
| Error messages | Hex codes, stack traces | **Structured JSON, human-readable** |
| Key management | Raw private key passed to SDK | **Local signing, user confirms** |
| Network config | Complex RPC setup | **One URL, auto-detected** |
| AI documentation | Written for humans | **Prompt-tuned for LLMs** |

### Two Parallel Access Paths

```
  ┌─────────────────────────────────────────────────────────┐
  │                      AI Agent                           │
  └──────────────┬──────────────────────┬───────────────────┘
                 │                      │
         ┌───────▼──────┐      ┌────────▼─────────┐
         │  MCP Server  │      │  CLI (`aii`)      │
         │  (stdio/TCP) │      │  (shell tool)     │
         │              │      │                   │
         │  12 tools    │      │  all subcommands  │
         │  JSON-RPC    │      │  bash-compatible  │
         └───────┬──────┘      └────────┬──────────┘
                 │                      │
                 └──────────┬───────────┘
                            │
                   ┌────────▼────────┐
                   │  AII Node (RPC) │
                   │  testnet/mainnet│
                   └─────────────────┘
```

---

## 🏗️ Architecture

The `aii-mcp` binary is a **stdio MCP server** — it speaks the [Model Context Protocol](https://modelcontextprotocol.io/) over stdin/stdout. Any MCP-compatible client (Claude Desktop, Cursor, Cline, OpenDevin, custom agents) can connect to it as a tool provider.

```
┌──────────────────────────────────────────────────────────────┐
│                     aii-mcp binary                           │
│                                                              │
│  ┌─────────────────┐     ┌──────────────────────────────┐   │
│  │  MCP Transport  │────►│  Tool Dispatcher             │   │
│  │  (stdio/JSON)   │     │  chain_status | account_new  │   │
│  └─────────────────┘     │  block_lookup | recent_blocks│   │
│                          │  tier_recommend | bft_capacity│   │
│                          │  discovery_probe | mnemonic  │   │
│                          └──────────────┬───────────────┘   │
│                                         │                    │
│                          ┌──────────────▼───────────────┐   │
│                          │  HTTP JSON-RPC Client         │   │
│                          │  → https://aii.allfund.xyz/api│   │
│                          └──────────────────────────────┘   │
└──────────────────────────────────────────────────────────────┘
```

**Key design properties:**
- **Stateless** — each tool call is independent; no session state
- **Read-safe** — most tools are read-only; wallet generation is local (private key never leaves the process)
- **Configurable** — point `--rpc` at any AII node (testnet, local, mainnet)

---

## ⚡ Quick Setup

### Claude Desktop

**1. Build the MCP binary:**

```bash
git clone https://github.com/kinglovesdao/aii.git
cd aii
cargo build --release -p aii-mcp
```

**2. Add to Claude Desktop config:**

Edit `~/.claude/claude_desktop_config.json` (macOS/Linux) or `%APPDATA%\Claude\claude_desktop_config.json` (Windows):

```json
{
  "mcpServers": {
    "aii": {
      "command": "/absolute/path/to/aii/target/release/aii-mcp",
      "args": ["--rpc", "https://aii.allfund.xyz/api"]
    }
  }
}
```

**3. Restart Claude Desktop.** You'll see "aii" appear in the tools panel.

**4. Test it immediately:**

```
You: What is the current block number on AII?
Claude: [calls chain_status] The AII testnet is at block 670,000+, chain ID 9999 (aii-testnet).
```

---

### Claude Code (CLI)

Claude Code can use the MCP server via its configuration:

```bash
# Add to Claude Code's MCP config
claude mcp add aii /path/to/aii-mcp --rpc https://aii.allfund.xyz/api

# Or use the CLI directly as a bash tool — Claude Code can run:
./aii --rpc https://aii.allfund.xyz/api status
./aii --rpc https://aii.allfund.xyz/api recent --limit 5 --json
```

Claude Code can also run `aii` CLI commands directly in your terminal session, making the full command surface available without any MCP setup.

---

### Cursor / Cline / Other MCP Clients

Any client implementing MCP can connect. The general pattern:

```json
{
  "servers": {
    "aii-blockchain": {
      "command": "/path/to/aii-mcp",
      "args": ["--rpc", "https://aii.allfund.xyz/api"],
      "transport": "stdio"
    }
  }
}
```

**Supported clients:**
- ✅ Claude Desktop
- ✅ Claude Code
- ✅ Cursor (via `.cursor/mcp.json`)
- ✅ Cline (VSCode extension)
- ✅ Any stdio MCP client

---

## 🛠️ MCP Tools Reference

The `aii-mcp` server exposes **12 tools** organized in 4 categories:

### 🔗 Chain Information

| Tool | Description | Parameters |
|------|-------------|------------|
| `chain_status` | Returns chain ID, network name, head block number | — |
| `chain_id` | Returns only the EIP-155 chain ID (decimal) | — |
| `block_lookup` | Fetch a block header by number or hash | `query`: decimal / `0x`-hex number or hash |
| `recent_blocks` | Return N most-recent block headers (newest first) | `limit`: 1–100, default 10 |

**Example response — `chain_status`:**
```json
{
  "chain_id": 9999,
  "network": "aii-testnet",
  "head_block": 670234,
  "head_hash": "0xabcd...1234"
}
```

---

### 👛 Wallet & Key Management

| Tool | Description | Parameters |
|------|-------------|------------|
| `account_new` | Generate fresh secp256k1 address (key dropped) | — |
| `account_new_encrypted` | Generate key + return Web3 v3 encrypted keystore | `password`: string |
| `account_verify` | Verify a password decrypts a keystore | `keystore_json`: string, `password`: string |
| `mnemonic_new` | Generate BIP-39 mnemonic + first address | `words`: 12/15/18/21/24, default 12 |
| `account_from_mnemonic` | Derive address from BIP-39 phrase at index | `phrase`, `passphrase?`, `index?` |

> 🔒 **Privacy guarantee:** Private keys and mnemonics are **generated locally inside `aii-mcp`** and never sent over the network. The MCP server performs all cryptography in-process.

**Example — `mnemonic_new`:**
```json
{
  "mnemonic": "abandon ability able about above absent absorb abstract absurd abuse access accident",
  "address": "0x1234...abcd",
  "derivation_path": "m/44'/60'/0'/0/0"
}
```

---

### 📊 Infrastructure Planning

| Tool | Description | Parameters |
|------|-------------|------------|
| `tier_recommend` | Probe hardware, return validator tier T1–T7 | — |
| `bft_capacity` | Compute BFT committee capacity for 30 s finality | `validators?`: 1–128, `proposal_bytes?`, `target_secs?` |
| `discovery_probe` | Probe Discovery v4 seeds, report peers + public IP | `seeds?`, `listen?`, `bft_listen?`, `timeout_ms?` |

**Example — `bft_capacity` (21 validators, 30 s target):**
```json
{
  "validators": 21,
  "target_secs": 30,
  "equal_stake_quorum_votes": 15,
  "vote_messages_per_round": 840,
  "vote_payload_bytes_per_round": 121800,
  "leader_proposal_fanout_bytes": 335544320,
  "min_leader_upload_mbps": 89,
  "satisfies_design_cap": true
}
```

---

## 🎬 AI Operation Walkthroughs

### 1. Check Chain Status

**Prompt:**
```
What is the current status of the AII testnet? Show me the latest block.
```

**What happens internally:**
```
AI → calls chain_status → returns head block + chain ID
AI → calls recent_blocks(limit=1) → returns latest block details
AI → formats response for user
```

**Claude response example:**
> The AII testnet is currently running at block **670,234** (chain ID: 9999, network: `aii-testnet`). The latest block was produced about 1 second ago with hash `0xabcd...1234`.

---

### 2. Create Wallet

**Prompt:**
```
Create a new AII wallet for me with a 24-word recovery phrase.
```

**What happens internally:**
```
AI → calls mnemonic_new(words=24)
AI → receives mnemonic + address (key never leaves aii-mcp process)
AI → reminds user to back up securely
```

**Claude response example:**
> I've generated a new AII wallet:
> 
> **Address:** `0x1a2b...9f0e`
> 
> **Recovery Phrase (24 words):**
> `word1 word2 word3 ... word24`
> 
> ⚠️ **Write down your recovery phrase and store it offline. Never share it.**

---

### 3. Query Blocks

**Prompt:**
```
Show me the last 5 blocks on AII. What's the average block time?
```

**What happens internally:**
```
AI → calls recent_blocks(limit=5)
AI → calculates time differences between block timestamps
AI → computes average and presents summary
```

**Claude response example:**
> Here are the last 5 AII blocks:
> 
> | Block | Hash | Timestamp | Gap |
> |-------|------|-----------|-----|
> | 670,234 | 0xabcd... | 18:52:03 | — |
> | 670,233 | 0x1234... | 18:52:02 | 1.0 s |
> | 670,232 | 0xef01... | 18:52:01 | 1.1 s |
> | 670,231 | 0x5678... | 18:52:00 | 0.9 s |
> | 670,230 | 0x9abc... | 18:51:59 | 1.0 s |
> 
> **Average block time: ~1.0 seconds** — the BFT-PoS consensus is performing well within target.

---

### 4. Hardware Tier Recommendation

**Prompt:**
```
Can I run an AII validator on this machine? What tier am I?
```

**What happens internally:**
```
AI → calls tier_recommend
AI → aii-mcp probes: CPU cores, RAM, disk speed, network
AI → returns T1–T7 tier + recommendation
```

**Tier definitions:**

| Tier | Hardware Profile | Recommended Role |
|------|-----------------|------------------|
| T1 | 1 core, 512 MB RAM | Light observer |
| T2 | 2 cores, 1 GB RAM | Observer / follower node |
| T3 | 4 cores, 4 GB RAM | Archive node |
| T4 | 4 cores, 8 GB RAM | Full node |
| T5 | 8 cores, 16 GB RAM | **Validator candidate** |
| T6 | 16 cores, 32 GB RAM | High-performance validator |
| T7 | 32+ cores, 64+ GB RAM | Infrastructure/seed node |

---

### 5. BFT Capacity Planning

**Prompt:**
```
If AII has 128 validators, can it still achieve 30-second finality? What's the bandwidth requirement?
```

**What happens internally:**
```
AI → calls bft_capacity(validators=128, target_secs=30)
AI → receives full capacity budget calculation
AI → explains the math and practical implications
```

**Claude response example:**
> I ran the BFT capacity calculation for 128 validators with a 30-second finality target:
> 
> - **Quorum required:** 86 validators (⅔+1 of 128)
> - **Vote messages per round:** 32,512
> - **Vote payload:** ~4.5 MB per round
> - **Leader upload requirement:** ~600 Mbps for proposal fan-out
> - **Design target met:** ✅ Yes — 128 validators fits within the protocol budget
> 
> The key constraint is the **leader node's upload bandwidth** (~600 Mbps). Any validator with a 1 Gbps uplink can serve as leader.

---

### 6. Discovery Probe

**Prompt:**
```
Probe the AII testnet discovery seeds and tell me what peers are visible from my machine.
```

**What happens internally:**
```
AI → calls discovery_probe(seeds="8.211.135.234:30310,106.14.223.128:30310")
AI → aii-mcp sends UDP Ping/FindNode to seeds
AI → collects Neighbours responses
AI → returns discovered peers + observed public IP
```

**Claude response example:**
> I probed the AII testnet Discovery v4 seeds:
> 
> **Your observed public endpoint:** `203.0.113.42:58291` (NAT detected — your 30310 port maps to this)
> 
> **Discovered peers:**
> - `8.211.135.234:30311` — JP validator (Tokyo)
> - `106.14.223.128:30311` — CN validator (Shanghai)
> 
> Both public validators are reachable. Your NAT is transparent — you can run `--bft-outbound-only` as a validator without port forwarding.

---

## 💻 CLI Shell Agent Operations

AI agents with shell access (Claude Code, bash-tool agents) can use the `aii` CLI directly:

```bash
# ── Chain queries ──────────────────────────────────────────
# Get current status
./aii --rpc https://aii.allfund.xyz/api status --json

# Get recent blocks
./aii --rpc https://aii.allfund.xyz/api recent --limit 20 --json

# Look up a specific block
./aii --rpc https://aii.allfund.xyz/api block 670000 --json

# ── Wallet operations ──────────────────────────────────────
# Generate new address
./aii account new --json

# Generate BIP-39 mnemonic
./aii account mnemonic --json

# Derive address from mnemonic
./aii account from-mnemonic --phrase "word1 word2 ... word12" --json

# ── Validator tools ────────────────────────────────────────
# Check hardware tier
./aii tier --json

# Generate validator keypair
./aii validator keygen --out /tmp/validator.json

# Show validator pubkeys
./aii validator pubkey --file /tmp/validator.json --json

# ── Stress testing ─────────────────────────────────────────
# Submit 1,000 real signed transfers
./aii live-transfer-load \
  --rpc https://aii.allfund.xyz/api \
  --chain-id 9999 \
  --key-file key1.hex --key-file key2.hex \
  --total 1000 --json

# ── Genesis creation (multi-validator setup) ───────────────
./aii genesis init \
  --network testnet \
  --validator-pubkey /tmp/pub1.json \
  --validator-pubkey /tmp/pub2.json \
  --out genesis.json
```

---

## 📚 Practical Prompts Library

Copy and paste these prompts directly into Claude Desktop, Claude Code, or any MCP-enabled AI:

### 🔍 Monitoring & Analytics

```
Check the AII testnet health. Show me:
1. Current block number
2. Last 10 blocks with their timestamps
3. Calculate the average block interval
4. Is consensus performing normally?
```

```
I want to understand AII's current state. Query the chain status,
show me 5 recent blocks, and explain what the data means.
```

### 💳 Wallet Management

```
Help me set up an AII wallet:
1. Generate a 24-word BIP-39 mnemonic
2. Show the derived address
3. Explain how to import it into MetaMask
4. What are the security best practices?
```

```
I have a mnemonic phrase: [your phrase here]
Derive the AII address at index 0 and index 1.
```

### 🏗️ Infrastructure Planning

```
I'm planning to run an AII validator node.
1. Check my hardware tier
2. Calculate BFT capacity for 21 validators at 30s finality
3. Probe the testnet discovery seeds
4. Tell me if my machine is suitable and what I need to improve
```

```
Help me plan an AII validator cluster with 5 nodes:
- Calculate the BFT capacity budget
- What minimum bandwidth does each leader node need?
- What's the quorum threshold?
```

### 🔬 Development & Testing

```
I'm building on AII. Help me:
1. Get the current chain ID and RPC endpoint details
2. Look up block 670000 to understand the block structure
3. Explain the difference between AII's eth_* RPC and standard Ethereum
```

```
Run a discovery probe against the AII testnet seeds.
Tell me:
- What peers are visible from my network?
- What's my observed public IP/port?
- Am I behind NAT? Can I run a public validator?
```

---

## 🔒 Security Model

AII's AI integration is designed with **security-first** principles:

```
┌─────────────────────────────────────────────────────────┐
│                    SECURITY BOUNDARIES                  │
│                                                         │
│  ┌─────────────────────────────────────────────────┐   │
│  │              aii-mcp process                    │   │
│  │                                                 │   │
│  │  ✅ Key generation (local, in-memory only)      │   │
│  │  ✅ BIP-39 entropy (system CSPRNG)              │   │
│  │  ✅ Web3 v3 keystore encryption                 │   │
│  │                                                 │   │
│  │  ❌ Private keys NEVER sent over network        │   │
│  │  ❌ Mnemonics NEVER logged or persisted         │   │
│  └─────────────────────────────────────────────────┘   │
│                          │                              │
│                   HTTPS / HTTP                          │
│                          │                              │
│  ┌─────────────────────────────────────────────────┐   │
│  │              AII Node (RPC)                     │   │
│  │  Only receives: read queries, block lookups     │   │
│  │  Does NOT receive: private keys or mnemonics   │   │
│  └─────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────┘
```

**What to trust vs. what to verify:**

| Action | Trust Level | Verify How |
|--------|------------|------------|
| Chain status queries | ✅ Safe | Read-only, no side effects |
| Block lookups | ✅ Safe | Read-only |
| Address generation | ✅ Safe | Key stays local |
| Mnemonic generation | ⚠️ Sensitive | Back up offline immediately |
| Transfer signing | 🔐 High risk | Always verify amounts and addresses |
| Validator operations | 🔐 High risk | Use hardware wallet when possible |

> 💡 **Recommendation:** For wallet operations, always use `account_new_encrypted` (encrypted keystore) rather than raw key generation. The MCP server handles encryption before returning anything to the AI.

---

## ❓ FAQ

**Q: Can the AI send transactions on my behalf?**

> Not through the MCP server — it's read-only for chain queries and local-only for wallet operations. To send transactions, you must use the `aii` CLI with your own key file, which requires explicit user action.

**Q: Does the AI see my private keys?**

> No. `account_new_encrypted` returns a password-protected Web3 v3 keystore JSON. The raw private key is generated and immediately encrypted inside the `aii-mcp` process. The AI receives only the encrypted JSON.

**Q: Which RPC endpoint should I point aii-mcp at?**

> Use `https://aii.allfund.xyz/api` for the public testnet (HTTPS, globally available). For production use, run your own node and point to `http://localhost:8545`.

**Q: Can I use aii-mcp with GPT-4, Gemini, or other models?**

> Any model that supports function calling / tool use via MCP can use `aii-mcp`. Claude Desktop has native MCP support. For other models, you can wrap `aii-mcp` in an API bridge or use the `aii` CLI directly as a shell tool.

**Q: What's the difference between MCP and the CLI?**

> MCP is for structured AI agent workflows — the AI calls tools by name with typed JSON parameters. The CLI is for humans and shell-scripting agents (like Claude Code) that execute terminal commands. Both access the same underlying functionality.

**Q: Is aii-mcp production-ready?**

> The testnet is live and stable. Mainnet is not yet launched (Phase 2 pending: feature freeze, ≥21 validators, audit). The MCP server's tool surface is stable but may expand as mainnet approaches.

---

<div align="center">

## 🚀 Get Started Now

```bash
git clone https://github.com/kinglovesdao/aii.git && cd aii
cargo build --release -p aii-mcp
# → point Claude Desktop at target/release/aii-mcp
```

**[← Back to README](../README.md)** · **[Installation Guide (10 languages)](../INSTALL.md)** · **[Live Explorer](https://aii.allfund.xyz/)**

[![Claude](https://img.shields.io/badge/Works_with-Claude_Desktop-orange?style=for-the-badge&logo=anthropic)](https://claude.ai/)
[![MCP](https://img.shields.io/badge/Protocol-MCP_v1.0-blueviolet?style=for-the-badge)](https://modelcontextprotocol.io/)

</div>
