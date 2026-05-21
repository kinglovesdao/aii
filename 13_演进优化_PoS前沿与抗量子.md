# 13 AII 演进优化建议 — PoS 前沿与抗量子加密

> 版本：v0.1（咨询建议稿，尚未采纳入主线设计）
> 适用对象：核心贡献者、共识工程师、密码学审计方
> 关联：本文档建议性分析，可能影响《03 黄皮书》《05 共识机制详细设计》《08 技术选型决策书》《09 安全与威胁模型》。**任何最终采纳须经 SECRC + 节点运营者 rough consensus**。
> 撰写日期：2026-05-21
> 主要参考前沿：截至 2026 年 5 月的公开研究与生产实践

---

## 0. 摘要

本文档基于 2024–2026 年 PoS / BFT 共识与抗量子密码学（PQC）的最新进展，对 AII 现有设计提出**两条平行优化路径**：

1. **共识层升级路径**：从当前 Tendermint 风格 BFT-PoS（6 秒 slot）→ 引入 **DAG-BFT**（Mysticeti / MonadBFT 思路）→ 子秒级最终性 + 100K+ TPS
2. **抗量子加密迁移路径**：从当前 secp256k1 + BLS12-381（**全部量子可破**）→ **crypto-agility 多签名方案**（同时支持 ML-DSA / SLH-DSA）→ Q-Day 临近时全网 PQ Forklift 迁移

两条路径**正交独立**，可并行推进，**不影响主网启动**——可作为 Phase 2（远航·Plus）与 Phase 5（大航海）的演进目标。

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

### 1.2 抗量子密码学（PQC）现状（2024-2026）

#### 1.2.1 NIST 标准化时间线

| 标准 | 算法 | 类型 | 用途 | 状态 |
|---|---|---|---|---|
| **FIPS 203** | ML-KEM（Kyber） | 格密码 | 密钥封装（KEM） | 2024-08 发布 |
| **FIPS 204** | ML-DSA（Dilithium） | 格密码 | 数字签名 | 2024-08 发布 |
| **FIPS 205** | SLH-DSA（SPHINCS+） | 哈希签名 | 数字签名（最保守）| 2024-08 发布 |
| HQC（待定）| 码密码 | 备用 KEM | 备用 | 2026-2027 finalize |

#### 1.2.2 性能对比（签名场景）

| 算法 | 公钥大小 | 签名大小 | 签名速度 | 验证速度 | 量子安全 |
|---|---|---|---|---|---|
| secp256k1（ECDSA）| 33 B | 71 B | 快 | 快 | ❌ |
| BLS12-381 | 48 B | 96 B | 快 | 慢（配对） | ❌ |
| Ed25519 | 32 B | 64 B | 快 | 快 | ❌ |
| **ML-DSA-65** | 1,952 B | 3,309 B | 中 | 快 | ✅ |
| **SLH-DSA-128s** | 32 B | 7,856 B | 慢 | 中 | ✅ |
| Falcon-512 | 897 B | 666 B | 慢 | 快 | ✅ |
| XMSS / LMS（有状态） | 32-68 B | 2,500 B | 中 | 快 | ✅ |

#### 1.2.3 量子威胁时间表

- **Q-Day 预测**：最早 2035 年（Google Quantum AI 2026-03 评估：破解 256-bit ECC 需约 1,200 个逻辑量子比特）
- **Harvest-Now-Decrypt-Later (HNDL)**：联邦储备 2026 研究指出，**区块链由于所有交易公开永久不可篡改，受 HNDL 威胁尤为严重**——攻击者今天抓取链上数据，Q-Day 后可解密所有历史交易
- **关键意义**：即便 AII 主网在 Q-Day 前正常运行，**今天产生的所有签名都可能在未来被破解**——主网启动当天就应有 PQC 迁移路线

#### 1.2.4 行业进展

| 项目 | 策略 |
|---|---|
| **Ethereum** | Post-Quantum Security 团队（2026-01 成立）；leanXMSS + leanVM zkVM 压缩签名（250×）；EIP-8141 拟入 Hegotá 硬分叉（2026 H2）|
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
| MEV 抵抗 | 未设计 | 加密 mempool / PBS / Time Boost 等多种 | 缺乏 |
| 账户抽象 | 路线图 Phase 4 引入 | EIP-4337 已上以太坊主网；EIP-7702 已激活 | 落后 |
| 状态树 | 计划 MPT | Verkle Tree（以太坊 2026 H2 启动）| 落后 |

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

**全部数字签名都量子可破**。AII 主网启动当天，所有链上签名都暴露于 HNDL 风险。

### 2.3 已具备的优势（无须改动）

