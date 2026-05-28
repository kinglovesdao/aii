//! Signed-release manifest (v0.0.74).
//!
//! The foundational primitive for the auto-update protocol:
//!
//! - The release manager generates an Ed25519 keypair once. The
//!   public half is later pinned into every node's binary (a later
//!   release wires that in). The secret half stays on the manager's
//!   air-gapped signer.
//! - For each release the manager runs `aii release sign --binary
//!   aiid --version X.Y.Z --secret-key SK`, which hashes the binary
//!   (SHA-256), assembles a [`ReleaseManifest`] containing version +
//!   hash + timestamp, signs the canonical payload with the secret
//!   key, and writes `release.json`.
//! - Any node can run `aii release verify --manifest release.json
//!   --binary aiid --pubkey 0x...` to check both halves: the
//!   binary's hash must match the manifest, and the manifest's
//!   signature must verify under the supplied public key.
//!
//! v0.0.74 ships **only** the manifest primitives + CLI. Wire-level
//! gossip of releases, peer binary fetch, and atomic in-place
//! install land in later versions on top of this foundation.
//!
//! ## Signed payload format
//!
//! The bytes fed to Ed25519 are:
//!
//! ```text
//! "aii-release-v1\0" || version_bytes || 0x00 || sha256_bytes(32) || timestamp_unix_be(8)
//! ```
//!
//! The leading domain tag prevents the same key being used to forge
//! a confounder signature on unrelated payloads (e.g. validator
//! votes). The version is variable-length so it's NUL-terminated
//! before the fixed-width hash + timestamp fields.

use std::path::Path;

use crate::ed25519::{PublicKey, SecretKey, Signature};
use crate::CryptoError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Domain-separation tag prepended to the signed payload.
pub const DOMAIN_TAG: &[u8] = b"aii-release-v1\0";

/// Hex of the official AII Network release-signing public key.
///
/// (v0.0.75) The matching secret seed is held off-chain by the
/// release manager — every node ships with this public half
/// compiled in so [`pinned_release_pubkey`] can be used as the
/// default trust anchor for `aii release verify` and for the
/// `aii_announceRelease` JSON-RPC endpoint.
///
/// Rotating this constant is a backwards-incompatible change:
/// nodes still on the old pinned key will reject manifests signed
/// by the new one. A future release will add an on-chain rotation
/// path; for now any rotation requires a workspace-wide bump.
pub const RELEASE_SIGNING_PUBKEY_HEX: &str =
    "f845c1bbf443bbf3e18f1a97599c34d39cac4a03fb22c80110f37491c92c0669";

/// Parse [`RELEASE_SIGNING_PUBKEY_HEX`] into a usable [`PublicKey`].
///
/// # Panics
///
/// Panics if the compiled-in constant is malformed — that's a
/// build-time error, so we want it to surface loudly.
#[must_use]
pub fn pinned_release_pubkey() -> PublicKey {
    PublicKey::from_hex(RELEASE_SIGNING_PUBKEY_HEX).expect("pinned release pubkey must parse")
}

/// On-disk representation of a signed binary release.
///
/// All hex fields are lowercase, no `0x` prefix on read (input may
/// include `0x`, output never does).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseManifest {
    /// Semver-style version string identifying this binary build
    /// (e.g. `"0.0.74"`).
    pub version: String,
    /// Hex-encoded SHA-256 digest of the released binary.
    pub sha256_hex: String,
    /// Unix-seconds timestamp the manifest was signed at. Operators
    /// can refuse manifests older than N days as a freshness check.
    pub timestamp_unix: u64,
    /// Hex-encoded Ed25519 signature over [`canonical_payload`].
    pub ed25519_sig_hex: String,
}

/// Errors produced by the release-manifest module.
#[derive(Debug, Error)]
pub enum ReleaseError {
    /// File I/O failure (binary not found, permission denied, …).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// JSON parse / serialize failure on the manifest file.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    /// Hex decode failure on a manifest field.
    #[error("hex: {0}")]
    Hex(String),

