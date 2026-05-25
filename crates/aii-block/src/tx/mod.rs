//! EIP-2718 transaction envelope — dispatch on the first wire byte.

pub mod eip1559;
pub mod eip4844;
pub mod legacy;
pub mod signer;

pub use eip1559::TxEip1559;
pub use eip4844::TxEip4844;
pub use legacy::TxLegacy;
pub use signer::RecoveryError;

use crate::Hashable;
use aii_crypto::keccak::keccak256;
use aii_types::H256;
use alloy_rlp::{Decodable, Encodable};

/// Transaction-type discriminant — also the EIP-2718 envelope marker byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TxType {
    /// Legacy (pre-EIP-2718).
    Legacy = 0,
    /// EIP-1559 dynamic fee.
    Eip1559 = 2,
    /// EIP-4844 blob-carrying.
    Eip4844 = 3,
}

/// Top-level transaction — EIP-2718 envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tx {
    /// Legacy variant.
    Legacy(TxLegacy),
    /// EIP-1559 variant.
    Eip1559(TxEip1559),
    /// EIP-4844 variant.
    Eip4844(TxEip4844),
}

impl Tx {
    /// Discriminant byte.
    #[must_use]
    pub const fn ty(&self) -> TxType {
        match self {
            Self::Legacy(_) => TxType::Legacy,
            Self::Eip1559(_) => TxType::Eip1559,
            Self::Eip4844(_) => TxType::Eip4844,
        }
    }

    /// Encode to the EIP-2718 wire form (legacy = raw RLP list; typed =
    /// single envelope byte + RLP of body).
    pub fn encode_2718(&self, out: &mut alloy_rlp::bytes::BytesMut) {
        match self {
            Self::Legacy(tx) => tx.encode(out),
            Self::Eip1559(tx) => {
                out.extend_from_slice(&[0x02]);
                tx.encode(out);
            }
            Self::Eip4844(tx) => {
                out.extend_from_slice(&[0x03]);
                tx.encode(out);
            }
        }
    }

    /// Decode from the EIP-2718 wire form.
    pub fn decode_2718(buf: &mut &[u8]) -> Result<Self, alloy_rlp::Error> {
        if buf.is_empty() {
            return Err(alloy_rlp::Error::InputTooShort);
        }
        let first = buf[0];
        if first >= 0xc0 {
            return Ok(Self::Legacy(TxLegacy::decode(buf)?));
        }
        let type_byte = first;
        *buf = &buf[1..];
        match type_byte {
            0x02 => Ok(Self::Eip1559(TxEip1559::decode(buf)?)),
            0x03 => Ok(Self::Eip4844(TxEip4844::decode(buf)?)),
            _ => Err(alloy_rlp::Error::Custom("unknown EIP-2718 type byte")),
        }
    }
}

impl Encodable for Tx {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        match self {
            Self::Legacy(tx) => tx.encode(out),
            Self::Eip1559(tx) => {
                let mut inner = alloy_rlp::bytes::BytesMut::new();
                inner.extend_from_slice(&[0x02]);
                tx.encode(&mut inner);
                inner.as_ref().encode(out);
            }
            Self::Eip4844(tx) => {
                let mut inner = alloy_rlp::bytes::BytesMut::new();
                inner.extend_from_slice(&[0x03]);
                tx.encode(&mut inner);
                inner.as_ref().encode(out);
            }
        }
    }
    fn length(&self) -> usize {
        match self {
            Self::Legacy(tx) => tx.length(),
            Self::Eip1559(tx) => {
                let inner = 1 + tx.length();
                inner + alloy_rlp::length_of_length(inner)
            }
            Self::Eip4844(tx) => {
                let inner = 1 + tx.length();
                inner + alloy_rlp::length_of_length(inner)
            }
        }
    }
}

impl Decodable for Tx {
    fn decode(buf: &mut &[u8]) -> Result<Self, alloy_rlp::Error> {
        if buf.is_empty() {
            return Err(alloy_rlp::Error::InputTooShort);
        }
        let first = buf[0];
        if first >= 0xc0 {
            return Ok(Self::Legacy(TxLegacy::decode(buf)?));
        }
        let payload = <alloy_rlp::bytes::Bytes as Decodable>::decode(buf)?;
        if payload.is_empty() {
            return Err(alloy_rlp::Error::InputTooShort);
        }
        let mut inner: &[u8] = &payload;
        Self::decode_2718(&mut inner)
    }
}

impl Hashable for Tx {
    fn hash(&self) -> H256 {
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        self.encode_2718(&mut buf);
        keccak256(&buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aii_types::{Address, AlgoId, H256, U256};

    fn legacy() -> Tx {
        Tx::Legacy(TxLegacy {
            nonce: 7,
            gas_price: U256::from(20_000_000_000u64),
            gas_limit: 21_000,
            to: Some(Address::new([0x12; 20])),
            value: U256::from(0u64),
            data: vec![],
            v: 27,
            r: H256::new([0x33; 32]),
            s: H256::new([0x44; 32]),
            algo_id: AlgoId::Secp256k1,
        })
    }

    fn eip1559() -> Tx {
        Tx::Eip1559(TxEip1559 {
            chain_id: 99,
            nonce: 0,
            max_priority_fee_per_gas: U256::from(1u64),
            max_fee_per_gas: U256::from(2u64),
            gas_limit: 21_000,
            to: Some(Address::new([0x12; 20])),
            value: U256::from(0u64),
            data: vec![],
            access_list: vec![],
            v: 0,
            r: H256::new([0x33; 32]),
            s: H256::new([0x44; 32]),
            algo_id: AlgoId::Secp256k1,
        })
    }

    #[test]
    fn envelope_round_trip_legacy() {
        let original = legacy();
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        original.encode_2718(&mut buf);
        let mut s: &[u8] = &buf;
        let decoded = Tx::decode_2718(&mut s).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn envelope_round_trip_eip1559() {
        let original = eip1559();
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        original.encode_2718(&mut buf);
        assert_eq!(buf[0], 0x02);
        let mut s: &[u8] = &buf;
        let decoded = Tx::decode_2718(&mut s).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn nested_round_trip_eip1559() {
        let original = eip1559();
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        original.encode(&mut buf);
        let mut s: &[u8] = &buf;
        let decoded = Tx::decode(&mut s).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn hash_is_deterministic() {
        let tx = eip1559();
        assert_eq!(tx.hash(), tx.hash());
    }

    #[test]
    fn hash_differs_per_variant() {
        assert_ne!(legacy().hash(), eip1559().hash());
    }
}
