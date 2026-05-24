//! [`KvBackend`] — the abstract storage trait every backend implements.

use crate::{
    batch::WriteBatch,
    cf::ColumnFamily,
    error::StorageError,
    snapshot::{KvIter, Snapshot},
};

/// Backend-abstracted KV store.
///
/// Implementors must be safe to share across threads (`Send + Sync`) and
/// outlive `&self` (`'static` bound enables `Arc<dyn KvBackend>` patterns
/// downstream).
pub trait KvBackend: Send + Sync + 'static {
    /// Snapshot type returned by [`KvBackend::snapshot`].
    type Snapshot: Snapshot;

    /// Read a single value.
    ///
    /// # Errors
    /// Returns [`StorageError`] on backend failure.
    fn get(&self, cf: ColumnFamily, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError>;

    /// Insert / overwrite a single key.
    ///
    /// # Errors
    /// Returns [`StorageError`] on backend failure.
    fn put(&self, cf: ColumnFamily, key: &[u8], value: &[u8]) -> Result<(), StorageError>;

    /// Delete a single key. No-op if absent.
    ///
    /// # Errors
    /// Returns [`StorageError`] on backend failure.
    fn delete(&self, cf: ColumnFamily, key: &[u8]) -> Result<(), StorageError>;

    /// Atomically apply `batch`. All ops land together or none do.
    ///
    /// # Errors
    /// Returns [`StorageError`] on backend failure.
    fn write(&self, batch: WriteBatch) -> Result<(), StorageError>;

    /// Take a consistent read-only snapshot of the current DB state.
    fn snapshot(&self) -> Self::Snapshot;

    /// Iterate every `(key, value)` pair in `cf` in ascending key order.
    fn iter(&self, cf: ColumnFamily) -> KvIter<'_>;

    /// Iterate `(key, value)` pairs in `cf` whose key starts with `prefix`,
    /// in ascending order.
    fn iter_prefix<'a>(&'a self, cf: ColumnFamily, prefix: &'a [u8]) -> KvIter<'a>;
}
