# aii-block — Crate Design Spec

> **状态**: 草案 v0.1 · 2026-05-24
> **范围**: `crates/aii-block` —— M1 第 1 块 crate（24-crate 列表 #6）。
> **权威源**: workspace 设计规范 `2026-05-21-aii-core-design.md` §3.2 #6；EIP-2718 / EIP-1559 / EIP-2930 / EIP-4844 / EIP-4895。
> **目标读者**: aii-evm / aii-net-sync / aii-state / aii-rpc crate 的实现者。

如果 workspace 设计规范 / 顶层文档与本 spec 冲突，**workspace 规范优先**。请通过 PR 修订本 spec，而非偏离。

---

## §1 目标与边界

### 1.1 目标

提供 AII 协议中"区块+交易+收据"层的 **数据结构 + 编码 + 哈希**。

- ✅ `Header`：EIP-1559 base_fee + EIP-4895 withdrawals_root 的 16 字段固定布局
- ✅ `Tx`：EIP-2718 envelope 包装三种类型 —— `Legacy` / `Eip1559` / `Eip4844`（4844 为 enum 占位，不实现 KZG）
- ✅ `Receipt`：EIP-2718 envelope，单一 Receipt 类型 + `tx_type` 字段
- ✅ `Block` = `Header` + `BlockBody { transactions, ommers, withdrawals }`
- ✅ `Hashable` trait：所有结构都可计算 Keccak-256 哈希
- ✅ RLP 编解码与以太主网 byte-perfect 对齐（≥ 10 个 mainnet block fixture）
- ✅ AII 扩展字段：`AlgoId` 内嵌于 `Tx`，默认 `Secp256k1` 时在 wire 上省略（向后兼容以太）

### 1.2 非目标

- ❌ 签名验证（仅承载未验证数据；签名验证留给 aii-state/evm 调用 aii-crypto）
- ❌ Gas 估算 / Gas 计量（aii-evm）
- ❌ State / Receipt trie 根的真实构造（aii-state 提供 MPT；本 crate 只提供占位字段）
- ❌ EIP-4844 KZG commitment 验证（占位 enum 变体，留给 aii-crypto 后续 PQ slot）
- ❌ 网络层（aii-net-p2p）
- ❌ SSZ Light-Client 序列化（Day-1，由 aii-codec 已有的 SSZ 路径承担时再加）

### 1.3 依赖位置

```
aii-evm ─┐
aii-net-sync ─┼──► aii-block ──► aii-codec ──► aii-types
aii-state ───┘                ──► aii-crypto (keccak256 only)
```

aii-block **不**依赖 aii-storage（pure data + codec，没有持久化）。

---

## §2 模块拓扑

```
crates/aii-block/
├── src/
│   ├── lib.rs           // re-exports + module map + Hashable trait
│   ├── error.rs         // BlockError (thiserror umbrella)
│   ├── header.rs        // Header + HeaderBuilder
│   ├── tx/
│   │   ├── mod.rs       // Tx enum + EIP-2718 envelope encode/decode
│   │   ├── legacy.rs    // TxLegacy (pre-EIP-2718)
│   │   ├── eip1559.rs   // TxEip1559 (type 0x02)
│   │   ├── eip4844.rs   // TxEip4844 (type 0x03, placeholder; encodes but no KZG)
│   │   └── access.rs    // AccessList shared by 1559/4844
│   ├── receipt.rs       // Receipt + EIP-2718 envelope + LogsBloom
│   ├── log.rs           // Log (address + topics + data)
│   ├── withdrawal.rs    // Withdrawal (EIP-4895)
│   ├── body.rs          // BlockBody { transactions, ommers, withdrawals }
│   └── block.rs         // Block { header, body }
├── tests/
│   ├── header_rlp.rs    // mainnet header fixtures (≥10) byte-perfect round-trip
│   ├── tx_rlp.rs        // tx fixtures per type
│   ├── receipt_rlp.rs   // receipt fixtures
│   ├── block_hash.rs    // known mainnet block hashes
│   └── proptest.rs      // ≥5 properties (round-trip, hash determinism, ...)
├── fixtures/            // raw hex from ethers/reth test corpus (committed)
│   ├── header_*.hex
│   ├── tx_*.hex
│   ├── receipt_*.hex
│   └── block_*.hex
├── Cargo.toml
└── README.md
```

---

## §3 核心类型

### 3.1 `Header` (EIP-1559 + EIP-4895)

16 字段固定布局，字段顺序锁定（用于 RLP）：

