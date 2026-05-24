# AII v0.0.9 → 文档对照 Gap Analysis

> **状态**: 2026-05-24 · 基于 `新链文档资料包/` 14 份设计文档 + CodeGraph 索引（72 files / 1181 nodes）
> **目的**: 把当前 18 crate 实现与文档要求做差分，识别 (A) 缺失功能 (B) 需要扩展的功能 (C) 待优化项 (D) 多余项。

## 0. 总览

- 当前 v0.0.9: **18 crate / 44 测试组 / aiid binary 已 live**
- 文档要求 Day-0 footprint = 18 crate（spec §3.1–§3.3）—— 已 100% 覆盖
- 但其中 ≥ 6 个 crate 是 **scoped 版**，离文档完整要求有差距
- spec §3.4 列出的 Day-1+ 扩展（6 crate）+ 客户端外壳尚未启动

---

## A. 完全没做（按价值×成本排序）

| # | crate / 模块 | 文档出处 | 价值 | 成本 | 优先级 |
|---|---|---|---|---|---|
| A1 | `aii-onboarding` — 硬件评分 + Tier 推荐 | 04 §14.4 / 05 §9.5 | 高（节点首启必经） | 低（< 200 LOC） | **P0** |
| A2 | `aii-cli` — 用户面 CLI `aii` (clap v4) | 04 §3, 12 全章 | 高（用户面） | 中（clap + RPC 客户端） | **P0** |
| A3 | `aii-mcp` — MCP Server（rmcp） | 12 §3–§8（差异化卖点） | 极高（差异化） | 中（rmcp + 22 工具） | **P1** |
| A4 | `aii-consensus-bft` — 真正 BFT 引擎 | 05 全章 / 04 §3 | 极高（出块） | 高（VRF + ⅔ PRE-COMMIT + slot timing） | P2 |
| A5 | `aii-consensus-plugins` — 子链 PoS/PBFT/DPoS | 05 §11 | 中（子链业务） | 高 | Day-1 |
| A6 | `aii-crosschain` — HTLC/多签/IBC/XCM | 04 §8, 03 §9 | 中 | 极高（多协议） | Day-1 |
| A7 | `aii-wasm` — 子链 WASM VM | 04 §3, 08 §3 | 中 | 高（wasmtime 集成） | Day-1 |
| A8 | `aii-bindings` — UniFFI/JNI/WASM 绑定 | 04 §14.1–§14.3 | 中（移动 / 浏览器扩展） | 高 | Day-1 |
| A9 | `aii-hsm` — HSM 抽象（V 节点签名硬件保护） | 04 §11, 09 §3.2 | 中（生产节点） | 中（PKCS#11） | Day-1 |
| A10 | Block Explorer (aii.allfund.xyz) | vode.md / 用户运维 | 高（生态可视化） | 中（前端 + indexer） | P1 |

> **不做**：`aii-governance` —— CLAUDE.md 明确"无 DAO / 无链上投票"。

---

## B. 已存在但需扩展（按价值排序）

| # | crate | 当前 scope | 文档要求 | 差距 |
|---|---|---|---|---|
| B1 | `aii-evm` | 仅 EOA→EOA value transfer | 完整 EVM bytecode 执行（revm）+ Gas 计量 + 预编译 + 合约 CREATE/CALL | **缺 revm 集成** |
| B2 | `aii-rpc` | `eth_chainId` / `eth_blockNumber` / `aii_status` (3 个) | 完整 eth_* (≥ 20 个) + 多个 aii_* + WebSocket + GraphQL | **缺 17+ 方法** |
| B3 | `aii-wallet` | in-memory secp256k1 only | 加密 keystore (PBKDF2/scrypt) + BIP-39 助记词 + BIP-32/44 派生 | **缺加密 / 助记词** |
| B4 | `aii-net-p2p` | TCP framing + 4 message types | devp2p / RLPx 加密握手 + UDP Kademlia discovery + Bootnode | **缺 discovery / 加密** |
| B5 | `aii-net-sync` | 状态机（pure logic） | 接驳真正 p2p 层 + 持久化进度 + Snap Sync | **未与 net-p2p 接驳** |
| B6 | `aii-state` | Account + StateDb + MPT (root only) | 完整 per-account storage trie + state pruning + 历史查询 | **缺 storage trie** |
| B7 | `aii-node` (aiid) | 启动 RocksDB + RPC，head 始终 0 | 接驳 consensus + sync + txpool + 真实出块/同步 | **缺 consensus loop** |
| B8 | `aii-storage` | sync API | 异步 API + 多列簇 batch 优化 | 现已可用，优化可后置 |
| B9 | `aii-config` | mainnet/testnet ChainSpec + Genesis | Genesis 文件加载 + alloc 应用到 StateDb | **缺 apply_to_state()** |
| B10 | `aii-types` | 现有 7 个类型 | + Hex serde 全面化 + Display 增强 | 小补丁 |

