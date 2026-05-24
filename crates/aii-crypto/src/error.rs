//! Unified error type for `aii-crypto`.
//!
//! Each crypto module returns its own native error. [`CryptoError`] is the
//! umbrella that callers reach for when they do not care which primitive
//! produced the failure (e.g. a top-level signature-verification dispatch).

use thiserror::Error;

/// Umbrella error returned by `aii-crypto` convenience functions.
#[derive(Debug, Error)]
pub enum CryptoError {
    /// Catch-all wire-format error (e.g. wrong byte length for a key).
    #[error("invalid encoding: {0}")]
    InvalidEncoding(&'static str),

    /// Signature verification ran to completion and rejected the signature.
    #[error("signature verification failed")]
    BadSignature,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_encoding_includes_reason() {
        let e = CryptoError::InvalidEncoding("pubkey must be 33 bytes");
        assert!(format!("{e}").contains("pubkey must be 33 bytes"));
    }

    #[test]
    fn bad_signature_message_is_stable() {
        assert_eq!(
            format!("{}", CryptoError::BadSignature),
            "signature verification failed"
        );
    }
}
