//! Schnorrkel-based VRF for V-node leader election.
//!
//! Each `PoS` round, every V-node feeds the previous block's randomness seed
//! into the VRF together with its own secret key. The lowest output among
//! the ⅔-stake quorum wins the proposer slot. Verifiers re-run [`verify`]
//! to confirm the proof + output without the secret.
//!
//! Curve: Ristretto-25519 (`schnorrkel`).
//! Transcript: Merlin with domain tag [`TRANSCRIPT_LABEL`] = `"AII-VRF"`.
//!
//! Wire form ([`VrfProof`]) is 96 bytes: 32-byte pre-output + 64-byte proof.
//! The 32-byte randomness consumed by leader-election is derived by
//! [`prove`] / [`verify`] from the pre-output via the auxiliary label
//! [`OUTPUT_LABEL`].

use rand_core::OsRng;
use schnorrkel::{
    vrf::{VRFPreOut, VRFProof, VRF_PROOF_LENGTH},
    Keypair, PublicKey as SrPublicKey, SecretKey as SrSecretKey, SignatureError,
};

use crate::error::CryptoError;

/// Domain-separation label for every AII VRF transcript.
pub const TRANSCRIPT_LABEL: &[u8] = b"AII-VRF";

/// Auxiliary label that mixes the VRF pre-output into the 32-byte randomness.
pub const OUTPUT_LABEL: &[u8] = b"AII-VRF-OUT";

/// VRF randomness length — 32 bytes.
pub const VRF_OUTPUT_LENGTH: usize = 32;

/// VRF pre-output length — 32 bytes (one Ristretto point).
pub const VRF_PREOUT_LENGTH: usize = 32;

/// VRF secret key (64 bytes serialized: 32-byte scalar + 32-byte nonce).
#[derive(Clone)]
pub struct SecretKey(SrSecretKey);

/// VRF public key (32 bytes serialized).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublicKey(SrPublicKey);

/// 96-byte wire-form VRF proof: `pre_output (32) ‖ proof (64)`.
///
/// The 32-byte randomness used for leader election is **derived** from this
/// proof by [`prove`] / [`verify`] — not embedded — to enforce that callers
/// always re-validate before consuming the randomness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VrfProof {
    /// Serialized [`VRFPreOut`] — 32-byte compressed Ristretto point.
    pub pre_output: [u8; VRF_PREOUT_LENGTH],
    /// Serialized [`VRFProof`] — 64-byte Schnorrkel proof.
    pub proof: [u8; VRF_PROOF_LENGTH],
}

impl SecretKey {
    /// Generate from OS entropy.
    #[must_use]
    pub fn generate() -> Self {
        Self(Keypair::generate_with(OsRng).secret.clone())
    }

    /// Decode from 64-byte serialized form.
    ///
    /// # Errors
    /// Returns [`CryptoError::InvalidEncoding`] for malformed inputs.
    pub fn from_bytes(bytes: &[u8; 64]) -> Result<Self, CryptoError> {
        SrSecretKey::from_bytes(bytes)
            .map(Self)
            .map_err(map_sig_err)
    }

    /// 64-byte serialized form.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; 64] {
        self.0.to_bytes()
    }

    /// Public key matching this secret.
    #[must_use]
    pub fn public_key(&self) -> PublicKey {
        PublicKey(self.0.to_public())
    }
}

impl PublicKey {
    /// Decode from 32-byte serialized form.
    ///
    /// # Errors
    /// Returns [`CryptoError::InvalidEncoding`] for off-curve inputs.
    pub fn from_bytes(bytes: &[u8; 32]) -> Result<Self, CryptoError> {
        SrPublicKey::from_bytes(bytes)
            .map(Self)
            .map_err(map_sig_err)
    }

    /// 32-byte serialized form.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_bytes()
    }
}

fn transcript(input: &[u8]) -> merlin::Transcript {
    let mut t = merlin::Transcript::new(TRANSCRIPT_LABEL);
    t.append_message(b"input", input);
    t
}

/// Produce a VRF proof + 32-byte randomness over `input`.
#[must_use]
pub fn prove(sk: &SecretKey, input: &[u8]) -> (VrfProof, [u8; VRF_OUTPUT_LENGTH]) {
    let kp = Keypair::from(sk.0.clone());
    let (io, proof, _batchable) = kp.vrf_sign(transcript(input));
    let pre = io.output.to_bytes();
    let randomness = io.make_bytes::<[u8; VRF_OUTPUT_LENGTH]>(OUTPUT_LABEL);
    (
        VrfProof {
            pre_output: pre,
            proof: proof.to_bytes(),
        },
        randomness,
    )
}

/// Verify a VRF proof and re-derive the 32-byte randomness.
///
/// # Errors
/// Returns [`CryptoError::InvalidEncoding`] if either component is malformed
/// or [`CryptoError::BadSignature`] if `pk` did not produce `proof`.
pub fn verify(
    pk: &PublicKey,
    input: &[u8],
    proof: &VrfProof,
) -> Result<[u8; VRF_OUTPUT_LENGTH], CryptoError> {
    let pre_out = VRFPreOut::from_bytes(&proof.pre_output).map_err(map_sig_err)?;
    let p = VRFProof::from_bytes(&proof.proof).map_err(map_sig_err)?;
    let (io, _) =
        pk.0.vrf_verify(transcript(input), &pre_out, &p)
            .map_err(|_| CryptoError::BadSignature)?;
    Ok(io.make_bytes::<[u8; VRF_OUTPUT_LENGTH]>(OUTPUT_LABEL))
}

const fn map_sig_err(_e: SignatureError) -> CryptoError {
    CryptoError::InvalidEncoding("schnorrkel VRF decoding failed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip_bytes() {
        let sk = SecretKey::generate();
        let pk = sk.public_key();
        assert_eq!(PublicKey::from_bytes(&pk.to_bytes()).unwrap(), pk);
        let _ = SecretKey::from_bytes(&sk.to_bytes()).unwrap();
    }

    #[test]
    fn prove_then_verify_round_trips() {
        let sk = SecretKey::generate();
        let pk = sk.public_key();
        let input = b"AII parent block randomness seed";
        let (proof, expected) = prove(&sk, input);
        let actual = verify(&pk, input, &proof).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn verify_rejects_wrong_input() {
        let sk = SecretKey::generate();
        let pk = sk.public_key();
        let (proof, _) = prove(&sk, b"correct input");
        assert!(matches!(
            verify(&pk, b"tampered input", &proof),
            Err(CryptoError::BadSignature)
        ));
    }

    #[test]
    fn verify_rejects_wrong_pubkey() {
        let sk_a = SecretKey::generate();
        let sk_b = SecretKey::generate();
        let pk_b = sk_b.public_key();
        let (proof, _) = prove(&sk_a, b"x");
        assert!(matches!(
            verify(&pk_b, b"x", &proof),
            Err(CryptoError::BadSignature)
        ));
    }

    #[test]
    fn malformed_proof_bytes_rejected() {
        let pk = SecretKey::generate().public_key();
        let bad = VrfProof {
            pre_output: [0xFFu8; 32],
            proof: [0xFFu8; 64],
        };
        assert!(verify(&pk, b"x", &bad).is_err());
    }
}
