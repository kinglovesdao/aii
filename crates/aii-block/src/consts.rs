//! Well-known constants used in block headers (empty Merkle/list roots).

use aii_types::H256;

/// Keccak-256 of `rlp(empty list)` — the canonical empty `ommers_hash`.
pub const EMPTY_LIST_HASH: H256 = H256::new([
    0x1d, 0xcc, 0x4d, 0xe8, 0xde, 0xc7, 0x5d, 0x7a, 0xab, 0x85, 0xb5, 0x67, 0xb6, 0xcc, 0xd4, 0x1a,
    0xd3, 0x12, 0x45, 0x1b, 0x94, 0x8a, 0x74, 0x13, 0xf0, 0xa1, 0x42, 0xfd, 0x40, 0xd4, 0x93, 0x47,
]);

/// Keccak-256 of `rlp(empty string)` — the canonical empty trie root.
pub const EMPTY_TRIE_HASH: H256 = H256::new([
    0x56, 0xe8, 0x1f, 0x17, 0x1b, 0xcc, 0x55, 0xa6, 0xff, 0x83, 0x45, 0xe6, 0x92, 0xc0, 0xf8, 0x6e,
    0x5b, 0x48, 0xe0, 0x1b, 0x99, 0x6c, 0xad, 0xc0, 0x01, 0x62, 0x2f, 0xb5, 0xe3, 0x63, 0xb4, 0x21,
]);

#[cfg(test)]
mod tests {
    use super::*;
    use aii_crypto::keccak::keccak256;
    use alloy_rlp::Encodable;

    #[test]
    fn empty_list_hash_matches_keccak() {
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        alloy_rlp::Header { list: true, payload_length: 0 }.encode(&mut buf);
        let h = keccak256(&buf);
        assert_eq!(h, EMPTY_LIST_HASH);
    }

    #[test]
    fn empty_trie_hash_matches_keccak() {
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        b"".as_slice().encode(&mut buf);
        let h = keccak256(&buf);
        assert_eq!(h, EMPTY_TRIE_HASH);
    }
}