| # | 字段 | 类型 | 备注 |
|---|---|---|---|
| 1 | `parent_hash` | `H256` | |
| 2 | `ommers_hash` | `H256` | 通常 `EMPTY_LIST_HASH`（AII 不挖 uncle，但保留以兼容） |
| 3 | `beneficiary` | `Address` | coinbase / miner |
| 4 | `state_root` | `H256` | aii-state 写入 |
| 5 | `transactions_root` | `H256` | MPT(tx index → rlp(tx))；aii-state 计算 |
| 6 | `receipts_root` | `H256` | 同上，收据 |
| 7 | `logs_bloom` | `Bloom` (256 bytes) | 见 §3.4 |
| 8 | `difficulty` | `U256` | PoS 后恒为 0；保留位 |
| 9 | `number` | `u64` | block height |
| 10 | `gas_limit` | `u64` | |
| 11 | `gas_used` | `u64` | |
| 12 | `timestamp` | `u64` | Unix seconds |
| 13 | `extra_data` | `Vec<u8>` | ≤ 32 字节 |
| 14 | `mix_hash` | `H256` | PoS 后存 prevrandao |
| 15 | `nonce` | `[u8; 8]` | PoS 后恒为 0 |
| 16 | `base_fee_per_gas` | `U256` | EIP-1559；AII 创世起强制存在 |
| 17 | `withdrawals_root` | `H256` | EIP-4895；AII 创世起强制存在 |
| 18 | `blob_gas_used` | `Option<u64>` | EIP-4844；占位，AII 创世可为 `None` |
| 19 | `excess_blob_gas` | `Option<u64>` | EIP-4844；同上 |
| 20 | `parent_beacon_block_root` | `Option<H256>` | EIP-4788；占位 |

> AII 主网创世锁定 `base_fee_per_gas` + `withdrawals_root` 强制存在（不像以太是分硬分叉激活）。4844/4788 字段在 v0.0.5 的编码里输出，但解码时允许向后兼容（如果末尾 RLP 列表更短则填 `None`）。

`Hashable::hash(&Header)` = `keccak256(rlp(header))`。

### 3.2 `Tx` enum（EIP-2718 envelope）

```rust
pub enum Tx {
    Legacy(TxLegacy),
    Eip1559(TxEip1559),
    Eip4844(TxEip4844),
}
```

EIP-2718 envelope 规则：
- Legacy：直接 RLP 编码（首字节 ≥ 0xc0，列表）
- 类型化（1559/4844）：`type_byte ‖ rlp(body)`（首字节 ∈ [0x00, 0x7f]）

每种 body 内嵌字段（以 EIP-1559 为例）：

| # | 字段 | 类型 |
|---|---|---|
| 1 | `chain_id` | `u64` |
| 2 | `nonce` | `u64` |
| 3 | `max_priority_fee_per_gas` | `U256` |
| 4 | `max_fee_per_gas` | `U256` |
| 5 | `gas_limit` | `u64` |
| 6 | `to` | `Option<Address>` (None = CREATE) |
| 7 | `value` | `U256` |
| 8 | `data` | `Vec<u8>` |
| 9 | `access_list` | `Vec<AccessListItem>` |
| 10 | `v` | `u8` (yParity, 0/1) |
| 11 | `r` | `H256` |
| 12 | `s` | `H256` |

**AII 扩展（D7 PQ slot）**：所有 Tx 变体编码末尾追加可选 `algo_id` 字段：
- 如果 `algo_id == Secp256k1`（默认值），**不输出**，保持与以太字节兼容
- 否则在 RLP 列表末尾追加 1 字节 `algo_id`

解码时 try-parse：先按以太兼容布局解析；如果列表多 1 项且最后一项是单字节，解释为 `algo_id`，否则当作以太版本。

> 这一兼容策略让 AII 网络可直接消费 MetaMask 签出的 EIP-1559 交易；只有 PQ 签名时（V 节点签 BLS / 未来 Dilithium）才使用扩展字段。

### 3.3 `Receipt`（单一类型 + tx_type）

```rust
pub struct Receipt {
    pub tx_type: TxType,             // matches Tx enum discriminant
    pub status: bool,                // success / fail
    pub cumulative_gas_used: u64,
    pub logs_bloom: Bloom,
    pub logs: Vec<Log>,
}
```

envelope：
- Legacy receipt：直接 RLP `[status, cum_gas, bloom, logs]`
- 类型化：`type_byte ‖ rlp([status, cum_gas, bloom, logs])`

### 3.4 `Bloom`（256 字节 = 2048 bits）

newtype 包装 `[u8; 256]`；提供 `accrue(&[u8])` 把 keccak 输入位累加进 bloom；提供 `contains(&[u8])` 做包含查询（false positive 允许）。

