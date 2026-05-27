# 13 AII 演进优化建议 — PoS 前沿与抗量子加密

> 版本：v0.2（咨询建议稿，尚未采纳入主线设计；2026-05-21 Codex 复核补充）
> 适用对象：核心贡献者、共识工程师、密码学审计方
> 关联：本文档建议性分析，可能影响《03 黄皮书》《05 共识机制详细设计》《08 技术选型决策书》《09 安全与威胁模型》。**任何最终采纳须经 SECRC + 节点运营者 rough consensus**。
> 撰写日期：2026-05-21
> 主要参考前沿：截至 2026 年 5 月的公开研究与生产实践

---

## 0. 摘要

本文档基于 2024–2026 年 PoS / BFT 共识、抗审查机制与抗量子密码学（PQC）的最新进展，对 AII 现有设计提出**三条平行优化路径**：

1. **共识层升级路径**：从当前 Tendermint 风格 BFT-PoS（6 秒 slot）→ 引入 **DAG-BFT**（Mysticeti / MonadBFT 思路）→ 子秒级最终性 + 100K+ TPS
2. **抗量子加密迁移路径**：从当前 secp256k1 + BLS12-381（**全部量子可破**）→ **crypto-agility 多签名方案**（同时支持 ML-DSA / SLH-DSA）→ Q-Day 临近时全网 PQ Forklift 迁移
3. **抗审查路径**：从"靠验证者善意不审查"→ **多入口 mempool + FOCIL 式包含列表 + 加密 mempool + 本地出块回退**，把抗审查从口号变成协议可检测、可惩罚、可恢复的机制

三条路径**正交独立**，可并行推进，**不影响主网启动**——但其中"签名算法 Registry / 账户结构预留 / 多入口交易传播"应在 Genesis Day 0 预留，否则后续迁移成本会明显上升。

### 0.1 与 Claude 原分析的对比结论

原 v0.1 分析抓住了两个关键方向：DAG-BFT 与 PQC 迁移，这两点应保留。但按 2026-05-21 公开资料复核，需要做四类修正：

| 项 | Claude 原判断 | 复核结论 | 优化动作 |
|---|---|---|---|
| 共识升级 | Tendermint → DAG-BFT 是主要性能跃迁 | 成立，但 Day-0 不应承诺 Mysticeti 级指标；应先做流水线 BFT 与数据传播解耦 | 保留 Phase 5 DAG-BFT，把 Phase 2 改为低风险工程化升级 |
| 抗审查 | 只提阈值加密 mempool / MEV 抵抗 | 不足。抗审查至少要覆盖交易入口、包含列表、builder/proposer 权力分离、私有订单流回流 | 新增 FOCIL / LUCID / AUCIL / SSLE 参考与 AII 可落地方案 |
| 抗量子 | Day-0 支持 ML-DSA / SLH-DSA，未来 PQ Forklift | 方向成立，但"冻结未迁移账户"不应作为默认承诺；应先以账户抽象与 key-rotation 降低强制冻结概率 | 增加不冻结优先原则、暴露公钥分级、on-spend 攻击响应 |
| 状态树 | Day-0 直接采用 Verkle，且视为 PQ-safe | 需要降级。Verkle 有利于小 witness，但承诺层与量子安全、SNARK 友好性仍有取舍；以太坊也在评估 Verkle / Binary Tree 路径 | 改为"状态承诺抽象层"，Genesis 可选 MPT/Verkle，但接口必须可迁移 |

---

## 1. 研究综述

### 1.1 BFT 共识最新进展（2024-2026）

| 协议 | 出处 | 关键指标 | 创新 |
|---|---|---|---|
| **Mysticeti** | Sui Mainnet（2024 上线，V2 升级 2025）| **390ms 共识 + 640ms 最终；50K-400K TPS** | 取消显式区块认证（uncertified DAG），每轮半个往返；相对 Bullshark 80% 延迟下降 / 40% CPU 下降 |
| **MonadBFT** | Monad（2025 测试网，2026 主网）| **400ms 出块；目标 10K-400K TPS** | 共识与执行解耦（共识只排序，执行异步并行）；RaptorCast 网络 + JIT EVM 编译 |
| **HotStuff-1** | 学术 2024 | 线性通信复杂度 + 投机最终性 | 比 HotStuff-2 减少 2 个网络跳 |
| **Shoal** | Aptos 2024 | DAG-BFT 延迟与鲁棒性增强 | 流水线 + leader 信誉 |
| **AlephBFT** | Aleph Zero | 子秒最终性，128 节点委员会 | DAG + PoS 混合 |
| **Tendermint / Cometbft** | Cosmos | ~6 秒区块，单区块最终 | 经典 PBFT，AII 当前对标 |

### 1.2 抗审查与 MEV 前沿（2024-2026）

抗审查不是单一功能，而是四层组合：**交易能否进入网络、能否被至少一个委员会成员观察、能否被强制进入区块、能否避免明文抢跑/选择性排序**。

| 机制 | 代表进展 | 解决的问题 | 对 AII 的启示 |
|---|---|---|---|
| **FOCIL / Inclusion Lists** | Ethereum FOCIL / EIP-7805 方向 | 单一 proposer 或 builder 不包含交易时，由委员会提交包含列表 | AII 的 V 节点委员会可在每 slot 生成 `InclusionListRoot`，延迟 1-2 slot 强制包含 |
| **LUCID / AUCIL** | 更低通信量、理性参与者激励兼容的 inclusion list 研究 | FOCIL 的带宽与激励问题 | AII Phase 2 可先实现轻量版 inclusion list，再逐步加经济惩罚 |
| **ePBS / PBS** | Ethereum EIP-7732 等研究 | 把 proposer 与 builder 权力拆开，降低单点排序权力 | AII 不宜 Day-0 引入外部 builder 市场，但应保留区块构建接口 |
| **Threshold-encrypted mempool** | Shutter / McFly / encrypted mempool 方向 | proposer 在排序前看不到明文，降低抢跑与选择性过滤 | 适合作为 Phase 2 opt-in 交易池，先保护高价值交易 |
| **SSLE / Secret Leader** | Single Secret Leader Election 研究 | 出块者身份提前暴露会被 DDoS 或定向审查 | AII VRF proposer 可先只对本地揭示，长期评估 SSLE |
| **多入口传播** | 公共 mempool + RPC gossip + light-client relay | RPC / gateway 层审查 | Genesis 即提供多 bootstrap、Tor/I2P 配置、轻节点 relay |

