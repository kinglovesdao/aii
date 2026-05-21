# AII-Core 主网 v1.0 架构设计 Spec

> Status：**Design — 待 writing-plans 实施计划阶段拆分**
> Date：2026-05-21
> Author：核心贡献者（AII 社区）+ Claude (brainstorming)
> Scope：AII 公链核心节点（`aii-core` + `aiid` 守护进程）主网 v1.0 可上线版本
> Sibling：本 spec 不涵盖 区块链浏览器 / 桌面客户端 / 移动客户端 / 浏览器扩展 / 跨链桥 / 子链框架——后续各自独立 spec
> Reference baseline：当前仓库 14 份文档 + CLAUDE.md；与基线冲突处以本 spec 为准并需同步基线

---

## 1. Context 与目标

### 1.1 Context

AII 是 Layer 1 PoS BFT 公链协议，灵感来源于 MOAC 的分层多链架构，但全部 Rust 重新实现。已有 14 份高层文档（白皮书、黄皮书、共识、代币经济、安全、AI 集成等）定义协议设计，但**尚未有任何代码**。本 spec 是从「文档设计」走向「代码实现」的第一步——回答"代码该怎么组织、什么时候算 main-net ready"。

### 1.2 已确认约束（brainstorming 阶段固化）

| # | 约束 | 说明 |
|---|---|---|
| C1 | 范围终点 = **主网 v1.0 可上线** | 9-12 月工程量；涵盖全部 Phase 0 mainnet 功能 + 2 轮独立审计完成 |
| C2 | **Day-0 必做 6 项全部纳入** | StateCommitment 抽象 / EIP-4337+7702 AA / 多签名 Registry / PQ Forklift 钩子 / 多入口传播 / Inclusion List 数据结构 |
| C3 | 实现起点 = **从零白手写** | 不 fork reth / erigon，仅复用独立 crate（revm、blst、libp2p、RocksDB） |
| C4 | 开发纪律 = **强制 TDD 全核心模块** | 所有 crate 单测/属性测试/集成测试全过才能合入 main |

### 1.3 非目标（明确不在本 spec 范围）

- ❌ 子链 MicroChain 框架（Phase 3，单独 spec）
- ❌ 跨链桥（HTLC + 多签 + IBC）（Phase 4，单独 spec）
- ❌ 区块链浏览器（独立子项目）
- ❌ 桌面 / 移动 / 浏览器扩展客户端（接入层 spec）
- ❌ DAG-BFT（Phase 5 路线图项，本 spec 仅保留升级接口）
- ❌ WASM 子链 VM（Phase 5，单独 spec）
- ❌ STARK 聚合签名（Phase 5+ / Q-Day 触发）
- ❌ 隐私子链（Phase 5）

### 1.4 成功标准

主网启动当天满足：

1. ≥ 21 个 V 节点跨地理分布运行 `aiid`，BFT-PoS 共识稳定出块
2. EVM 完整兼容（通过 `ethereum/tests` GeneralStateTests / BlockchainTests）
3. RPC 与 MetaMask / Hardhat / Foundry 钱包工具链开箱即用
4. 全部 Day-0 6 项**已实现并通过测试**（即使 Inclusion List 强制启用延后到 Phase 2，数据结构 Day-0 即在线）
5. ≥ 2 家独立安全审计完成，无 Critical / High 未修复
6. Bug Bounty 计划上线（SECRC 多签持 5 亿 AII / 500M AII）
7. Testnet Alpha 稳定运行 ≥ 6 月、Testnet Beta ≥ 3 月
8. Nakamoto coefficient ≥ 7（验证者去中心化指标）

---

## 2. 系统架构

### 2.1 分层架构（6 层 + 3 横切）

