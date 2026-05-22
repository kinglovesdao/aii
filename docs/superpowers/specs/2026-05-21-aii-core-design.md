# AII Core — Workspace 设计规范

> **状态**: 草案 v0.1 · 2026-05-21
> **范围**: `aii-code` Rust workspace 的 crate 拓扑、依赖图、里程碑与稳定边界。
> **权威源**: 《04 架构设计文档》§3、《08 技术选型决策书》§5、《10 开发路线图与里程碑》§2–§4。
> **目标读者**: AII 协议核心贡献者；任何打算新增或重构 crate 的开发者。

本规范是 *实现层* 的蓝图——把 14 份顶层文档转换为可执行的 cargo workspace 计划。文档定义 **是什么** 与 **为什么**；此 spec 定义 **怎么切 crate、怎么依赖、什么时候交付**。

如果文档与 spec 冲突，**文档优先**。请通过 PR 修订本 spec，而非偏离文档。

---

## §1 总览

### 1.1 与《04 架构设计》的对齐

《04》§1 给出五层架构（生态/接入/应用执行/共识协调/P2P网络）；§3 列出 ~27 个逻辑模块。本 spec 把这些逻辑模块 **重组为 24 个 Rust crate**，遵循以下原则：

| # | 原则 | 落实方式 |
|---|---|---|
| 1 | **关注点分离** | crate 边界 = 团队可独立 review 的最小单元 |
| 2 | **最小可信内核** | 把"主网启动 Day-0 必需"的 18 个 crate 隔离为强冻结目标；Day-1+ 的 6 个为后续扩展 |
| 3 | **以太兼容** | `aii-rlp` 强约束 RLP 编码与以太主网对齐；`aii-evm` 包装 `revm` 而非自研 |
| 4 | **故障隔离** | 子链共识插件（pos/pbft/dpos）通过 `aii-consensus-iface` trait 接入；主链 BFT 独立 crate |
| 5 | **无 unsafe** | workspace lint `unsafe_code = "forbid"`；唯一例外通过 spec 修订单独豁免（见 §6.2） |

### 1.2 与《08 技术选型》的对齐

| 模块 | 关键依赖 | 来源 |
|---|---|---|
| `aii-crypto` | `k256` + `sha3`(tiny-keccak family) + `blst` + `schnorrkel` | 08 §5.2 |
| `aii-storage` | `rocksdb` (Rust binding) | 08 §4.2 |
| `aii-evm` | `revm` | 08 §3.3 |
| `aii-net-*` (主链) | `devp2p` / `RLPx` 兼容（Rust 自研或 reth 复用） | 08 §2.2 |
| `aii-rpc` | `jsonrpsee` | 04 §5.4 + 08 §1.3 |
| `aii-cli` | `clap` v4 | 04 §3 |
| `aii-mcp` | `rmcp` | 04 §3、12 章 |
| `aii-wasm` (Day-1) | `wasmtime` | 04 §3、08 §3 |

### 1.3 与《10 路线图》的对齐

| 路线图 Phase | 对应 spec 里程碑 | 主交付 crate |
|---|---|---|
| Phase 0 启航·Alpha (T0-12月→T0-9月) | **M0** Workspace 骨架 | aii-types ✅、aii-rlp、aii-crypto、aii-storage |
| Phase 1 启航·Beta (T0-9月→T0-3月) | **M1** 状态 + 执行 | aii-state、aii-evm、aii-block、aii-net-* |
| Phase 1 后半 + Phase 2 启航·Main | **M2** 共识 + 节点二进制 | aii-consensus-{iface,bft}、aii-vnode、aii-microchain、aii-rpc、aii-wallet、aii-cli、aii-node、aii-metrics、aii-config |
| Phase 3+ 远航 | **M3** 扩展 | aii-mcp、aii-wasm、aii-consensus-plugins、aii-crosschain、aii-bindings、aii-onboarding |

M0 + M1 + M2 = 18 crate = **主网启动 Day-0 footprint**。

---

## §2 Crate 依赖图