`Bloom::ZERO` 常量。

### 3.5 `Log`

```rust
pub struct Log {
    pub address: Address,
    pub topics: Vec<H256>,       // ≤ 4
    pub data: Vec<u8>,
}
```

### 3.6 `Withdrawal` (EIP-4895)

```rust
pub struct Withdrawal {
    pub index: u64,
    pub validator_index: u64,
    pub address: Address,
    pub amount: u64,   // Gwei (per EIP-4895)
}
```

### 3.7 `BlockBody` & `Block`

```rust
pub struct BlockBody {
    pub transactions: Vec<Tx>,
    pub ommers: Vec<Header>,           // PoS 时永远是空 vec
    pub withdrawals: Vec<Withdrawal>,
}

pub struct Block {
    pub header: Header,
    pub body: BlockBody,
}
```

`Block::hash() == Header::hash()`（区块的身份取自 header；body 通过 header 内三个 root 字段间接背书）。

---

## §4 错误类型

```rust
#[derive(Debug, thiserror::Error)]
pub enum BlockError {
    #[error("rlp decode: {0}")]
    Rlp(#[from] alloy_rlp::Error),

    #[error("unknown tx type byte: 0x{0:02x}")]
    UnknownTxType(u8),

    #[error("invalid receipt envelope")]
    InvalidReceiptEnvelope,

    #[error("invalid bloom length (expected 256, got {0})")]
    InvalidBloomLength(usize),

    #[error("extra_data too long ({0} > 32)")]
    ExtraDataTooLong(usize),
}
```

---

## §5 公共 API（lib.rs 头部）

```rust
//! # aii-block
//!
//! ## Public API
//! - `Header`, `Tx (enum)`, `TxLegacy/Eip1559/Eip4844`, `Receipt`, `Block`, `BlockBody`
//! - `Log`, `Bloom`, `Withdrawal`, `AccessListItem`
//! - `Hashable` trait (single method `hash() -> H256`)
//! - Re-exports `aii_types::{H256, Address, U256, AlgoId}`
//! - `BlockError` umbrella
//!
//! ## Internal
//! - RLP/SSZ encode helpers are `pub(crate)` and delegated to `aii_codec`
//! - Constants (`EMPTY_LIST_HASH`, `EMPTY_TRIE_HASH`) are `pub`
```

---

## §6 测试矩阵

| 层级 | 测试数 | 工具 |
|---|---|---|
| 单元测试 | ≥ 30 | `cargo test -p aii-block` |
| 属性测试 | ≥ 5 | `proptest` |
| Fixture 测试 | ≥ 10 mainnet blocks | 静态 hex 文件 |
| 编码 round-trip | 100% | encode → decode → 字节比对 |
| 哈希 KAT | ≥ 5 known hashes | Etherscan 验证过的真实 hash |

属性清单：
1. **`header_rlp_round_trip`**：任意 Header → encode → decode → `==`
2. **`tx_rlp_round_trip_per_variant`**：每种 Tx 变体单独 round-trip
3. **`receipt_rlp_round_trip`**：Receipt round-trip
4. **`block_rlp_round_trip`**：Block round-trip
5. **`hash_is_deterministic`**：相同输入两次 hash 字节相等
6. **`algo_id_default_bytewise_eth_compat`**：`AlgoId::Secp256k1` 编码 = 以太编码（关键 wire 兼容性）

Mainnet fixtures（最少集合）：
- Block #0（创世，pre-merge style，单独标注）
- Block #15537393（the merge）
- Block #17034870（Shanghai / EIP-4895 启用）
- Block #19426587（Cancun / EIP-4844 + 4788 启用）
- 任意 ≥ 6 个其他历史区块

---

## §7 性能预算

- 编码 1KB 区块头 < 1µs
- 解码 1KB 区块头 < 5µs
- Block hash（含 keccak）< 10µs

不强制 CI gate；只在 README 中给参考数字。aii-evm 的执行 gate 才是关键路径。

---

## §8 文档与发布

- 每个模块 `//!` 顶部 doc 描述用途与 wire 格式
- `lib.rs` 列出 Public/Internal（同 §5）
- `README.md` 含一段 quick start
- v0.0.5 不发布 crates.io（spec §5.3：M2 退出前 internal）

---

## §9 变更日志

| 版本 | 日期 | 变更 |
|---|---|---|
| v0.1 | 2026-05-24 | 初稿；EIP-2718 envelope + AlgoId 扩展；Header 20 字段（含 4844/4788 占位）；测试矩阵 |