```
┌─────────────────────────────────────────────────────────────────┐
│  L6 API 层      JSON-RPC (ETH 兼容) + GraphQL + MCP Server     │
│                  aii-rpc / aii-mcp                              │
├─────────────────────────────────────────────────────────────────┤
│  L5 网络层      libp2p P2P + 4 入口 mempool + sync             │
│                  aii-p2p / aii-mempool / aii-sync               │
├─────────────────────────────────────────────────────────────────┤
│  L4 共识层      BFT-PoS (Tendermint-style)                     │
│                  aii-consensus / aii-bft / aii-vrf / aii-slashing│
├─────────────────────────────────────────────────────────────────┤
│  L3 执行层      revm EVM + EIP-1559 + EIP-4337/7702 AA         │
│                  aii-evm / aii-aa                               │
├─────────────────────────────────────────────────────────────────┤
│  L2 状态层      StateCommitment 抽象 + 默认 MPT (Verkle 预留)   │
│                  aii-state / aii-storage / aii-trie             │
├─────────────────────────────────────────────────────────────────┤
│  L1 协议/数据层 类型 / 编码 / Genesis / 协议常量               │
│                  aii-types / aii-codec / aii-config / aii-genesis│
└─────────────────────────────────────────────────────────────────┘

横切关注点（X1-X3）：

┌────────────────────────────────────────────────────────────────┐
│  X1 加密 Registry   aii-crypto + aii-registry                  │
│      默认 secp256k1 / BLS12-381 / schnorrkel；                 │
│      预留 ML-DSA / SLH-DSA / Falcon 槽位（PQ ready）           │
├────────────────────────────────────────────────────────────────┤
│  X2 Inclusion List  aii-il                                     │
│      Genesis 数据结构上线，Phase 2 启用强制 + 轻罚没           │
├────────────────────────────────────────────────────────────────┤
│  X3 账户抽象       aii-aa                                      │
│      EIP-4337 EntryPoint + EIP-7702 EOA 临时委托               │
│      为 PQ Forklift 提供迁移钩子                                │
└────────────────────────────────────────────────────────────────┘
```

### 2.2 架构原则

1. **单向依赖**：L6 → L5 → L4 → L3 → L2 → L1，禁止反向
2. **trait 暴露接口**：跨层调用通过 trait，便于 TDD 注入 mock
3. **可演进**：StateCommitment 抽象（v0.2 修正）使 MPT→Verkle/Binary Tree 可不破坏 L3+
4. **协议常量化**：L1 所有协议参数 hard-coded const，hard fork 才能改
5. **横切预留**：Day-0 必做 6 项中的 1/3/4/5/6 都在 L1/X1/X2/X3 预留接口或槽位
6. **横切 crate 不依赖业务层**：`aii-crypto`/`aii-registry` 只依赖 `aii-types`，不依赖 `aii-evm` 等

### 2.3 与现有文档的关系

- 本 spec 是 **04 架构设计文档** 在「真正进入代码实现」时的细化版本，更注重 crate 边界与 trait 接口。
- 与 **02 项目章程** 第三章「协议常量」严格一致。
- 与 **05 共识机制详细设计** 一致；本 spec 不重复共识算法细节，仅给出 crate 实现规划。
- 与 **13 演进优化** v0.2 Day-0 必做 6 项一一对应（C2）。

---

## 3. 子系统划分（Cargo Workspace）

### 3.1 Workspace 布局（24 内部 crate + 2 app）

**crate 统计**：L1 共 4 个 / L2 共 3 个 / L3 共 2 个 / L4 共 4 个 / L5 共 4 个 / L6 共 2 个 / 横切 5 个 = **24 内部 crate**；加上 `aiid` + `aii` 两个 binary = **总计 26 个 workspace 成员**。