```
                     ┌────────────────────────┐
                     │  aii-node (binary)      │ ── M2
                     └─────────┬──────────────┘
                               ▼
        ┌────────────┬─────────┴────────┬──────────────┐
        ▼            ▼                  ▼              ▼
   aii-cli      aii-rpc          aii-microchain   aii-metrics
        │            │                  │              │
        │            │                  ▼              │
        │            │     ┌──────aii-consensus-bft────┐│
        │            │     │           │              ││
        │            │     ▼           ▼              ▼▼
        │            │  aii-vnode  aii-consensus-iface
        │            ▼     │           │
        │     aii-wallet   │           │
        │            │     │           │
        └────────┬───┴─────┴───────────┴──────────┐
                 ▼                                ▼
            aii-block ◀────── aii-evm ◀───── aii-state
                 │                │                │
                 │                │                ▼
                 │                └─────────► aii-storage
                 ▼                                 │
            aii-rlp                                │
                 │                                 │
                 └─────────┬───────────────────────┘
                           ▼
                     aii-crypto
                           │
                           ▼
                     aii-types ✅
```

**规则**:
1. 依赖方向严格自上而下；禁止反向边。
2. `aii-types`、`aii-rlp`、`aii-crypto`、`aii-storage` 是 **基石层**——任何修改需要全 workspace 重新测试。
3. `aii-net-*` 在图中省略（横向依赖：被 `aii-consensus-bft` / `aii-microchain` 引用）。
4. `aii-config`（chain spec / genesis 解析）是叶子模块，被 `aii-node` 引用，不在依赖链中。
5. Day-1 crate（虚线，未画）：`aii-mcp` 复用 `aii-rpc` 的 handler；`aii-wasm` 平级于 `aii-evm`；`aii-crosschain` 在 `aii-microchain` 与 `aii-net-*` 之间；`aii-consensus-plugins` 实现 `aii-consensus-iface`；`aii-bindings` 与 `aii-onboarding` 是 `aii-cli` / 客户端外壳的辅助。

---

## §3 24-crate 列表

约定：
- 路径形如 `crates/<name>/`
- "依赖" 列只列 **workspace 内** 依赖；外部依赖见 §1.2 与各 crate `Cargo.toml`
- "状态" 列：✅ 已完成 · 🟡 进行中 · ⚪ 未启动
- 列表按里程碑分组

### 3.1 M0 — 基石层（4 个）

| # | crate | 路径 | 职责 | 依赖 | 状态 |
|---|---|---|---|---|---|
| 1 | `aii-types` | `crates/aii-types/` | 基础类型：`H256`、`Address`、`U256`、`AlgoId`、`BlsPubKey/Signature`、`SignedTx`、`TypesError` | – | ✅ |
| 2 | `aii-rlp` | `crates/aii-rlp/` | RLP 编解码；导出 `Encodable`/`Decodable` derive；与以太主网 fixture 100% 对齐 | `aii-types` | ⚪ |
| 3 | `aii-crypto` | `crates/aii-crypto/` | Keccak-256；secp256k1 sign/verify/recover；BLS12-381（blst）单签/聚合；VRF（schnorrkel）；PQ slot 占位 | `aii-types` | ⚪ |
| 4 | `aii-storage` | `crates/aii-storage/` | KV abstract trait + RocksDB 默认实现；ColumnFamily 约定；快照与批量写入 | `aii-types` | ⚪ |

### 3.2 M1 — 状态与执行（5 个）

