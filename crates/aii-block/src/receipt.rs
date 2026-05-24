//! EIP-2718 transaction receipt.

use crate::{bloom::Bloom, log::Log, tx::TxType, Hashable};
use aii_crypto::keccak::keccak256;
use aii_types::H256;
use alloy_rlp::{Decodable, Encodable};

/// A transaction receipt — included in the receipts trie of the block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    /// Transaction-type discriminant (controls envelope byte on the wire).
    pub tx_type: TxType,
    /// `true` if the transaction succeeded.
    pub status: bool,
    /// Cumulative gas consumed by this transaction and all preceding
    /// transactions in the same block.
    pub cumulative_gas_used: u64,
    /// 2048-bit bloom over `logs`.
    pub logs_bloom: Bloom,
    /// Event logs emitted during execution.
    pub logs: Vec<Log>,
}

impl Receipt {
    fn payload_length(&self) -> usize {
        let logs_inner: usize = self.logs.iter().map(Encodable::length).sum();
        let logs = alloy_rlp::length_of_length(logs_inner) + logs_inner;
        self.status.length() + self.cumulative_gas_used.length() + self.logs_bloom.length() + logs
    }

    fn encode_inner(&self, out: &mut dyn alloy_rlp::BufMut) {
        let payload_length = self.payload_length();
        alloy_rlp::Header {
            list: true,
            payload_length,
        }
        .encode(out);
        self.status.encode(out);
        self.cumulative_gas_used.encode(out);
        self.logs_bloom.encode(out);
        let logs_inner: usize = self.logs.iter().map(Encodable::length).sum();
        alloy_rlp::Header {
            list: true,
            payload_length: logs_inner,
        }
        .encode(out);
        for l in &self.logs {
            l.encode(out);
        }
    }

    fn decode_inner(buf: &mut &[u8], tx_type: TxType) -> Result<Self, alloy_rlp::Error> {
        let h = alloy_rlp::Header::decode(buf)?;
        if !h.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }
        let status = bool::decode(buf)?;
        let cumulative_gas_used = u64::decode(buf)?;
        let logs_bloom = Bloom::decode(buf)?;
        let logs_h = alloy_rlp::Header::decode(buf)?;
        if !logs_h.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }
        let mut logs = Vec::new();
        let logs_start = buf.len();
        while logs_start - buf.len() < logs_h.payload_length {
            logs.push(Log::decode(buf)?);
        }
        Ok(Self {
            tx_type,
            status,
            cumulative_gas_used,
            logs_bloom,
            logs,
        })
    }

    /// Encode receipt to the EIP-2718 wire form.
    pub fn encode_2718(&self, out: &mut alloy_rlp::bytes::BytesMut) {
        match self.tx_type {
            TxType::Legacy => self.encode_inner(out),
            TxType::Eip1559 => {
                out.extend_from_slice(&[0x02]);
                self.encode_inner(out);
            }
            TxType::Eip4844 => {
                out.extend_from_slice(&[0x03]);
                self.encode_inner(out);
            }
        }
    }

    /// Decode from EIP-2718 wire form.
    pub fn decode_2718(buf: &mut &[u8]) -> Result<Self, alloy_rlp::Error> {
        if buf.is_empty() {
            return Err(alloy_rlp::Error::InputTooShort);
        }
        let first = buf[0];
        if first >= 0xc0 {
            return Self::decode_inner(buf, TxType::Legacy);
        }
        let type_byte = first;
        *buf = &buf[1..];
        match type_byte {
            0x02 => Self::decode_inner(buf, TxType::Eip1559),
            0x03 => Self::decode_inner(buf, TxType::Eip4844),
            _ => Err(alloy_rlp::Error::Custom("unknown receipt envelope type")),
        }
    }
}

impl Hashable for Receipt {
    fn hash(&self) -> H256 {
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        self.encode_2718(&mut buf);
        keccak256(&buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aii_types::Address;

    fn sample(tx_type: TxType) -> Receipt {
        Receipt {
            tx_type,
            status: true,
            cumulative_gas_used: 21_000,
            logs_bloom: Bloom::ZERO,
            logs: vec![Log {
                address: Address::new([0x11; 20]),
                topics: vec![],
                data: vec![],
            }],
        }
    }

    #[test]
    fn envelope_round_trip_legacy() {
        let original = sample(TxType::Legacy);
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        original.encode_2718(&mut buf);
        let mut s: &[u8] = &buf;
        let decoded = Receipt::decode_2718(&mut s).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn envelope_round_trip_eip1559() {
        let original = sample(TxType::Eip1559);
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        original.encode_2718(&mut buf);
        assert_eq!(buf[0], 0x02);
        let mut s: &[u8] = &buf;
        let decoded = Receipt::decode_2718(&mut s).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn envelope_round_trip_eip4844() {
        let original = sample(TxType::Eip4844);
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        original.encode_2718(&mut buf);
        assert_eq!(buf[0], 0x03);
        let mut s: &[u8] = &buf;
        let decoded = Receipt::decode_2718(&mut s).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn hash_is_deterministic() {
        let r = sample(TxType::Eip1559);
        assert_eq!(r.hash(), r.hash());
    }
}
