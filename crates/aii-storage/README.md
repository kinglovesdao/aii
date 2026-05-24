# aii-storage

Key-value storage abstraction for the AII protocol.

## Backends

| Backend          | Use case                                | Feature      |
|------------------|------------------------------------------|--------------|
| `RocksDbBackend` | Production / testnet / mainnet           | `rocksdb` ✅ default |
| `MemoryBackend`  | Downstream-crate unit tests              | always-on    |

Both implement the same `KvBackend` trait — downstream code is
parametric and never names a concrete backend.

## Column families

Closed set, 10 variants (`ColumnFamily::ALL`):

`Default`, `Headers`, `Bodies`, `Receipts`, `Transactions`, `State`,
`AccountStorage`, `TxLookup`, `Meta`, `MicroChain`.

Adding a CF requires a spec revision (see
`docs/superpowers/specs/2026-05-24-aii-storage-design.md`).

## Quickstart

```rust
use aii_storage::{ColumnFamily, KvBackend, RocksDbBackend, WriteBatch};

let db = RocksDbBackend::open("/tmp/aii-db")?;

// Single-op
db.put(ColumnFamily::State, b"key", b"value")?;
let v = db.get(ColumnFamily::State, b"key")?;

// Atomic batch (cross-CF)
let mut batch = WriteBatch::new();
batch.put(ColumnFamily::Headers, b"h1", b"...")
     .put(ColumnFamily::Bodies, b"h1", b"...");
db.write(batch)?;

// Read-only snapshot
let snap = db.snapshot();
// ... use `snap` while concurrent writes proceed on `db` ...
# Ok::<(), aii_storage::StorageError>(())
```

## Testing

```bash
cargo test -p aii-storage                       # unit tests (~20)
cargo test -p aii-storage --test conformance    # 8 tests x 2 backends
cargo test -p aii-storage --test proptest       # 2 properties
cargo bench -p aii-storage --bench write_throughput -- --quick
scripts/check_storage_perf.sh                   # asserts >= 50k op/s
```

## Stability

`0.0.x` — unstable; breaking changes allowed in any release until `0.1.0`.

## Roadmap

- v0.0.4 (this release): `KvBackend` + `Snapshot` traits + 10-CF enum +
  `WriteBatch` + `MemoryBackend` + `RocksDbBackend` + criterion bench
  meeting >=50k op/s gate.
- v0.0.5: TTL CF support (for `Transactions` mempool reap) + ReadCache.
- v0.1.0: Snapshot-promotion to WriteBatch (for `aii-evm` journal
  semantics); cargo-fuzz harness on the WriteBatch replay path.

## External dependencies

| crate      | version | role                                          |
|------------|---------|-----------------------------------------------|
| `rocksdb`  | 0.22    | TiKV Rust binding to librocksdb (lz4 feature) |
| `tempfile` | 3       | test sandbox dirs                             |
| `proptest` | 1       | property testing                              |
| `criterion`| 0.5     | benchmark harness                             |