> **差异化决策**：AII 不应宣称"完全抗审查"。更准确的目标是：在 ≤ 1/3 stake 拒绝服务、少数 RPC 网关审查、单 proposer 作恶的情况下，用户交易可通过替代入口传播，并在有限 slot 内被 inclusion list 强制纳入或生成可罚没证据。

### 1.3 抗量子密码学（PQC）现状（2024-2026）

#### 1.3.1 NIST 标准化时间线

| 标准 | 算法 | 类型 | 用途 | 状态 |
|---|---|---|---|---|
| **FIPS 203** | ML-KEM（Kyber） | 格密码 | 密钥封装（KEM） | 2024-08 发布 |
| **FIPS 204** | ML-DSA（Dilithium） | 格密码 | 数字签名 | 2024-08 发布 |
| **FIPS 205** | SLH-DSA（SPHINCS+） | 哈希签名 | 数字签名（最保守）| 2024-08 发布 |
| HQC（待定）| 码密码 | 备用 KEM | 备用 | 2026-2027 finalize |

#### 1.3.2 性能对比（签名场景）

| 算法 | 公钥大小 | 签名大小 | 签名速度 | 验证速度 | 量子安全 |
|---|---|---|---|---|---|
| secp256k1（ECDSA）| 33 B | 71 B | 快 | 快 | ❌ |
| BLS12-381 | 48 B | 96 B | 快 | 慢（配对） | ❌ |
| Ed25519 | 32 B | 64 B | 快 | 快 | ❌ |
| **ML-DSA-65** | 1,952 B | 3,309 B | 中 | 快 | ✅ |
| **SLH-DSA-128s** | 32 B | 7,856 B | 慢 | 中 | ✅ |
| Falcon-512 | 897 B | 666 B | 慢 | 快 | ✅ |
| XMSS / LMS（有状态） | 32-68 B | 2,500 B | 中 | 快 | ✅ |

#### 1.3.3 量子威胁时间表

- **Q-Day 预测**：没有可信的单一年份。2026 年资源估计论文给出的重点不是"明天可破"，而是 ECC / secp256k1 一旦进入可用量子计算规模，链上已暴露公钥账户会成为直接目标。
- **On-spend 风险高于 HNDL**：区块链交易通常不是"加密后等待未来解密"，而是用户一旦花费 UTXO / 账户发起交易，公钥和签名公开，未来量子攻击可从公钥恢复私钥，进而抢先转移剩余资产。
- **暴露公钥分级**：长期未花费、未暴露公钥的哈希地址风险较低；已经频繁签名的账户、验证者密钥、桥多签、合约管理员密钥风险最高。
- **关键意义**：AII 主网启动当天就应支持 key rotation、签名算法 Registry 与账户抽象钱包，使用户可在 Q-Day 前迁移到 PQC 或 hybrid 账户，而不是等到全网紧急冻结。

#### 1.3.4 行业进展

| 项目 | 策略 |
|---|---|
| **Ethereum** | pq.ethereum.org 汇总了 leanVM / hash-based signatures / EIP-8141 等探索；仍处研究与 EIP 讨论阶段，不应视为已定路线 |
| **QRL** | 第一个 PQC 公链（2018 上线），XMSS → 迁移至 SPHINCS+ |
| **Bitcoin** | 尚无正式 PQC 计划；社区讨论 BIP-360 等 |
| **Solana** | 评估阶段 |
| **Cosmos / Polkadot** | 评估阶段，依赖 Substrate / Cosmos SDK 上游 |

---

## 2. AII 现有设计的差距与机会

### 2.1 共识层差距

| 维度 | AII 当前（v0.4） | 行业前沿 | 差距 |
|---|---|---|---|
| 主链 slot 时间 | 6 秒 | Mysticeti 390ms / Monad 400ms | **15× 慢** |
| 单区块 TPS（理论）| ~1,500 | Mysticeti 50K-400K | **30-260×** |
| 最终性延迟 | 单 slot ~6 秒 | Mysticeti 640ms | **9× 慢** |
| 共识架构 | 线性 BFT（类 Tendermint）| DAG-based | 架构代差 |
| 数据/执行分离 | 否 | Monad / Bullshark 是 | 缺乏 |
| MEV / 抗审查 | 未设计 | FOCIL / inclusion lists / encrypted mempool / PBS | 缺乏协议级包含保证 |
| 账户抽象 | 路线图 Phase 4 引入 | EIP-4337 已上以太坊主网；EIP-7702 已激活 | 落后 |
| 状态树 | 计划 MPT | Verkle / Binary Tree / stateless witness 多路线并行 | 需要状态承诺抽象层 |

### 2.2 密码学层差距

| 组件 | AII 当前 | 量子安全？ | 风险 |
|---|---|---|---|
| 用户账户签名 | secp256k1 ECDSA | ❌ 可破 | 高（直接对应资产）|
| V 节点签名 | BLS12-381 | ❌ 可破 | 极高（共识根）|
| PRE-COMMIT 聚合 | BLS12-381 聚合 | ❌ 可破 | 极高 |
| VRF 提议者选举 | schnorrkel（椭圆曲线）| ❌ 可破 | 高 |
| 跨链 HTLC | SHA-256（哈希）| ✅ 安全 | 低 |
| 默克尔树 | Keccak-256 | ✅ 安全 | 低 |
| 跨链桥多签 | BLS12-381 / ECDSA | ❌ 可破 | 高 |

