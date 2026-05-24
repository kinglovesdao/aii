//! Block header — 20 fields per AII spec (EIP-1559 + 4895 + 4844 + 4788).

use crate::{bloom::Bloom, Hashable};
use aii_crypto::keccak::keccak256;
use aii_types::{Address, H256, U256};
use alloy_rlp::{Decodable, Encodable, Header as RlpHeader};

/// A block header. Field order matches Ethereum mainnet (EIP-1559 → 4895 →
/// 4844 → 4788), with the four trailing fields optional for forward/back
/// compatibility on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// Hash of the parent block's header.
    pub parent_hash: H256,
    /// `keccak(rlp(ommers))` — usually `EMPTY_LIST_HASH` post-merge.
    pub ommers_hash: H256,
    /// Miner / proposer / coinbase address.
    pub beneficiary: Address,
    /// Root of the world state trie *after* this block.
    pub state_root: H256,
    /// Root of the trie built over `(index → rlp(tx))` for this block.
    pub transactions_root: H256,
    /// Root of the trie built over `(index → rlp(receipt))` for this block.
    pub receipts_root: H256,
    /// 2048-bit bloom over all logs in this block.
    pub logs_bloom: Bloom,
    /// PoW difficulty — `0` post-merge.
    pub difficulty: U256,
    /// Block height.
    pub number: u64,
    /// Gas ceiling.
    pub gas_limit: u64,
    /// Gas used by all transactions.
    pub gas_used: u64,
    /// Unix-seconds timestamp.
    pub timestamp: u64,
    /// Free-form bytes ≤ 32.
    pub extra_data: Vec<u8>,
    /// PoW mix-hash; post-merge stores `prevrandao`.
    pub mix_hash: H256,
    /// PoW nonce — `[0; 8]` post-merge.
    pub nonce: [u8; 8],
    /// EIP-1559 base fee per gas.
    pub base_fee_per_gas: U256,
    /// EIP-4895 withdrawals trie root.
    pub withdrawals_root: H256,
    /// EIP-4844 blob gas used in this block (None pre-Cancun).
    pub blob_gas_used: Option<u64>,
    /// EIP-4844 cumulative excess blob gas (None pre-Cancun).
    pub excess_blob_gas: Option<u64>,
    /// EIP-4788 parent beacon block root (None pre-Cancun).
    pub parent_beacon_block_root: Option<H256>,
}

pub(crate) fn u256_length(v: &U256) -> usize {
    let bytes: [u8; 32] = v.to_be_bytes();
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
    bytes[start..].length()
}

pub(crate) fn encode_u256(v: &U256, out: &mut dyn alloy_rlp::BufMut) {
    let bytes: [u8; 32] = v.to_be_bytes();
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
    bytes[start..].encode(out);
}

pub(crate) fn decode_u256(buf: &mut &[u8]) -> Result<U256, alloy_rlp::Error> {
    let v = <alloy_rlp::bytes::Bytes as Decodable>::decode(buf)?;
    if v.len() > 32 {
        return Err(alloy_rlp::Error::UnexpectedLength);
    }
    let mut padded = [0u8; 32];
    padded[32 - v.len()..].copy_from_slice(&v);
    Ok(U256::from_be_bytes(padded))
}

impl Header {
    fn payload_length(&self) -> usize {
        let mut len = self.parent_hash.length()
            + self.ommers_hash.length()
            + self.beneficiary.length()
            + self.state_root.length()
            + self.transactions_root.length()
            + self.receipts_root.length()
            + self.logs_bloom.length()
            + u256_length(&self.difficulty)
            + self.number.length()
            + self.gas_limit.length()
            + self.gas_used.length()
            + self.timestamp.length()
            + self.extra_data.as_slice().length()
            + self.mix_hash.length()
            + self.nonce.as_slice().length()
            + u256_length(&self.base_fee_per_gas)
            + self.withdrawals_root.length();
        if let Some(v) = self.blob_gas_used {
            len += v.length();
        }
        if let Some(v) = self.excess_blob_gas {
            len += v.length();
        }
        if let Some(ref h) = self.parent_beacon_block_root {
            len += h.length();
        }
        len
    }
}

impl Encodable for Header {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        let payload_length = self.payload_length();
        RlpHeader { list: true, payload_length }.encode(out);
        self.parent_hash.encode(out);
        self.ommers_hash.encode(out);
        self.beneficiary.encode(out);
        self.state_root.encode(out);
        self.transactions_root.encode(out);
        self.receipts_root.encode(out);
        self.logs_bloom.encode(out);
        encode_u256(&self.difficulty, out);
        self.number.encode(out);
        self.gas_limit.encode(out);
        self.gas_used.encode(out);
        self.timestamp.encode(out);
        self.extra_data.as_slice().encode(out);
        self.mix_hash.encode(out);
        self.nonce.as_slice().encode(out);
        encode_u256(&self.base_fee_per_gas, out);
        self.withdrawals_root.encode(out);
        if let Some(v) = self.blob_gas_used {
            v.encode(out);
        }
        if let Some(v) = self.excess_blob_gas {
            v.encode(out);
        }
        if let Some(ref h) = self.parent_beacon_block_root {
            h.encode(out);
        }
    }

    fn length(&self) -> usize {
        let payload = self.payload_length();
        alloy_rlp::length_of_length(payload) + payload
    }
}

