# aii-storage — Crate Design Spec

> **状态**: 草案 v0.1 · 2026-05-24
> **范围**: `crates/aii-storage` —— M0 第 4 块基石 crate。
> **权威源**: workspace 设计规范 `2026-05-21-aii-core-design.md` §3.1 #4 + §1.2（08 §4.2 → rocksdb）。
> **目标读者**: aii-state / aii-block / aii-net-sync / aii-microchain crate 的实现者。

如果工作坊设计 / 文档与本 spec 冲突，**workspace 规范优先**。请通过 PR 修订本 spec，而非偏离。

---

## §1 目标与边界

### 1.1 目标

为 AII 协议提供 **一个抽象的、列簇化的、支持快照与原子批写的 KV 存储**。

- ✅ 抽象 `KvBackend` trait，使 aii-state / aii-block 不直接依赖 `rocksdb`
- ✅ 默认 `RocksDbBackend` 实现，达到 M0 退出标准（顺序写 ≥ 50k op/s）
- ✅ `MemoryBackend` 提供给下游做单元测试（无需 fs / 无需 librocksdb 编译）
- ✅ 闭集 `ColumnFamily` enum，命名与 reth/erigon 习惯一致
- ✅ `Snapshot` 提供只读一致性视图（aii-state 历史状态 access 的基础）
- ✅ `WriteBatch` 提供跨 CF 原子提交

### 1.2 非目标

- 不实现历史 state pruning（留给 aii-state 的"在 trie 层维护"策略）
- 不实现远程 KV（IPFS / S3 等）— 等 Day-1
- 不暴露 RocksDB 的高级选项（merge operator、column family options、TTL）—— 第一版只暴露最小集，后续 spec 修订加
- 不提供异步 API —— 调用方在 async 上下文用 `tokio::task::spawn_blocking`

### 1.3 依赖位置

```
aii-state ──► aii-storage ──► rocksdb (FFI to librocksdb)
                          ──► aii-types
```

不依赖 aii-codec（类型化序列化由 consumer crate 自理）。

---

## §2 模块拓扑

```
crates/aii-storage/
├── src/
│   ├── lib.rs           // re-exports + module map
│   ├── error.rs         // StorageError (thiserror umbrella)
│   ├── cf.rs            // ColumnFamily enum (closed set, §3)
│   ├── backend.rs       // KvBackend trait
│   ├── batch.rs         // WriteBatch (backend-agnostic Op log)
│   ├── snapshot.rs      // Snapshot trait
│   ├── rocksdb.rs       // RocksDbBackend impl + RocksDbSnapshot
│   └── memory.rs        // MemoryBackend impl + MemorySnapshot
├── tests/
│   ├── conformance.rs   // shared trait-level tests parametrized over backend
│   └── proptest.rs      // Op-sequence equivalence; snapshot isolation
├── benches/
│   └── write_throughput.rs   // criterion: ≥50k op/s gate
├── Cargo.toml
└── README.md
```

---

## §3 ColumnFamily（闭集 enum）

| Variant | RocksDB name | 用途 (consumer) |
|---|---|---|
| `Default` | `default` | RocksDB 默认 CF（必有，几乎不写） |
| `Headers` | `headers` | block hash → header bytes（aii-block） |
| `Bodies` | `bodies` | block hash → tx list bytes |
| `Receipts` | `receipts` | block hash → receipts bytes |
| `Transactions` | `transactions` | tx hash → tx bytes（mempool 持久化） |
| `State` | `state` | MPT node hash → node bytes（aii-state） |
| `AccountStorage` | `account_storage` | per-account storage trie 节点 |
| `TxLookup` | `tx_lookup` | tx hash → (block hash, index) |
| `Meta` | `meta` | head/finalized/snapshot markers + schema version |
| `MicroChain` | `microchain` | 子链注册表 + flush 锚点 |

新增 CF 需修订本 spec —— `ColumnFamily::ALL: &[Self]` 用于一次性创建所有 CF。
`ColumnFamily::as_str()` 返回 RocksDB 名字（snake_case），稳定 wire 字符串。

---

## §4 核心接口

### 4.1 `KvBackend` trait