    /// Crypto-layer failure (bad key, bad signature, …).
    #[error("crypto: {0}")]
    Crypto(#[from] CryptoError),

    /// The hash recorded in the manifest does not match the bytes of
    /// the binary the caller supplied.
    #[error("binary hash mismatch: manifest says {manifest}, computed {computed}")]
    HashMismatch {
        /// Hash from `manifest.sha256_hex`.
        manifest: String,
        /// Hash recomputed from the supplied binary.
        computed: String,
    },
}

/// Compute the SHA-256 digest of `path` and return both the raw bytes
/// and the lowercase hex encoding.
///
/// # Errors
///
/// Bubbles up `io::Error` for missing-file / permission failures.
pub fn sha256_file(path: &Path) -> Result<([u8; 32], String), ReleaseError> {
    let bytes = std::fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let digest: [u8; 32] = hasher.finalize().into();
    Ok((digest, hex::encode(digest)))
}

/// Build the canonical payload that gets fed to Ed25519.
///
/// See module docs for the byte layout.
#[must_use]
pub fn canonical_payload(version: &str, sha256: &[u8; 32], timestamp_unix: u64) -> Vec<u8> {
    let mut buf = Vec::with_capacity(DOMAIN_TAG.len() + version.len() + 1 + 32 + 8);
    buf.extend_from_slice(DOMAIN_TAG);
    buf.extend_from_slice(version.as_bytes());
    buf.push(0); // NUL terminator separating variable-length version from fixed fields
    buf.extend_from_slice(sha256);
    buf.extend_from_slice(&timestamp_unix.to_be_bytes());
    buf
}

/// Sign a release: hash the binary, build the canonical payload,
/// produce the signature, and return the assembled
/// [`ReleaseManifest`].
///
/// # Errors
///
/// Returns a [`ReleaseError`] if the binary can't be read.
pub fn sign_release(
    secret: &SecretKey,
    binary_path: &Path,
    version: &str,
    timestamp_unix: u64,
) -> Result<ReleaseManifest, ReleaseError> {
    let (digest, sha256_hex) = sha256_file(binary_path)?;
    let payload = canonical_payload(version, &digest, timestamp_unix);
    let sig = secret.sign(&payload);
    Ok(ReleaseManifest {
        version: version.to_string(),
        sha256_hex,
        timestamp_unix,
        ed25519_sig_hex: sig.to_hex(),
    })
}

/// Verify the Ed25519 signature on a manifest WITHOUT requiring
/// the binary on disk (v0.0.81).
///
/// Useful when a node has received a manifest via gossip /
/// periodic poll but doesn't yet have the binary bytes to
/// re-hash. The full [`verify_release`] is still the right call
/// before trusting the binary itself — this only proves "the
/// pinned holder of `expected_pubkey` claimed this
/// `(version, sha256, timestamp)` tuple."
///
/// # Errors
///
/// - `ReleaseError::Hex` when `manifest.sha256_hex` doesn't
///   decode to exactly 32 bytes.
/// - `ReleaseError::Crypto` on bad signature hex or signature
///   that doesn't verify under `expected_pubkey`.
pub fn verify_manifest_signature(
    expected_pubkey: &PublicKey,
    manifest: &ReleaseManifest,
) -> Result<(), ReleaseError> {
    let manifest_hex = manifest.sha256_hex.trim_start_matches("0x").to_lowercase();
    let bytes = hex::decode(&manifest_hex).map_err(|e| ReleaseError::Hex(e.to_string()))?;
    if bytes.len() != 32 {
        return Err(ReleaseError::Hex(format!(
            "sha256 must decode to 32 bytes, got {}",
            bytes.len()
        )));
    }
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&bytes);
    let payload = canonical_payload(&manifest.version, &digest, manifest.timestamp_unix);
    let sig = Signature::from_hex(&manifest.ed25519_sig_hex)?;
    expected_pubkey.verify(&payload, &sig)?;
    Ok(())
}

