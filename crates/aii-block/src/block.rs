//! Top-level `Block` = `Header` + `BlockBody`.

use crate::{body::BlockBody, header::Header, Hashable};
use aii_types::H256;
use alloy_rlp::{Decodable, Encodable};

/// A full block — header and body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// Block header.
    pub header: Header,
    /// Block body — transactions, ommers, withdrawals.
    pub body: BlockBody,
}

impl Block {
    /// Convenience constructor.
    #[must_use]
    pub const fn new(header: Header, body: BlockBody) -> Self {
        Self { header, body }
    }
}

impl Encodable for Block {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        let header_len = self.header.length();
        let body_len = self.body.length();
        let payload_length = header_len + body_len;
        alloy_rlp::Header { list: true, payload_length }.encode(out);
        self.header.encode(out);
        self.body.encode(out);
    }
    fn length(&self) -> usize {
        let payload = self.header.length() + self.body.length();
        payload + alloy_rlp::length_of_length(payload)
    }
}

impl Decodable for Block {
    fn decode(buf: &mut &[u8]) -> Result<Self, alloy_rlp::Error> {
        let h = alloy_rlp::Header::decode(buf)?;
        if !h.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }
        let header = Header::decode(buf)?;
        let body = BlockBody::decode(buf)?;
        Ok(Self { header, body })
    }
}

impl Hashable for Block {
    fn hash(&self) -> H256 {
        self.header.hash()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        bloom::Bloom,
        consts::{EMPTY_LIST_HASH, EMPTY_TRIE_HASH},
    };
    use aii_types::{Address, U256};

    fn empty_block() -> Block {
        Block {
            header: Header {
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
            },
            body: BlockBody::default(),
        }
    }

    #[test]
    fn rlp_round_trip_empty_block() {
        let original = empty_block();
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        original.encode(&mut buf);
        let mut s: &[u8] = &buf;
        let decoded = Block::decode(&mut s).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn block_hash_equals_header_hash() {
        let b = empty_block();
        assert_eq!(b.hash(), b.header.hash());
    }
}