impl Decodable for Header {
    fn decode(buf: &mut &[u8]) -> Result<Self, alloy_rlp::Error> {
        let h = RlpHeader::decode(buf)?;
        if !h.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }
        let started_len = buf.len();

        let parent_hash = H256::decode(buf)?;
        let ommers_hash = H256::decode(buf)?;
        let beneficiary = Address::decode(buf)?;
        let state_root = H256::decode(buf)?;
        let transactions_root = H256::decode(buf)?;
        let receipts_root = H256::decode(buf)?;
        let logs_bloom = Bloom::decode(buf)?;
        let difficulty = decode_u256(buf)?;
        let number = u64::decode(buf)?;
        let gas_limit = u64::decode(buf)?;
        let gas_used = u64::decode(buf)?;
        let timestamp = u64::decode(buf)?;
        let extra_data = <alloy_rlp::bytes::Bytes as Decodable>::decode(buf)?;
        if extra_data.len() > 32 {
            return Err(alloy_rlp::Error::Custom("extra_data > 32"));
        }
        let mix_hash = H256::decode(buf)?;
        let nonce_bytes = <alloy_rlp::bytes::Bytes as Decodable>::decode(buf)?;
        if nonce_bytes.len() != 8 {
            return Err(alloy_rlp::Error::UnexpectedLength);
        }
        let mut nonce = [0u8; 8];
        nonce.copy_from_slice(&nonce_bytes);
        let base_fee_per_gas = decode_u256(buf)?;
        let withdrawals_root = H256::decode(buf)?;

        let mut blob_gas_used = None;
        let mut excess_blob_gas = None;
        let mut parent_beacon_block_root = None;
        if started_len - buf.len() < h.payload_length {
            blob_gas_used = Some(u64::decode(buf)?);
        }
        if started_len - buf.len() < h.payload_length {
            excess_blob_gas = Some(u64::decode(buf)?);
        }
        if started_len - buf.len() < h.payload_length {
            parent_beacon_block_root = Some(H256::decode(buf)?);
        }

        Ok(Self {
            parent_hash, ommers_hash, beneficiary, state_root, transactions_root,
            receipts_root, logs_bloom, difficulty, number, gas_limit, gas_used,
            timestamp, extra_data: extra_data.to_vec(), mix_hash, nonce,
            base_fee_per_gas, withdrawals_root,
            blob_gas_used, excess_blob_gas, parent_beacon_block_root,
        })
    }
}

impl Hashable for Header {
    fn hash(&self) -> H256 {
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        self.encode(&mut buf);
        keccak256(&buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consts::{EMPTY_LIST_HASH, EMPTY_TRIE_HASH};

    fn sample_header() -> Header {
        Header {
            parent_hash: H256::new([0x11; 32]),
            ommers_hash: EMPTY_LIST_HASH,
            beneficiary: Address::new([0x22; 20]),
            state_root: EMPTY_TRIE_HASH,
            transactions_root: EMPTY_TRIE_HASH,
            receipts_root: EMPTY_TRIE_HASH,
            logs_bloom: Bloom::ZERO,
            difficulty: U256::from(0u64),
            number: 1,
            gas_limit: 30_000_000,
            gas_used: 0,
            timestamp: 1_700_000_000,
            extra_data: vec![],
            mix_hash: H256::new([0x33; 32]),
            nonce: [0u8; 8],
            base_fee_per_gas: U256::from(1_000_000_000u64),
            withdrawals_root: EMPTY_TRIE_HASH,
            blob_gas_used: None,
            excess_blob_gas: None,
            parent_beacon_block_root: None,
        }
    }

    #[test]
    fn rlp_round_trip_minimal() {
        let original = sample_header();
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        original.encode(&mut buf);
        let mut s: &[u8] = &buf;
        let decoded = Header::decode(&mut s).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn rlp_round_trip_with_cancun_fields() {
        let mut original = sample_header();
        original.blob_gas_used = Some(0);
        original.excess_blob_gas = Some(0);
        original.parent_beacon_block_root = Some(H256::new([0x44; 32]));
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        original.encode(&mut buf);
        let mut s: &[u8] = &buf;
        let decoded = Header::decode(&mut s).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn hash_is_deterministic() {
        let h = sample_header();
        assert_eq!(h.hash(), h.hash());
    }

    #[test]
    fn hash_changes_on_field_change() {
        let h1 = sample_header();
        let mut h2 = sample_header();
        h2.number = 2;
        assert_ne!(h1.hash(), h2.hash());
    }
}