```
aii/                                    # github.com/AII-Network/aii
├── Cargo.toml                          # workspace root
├── README.md
├── LICENSE                             # MIT 或 Apache-2.0
├── CHANGELOG.md
│
├── crates/
│   │
│   ├── # L1 协议/数据层（4）
│   ├── aii-types/                      # H256, Address, U256, BlsKey
│   ├── aii-codec/                      # RLP / SSZ / JSON
│   ├── aii-config/                     # 协议常量 + 配置加载
│   ├── aii-genesis/                    # Genesis 初始化、预约登记解析
│   │
│   ├── # L2 状态层（3）
│   ├── aii-state/                      # StateCommitment trait + 默认 MPT
│   ├── aii-storage/                    # RocksDB KV 抽象
│   ├── aii-trie/                       # MPT 内部实现（private）
│   │
│   ├── # L3 执行层（2）
│   ├── aii-evm/                        # revm 集成 + AII 预编译 + 系统合约
│   ├── aii-aa/                         # EIP-4337 EntryPoint + EIP-7702
│   │
│   ├── # L4 共识层（4）
│   ├── aii-consensus/                  # 共识 trait + 子链可插拔接口
│   ├── aii-bft/                        # 主链 BFT-PoS 引擎
│   ├── aii-slashing/                   # 罚没规则 + 证据收集
│   ├── aii-vrf/                        # VRF 提议者选举
│   │
│   ├── # L5 网络层（4）
│   ├── aii-p2p/                        # libp2p 栈
│   ├── aii-mempool/                    # txpool + EIP-1559 + 4 入口
│   ├── aii-sync/                       # 区块同步
│   ├── aii-il/                         # Inclusion List
│   │
│   ├── # L6 API 层（2）
│   ├── aii-rpc/                        # JSON-RPC + WebSocket + GraphQL
│   ├── aii-mcp/                        # MCP Server
│   │
│   ├── # 横切（5）
│   ├── aii-crypto/                     # 密码学原语适配器
│   ├── aii-registry/                   # 多签名 algo Registry
│   ├── aii-pq-forklift/                # PQ 迁移操作码
│   ├── aii-wallet/                     # 钱包/签名 SDK
│   └── aii-bindings/                   # UniFFI / WASM / JNI 绑定生成
│
├── apps/                               # 可执行
│   ├── aiid/                           # 守护进程（节点）
│   └── aii/                            # 用户 CLI（含 ai serve）
│
├── tests/
│   ├── conformance/                    # ethereum/tests 适配
│   ├── integration/                    # 多节点 e2e
│   └── fuzz/                           # cargo-fuzz targets
│
├── benches/                            # criterion 基准
└── .github/workflows/                  # CI
```

### 3.2 关键依赖原则

| 原则 | 说明 |
|---|---|
| 严格 DAG 依赖 | crate 间依赖必须为 DAG，循环依赖编译失败 |
| trait 暴露接口 | 跨层调用通过 trait，便于 TDD 注入 mock |
| 私有 crate | `aii-trie` 等仅 workspace 内使用，不发 crates.io |
| 公开 crate | `aii-types` / `aii-wallet` / `aii-rpc` 可发布到 crates.io |
| 横切不依业务 | `aii-crypto` / `aii-registry` 只依 `aii-types` |
| 二进制最小化 | `aiid` / `aii` 只 link 必要 crate，避免膨胀 |

### 3.3 子系统职责一句话定义

| Crate | 职责（≤ 15 字） |
|---|---|
| aii-types | 共享基础类型 |
| aii-codec | 编解码（RLP/SSZ/JSON） |
| aii-config | 协议常量 + 配置 |
| aii-genesis | 创世初始化 |
| aii-state | 状态承诺抽象 |
| aii-storage | RocksDB KV |
| aii-trie | MPT 私有实现 |
| aii-evm | revm 集成 + AII 扩展 |
| aii-aa | 账户抽象 |
| aii-consensus | 共识 trait |
| aii-bft | BFT-PoS 引擎 |
| aii-slashing | 罚没规则 |
| aii-vrf | VRF 选举 |
| aii-p2p | libp2p 栈 |
| aii-mempool | 交易池 |
| aii-sync | 区块同步 |
| aii-il | Inclusion List |
| aii-rpc | RPC/WS/GraphQL |
| aii-mcp | MCP Server |
| aii-crypto | 密码学原语 |
| aii-registry | 算法 Registry |
| aii-pq-forklift | PQ 迁移 |
| aii-wallet | 钱包 SDK |
| aii-bindings | 跨平台绑定 |
| aiid | 守护进程 |
| aii | 用户 CLI |

