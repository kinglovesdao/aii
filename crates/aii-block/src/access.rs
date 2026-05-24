//! EIP-2930 access list — opt-in pre-warmed storage slots.

use aii_types::{Address, H256};
use alloy_rlp::{RlpDecodable, RlpEncodable};

/// One entry in an access list: an address and a set of slot keys.
#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct AccessListItem {
    /// Storage-warmed address.
    pub address: Address,
    /// Storage keys at `address` that will be touched.
    pub storage_keys: Vec<H256>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_rlp::{Decodable, Encodable};

    #[test]
    fn rlp_round_trip() {
        let item = AccessListItem {
            address: Address::new([0xbb; 20]),
            storage_keys: vec![H256::new([0xcc; 32]), H256::new([0xdd; 32])],
        };
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        item.encode(&mut buf);
        let mut s: &[u8] = &buf;
        let decoded = AccessListItem::decode(&mut s).unwrap();
        assert_eq!(decoded, item);
    }
}
