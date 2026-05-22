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

use aii_types::{Address, AlgoId, SignedTx, H256};
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

/// Encode an [`AlgoId`] as RLP. Wire format is a single byte (RLP short
/// string of length 1).
#[must_use]
pub fn encode_algo_id(a: AlgoId) -> alloy_rlp::bytes::BytesMut {
    let mut buf = alloy_rlp::bytes::BytesMut::with_capacity(2);
    [a.as_byte()].as_slice().encode(&mut buf);
    buf
}

/// Decode an RLP-encoded [`AlgoId`].
pub fn decode_algo_id(mut bytes: &[u8]) -> Result<AlgoId, alloy_rlp::Error> {
    let v = <alloy_rlp::bytes::Bytes as Decodable>::decode(&mut bytes)?;
    if v.len() != 1 {
        return Err(alloy_rlp::Error::UnexpectedLength);
    }
    AlgoId::from_byte(v[0]).map_err(|_| alloy_rlp::Error::Custom("unknown AlgoId byte"))
}

/// Encode a [`SignedTx`] as RLP. Wire layout is a 4-element list:
/// `[algo_id (1 byte), pubkey, signature, payload]`.
#[must_use]
pub fn encode_signed_tx(tx: &SignedTx) -> alloy_rlp::bytes::BytesMut {
    use alloy_rlp::Header;

    let inner_len = [tx.algo_id.as_byte()].as_slice().length()
        + tx.pubkey.as_slice().length()
        + tx.signature.as_slice().length()
        + tx.payload.as_slice().length();

    let mut buf = alloy_rlp::bytes::BytesMut::with_capacity(inner_len + 8);
    let header = Header {
        list: true,
        payload_length: inner_len,
    };
    header.encode(&mut buf);
    [tx.algo_id.as_byte()].as_slice().encode(&mut buf);
    tx.pubkey.as_slice().encode(&mut buf);
    tx.signature.as_slice().encode(&mut buf);
    tx.payload.as_slice().encode(&mut buf);
    buf
}

/// Decode an RLP-encoded [`SignedTx`].
pub fn decode_signed_tx(mut bytes: &[u8]) -> Result<SignedTx, alloy_rlp::Error> {
    use alloy_rlp::Header;

    let header = Header::decode(&mut bytes)?;
    if !header.list {
        return Err(alloy_rlp::Error::UnexpectedString);
    }
    if bytes.len() < header.payload_length {
        return Err(alloy_rlp::Error::InputTooShort);
    }
    let (inner_slice, _rest) = bytes.split_at(header.payload_length);
    let mut inner = inner_slice;

    let algo_bytes = <alloy_rlp::bytes::Bytes as Decodable>::decode(&mut inner)?;
    if algo_bytes.len() != 1 {
        return Err(alloy_rlp::Error::UnexpectedLength);
    }
    let algo_id = AlgoId::from_byte(algo_bytes[0])
        .map_err(|_| alloy_rlp::Error::Custom("unknown AlgoId byte"))?;

    let pubkey = <alloy_rlp::bytes::Bytes as Decodable>::decode(&mut inner)?.to_vec();
    let signature = <alloy_rlp::bytes::Bytes as Decodable>::decode(&mut inner)?.to_vec();
    let payload = <alloy_rlp::bytes::Bytes as Decodable>::decode(&mut inner)?.to_vec();

    if !inner.is_empty() {
        return Err(alloy_rlp::Error::Custom("trailing bytes in SignedTx RLP"));
    }

    Ok(SignedTx::new(algo_id, pubkey, signature, payload))
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
        bad.extend(std::iter::repeat_n(0u8, 31));
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
        bad.extend(std::iter::repeat_n(0u8, 19));
        assert!(decode_address(&bad).is_err());
    }

    #[test]
    fn algo_id_secp256k1_round_trips() {
        let encoded = encode_algo_id(AlgoId::Secp256k1);
        assert_eq!(decode_algo_id(&encoded).unwrap(), AlgoId::Secp256k1);
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
            let encoded = encode_algo_id(v);
            assert_eq!(decode_algo_id(&encoded).unwrap(), v);
        }
    }

    #[test]
    fn algo_id_decode_rejects_unknown_byte() {
        let bad = [0x81, 0xFF];
        assert!(decode_algo_id(&bad).is_err());
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
        let decoded = decode_signed_tx(&encoded).unwrap();
        assert_eq!(decoded, tx);
    }

    #[test]
    fn signed_tx_decode_rejects_non_list_header() {
        let bad = [0x84, 0x01, 0x02, 0x03, 0x04];
        assert!(decode_signed_tx(&bad).is_err());
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
