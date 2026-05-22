//! Property-based tests for `aii-types`.

use aii_types::{AlgoId, BlsPubKey, BlsSignature, SignedTx, H256, U256};
use proptest::prelude::*;

proptest! {
    /// Any 32-byte array round-trips through H256::new.
    #[test]
    fn h256_bytes_round_trip(bytes in proptest::array::uniform32(any::<u8>())) {
        let h = H256::new(bytes);
        prop_assert_eq!(*h.as_bytes(), bytes);
    }

    /// 48-byte arrays round-trip through BlsPubKey::new.
    #[test]
    fn bls_pubkey_round_trip(seed in proptest::array::uniform32(any::<u8>())) {
        let mut bytes = [0u8; 48];
        for i in 0..48 { bytes[i] = seed[i % 32]; }
        let k = BlsPubKey::new(bytes);
        prop_assert_eq!(*k.as_bytes(), bytes);
    }

    /// 96-byte arrays round-trip through BlsSignature::new.
    #[test]
    fn bls_signature_round_trip(seed in proptest::array::uniform32(any::<u8>())) {
        let mut bytes = [0u8; 96];
        for i in 0..96 { bytes[i] = seed[i % 32]; }
        let s = BlsSignature::new(bytes);
        prop_assert_eq!(*s.as_bytes(), bytes);
    }

    /// Every assigned AlgoId byte decodes; unassigned bytes fail.
    #[test]
    fn algo_id_assigned_vs_unassigned(byte in any::<u8>()) {
        let known = [0x01, 0x02, 0x03, 0x10, 0x11, 0x12, 0x20];
        let result = AlgoId::from_byte(byte);
        if known.contains(&byte) {
            prop_assert!(result.is_ok());
        } else {
            prop_assert!(result.is_err());
        }
    }

    /// SignedTx::wire_size is exact: 1 + pubkey + signature + payload.
    #[test]
    fn signed_tx_wire_size_invariant(
        pubkey in proptest::collection::vec(any::<u8>(), 0..200),
        signature in proptest::collection::vec(any::<u8>(), 0..200),
        payload in proptest::collection::vec(any::<u8>(), 0..1024),
    ) {
        let pubkey_len = pubkey.len();
        let signature_len = signature.len();
        let payload_len = payload.len();
        let tx = SignedTx::new(AlgoId::Secp256k1, pubkey, signature, payload);
        prop_assert_eq!(tx.wire_size(), 1 + pubkey_len + signature_len + payload_len);
    }

    /// U256 addition is associative.
    #[test]
    fn u256_addition_associative(a in any::<u64>(), b in any::<u64>(), c in any::<u64>()) {
        let (a, b, c) = (U256::from(a), U256::from(b), U256::from(c));
        prop_assert_eq!((a + b) + c, a + (b + c));
    }
}
