//! EVM event log: `(address, topics, data)`.

use aii_types::{Address, H256};
use alloy_rlp::{RlpDecodable, RlpEncodable};

/// An EVM log entry — emitted by a contract via the `LOG*` opcodes.
#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct Log {
    /// Emitting contract address.
    pub address: Address,
    /// Indexed topics (Solidity event signatures + indexed params); ≤ 4.
    pub topics: Vec<H256>,
    /// Non-indexed event data.
    pub data: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_rlp::{Decodable, Encodable};

    #[test]
    fn rlp_round_trip_empty() {
        let l = Log {
            address: Address::ZERO,
            topics: vec![],
            data: vec![],
        };
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        l.encode(&mut buf);
        let mut s: &[u8] = &buf;
        let decoded = Log::decode(&mut s).unwrap();
        assert_eq!(decoded, l);
    }

    #[test]
    fn rlp_round_trip_with_topics_and_data() {
        let l = Log {
            address: Address::new([0x11; 20]),
            topics: vec![H256::new([0x22; 32]), H256::new([0x33; 32])],
            data: vec![0xde, 0xad, 0xbe, 0xef],
        };
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        l.encode(&mut buf);
        let mut s: &[u8] = &buf;
        let decoded = Log::decode(&mut s).unwrap();
        assert_eq!(decoded, l);
    }
}
