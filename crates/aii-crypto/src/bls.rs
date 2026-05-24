//! BLS12-381 signatures (V-node consensus aggregation).
//!
//! Wire conventions follow the Ethereum 2.0 spec — `min-pk` scheme:
//!
//! - **Public keys** live on G1, 48-byte compressed.
//! - **Signatures** live on G2, 96-byte compressed.
//! - **Hash-to-curve** uses the standardized
//!   `BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_POP_` DST.
//!
//! `aii-vnode` aggregates signatures from ⅔-stake of the active `VSet` into a
//! single 96-byte object, the core enabler for our single-round PRE-COMMIT
//! BFT path.
//!
//! Concrete cryptography is delegated to `blst` (Supranational). We never
//! write `unsafe` here — `blst` wraps its own FFI surface.

use aii_types::{BlsPubKey, BlsSignature};
use blst::min_pk::{
    AggregatePublicKey, AggregateSignature, PublicKey as BlstPubKey,
    SecretKey as BlstSecretKey, Signature as BlstSignature,
};
use blst::BLST_ERROR;

use crate::error::CryptoError;

/// Domain Separation Tag — matches Ethereum 2.0 mainnet.
pub const DST: &[u8] = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_POP_";

/// BLS12-381 secret key (32 bytes, scalar modulo the group order).
#[derive(Clone)]
pub struct SecretKey(BlstSecretKey);

/// BLS12-381 public key on G1 (48-byte compressed wire form).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicKey(BlstPubKey);

/// BLS12-381 signature on G2 (96-byte compressed wire form).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signature(BlstSignature);

impl SecretKey {
    /// Derive a key from `ikm` via HKDF (`info` is the personalization).
    /// `blst` rejects IKMs shorter than 32 bytes.
    ///
    /// # Errors
    /// Returns [`CryptoError::InvalidEncoding`] for IKM < 32 bytes.
    pub fn from_ikm(ikm: &[u8], info: &[u8]) -> Result<Self, CryptoError> {
        BlstSecretKey::key_gen(ikm, info)
            .map(Self)
            .map_err(|_| CryptoError::InvalidEncoding("BLS IKM too short (need >=32 bytes)"))
    }

    /// Decode from a 32-byte big-endian scalar.
    ///
    /// # Errors
    /// Returns [`CryptoError::InvalidEncoding`] if `bytes` is not a valid
    /// scalar.
    pub fn from_bytes(bytes: &[u8; 32]) -> Result<Self, CryptoError> {
        BlstSecretKey::from_bytes(bytes)
            .map(Self)
            .map_err(|_| CryptoError::InvalidEncoding("BLS secret key invalid"))
    }

    /// Raw 32-byte big-endian scalar.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_bytes()
    }

    /// Derive the matching public key.
    #[must_use]
    pub fn public_key(&self) -> PublicKey {
        PublicKey(self.0.sk_to_pk())
    }

    /// Sign `msg` (raw bytes — hash-to-curve happens inside).
    #[must_use]
    pub fn sign(&self, msg: &[u8]) -> Signature {
        Signature(self.0.sign(msg, DST, &[]))
    }
}

impl PublicKey {
    /// Decode the 48-byte compressed wire form.
    ///
    /// # Errors
    /// Returns [`CryptoError::InvalidEncoding`] for off-curve or
    /// invalid-subgroup inputs.
    pub fn from_compressed(bytes: &[u8; 48]) -> Result<Self, CryptoError> {
        BlstPubKey::from_bytes(bytes)
            .map(Self)
            .map_err(|_| CryptoError::InvalidEncoding("BLS pubkey decompression failed"))
    }

    /// 48-byte compressed wire form.
    #[must_use]
    pub fn to_compressed(&self) -> [u8; 48] {
        self.0.compress()
    }

    /// Project to the wire-level [`BlsPubKey`] new-type used in `aii-types`.
    #[must_use]
    pub fn to_wire(&self) -> BlsPubKey {
        BlsPubKey::new(self.to_compressed())
    }