**全部数字签名都量子可破**。AII 主网启动当天，用户账户、验证者、跨链桥与管理员密钥都必须被视为"未来可迁移资产"，不能把单一椭圆曲线签名写死进协议。

### 2.3 已具备的优势（无须改动）

- ✅ 哈希原语用 Keccak-256（PQ 安全）
- ✅ Rust 全栈（PQC 库生态最完善：blst、arkworks、liboqs-rs、pqcrypto）
- ✅ 无 DAO 治理 = 协议升级灵活，hard fork 可由社区直接驱动
- ✅ 210B AII (2100亿) 总量上限 + 公平启动 = 量子威胁不改变发行曲线本身（但会影响资产控制权与隐私）
- ✅ MCP / CLI 接入设计天然支持多种签名方案切换

---

## 3. 共识层优化方案

### 3.1 三阶段演进路径

```
Phase 0（创世）        Phase 2（远航·Plus）          Phase 5（大航海）
T0                    T0 + 9~12 月                  T0 + 18~24 月
─────┬──────────────┬─────────────────────────┬──────────────────►
 BFT-PoS v1         BFT-PoS v2 (Pipelined)    DAG-BFT v3 (Mysticeti)
 Tendermint 风格    Shoal-style 流水线         Mysticeti-inspired
 6s slot            3s slot                   < 1s 最终性
 单区块最终         单区块最终                 子秒最终
 ~1,500 TPS         ~5,000 TPS                100K+ TPS
```

### 3.2 Phase 0 → Phase 2 优化（不破坏兼容）

**保留**：协议常量、V 节点抵押结构、BFT 三阶段（PRE-VOTE / PRE-COMMIT）。

**优化**（软分叉级别）：

1. **Slot 时间**：6 秒 → 3 秒（更激进可至 2 秒）
2. **流水线**：PRE-VOTE/PRE-COMMIT 跨 slot 流水线化（Shoal 思路）
3. **VRF 提议者选举预计算**：epoch 初一次性计算所有 slot 的 proposer（已在《05》§2.3 描述），但加入**抢救式 proposer skip**——如 proposer 超时 1 秒未广播即由次顺位接替
4. **BLS 签名聚合优化**：使用 blst 的 zkSig / FrostBLS 提升 5-10× 聚合速度

### 3.3 Phase 2 → Phase 5 优化（DAG 重构，硬分叉）

**Mysticeti-inspired DAG 重构**：

```
传统 BFT（AII v1/v2）              DAG-BFT（AII v3）
proposer 出块             →       所有 V 节点并行出"区块单位"（block unit）
所有节点对单个区块投票     →       投票嵌入 DAG 边，不需要独立投票轮
单个 proposer 拖累全网     →       N 个节点同时贡献，单点慢不阻塞
6s 串行                    →       <1s 并行
```

**关键 DAG-BFT 设计要点**：

- **Uncertified blocks**（Mysticeti 思路）：V 节点直接广播自己的"区块单位"，不需要先收集 ⅔ 认证再扩散
- **DAG 边即投票**：当一个区块单位 b 在 DAG 中被 ≥ ⅔ stake 加权的后续单位"指向"（包含其哈希），它即被视为 finalized
- **数据/执行解耦**（Monad 思路）：共识只负责确定**交易顺序**；EVM/WASM 执行**异步并行**进行，不阻塞下一区块
- **TPS 跳升**：从 Phase 2 的 5K TPS 跃升至 100K+

> **差异化决策（建议）**：DAG-BFT 引入须谨慎——这是主链协议根本性变更，需 ≥ 6 个月稳定测试网验证 + ≥ 2 轮独立审计 + 节点运营者 ⅔ stake 同意 hard fork。

### 3.4 配套优化

#### 3.4.1 状态承诺：抽象优先，避免押注单一路径

| 项 | MPT（AII 当前计划）| Verkle Tree（建议）|
|---|---|---|
| 状态见证大小 | ~10-50 KB / 账户 | **~200 B / 账户**（缩小 50-250×）|
| 无状态客户端可行性 | 不可行 | **可行**（轻节点跑在浏览器内）|
| 实现复杂度 | 成熟（geth/erigon）| 中等（go-verkle / verkle-trie crate） |
| 量子安全 | ✅（hash-based）| 取决于向量承诺；KZG / IPA 不应默认视为严格 PQ-safe |

**修正建议（v0.2）**：不要把"Verkle Tree"写成不可替换的创世承诺，而应把**状态承诺抽象层**写入 Genesis：

```rust
trait StateCommitment {
    type Witness;
    type Proof;

    fn root(&self) -> H256;
    fn prove(keys: &[StateKey]) -> Self::Witness;
    fn verify(root: H256, witness: Self::Witness) -> bool;
    fn migrate_epoch(old_root: H256, new_scheme: CommitmentSchemeId) -> MigrationRoot;
}
```

Genesis 可在 MPT / Verkle / Binary Merkle Tree 三者中择一，但协议层必须从第一天支持 `commitment_scheme_id`。理由：

- Verkle 对 witness size 有明确优势，EIP-6800 目标约 200B/account witness；
- 但 Verkle 依赖向量承诺，PQC 与 SNARK/STARK 友好性仍需评估；
- Binary Merkle Tree 牺牲部分 witness 大小，但更接近 hash-based PQ 安全与 zk 证明工程路径；
- AII 不背负以太坊历史状态包袱，应把"可迁移状态承诺"作为设计基线，而不是押注单一树。

#### 3.4.2 MEV 抵抗：协议级加密 mempool

- 主网启动即引入 **threshold-encrypted mempool**（McFly / Shutter Network 思路）
- 用户交易先以阈值加密公钥加密提交，达成共识后由 V 节点集合阈值解密
- 防止 proposer 看到明文交易后抢跑

