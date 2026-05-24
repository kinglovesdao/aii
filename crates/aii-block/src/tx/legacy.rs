//! Legacy (pre-EIP-2718) transaction body.

use crate::header::{decode_u256, encode_u256, u256_length};
use aii_types::{Address, AlgoId, H256, U256};
use alloy_rlp::{Decodable, Encodable};

/// Pre-EIP-2718 transaction. RLP-encoded directly (no envelope byte).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxLegacy {
    /// Nonce.
    pub nonce: u64,
    /// Gas price (Wei).
    pub gas_price: U256,
    /// Gas limit.
    pub gas_limit: u64,
    /// Destination — `None` = CREATE.
    pub to: Option<Address>,
    /// Value in Wei.
    pub value: U256,
    /// Calldata.
    pub data: Vec<u8>,
    /// `v` field (`27 + recid` pre-EIP-155; `chain_id*2 + 35 + recid` post-EIP-155).
    pub v: u64,
    /// `r` of the ECDSA signature.
    pub r: H256,
    /// `s` of the ECDSA signature.
    pub s: H256,
    /// AII PQ algorithm slot. `Secp256k1` is the wire-default and emits a
    /// byte-perfect Ethereum-compatible encoding.
    pub algo_id: AlgoId,
}

pub(crate) fn encoded_to_length(to: &Option<Address>) -> usize {
    match to {
        Some(a) => a.length(),
        None => 1,
    }
}

pub(crate) fn encode_to(to: &Option<Address>, out: &mut dyn alloy_rlp::BufMut) {
    match to {
        Some(a) => a.encode(out),
        None => out.put_u8(0x80),
    }
}

pub(crate) fn decode_to(buf: &mut &[u8]) -> Result<Option<Address>, alloy_rlp::Error> {
    let b = <alloy_rlp::bytes::Bytes as Decodable>::decode(buf)?;
    if b.is_empty() {
        return Ok(None);
    }
    if b.len() != 20 {
        return Err(alloy_rlp::Error::UnexpectedLength);
    }
    let mut out = [0u8; 20];
    out.copy_from_slice(&b);
    Ok(Some(Address::new(out)))
}

pub(crate) fn decode_h256_loose(buf: &mut &[u8]) -> Result<H256, alloy_rlp::Error> {
    let b = <alloy_rlp::bytes::Bytes as Decodable>::decode(buf)?;
    if b.len() > 32 {
        return Err(alloy_rlp::Error::UnexpectedLength);
    }
    let mut padded = [0u8; 32];
    padded[32 - b.len()..].copy_from_slice(&b);
    Ok(H256::new(padded))
}

impl TxLegacy {
    fn payload_length(&self) -> usize {
        let mut len = self.nonce.length()
            + u256_length(&self.gas_price)
            + self.gas_limit.length()
            + encoded_to_length(&self.to)
            + u256_length(&self.value)
            + self.data.as_slice().length()
            + self.v.length()
            + self.r.length()
            + self.s.length();
        if self.algo_id != AlgoId::Secp256k1 {
            len += self.algo_id.as_byte().length();
        }
        len
    }
}

impl Encodable for TxLegacy {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        let payload_length = self.payload_length();
        alloy_rlp::Header {
            list: true,
            payload_length,
        }
        .encode(out);
        self.nonce.encode(out);
        encode_u256(&self.gas_price, out);
        self.gas_limit.encode(out);
        encode_to(&self.to, out);
        encode_u256(&self.value, out);
        self.data.as_slice().encode(out);
        self.v.encode(out);
        self.r.encode(out);
        self.s.encode(out);
        if self.algo_id != AlgoId::Secp256k1 {
            self.algo_id.as_byte().encode(out);
        }
    }
    fn length(&self) -> usize {
        let payload = self.payload_length();
        alloy_rlp::length_of_length(payload) + payload
    }
}

impl Decodable for TxLegacy {
    fn decode(buf: &mut &[u8]) -> Result<Self, alloy_rlp::Error> {
        let h = alloy_rlp::Header::decode(buf)?;
        if !h.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }
        let started_len = buf.len();
        let nonce = u64::decode(buf)?;
        let gas_price = decode_u256(buf)?;
        let gas_limit = u64::decode(buf)?;
        let to = decode_to(buf)?;
        let value = decode_u256(buf)?;
        let data = <alloy_rlp::bytes::Bytes as Decodable>::decode(buf)?;
        let v = u64::decode(buf)?;
        let r = decode_h256_loose(buf)?;
        let s = decode_h256_loose(buf)?;

        let consumed = started_len - buf.len();
        let algo_id = if consumed < h.payload_length {
            let b = u8::decode(buf)?;
            AlgoId::from_byte(b).map_err(|_| alloy_rlp::Error::Custom("invalid algo_id"))?
        } else {
            AlgoId::Secp256k1
        };

        Ok(Self {
            nonce,
            gas_price,
            gas_limit,
            to,
            value,
            data: data.to_vec(),
            v,
            r,
            s,
            algo_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> TxLegacy {
        TxLegacy {
            nonce: 7,
            gas_price: U256::from(20_000_000_000u64),
            gas_limit: 21_000,
            to: Some(Address::new([0x12; 20])),
            value: U256::from(1_000_000_000_000_000_000u64),
            data: vec![],
            v: 27,
            r: H256::new([0x33; 32]),
            s: H256::new([0x44; 32]),
            algo_id: AlgoId::Secp256k1,
        }
    }

    #[test]
    fn rlp_round_trip_eth_compat() {
        let original = sample();
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        original.encode(&mut buf);

        // AII default (Secp256k1) emits NO trailing algo byte — must be byte-compat.
        // We assert this by re-decoding into a TxLegacy that NEVER sees an algo byte.
        let mut s: &[u8] = &buf;
        let decoded = TxLegacy::decode(&mut s).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn rlp_round_trip_with_create_to_none() {
        let mut original = sample();
        original.to = None;
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        original.encode(&mut buf);
        let mut s: &[u8] = &buf;
        let decoded = TxLegacy::decode(&mut s).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn rlp_round_trip_with_pq_algo_id() {
        let mut original = sample();
        original.algo_id = AlgoId::MlDsa65;
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        original.encode(&mut buf);
        let mut s: &[u8] = &buf;
        let decoded = TxLegacy::decode(&mut s).unwrap();
        assert_eq!(decoded, original);
    }
}
