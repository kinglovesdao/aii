//! EIP-4844 (type 0x03) transaction body — placeholder, no KZG verification.

use crate::access::AccessListItem;
use crate::header::{decode_u256, encode_u256, u256_length};
use crate::tx::legacy::decode_h256_loose;
use aii_types::{Address, AlgoId, H256, U256};
use alloy_rlp::{Decodable, Encodable};

/// EIP-4844 blob-carrying transaction body (network envelope, no sidecar).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxEip4844 {
    /// EIP-155 chain id.
    pub chain_id: u64,
    /// Nonce.
    pub nonce: u64,
    /// `max_priority_fee_per_gas`.
    pub max_priority_fee_per_gas: U256,
    /// `max_fee_per_gas`.
    pub max_fee_per_gas: U256,
    /// Gas limit.
    pub gas_limit: u64,
    /// Destination — MUST be Some (no contract creation in 4844).
    pub to: Address,
    /// Value in Wei.
    pub value: U256,
    /// Calldata.
    pub data: Vec<u8>,
    /// Access list (EIP-2930).
    pub access_list: Vec<AccessListItem>,
    /// `max_fee_per_blob_gas` (EIP-4844).
    pub max_fee_per_blob_gas: U256,
    /// Versioned KZG commitment hashes — opaque here.
    pub blob_versioned_hashes: Vec<H256>,
    /// `y_parity`.
    pub v: u8,
    /// `r` of the ECDSA signature.
    pub r: H256,
    /// `s` of the ECDSA signature.
    pub s: H256,
    /// AII PQ algorithm slot.
    pub algo_id: AlgoId,
}

impl TxEip4844 {
    fn payload_length(&self) -> usize {
        let al_inner: usize = self.access_list.iter().map(Encodable::length).sum();
        let al = alloy_rlp::length_of_length(al_inner) + al_inner;
        let bvh_inner: usize = self
            .blob_versioned_hashes
            .iter()
            .map(Encodable::length)
            .sum();
        let bvh = alloy_rlp::length_of_length(bvh_inner) + bvh_inner;
        let mut len = self.chain_id.length()
            + self.nonce.length()
            + u256_length(&self.max_priority_fee_per_gas)
            + u256_length(&self.max_fee_per_gas)
            + self.gas_limit.length()
            + self.to.length()
            + u256_length(&self.value)
            + self.data.as_slice().length()
            + al
            + u256_length(&self.max_fee_per_blob_gas)
            + bvh
            + self.v.length()
            + self.r.length()
            + self.s.length();
        if self.algo_id != AlgoId::Secp256k1 {
            len += self.algo_id.as_byte().length();
        }
        len
    }
}

impl Encodable for TxEip4844 {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        let payload_length = self.payload_length();
        alloy_rlp::Header {
            list: true,
            payload_length,
        }
        .encode(out);
        self.chain_id.encode(out);
        self.nonce.encode(out);
        encode_u256(&self.max_priority_fee_per_gas, out);
        encode_u256(&self.max_fee_per_gas, out);
        self.gas_limit.encode(out);
        self.to.encode(out);
        encode_u256(&self.value, out);
        self.data.as_slice().encode(out);
        let al_inner: usize = self.access_list.iter().map(Encodable::length).sum();
        alloy_rlp::Header {
            list: true,
            payload_length: al_inner,
        }
        .encode(out);
        for item in &self.access_list {
            item.encode(out);
        }
        encode_u256(&self.max_fee_per_blob_gas, out);
        let bvh_inner: usize = self
            .blob_versioned_hashes
            .iter()
            .map(Encodable::length)
            .sum();
        alloy_rlp::Header {
            list: true,
            payload_length: bvh_inner,
        }
        .encode(out);
        for h in &self.blob_versioned_hashes {
            h.encode(out);
        }
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

impl Decodable for TxEip4844 {
    fn decode(buf: &mut &[u8]) -> Result<Self, alloy_rlp::Error> {
        let h = alloy_rlp::Header::decode(buf)?;
        if !h.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }
        let started_len = buf.len();
        let chain_id = u64::decode(buf)?;
        let nonce = u64::decode(buf)?;
        let max_priority_fee_per_gas = decode_u256(buf)?;
        let max_fee_per_gas = decode_u256(buf)?;
        let gas_limit = u64::decode(buf)?;
        let to = Address::decode(buf)?;
        let value = decode_u256(buf)?;
        let data = <alloy_rlp::bytes::Bytes as Decodable>::decode(buf)?;
        let al_h = alloy_rlp::Header::decode(buf)?;
        let mut access_list = Vec::new();
        let al_start = buf.len();
        while al_start - buf.len() < al_h.payload_length {
            access_list.push(AccessListItem::decode(buf)?);
        }
        let max_fee_per_blob_gas = decode_u256(buf)?;
        let bvh_h = alloy_rlp::Header::decode(buf)?;
        let mut blob_versioned_hashes = Vec::new();
        let bvh_start = buf.len();
        while bvh_start - buf.len() < bvh_h.payload_length {
            blob_versioned_hashes.push(decode_h256_loose(buf)?);
        }
        let v = u8::decode(buf)?;
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
            chain_id,
            nonce,
            max_priority_fee_per_gas,
            max_fee_per_gas,
            gas_limit,
            to,
            value,
            data: data.to_vec(),
            access_list,
            max_fee_per_blob_gas,
            blob_versioned_hashes,
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

    #[test]
    fn rlp_round_trip() {
        let original = TxEip4844 {
            chain_id: 99,
            nonce: 0,
            max_priority_fee_per_gas: U256::from(1_000_000_000u64),
            max_fee_per_gas: U256::from(10_000_000_000u64),
            gas_limit: 21_000,
            to: Address::new([0x12; 20]),
            value: U256::from(0u64),
            data: vec![],
            access_list: vec![],
            max_fee_per_blob_gas: U256::from(1u64),
            blob_versioned_hashes: vec![H256::new([0x77; 32])],
            v: 1,
            r: H256::new([0x33; 32]),
            s: H256::new([0x44; 32]),
            algo_id: AlgoId::Secp256k1,
        };
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        original.encode(&mut buf);
        let mut s: &[u8] = &buf;
        let decoded = TxEip4844::decode(&mut s).unwrap();
        assert_eq!(decoded, original);
    }
}