| # | crate | 路径 | 职责 | 依赖 | 状态 |
|---|---|---|---|---|---|
| 5 | `aii-state` | `crates/aii-state/` | MPT；`Account` (nonce/balance/code_hash/storage_root)；StateDB；Trie root 计算 | `aii-types`, `aii-rlp`, `aii-crypto`, `aii-storage` | ⚪ |
| 6 | `aii-block` | `crates/aii-block/` | `Block`、`Header`、`Tx`、`Receipt`；交易类型 enum；hash 计算 | `aii-types`, `aii-rlp`, `aii-crypto` | ⚪ |
| 7 | `aii-evm` | `crates/aii-evm/` | `revm` 包装；Gas 计量；预编译接入；状态托管 | `aii-types`, `aii-state`, `aii-block` | ⚪ |
| 8 | `aii-net-p2p` | `crates/aii-net-p2p/` | devp2p / RLPx；节点发现；消息广播 | `aii-types`, `aii-rlp`, `aii-crypto` | ⚪ |
| 9 | `aii-net-sync` | `crates/aii-net-sync/` | 区块同步、Snap Sync；快照拉取 | `aii-types`, `aii-block`, `aii-net-p2p`, `aii-storage` | ⚪ |

### 3.3 M2 — 共识、网络、入口（9 个）

| # | crate | 路径 | 职责 | 依赖 | 状态 |
|---|---|---|---|---|---|
| 10 | `aii-consensus-iface` | `crates/aii-consensus-iface/` | 共识引擎 trait（`Engine`、`Proposer`、`Voter`）；区块验证回调 | `aii-types`, `aii-block` | ⚪ |
| 11 | `aii-consensus-bft` | `crates/aii-consensus-bft/` | **主链** BFT-PoS（VRF 提议者 + ⅔ stake PRE-COMMIT 单区块即时最终性） | `aii-consensus-iface`, `aii-crypto`, `aii-vnode` | ⚪ |
| 12 | `aii-vnode` | `crates/aii-vnode/` | V 节点抵押 (100,000 AII)；VSet 维护；选举；签名验证；80/20 奖励拆分 | `aii-types`, `aii-crypto`, `aii-state` | ⚪ |
| 13 | `aii-microchain` | `crates/aii-microchain/` | 子链生命周期；Flush 调度；子链注册表 | `aii-types`, `aii-block`, `aii-consensus-iface`, `aii-storage` | ⚪ |
| 14 | `aii-net-txpool` | `crates/aii-net-txpool/` | 交易池；nonce 排序；驱逐策略 | `aii-types`, `aii-block`, `aii-state` | ⚪ |
| 15 | `aii-rpc` | `crates/aii-rpc/` | JSON-RPC / WebSocket（jsonrpsee）；eth_* + aii_* 命名空间 | `aii-types`, `aii-block`, `aii-state`, `aii-net-txpool`, `aii-consensus-iface` | ⚪ |
| 16 | `aii-wallet` | `crates/aii-wallet/` | 本地 keystore；PBKDF2/scrypt；签名打包；BIP-39 助记词 | `aii-types`, `aii-crypto` | ⚪ |
| 17 | `aii-config` | `crates/aii-config/` | Chain spec、genesis、链 ID（占位 99）、参数解析 | `aii-types`, `aii-block` | ⚪ |
| 18 | `aii-metrics` | `crates/aii-metrics/` | Prometheus 指标导出；关键 metric 名称约定 | – | ⚪ |

> **二进制 `aiid`** 通过 `crates/aii-node/` 装配（cargo workspace 的可执行 crate；本表第 25 项归到 Day-1 之前的"装配"——见 3.4）。

### 3.4 装配 + Day-1 扩展（6 个）

| # | crate | 路径 | 职责 | 依赖 | 状态 |
|---|---|---|---|---|---|
| 19 | `aii-node` | `crates/aii-node/` | **bin** `aiid` 守护进程：装配所有 M0–M2 crate；命令行参数；引导流程 | (装配所有 M0–M2) | ⚪ |
| 20 | `aii-cli` | `crates/aii-cli/` | **bin** `aii`：用户面 CLI（clap v4）；账户/转账/查询/AI 命令 | `aii-rpc` 客户端、`aii-wallet` | ⚪ |
| 21 | `aii-mcp` | `crates/aii-mcp/` | MCP Server（rmcp）；stdio/SSE/HTTPS 多 transport；工具暴露给 Claude/Cursor/Cline | `aii-rpc` handler 复用 | ⚪ Day-1 |
| 22 | `aii-wasm` | `crates/aii-wasm/` | WASM 虚拟机（wasmtime）；子链可选执行环境 | `aii-types`, `aii-state` | ⚪ Day-1 |
| 23 | `aii-consensus-plugins` | `crates/aii-consensus-plugins/` | 子链可插拔共识：PoS / PBFT / DPoS；通过 features 选择 | `aii-consensus-iface`, `aii-crypto` | ⚪ Day-1 |
| 24 | `aii-crosschain` | `crates/aii-crosschain/` | HTLC 桥；IBC 适配；XCM 适配；多签桥 | `aii-types`, `aii-block`, `aii-net-p2p` | ⚪ Day-1 |