#### 3.4.3 抗审查：FOCIL 式包含列表

阈值加密 mempool 主要解决 MEV / 抢跑，不足以解决审查。AII 应新增**包含列表（Inclusion List, IL）**：

```
slot n:
  V 节点委员会观察 mempool
  每个 IL 委员提交 InclusionList = {tx_hash, first_seen_time, fee_cap}
  区块头写入 inclusion_list_root

slot n+1 / n+2:
  proposer 必须包含符合 gas / nonce / fee 条件的 listed tx
  如不包含，需给出 invalid_reason
  其他 V 节点验证 invalid_reason；无理由遗漏则拒绝 PRE-COMMIT 或触发轻罚没
```

**AII 可落地规则**：

| 规则 | 建议值 | 理由 |
|---|---:|---|
| IL 委员数量 | 每 slot 8-16 个 V 节点，VRF 抽样 | 降低带宽，不依赖单 proposer |
| 强制包含延迟 | 2 slots | 给 proposer 留 nonce / gas 检查窗口 |
| 交易上限 | 每 IL 最多 `《[占位] 256》` 条或 `《[占位] 1 MB》` | 防止 DoS |
| 无效原因 | nonce too low / insufficient fee / gas limit / already included / invalid signature | 限定枚举，便于验证 |
| 罚没 | 首期仅拒绝投票；Phase 2 后轻罚没 | 避免早期误罚 |

> **差异化决策**：AII 的抗审查优先级高于外部 builder 市场。Genesis 应实现公共 mempool + 多入口传播 + IL 数据结构；Phase 2 再启用强制包含与罚没。

#### 3.4.4 账户抽象：Genesis 即支持 EIP-4337 + EIP-7702

- 主网启动即原生支持 EIP-4337（智能合约钱包）
- 同时支持 EIP-7702（EOA 临时委托给合约）
- **关键意义**：账户抽象是 PQC 迁移的核心抓手——用户可通过 4337 钱包随时切换签名算法

---

## 4. 抗量子加密迁移方案

### 4.1 设计哲学：Crypto-Agility（密码学敏捷性）

**不以"一次性切换到 PQC"为目标**，而是**从 Genesis Day 0 就支持多签名方案并存**：

```
┌──────────────────────────────────────────────────────────┐
│  AII 协议签名层（Multi-Sig Scheme Registry）              │
├──────────────────────────────────────────────────────────┤
│  ID    算法            状态        默认场景              │
│  0x01  secp256k1       默认        与 ETH 兼容、低价值     │
│  0x02  Ed25519         可选        高性能场景             │
│  0x03  ML-DSA-65       PQC 主推    新账户、高价值         │
│  0x04  SLH-DSA-128s    PQC 保守    跨链桥、归档存证        │
│  0x05  Hybrid (0x01+0x03)  推荐    高价值账户             │
│  0x06  Falcon-512      可选        签名小、PQC            │
│  0x07  (预留)          未来        新算法                 │
└──────────────────────────────────────────────────────────┘
```

每笔交易、每个抵押操作携带 **1 字节算法 ID**，让客户端验证不同签名类型。

### 4.2 三阶段迁移路径

#### Phase A：Genesis Day 0（主网启动）

- 默认仍是 **secp256k1**（保持 MetaMask 兼容、保持以太坊生态接入）
- **同时支持 ML-DSA 与 SLH-DSA**（用户可选）
- V 节点抵押允许选择 BLS12-381 或 ML-DSA
- 协议**保留** PQ Forklift 升级钩子（详见 §4.4）

#### Phase B：Q-Day 警报触发（外部触发，不绑定固定年份）

当下列任一条件成立时，启动"PQ Forklift 准备期"：

- NIST / 国家安全机构正式发布 Q-Day 警告
- Google / IBM / 公开论文宣称破解了 ≥ 256-bit ECC
- Ethereum / Bitcoin 等主流公链启动 PQC 强制迁移

行动：

- **V 节点必须使用 hybrid 签名**（secp256k1 + ML-DSA 双签）
- **新账户默认 ML-DSA**（钱包客户端默认改为 PQC）
- **高风险账户预警**：已暴露公钥、桥多签、合约管理员、验证者热密钥进入强提醒迁移名单
- **设立 PQ 迁移激励**：用户主动迁移到 PQC 账户的，可享受 1 年内免 base_fee 销毁

#### Phase C：PQ Forklift（Q-Day 后）

通过 hard fork 执行：

- **不冻结优先原则**：默认不冻结普通账户，优先通过钱包强提醒、迁移激励、交易费优惠、账户抽象批量迁移完成过渡
- **限制高风险操作**：对已暴露公钥且仍使用 secp256k1 的高价值账户，可先限制合约管理员调用、桥提款、验证者签名等系统性风险操作
- **最后手段冻结**：只有在公开量子攻击已能实际恢复 secp256k1 私钥，且社区 rough consensus 明确支持时，才考虑冻结未迁移账户
- 每个用户通过原私钥签一笔"PQ 重签名"交易，将余额迁移到新 ML-DSA 公钥派生的地址
- 已签未冻结的合约通过"账户抽象升级"机制更换底层签名验证逻辑
- 跨链桥多签强制全部升级为 PQC

### 4.3 V 节点签名层：从 BLS12-381 到 STARK 聚合

BLS 聚合签名是 AII 共识效率的关键（O(1) 验证 N 个签名）。但 BLS12-381 量子可破。

**两种 PQC 替代方案**：

| 方案 | 描述 | 优势 | 劣势 |
|---|---|---|---|
| **STARK 聚合** | 用 STARK 证明"≥ ⅔ 节点对区块签名"，验证哈希为单一 STARK 证明 | 量子安全（hash-based）；可证明 N 个签名为 O(log N) 大小 | 证明生成慢（每 slot 数秒到数十秒）；当前研究阶段 |
| **格基聚合签名** | 类 Dilithium 聚合（lattice-based aggregation）| 验证快；PQ 安全 | 签名大；技术尚不成熟 |
| **Hybrid BLS + STARK** | Phase B：BLS + STARK 双重；Phase C：仅 STARK | 平滑过渡 | 实现复杂度 |

