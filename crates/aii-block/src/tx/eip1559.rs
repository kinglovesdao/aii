//! EIP-1559 (type 0x02) transaction body.

use crate::access::AccessListItem;
use crate::header::{decode_u256, encode_u256, u256_length};
use crate::tx::legacy::{decode_h256_loose, decode_to, encode_to, encoded_to_length};
use aii_types::{Address, AlgoId, H256, U256};
use alloy_rlp::{Decodable, Encodable};

/// EIP-1559 dynamic-fee transaction body (the bytes following the 0x02
/// envelope marker).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxEip1559 {
    /// EIP-155 chain id.
    pub chain_id: u64,
    /// Nonce.
    pub nonce: u64,
    /// `max_priority_fee_per_gas` — the tip to the proposer.
    pub max_priority_fee_per_gas: U256,
    /// `max_fee_per_gas` — the absolute ceiling on `priority + base`.
    pub max_fee_per_gas: U256,
    /// Gas limit.
    pub gas_limit: u64,
    /// Destination — `None` = CREATE.
    pub to: Option<Address>,
    /// Value in Wei.
    pub value: U256,
    /// Calldata.
    pub data: Vec<u8>,
    /// Access list (EIP-2930).
    pub access_list: Vec<AccessListItem>,
    /// `y_parity` — 0 or 1.
    pub v: u8,
    /// `r` of the ECDSA signature.
    pub r: H256,
    /// `s` of the ECDSA signature.
    pub s: H256,
    /// AII PQ algorithm slot (Secp256k1 = wire-default).
    pub algo_id: AlgoId,
}

impl TxEip1559 {
    fn payload_length(&self) -> usize {
        let access_list_inner: usize = self.access_list.iter().map(Encodable::length).sum();
        let access_list = alloy_rlp::length_of_length(access_list_inner) + access_list_inner;
        let mut len = self.chain_id.length()
            + self.nonce.length()
            + u256_length(&self.max_priority_fee_per_gas)
            + u256_length(&self.max_fee_per_gas)
            + self.gas_limit.length()
            + encoded_to_length(&self.to)
            + u256_length(&self.value)
            + self.data.as_slice().length()
            + access_list
            + self.v.length()
            + self.r.length()
            + self.s.length();
        if self.algo_id != AlgoId::Secp256k1 {
            len += self.algo_id.as_byte().length();
        }
        len
    }
}

impl Encodable for TxEip1559 {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        let payload_length = self.payload_length();
        alloy_rlp::Header { list: true, payload_length }.encode(out);
        self.chain_id.encode(out);
        self.nonce.encode(out);
        encode_u256(&self.max_priority_fee_per_gas, out);
        encode_u256(&self.max_fee_per_gas, out);
        self.gas_limit.encode(out);
        encode_to(&self.to, out);
        encode_u256(&self.value, out);
        self.data.as_slice().encode(out);
        let access_list_inner: usize = self.access_list.iter().map(Encodable::length).sum();
        alloy_rlp::Header { list: true, payload_length: access_list_inner }.encode(out);
        for item in &self.access_list {
            item.encode(out);
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

impl Decodable for TxEip1559 {
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
        let to = decode_to(buf)?;
        let value = decode_u256(buf)?;
        let data = <alloy_rlp::bytes::Bytes as Decodable>::decode(buf)?;
        let al_header = alloy_rlp::Header::decode(buf)?;
        if !al_header.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }
        let mut access_list = Vec::new();
        let al_started = buf.len();
        while al_started - buf.len() < al_header.payload_length {
            access_list.push(AccessListItem::decode(buf)?);
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

        Ok(Self { chain_id, nonce, max_priority_fee_per_gas, max_fee_per_gas, gas_limit,
                  to, value, data: data.to_vec(), access_list, v, r, s, algo_id })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> TxEip1559 {
        TxEip1559 {
            chain_id: 99,
            nonce: 7,
            max_priority_fee_per_gas: U256::from(2_000_000_000u64),
            max_fee_per_gas: U256::from(20_000_000_000u64),
            gas_limit: 21_000,
            to: Some(Address::new([0x12; 20])),
            value: U256::from(1_000_000_000_000_000_000u64),
            data: vec![],
            access_list: vec![],
            v: 0,
            r: H256::new([0x33; 32]),
            s: H256::new([0x44; 32]),
            algo_id: AlgoId::Secp256k1,
        }
    }

    #[test]
    fn rlp_round_trip() {
        let original = sample();
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        original.encode(&mut buf);
        let mut s: &[u8] = &buf;
        let decoded = TxEip1559::decode(&mut s).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn rlp_round_trip_with_access_list() {
        let mut original = sample();
        original.access_list = vec![AccessListItem {
            address: Address::new([0xaa; 20]),
            storage_keys: vec![H256::new([0x55; 32])],
        }];
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        original.encode(&mut buf);
        let mut s: &[u8] = &buf;
        let decoded = TxEip1559::decode(&mut s).unwrap();
        assert_eq!(decoded, original);
    }
}