### 3.4 关键 trait 草案

下面给出**最重要的 8 个 trait**，作为 crate 间契约。详细签名在 writing-plans 阶段为每个 crate 单独细化。

```rust
// L2: 状态承诺抽象
trait StateCommitment {
    type Witness;
    fn root(&self) -> H256;
    fn prove(&self, keys: &[StateKey]) -> Self::Witness;
    fn verify(root: H256, witness: Self::Witness) -> bool;
    fn migrate_epoch(old: H256, new_scheme: CommitmentSchemeId) -> MigrationRoot;
}

// X1: 签名算法 Registry
trait SignatureScheme {
    const ALGO_ID: u8;
    fn verify(pubkey: &[u8], msg: &[u8], sig: &[u8]) -> bool;
    fn pubkey_size() -> usize;
    fn signature_size() -> usize;
    fn quantum_safe() -> bool;
}

// L4: 共识引擎（子链可插拔）
trait ConsensusEngine {
    type Block;
    type Vote;
    fn propose(&mut self, slot: u64, parent: H256) -> Option<Self::Block>;
    fn validate(&self, block: &Self::Block) -> Result<(), ConsensusError>;
    fn finalize(&mut self, block: Self::Block, votes: &[Self::Vote]);
}

// L3: VM 抽象（主链 EVM，子链可换 WASM）
trait VirtualMachine {
    type State;
    type Tx;
    type Receipt;
    fn execute(&mut self, state: &mut Self::State, tx: Self::Tx) -> Self::Receipt;
}

// X2: Inclusion List 委员会
trait InclusionListCommittee {
    fn select_for_slot(slot: u64, epoch_seed: H256) -> Vec<ValidatorId>;
    fn submit_list(&self, slot: u64, list: Vec<TxHash>) -> Result<(), ILError>;
    fn aggregate(&self, slot: u64) -> InclusionListRoot;
}

// X3: 账户抽象 EntryPoint
trait AccountAbstraction {
    fn validate_userop(&self, op: &UserOperation) -> Result<(), AAError>;
    fn execute_userop(&mut self, op: UserOperation) -> Result<Receipt, AAError>;
    fn supports_algo(&self, algo_id: u8) -> bool;
}

// L5: 4 入口 mempool
trait TxEntry {
    fn submit(&mut self, tx: SignedTx, source: TxSource) -> Result<TxHash, MempoolError>;
    fn prove_seen(&self, tx_hash: TxHash) -> Option<TxSeenReceipt>;
}

// L4: 罚没证据
trait SlashingEvidence {
    type Evidence;
    fn collect(&mut self, evidence: Self::Evidence);
    fn verify(&self, evidence: &Self::Evidence) -> Result<SlashTarget, ()>;
    fn apply(&mut self, target: SlashTarget) -> Result<SlashedAmount, ()>;
}
```

---

## 4. 关键数据流

详见 brainstorm Section 3。本节给出每条流程的 Rust crate 触达图，便于实现追踪。

### 4.1 Genesis Bootstrap

```
genesis.json (本地)
  → aii-genesis::load_json()
  → aii-config::validate_protocol_constants()
  → aii-state::build_initial_trie(allocations)
  → aii-registry::register_default_schemes([secp256k1, BLS, schnorrkel])
  → aii-pq-forklift::reserve_slots([ML-DSA, SLH-DSA, Falcon])
  → aii-bft::wait_for_validator_quorum(≥21)
  → 主网激活
```

### 4.2 交易生命周期（4 入口 → finalized）

