//! Persistent BFT round-state snapshot (v0.0.71).
//!
//! Stores the engine's `(height, round)` at every successful tick so
//! a restarted validator can fast-forward its coordinator to the
//! current network round instead of starting at round 0. Without
//! this, a single-validator restart leaves the rest of the validator
//! set stuck at round R while the restarted node sits at round 0 —
//! their votes never combine and the chain freezes until every
//! validator restarts together.
//!
//! The snapshot is intentionally minimal — `{height, round}` — and
//! omits `locked_value` / `polc` / vote tallies. Those are
//! safety-critical state but persisting them needs a careful
//! serializer for the BLS / VRF certificate machinery; v0.0.71 ships
//! the liveness fix alone and a future release (v0.0.72+) extends
//! the snapshot to include lock state. For a development testnet
//! the liveness win is what unblocks operational work.
//!
//! ## File format
//!
//! JSON, two lines:
//!
//! ```json
//! {"height": 696, "round": 3}
//! ```
//!
//! Writes go through `bft_state.json.tmp` + `rename(2)` so a crash
//! mid-write can never strand a partial file.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Filename inside the node data directory.
pub const BFT_STATE_FILENAME: &str = "bft_state.json";

/// Resolve `<data_dir>/bft_state.json` without creating it.
#[must_use]
pub fn state_path(data_dir: &Path) -> PathBuf {
    data_dir.join(BFT_STATE_FILENAME)
}

/// Persisted snapshot of the engine's in-flight BFT round state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BftStateSnapshot {
    /// Height the coordinator is finalising (i.e. one above the
    /// chain's last committed block).
    pub height: u64,
    /// Round number within that height.
    pub round: u32,
}

impl BftStateSnapshot {
    /// Construct a fresh `(height, round)` snapshot.
    #[must_use]
    pub const fn new(height: u64, round: u32) -> Self {
        Self { height, round }
    }
}

/// Load a snapshot from disk.
///
/// Returns `Ok(None)` if the file doesn't exist (fresh node, no
/// prior round state). Returns `Ok(None)` also if the file is
/// malformed — we'd rather restart at round 0 than crash on a
/// corrupted snapshot.
///
/// # Errors
///
/// Only true I/O errors (permission denied, broken disk, …) bubble
/// up. Missing files and parse errors fall back to `None`.
pub fn load(path: &Path) -> io::Result<Option<BftStateSnapshot>> {
    let content = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    Ok(serde_json::from_str(&content).ok())
}

/// Atomically write `snapshot` to `path`.
///
/// # Errors
///
/// Bubbles up `io::Error` if the parent directory is missing, the
/// temp file can't be written, or the rename fails. JSON
/// serialization is infallible for this type so no `serde_json`
/// error path is exposed.
pub fn save(path: &Path, snapshot: BftStateSnapshot) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let body = serde_json::to_string(&snapshot)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, body.as_bytes())?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "aii-bft-state-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn load_missing_file_returns_none() {
        let dir = tempdir();
        let path = state_path(&dir);
        assert_eq!(load(&path).unwrap(), None);
    }

    #[test]
    fn save_then_load_round_trip() {
        let dir = tempdir();
        let path = state_path(&dir);
        let snap = BftStateSnapshot::new(696, 3);
        save(&path, snap).unwrap();
        assert_eq!(load(&path).unwrap(), Some(snap));
    }

    #[test]
    fn save_overwrites_atomically_no_tmp_left_behind() {
        let dir = tempdir();
        let path = state_path(&dir);
        save(&path, BftStateSnapshot::new(1, 0)).unwrap();
        save(&path, BftStateSnapshot::new(1, 5)).unwrap();
        let tmp = path.with_extension("json.tmp");
        assert!(!tmp.exists(), "temp file must be cleaned up");
        assert_eq!(load(&path).unwrap(), Some(BftStateSnapshot::new(1, 5)));
    }

    #[test]
    fn load_tolerates_garbage() {
        let dir = tempdir();
        let path = state_path(&dir);
        fs::write(&path, "not really json").unwrap();
        // Garbage → treated as "no snapshot" rather than I/O error.
        assert_eq!(load(&path).unwrap(), None);
    }
}
