//! RocksDB-backed [`KvBackend`] — the production backend.
//!
//! Gated behind the `rocksdb` cargo feature (on by default). Disabling lets
//! downstream crates compile a slim version that only uses
//! [`crate::memory::MemoryBackend`].

#![allow(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rocksdb::{ColumnFamilyDescriptor, IteratorMode, Options, ReadOptions, DB};

use crate::{
    backend::KvBackend,
    batch::{Op, WriteBatch},
    cf::ColumnFamily,
    error::StorageError,
    snapshot::{KvIter, Snapshot},
};

/// Production RocksDB-backed KV store.
#[derive(Clone)]
pub struct RocksDbBackend {
    db: Arc<DB>,
    // Keep path alive for diagnostics + `open_in_temp`'s tempdir lifecycle.
    _path: PathBuf,
}

impl RocksDbBackend {
    /// Open the DB at `path`, creating it (and every column family in
    /// [`ColumnFamily::ALL`]) if absent.
    ///
    /// # Errors
    /// Returns [`StorageError::Backend`] if `RocksDB` fails to open / create.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);
        opts.set_use_fsync(false);
        opts.set_bytes_per_sync(1 << 20); // 1 MiB
        opts.set_compression_type(rocksdb::DBCompressionType::Lz4);

        let cf_descs: Vec<ColumnFamilyDescriptor> = ColumnFamily::ALL
            .iter()
            .map(|cf| ColumnFamilyDescriptor::new(cf.as_str(), Options::default()))
            .collect();

        let db = DB::open_cf_descriptors(&opts, path.as_ref(), cf_descs)
            .map_err(|e| StorageError::Backend(e.to_string()))?;

        Ok(Self {
            db: Arc::new(db),
            _path: path.as_ref().to_path_buf(),
        })
    }

    /// Open a fresh DB in a private tempdir — used by unit/integration tests.
    /// The tempdir is leaked intentionally; tests run in `target/tmp/...`
    /// which the OS reaps eventually.
    ///
    /// # Errors
    /// Returns [`StorageError::Backend`] / [`StorageError::Io`] on failure.
    pub fn open_in_temp() -> Result<Self, StorageError> {
        let dir = tempfile::tempdir().map_err(StorageError::Io)?;
        // Leak the tempdir guard so it outlives this fn; the OS reaps
        // `target/tmp/.../` eventually. `.keep()` is the non-deprecated
        // replacement for `into_path()` in tempfile 3.13+.
        let path = dir.keep();
        Self::open(path)
    }

    fn cf_handle(&self, cf: ColumnFamily) -> Result<&rocksdb::ColumnFamily, StorageError> {
        self.db
            .cf_handle(cf.as_str())
            .ok_or(StorageError::InvalidColumnFamily(cf))
    }
}

impl KvBackend for RocksDbBackend {
    type Snapshot = RocksDbSnapshot;

    fn get(&self, cf: ColumnFamily, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        let handle = self.cf_handle(cf)?;
        self.db
            .get_cf(handle, key)
            .map_err(|e| StorageError::Backend(e.to_string()))
    }

    fn put(&self, cf: ColumnFamily, key: &[u8], value: &[u8]) -> Result<(), StorageError> {
        let handle = self.cf_handle(cf)?;
        self.db
            .put_cf(handle, key, value)
            .map_err(|e| StorageError::Backend(e.to_string()))
    }

    fn delete(&self, cf: ColumnFamily, key: &[u8]) -> Result<(), StorageError> {
        let handle = self.cf_handle(cf)?;
        self.db
            .delete_cf(handle, key)
            .map_err(|e| StorageError::Backend(e.to_string()))
    }

    fn write(&self, batch: WriteBatch) -> Result<(), StorageError> {
        let mut wb = rocksdb::WriteBatch::default();
        for op in batch.iter() {
            match op {
                Op::Put { cf, key, value } => {
                    let handle = self.cf_handle(*cf)?;
                    wb.put_cf(handle, key, value);
                }
                Op::Delete { cf, key } => {
                    let handle = self.cf_handle(*cf)?;
                    wb.delete_cf(handle, key);
                }
            }
        }
        self.db
            .write(wb)
            .map_err(|e| StorageError::Backend(e.to_string()))
    }

    fn snapshot(&self) -> Self::Snapshot {
        RocksDbSnapshot {
            // SAFETY: We lift the snapshot's lifetime to 'static. This is
            // sound iff: (a) the underlying `DB` outlives the snapshot,
            // guaranteed by the sibling `Arc<DB>` we store alongside, AND
            // (b) when `RocksDbSnapshot` is dropped, `snap` drops BEFORE
            // `db` so the snapshot's `release_snapshot` FFI call runs
            // against a still-live DB. Rust drops struct fields in
            // declaration order — `snap` is declared first below.
            snap: unsafe {
                std::mem::transmute::<rocksdb::Snapshot<'_>, rocksdb::Snapshot<'static>>(
                    self.db.snapshot(),
                )
            },
            db: Arc::clone(&self.db),
        }
    }

