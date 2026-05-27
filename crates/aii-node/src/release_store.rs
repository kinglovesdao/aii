//! On-disk store for verified release binaries (v0.0.76).
//!
//! Lives at `<data-dir>/releases/<version>` so an operator can also
//! cherry-pick a known-good binary by version when running outside
//! the auto-update path. Every write goes through
//! [`store_verified_binary`] which:
//!
//! 1. Recomputes the SHA-256 of the supplied bytes.
//! 2. Compares to the expected hash (from a verified
//!    [`aii_crypto::release::ReleaseManifest`]).
//! 3. Writes atomically (`.tmp` then `rename(2)`) on hash match,
//!    or returns [`ReleaseStoreError::HashMismatch`] without ever
//!    creating the target file on a mismatch.
//!
//! v0.0.76 ships only the store + RPC plumbing. Auto-fetch from a
//! peer that has the binary, gossip relay of announcements, and
//! atomic install + restart all land in v0.0.77+ on top of this.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;

/// Sub-directory inside the node data dir where verified release
/// binaries are cached.
pub const RELEASES_SUBDIR: &str = "releases";

/// Errors raised by the release-store module.
#[derive(Debug, Error)]
pub enum ReleaseStoreError {
    /// File I/O failure (directory missing, permission denied, …).
    #[error("io: {0}")]
    Io(#[from] io::Error),

    /// Hex decode failure on the expected SHA-256 from the manifest.
    #[error("hex: {0}")]
    Hex(String),

    /// The supplied binary bytes hash to a different SHA-256 than
    /// the manifest claims. The on-disk file is NOT created.
    #[error("hash mismatch: expected {expected}, computed {computed}")]
    HashMismatch {
        /// Hash claimed by the manifest (lowercase hex, no `0x`).
        expected: String,
        /// Hash computed from the supplied bytes (lowercase hex).
        computed: String,
    },
}

/// Resolve the canonical on-disk path `<data-dir>/releases/<version>`.
#[must_use]
pub fn binary_path(data_dir: &Path, version: &str) -> PathBuf {
    data_dir.join(RELEASES_SUBDIR).join(version)
}

/// Write `bytes` to `<data-dir>/releases/<version>` iff
/// `sha256(bytes)` matches `expected_sha256_hex`.
///
/// Atomic: bytes land at `<target>.tmp` first, then `rename(2)` to
/// the final path. On hash mismatch nothing is written.
///
/// Returns the absolute path the binary now lives at.
///
/// # Errors
///
/// - [`ReleaseStoreError::Hex`] when `expected_sha256_hex` doesn't
///   decode to 32 bytes of hex.
/// - [`ReleaseStoreError::HashMismatch`] when the supplied bytes
///   hash differently from `expected_sha256_hex`.
/// - [`ReleaseStoreError::Io`] for parent-dir mkdir, temp-file
///   write, or rename failure.
pub fn store_verified_binary(
    data_dir: &Path,
    version: &str,
    expected_sha256_hex: &str,
    bytes: &[u8],
) -> Result<PathBuf, ReleaseStoreError> {
    let expected_lower = expected_sha256_hex
        .trim_start_matches("0x")
        .to_ascii_lowercase();
    let expected_bytes =
        hex::decode(&expected_lower).map_err(|e| ReleaseStoreError::Hex(e.to_string()))?;
    if expected_bytes.len() != 32 {
        return Err(ReleaseStoreError::Hex(format!(
            "sha256 must decode to 32 bytes, got {}",
            expected_bytes.len()
        )));
    }
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let computed: [u8; 32] = hasher.finalize().into();
    let computed_hex = hex::encode(computed);
    if computed_hex != expected_lower {
        return Err(ReleaseStoreError::HashMismatch {
            expected: expected_lower,
            computed: computed_hex,
        });
    }
    let target = binary_path(data_dir, version);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = target.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, &target)?;
    Ok(target)
}

/// Read the cached binary for `version`, or `Ok(None)` if not present.
///
/// # Errors
///
/// Only true I/O errors bubble up; missing-file is `Ok(None)`.
pub fn load_binary(data_dir: &Path, version: &str) -> io::Result<Option<Vec<u8>>> {
    match fs::read(binary_path(data_dir, version)) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "aii-release-store-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(bytes);
        hex::encode(<[u8; 32]>::from(h.finalize()))
    }

    #[test]
    fn store_then_load_round_trip() {
        let dir = tempdir();
        let bytes = b"a release binary body";
        let h = sha256_hex(bytes);
        let path = store_verified_binary(&dir, "0.0.76", &h, bytes).unwrap();
        assert!(path.exists());
        let loaded = load_binary(&dir, "0.0.76").unwrap().unwrap();
        assert_eq!(loaded, bytes);
    }

    #[test]
    fn hash_mismatch_rejects_and_does_not_create_file() {
        let dir = tempdir();
        let bytes = b"actual contents";
        let wrong = sha256_hex(b"different bytes");
        let err = store_verified_binary(&dir, "0.0.76", &wrong, bytes).unwrap_err();
        assert!(matches!(err, ReleaseStoreError::HashMismatch { .. }));
        assert!(!binary_path(&dir, "0.0.76").exists());
    }

    #[test]
    fn malformed_hash_rejected() {
        let dir = tempdir();
        let err = store_verified_binary(&dir, "0.0.76", "not-hex", b"any").unwrap_err();
        assert!(matches!(err, ReleaseStoreError::Hex(_)));
    }

    #[test]
    fn load_missing_returns_none() {
        let dir = tempdir();
        assert!(load_binary(&dir, "0.0.99").unwrap().is_none());
    }

    #[test]
    fn tmp_file_cleaned_up_on_successful_write() {
        let dir = tempdir();
        let bytes = b"hello";
        store_verified_binary(&dir, "0.0.76", &sha256_hex(bytes), bytes).unwrap();
        let tmp = binary_path(&dir, "0.0.76").with_extension("tmp");
        assert!(!tmp.exists(), "atomic rename should remove .tmp");
    }

    #[test]
    fn accepts_0x_prefixed_expected_hash() {
        let dir = tempdir();
        let bytes = b"with prefix";
        let prefixed = format!("0x{}", sha256_hex(bytes));
        store_verified_binary(&dir, "0.0.76", &prefixed, bytes).unwrap();
    }
}