```
[L6] wallet → sign(tx, algoId)
[L5] aii-mempool::submit(tx, source ∈ {RPC, P2P, LightRelay, Tor})
       └→ aii-registry::verify_sig(algoId, pubkey, sig)
       └→ aii-mempool::add_to_pending(tx)
[X2] aii-il::committee_observe(tx_hash)
[L4] slot N: aii-vrf::select_proposer(slot)
[L4] aii-bft::construct_block(mempool, il)
[L4] aii-bft::broadcast_prevote → wait ⅔
[L4] aii-bft::broadcast_precommit → wait ⅔ → finalize
[L3] aii-evm::apply_block(block)
       └→ aii-aa::dispatch_userop(若 tx 进入 EntryPoint)
       └→ revm::transact(tx)
[L2] aii-state::update_trie(state_changes)
[L1] aii-codec::encode_receipt → return to RPC
```

### 4.3 出块单 slot（6 秒）

```
T+0s : aii-vrf::select_proposer(slot=N)
T+0-1s: proposer 构造 block (含 IL + parentCommitSig)
T+1s : broadcast block
T+1-2s: V 节点 verify + aii-bft::broadcast_prevote
T+2-3s: 收齐 ⅔ stake → aii-bft::broadcast_precommit
T+3-4s: 收齐 ⅔ stake → finalized
T+6s : slot N+1 starts
```

### 4.4 状态承诺迁移（hard fork）

详见 brainstorm Section 3.4。本 spec 仅保证 `StateCommitment` trait 在 Day-0 上线，具体迁移方案（MPT → Verkle）属于未来 hard fork，由独立 spec 处理。

### 4.5 4 入口交易传播

```
钱包 → 默认 N=3 个入口同时广播:
  ├── Public RPC (POST /jsonrpc → aii-rpc)
  ├── P2P direct (aii tx --p2p → aii-p2p::gossip)
  ├── Light client relay (T6/T7 节点 forward)
  └── (可选) Tor/I2P bootstrap
aii-mempool 任一入口收到即 dedup + validate + add
aii-mempool::prove_seen(tx_hash) → TxSeenReceipt
```

### 4.6 罚没证据流

```
节点观察到双签:
  → 收集 2 条冲突 PRE-COMMIT
  → aii-slashing::collect(SlashingEvidence::DoubleSign{a, b})
  → 验证: 同 slot 不同 block + 同 signer
  → 提交到 SystemContract::SlashingMonitor
  → V 节点 stake 100% 罚没
  → 50% 销毁 / 30% 举报者 / 20% 保险池
```

---

## 5. 错误处理与恢复

详见 brainstorm Section 4。本节做摘要：

| 类别 | 错误 | 处理 |
|---|---|---|
| **共识级** | 双签、反向投票、proposer 超时 | 协议罚没 / slot skip / 不分叉 |
| **状态级** | state root 不一致、DB 损坏 | reject block / Snap Sync 恢复 |
| **执行级** | EVM revert / panic | tx 失败 + gas 消耗 / SECRC 热补丁 |
| **网络级** | peer 断、Eclipse、DDoS | peer score、邻居多样性、限速 |
| **API 级** | RPC 限速、MCP 写工具滥用 | 429、本地用户确认 |
| **升级级** | 版本不兼容、SECRC 热补丁验证失败 | 分叉链 / 拒绝 |

### 5.1 设计原则

- **Fail-fast**：协议级错误立即停止
- **协议不回滚**：finalized 即最终（BFT 保证）
- **节点级错误可恢复**：DB / 同步可恢复
- **罚没 permissionless**：举报者获 30% 罚没金
- **没有 EVR**：Bitcoin 式 hard fork 是唯一"回滚"
- **错误日志结构化**：tracing + JSON 输出（AI/监控友好）

---

## 6. 测试策略

### 6.1 7 层测试金字塔

```
7. 红蓝对抗（≥ 2 轮，主网前）
6. 形式化验证 TLA+（VRF/Flush/跨链/SECRC 4 项）
5. 安全审计（≥ 2 家独立）
4. 一致性 + e2e（testcontainers + ethereum/tests）
3. Fuzz（cargo-fuzz, 持续运行 ≥ 1 周无 crash）
2. 属性测试（proptest 不变量 / 序列化往返）
1. 单元测试 ★ TDD 红→绿→重构（每 fn ≥ 1 happy + 1 edge）
```

