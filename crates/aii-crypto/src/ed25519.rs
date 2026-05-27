//! Ed25519 signing primitives (v0.0.74).
//!
//! Used by the AII release-signing pipeline so that any node can
//! verify a peer-distributed binary update was authorised by the
//! release manager (the holder of the secret key whose public half
//! is pinned in `aii-cli`). Independent from the BLS validator keys
//! (`bls.rs`) and the VRF leader-election keys (`vrf.rs`) — release
//! signing is an operator-trust signal, not a chain consensus signal.
//!
//! Thin wrapper around `ed25519-dalek` 2.x: stable hex serialization,
//! zero-cost re-exports of the underlying types, no `unsafe`.
//!
//! ## Errors
//!
//! - [`CryptoError::Hex`] for hex decode failures on
//!   [`SecretKey::from_hex`] / [`PublicKey::from_hex`] /
//!   [`Signature::from_hex`].
//! - [`CryptoError::BadLength`] for inputs of the wrong size.
//! - [`CryptoError::Ed25519`] for downstream `ed25519-dalek` errors.

use ed25519_dalek::Signer as _;
use ed25519_dalek::Verifier as _;
use rand_core::{CryptoRng, RngCore};

use crate::error::CryptoError;

/// Length of an Ed25519 secret key (seed).
pub const ED25519_SECRET_LEN: usize = 32;
/// Length of an Ed25519 public key.
pub const ED25519_PUBLIC_LEN: usize = 32;
/// Length of an Ed25519 signature.
pub const ED25519_SIG_LEN: usize = 64;

/// Owned Ed25519 secret key.
///
/// Holds the 32-byte seed; expand to a signing key on demand.
#[derive(Clone)]
pub struct SecretKey(ed25519_dalek::SigningKey);

impl SecretKey {
    /// Generate a fresh keypair from `rng`.
    pub fn generate<R: CryptoRng + RngCore>(rng: &mut R) -> Self {
        let mut bytes = [0u8; ED25519_SECRET_LEN];
        rng.fill_bytes(&mut bytes);
        Self(ed25519_dalek::SigningKey::from_bytes(&bytes))
    }

    /// Load from a 32-byte seed.
    #[must_use]
    pub fn from_bytes(bytes: &[u8; ED25519_SECRET_LEN]) -> Self {
        Self(ed25519_dalek::SigningKey::from_bytes(bytes))
    }

    /// Parse a hex-encoded 32-byte seed (with or without `0x` prefix).
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::Hex`] for malformed hex and
    /// [`CryptoError::BadLength`] when the decoded length differs from
    /// [`ED25519_SECRET_LEN`].
    pub fn from_hex(s: &str) -> Result<Self, CryptoError> {
        let raw =
            hex::decode(s.trim_start_matches("0x")).map_err(|e| CryptoError::Hex(e.to_string()))?;
        let arr: [u8; ED25519_SECRET_LEN] =
            raw.try_into()
                .map_err(|v: Vec<u8>| CryptoError::BadLength {
                    expected: ED25519_SECRET_LEN,
                    got: v.len(),
                })?;
        Ok(Self::from_bytes(&arr))
    }

    /// Hex-encode the seed (no `0x` prefix).
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex::encode(self.0.to_bytes())
    }

    /// Derive the paired [`PublicKey`].
    #[must_use]
    pub fn public(&self) -> PublicKey {
        PublicKey(self.0.verifying_key())
    }

    /// Produce a detached signature over `msg`.
    #[must_use]
    pub fn sign(&self, msg: &[u8]) -> Signature {
        Signature(self.0.sign(msg))
    }
}

/// Ed25519 public key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublicKey(ed25519_dalek::VerifyingKey);

impl PublicKey {
    /// Load from a 32-byte compressed encoding.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::Ed25519`] if the bytes don't encode a
    /// valid Edwards-curve point.
    pub fn from_bytes(bytes: &[u8; ED25519_PUBLIC_LEN]) -> Result<Self, CryptoError> {
        Ok(Self(
            ed25519_dalek::VerifyingKey::from_bytes(bytes).map_err(CryptoError::ed25519)?,
        ))
    }

    /// Parse a hex-encoded public key (with or without `0x` prefix).
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::Hex`], [`CryptoError::BadLength`], or
    /// [`CryptoError::Ed25519`] for the respective failure modes.
    pub fn from_hex(s: &str) -> Result<Self, CryptoError> {
        let raw =
            hex::decode(s.trim_start_matches("0x")).map_err(|e| CryptoError::Hex(e.to_string()))?;
        let arr: [u8; ED25519_PUBLIC_LEN] =
            raw.try_into()
                .map_err(|v: Vec<u8>| CryptoError::BadLength {
                    expected: ED25519_PUBLIC_LEN,
                    got: v.len(),
                })?;
        Self::from_bytes(&arr)
    }

    /// Hex-encode the public key (no `0x` prefix).
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex::encode(self.0.to_bytes())
    }

    /// Raw 32-byte compressed encoding.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; ED25519_PUBLIC_LEN] {
        self.0.as_bytes()
    }

    /// Verify `sig` against `msg` under this public key.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::Ed25519`] when the signature is not
    /// valid (forged, wrong key, wrong message).
    pub fn verify(&self, msg: &[u8], sig: &Signature) -> Result<(), CryptoError> {
        self.0.verify(msg, &sig.0).map_err(CryptoError::ed25519)
    }
}

