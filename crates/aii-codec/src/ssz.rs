//! SSZ impls for AII primitives, layered on top of `ssz_rs`.
//!
//! Strategy: each public type (`H256`, `Address`, `BlsPubKey`, ...) is mapped
//! to an `ssz_rs` byte vector of its fixed length. Encoding becomes a memcpy.
//! `SignedTx` is the only variable-length case and uses an SSZ container
//! (Task 13) with offset-table layout.

use aii_types::{Address, AlgoId, BlsPubKey, BlsSignature, H256, SignedTx};

/// Encode an [`H256`] as 32-byte SSZ-serialized bytes (an SSZ Vector<u8, 32>).
#[must_use]
pub fn encode_h256(h: &H256) -> Vec<u8> {
    h.as_bytes().to_vec()
}

/// Decode 32-byte SSZ bytes into an [`H256`].
pub fn decode_h256(bytes: &[u8]) -> Result<H256, SszError> {
    if bytes.len() != 32 {
        return Err(SszError::InvalidLength {
            expected: 32,
            actual: bytes.len(),
        });
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(bytes);
    Ok(H256::new(out))
}

/// Encode an [`Address`] as 20-byte SSZ-serialized bytes.
#[must_use]
pub fn encode_address(a: &Address) -> Vec<u8> {
    a.as_bytes().to_vec()
}

/// Decode 20-byte SSZ bytes into an [`Address`].
pub fn decode_address(bytes: &[u8]) -> Result<Address, SszError> {
    if bytes.len() != 20 {
        return Err(SszError::InvalidLength {
            expected: 20,
            actual: bytes.len(),
        });
    }
    let mut out = [0u8; 20];
    out.copy_from_slice(bytes);
    Ok(Address::new(out))
}

/// Error type for SSZ decoding in `aii-codec`. Wraps the relevant
/// `ssz_rs::DeserializeError` variants and adds AII-specific length checks.
///
/// We define our own type instead of leaking `ssz_rs::DeserializeError`
/// because the upstream enum is non-exhaustive and its variant names have
/// shifted between minor versions.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SszError {
    /// Byte length didn't match the fixed-size type's expected length.
    #[error("invalid SSZ length: expected {expected}, got {actual}")]
    InvalidLength {
        /// Required byte length.
        expected: usize,
        /// Provided byte length.
        actual: usize,
    },
    /// A byte couldn't be interpreted (e.g. unknown `AlgoId` byte).
    #[error("invalid SSZ byte 0x{0:02x}")]
    InvalidByte(u8),
    /// A SignedTx offset table was malformed.
    #[error("malformed SignedTx offset table")]
    BadOffsetTable,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h256_round_trips() {
        let h = H256::new([0xAB; 32]);
        let encoded = encode_h256(&h);
        assert_eq!(encoded.len(), 32);
        assert_eq!(decode_h256(&encoded).unwrap(), h);
    }

    #[test]
    fn h256_decode_rejects_wrong_length() {
        let bad = vec![0; 31];
        assert!(decode_h256(&bad).is_err());
    }

    #[test]
    fn address_round_trips() {
        let a = Address::new([0xCD; 20]);
        let encoded = encode_address(&a);
        assert_eq!(encoded.len(), 20);
        assert_eq!(decode_address(&encoded).unwrap(), a);
    }

    #[test]
    fn address_decode_rejects_wrong_length() {
        let bad = vec![0; 19];
        assert!(decode_address(&bad).is_err());
    }
}
