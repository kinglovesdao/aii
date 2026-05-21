//! AlgoId — signature-algorithm identifier (1 byte).
//!
//! Implements spec decision D7 (multi-sig Registry). Every transaction and
//! every V-node stake operation carries an `AlgoId` as the first byte of its
//! signature envelope, letting consumers dispatch verification through the
//! Registry (`aii-registry` crate, planned for a later plan).
//!
//! Day-0 reserved values are intentionally sparse: PQ algorithms have
//! placeholders in the enum so adding their concrete verifier later is a
//! purely additive change in `aii-registry`, never a breaking change to
//! transactions or storage layouts.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Signature-algorithm identifier (`#[repr(u8)]` — wire format is 1 byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum AlgoId {
    /// secp256k1 ECDSA — default; ETH-compatible.
    Secp256k1 = 0x01,
    /// Ed25519 — high-perf alt, also classical.
    Ed25519 = 0x02,
    /// BLS12-381 — V-node signatures and PRE-COMMIT aggregation.
    Bls12_381 = 0x03,
    /// ML-DSA-65 (Dilithium) — NIST PQ standard, lattice-based.
    MlDsa65 = 0x10,
    /// SLH-DSA-128s (SPHINCS+) — NIST PQ standard, hash-based.
    SlhDsa128s = 0x11,
    /// Falcon-512 — alternative PQ signature (smaller, slower).
    Falcon512 = 0x12,
    /// Hybrid `Secp256k1 ∥ MlDsa65` — bridges to PQ migration period.
    HybridSecpMlDsa = 0x20,
}

impl AlgoId {
    /// Wire-format byte.
    pub const fn as_byte(self) -> u8 {
        self as u8
    }

    /// Decode from wire byte. Unknown values are an error.
    pub const fn from_byte(b: u8) -> Result<Self, AlgoIdError> {
        match b {
            0x01 => Ok(Self::Secp256k1),
            0x02 => Ok(Self::Ed25519),
            0x03 => Ok(Self::Bls12_381),
            0x10 => Ok(Self::MlDsa65),
            0x11 => Ok(Self::SlhDsa128s),
            0x12 => Ok(Self::Falcon512),
            0x20 => Ok(Self::HybridSecpMlDsa),
            other => Err(AlgoIdError::Unknown(other)),
        }
    }

    /// `true` iff this scheme is believed quantum-safe.
    pub const fn quantum_safe(self) -> bool {
        matches!(
            self,
            Self::MlDsa65 | Self::SlhDsa128s | Self::Falcon512 | Self::HybridSecpMlDsa
        )
    }
}

/// Error decoding an `AlgoId` byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AlgoIdError {
    /// Byte value is not assigned to any algorithm.
    #[error("unknown AlgoId byte 0x{0:02x}")]
    Unknown(u8),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secp256k1_is_default_byte_01() {
        assert_eq!(AlgoId::Secp256k1.as_byte(), 0x01);
    }

    #[test]
    fn pq_algorithms_have_high_byte_block() {
        assert!(AlgoId::MlDsa65.as_byte() >= 0x10);
        assert!(AlgoId::SlhDsa128s.as_byte() >= 0x10);
        assert!(AlgoId::Falcon512.as_byte() >= 0x10);
    }

    #[test]
    fn quantum_safe_classification() {
        assert!(!AlgoId::Secp256k1.quantum_safe());
        assert!(!AlgoId::Ed25519.quantum_safe());
        assert!(!AlgoId::Bls12_381.quantum_safe());
        assert!(AlgoId::MlDsa65.quantum_safe());
        assert!(AlgoId::SlhDsa128s.quantum_safe());
        assert!(AlgoId::Falcon512.quantum_safe());
        assert!(AlgoId::HybridSecpMlDsa.quantum_safe());
    }

    #[test]
    fn from_byte_round_trips_all_variants() {
        for variant in [
            AlgoId::Secp256k1,
            AlgoId::Ed25519,
            AlgoId::Bls12_381,
            AlgoId::MlDsa65,
            AlgoId::SlhDsa128s,
            AlgoId::Falcon512,
            AlgoId::HybridSecpMlDsa,
        ] {
            assert_eq!(AlgoId::from_byte(variant.as_byte()), Ok(variant));
        }
    }

    #[test]
    fn unknown_byte_returns_error() {
        assert_eq!(AlgoId::from_byte(0xFF), Err(AlgoIdError::Unknown(0xFF)));
        assert_eq!(AlgoId::from_byte(0x00), Err(AlgoIdError::Unknown(0x00)));
    }
}