> **未列入的支持 crate**（合并到 24 项之外的开发支撑）：
> - `aii-bindings`（UniFFI / WASM / JNI 跨平台绑定）——属于客户端外壳（Tauri / iOS / Android）所在的独立 repo（参见 04 §14），不在 `aii-code` workspace 内。
> - `aii-onboarding`（硬件识别 + Tier 推荐）——同上，属于客户端外壳。
> - 端到端测试 crate（`tests/e2e/`）不计入 24 项；它消费 workspace 但不被消费。

---

## §4 里程碑

### M0 — Workspace 骨架（对齐 Phase 0 启航·Alpha · 3 个月）

**入口条件**: workspace bootstrap 已完成（commit `f416efc`），CI 绿（fmt + clippy + test + deny + audit + llvm-cov）。

**交付**:
- ✅ `aii-types` 完成（H256/Address/U256/AlgoId/BLS/SignedTx/Errors/proptest/rustdoc）
- ⚪ `aii-rlp` 实现 + 与以太主网 fixture 对齐（≥ 100 个测试用例）
- ⚪ `aii-crypto` 实现 + KAT 向量验证
- ⚪ `aii-storage` 实现 + RocksDB 基准测试（写入 ≥ 50k op/s）

**退出标准**: 4 个基石 crate 全部 `cargo test -p <crate>` 通过；workspace `cargo doc` 无 warning；`llvm-cov` 行覆盖 ≥ 80%。

### M1 — 状态与执行（对齐 Phase 1 启航·Beta 前半 · ~3 个月）

**交付**:
- ⚪ `aii-state` MPT root 与以太主网区块 0 root 对齐（创世状态测试）
- ⚪ `aii-block` 序列化与以太主网 ≥ 100 个区块 fixture 对齐
- ⚪ `aii-evm` 通过 `ethereum-tests` 子集（≥ 80%）
- ⚪ `aii-net-p2p` 节点发现 + 双向握手 + 链上消息收发
- ⚪ `aii-net-sync` 全节点同步以太主网或测试网（用于回归测试）

**退出标准**: 测试网 v0.1（单节点）能够本地启动、产生空区块、接受预签名交易并执行。

### M2 — 共识、入口、节点二进制（对齐 Phase 1 后半 + Phase 2 · ~6 个月）

**交付**:
- ⚪ `aii-consensus-iface` + `aii-consensus-bft` 联调通过：4 节点本地 BFT 网络，单区块即时最终性
- ⚪ `aii-vnode` 抵押 / 解抵押 / VSet 切换流程
- ⚪ `aii-microchain` 子链注册 + Flush 到主链
- ⚪ `aii-rpc` `eth_*` 兼容性测试（MetaMask 连接成功）+ `aii_*` 命名空间
- ⚪ `aii-wallet` keystore 读写 + secp256k1/BLS 签名
- ⚪ `aii-cli` 核心命令（account / balance / send / vnode-stake）
- ⚪ `aii-config` 测试网 + 主网 genesis 配置
- ⚪ `aii-metrics` Prometheus 端点暴露
- ⚪ `aii-node` `aiid` 单二进制启动；systemd unit + Docker image

**退出标准**: 公开测试网上线，第三方贡献者能跑通节点、参与共识、提交交易、连接 MetaMask。**Day-0 footprint 冻结**。

