//! Umbrella error for `aii-state`.

use thiserror::Error;

/// Errors produced while reading or writing world-state.
#[derive(Debug, Error)]
pub enum StateError {
    /// Underlying storage backend error.
    #[error("storage: {0}")]
    Storage(#[from] aii_storage::StorageError),

    /// RLP encode/decode failure.
    #[error("rlp: {0}")]
    Rlp(#[from] alloy_rlp::Error),
}