### 6.2 TDD 强制纪律

- 先写失败测试 → 写最小实现 → 重构
- CI 强制 `cargo test --workspace` 全过才能合入 main
- 覆盖率门槛：**核心 crate ≥ 95% / 其他 ≥ 80%**（`cargo llvm-cov`）
- **核心 crate 定义**（≥ 95% 覆盖率门槛）：`aii-bft` / `aii-vrf` / `aii-slashing` / `aii-evm` / `aii-state` / `aii-trie` / `aii-crypto` / `aii-registry` / `aii-pq-forklift` / `aii-il`（共 10 个）
- **其他 crate**（≥ 80%）：剩余 14 个 lib crate + 2 个 binary
- 没有对应测试的 PR 自动拒绝（CODEOWNERS + checklist）

### 6.3 每 crate 测试矩阵

| crate | 单元 | 属性 | Fuzz | 集成 | 一致性 | 基准 | 形式化 |
|---|---|---|---|---|---|---|---|
| aii-types | ✓ | ✓ | – | – | – | – | – |
| aii-codec | ✓ | ✓ | ✓ | – | – | – | – |
| aii-crypto | ✓ | ✓ | ✓ | – | – | ✓ | – |
| aii-registry | ✓ | ✓ | ✓ | – | – | – | – |
| aii-state | ✓ | ✓ | – | ✓ | – | ✓ | – |
| aii-trie | ✓ | ✓ | ✓ | – | – | ✓ | – |
| aii-evm | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | – |
| aii-aa | ✓ | ✓ | – | ✓ | ✓ | – | – |
| aii-consensus | ✓ | – | – | ✓ | – | – | – |
| aii-bft | ✓ | ✓ | – | ✓ | – | ✓ | – |
| aii-vrf | ✓ | ✓ | – | – | – | ✓ | ✓ |
| aii-slashing | ✓ | ✓ | – | ✓ | – | – | ✓ |
| aii-il | ✓ | ✓ | – | ✓ | – | – | – |
| aii-mempool | ✓ | ✓ | ✓ | ✓ | – | ✓ | – |
| aii-p2p | ✓ | – | ✓ | ✓ | – | – | – |
| aii-sync | ✓ | – | – | ✓ | – | – | – |
| aii-rpc | ✓ | – | – | ✓ | ✓ | – | – |
| aii-mcp | ✓ | – | – | ✓ | – | – | – |
| aii-pq-forklift | ✓ | ✓ | – | ✓ | – | – | ✓ |
| aii-wallet | ✓ | ✓ | – | ✓ | – | – | – |
| aii-bindings | ✓ | – | – | ✓ | – | – | – |

### 6.4 CI 配置（GitHub Actions）

```yaml
on: [pull_request, push]
jobs:
  test:
    matrix:
      rust: [stable, beta]
      os: [ubuntu-latest, macos-latest]
    steps:
      - cargo fmt --check
      - cargo clippy -- -D warnings
      - cargo test --workspace --all-features
      - cargo deny check
      - cargo audit
      - cargo llvm-cov --workspace --fail-under-lines 90
  bench-regression:
    if: github.base_ref == 'main'
    steps:
      - cargo bench --workspace
      - 对比主分支 baseline，> 10% 回退即失败
  fuzz-smoke:
    timeout-minutes: 10
    steps:
      - cargo fuzz run aii-codec/decode -- -max_total_time=300
      - cargo fuzz run aii-evm/execute -- -max_total_time=300
```

### 6.5 测试环境矩阵

