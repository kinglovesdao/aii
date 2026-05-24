# aii-state — Crate Design Spec

> **状态**: 草案 v0.1 · 2026-05-24
> **范围**: `crates/aii-state` —— M1 第 2 块 crate（24-crate 列表 #5）。
> **权威源**: workspace 设计规范 `2026-05-21-aii-core-design.md` §3.2 #5；以太黄皮书 §4（State）+ Appendix D（Modified Merkle Patricia Tree）。
> **目标读者**: aii-evm / aii-rpc / aii-consensus-bft 实现者。

---

## §1 目标与边界

### 1.1 目标 (v0.0.6 — 本 PR)

- ✅ `Account`：4 字段固定布局（nonce / balance / code_hash / storage_root）+ RLP 编解码 + Keccak hash
- ✅ `StateDb`：基于 `aii_storage::KvBackend` 的 Account 读/写抽象（写入 ColumnFamily::State）
- ✅ `EMPTY_CODE_HASH` 常量 + re-export `EMPTY_TRIE_HASH` 自 `aii-block`
- ✅ `mpt_root` 占位入口（空输入返回 `EMPTY_TRIE_HASH`，非空输入 `unimplemented!`）—— 真正实现见 v0.0.7

### 1.2 留给 v0.0.7+

- ❌ 完整 MPT（hex-prefix + branch/extension/leaf 节点 + RLP-pruning）
- ❌ `transactions_root` / `receipts_root` / `withdrawals_root` helper
- ❌ Storage trie（per-account）
- ❌ 完整 ethereum-tests/MerkleTrie/* 兼容
- ❌ 历史状态查询（trie root + Snapshot 组合）
- ❌ EVM 集成（aii-evm 单独 crate）

> v0.0.6 让 aii-evm 能拿到 Account（最关键依赖）；MPT 在 v0.0.7 单独 PR 落地，避免本 PR 体积过大。

### 1.3 依赖

```
aii-evm ─┐
aii-rpc ─┼──► aii-state ──► aii-storage (KvBackend / ColumnFamily::State)
         │             ──► aii-codec (RLP for Account)
         │             ──► aii-crypto (keccak256)
         │             ──► aii-types  (H256, Address, U256)
```

---

## §2 模块拓扑

```
crates/aii-state/
├── src/
│   ├── lib.rs           // re-exports + module map
│   ├── error.rs         // StateError
│   ├── account.rs       // Account struct + RLP codec + Hashable
│   ├── trie.rs          // mpt_root + 内部 hex-prefix helper
│   └── db.rs            // StateDb<B: KvBackend>
├── tests/
│   ├── account_rlp.rs   // round-trip + known-Account hash
│   ├── trie_kat.rs      // mpt_root 对齐 EMPTY_TRIE_HASH + 1 known sample
│   └── statedb.rs       // get/put round-trip + nonexistent → None
├── Cargo.toml
└── README.md
```

---

## §3 核心类型

### 3.1 `Account`

```rust
pub struct Account {
    pub nonce: u64,
    pub balance: U256,
    pub code_hash: H256,     // keccak256 of contract bytecode; EMPTY_CODE_HASH for EOA
    pub storage_root: H256,  // root of per-account storage trie; EMPTY_TRIE_HASH for EOA
}
```

RLP 顺序：`[nonce, balance, storage_root, code_hash]`（与以太一致 —— 注意 `storage_root` 在 `code_hash` 前）。

常量：
- `EMPTY_CODE_HASH = keccak256(b"")` = `0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470`
- `EMPTY_TRIE_HASH` 已在 aii-block 提供 —— state 也 re-export

### 3.2 `mpt_root` (v0.0.6 占位)

```rust
pub fn mpt_root<I, K, V>(items: I) -> H256
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<[u8]>,
    V: AsRef<[u8]>;
```

v0.0.6 行为：
- 空输入 → `EMPTY_TRIE_HASH`
- 非空输入 → `unimplemented!()` panic (调用方未到时不应触发)

v0.0.7 将实现完整算法。

### 3.3 `StateDb<B>`

```rust
pub struct StateDb<B: KvBackend> { backend: Arc<B> }

impl<B: KvBackend> StateDb<B> {
    pub fn new(backend: Arc<B>) -> Self;
    pub fn account(&self, addr: &Address) -> Result<Option<Account>, StateError>;
    pub fn set_account(&self, addr: &Address, account: &Account) -> Result<(), StateError>;
    pub fn remove_account(&self, addr: &Address) -> Result<(), StateError>;
}
```

Key 格式：`keccak256(address.as_bytes())` 的 32 字节作为 ColumnFamily::State 中的 key（与以太 state trie key derivation 一致）。

---

## §4 测试矩阵

- **Account**：≥ 5 单元测试（RLP round-trip / EOA defaults / hash 确定性 / 字段独立性 / nonce 边界）
- **mpt_root（v0.0.6）**：
  - `empty_input_equals_empty_trie_hash`
- **StateDb**：
  - `get_nonexistent_returns_none`
  - `set_then_get_round_trip`
  - `remove_clears`

---

## §5 v0.0.6 完成标准

- `cargo test -p aii-state` 全绿 (≥ 10 tests)
- 工作区 `cargo clippy -- -D warnings` 仍清
- 0.0.5 → 0.0.6 版本号
- aii-block 的 fixture 测试通过（不依赖）
