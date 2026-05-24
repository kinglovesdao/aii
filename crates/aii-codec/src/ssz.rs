//! SSZ impls for AII primitives, layered on top of `ssz_rs`.
//!
//! Strategy: each public type (`H256`, `Address`, `BlsPubKey`, ...) is mapped
//! to an `ssz_rs` byte vector of its fixed length. Encoding becomes a memcpy.
//! `SignedTx` is the only variable-length case and uses an SSZ container
//! (Task 13) with offset-table layout.

use aii_types::{Address, AlgoId, BlsPubKey, BlsSignature, SignedTx, H256};

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
    /// A `SignedTx` offset table was malformed.
    #[error("malformed SignedTx offset table")]
    BadOffsetTable,
}

/// Encode an [`AlgoId`] as a 1-byte SSZ uint8.
#[must_use]
pub fn encode_algo_id(a: AlgoId) -> Vec<u8> {
    vec![a.as_byte()]
}

/// Decode an SSZ-encoded [`AlgoId`].
pub fn decode_algo_id(bytes: &[u8]) -> Result<AlgoId, SszError> {
    if bytes.len() != 1 {
        return Err(SszError::InvalidLength {
            expected: 1,
            actual: bytes.len(),
        });
    }
    AlgoId::from_byte(bytes[0]).map_err(|_| SszError::InvalidByte(bytes[0]))
}

/// Encode a [`BlsPubKey`] (48-byte SSZ Vector<u8, 48>).
#[must_use]
pub fn encode_bls_pubkey(k: &BlsPubKey) -> Vec<u8> {
    k.as_bytes().to_vec()
}

/// Decode a 48-byte SSZ payload into a [`BlsPubKey`].
pub fn decode_bls_pubkey(bytes: &[u8]) -> Result<BlsPubKey, SszError> {
    if bytes.len() != 48 {
        return Err(SszError::InvalidLength {
            expected: 48,
            actual: bytes.len(),
        });
    }
    let mut out = [0u8; 48];
    out.copy_from_slice(bytes);
    Ok(BlsPubKey::new(out))
}

/// Encode a [`BlsSignature`] (96-byte SSZ Vector<u8, 96>).
#[must_use]
pub fn encode_bls_signature(s: &BlsSignature) -> Vec<u8> {
    s.as_bytes().to_vec()
}

/// Decode a 96-byte SSZ payload into a [`BlsSignature`].
pub fn decode_bls_signature(bytes: &[u8]) -> Result<BlsSignature, SszError> {
    if bytes.len() != 96 {
        return Err(SszError::InvalidLength {
            expected: 96,
            actual: bytes.len(),
        });
    }
    let mut out = [0u8; 96];
    out.copy_from_slice(bytes);
    Ok(BlsSignature::new(out))
}

/// Encode a [`SignedTx`] as an SSZ container with layout:
///
/// ```text
/// container SignedTx {
///     algo_id  : uint8,                  // fixed 1B
///     pubkey   : List[byte, MAX_PUBKEY], // variable (offset)
///     signature: List[byte, MAX_SIG],    // variable (offset)
///     payload  : List[byte, MAX_PAYLOAD] // variable (offset)
/// }
/// ```
///
/// Wire layout: `[algo_id (1B), off1 (4B LE), off2 (4B LE), off3 (4B LE), pubkey, signature, payload]`.
#[must_use]
pub fn encode_signed_tx(tx: &SignedTx) -> Vec<u8> {
    let fixed_part: usize = 1 + 4 + 4 + 4;
    let off1 = fixed_part as u32;
    let off2 = off1 + tx.pubkey.len() as u32;
    let off3 = off2 + tx.signature.len() as u32;

    let mut out =
        Vec::with_capacity(fixed_part + tx.pubkey.len() + tx.signature.len() + tx.payload.len());
    out.push(tx.algo_id.as_byte());
    out.extend_from_slice(&off1.to_le_bytes());
    out.extend_from_slice(&off2.to_le_bytes());
    out.extend_from_slice(&off3.to_le_bytes());
    out.extend_from_slice(&tx.pubkey);
    out.extend_from_slice(&tx.signature);
    out.extend_from_slice(&tx.payload);
    out
}

