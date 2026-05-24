//! Read-only consistent view of a [`crate::backend::KvBackend`].
//!
//! Created via [`KvBackend::snapshot`]; reads see the database state at the
//! moment of creation regardless of concurrent writes. Snapshots are not
//! mutable — callers who need to "write on top of a snapshot" should build
//! a [`crate::WriteBatch`] and commit it via the parent backend.

use crate::{cf::ColumnFamily, error::StorageError};

/// A single `(key, value)` pair surfaced by a [`KvIter`].
pub type KvItem = Result<(Vec<u8>, Vec<u8>), StorageError>;

/// Trait alias for the boxed-iterator type backends hand out, to keep
/// the trait signatures readable.
pub type KvIter<'a> = Box<dyn Iterator<Item = KvItem> + 'a>;

/// A read-only point-in-time view.
pub trait Snapshot: Send + Sync {
    /// Read a single value.
    ///
    /// # Errors
    /// Returns [`StorageError`] if the backend fails.
    fn get(&self, cf: ColumnFamily, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError>;

    /// Iterate every `(key, value)` pair in `cf` in ascending key order.
    fn iter<'a>(&'a self, cf: ColumnFamily) -> KvIter<'a>;
}
