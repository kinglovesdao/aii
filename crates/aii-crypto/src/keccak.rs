//! Keccak-256 hashing.
//!
//! Returns the **legacy Keccak-256** (pre-NIST sha3) output that Ethereum uses
//! for transaction / block hashing and for `Address` derivation. *Not* the
//! padded SHA3-256 from FIPS-202.

use aii_types::H256;
use tiny_keccak::{Hasher, Keccak};

/// Hash `input` with Keccak-256 and return the 32-byte digest.
#[must_use]
pub fn keccak256(input: &[u8]) -> H256 {
    let mut k = Keccak::v256();
    k.update(input);
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    H256::new(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Known-answer test: empty input → `c5d2460186…`.
    /// This is the standard Keccak-256("") vector and appears verbatim in the
    /// Ethereum yellow paper.
    #[test]
    fn keccak256_empty_matches_eth_kat() {
        let got = keccak256(b"");
        let want = hex::decode("c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470")
            .unwrap();
        assert_eq!(got.as_bytes(), &want.as_slice()[..32]);
    }

    /// KAT: `"abc"` → `4e03657aea45a94fc7d47ba826c8d667c0d1e6e33a64a036ec44f58fa12d6c45`.
    #[test]
    fn keccak256_abc_matches_kat() {
        let got = keccak256(b"abc");
        let want = hex::decode("4e03657aea45a94fc7d47ba826c8d667c0d1e6e33a64a036ec44f58fa12d6c45")
            .unwrap();
        assert_eq!(got.as_bytes(), &want.as_slice()[..32]);
    }

    /// KAT: 1-million 'a' characters — standard NIST/Keccak stress vector.
    #[test]
    fn keccak256_million_a_matches_kat() {
        let input = vec![b'a'; 1_000_000];
        let got = keccak256(&input);
        let want = hex::decode("fadae6b49f129bbb812be8407b7b2894f34aecf6dbd1f9b0f0c7e9853098fc96")
            .unwrap();
        assert_eq!(got.as_bytes(), &want.as_slice()[..32]);
    }

    #[test]
    fn keccak256_is_deterministic() {
        assert_eq!(keccak256(b"aii"), keccak256(b"aii"));
    }

    #[test]
    fn keccak256_distinguishes_inputs() {
        assert_ne!(keccak256(b"aii"), keccak256(b"aIi"));
    }
}