**建议**：

- Phase A：继续 BLS12-381（性能优先）
- Phase B：研究 STARK 聚合的工程可行性（参考 Polygon Plonky3 / Linea / Starkware Stwo）
- Phase C：切换到 STARK 聚合或格基聚合

### 4.4 PQ Forklift 协议级钩子

主网 Genesis 即写入下列**预留机制**（避免未来 PQC 迁移时需要协议重大变更）：

1. **多签名 Registry 合约**：维护 `(算法 ID → 验证字节码)` 映射，可通过 hard fork 新增算法
2. **账户结构扩展**：账户状态 `σ[a]` 新增 `pq_pubkey` 槽位（默认空），存储用户预注册的 PQ 公钥
3. **签名验证函数泛化**：交易验证逻辑接受 `(algoId, pubkey, sig)` 三元组，根据 algoId 调度
4. **迁移操作码**：新增 `MIGRATE_PQ` 操作码，允许账户从 algoId X 迁移到 algoId Y（需用旧密钥签名授权）

### 4.5 VRF / 随机性的抗量子升级

当前 `schnorrkel` VRF 基于椭圆曲线，量子可破。替代：

| 方案 | 描述 | PQ |
|---|---|---|
| **HashVRF**（基于哈希）| 单纯哈希函数 + 阈值签名 | ✅ |
| **Lattice VRF** | 基于格密码 | ✅ |
| **drand** | 阈值 BLS 随机信标（外部）| ❌（BLS 量子可破）|
| **Verifiable Delay Function (VDF)** | 时间锁链 | 部分 ✅ |

**建议**：Phase A 继续 schnorrkel；Phase B 启动 HashVRF 评估；Phase C 切换。

### 4.6 On-spend / HNDL 数据隐私防护

由于链上所有交易公开，on-spend 与 HNDL 风险无法完全消除，但可缓解：

- **公钥延迟暴露**：钱包默认使用一次性地址 / 合约钱包，尽量避免长期复用同一 EOA 公钥
- **私密交易选项**：用户可选用 zk-STARK 隐私交易（隐藏金额或关联关系）
- **隐私子链**：基于 STARK 的隐私子链（类似 Aleo / Aztec 模式）
- **批量迁移交易**：Q-Day 预警期支持批量 PQ 迁移，降低攻击者根据单笔迁移交易抢跑的窗口

不强制——大部分交易仍走公开模式（兼容 EVM 生态）。

---

## 5. 完全去中心化与抗审查优化方案

### 5.1 目标边界

AII 的文档基线是"完全去中心化、无公司、无基金会、无 DAO、无链上投票"。工程上应把这个目标拆成可验证指标：

| 维度 | 不足做法 | AII 应采用的做法 |
|---|---|---|
| 验证者 | 只要求 stake 足够 | 限制单实体权重、鼓励地理/ASN/客户端多样性、公开可观测去中心化指标 |
| 交易入口 | 依赖少数 RPC | 多 RPC、多 bootstrap、P2P 直连提交、轻节点 relay、Tor/I2P 可选 |
| 出块权力 | 单 proposer 决定内容 | inclusion list + proposer skip + 本地出块回退 |
| Builder/MEV | 私有订单流黑箱 | 公共 mempool 优先，私有流必须回流到可审计接口 |
| 客户端 | 单一官方实现 | Rust 主实现 + 最小验证客户端规范 + 独立轻客户端 |
| 升级 | 多签或 DAO 决定 | 版本发布 + 节点运营者自愿升级 + rough consensus |

> **差异化决策**：AII 的抗审查优先级排序应为：交易可传播 > 交易可被强制包含 > 节点可独立验证 > 性能优化。任何提升 TPS 的方案如果引入中心化 RPC、中心化 builder 或高硬件门槛，都不得进入 Genesis 基线。

### 5.2 多入口交易传播

Genesis Day 0 应实现四种交易入口，避免 RPC 层成为审查瓶颈：

| 入口 | 说明 | 抗审查价值 |
|---|---|---|
| Public RPC | 常规钱包 / DApp 使用 | 低摩擦，但不可单点依赖 |
| P2P tx gossip | 钱包或轻节点直接向 P2P 网络广播 | 绕过中心化 RPC |
| Light-client relay | T6/T7 节点转发交易哈希与原文 | 移动端可参与传播 |
| Tor/I2P bootstrap | 可选匿名网络入口 | 在网络封锁环境下保留提交通道 |

最低实现要求：

- `aii tx send --p2p`：CLI 可跳过 RPC，直接向多个 peers 广播；
- `aii tx prove-seen`：返回交易被哪些 V 节点 / relay 首次观察的证明；
- `TxSeenReceipt`：由节点签名，包含 `tx_hash / first_seen_slot / peer_id / node_sig`；
- 钱包默认广播到 `N >= 3` 个入口，且不把单一 RPC 失败误报为链上失败。

### 5.3 Inclusion List 与有限 slot 包含保证

目标不是保证任何垃圾交易都上链，而是保证**有效且愿意支付合理费用的交易不会被单个 proposer 或少数 builder 无限期过滤**。

建议写入《03 黄皮书》的规则：

```text
valid_for_inclusion(tx, state, base_fee):
  signature_valid(tx)
  nonce_executable_or_future_bounded(tx, max_nonce_gap)
  tx.max_fee_per_gas >= base_fee
  tx.gas_limit <= block_gas_limit
  not already_included(tx.hash)
```

若 `tx` 在 slot `s` 被至少 `k` 个 IL 委员签入 inclusion list，则 proposer 必须在 `s + Δ` 前：

