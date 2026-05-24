//! Property-based round-trip tests for `aii-crypto` primitives.
//!
//! Strategy: pick a deterministic test key per primitive (BLS / Schnorrkel
//! key generation is expensive, so we share one keypair across many inputs)
//! and assert that `verify(sign(msg)) == ok` and `recover(sign(msg)) == pk`
//! hold across arbitrary byte payloads.

use aii_crypto::{
    bls,
    keccak::keccak256,
    secp::{self, SecretKey as SecpSecretKey},
    vrf,
};
use proptest::prelude::*;

fn secp_sk() -> SecpSecretKey {
    let mut b = [0u8; 32];
    b[31] = 7;
    SecpSecretKey::from_bytes(&b).unwrap()
}

fn bls_sk() -> bls::SecretKey {
    bls::SecretKey::from_ikm(&[42u8; 32], b"AII-PROPTEST").unwrap()
}

fn vrf_sk() -> vrf::SecretKey {
    // Schnorrkel rejects arbitrary 64-byte vectors as keys, so we draw
    // a fresh canonical key from OS entropy each property body. The VRF
    // property under test (`prove`-then-`verify` agreement) holds for any
    // valid key, so determinism across runs is unnecessary.
    vrf::SecretKey::generate()
}

proptest! {
    #[test]
    fn keccak256_is_deterministic(msg in proptest::collection::vec(any::<u8>(), 0..512)) {
        prop_assert_eq!(keccak256(&msg), keccak256(&msg));
    }

    #[test]
    fn secp_sign_verify_recover_round_trips(
        msg in proptest::collection::vec(any::<u8>(), 0..256)
    ) {
        let sk = secp_sk();
        let pk = sk.public_key();
        let h = keccak256(&msg);
        let sig = secp::sign(&sk, &h).unwrap();
        prop_assert!(secp::verify(&sig, &h, &pk).is_ok());
        prop_assert_eq!(secp::recover(&sig, &h).unwrap(), pk);
    }

    #[test]
    fn secp_signature_wire_round_trips(
        msg in proptest::collection::vec(any::<u8>(), 0..256)
    ) {
        let sk = secp_sk();
        let h = keccak256(&msg);
        let sig = secp::sign(&sk, &h).unwrap();
        let bytes = sig.to_bytes();
        prop_assert_eq!(secp::Signature::from_bytes(&bytes).unwrap().to_bytes(), bytes);
    }

    #[test]
    fn bls_sign_verify_round_trips(msg in proptest::collection::vec(any::<u8>(), 0..256)) {
        let sk = bls_sk();
        let pk = sk.public_key();
        let sig = sk.sign(&msg);
        prop_assert!(sig.verify(&msg, &pk).is_ok());
    }

    #[test]
    fn vrf_prove_verify_round_trips(input in proptest::collection::vec(any::<u8>(), 0..256)) {
        let sk = vrf_sk();
        let pk = sk.public_key();
        let (proof, randomness) = vrf::prove(&sk, &input);
        let recovered = vrf::verify(&pk, &input, &proof).unwrap();
        prop_assert_eq!(recovered, randomness);
    }
}
