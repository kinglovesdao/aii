//! secp256k1 ECDSA — Ethereum-compatible sign / verify / recover.
//!
//! Wire format ([`Signature::to_bytes`] / [`Signature::from_bytes`]) is the
//! 65-byte Ethereum layout: `r (32) ‖ s (32) ‖ v (1)`, where `v ∈ {0,1}`
//! is the recovery id. EIP-155 chain-id mixing happens at the
//! transaction-encoding layer, not here.
//!
//! All operations consume **the pre-hashed message** — callers are expected
//! to apply [`crate::keccak256`] (or a domain-specific tag + hash) before
//! calling [`sign`] / [`verify`] / [`recover`].

use aii_types::{Address, H256};
use k256::ecdsa::{
    signature::hazmat::PrehashVerifier, RecoveryId, Signature as EcdsaSignature, SigningKey,
    VerifyingKey,
};

use crate::error::CryptoError;
use crate::keccak::keccak256;

/// secp256k1 32-byte secret key (scalar).
#[derive(Clone)]
pub struct SecretKey(SigningKey);

/// secp256k1 public key (33-byte compressed wire form by default).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublicKey(VerifyingKey);

/// 65-byte Ethereum signature (`r ‖ s ‖ v`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Signature {
    inner: EcdsaSignature,
    recovery_id: RecoveryId,
}

impl SecretKey {
    /// Construct from a 32-byte scalar; rejects all-zero / out-of-range.
    ///
    /// # Errors
    /// Returns [`CryptoError::InvalidEncoding`] if `bytes` is not in the
    /// secp256k1 scalar field.
    pub fn from_bytes(bytes: &[u8; 32]) -> Result<Self, CryptoError> {
        SigningKey::from_bytes(bytes.into())
            .map(Self)
            .map_err(|_| CryptoError::InvalidEncoding("secp256k1 secret key out of range"))
    }

    /// Raw 32-byte scalar.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_bytes().into()
    }

    /// Derive the matching public key.
    #[must_use]
    pub fn public_key(&self) -> PublicKey {
        PublicKey(*self.0.verifying_key())
    }
}

impl PublicKey {
    /// Decode a 33-byte SEC1-compressed public key.
    ///
    /// # Errors
    /// Returns [`CryptoError::InvalidEncoding`] for off-curve or malformed
    /// inputs.
    pub fn from_compressed(bytes: &[u8; 33]) -> Result<Self, CryptoError> {
        VerifyingKey::from_sec1_bytes(bytes)
            .map(Self)
            .map_err(|_| CryptoError::InvalidEncoding("secp256k1 compressed pubkey invalid"))
    }

    /// Decode a 65-byte SEC1-uncompressed public key (`0x04 ‖ X ‖ Y`).
    ///
    /// # Errors
    /// Returns [`CryptoError::InvalidEncoding`] for off-curve or malformed
    /// inputs.
    pub fn from_uncompressed(bytes: &[u8; 65]) -> Result<Self, CryptoError> {
        VerifyingKey::from_sec1_bytes(bytes)
            .map(Self)
            .map_err(|_| CryptoError::InvalidEncoding("secp256k1 uncompressed pubkey invalid"))
    }

    /// 33-byte SEC1-compressed wire form.
    #[must_use]
    pub fn to_compressed(&self) -> [u8; 33] {
        let pt = self.0.to_encoded_point(true);
        let mut out = [0u8; 33];
        out.copy_from_slice(pt.as_bytes());
        out
    }

    /// 65-byte SEC1-uncompressed wire form (`0x04 ‖ X ‖ Y`).
    #[must_use]
    pub fn to_uncompressed(&self) -> [u8; 65] {
        let pt = self.0.to_encoded_point(false);
        let mut out = [0u8; 65];
        out.copy_from_slice(pt.as_bytes());
        out
    }

    /// Derive the Ethereum-style address: last 20 bytes of
    /// `Keccak256(uncompressed_pubkey[1..])`.
    #[must_use]
    pub fn address(&self) -> Address {
        let bytes = self.to_uncompressed();
        Address::from_pubkey_hash(keccak256(&bytes[1..]))
    }
}

impl Signature {
    /// Decode the 65-byte Ethereum layout (`r ‖ s ‖ v`).
    ///
    /// # Errors
    /// Returns [`CryptoError::InvalidEncoding`] if the scalars do not
    /// represent valid signature components or the recovery id ∉ `{0, 1}`.
    pub fn from_bytes(bytes: &[u8; 65]) -> Result<Self, CryptoError> {
        let inner = EcdsaSignature::from_slice(&bytes[..64])
            .map_err(|_| CryptoError::InvalidEncoding("secp256k1 signature scalars invalid"))?;
        let recovery_id = RecoveryId::from_byte(bytes[64])
            .ok_or(CryptoError::InvalidEncoding("secp256k1 recovery id not in {0,1}"))?;
        Ok(Self { inner, recovery_id })
    }

    /// 65-byte Ethereum layout.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; 65] {
        let mut out = [0u8; 65];
        out[..64].copy_from_slice(&self.inner.to_bytes());
        out[64] = self.recovery_id.to_byte();
        out
    }

    /// Recovery byte (0 or 1).
    #[must_use]
    pub const fn v(&self) -> u8 {
        self.recovery_id.to_byte()
    }
}

