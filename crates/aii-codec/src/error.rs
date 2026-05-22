//! Unified error type for `aii-codec`.
//!
//! Each codec module returns its own native error (`alloy_rlp::Error`,
//! `SszError` from `crate::ssz`, `serde_json::Error`, [`crate::hex::HexError`]).
//! `CodecError` is the umbrella callers reach for when they don't care
//! which format produced the error.

use crate::hex::HexError;
use crate::ssz::SszError;
use thiserror::Error;

/// Umbrella error returned by `aii-codec` convenience functions.
#[derive(Debug, Error)]
pub enum CodecError {
    /// RLP encode/decode error.
    #[error("RLP error: {0}")]
    Rlp(#[from] alloy_rlp::Error),

    /// SSZ decode error.
    #[error("SSZ error: {0}")]
    Ssz(#[from] SszError),

    /// JSON encode/decode error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Hex parsing error (missing prefix, odd length, ...).
    #[error("hex error: {0}")]
    Hex(#[from] HexError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_error_converts_to_codec_error() {
        let inner = HexError::MissingPrefix;
        let outer: CodecError = inner.into();
        assert!(matches!(outer, CodecError::Hex(_)));
    }

    #[test]
    fn rlp_error_converts_to_codec_error() {
        let inner = alloy_rlp::Error::UnexpectedLength;
        let outer: CodecError = inner.into();
        assert!(matches!(outer, CodecError::Rlp(_)));
    }

    #[test]
    fn json_error_converts_to_codec_error() {
        let inner = serde_json::from_str::<u32>("not valid").unwrap_err();
        let outer: CodecError = inner.into();
        assert!(matches!(outer, CodecError::Json(_)));
    }

    #[test]
    fn ssz_error_converts_to_codec_error() {
        let inner = SszError::BadOffsetTable;
        let outer: CodecError = inner.into();
        assert!(matches!(outer, CodecError::Ssz(_)));
    }

    #[test]
    fn error_messages_include_inner_format() {
        let e: CodecError = HexError::MissingPrefix.into();
        assert!(format!("{e}").starts_with("hex error:"));
    }
}