/// Verify a release: confirm the binary's hash matches the manifest
/// AND the manifest's signature verifies under `expected_pubkey`.
///
/// # Errors
///
/// Returns the most specific [`ReleaseError`] for whichever check
/// failed first — hex parse, hash mismatch, or signature reject.
/// On success, returns the bytes of the verified binary so callers
/// can avoid re-reading the file.
pub fn verify_release(
    expected_pubkey: &PublicKey,
    manifest: &ReleaseManifest,
    binary_path: &Path,
) -> Result<Vec<u8>, ReleaseError> {
    // 1. Hash the binary.
    let bytes = std::fs::read(binary_path)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let computed: [u8; 32] = hasher.finalize().into();
    let computed_hex = hex::encode(computed);
    // 2. Compare to the hash claimed in the manifest.
    let manifest_hex = manifest.sha256_hex.trim_start_matches("0x").to_lowercase();
    if manifest_hex != computed_hex {
        return Err(ReleaseError::HashMismatch {
            manifest: manifest_hex,
            computed: computed_hex,
        });
    }
    // 3. Verify the Ed25519 signature over the canonical payload.
    let payload = canonical_payload(&manifest.version, &computed, manifest.timestamp_unix);
    let sig = Signature::from_hex(&manifest.ed25519_sig_hex)?;
    expected_pubkey.verify(&payload, &sig)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_core::OsRng;
    use std::io::Write;

    fn write_tmp(content: &[u8]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content).unwrap();
        f
    }

    #[test]
    fn sign_then_verify_round_trip() {
        let mut rng = OsRng;
        let sk = SecretKey::generate(&mut rng);
        let pk = sk.public();
        let bin = write_tmp(b"hello, aii v0.0.74 binary contents");
        let m = sign_release(&sk, bin.path(), "0.0.74", 1_716_800_000).unwrap();
        verify_release(&pk, &m, bin.path()).expect("happy-path verify must succeed");
        // Manifest fields look sane.
        assert_eq!(m.version, "0.0.74");
        assert_eq!(m.timestamp_unix, 1_716_800_000);
        assert_eq!(m.sha256_hex.len(), 64);
        assert_eq!(m.ed25519_sig_hex.len(), 128);
    }

    #[test]
    fn tampered_binary_fails_hash_check() {
        let mut rng = OsRng;
        let sk = SecretKey::generate(&mut rng);
        let pk = sk.public();
        let bin = write_tmp(b"original binary");
        let m = sign_release(&sk, bin.path(), "0.0.74", 1_716_800_000).unwrap();

        // Swap in a tampered binary at the same path.
        let tampered = write_tmp(b"tampered binary");
        let err = verify_release(&pk, &m, tampered.path()).unwrap_err();
        assert!(
            matches!(err, ReleaseError::HashMismatch { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn tampered_manifest_version_fails_signature() {
        let mut rng = OsRng;
        let sk = SecretKey::generate(&mut rng);
        let pk = sk.public();
        let bin = write_tmp(b"some binary");
        let mut m = sign_release(&sk, bin.path(), "0.0.74", 1_716_800_000).unwrap();
        // Forge a higher version into the manifest.
        m.version = "0.0.99".into();
        let err = verify_release(&pk, &m, bin.path()).unwrap_err();
        assert!(matches!(err, ReleaseError::Crypto(_)), "got {err:?}");
    }

    #[test]
    fn tampered_manifest_timestamp_fails_signature() {
        let mut rng = OsRng;
        let sk = SecretKey::generate(&mut rng);
        let pk = sk.public();
        let bin = write_tmp(b"some binary");
        let mut m = sign_release(&sk, bin.path(), "0.0.74", 1_716_800_000).unwrap();
        m.timestamp_unix += 86_400;
        let err = verify_release(&pk, &m, bin.path()).unwrap_err();
        assert!(matches!(err, ReleaseError::Crypto(_)), "got {err:?}");
    }

    #[test]
    fn wrong_public_key_fails_signature() {
        let mut rng = OsRng;
        let sk = SecretKey::generate(&mut rng);
        let bin = write_tmp(b"binary");
        let m = sign_release(&sk, bin.path(), "0.0.74", 1_716_800_000).unwrap();
        let other_pk = SecretKey::generate(&mut rng).public();
        let err = verify_release(&other_pk, &m, bin.path()).unwrap_err();
        assert!(matches!(err, ReleaseError::Crypto(_)), "got {err:?}");
    }

    #[test]
    fn json_round_trip_preserves_all_fields() {
        let mut rng = OsRng;
        let sk = SecretKey::generate(&mut rng);
        let pk = sk.public();
        let bin = write_tmp(b"json round trip binary");
        let m = sign_release(&sk, bin.path(), "0.0.74", 1_716_800_000).unwrap();
        let s = serde_json::to_string_pretty(&m).unwrap();
        let m2: ReleaseManifest = serde_json::from_str(&s).unwrap();
        verify_release(&pk, &m2, bin.path()).unwrap();
    }
}
