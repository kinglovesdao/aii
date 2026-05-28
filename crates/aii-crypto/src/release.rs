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

/// Hex of the **primary** AII Network release-signing public key.
///
/// (v0.0.75) The matching secret seed is held off-chain by the
/// release manager — every node ships with this public half
/// compiled in so [`pinned_release_pubkey`] can be used as the
/// default trust anchor for `aii release verify` and for the
/// `aii_announceRelease` JSON-RPC endpoint.
///
/// **v0.0.87**: this constant is now the FIRST entry of
/// [`RELEASE_SIGNING_PUBKEYS`]. See that constant for the
/// rotation procedure.
pub const RELEASE_SIGNING_PUBKEY_HEX: &str =
    "f845c1bbf443bbf3e18f1a97599c34d39cac4a03fb22c80110f37491c92c0669";

/// All compile-time-pinned release-signing public keys
/// (v0.0.87).
///
/// A release manifest verifies iff it is signed by **any** key
/// in this slice. The first entry is the primary key; any
/// additional entries are pending-deprecation or
/// rolling-in keys.
///
/// ### Rotation procedure
///
/// To rotate from key `A` to a new key `B`:
///
/// 1. Generate `B` with `aii release keygen`.
/// 2. Edit this slice to `[A_HEX, B_HEX]`, ship the workspace,
///    and roll the binary out to every node via the v0.0.74-86
///    auto-update protocol.
/// 3. Once every node is on a binary with both keys pinned,
///    switch the release manager's signing operations to `B`.
///    Existing nodes accept both, so this is invisible to
///    consensus.
/// 4. In a subsequent release, edit this slice to `[B_HEX]` and
///    ship again. After that wave lands, `A` is officially
///    retired.
///
/// The order matters only for [`pinned_release_pubkey`], which
/// returns the first entry for back-compat callers. Verification
/// tries them in order; the first match wins, so put the
/// most-likely-active key first when ordering is otherwise
/// unimportant.
pub const RELEASE_SIGNING_PUBKEYS: &[&str] = &[RELEASE_SIGNING_PUBKEY_HEX];

/// Parse the **primary** pinned release pubkey into a usable
/// [`PublicKey`].
///
/// Equivalent to `pinned_release_pubkeys()[0]`. Retained for
/// back-compat with v0.0.74-v0.0.86 callers that knew only the
/// single-key world.
///
/// # Panics
///
/// Panics if the compiled-in constant is malformed — that's a
/// build-time error, so we want it to surface loudly.
#[must_use]
pub fn pinned_release_pubkey() -> PublicKey {
    PublicKey::from_hex(RELEASE_SIGNING_PUBKEY_HEX).expect("pinned release pubkey must parse")
}

