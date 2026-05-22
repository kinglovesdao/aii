//! Property-based round-trip tests for every format in `aii-codec`.
//!
//! Strategy: generate arbitrary values of each AII type, then verify that
//! `decode(encode(value)) == value` for each codec.

use aii_codec::{hex, json, rlp, ssz};
use aii_types::{Address, AlgoId, BlsPubKey, BlsSignature, H256, SignedTx, U256};
use proptest::prelude::*;

fn algo_id_strategy() -> impl Strategy<Value = AlgoId> {
    prop_oneof![
        Just(AlgoId::Secp256k1),
        Just(AlgoId::Ed25519),
        Just(AlgoId::Bls12_381),
        Just(AlgoId::MlDsa65),
        Just(AlgoId::SlhDsa128s),
        Just(AlgoId::Falcon512),
        Just(AlgoId::HybridSecpMlDsa),
    ]
}

fn signed_tx_strategy() -> impl Strategy<Value = SignedTx> {
    (
        algo_id_strategy(),
        proptest::collection::vec(any::<u8>(), 0..200),
        proptest::collection::vec(any::<u8>(), 0..200),
        proptest::collection::vec(any::<u8>(), 0..1024),
    )
        .prop_map(|(algo_id, pubkey, signature, payload)| {
            SignedTx::new(algo_id, pubkey, signature, payload)
        })
}

proptest! {
    #[test]
    fn hex_bytes_round_trip(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
        let s = hex::encode_bytes(&bytes);
        prop_assert_eq!(hex::decode_bytes(&s).unwrap(), bytes);
    }

    #[test]
    fn hex_quantity_round_trip(n in any::<u128>()) {
        let q = U256::from(n);
        let s = hex::encode_quantity(q);
        prop_assert_eq!(hex::decode_quantity(&s).unwrap(), q);
    }

    #[test]
    fn rlp_h256_round_trip(bytes in proptest::array::uniform32(any::<u8>())) {
        let h = H256::new(bytes);
        let encoded = rlp::encode_h256(&h);
        prop_assert_eq!(rlp::decode_h256(&encoded).unwrap(), h);
    }

    #[test]
    fn rlp_address_round_trip(seed in proptest::array::uniform32(any::<u8>())) {
        let mut a_bytes = [0u8; 20];
        for i in 0..20 { a_bytes[i] = seed[i]; }
        let a = Address::new(a_bytes);
        let encoded = rlp::encode_address(&a);
        prop_assert_eq!(rlp::decode_address(&encoded).unwrap(), a);
    }

    #[test]
    fn rlp_algo_id_round_trip(a in algo_id_strategy()) {
        let encoded = rlp::encode_algo_id(a);
        prop_assert_eq!(rlp::decode_algo_id(&encoded).unwrap(), a);
    }

    #[test]
    fn rlp_signed_tx_round_trip(tx in signed_tx_strategy()) {
        let encoded = rlp::encode_signed_tx(&tx);
        prop_assert_eq!(rlp::decode_signed_tx(&encoded).unwrap(), tx);
    }

    #[test]
    fn ssz_h256_round_trip(bytes in proptest::array::uniform32(any::<u8>())) {
        let h = H256::new(bytes);
        let encoded = ssz::encode_h256(&h);
        prop_assert_eq!(ssz::decode_h256(&encoded).unwrap(), h);
    }

    #[test]
    fn ssz_bls_pubkey_round_trip(seed in proptest::array::uniform32(any::<u8>())) {
        let mut b = [0u8; 48];
        for i in 0..48 { b[i] = seed[i % 32]; }
        let k = BlsPubKey::new(b);
        let encoded = ssz::encode_bls_pubkey(&k);
        prop_assert_eq!(ssz::decode_bls_pubkey(&encoded).unwrap(), k);
    }

    #[test]
    fn ssz_bls_signature_round_trip(seed in proptest::array::uniform32(any::<u8>())) {
        let mut b = [0u8; 96];
        for i in 0..96 { b[i] = seed[i % 32]; }
        let s = BlsSignature::new(b);
        let encoded = ssz::encode_bls_signature(&s);
        prop_assert_eq!(ssz::decode_bls_signature(&encoded).unwrap(), s);
    }

    #[test]
    fn ssz_signed_tx_round_trip(tx in signed_tx_strategy()) {
        let encoded = ssz::encode_signed_tx(&tx);
        prop_assert_eq!(ssz::decode_signed_tx(&encoded).unwrap(), tx);
    }

    #[test]
    fn json_quantity_round_trip(n in any::<u128>()) {
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct W {
            #[serde(with = "json::quantity")]
            n: U256,
        }
        let w = W { n: U256::from(n) };
        let s = serde_json::to_string(&w).unwrap();
        let back: W = serde_json::from_str(&s).unwrap();
        prop_assert_eq!(back, w);
    }

    #[test]
    fn json_h256_round_trip(bytes in proptest::array::uniform32(any::<u8>())) {
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct W {
            #[serde(with = "json::hex_h256")]
            h: H256,
        }
        let w = W { h: H256::new(bytes) };
        let s = serde_json::to_string(&w).unwrap();
        let back: W = serde_json::from_str(&s).unwrap();
        prop_assert_eq!(back, w);
    }
}