1. 包含该交易；或
2. 给出可验证的 `invalid_reason`；或
3. 面临其他 V 节点拒绝 PRE-COMMIT，Phase 2 后增加轻罚没。

建议参数：

| 参数 | Genesis | Phase 2 |
|---|---:|---:|
| `IL_COMMITTEE_SIZE` | 8 | 16 |
| `IL_THRESHOLD` | 3 | 5 |
| `IL_FORCE_DELAY` | 2 slots | 2 slots |
| `IL_MAX_TXS` | 128 | 256 |
| `IL_PENALTY` | 0，仅拒绝投票 | `《[占位] 0.01%》` 抵押 |

### 5.4 Builder / PBS 的保守引入

AII 不应在 Genesis 引入中心化 builder 市场。推荐顺序：

1. **Genesis**：只有本地构块，V 节点必须能独立从公共 mempool 构建区块；
2. **Phase 2**：开放 builder API，但 proposer 必须验证区块、合并 IL 交易、保留本地 fallback；
3. **Phase 5**：如 MEV 市场成熟，再评估 ePBS / PBS，把 builder 承诺与执行负载从共识中隔离。

硬性规则：

- builder 不能绕过 inclusion list；
- proposer 不能把"builder 未提供区块"作为漏块免责理由；
- 私有订单流必须在 `《[占位] 2》` slots 内向公共 mempool 或 IL 证明层回流；
- 任何外部 builder 协议不得要求 KYC、许可准入或中心化 API 密钥。

### 5.5 验证者去中心化指标

文档应新增可公开观测的去中心化面板：

| 指标 | 建议阈值 | 说明 |
|---|---:|---|
| Nakamoto coefficient（stake） | ≥ 7，长期目标 ≥ 15 | 控制 1/3 stake 所需实体数 |
| 单实体有效权重上限 | `S_max(V)` 已有规则继续保留 | 防止大户用单节点垄断 |
| ASN 集中度 | 单 ASN < 25% 活跃 stake | 防止云厂商/机房故障导致停摆 |
| 客户端集中度 | 单客户端 < 80% 活跃 stake | 早期可豁免，但需接口规范 |
| 地理集中度 | 单司法辖区 < 40% 活跃 stake | 降低监管审查风险 |
| RPC 入口集中度 | 前 3 RPC < 60% 钱包默认流量 | 钱包必须支持自定义 RPC 与 P2P |

这些指标不作为链上治理投票条件，只作为客户端、区块浏览器和社区报告的公开风险信号。

---

## 6. 协议常量调整建议

下表为本文档对 AII 现有协议常量的**建议调整值**（须 hard fork）：

| 常量 | 当前值（v0.4）| 建议值（Phase 2）| 建议值（Phase 5）|
|---|---|---|---|
| 主链 slot 时间 | 6 秒 | 3 秒 | < 1 秒（DAG）|
| epoch 长度 | 32 slots | 32 slots | DAG 模式下重新定义 |
| `S_min(V)` | 100,000 AII | 100,000 AII | 100,000 AII |
| 签名算法默认 | secp256k1 | secp256k1 | ML-DSA |
| VRF 算法 | schnorrkel | schnorrkel | HashVRF |
| 状态承诺 | MPT | **StateCommitment 抽象 + MPT/Verkle 二选一** | Verkle / Binary Tree 可迁移 |
| 区块奖励初始 | 2 AII | 2 AII | 2 AII（不变）|
| MEV 防护 | 无 | 阈值加密 mempool（opt-in） | 同 + Time Boost / PBS 评估 |
| 抗审查 | 公共 mempool | 多入口传播 + IL 数据结构 | 强制 IL + 轻罚没 |
| 账户抽象 | 路线图 Phase 4 | **创世即支持 EIP-4337/7702** | 同 |

> **强烈建议**：将 StateCommitment 抽象、EIP-4337/7702、多签名 Registry、多入口交易传播与 IL 数据结构 **提前到主网 Genesis** 引入。这些接口一旦缺失，后续需要高风险硬分叉才能补齐。

---

## 7. 实施路线图建议

```
Phase 0  Genesis（T0）
├─ 仍用 BFT-PoS Tendermint 风格（6s slot，~1.5K TPS）
├─ ★ StateCommitment 抽象层（MPT/Verkle/Binary Tree 可迁移）
├─ ★ 多签名 Registry（默认 secp256k1，可选 ML-DSA / SLH-DSA）
├─ ★ EIP-4337 + EIP-7702 账户抽象（创世即支持）
├─ ★ PQ Forklift 钩子（账户结构、迁移操作码）
└─ ★ 多入口交易传播 + Inclusion List 数据结构

Phase 2  远航·Plus（T0 + 9-12 月）
├─ Slot 时间 6s → 3s（共识参数 hard fork）
├─ 流水线 BFT（Shoal 思路）
├─ 阈值加密 mempool（MEV 抵抗，opt-in）
├─ FOCIL 式强制包含 + 轻罚没
├─ Builder API（必须保留本地 fallback）
└─ V 节点 hybrid 签名建议（BLS + ML-DSA）

Phase 5  大航海（T0 + 18-24 月）
├─ DAG-BFT 切换（Mysticeti-inspired）
├─ 数据/执行解耦（Monad 思路）
├─ 子秒最终性 + 100K+ TPS
├─ 隐私子链上线（zk-STARK）
└─ STARK 聚合签名研究

Phase Q  Q-Day 警报响应（外部触发，不绑定固定年份）
├─ V 节点强制 hybrid → 纯 PQ 签名迁移
├─ 新账户默认 ML-DSA
├─ PQ Forklift hard fork（不冻结优先，高风险操作先限制）
└─ HashVRF 切换
```

**★** 标记为**建议提前到 Genesis** 的项目——后续引入成本高。

---

## 8. 风险与权衡

### 8.1 共识 DAG 化的风险