/// Decode an SSZ-encoded [`SignedTx`].
pub fn decode_signed_tx(bytes: &[u8]) -> Result<SignedTx, SszError> {
    let fixed_part: usize = 1 + 4 + 4 + 4;
    if bytes.len() < fixed_part {
        return Err(SszError::InvalidLength {
            expected: fixed_part,
            actual: bytes.len(),
        });
    }
    let algo_id = AlgoId::from_byte(bytes[0]).map_err(|_| SszError::InvalidByte(bytes[0]))?;
    let off1 = u32::from_le_bytes(bytes[1..5].try_into().unwrap()) as usize;
    let off2 = u32::from_le_bytes(bytes[5..9].try_into().unwrap()) as usize;
    let off3 = u32::from_le_bytes(bytes[9..13].try_into().unwrap()) as usize;

    if off1 != fixed_part || off2 < off1 || off3 < off2 || off3 > bytes.len() {
        return Err(SszError::BadOffsetTable);
    }
    let pubkey = bytes[off1..off2].to_vec();
    let signature = bytes[off2..off3].to_vec();
    let payload = bytes[off3..].to_vec();
    Ok(SignedTx::new(algo_id, pubkey, signature, payload))
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

    #[test]
    fn algo_id_all_variants_round_trip() {
        for v in [
            AlgoId::Secp256k1,
            AlgoId::Ed25519,
            AlgoId::Bls12_381,
            AlgoId::MlDsa65,
            AlgoId::SlhDsa128s,
            AlgoId::Falcon512,
            AlgoId::HybridSecpMlDsa,
        ] {
            assert_eq!(decode_algo_id(&encode_algo_id(v)).unwrap(), v);
        }
    }

    #[test]
    fn algo_id_decode_rejects_unknown_byte() {
        assert!(decode_algo_id(&[0xFF]).is_err());
    }

    #[test]
    fn bls_pubkey_round_trips() {
        let k = BlsPubKey::new([0x77; 48]);
        let encoded = encode_bls_pubkey(&k);
        assert_eq!(encoded.len(), 48);
        assert_eq!(decode_bls_pubkey(&encoded).unwrap(), k);
    }

    #[test]
    fn bls_signature_round_trips() {
        let s = BlsSignature::new([0x88; 96]);
        let encoded = encode_bls_signature(&s);
        assert_eq!(encoded.len(), 96);
        assert_eq!(decode_bls_signature(&encoded).unwrap(), s);
    }

    fn dummy_tx() -> SignedTx {
        SignedTx::new(
            AlgoId::Secp256k1,
            vec![0xAA; 33],
            vec![0xBB; 65],
            vec![0xCC; 100],
        )
    }

    #[test]
    fn signed_tx_round_trips() {
        let tx = dummy_tx();
        let encoded = encode_signed_tx(&tx);
        assert_eq!(decode_signed_tx(&encoded).unwrap(), tx);
    }

    #[test]
    fn signed_tx_encoded_length_matches_wire_size_plus_offsets() {
        let tx = dummy_tx();
        // wire_size from aii-types is 1 + 33 + 65 + 100 = 199 (no offsets).
        // SSZ adds 3 × 4-byte offsets = 12 extra.
        let encoded = encode_signed_tx(&tx);
        assert_eq!(encoded.len(), 199 + 12);
    }

    #[test]
    fn signed_tx_decode_rejects_truncated_input() {
        let truncated = vec![0u8; 8];
        assert!(decode_signed_tx(&truncated).is_err());
    }

    #[test]
    fn signed_tx_round_trips_with_pq_sizes() {
        let pq_tx = SignedTx::new(
            AlgoId::MlDsa65,
            vec![0x11; 1952],
            vec![0x22; 3309],
            vec![0x33; 256],
        );
        let encoded = encode_signed_tx(&pq_tx);
        assert_eq!(decode_signed_tx(&encoded).unwrap(), pq_tx);
    }
}