    /// Lift a wire-level [`BlsPubKey`] back into a typed key. Validates it
    /// lies on the curve and in the prime-order subgroup.
    ///
    /// # Errors
    /// Returns [`CryptoError::InvalidEncoding`] for invalid points.
    pub fn from_wire(wire: &BlsPubKey) -> Result<Self, CryptoError> {
        Self::from_compressed(wire.as_bytes())
    }
}

impl Signature {
    /// Decode the 96-byte compressed wire form.
    ///
    /// # Errors
    /// Returns [`CryptoError::InvalidEncoding`] for invalid points.
    pub fn from_compressed(bytes: &[u8; 96]) -> Result<Self, CryptoError> {
        BlstSignature::from_bytes(bytes)
            .map(Self)
            .map_err(|_| CryptoError::InvalidEncoding("BLS signature decompression failed"))
    }

    /// 96-byte compressed wire form.
    #[must_use]
    pub fn to_compressed(&self) -> [u8; 96] {
        self.0.compress()
    }

    /// Project to the wire-level [`BlsSignature`] new-type.
    #[must_use]
    pub fn to_wire(&self) -> BlsSignature {
        BlsSignature::new(self.to_compressed())
    }

    /// Lift a wire-level [`BlsSignature`] back into a typed signature.
    ///
    /// # Errors
    /// Returns [`CryptoError::InvalidEncoding`] for invalid points.
    pub fn from_wire(wire: &BlsSignature) -> Result<Self, CryptoError> {
        Self::from_compressed(wire.as_bytes())
    }

    /// Verify against a single `(pk, msg)` pair.
    ///
    /// # Errors
    /// Returns [`CryptoError::BadSignature`] on verification failure.
    pub fn verify(&self, msg: &[u8], pk: &PublicKey) -> Result<(), CryptoError> {
        match self.0.verify(true, msg, DST, &[], &pk.0, true) {
            BLST_ERROR::BLST_SUCCESS => Ok(()),
            _ => Err(CryptoError::BadSignature),
        }
    }
}

/// Aggregate `signatures.len()` signatures into a single 96-byte signature.
/// Empty input is an error (matches Eth2 spec).
///
/// # Errors
/// Returns [`CryptoError::InvalidEncoding`] on empty input or
/// [`CryptoError::BadSignature`] if `blst` rejects the aggregation.
pub fn aggregate_signatures(signatures: &[Signature]) -> Result<Signature, CryptoError> {
    if signatures.is_empty() {
        return Err(CryptoError::InvalidEncoding(
            "BLS aggregate requires at least one signature",
        ));
    }
    let refs: Vec<&BlstSignature> = signatures.iter().map(|s| &s.0).collect();
    let agg = AggregateSignature::aggregate(&refs, true).map_err(|_| CryptoError::BadSignature)?;
    Ok(Signature(agg.to_signature()))
}

/// Aggregate `pubkeys.len()` public keys into a single 48-byte aggregate.
///
/// # Errors
/// Returns [`CryptoError::InvalidEncoding`] on empty input or
/// [`CryptoError::BadSignature`] if `blst` rejects the aggregation.
pub fn aggregate_pubkeys(pubkeys: &[PublicKey]) -> Result<PublicKey, CryptoError> {
    if pubkeys.is_empty() {
        return Err(CryptoError::InvalidEncoding(
            "BLS aggregate requires at least one pubkey",
        ));
    }
    let refs: Vec<&BlstPubKey> = pubkeys.iter().map(|p| &p.0).collect();
    let agg = AggregatePublicKey::aggregate(&refs, true).map_err(|_| CryptoError::BadSignature)?;
    Ok(PublicKey(agg.to_public_key()))
}