| 风险 | 缓解 |
|---|---|
| DAG-BFT 工程复杂度高，bug 风险大 | Phase 5 时机较晚（T0+18 月），已积累 1.5 年 BFT 运维经验 |
| 实现参考较少（Sui / Aptos 是主要参考） | 与 Mysticeti / MonadBFT 团队公开技术交流；可贡献 reth-style 开源实现 |
| 节点硬件需求上升 | 7-Tier 节点模型本身已支持高配节点；DAG 主要影响 T1-T2 |

### 8.2 多签名 Registry 的风险

| 风险 | 缓解 |
|---|---|
| 验证逻辑复杂化，攻击面增大 | 严格审计每个新增 algoId 的实现；上线前形式化验证 |
| 用户混淆不同签名类型 | 钱包 UI 强制提示；默认仍是 secp256k1 |
| 跨链桥可能仅支持某些 algoId | 桥协议层做 algoId 转换；HTLC 等机制不依赖具体签名算法 |

### 8.3 状态承诺抽象的风险

| 风险 | 缓解 |
|---|---|
| Verkle Tree 实现库尚不如 MPT 成熟 | Genesis 不绑定单一路径，先抽象 `StateCommitment`，测试网对比 MPT / Verkle / Binary Tree |
| 工具链（Etherscan-like 浏览器）需适配 | Blockscout 已计划支持 |
| 状态迁移复杂 | 迁移只允许 epoch 边界执行，输出 `MigrationRoot` 并保留旧 witness 验证窗口 |

### 8.4 提前引入 EIP-4337/7702 的风险

| 风险 | 缓解 |
|---|---|
| 攻击面比 EOA 大（合约钱包逻辑漏洞） | 推荐使用 Safe / Argent / Biconomy 等已审计合约钱包模板 |
| 用户教育成本 | CLI / 钱包默认值仍是 EOA + secp256k1，AA 是 opt-in |

### 8.5 Inclusion List / 抗审查机制的风险

| 风险 | 缓解 |
|---|---|
| IL 被垃圾交易填满，形成 DoS | 限制 `IL_MAX_TXS`、要求最低 fee、只接受可执行或有限未来 nonce |
| 节点对 `first_seen` 作假 | 多节点 `TxSeenReceipt` 交叉验证，Phase 2 前不直接重罚 |
| 强制包含降低 builder 收益 | IL 优先级高于 builder 收益；builder API 必须合并 IL |
| 加密 mempool 解密失败 | 先 opt-in，不作为普通交易唯一入口；保留公共 mempool fallback |

---

## 9. 决策矩阵：哪些建议立即采纳

按"影响 / 紧迫 / 实施成本"评估每个建议：

| 建议 | 影响 | 紧迫 | 成本 | 建议 |
|---|---|---|---|---|
| StateCommitment 抽象层 | 🔴 高 | 🔴 高 | 🟡 中 | ✅ **强烈建议 Genesis 引入** |
| EIP-4337/7702 账户抽象 | 🔴 高 | 🟡 中 | 🟢 低 | ✅ **强烈建议 Genesis 引入** |
| 多签名 Registry（PQC 准备）| 🔴 高 | 🔴 高 | 🟡 中 | ✅ **强烈建议 Genesis 引入** |
| PQ Forklift 钩子 | 🔴 高 | 🟡 中 | 🟢 低 | ✅ **强烈建议 Genesis 引入** |
| 多入口交易传播 | 🔴 高 | 🔴 高 | 🟢 低 | ✅ **强烈建议 Genesis 引入** |
| Inclusion List 数据结构 | 🔴 高 | 🔴 高 | 🟡 中 | ✅ **Genesis 预留，Phase 2 强制** |
| Slot 6s → 3s | 🟡 中 | 🟢 低 | 🟢 低 | ✅ Phase 2 |
| 阈值加密 mempool | 🟡 中 | 🟡 中 | 🟡 中 | 🟡 Phase 2 评估 |
| DAG-BFT（Mysticeti）| 🔴 高 | 🟢 低 | 🔴 高 | 🟡 Phase 5（充分测试后）|
| STARK 聚合 V 节点签名 | 🔴 高 | 🟢 低 | 🔴 高 | 🟡 Phase 5+ 或 Q-Day 触发 |
| HashVRF 替代 schnorrkel | 🟡 中 | 🟢 低 | 🟡 中 | 🟡 Phase 5+ 或 Q-Day 触发 |
| 隐私子链（STARK）| 🟡 中 | 🟢 低 | 🟡 中 | 🟡 Phase 5 |

**Day-0 必做 6 项**（如不引入，后期成本将 10-100×）：

1. ✅ **StateCommitment 抽象层**
2. ✅ **EIP-4337 + EIP-7702 账户抽象**
3. ✅ **多签名 Registry**（即便 PQ 算法 Day 0 仅注册不强制使用）
4. ✅ **PQ Forklift 协议钩子**
5. ✅ **多入口交易传播**
6. ✅ **Inclusion List 数据结构**

---

## 10. 对现有文档的修订建议

如本文档的建议被采纳，下列现有文档需同步更新：

| 文档 | 修订点 |
|---|---|
| 03 黄皮书 | §1.2 账户状态新增 `pq_pubkey` 字段；§4 共识考虑 DAG-BFT；§7 密码学增加多签名 Registry；§13 参数表；新增 inclusion list 状态根 |
| 04 架构设计文档 | txpool / p2p / rpc 新增多入口传播、`TxSeenReceipt`、Tor/I2P bootstrap、builder API fallback |
| 05 共识机制详细设计 | §2 slot 时间路线图；新增 DAG-BFT 章节；新增多签名章节；新增 FOCIL 式强制包含规则 |
| 06 代币经济与分配模型 | §6 V 节点抵押允许 hybrid 签名；§12 协议常量更新 |
| 08 技术选型决策书 | §3 EVM 部分加入 StateCommitment 抽象；§5 密码学库新增 liboqs-rs / pqcrypto；§6 共识加入 DAG-BFT 评估 |
| 09 安全与威胁模型 | §3 新增 Q-Day / on-spend 威胁；§5 跨链桥 PQC 升级路径；§11 形式化验证加入 PQ 算法；新增 RPC/Builder 审查威胁 |
| 10 路线图 | Phase 2 / Phase 5 / Phase Q 新增任务；Genesis 增加多入口传播和 IL 数据结构 |
| 12 章程 | 第五章协议演进承诺：明确 PQ Forklift 钩子为不可变核心的一部分 |

