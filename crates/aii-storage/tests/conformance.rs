//! Conformance tests parametrized over every backend.
//!
//! The `backend_tests!($name, $factory_expr)` macro generates the same
//! battery of tests against whatever backend `$factory_expr` returns.
//! Both backends are tested with the same body so any divergence in
//! semantics surfaces immediately.

use aii_storage::{ColumnFamily, KvBackend, MemoryBackend, Snapshot, WriteBatch};

macro_rules! backend_tests {
    ($mod_name:ident, $factory:expr) => {
        mod $mod_name {
            use super::*;

            fn make_backend() -> impl KvBackend {
                $factory
            }

            #[test]
            fn get_returns_none_on_missing() {
                let db = make_backend();
                assert!(db.get(ColumnFamily::State, b"missing").unwrap().is_none());
            }

            #[test]
            fn put_then_get_round_trips() {
                let db = make_backend();
                db.put(ColumnFamily::State, b"k", b"v").unwrap();
                assert_eq!(
                    db.get(ColumnFamily::State, b"k").unwrap().as_deref(),
                    Some(&b"v"[..])
                );
            }

            #[test]
            fn delete_removes_key() {
                let db = make_backend();
                db.put(ColumnFamily::State, b"k", b"v").unwrap();
                db.delete(ColumnFamily::State, b"k").unwrap();
                assert!(db.get(ColumnFamily::State, b"k").unwrap().is_none());
            }

            #[test]
            fn write_batch_atomic_across_cfs() {
                let db = make_backend();
                let mut wb = WriteBatch::new();
                wb.put(ColumnFamily::State, b"s1", b"sv")
                    .put(ColumnFamily::Headers, b"h1", b"hv")
                    .delete(ColumnFamily::Meta, b"absent");
                db.write(wb).unwrap();
                assert_eq!(db.get(ColumnFamily::State, b"s1").unwrap().as_deref(), Some(&b"sv"[..]));
                assert_eq!(db.get(ColumnFamily::Headers, b"h1").unwrap().as_deref(), Some(&b"hv"[..]));
            }

            #[test]
            fn snapshot_sees_consistent_view() {
                let db = make_backend();
                db.put(ColumnFamily::State, b"k", b"v1").unwrap();
                let snap = db.snapshot();
                db.put(ColumnFamily::State, b"k", b"v2").unwrap();
                assert_eq!(snap.get(ColumnFamily::State, b"k").unwrap().as_deref(), Some(&b"v1"[..]));
            }

            #[test]
            fn iter_returns_sorted_keys() {
                let db = make_backend();
                db.put(ColumnFamily::State, b"b", b"2").unwrap();
                db.put(ColumnFamily::State, b"a", b"1").unwrap();
                db.put(ColumnFamily::State, b"c", b"3").unwrap();
                let keys: Vec<Vec<u8>> = db
                    .iter(ColumnFamily::State)
                    .map(|r| r.unwrap().0)
                    .collect();
                assert_eq!(keys, vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]);
            }

            #[test]
            fn iter_prefix_filters_correctly() {
                let db = make_backend();
                db.put(ColumnFamily::State, b"foo:1", b"a").unwrap();
                db.put(ColumnFamily::State, b"foo:2", b"b").unwrap();
                db.put(ColumnFamily::State, b"bar:3", b"c").unwrap();
                let keys: Vec<Vec<u8>> = db
                    .iter_prefix(ColumnFamily::State, b"foo:")
                    .map(|r| r.unwrap().0)
                    .collect();
                assert_eq!(keys, vec![b"foo:1".to_vec(), b"foo:2".to_vec()]);
            }

            #[test]
            fn cross_cf_keys_dont_collide() {
                let db = make_backend();
                db.put(ColumnFamily::State, b"k", b"state-value").unwrap();
                db.put(ColumnFamily::Headers, b"k", b"headers-value").unwrap();
                assert_eq!(
                    db.get(ColumnFamily::State, b"k").unwrap().as_deref(),
                    Some(&b"state-value"[..])
                );
                assert_eq!(
                    db.get(ColumnFamily::Headers, b"k").unwrap().as_deref(),
                    Some(&b"headers-value"[..])
                );
            }
        }
    };
}

backend_tests!(memory, MemoryBackend::new());

#[cfg(feature = "rocksdb")]
backend_tests!(rocksdb, aii_storage::RocksDbBackend::open_in_temp().unwrap());
