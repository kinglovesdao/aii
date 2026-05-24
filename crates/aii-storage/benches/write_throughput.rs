//! Sequential write throughput benchmark — the M0 exit gate for aii-storage.
//!
//! 100k records, each: 32-byte deterministic key (`u64::to_be_bytes` zero
//! padded) + 256-byte value. Run as individual `put_cf` calls (not batched)
//! so the number reported is the worst case the protocol layer hits when it
//! has to write one record at a time.
//!
//! The target is 50k op/s on commodity NVMe; failing that, the M0 exit
//! criterion is not met. `scripts/check_storage_perf.sh` parses criterion's
//! output and asserts the threshold for CI.

// `criterion_group!` expands to a `fn benches()` without doc comments;
// the crate-wide `missing_docs = "warn"` would flag it otherwise.
#![allow(missing_docs)]

use aii_storage::{ColumnFamily, KvBackend, RocksDbBackend};
use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};

const N: usize = 100_000;
const VALUE: [u8; 256] = [0xABu8; 256];

fn key_for(i: usize) -> [u8; 32] {
    let mut k = [0u8; 32];
    k[24..].copy_from_slice(&(i as u64).to_be_bytes());
    k
}

fn bench_rocksdb_sequential_put(c: &mut Criterion) {
    let mut group = c.benchmark_group("storage.rocksdb");
    group.throughput(Throughput::Elements(N as u64));
    group.sample_size(10);
    group.bench_function("sequential_put_100k_lz4", |b| {
        b.iter_batched(
            || RocksDbBackend::open_in_temp().expect("open temp db"),
            |db| {
                for i in 0..N {
                    db.put(ColumnFamily::State, &key_for(i), &VALUE).unwrap();
                }
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

criterion_group!(benches, bench_rocksdb_sequential_put);
criterion_main!(benches);
