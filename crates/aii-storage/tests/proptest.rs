//! Property: any random sequence of `Op`s applied to `MemoryBackend` and
//! `RocksDbBackend` leaves the two with identical contents.

use aii_storage::{ColumnFamily, KvBackend, MemoryBackend, Op, Snapshot, WriteBatch};
use proptest::prelude::*;

fn cf_strategy() -> impl Strategy<Value = ColumnFamily> {
    prop_oneof![
        Just(ColumnFamily::State),
        Just(ColumnFamily::Headers),
        Just(ColumnFamily::Meta),
    ]
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        (
            cf_strategy(),
            proptest::collection::vec(any::<u8>(), 1..16),
            proptest::collection::vec(any::<u8>(), 0..32)
        )
            .prop_map(|(cf, key, value)| Op::Put { cf, key, value }),
        (cf_strategy(), proptest::collection::vec(any::<u8>(), 1..16))
            .prop_map(|(cf, key)| Op::Delete { cf, key }),
    ]
}

fn apply(db: &impl KvBackend, ops: &[Op]) {
    let mut wb = WriteBatch::new();
    for op in ops {
        match op {
            Op::Put { cf, key, value } => {
                wb.put(*cf, key, value);
            }
            Op::Delete { cf, key } => {
                wb.delete(*cf, key);
            }
        }
    }
    db.write(wb).unwrap();
}

fn dump(db: &impl KvBackend, cf: ColumnFamily) -> Vec<(Vec<u8>, Vec<u8>)> {
    db.iter(cf).map(|r| r.unwrap()).collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]
    #[test]
    fn memory_and_rocksdb_agree_under_random_ops(
        ops in proptest::collection::vec(op_strategy(), 0..50)
    ) {
        let mem = MemoryBackend::new();
        #[cfg(feature = "rocksdb")]
        let rocks = aii_storage::RocksDbBackend::open_in_temp().unwrap();

        apply(&mem, &ops);
        #[cfg(feature = "rocksdb")]
        apply(&rocks, &ops);

        for cf in [ColumnFamily::State, ColumnFamily::Headers, ColumnFamily::Meta] {
            let mem_dump = dump(&mem, cf);
            #[cfg(feature = "rocksdb")]
            {
                let rocks_dump = dump(&rocks, cf);
                prop_assert_eq!(mem_dump.clone(), rocks_dump, "CF {} divergence", cf);
            }
            // smoke: each CF dump is itself sorted
            for w in mem_dump.windows(2) {
                prop_assert!(w[0].0 <= w[1].0);
            }
        }
    }

    #[test]
    fn snapshot_unchanged_under_concurrent_writer(
        seed_pairs in proptest::collection::vec(
            (proptest::collection::vec(any::<u8>(), 1..16), proptest::collection::vec(any::<u8>(), 0..32)),
            0..10
        )
    ) {
        let db = MemoryBackend::new();
        for (k, v) in &seed_pairs {
            db.put(ColumnFamily::State, k, v).unwrap();
        }
        let snap = db.snapshot();

        // Mutate after the snapshot.
        for (k, _) in &seed_pairs {
            db.put(ColumnFamily::State, k, b"OVERWRITTEN").unwrap();
        }

        // Snapshot must still report the original values.
        for (k, v) in &seed_pairs {
            let got = snap.get(ColumnFamily::State, k).unwrap();
            prop_assert_eq!(got.as_deref(), Some(&v[..]));
        }
    }
}