| 环境 | 节点数 | 用途 |
|---|---|---|
| Devnet 本机 | 1 | 单 V 节点本地调试 |
| Devnet Docker | 4 | 共识本地集成 |
| Devnet CI | 7 | 自动化 e2e（含拜占庭场景）|
| Testnet Alpha | 21（招募） | 主网前 6 月开放 |
| Testnet Beta | 21+（公开） | 主网前 3 月，含子链/跨链 |
| Testnet 永久 | 与主网同生 | 长期保留，主网功能镜像 |
| Mainnet | ≥ 21 V + ≥ 100 全节点 | 生产 |

### 6.6 主网启动前测试清单

参考《09》§13；本 spec 增量项：

- ☐ 全部 20 crate 单测 + 属性测试通过，覆盖率达标
- ☐ Fuzz 持续 ≥ 1 周无 crash（aii-codec, aii-evm, aii-trie 至少）
- ☐ `ethereum/tests` 100% 通过 EVM Shanghai 子集
- ☐ TLA+ 4 项形式化验证完成（aii-vrf / aii-slashing / aii-pq-forklift / Flush）
- ☐ ≥ 2 家独立审计（Trail of Bits / SlowMist / Cure53 / Least Authority）
- ☐ Bug Bounty 上线，SECRC 多签 5 亿 AII 就位
- ☐ ≥ 2 轮红蓝对抗
- ☐ Testnet Alpha ≥ 6 月 / Beta ≥ 3 月稳定
- ☐ Nakamoto coefficient ≥ 7

---

## 7. 关键技术决策日志

| # | 决策 | 选择 | 理由 |
|---|---|---|---|
| D1 | 主链客户端语言 | **Rust 全栈** | 内存安全、零 GC、PQ 库生态最完善；与 Solana/Polkadot/Aptos/reth 一致 |
| D2 | EVM 实现 | **revm** | 与 reth / Foundry 共享生态，性能最佳 |
| D3 | 主链共识 | **BFT-PoS（Tendermint-style）Day-0** | 简单、稳定、易审计；Phase 5 再切 DAG-BFT |
| D4 | P2P | **libp2p**（核心节点） | 模块化、子链可换、Rust 生态首选 |
| D5 | 存储 | **RocksDB**（rust-rocksdb） | 写吞吐最强；reth 一致 |
| D6 | 默认密码学 | secp256k1 + BLS12-381（blst）+ schnorrkel VRF | 与 ETH 兼容、性能最优 |
| D7 | PQ 算法预留 | ML-DSA-65 + SLH-DSA-128s + Falcon-512 | NIST 标准化，多策略并存 |
| D8 | 状态承诺 | StateCommitment 抽象 + Day-0 用 MPT，Verkle/Binary Tree 预留 | v0.2 修正；不押注单一路径 |
| D9 | 账户模型 | EOA + EIP-4337 + EIP-7702 全部 Day-0 | 为 PQ Forklift 与 AI 钱包做基础 |
| D10 | 4 入口 mempool | RPC + P2P + LightRelay + (可选) Tor/I2P | 抗审查 Day-0 必需 |
| D11 | Inclusion List | 数据结构 Day-0；强制启用 Phase 2 | v0.2 共识 |
| D12 | RPC | ETH 兼容 JSON-RPC + GraphQL + MCP | MetaMask / 钱包 / AI 一体 |
| D13 | 总量 | **210,000,000,000 AII（2100 亿）** | 当前文档基线 |
| D14 | V 节点 S_min | 100,000 AII（10 万） | 预约启动 100 万/地址，10% 可质押 |
| D15 | 区块奖励分配 | 80% 提议者 / 20% PRE-COMMIT 见证者 | 无 DAO 国库份额，无保险基金份额 |

---

## 8. 实施路线图（高层）

详细 milestone 与人月由 **writing-plans 技能**在各 crate 单独生成。本 spec 给出粗粒度时间窗：