### M3 — 扩展（对齐 Phase 3 远航 + 之后）

**交付**:
- ⚪ `aii-mcp` MCP Server 对接 Claude Desktop / Claude Code / Cursor
- ⚪ `aii-wasm` 子链选项实装
- ⚪ `aii-consensus-plugins` 至少完成 PoS 与 PBFT
- ⚪ `aii-crosschain` HTLC 桥首发；IBC / XCM 后续

**退出标准**: 主网启动后 6 个月内逐项落地；不阻塞 Day-0 主网启动。

---

## §5 接口锁定边界

### 5.1 语义化版本（SemVer）

| 阶段 | 版本号范围 | 含义 |
|---|---|---|
| 当前 | `0.0.x` | **不稳定**——任何 release 都可以 break；不发布 crates.io |
| M2 退出后 | `0.1.0` | Day-0 footprint 冻结；公共 API 进入 SemVer 跟踪；patch 不破坏；minor 仅向后兼容增量；major 需 spec 修订 |
| 主网启动后 | `1.0.0` | 强冻结；major bump 需要硬分叉级别的协议共识 |

### 5.2 公共 vs 内部 API

每个 crate 在 `lib.rs` 头部声明：

```rust
//! # aii-<name>
//!
//! ## Public API
//! 列出向下游 crate 暴露的 trait/struct/fn。
//!
//! ## Internal
//! 任何 `pub(crate)` 项；外部不依赖；可任意重构。
```

**公共 API 变更**必须更新本 spec §3 的 crate "职责" 列；如果跨多 crate，单独提交 spec 修订 PR。

### 5.3 不会暴露的事项

- crates.io 发布：M2 退出前不发布；M2 退出后仅发布 `aii-types`、`aii-rlp`、`aii-crypto`（给生态 SDK 使用）。其余 crate 通过 git 引用。
- 跨语言 ABI：客户端外壳通过 `aii-bindings`（独立 repo）暴露 UniFFI / JNI / WASM；本 workspace 不直接暴露 C ABI。

---

## §6 安全策略

### 6.1 Lint 配置

workspace `Cargo.toml` 已锁定：

```toml
[workspace.lints.rust]
unsafe_code = "forbid"
missing_docs = "warn"

[workspace.lints.clippy]
all = { level = "deny", priority = -1 }
pedantic = "warn"
nursery = "warn"
```

每个 crate 在 `lib.rs` 顶部声明 `#![deny(missing_docs)]` 升级为硬错误（在 crate 达到 v0.1 时）。

### 6.2 unsafe 豁免

唯一可能需要豁免的场景：
- `aii-crypto` 中调用 `blst` C 库的薄包装（blst 本身含汇编）——通过 FFI 调用，**不在我们 crate 内写 `unsafe`**；`blst-rs` 已封装。

如未来需要新增豁免，必须：
1. 提交 spec 修订 PR
2. crate `lib.rs` 顶部从 `forbid` 改为 `#![deny(unsafe_code)]` 并 `#[allow(unsafe_code)]` 标注精确位置
3. 该位置需独立审计

### 6.3 第三方依赖审计

| 工具 | 用途 | 频率 |
|---|---|---|
| `cargo deny` | 许可证白名单 + 已知漏洞 | 每次 PR (CI) |
| `cargo audit` | RustSec 数据库 | 每次 PR (CI) |
| `cargo vet` | 供应链审计（08 §10） | M1 退出前接入 |

### 6.4 审计计划

按《10》§3.4：M2 退出前完成 **两家独立审计机构** 评审，候选：Trail of Bits、Cure53、SlowMist、PeckShield、Least Authority。审计范围 = M0 + M1 + M2 所有 18 个 Day-0 crate。

---

## §7 变更日志

| 版本 | 日期 | 变更 |
|---|---|---|
| v0.1 | 2026-05-21 | 初稿；24-crate 列表；M0–M3 里程碑；与 04/08/10 文档对齐 |

后续修订追加于此表，并在 commit message 引用本 spec 段落号。