- ✅ 哈希原语用 Keccak-256（PQ 安全）
- ✅ Rust 全栈（PQC 库生态最完善：blst、arkworks、liboqs-rs、pqcrypto）
- ✅ 无 DAO 治理 = 协议升级灵活，hard fork 可由社区直接驱动
- ✅ 21M AII 总量上限 + 公平启动 = HNDL 不影响代币经济本身（只影响隐私）
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

#### 3.4.1 状态树：直接采用 Verkle Tree（不走 MPT 弯路）

| 项 | MPT（AII 当前计划）| Verkle Tree（建议）|
|---|---|---|
| 状态见证大小 | ~10-50 KB / 账户 | **~200 B / 账户**（缩小 50-250×）|
| 无状态客户端可行性 | 不可行 | **可行**（轻节点跑在浏览器内）|
| 实现复杂度 | 成熟（geth/erigon）| 中等（go-verkle / verkle-trie crate） |
| 量子安全 | ✅（Keccak-256）| ✅（IPA 或 KZG，但 IPA 配 STARK 后 PQ-safe） |

**建议**：AII 主网启动即用 Verkle Tree（基于 Banderwagon / Pedersen IPA）。**避开以太坊的 MPT → Verkle 痛苦迁移**。

#### 3.4.2 MEV 抵抗：协议级加密 mempool

- 主网启动即引入 **threshold-encrypted mempool**（McFly / Shutter Network 思路）
- 用户交易先以阈值加密公钥加密提交，达成共识后由 V 节点集合阈值解密
- 防止 proposer 看到明文交易后抢跑

#### 3.4.3 账户抽象：Genesis 即支持 EIP-4337 + EIP-7702

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

#### Phase B：Q-Day 警报触发（预计 2030-2035）

当下列任一条件成立时，启动"PQ Forklift 准备期"：

- NIST / 国家安全机构正式发布 Q-Day 警告
- Google / IBM / 公开论文宣称破解了 ≥ 256-bit ECC
- Ethereum / Bitcoin 等主流公链启动 PQC 强制迁移

行动：
- **V 节点必须使用 hybrid 签名**（secp256k1 + ML-DSA 双签）
- **新账户默认 ML-DSA**（钱包客户端默认改为 PQC）
- **设立 PQ 迁移激励**：用户主动迁移到 PQC 账户的，可享受 1 年内免 base_fee 销毁

#### Phase C：PQ Forklift（Q-Day 后）

通过 hard fork 执行：

- **冻结所有未迁移的 secp256k1 账户**（按公开警告时间表）
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

### 4.6 HNDL 数据隐私防护

由于链上所有交易公开，HNDL 风险无法完全消除，但可缓解：

- **私密交易选项**：用户可选用 zk-STARK 加密交易（仅金额、不暴露发送方）
- **隐私子链**：基于 STARK 的隐私子链（类似 Aleo / Aztec 模式）
- **CoinJoin / 混币子链**：内置混币子链，主网启动即可用

不强制——大部分交易仍走公开模式（兼容 EVM 生态）。

---

## 5. 协议常量调整建议

下表为本文档对 AII 现有协议常量的**建议调整值**（须 hard fork）：

| 常量 | 当前值（v0.4）| 建议值（Phase 2）| 建议值（Phase 5）|
|---|---|---|---|
| 主链 slot 时间 | 6 秒 | 3 秒 | < 1 秒（DAG）|
| epoch 长度 | 32 slots | 32 slots | DAG 模式下重新定义 |
| `S_min(V)` | 50 AII | 50 AII | 50 AII |
| 签名算法默认 | secp256k1 | secp256k1 | ML-DSA |
| VRF 算法 | schnorrkel | schnorrkel | HashVRF |
| 状态树 | MPT | **Verkle Tree** | Verkle Tree |
| 区块奖励初始 | 2 AII | 2 AII | 2 AII（不变）|
| MEV 防护 | 无 | 阈值加密 mempool | 同 + Time Boost |
| 账户抽象 | 路线图 Phase 4 | **创世即支持 EIP-4337/7702** | 同 |

> **强烈建议**：将 Verkle Tree 与 EIP-4337/7702 **提前到主网 Genesis** 引入——这两项现在不引入，后续硬分叉迁移痛苦巨大。

---

## 6. 实施路线图建议