    fn iter(&self, cf: ColumnFamily) -> KvIter<'_> {
        let handle = match self.cf_handle(cf) {
            Ok(h) => h,
            Err(e) => return Box::new(std::iter::once(Err(e))),
        };
        Box::new(self.db.iterator_cf(handle, IteratorMode::Start).map(|kv| {
            kv.map(|(k, v)| (k.to_vec(), v.to_vec()))
                .map_err(|e| StorageError::Backend(e.to_string()))
        }))
    }

    fn iter_prefix<'a>(&'a self, cf: ColumnFamily, prefix: &'a [u8]) -> KvIter<'a> {
        let handle = match self.cf_handle(cf) {
            Ok(h) => h,
            Err(e) => return Box::new(std::iter::once(Err(e))),
        };
        let mut read_opts = ReadOptions::default();
        read_opts.set_iterate_lower_bound(prefix.to_vec());
        let upper = next_prefix_upper_bound(prefix);
        if let Some(ub) = upper {
            read_opts.set_iterate_upper_bound(ub);
        }
        Box::new(
            self.db
                .iterator_cf_opt(handle, read_opts, IteratorMode::Start)
                .map(|kv| {
                    kv.map(|(k, v)| (k.to_vec(), v.to_vec()))
                        .map_err(|e| StorageError::Backend(e.to_string()))
                }),
        )
    }
}

/// Compute the lexicographic upper bound of all keys starting with `prefix`.
/// `None` means "no finite upper bound" (the prefix is all-0xFF bytes).
fn next_prefix_upper_bound(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut out = prefix.to_vec();
    while let Some(b) = out.last_mut() {
        if *b == 0xFF {
            out.pop();
        } else {
            *b += 1;
            return Some(out);
        }
    }
    None
}

/// Read-only snapshot of a [`RocksDbBackend`].
pub struct RocksDbSnapshot {
    // Field order matters: `snap` drops FIRST so its Drop impl calls
    // `release_snapshot` while the underlying DB is still alive via the
    // sibling `Arc<DB>` clone below.
    snap: rocksdb::Snapshot<'static>,
    db: Arc<DB>,
}

impl RocksDbSnapshot {
    fn cf_handle(&self, cf: ColumnFamily) -> Result<&rocksdb::ColumnFamily, StorageError> {
        self.db
            .cf_handle(cf.as_str())
            .ok_or(StorageError::InvalidColumnFamily(cf))
    }
}

impl Snapshot for RocksDbSnapshot {
    fn get(&self, cf: ColumnFamily, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        let handle = self.cf_handle(cf)?;
        self.snap
            .get_cf(handle, key)
            .map_err(|e| StorageError::Backend(e.to_string()))
    }

    fn iter(&self, cf: ColumnFamily) -> KvIter<'_> {
        let handle = match self.cf_handle(cf) {
            Ok(h) => h,
            Err(e) => return Box::new(std::iter::once(Err(e))),
        };
        Box::new(
            self.snap
                .iterator_cf(handle, IteratorMode::Start)
                .map(|kv| {
                    kv.map(|(k, v)| (k.to_vec(), v.to_vec()))
                        .map_err(|e| StorageError::Backend(e.to_string()))
                }),
        )
    }
}

// SAFETY: `Snapshot<'static>` holds a pointer derived from `Arc<DB>`; the
// snapshot's lifetime is bound to the DB clone we keep, not the original
// borrow. RocksDB snapshots are documented to be `Send + Sync` and safe to
// hand across threads as long as the DB outlives them — which `Arc<DB>`
// guarantees.
unsafe impl Send for RocksDbSnapshot {}
unsafe impl Sync for RocksDbSnapshot {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_in_temp_then_put_get_round_trips() {
        let b = RocksDbBackend::open_in_temp().unwrap();
        b.put(ColumnFamily::State, b"k", b"v").unwrap();
        assert_eq!(
            b.get(ColumnFamily::State, b"k").unwrap().as_deref(),
            Some(&b"v"[..])
        );
    }

    #[test]
    fn delete_removes_key() {
        let b = RocksDbBackend::open_in_temp().unwrap();
        b.put(ColumnFamily::State, b"k", b"v").unwrap();
        b.delete(ColumnFamily::State, b"k").unwrap();
        assert_eq!(b.get(ColumnFamily::State, b"k").unwrap(), None);
    }

    #[test]
    fn snapshot_is_isolated_from_later_writes() {
        let b = RocksDbBackend::open_in_temp().unwrap();
        b.put(ColumnFamily::State, b"k", b"v1").unwrap();
        let snap = b.snapshot();
        b.put(ColumnFamily::State, b"k", b"v2").unwrap();
        assert_eq!(
            snap.get(ColumnFamily::State, b"k").unwrap().as_deref(),
            Some(&b"v1"[..])
        );
        assert_eq!(
            b.get(ColumnFamily::State, b"k").unwrap().as_deref(),
            Some(&b"v2"[..])
        );
    }

    #[test]
    fn next_prefix_upper_bound_works() {
        assert_eq!(next_prefix_upper_bound(b"abc"), Some(b"abd".to_vec()));
        assert_eq!(
            next_prefix_upper_bound(&[0xFF, 0x00]),
            Some(vec![0xFF, 0x01])
        );
        assert_eq!(next_prefix_upper_bound(&[0xFF, 0xFF]), None);
    }

    #[test]
    fn snapshot_outlives_backend_drop_does_not_uaf() {
        // Reproduces the UAF that existed in the original field order:
        // we capture a snapshot, then DROP the backend (releasing its
        // Arc<DB> strong ref), then read through the snapshot. The
        // snapshot's own Arc<DB> clone keeps the DB alive, and the
        // field-drop order ensures `release_snapshot` runs before the
        // DB is freed when the snapshot itself is dropped at scope end.
        let b = RocksDbBackend::open_in_temp().unwrap();
        b.put(ColumnFamily::State, b"k", b"v").unwrap();
        let snap = b.snapshot();
        drop(b); // release the original strong ref
                 // Snapshot now holds the only Arc<DB> strong ref.
        assert_eq!(
            snap.get(ColumnFamily::State, b"k").unwrap().as_deref(),
            Some(&b"v"[..])
        );
        // snap drops here — must not UAF.
    }
}