```rust
pub trait KvBackend: Send + Sync + 'static {
    type Snapshot: Snapshot;

    fn get(&self, cf: ColumnFamily, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError>;
    fn put(&self, cf: ColumnFamily, key: &[u8], value: &[u8]) -> Result<(), StorageError>;
    fn delete(&self, cf: ColumnFamily, key: &[u8]) -> Result<(), StorageError>;

    /// Atomic across CFs — all-or-nothing.
    fn write(&self, batch: WriteBatch) -> Result<(), StorageError>;

    fn snapshot(&self) -> Self::Snapshot;

    fn iter<'a>(
        &'a self,
        cf: ColumnFamily,
    ) -> Box<dyn Iterator<Item = Result<(Vec<u8>, Vec<u8>), StorageError>> + 'a>;

    fn iter_prefix<'a>(
        &'a self,
        cf: ColumnFamily,
        prefix: &'a [u8],
    ) -> Box<dyn Iterator<Item = Result<(Vec<u8>, Vec<u8>), StorageError>> + 'a>;
}
```

设计取舍：
- **Owned `Vec<u8>` results** —— 避免泄露 RocksDB `PinnedSlice` 生命周期到 trait
- **Boxed iterator** —— GAT (generic associated types) 还在演进；boxed dyn 简单可工程
- **`'static` 约束** —— 让 `Arc<dyn KvBackend>` 在 aii-state / aii-rpc 中可跨线程持有

### 4.2 `Snapshot` trait

```rust
pub trait Snapshot: Send + Sync {
    fn get(&self, cf: ColumnFamily, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError>;
    fn iter<'a>(
        &'a self,
        cf: ColumnFamily,
    ) -> Box<dyn Iterator<Item = Result<(Vec<u8>, Vec<u8>), StorageError>> + 'a>;
}
```

Snapshot 是只读一致视图。生命周期之内看见的库状态稳定（即使 backend 发生 write）。
RocksDB 用原生 `Snapshot`（增量保留 SST），Memory 用 `Arc<HashMap<...>>` 的 clone-on-snapshot。

### 4.3 `WriteBatch`（后端无关）

```rust
#[derive(Default, Clone)]
pub struct WriteBatch {
    ops: Vec<Op>,
}

#[derive(Clone)]
enum Op {
    Put { cf: ColumnFamily, key: Vec<u8>, value: Vec<u8> },
    Delete { cf: ColumnFamily, key: Vec<u8> },
}

impl WriteBatch {
    pub fn new() -> Self { Self::default() }
    pub fn put(&mut self, cf: ColumnFamily, key: &[u8], value: &[u8]) -> &mut Self;
    pub fn delete(&mut self, cf: ColumnFamily, key: &[u8]) -> &mut Self;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn iter(&self) -> impl Iterator<Item = &Op>;
}
```

每个 backend 实现 `KvBackend::write` 时把 `Op` replay 成原生格式（RocksDB `WriteBatch::put_cf` / BTreeMap 直接 mutate）。

### 4.4 `StorageError`

```rust
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("backend error: {0}")]
    Backend(String),

    #[error("column family not registered: {0:?}")]
    InvalidColumnFamily(ColumnFamily),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
```

不实现 `From<rocksdb::Error>` —— 防止 rocksdb crate 类型穿透到 trait 边界（保持后端可替换性）。

---

## §5 后端实现

### 5.1 RocksDbBackend

```rust
pub struct RocksDbBackend {
    db: Arc<rocksdb::DB>,
}

impl RocksDbBackend {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError>;
    pub fn open_in_temp() -> Result<Self, StorageError>;  // tempdir for tests
}
```

默认 options：
- `create_if_missing = true`
- `create_missing_column_families = true`
- 全部 CF 用 `ColumnFamily::ALL`
- `use_fsync = false`
- `bytes_per_sync = 1 MB`
- compression = `lz4`（zstd 后续暴露给 aii-config）

`RocksDbSnapshot` 持有 `db.snapshot()` + `Arc<rocksdb::DB>` 引用以保证 CF handle 有效。

### 5.2 MemoryBackend

```rust
pub struct MemoryBackend {
    inner: Arc<RwLock<HashMap<ColumnFamily, BTreeMap<Vec<u8>, Vec<u8>>>>>,
}

impl MemoryBackend {
    pub fn new() -> Self;
}
```

`MemorySnapshot` 是创建瞬间所有 CF 的 `Arc<HashMap<...>>` clone（O(N)，可接受 —— 测试用途）。

---

## §6 测试

### 6.1 单元测试（in `src/*.rs`）

- `cf::tests` — ColumnFamily round-trip via `as_str` / `ALL` 包含全部 variant
- `batch::tests` — put/delete 推入；len/is_empty；iter 顺序
- `error::tests` — display 文本稳定
- `memory::tests` — basic happy path
- `rocksdb::tests` — basic happy path with `open_in_temp`