---

## C. 优化项（DRY / 工程化）

| # | 项 | 现状 | 建议 |
|---|---|---|---|
| C1 | `u256_length / encode_u256 / decode_u256` | 在 `aii-block::header`、`aii-state::account` 各复制 1 份 | 提到 `aii-codec::rlp` 共享 |
| C2 | `hex_prefix` MPT helper | 仅在 `aii-state::trie`，无复用 | 保留（仅 MPT 用） |
| C3 | `decode_h256_loose` 在 tx/legacy + tx/eip1559 + tx/eip4844 | 各自有副本 | 提到 crate 顶层 |
| C4 | `cargo deny` / `cargo audit` 未跑 | CI 配了但本地无 | 本地装 + 跑一遍 |
| C5 | `llvm-cov` 覆盖率未量化 | CI 配了 ≥ 80% gate | 跑一遍 + 记录 baseline |
| C6 | Docker 镜像 | 没有 | 加 `Dockerfile` + `docker-compose.yml` |
| C7 | systemd unit | 没有 | 加 `deploy/aiid.service` |
| C8 | aiid binary 没接驳 storage 真实数据 | NodeState.head 总返回 0 | 从 ColumnFamily::Meta 读 `head_block_number` |
| C9 | 没有 ChangeLog 多版本对齐 | 单一文件 | 可视化 release notes（github tag 注释已够） |

---

## D. 多余 / 应移除

| # | 项 | 原因 | 建议 |
|---|---|---|---|
| D1 | spec §3.3 列出 `aii-consensus-pow` | 04 §1 明确"无 PoW、无矿工、无 Ethash" | 修订 spec 移除 |
| D2 | `aii-block::TxEip4844` placeholder | 4844 blob 在 AII 是 Day-1+，现在占位代码常驻 | 保留（向前兼容好处大于负担） |
| D3 | `aii-state::trie::mpt_root` 的 unimplemented panic 已替换为真实算法 | 历史包袱 | 已清理 ✅ |

---

## E. 立即推进计划（P0 + P1）

按价值/成本排序，并行展开：

1. **P0 — `aii-onboarding`**（新 crate）：硬件评分 + Tier 推荐（leaf 模块，~250 LOC）
2. **P0 — `aii-cli`**（新 crate）：用户面 CLI，子命令 `account` / `balance` / `send` / `status` / `node`（依赖 jsonrpsee http-client）
3. **P1 — `aii-mcp`**（新 crate）：MCP Server (rmcp)，先做 5–8 个只读工具
4. **P1 — `aii-rpc` 扩展**：补 `eth_getBalance` / `eth_getBlockByNumber` / `eth_getTransactionByHash` / `eth_sendRawTransaction` / `eth_gasPrice` / `aii_getAccount`
5. **P1 — `aii-wallet` keystore**：PBKDF2/scrypt 加密 keystore + BIP-39
6. **P2 — `aii-evm` revm**：合约执行 + gas
7. **P2 — block explorer**：前端 + indexer，部署到 8.211.135.234 / aii.allfund.xyz

每完成一个 crate：单测 → workspace clippy `-D warnings` → fmt → CHANGELOG → commit → push。

---

## F. 度量基线

- 现状：18 crate / ~340 tests / 7 tags (v0.0.5–v0.0.9)
- 目标：v0.0.10 = +onboarding +cli +mcp +rpc 扩展（4 个 crate），~50 新测试
- 目标：v0.1.0 = M2 全面退出（含 BFT 引擎 + 完整 evm + wallet keystore）