/// Fast-aggregate verify: all signers signed the **same** `msg`. Aggregates
/// `pubkeys` internally before verification. Optimal for PRE-COMMIT votes.
///
/// # Errors
/// Returns [`CryptoError::InvalidEncoding`] on empty input or
/// [`CryptoError::BadSignature`] on verification failure.
pub fn fast_aggregate_verify(
    agg_sig: &Signature,
    msg: &[u8],
    pubkeys: &[PublicKey],
) -> Result<(), CryptoError> {
    if pubkeys.is_empty() {
        return Err(CryptoError::InvalidEncoding(
            "fast_aggregate_verify requires at least one pubkey",
        ));
    }
    let refs: Vec<&BlstPubKey> = pubkeys.iter().map(|p| &p.0).collect();
    match agg_sig.0.fast_aggregate_verify(true, msg, DST, &refs) {
        BLST_ERROR::BLST_SUCCESS => Ok(()),
        _ => Err(CryptoError::BadSignature),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sk_from_seed(seed: u8) -> SecretKey {
        let ikm = [seed; 32];
        SecretKey::from_ikm(&ikm, b"AII-TEST").unwrap()
    }

    #[test]
    fn sign_then_verify_round_trips() {
        let sk = sk_from_seed(7);
        let pk = sk.public_key();
        let msg = b"V-node PRE-COMMIT";
        let sig = sk.sign(msg);
        assert!(sig.verify(msg, &pk).is_ok());
    }

    #[test]
    fn verify_rejects_wrong_message() {
        let sk = sk_from_seed(7);
        let pk = sk.public_key();
        let sig = sk.sign(b"original");
        assert!(matches!(sig.verify(b"tampered", &pk), Err(CryptoError::BadSignature)));
    }

    #[test]
    fn pubkey_compressed_round_trips() {
        let pk = sk_from_seed(3).public_key();
        let bytes = pk.to_compressed();
        let decoded = PublicKey::from_compressed(&bytes).unwrap();
        assert_eq!(pk.to_compressed(), decoded.to_compressed());
    }

    #[test]
    fn signature_compressed_round_trips() {
        let sk = sk_from_seed(3);
        let sig = sk.sign(b"x");
        let bytes = sig.to_compressed();
        let decoded = Signature::from_compressed(&bytes).unwrap();
        assert_eq!(sig.to_compressed(), decoded.to_compressed());
    }

    #[test]
    fn fast_aggregate_verify_succeeds_for_same_msg() {
        let msg = b"BFT round 42";
        let sks: Vec<SecretKey> = (0..4u8).map(|i| sk_from_seed(i + 1)).collect();
        let pks: Vec<PublicKey> = sks.iter().map(SecretKey::public_key).collect();
        let sigs: Vec<Signature> = sks.iter().map(|sk| sk.sign(msg)).collect();
        let agg = aggregate_signatures(&sigs).unwrap();
        assert!(fast_aggregate_verify(&agg, msg, &pks).is_ok());
    }

    #[test]
    fn fast_aggregate_verify_rejects_tampered_message() {
        let sks: Vec<SecretKey> = (0..3u8).map(|i| sk_from_seed(i + 10)).collect();
        let pks: Vec<PublicKey> = sks.iter().map(SecretKey::public_key).collect();
        let sigs: Vec<Signature> = sks.iter().map(|sk| sk.sign(b"original")).collect();
        let agg = aggregate_signatures(&sigs).unwrap();
        assert!(matches!(
            fast_aggregate_verify(&agg, b"tampered", &pks),
            Err(CryptoError::BadSignature)
        ));
    }

    #[test]
    fn aggregate_empty_is_error() {
        assert!(aggregate_signatures(&[]).is_err());
        assert!(aggregate_pubkeys(&[]).is_err());
    }

    #[test]
    fn to_wire_round_trips_pubkey() {
        let pk = sk_from_seed(9).public_key();
        let wire = pk.to_wire();
        let back = PublicKey::from_wire(&wire).unwrap();
        assert_eq!(pk, back);
    }

    #[test]
    fn to_wire_round_trips_signature() {
        let sk = sk_from_seed(9);
        let sig = sk.sign(b"msg");
        let wire = sig.to_wire();
        let back = Signature::from_wire(&wire).unwrap();
        assert_eq!(sig, back);
    }

    #[test]
    fn secret_key_round_trips_bytes() {
        let sk = sk_from_seed(12);
        let bytes = sk.to_bytes();
        let recovered = SecretKey::from_bytes(&bytes).unwrap();
        assert_eq!(sk.to_bytes(), recovered.to_bytes());
    }
}