### 6.2 Conformance suite（`tests/conformance.rs`）

用 macro `backend_tests!($name, $factory)` 把同一组测试对 Memory 与 RocksDB 两个 backend 各跑一遍：

| 测试 | 验证内容 |
|---|---|
| `get_returns_none_on_missing` | 未写入的 key → None |
| `put_then_get_round_trips` | put → get 返回相同 value |
| `delete_removes_key` | put → delete → get None |
| `write_batch_atomic_across_cfs` | batch 含 2 CF 的写，全部生效 |
| `snapshot_sees_consistent_view` | snapshot 后修改 db，snapshot 读旧值 |
| `iter_returns_sorted_keys` | iterator 输出按 key 字节序 |
| `iter_prefix_filters_correctly` | prefix iter 只返回前缀匹配的 keys |
| `cross_cf_keys_dont_collide` | 同 key 在不同 CF 互不干扰 |

### 6.3 Property tests（`tests/proptest.rs`）

- `equivalence_under_random_op_sequence` — 随机 `Vec<Op>` apply 到两个 backend，最终对每个 CF 的全 iter 输出相等
- `snapshot_isolation_under_concurrent_write` — 启动一个 snapshot，并发 spawn 一个 thread 做 100 次 write，snapshot 读到的值不变

### 6.4 Benchmark（`benches/write_throughput.rs`）

`criterion` benchmark：单线程顺序写 100k 条记录，每条 32B 随机 key + 256B 随机 value，到 `State` CF。

```rust
fn bench_write(c: &mut Criterion) {
    c.bench_function("rocksdb_write_100k_lz4", |b| {
        b.iter_batched(
            || RocksDbBackend::open_in_temp().unwrap(),
            |db| write_100k_records(&db),
            BatchSize::PerIteration,
        );
    });
}
```

CI invokes `cargo bench --bench write_throughput -- --quick`；本地 dev 跑全量 quick 后 assert 吞吐 ≥ 50k op/s（脚本 `scripts/check_storage_perf.sh`，CI 选择性启用）。

---

## §7 提交计划

按 codec / crypto 已建立的小步原子提交模式：

1. `chore: register aii-storage in workspace`
2. `feat(storage): scaffold + StorageError + ColumnFamily enum`
3. `feat(storage): WriteBatch (backend-agnostic Op log)`
4. `feat(storage): KvBackend + Snapshot traits`
5. `feat(storage): MemoryBackend (BTreeMap, snapshot via Arc<clone>)`
6. `feat(storage): RocksDbBackend (cf open + put/get/delete/iter)`
7. `feat(storage): RocksDB snapshot + WriteBatch replay`
8. `test(storage): conformance suite (memory + rocksdb parametrized)`
9. `test(storage): proptest (Op sequence equivalence)`
10. `bench(storage): write_throughput criterion + perf check script`
11. `docs(storage): crate README + rustdoc clean build`
12. `release: v0.0.4 — aii-storage`

---

## §8 接口锁定声明

按 workspace 规范 §5.2，本 crate `lib.rs` 头部声明：

```rust
//! ## Public API
//!
//! - [`KvBackend`] / [`Snapshot`] traits
//! - [`ColumnFamily`] enum
//! - [`WriteBatch`]
//! - [`RocksDbBackend`] / [`MemoryBackend`]
//! - [`StorageError`]
//!
//! ## Internal
//!
//! Any `pub(crate)` items wire backends to the trait machinery; they are
//! expected to churn in 0.0.x.
```

公共 API 变更（trait 签名 / CF enum） 要求修订 workspace 规范 §3.1 #4 的"职责"列。

---

## §9 后续 (out of scope)

| 项 | 计划 |
|---|---|
| Async wrapper | 等 aii-rpc 上线后看实际使用模式再加 `AsyncKv` adapter |
| TTL CF / Merge operator | spec 修订；M2 时机 |
| 历史 state pruning | 由 aii-state 在 trie 层处理；本 crate 不感知 |
| Snapshot 转可写 batch | 暂不提供；调用方手动构造 WriteBatch |
| 远程 KV / 分片 | Day-1+ |

---

## §10 变更日志

| 版本 | 日期 | 变更 |
|---|---|---|
| v0.1 | 2026-05-24 | 初稿；KV trait + RocksDB + Memory + 闭集 CF + Snapshot + WriteBatch；M0 退出基准 50k op/s |