```
Phase 0  Genesis（T0）
├─ 仍用 BFT-PoS Tendermint 风格（6s slot，~1.5K TPS）
├─ ★ Verkle Tree 状态树（代替 MPT）
├─ ★ 多签名 Registry（默认 secp256k1，可选 ML-DSA / SLH-DSA）
├─ ★ EIP-4337 + EIP-7702 账户抽象（创世即支持）
└─ ★ PQ Forklift 钩子（账户结构、迁移操作码）

Phase 2  远航·Plus（T0 + 9-12 月）
├─ Slot 时间 6s → 3s（共识参数 hard fork）
├─ 流水线 BFT（Shoal 思路）
├─ 阈值加密 mempool（MEV 抵抗）
└─ V 节点 hybrid 签名建议（BLS + ML-DSA）

Phase 5  大航海（T0 + 18-24 月）
├─ DAG-BFT 切换（Mysticeti-inspired）
├─ 数据/执行解耦（Monad 思路）
├─ 子秒最终性 + 100K+ TPS
├─ 隐私子链上线（zk-STARK）
└─ STARK 聚合签名研究

Phase Q  Q-Day 警报响应（外部触发，2030-2035）
├─ V 节点强制 hybrid → 纯 PQ 签名迁移
├─ 新账户默认 ML-DSA
├─ 全网 PQ Forklift hard fork
└─ HashVRF 切换
```

**★** 标记为**建议提前到 Genesis** 的项目——后续引入成本高。

---

## 7. 风险与权衡

### 7.1 共识 DAG 化的风险

| 风险 | 缓解 |
|---|---|
| DAG-BFT 工程复杂度高，bug 风险大 | Phase 5 时机较晚（T0+18 月），已积累 1.5 年 BFT 运维经验 |
| 实现参考较少（Sui / Aptos 是主要参考） | 与 Mysticeti / MonadBFT 团队公开技术交流；可贡献 reth-style 开源实现 |
| 节点硬件需求上升 | 7-Tier 节点模型本身已支持高配节点；DAG 主要影响 T1-T2 |

### 7.2 多签名 Registry 的风险

| 风险 | 缓解 |
|---|---|
| 验证逻辑复杂化，攻击面增大 | 严格审计每个新增 algoId 的实现；上线前形式化验证 |
| 用户混淆不同签名类型 | 钱包 UI 强制提示；默认仍是 secp256k1 |
| 跨链桥可能仅支持某些 algoId | 桥协议层做 algoId 转换；HTLC 等机制不依赖具体签名算法 |

### 7.3 Verkle Tree 的风险

| 风险 | 缓解 |
|---|---|
| Verkle Tree 实现库尚不如 MPT 成熟 | 与以太坊 Verkle 团队同步实现；2026 末以太坊 Verkle 上线后参考最佳实践 |
| 工具链（Etherscan-like 浏览器）需适配 | Blockscout 已计划支持 |

### 7.4 提前引入 EIP-4337/7702 的风险

| 风险 | 缓解 |
|---|---|
| 攻击面比 EOA 大（合约钱包逻辑漏洞） | 推荐使用 Safe / Argent / Biconomy 等已审计合约钱包模板 |
| 用户教育成本 | CLI / 钱包默认值仍是 EOA + secp256k1，AA 是 opt-in |

---

## 8. 决策矩阵：哪些建议立即采纳

按"影响 / 紧迫 / 实施成本"评估每个建议：

| 建议 | 影响 | 紧迫 | 成本 | 建议 |
|---|---|---|---|---|
| Verkle Tree 替代 MPT | 🔴 高 | 🟡 中 | 🟡 中 | ✅ **强烈建议 Genesis 引入** |
| EIP-4337/7702 账户抽象 | 🔴 高 | 🟡 中 | 🟢 低 | ✅ **强烈建议 Genesis 引入** |
| 多签名 Registry（PQC 准备）| 🔴 高 | 🔴 高 | 🟡 中 | ✅ **强烈建议 Genesis 引入** |
| PQ Forklift 钩子 | 🔴 高 | 🟡 中 | 🟢 低 | ✅ **强烈建议 Genesis 引入** |
| Slot 6s → 3s | 🟡 中 | 🟢 低 | 🟢 低 | ✅ Phase 2 |
| 阈值加密 mempool | 🟡 中 | 🟡 中 | 🟡 中 | 🟡 Phase 2 评估 |
| DAG-BFT（Mysticeti）| 🔴 高 | 🟢 低 | 🔴 高 | 🟡 Phase 5（充分测试后）|
| STARK 聚合 V 节点签名 | 🔴 高 | 🟢 低 | 🔴 高 | 🟡 Phase 5+ 或 Q-Day 触发 |
| HashVRF 替代 schnorrkel | 🟡 中 | 🟢 低 | 🟡 中 | 🟡 Phase 5+ 或 Q-Day 触发 |
| 隐私子链（STARK）| 🟡 中 | 🟢 低 | 🟡 中 | 🟡 Phase 5 |

**Day-0 必做 4 项**（如不引入，后期成本将 10-100×）：

1. ✅ **Verkle Tree**
2. ✅ **EIP-4337 + EIP-7702 账户抽象**
3. ✅ **多签名 Registry**（即便 PQ 算法 Day 0 仅注册不强制使用）
4. ✅ **PQ Forklift 协议钩子**

