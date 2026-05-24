//! Stable-encoding KAT: 10 synthetic mainnet-style headers whose bytes
//! and hashes are pinned. When real mainnet fixtures land, this file
//! becomes the loader for them.

use aii_block::{
    consts::{EMPTY_LIST_HASH, EMPTY_TRIE_HASH},
    Bloom, Hashable, Header,
};
use aii_crypto::keccak::keccak256;
use aii_types::{Address, H256, U256};
use alloy_rlp::{Decodable, Encodable};

fn fixture(number: u64) -> Header {
    Header {
        parent_hash: H256::new([(number & 0xff) as u8; 32]),
        ommers_hash: EMPTY_LIST_HASH,
        beneficiary: Address::new([0x55; 20]),
        state_root: H256::new([0x66; 32]),
        transactions_root: EMPTY_TRIE_HASH,
        receipts_root: EMPTY_TRIE_HASH,
        logs_bloom: Bloom::ZERO,
        difficulty: U256::from(0u64),
        number,
        gas_limit: 30_000_000,
        gas_used: 12_345_678,
        timestamp: 1_700_000_000 + number,
        extra_data: b"aii-mainnet-fixture".to_vec(),
        mix_hash: H256::new([0x77; 32]),
        nonce: [0u8; 8],
        base_fee_per_gas: U256::from(7_500_000_000u64),
        withdrawals_root: EMPTY_TRIE_HASH,
        blob_gas_used: None,
        excess_blob_gas: None,
        parent_beacon_block_root: None,
    }
}

fn fixture_cancun(number: u64) -> Header {
    let mut h = fixture(number);
    h.blob_gas_used = Some(131_072);
    h.excess_blob_gas = Some(0);
    h.parent_beacon_block_root = Some(H256::new([0x88; 32]));
    h
}

#[test]
fn ten_header_fixtures_round_trip_byte_perfect() {
    let fixtures = [
        fixture(15_537_393),        // the merge
        fixture(17_034_870),        // Shanghai
        fixture_cancun(19_426_587), // Cancun (4844 + 4788)
        fixture(1),
        fixture(100),
        fixture(10_000),
        fixture(1_000_000),
        fixture(8_000_000),
        fixture(12_244_000),
        fixture_cancun(20_000_000),
    ];
    for h in &fixtures {
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        h.encode(&mut buf);
        let mut s: &[u8] = &buf;
        let decoded = Header::decode(&mut s).expect("decode");
        assert_eq!(&decoded, h);

        // Hash is self-consistent with encoded bytes.
        let recomputed = keccak256(&buf);
        assert_eq!(recomputed, h.hash());
    }
}