---

## 11. 决策待回填

| # | 问题 | 候选 | 决定 |
|---|---|---|---|
| 1 | Genesis 状态承诺选型 | MPT / Verkle / Binary Tree；但 `StateCommitment` 抽象必须引入 | 待测试网基准 |
| 2 | 默认 PQC 签名选 ML-DSA 还是 SLH-DSA？ | ML-DSA-65（性能优）/ SLH-DSA-128s（最保守）| 待审计 |
| 3 | V 节点 hybrid 签名 Day-0 强制还是 opt-in？ | 强制 / opt-in | 待评估生态阻力 |
| 4 | DAG-BFT 切换前是否设独立 testnet？ | 是（强烈建议）/ 否 | – |
| 5 | 隐私子链优先级 | 高 / 中 / 低 | 待社区需求评估 |
| 6 | Q-Day 警报触发标准 | NIST 公告 / 公开破解事件 / 主流公链先迁移 | 待章程修订 |
| 7 | Inclusion List 强制启用时间 | Genesis / Phase 2 | 建议 Genesis 预留、Phase 2 强制 |
| 8 | Builder API 是否开放 | 不开放 / Phase 2 opt-in / Phase 5 ePBS | 建议 Phase 2 opt-in |

---

## 12. 参考资料（截至 2026-05）

### PoS / BFT 共识

- Mysticeti: Reaching the Latency Limits with Uncertified DAGs（Sui，2024）
- MonadBFT 与 RaptorCast 网络（Monad，2025-2026）
- Bullshark: DAG BFT Protocols Made Practical（Aptos / Sui）
- HotStuff-1: Linear Consensus with One-Phase Speculation（2024）
- Shoal: Improving DAG-BFT Latency And Robustness（Aptos，2024）
- AlephBFT: A DAG and PoS Hybrid（Aleph Zero）
- Ethereum Single Slot Finality（vbuterin notes）

### 抗量子加密

- NIST FIPS 203 / 204 / 205（ML-KEM / ML-DSA / SLH-DSA），2024-08
- Google Quantum AI / arXiv: 256-bit ECC / secp256k1 量子攻击资源估算（2026）
- pq.ethereum.org — Post-Quantum Ethereum
- Federal Reserve: Harvest Now, Decrypt Later 对分布式账本网络的影响（2025）
- QRL 文档（XMSS / SPHINCS+ 迁移）
- Ethereum EIP-8141 提案（leanXMSS 验证者签名）
- liboqs / pqcrypto Rust crate

### 抗审查 / MEV / PBS

- EIP-7805: Fork-choice enforced Inclusion Lists（FOCIL）
- LUCID: Lower-cost Unconditional Inclusion Lists
- AUCIL: Auction-based Conditional Inclusion Lists
- EIP-7732: Enshrined Proposer-Builder Separation（ePBS）
- Shutter Network / McFly: threshold-encrypted mempool
- Single Secret Leader Election（SSLE）相关论文

### Verkle Trees 与状态层

- Ethereum Verkle Trees 文档与 EIP-6800
- Banderwagon: A curve made of banderwagons for Verkle trees
- Ethereum statelessness / Binary Tree 研究路线

### 账户抽象

- EIP-4337: Account Abstraction via Entry Point Contract
- EIP-7702: Set EOA account code

---

## 13. 总结

AII 当前设计（v0.4）在**清晰性、极简哲学、公平启动**方面达到了优秀水准，但相对 2024-2026 年最新前沿存在四个**架构代差**：

1. **共识架构代差**：仍是 Tendermint-style 串行 BFT，相比 DAG-based Mysticeti / MonadBFT 慢 1-2 个数量级
2. **抗量子代差**：全套数字签名都量子可破，Q-Day 临近时需要紧急迁移
3. **抗审查代差**：当前缺少多入口传播、inclusion list、builder fallback 与可罚没的包含保证
4. **状态层代差**：MPT 见证过大；但 Verkle / Binary Tree 路线仍需基准对比，因此应先引入状态承诺抽象

**最低成本的优化方向**（Day-0 引入）：

- ✅ StateCommitment 抽象层
- ✅ 多签名 Registry（PQC 准备）
- ✅ EIP-4337/7702 账户抽象
- ✅ PQ Forklift 协议钩子
- ✅ 多入口交易传播
- ✅ Inclusion List 数据结构

这 6 项在 Day-0 引入的工程量约 8-12 人月，但能将后期 PQ 迁移、状态升级与抗审查改造的成本降低 10-100×。

**中期优化**（Phase 2，T0+9-12 月）：

- 🟡 Slot 时间 6s → 3s
- 🟡 流水线 BFT
- 🟡 阈值加密 mempool
- 🟡 FOCIL 式强制包含 + 轻罚没

**长期优化**（Phase 5，T0+18-24 月）：

- 🟡 DAG-BFT 切换（Mysticeti 路径）
- 🟡 V 节点 hybrid → STARK 聚合签名

**应急路径**（Q-Day 警报触发）：

- 🔴 PQ Forklift hard fork（不冻结优先，高风险操作先限制）

—— 本演进优化建议完 ——

> **下一步**：本文档为 v0.2 咨询稿。建议在 GitHub Discussions 开启专题讨论；3 个月内若社区共识倾向采纳，可作为 v0.5 设计基线纳入正式文档（同步修订 03/04/05/06/08/09/10/12）。