```
T0 - 12m ~ T0 - 10m:  Cargo workspace 骨架 + CI + L1 (aii-types/codec/config)
T0 - 10m ~ T0 - 8m:   L2 状态层 (aii-state/storage/trie) + 横切 (crypto/registry)
T0 - 8m  ~ T0 - 6m:   L3 执行层 (aii-evm/aa) + 通过 ethereum/tests
T0 - 6m  ~ T0 - 4m:   L4 共识层 (consensus/bft/vrf/slashing) + Devnet
T0 - 4m  ~ T0 - 2m:   L5 网络层 (p2p/mempool/sync/il) + Testnet Alpha
T0 - 2m  ~ T0 - 1m:   L6 API 层 (rpc/mcp) + aiid/aii apps + Testnet Beta
T0 - 1m  ~ T0:         第二轮审计 + Bug Bounty 上线 + 主网创世预演
T0:                    主网上线
```

**关键依赖**：L1 → L2 → L3 → L4 → L5 → L6 顺序展开。横切 crate 在各自最早被依赖前完成（如 aii-crypto 在 L2 之前）。

每个 milestone 在 writing-plans 阶段拆分为 ≤ 2 周的可验收单元，配套 TDD checklist + 审计 checkpoint。

---

## 9. 开放问题与待社区决定

| # | 问题 | 状态 | 决定方式 |
|---|---|---|---|
| O1 | 创世 SECRC 7 人名单 | 待提名 | 预约期 GitHub Discussions 鬆散共识 |
| O2 | Genesis 状态承诺选 MPT 还是 Verkle | Day-0 用 MPT；预留切换 | testnet 基准对比后决定 |
| O3 | Inclusion List 委员会大小（Day-0） | 8 / Phase 2 升至 16 | 沿用《13》v0.2 建议 |
| O4 | RPC 限速默认值 | TBD | 测试网压测决定 |
| O5 | 主链 chainId 数值（避免与 ETH/BSC/Polygon 冲突） | 99（占位） | 核心贡献者技术评审 |
| O6 | T0 主网上线日 | TBD | 论坛共识 |
| O7 | 是否在 Day-0 引入 EIP-7702 子集（部分协议特性）| Day-0 全引入 | 与 EIP-4337 同步 |
| O8 | 默认 PQ 算法（ML-DSA-65 vs SLH-DSA-128s） | 预留多个，默认仍 secp256k1 | Q-Day 临近时切换 |

---

## 10. 文档同步要求

如本 spec 进入实施，下列**现有文档需相应更新**（每条都附 PR 模板）：

| 文档 | 更新点 |
|---|---|
| 03 黄皮书 | §1.2 账户状态新增 `pq_pubkey`、`algo_id`；§7 密码学加 Registry；§13 参数表加 Day-0 必做 6 项参数 |
| 04 架构设计 | §3 模块清单与本 spec §3 对齐；§14 客户端架构指向 aii-bindings |
| 05 共识机制 | §2 加入 4 入口 mempool 与 IL 委员会描述 |
| 06 代币经济 | §6 V 节点抵押允许 hybrid 签名（algo_id 描述） |
| 07 社区与协议演进 | §3 协议升级流程加入"测试网→主网"硬性 checkpoint |
| 08 技术选型 | §3 EVM 选 revm 落地；§5 密码学加 liboqs-rs / pqcrypto；§9 客户端依赖 aii-bindings |
| 09 安全与威胁模型 | §3 加入 Q-Day on-spend 风险；§5 跨链桥 PQC 升级路径 |
| 10 路线图 | Phase 0-Phase 2 加入本 spec 各 milestone |
| 12 章程 | 第五章协议演进承诺：明确 PQ Forklift 钩子为不可变核心 |
| CLAUDE.md | 文档基线表加入：StateCommitment 抽象 / 4 入口 mempool / Inclusion List 数据结构 |

---

## 11. 后续步骤

1. **本 spec 完成 + 用户审阅通过**（当前阶段）
2. **写入 git 并 commit**（即将进行）
3. **invoke `superpowers:writing-plans` 技能**，为每个 crate 生成详细实施计划
4. **invoke `superpowers:executing-plans` 技能**，按 TDD 纪律逐 crate 实施

—— spec 完 ——
