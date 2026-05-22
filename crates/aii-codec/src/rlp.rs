//! RLP impls for AII primitives, layered on top of `alloy-rlp`.
//!
//! Strategy: free functions only (the orphan rule blocks `impl Encodable for
//! H256` here because both trait and type are foreign to `aii-codec`). Each
//! function delegates to `alloy_rlp::Encodable` / `Decodable` for the
//! underlying byte slice and validates length after decode.
//!
//! Encoding format per type:
//! - `H256` / `Address` / `BlsPubKey` / `BlsSignature`: fixed-size byte string
//!   (length-prefixed per RLP's standard).
//! - `AlgoId`: 1-byte integer (RLP short string of length 1).
//! - `SignedTx`: 4-element RLP list `[algo_id, pubkey, signature, payload]`.

use aii_types::{Address, H256};
use alloy_rlp::{Decodable, Encodable};

/// Encode an [`H256`] as RLP, returning the bytes.
#[must_use]
pub fn encode_h256(h: &H256) -> alloy_rlp::bytes::BytesMut {
    let mut buf = alloy_rlp::bytes::BytesMut::with_capacity(33);
    h.as_bytes().as_slice().encode(&mut buf);
    buf
}

/// Decode an RLP-encoded [`H256`] from `bytes`.
pub fn decode_h256(mut bytes: &[u8]) -> Result<H256, alloy_rlp::Error> {
    let v = <alloy_rlp::bytes::Bytes as Decodable>::decode(&mut bytes)?;
    if v.len() != 32 {
        return Err(alloy_rlp::Error::UnexpectedLength);
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&v);
    Ok(H256::new(out))
}

/// Encode an [`Address`] as RLP (20-byte string).
#[must_use]
pub fn encode_address(a: &Address) -> alloy_rlp::bytes::BytesMut {
    let mut buf = alloy_rlp::bytes::BytesMut::with_capacity(21);
    a.as_bytes().as_slice().encode(&mut buf);
    buf
}

/// Decode an RLP-encoded [`Address`].
pub fn decode_address(mut bytes: &[u8]) -> Result<Address, alloy_rlp::Error> {
    let v = <alloy_rlp::bytes::Bytes as Decodable>::decode(&mut bytes)?;
    if v.len() != 20 {
        return Err(alloy_rlp::Error::UnexpectedLength);
    }
    let mut out = [0u8; 20];
    out.copy_from_slice(&v);
    Ok(Address::new(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h256_zero_round_trips() {
        let bytes = encode_h256(&H256::ZERO);
        let decoded = decode_h256(&bytes).unwrap();
        assert_eq!(decoded, H256::ZERO);
    }

    #[test]
    fn h256_arbitrary_round_trips() {
        let mut raw = [0u8; 32];
        for (i, b) in raw.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(13).wrapping_add(7);
        }
        let h = H256::new(raw);
        let encoded = encode_h256(&h);
        assert_eq!(decode_h256(&encoded).unwrap(), h);
    }

    #[test]
    fn h256_encoding_length_is_33_bytes() {
        // 1 RLP length-prefix byte (0x80 + 32 = 0xa0) + 32 payload bytes.
        let encoded = encode_h256(&H256::ZERO);
        assert_eq!(encoded.len(), 33);
        assert_eq!(encoded[0], 0xa0);
    }

    #[test]
    fn h256_decode_rejects_wrong_length() {
        let mut bad = vec![0x9f];
        bad.extend(std::iter::repeat(0u8).take(31));
        assert!(decode_h256(&bad).is_err());
    }

    #[test]
    fn address_round_trips() {
        let a = Address::new([0xAB; 20]);
        let encoded = encode_address(&a);
        assert_eq!(decode_address(&encoded).unwrap(), a);
    }

    #[test]
    fn address_encoding_length_is_21_bytes() {
        let encoded = encode_address(&Address::ZERO);
        assert_eq!(encoded.len(), 21);
        assert_eq!(encoded[0], 0x94);
    }

    #[test]
    fn address_decode_rejects_wrong_length() {
        let mut bad = vec![0x93];
        bad.extend(std::iter::repeat(0u8).take(19));
        assert!(decode_address(&bad).is_err());
    }
}
