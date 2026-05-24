//! In-process [`KvBackend`] backed by `BTreeMap` per column family.
//!
//! Intended for unit tests in downstream crates (aii-state, aii-block, ...)
//! that need a real storage backend without spinning up RocksDB. Snapshot
//! semantics are achieved by cloning the entire per-CF map into an `Arc`
//! at snapshot time — O(N) but acceptable for test data sizes.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};

use crate::{
    backend::KvBackend,
    batch::{Op, WriteBatch},
    cf::ColumnFamily,
    error::StorageError,
    snapshot::{KvItem, KvIter, Snapshot},
};

type CfMap = BTreeMap<Vec<u8>, Vec<u8>>;
type Store = HashMap<ColumnFamily, CfMap>;

/// In-memory KV backend. Cheap to construct; loses data on drop.
#[derive(Clone, Default)]
pub struct MemoryBackend {
    inner: Arc<RwLock<Store>>,
}

impl MemoryBackend {
    /// New empty backend, with one empty `BTreeMap` per [`ColumnFamily`].
    #[must_use]
    pub fn new() -> Self {
        let mut store = Store::with_capacity(ColumnFamily::ALL.len());
        for cf in ColumnFamily::ALL {
            store.insert(*cf, CfMap::new());
        }
        Self {
            inner: Arc::new(RwLock::new(store)),
        }
    }
}

impl KvBackend for MemoryBackend {
    type Snapshot = MemorySnapshot;

    fn get(&self, cf: ColumnFamily, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        let store = self.inner.read().expect("memory backend lock poisoned");
        let cf_map = store
            .get(&cf)
            .ok_or(StorageError::InvalidColumnFamily(cf))?;
        Ok(cf_map.get(key).cloned())
    }

    fn put(&self, cf: ColumnFamily, key: &[u8], value: &[u8]) -> Result<(), StorageError> {
        let mut store = self.inner.write().expect("memory backend lock poisoned");
        let cf_map = store
            .get_mut(&cf)
            .ok_or(StorageError::InvalidColumnFamily(cf))?;
        cf_map.insert(key.to_vec(), value.to_vec());
        Ok(())
    }

    fn delete(&self, cf: ColumnFamily, key: &[u8]) -> Result<(), StorageError> {
        let mut store = self.inner.write().expect("memory backend lock poisoned");
        let cf_map = store
            .get_mut(&cf)
            .ok_or(StorageError::InvalidColumnFamily(cf))?;
        cf_map.remove(key);
        Ok(())
    }

    fn write(&self, batch: WriteBatch) -> Result<(), StorageError> {
        let mut store = self.inner.write().expect("memory backend lock poisoned");
        for op in batch.iter() {
            match op {
                Op::Put { cf, key, value } => {
                    let cf_map = store
                        .get_mut(cf)
                        .ok_or(StorageError::InvalidColumnFamily(*cf))?;
                    cf_map.insert(key.clone(), value.clone());
                }
                Op::Delete { cf, key } => {
                    let cf_map = store
                        .get_mut(cf)
                        .ok_or(StorageError::InvalidColumnFamily(*cf))?;
                    cf_map.remove(key);
                }
            }
        }
        Ok(())
    }

    fn snapshot(&self) -> Self::Snapshot {
        let store = self.inner.read().expect("memory backend lock poisoned");
        MemorySnapshot {
            store: Arc::new(store.clone()),
        }
    }

    fn iter<'a>(&'a self, cf: ColumnFamily) -> KvIter<'a> {
        let store = self.inner.read().expect("memory backend lock poisoned");
        let items: Vec<KvItem> = match store.get(&cf) {
            Some(map) => map.iter().map(|(k, v)| Ok((k.clone(), v.clone()))).collect(),
            None => vec![Err(StorageError::InvalidColumnFamily(cf))],
        };
        Box::new(items.into_iter())
    }

    fn iter_prefix<'a>(&'a self, cf: ColumnFamily, prefix: &'a [u8]) -> KvIter<'a> {
        let store = self.inner.read().expect("memory backend lock poisoned");
        let items: Vec<KvItem> = match store.get(&cf) {
            Some(map) => map
                .range(prefix.to_vec()..)
                .take_while(|(k, _)| k.starts_with(prefix))
                .map(|(k, v)| Ok((k.clone(), v.clone())))
                .collect(),
            None => vec![Err(StorageError::InvalidColumnFamily(cf))],
        };
        Box::new(items.into_iter())
    }
}

/// Snapshot of a [`MemoryBackend`] taken at construction time.
#[derive(Clone)]
pub struct MemorySnapshot {
    store: Arc<Store>,
}

impl Snapshot for MemorySnapshot {
    fn get(&self, cf: ColumnFamily, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        let cf_map = self
            .store
            .get(&cf)
            .ok_or(StorageError::InvalidColumnFamily(cf))?;
        Ok(cf_map.get(key).cloned())
    }

    fn iter<'a>(&'a self, cf: ColumnFamily) -> KvIter<'a> {
        let items: Vec<KvItem> = match self.store.get(&cf) {
            Some(map) => map.iter().map(|(k, v)| Ok((k.clone(), v.clone()))).collect(),
            None => vec![Err(StorageError::InvalidColumnFamily(cf))],
        };
        Box::new(items.into_iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_then_get_round_trips() {
        let b = MemoryBackend::new();
        b.put(ColumnFamily::State, b"k", b"v").unwrap();
        assert_eq!(b.get(ColumnFamily::State, b"k").unwrap().as_deref(), Some(&b"v"[..]));
    }

    #[test]
    fn delete_removes_key() {
        let b = MemoryBackend::new();
        b.put(ColumnFamily::State, b"k", b"v").unwrap();
        b.delete(ColumnFamily::State, b"k").unwrap();
        assert_eq!(b.get(ColumnFamily::State, b"k").unwrap(), None);
    }

    #[test]
    fn snapshot_is_isolated_from_later_writes() {
        let b = MemoryBackend::new();
        b.put(ColumnFamily::State, b"k", b"v1").unwrap();
        let snap = b.snapshot();
        b.put(ColumnFamily::State, b"k", b"v2").unwrap();
        assert_eq!(snap.get(ColumnFamily::State, b"k").unwrap().as_deref(), Some(&b"v1"[..]));
        assert_eq!(b.get(ColumnFamily::State, b"k").unwrap().as_deref(), Some(&b"v2"[..]));
    }
}
