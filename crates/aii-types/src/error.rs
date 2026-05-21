//! Unified error type for `aii-types`.

use crate::algo::AlgoIdError;
use thiserror::Error;

/// Top-level error returned by `aii-types` operations.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TypesError {
    /// Failed to decode an [`AlgoId`](crate::AlgoId) byte.
    #[error("algo-id decode error: {0}")]
    AlgoId(#[from] AlgoIdError),

    /// Field width mismatch.
    #[error("invalid field length: expected {expected}, got {actual}")]
    InvalidLength {
        /// Required byte length.
        expected: usize,
        /// Provided byte length.
        actual: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AlgoId;

    #[test]
    fn algo_id_error_converts_to_types_error() {
        let inner = AlgoId::from_byte(0xFF).unwrap_err();
        let outer: TypesError = inner.into();
        assert!(matches!(outer, TypesError::AlgoId(_)));
    }

    #[test]
    fn invalid_length_formats_human_readable() {
        let e = TypesError::InvalidLength {
            expected: 48,
            actual: 33,
        };
        assert_eq!(format!("{e}"), "invalid field length: expected 48, got 33");
    }
}