---

## 9. 对现有文档的修订建议

如本文档的建议被采纳，下列现有文档需同步更新：

| 文档 | 修订点 |
|---|---|
| 03 黄皮书 | §1.2 账户状态新增 `pq_pubkey` 字段；§4 共识考虑 DAG-BFT；§7 密码学增加多签名 Registry；§13 参数表 |
| 05 共识机制详细设计 | §2 slot 时间路线图；新增 DAG-BFT 章节；新增多签名章节 |
| 06 代币经济与分配模型 | §6 V 节点抵押允许 hybrid 签名；§12 协议常量更新 |
| 08 技术选型决策书 | §3 EVM 部分加入 Verkle 支持；§5 密码学库新增 liboqs-rs / pqcrypto；§6 共识加入 DAG-BFT 评估 |
| 09 安全与威胁模型 | §3 新增 Q-Day 长程威胁；§5 跨链桥 PQC 升级路径；§11 形式化验证加入 PQ 算法 |
| 10 路线图 | Phase 2 / Phase 5 / Phase Q 新增任务 |
| 12 章程 | 第五章协议演进承诺：明确 PQ Forklift 钩子为不可变核心的一部分 |

---

## 10. 决策待回填

| # | 问题 | 候选 | 决定 |
|---|---|---|---|
| 1 | Verkle Tree 实现是否 Day-0 引入？ | 是 / 否 | 待社区论坛共识 |
| 2 | 默认 PQC 签名选 ML-DSA 还是 SLH-DSA？ | ML-DSA-65（性能优）/ SLH-DSA-128s（最保守）| 待审计 |
| 3 | V 节点 hybrid 签名 Day-0 强制还是 opt-in？ | 强制 / opt-in | 待评估生态阻力 |
| 4 | DAG-BFT 切换前是否设独立 testnet？ | 是（强烈建议）/ 否 | – |
| 5 | 隐私子链优先级 | 高 / 中 / 低 | 待社区需求评估 |
| 6 | Q-Day 警报触发标准 | NIST 公告 / 公开破解事件 / 主流公链先迁移 | 待章程修订 |

---

## 11. 参考资料（截至 2026-05）

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
- Google Quantum AI: 256-bit ECC 破解所需逻辑量子比特估算（2026-03）
- pq.ethereum.org — Post-Quantum Ethereum
- Federal Reserve: Harvest Now, Decrypt Later 对分布式账本网络的影响（2025）
- QRL 文档（XMSS / SPHINCS+ 迁移）
- Ethereum EIP-8141 提案（leanXMSS 验证者签名）
- liboqs / pqcrypto Rust crate

### Verkle Trees 与状态层

- Ethereum Verkle Trees 文档与 EIP-6800
- Banderwagon: A curve made of banderwagons for Verkle trees

### 账户抽象

- EIP-4337: Account Abstraction via Entry Point Contract
- EIP-7702: Set EOA account code

---

## 12. 总结

AII 当前设计（v0.4）在**清晰性、极简哲学、公平启动**方面达到了优秀水准，但相对 2024-2026 年最新前沿存在三个**架构代差**：

1. **共识架构代差**：仍是 Tendermint-style 串行 BFT，相比 DAG-based Mysticeti / MonadBFT 慢 1-2 个数量级
2. **抗量子代差**：全套数字签名都量子可破，Q-Day 临近时需要紧急迁移
3. **状态层代差**：MPT 是过时的选择，Verkle Tree 即将成为标准

**最低成本的优化方向**（Day-0 引入）：

- ✅ Verkle Tree
- ✅ 多签名 Registry（PQC 准备）
- ✅ EIP-4337/7702 账户抽象
- ✅ PQ Forklift 协议钩子

这 4 项在 Day-0 引入的工程量约 6-8 人月，但能将后期 PQ 迁移与状态升级的成本降低 10-100×。

**中期优化**（Phase 2，T0+9-12 月）：

- 🟡 Slot 时间 6s → 3s
- 🟡 流水线 BFT
- 🟡 阈值加密 mempool

**长期优化**（Phase 5，T0+18-24 月）：

- 🟡 DAG-BFT 切换（Mysticeti 路径）
- 🟡 V 节点 hybrid → STARK 聚合签名

**应急路径**（Q-Day 警报触发）：

- 🔴 全网 PQ Forklift hard fork

—— 本演进优化建议完 ——

> **下一步**：本文档为 v0.1 咨询稿。建议在 GitHub Discussions 开启专题讨论；3 个月内若社区共识倾向采纳，可作为 v0.5 设计基线纳入正式文档（同步修订 03/05/06/08/09/10/12）。