/// Parse all compile-time pinned release pubkeys into usable
/// [`PublicKey`] values (v0.0.87).
///
/// Use this when verifying a manifest you want to accept under
/// the current rotation policy. The single-key
/// [`pinned_release_pubkey`] only sees the primary; this
/// returns every key the binary trusts.
///
/// # Panics
///
/// Panics if any compiled-in entry is malformed — a build-time
/// error, so it surfaces loudly.
#[must_use]
pub fn pinned_release_pubkeys() -> Vec<PublicKey> {
    RELEASE_SIGNING_PUBKEYS
        .iter()
        .map(|hex| PublicKey::from_hex(hex).expect("pinned release pubkey must parse"))
        .collect()
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

/// Verify the Ed25519 signature on a manifest against ANY of
/// the supplied pubkeys (v0.0.87 rotation support).
///
/// Returns `Ok(())` on the first key that verifies; returns the
/// LAST key's error if none verify. Useful for the multi-key
/// pinned set during a key rotation window.
///
/// # Errors
///
/// - `ReleaseError::Hex` when `manifest.sha256_hex` is
///   malformed (decoded once for all candidate keys).
/// - `ReleaseError::Crypto` carrying the last attempted key's
///   rejection reason when none of `pubkeys` verifies.
/// - `ReleaseError::Hex` carrying "no pubkeys supplied" when
///   `pubkeys` is empty.
pub fn verify_manifest_signature_any(
    pubkeys: &[PublicKey],
    manifest: &ReleaseManifest,
) -> Result<(), ReleaseError> {
    if pubkeys.is_empty() {
        return Err(ReleaseError::Hex("no pubkeys supplied".to_string()));
    }
    // Decode the manifest fields ONCE so we don't redo it per
    // candidate key.
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
    let mut last_err: Option<ReleaseError> = None;
    for pk in pubkeys {
        match pk.verify(&payload, &sig) {
            Ok(()) => return Ok(()),
            Err(e) => last_err = Some(e.into()),
        }
    }
    Err(last_err.expect("non-empty pubkeys must produce at least one verify attempt"))
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

    // ───────────────── v0.0.87 multi-key rotation tests ─────────────────

    #[test]
    fn pinned_release_pubkeys_contains_primary_first() {
        let pks = pinned_release_pubkeys();
        assert!(!pks.is_empty(), "must always have at least one pinned key");
        assert_eq!(
            pks[0].to_hex(),
            RELEASE_SIGNING_PUBKEY_HEX,
            "primary pubkey must be first entry",
        );
        assert_eq!(
            pks[0].to_hex(),
            pinned_release_pubkey().to_hex(),
            "single-key getter must equal first entry of multi-key getter",
        );
    }

    #[test]
    fn verify_any_accepts_manifest_signed_by_first_key() {
        let mut rng = OsRng;
        let sk_a = SecretKey::generate(&mut rng);
        let sk_b = SecretKey::generate(&mut rng);
        let bin = write_tmp(b"multikey body");
        let m = sign_release(&sk_a, bin.path(), "0.0.87", 1_900_000_087).unwrap();
        // Pubkey list with A first, B second.
        let keys = vec![sk_a.public(), sk_b.public()];
        verify_manifest_signature_any(&keys, &m).unwrap();
    }

    #[test]
    fn verify_any_accepts_manifest_signed_by_secondary_key() {
        let mut rng = OsRng;
        let sk_a = SecretKey::generate(&mut rng);
        let sk_b = SecretKey::generate(&mut rng);
        let bin = write_tmp(b"multikey body");
        // Sign with B but B is the SECOND key in the list.
        let m = sign_release(&sk_b, bin.path(), "0.0.87", 1_900_000_088).unwrap();
        let keys = vec![sk_a.public(), sk_b.public()];
        verify_manifest_signature_any(&keys, &m)
            .expect("manifest signed by B must verify under [A, B]");
    }

    #[test]
    fn verify_any_rejects_manifest_not_in_set() {
        let mut rng = OsRng;
        let sk_a = SecretKey::generate(&mut rng);
        let sk_b = SecretKey::generate(&mut rng);
        let sk_c = SecretKey::generate(&mut rng); // unknown signer
        let bin = write_tmp(b"multikey body");
        let m = sign_release(&sk_c, bin.path(), "0.0.87", 1_900_000_089).unwrap();
        let keys = vec![sk_a.public(), sk_b.public()];
        let err = verify_manifest_signature_any(&keys, &m).unwrap_err();
        assert!(matches!(err, ReleaseError::Crypto(_)), "got {err:?}");
    }

    #[test]
    fn verify_any_with_empty_set_returns_hex_error() {
        // Building a manifest first so the function reaches the
        // empty-set guard before any other checks.
        let mut rng = OsRng;
        let sk = SecretKey::generate(&mut rng);
        let bin = write_tmp(b"empty set body");
        let m = sign_release(&sk, bin.path(), "0.0.87", 1_900_000_090).unwrap();
        let err = verify_manifest_signature_any(&[], &m).unwrap_err();
        assert!(matches!(err, ReleaseError::Hex(_)), "got {err:?}");
    }

    #[test]
    fn verify_any_short_circuits_on_first_match() {
        // If the first key verifies, the function MUST NOT
        // attempt subsequent keys. We can't observe the
        // short-circuit directly, but a wrong second key with
        // garbage hex would error during Signature::from_hex
        // before any verify() call. Since the function decodes
        // the signature ONCE up front, this test mainly proves
        // it doesn't iterate redundantly.
        let mut rng = OsRng;
        let sk_a = SecretKey::generate(&mut rng);
        let sk_b = SecretKey::generate(&mut rng);
        let bin = write_tmp(b"short circuit body");
        let m = sign_release(&sk_a, bin.path(), "0.0.87", 1_900_000_091).unwrap();
        // Putting A first means we should accept immediately
        // even though B is unrelated.
        let keys = vec![sk_a.public(), sk_b.public()];
        verify_manifest_signature_any(&keys, &m).unwrap();
    }
}
