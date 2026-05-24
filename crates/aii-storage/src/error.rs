//! Unified error type for `aii-storage`.
//!
//! `StorageError::Backend` wraps the backend-native error message as a
//! string so the public surface stays free of `rocksdb` types — this is
//! what lets `aii-state` / `aii-block` swap backends in tests without
//! conditional compilation.

use thiserror::Error;

use crate::cf::ColumnFamily;

/// Umbrella error returned by every `aii-storage` API.
#[derive(Debug, Error)]
pub enum StorageError {
    /// Backend-native error (e.g. `RocksDB`), captured as its `Display` text.
    #[error("backend error: {0}")]
    Backend(String),

    /// Backend reports it does not know the named column family.
    #[error("column family not registered: {0}")]
    InvalidColumnFamily(ColumnFamily),

    /// I/O failure outside the backend (e.g. opening the DB directory).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_display_includes_inner_text() {
        let e = StorageError::Backend("disk full".to_string());
        assert!(format!("{e}").contains("disk full"));
    }

    #[test]
    fn invalid_cf_includes_cf_name() {
        let e = StorageError::InvalidColumnFamily(ColumnFamily::State);
        assert!(format!("{e}").contains("state"));
    }

    #[test]
    fn io_error_converts_via_from() {
        let inner = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "nope");
        let outer: StorageError = inner.into();
        assert!(matches!(outer, StorageError::Io(_)));
    }
}