/// Ed25519 signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Signature(ed25519_dalek::Signature);

impl Signature {
    /// Load from a 64-byte encoding.
    #[must_use]
    pub fn from_bytes(bytes: &[u8; ED25519_SIG_LEN]) -> Self {
        Self(ed25519_dalek::Signature::from_bytes(bytes))
    }

    /// Parse a hex-encoded signature (with or without `0x` prefix).
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::Hex`] or [`CryptoError::BadLength`] for
    /// the respective failure modes.
    pub fn from_hex(s: &str) -> Result<Self, CryptoError> {
        let raw =
            hex::decode(s.trim_start_matches("0x")).map_err(|e| CryptoError::Hex(e.to_string()))?;
        let arr: [u8; ED25519_SIG_LEN] =
            raw.try_into()
                .map_err(|v: Vec<u8>| CryptoError::BadLength {
                    expected: ED25519_SIG_LEN,
                    got: v.len(),
                })?;
        Ok(Self::from_bytes(&arr))
    }

    /// Hex-encode the signature (no `0x` prefix).
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex::encode(self.0.to_bytes())
    }

    /// Raw 64-byte encoding.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; ED25519_SIG_LEN] {
        self.0.to_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_core::OsRng;

    #[test]
    fn sign_verify_round_trip() {
        let mut rng = OsRng;
        let sk = SecretKey::generate(&mut rng);
        let pk = sk.public();
        let msg = b"aii v0.0.74 release manifest test";
        let sig = sk.sign(msg);
        pk.verify(msg, &sig).expect("signature must verify");
    }

    #[test]
    fn tampered_message_fails_verify() {
        let mut rng = OsRng;
        let sk = SecretKey::generate(&mut rng);
        let pk = sk.public();
        let sig = sk.sign(b"original message");
        assert!(pk.verify(b"tampered message", &sig).is_err());
    }

    #[test]
    fn wrong_pubkey_fails_verify() {
        let mut rng = OsRng;
        let sk_a = SecretKey::generate(&mut rng);
        let sk_b = SecretKey::generate(&mut rng);
        let msg = b"signed by A";
        let sig = sk_a.sign(msg);
        assert!(sk_b.public().verify(msg, &sig).is_err());
    }

    #[test]
    fn hex_round_trip_secret_public_signature() {
        let mut rng = OsRng;
        let sk = SecretKey::generate(&mut rng);
        let pk = sk.public();
        let sig = sk.sign(b"hex round trip");

        let sk2 = SecretKey::from_hex(&sk.to_hex()).unwrap();
        let pk2 = PublicKey::from_hex(&pk.to_hex()).unwrap();
        let sig2 = Signature::from_hex(&sig.to_hex()).unwrap();

        assert_eq!(sk.to_hex(), sk2.to_hex());
        assert_eq!(pk2, pk);
        assert_eq!(sig2, sig);
        pk2.verify(b"hex round trip", &sig2).unwrap();
    }

    #[test]
    fn hex_with_0x_prefix_accepted() {
        let mut rng = OsRng;
        let pk = SecretKey::generate(&mut rng).public();
        let with_prefix = format!("0x{}", pk.to_hex());
        let parsed = PublicKey::from_hex(&with_prefix).unwrap();
        assert_eq!(parsed, pk);
    }

    #[test]
    fn bad_length_rejected() {
        let too_short = "0123456789abcdef";
        assert!(SecretKey::from_hex(too_short).is_err());
        assert!(PublicKey::from_hex(too_short).is_err());
        assert!(Signature::from_hex(too_short).is_err());
    }
}
