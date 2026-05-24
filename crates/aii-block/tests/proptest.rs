//! Property tests for aii-block.

use aii_block::{
    consts::{EMPTY_LIST_HASH, EMPTY_TRIE_HASH},
    Bloom, Hashable, Header, Tx, TxLegacy,
};
use aii_types::{Address, AlgoId, H256, U256};
use alloy_rlp::{Decodable, Encodable};
use proptest::prelude::*;

fn h256_strategy() -> impl Strategy<Value = H256> {
    any::<[u8; 32]>().prop_map(H256::new)
}

fn address_strategy() -> impl Strategy<Value = Address> {
    any::<[u8; 20]>().prop_map(Address::new)
}

prop_compose! {
    fn header_strategy()
        (parent_hash in h256_strategy(),
         ben in address_strategy(),
         root in h256_strategy(),
         num in any::<u64>(),
         gl in any::<u64>(),
         gu in any::<u64>(),
         ts in any::<u64>(),
         extra in prop::collection::vec(any::<u8>(), 0..=32),
         mix in h256_strategy(),
         base_fee in any::<u64>())
        -> Header
    {
        Header {
            parent_hash,
            ommers_hash: EMPTY_LIST_HASH,
            beneficiary: ben,
            state_root: root,
            transactions_root: EMPTY_TRIE_HASH,
            receipts_root: EMPTY_TRIE_HASH,
            logs_bloom: Bloom::ZERO,
            difficulty: U256::from(0u64),
            number: num,
            gas_limit: gl,
            gas_used: gu,
            timestamp: ts,
            extra_data: extra,
            mix_hash: mix,
            nonce: [0u8; 8],
            base_fee_per_gas: U256::from(base_fee),
            withdrawals_root: EMPTY_TRIE_HASH,
            blob_gas_used: None,
            excess_blob_gas: None,
            parent_beacon_block_root: None,
        }
    }
}

prop_compose! {
    fn legacy_strategy()
        (nonce in any::<u64>(),
         gp in any::<u64>(),
         gl in any::<u64>(),
         val in any::<u64>(),
         to in prop::option::of(address_strategy()),
         data in prop::collection::vec(any::<u8>(), 0..=64),
         r in h256_strategy(),
         s in h256_strategy())
        -> TxLegacy
    {
        TxLegacy {
            nonce,
            gas_price: U256::from(gp),
            gas_limit: gl,
            to,
            value: U256::from(val),
            data,
            v: 27,
            r,
            s,
            algo_id: AlgoId::Secp256k1,
        }
    }
}

proptest! {
    #[test]
    fn header_rlp_round_trip(h in header_strategy()) {
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        h.encode(&mut buf);
        let mut s: &[u8] = &buf;
        let decoded = Header::decode(&mut s)
            .map_err(|e| TestCaseError::fail(format!("{e:?}")))?;
        prop_assert_eq!(decoded, h);
    }

    #[test]
    fn tx_legacy_rlp_round_trip(t in legacy_strategy()) {
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        t.encode(&mut buf);
        let mut s: &[u8] = &buf;
        let decoded = TxLegacy::decode(&mut s)
            .map_err(|e| TestCaseError::fail(format!("{e:?}")))?;
        prop_assert_eq!(decoded, t);
    }

    #[test]
    fn envelope_round_trip_legacy(t in legacy_strategy()) {
        let original = Tx::Legacy(t);
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        original.encode_2718(&mut buf);
        let mut s: &[u8] = &buf;
        let decoded = Tx::decode_2718(&mut s)
            .map_err(|e| TestCaseError::fail(format!("{e:?}")))?;
        prop_assert_eq!(decoded, original);
    }

    #[test]
    fn header_hash_deterministic(h in header_strategy()) {
        prop_assert_eq!(h.hash(), h.hash());
    }

    #[test]
    fn header_hash_distinguishes_number(h in header_strategy()) {
        let mut h2 = h.clone();
        h2.number = h.number.wrapping_add(1);
        prop_assert_ne!(h.hash(), h2.hash());
    }
}