/// Sign a pre-hashed message with `sk`. Always emits a low-S, EIP-2 canonical
/// signature with a recovery id — matching Ethereum's transaction signing
/// rules.
///
/// # Errors
/// Returns [`CryptoError::InvalidEncoding`] only if the underlying ECDSA
/// engine fails internally (unreachable for well-formed inputs).
pub fn sign(sk: &SecretKey, message_hash: &H256) -> Result<Signature, CryptoError> {
    let (sig, recid) = sk
        .0
        .sign_prehash_recoverable(message_hash.as_bytes())
        .map_err(|_| CryptoError::InvalidEncoding("secp256k1 signing failed"))?;
    Ok(Signature { inner: sig, recovery_id: recid })
}

/// Verify that `sig` is a valid signature over `message_hash` by `pk`.
///
/// # Errors
/// Returns [`CryptoError::BadSignature`] if verification fails.
pub fn verify(sig: &Signature, message_hash: &H256, pk: &PublicKey) -> Result<(), CryptoError> {
    pk.0.verify_prehash(message_hash.as_bytes(), &sig.inner)
        .map_err(|_| CryptoError::BadSignature)
}

/// Recover the signing public key from `(sig, message_hash)`.
///
/// # Errors
/// Returns [`CryptoError::BadSignature`] if recovery fails.
pub fn recover(sig: &Signature, message_hash: &H256) -> Result<PublicKey, CryptoError> {
    VerifyingKey::recover_from_prehash(message_hash.as_bytes(), &sig.inner, sig.recovery_id)
        .map(PublicKey)
        .map_err(|_| CryptoError::BadSignature)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic test key — secret = 1 — lets us assert the canonical
    /// public key and address without external state.
    fn sk_one() -> SecretKey {
        let mut bytes = [0u8; 32];
        bytes[31] = 1;
        SecretKey::from_bytes(&bytes).unwrap()
    }

    #[test]
    fn sign_then_verify_round_trips() {
        let sk = sk_one();
        let pk = sk.public_key();
        let h = keccak256(b"AII genesis block");
        let sig = sign(&sk, &h).unwrap();
        assert!(verify(&sig, &h, &pk).is_ok());
    }

    #[test]
    fn recover_returns_signer_pubkey() {
        let sk = sk_one();
        let pk = sk.public_key();
        let h = keccak256(b"recover me");
        let sig = sign(&sk, &h).unwrap();
        let recovered = recover(&sig, &h).unwrap();
        assert_eq!(recovered, pk);
    }

    #[test]
    fn verify_rejects_wrong_message() {
        let sk = sk_one();
        let pk = sk.public_key();
        let h = keccak256(b"original");
        let sig = sign(&sk, &h).unwrap();
        let other = keccak256(b"tampered");
        assert!(matches!(verify(&sig, &other, &pk), Err(CryptoError::BadSignature)));
    }

    #[test]
    fn signature_wire_round_trips() {
        let sk = sk_one();
        let h = keccak256(b"wire");
        let sig = sign(&sk, &h).unwrap();
        let bytes = sig.to_bytes();
        let decoded = Signature::from_bytes(&bytes).unwrap();
        assert_eq!(sig.to_bytes(), decoded.to_bytes());
    }

    #[test]
    fn pubkey_compressed_round_trips() {
        let pk = sk_one().public_key();
        let bytes = pk.to_compressed();
        assert_eq!(bytes.len(), 33);
        let decoded = PublicKey::from_compressed(&bytes).unwrap();
        assert_eq!(pk.to_compressed(), decoded.to_compressed());
    }

    #[test]
    fn pubkey_uncompressed_round_trips() {
        let pk = sk_one().public_key();
        let bytes = pk.to_uncompressed();
        assert_eq!(bytes[0], 0x04);
        let decoded = PublicKey::from_uncompressed(&bytes).unwrap();
        assert_eq!(pk.to_uncompressed(), decoded.to_uncompressed());
    }

    #[test]
    fn secret_key_rejects_all_zero() {
        let zeros = [0u8; 32];
        assert!(SecretKey::from_bytes(&zeros).is_err());
    }

    #[test]
    fn signature_rejects_bad_recovery_id() {
        let sk = sk_one();
        let h = keccak256(b"x");
        let sig = sign(&sk, &h).unwrap();
        let mut bytes = sig.to_bytes();
        bytes[64] = 99;
        assert!(matches!(
            Signature::from_bytes(&bytes),
            Err(CryptoError::InvalidEncoding(_))
        ));
    }

    /// Public Ethereum knowledge: `address(sk = 1) =
    /// 0x7e5f4552091a69125d5dfcb7b8c2659029395bdf`.
    #[test]
    fn address_for_secret_key_one_matches_known_constant() {
        let pk = sk_one().public_key();
        let addr = pk.address();
        let expected = hex::decode("7e5f4552091a69125d5dfcb7b8c2659029395bdf").unwrap();
        assert_eq!(addr.as_bytes(), &expected.as_slice()[..20]);
    }
}
