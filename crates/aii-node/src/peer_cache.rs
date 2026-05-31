//! Persistent peer cache (v0.0.69).
//!
//! Saves the set of validator peer endpoints `aiid` has successfully
//! talked to. On startup, the cache is loaded and merged with any
//! `--peers` passed on the command line, so a node that has talked to
//! the network even once can rejoin without operator config after a
//! restart. This is the on-disk half of the "断网恢复后立即自动组网"
//! property — combined with `--bft-outbound-only` from v0.0.68 it
//! removes every manual step required to rejoin BFT.
//!
//! ## File format
//!
//! One `host:port` line per peer, sorted, deduplicated, trailing
//! newline. Comments (`#` prefix) and blank lines are tolerated on
//! load. Writes are atomic: the new content lands in `peers.json.tmp`
//! and is then `rename(2)`d over the target so a crash mid-write can
//! never leave a half-written cache.
//!
//! ## Why text, not JSON
//!
//! A flat `host:port\n` file is greppable, hand-editable, and the
//! file name (`peers.json`) is a hint for the future when richer
//! per-peer metadata (e.g. multi-endpoint sets from task #46) will
//! warrant a real JSON schema. Until then the parser ignores
//! anything that doesn't look like a SocketAddr.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

/// Filename inside the node data directory.
pub const PEER_CACHE_FILENAME: &str = "peers.json";

/// Resolve `<data_dir>/peers.json` without creating it.
#[must_use]
pub fn cache_path(data_dir: &Path) -> PathBuf {
    data_dir.join(PEER_CACHE_FILENAME)
}

/// Load a peer cache from `path`.
///
/// Missing file is **not** an error — returns an empty Vec. Lines
/// that don't parse as `SocketAddr` are skipped silently (so the
/// file can host comments and stale junk without breaking startup).
///
/// # Errors
///
/// Only true I/O errors (permission denied, broken disk, …) bubble
/// up. Malformed content is tolerated.
pub fn load(path: &Path) -> io::Result<Vec<SocketAddr>> {
    let content = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out: BTreeSet<SocketAddr> = BTreeSet::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Ok(addr) = trimmed.parse::<SocketAddr>() {
            out.insert(addr);
        }
    }
    Ok(out.into_iter().collect())
}

/// Atomically save `peers` to `path`.
///
/// Writes to `path.tmp` then renames over `path` so a concurrent
/// reader (or a crash mid-write) always observes the previous valid
/// content. The output is sorted + deduplicated for stable diffs and
/// idempotent saves.
///
/// # Errors
///
/// Returns the underlying `io::Error` if the parent directory is
/// missing, the temp file can't be written, or the rename fails.
pub fn save(path: &Path, peers: &[SocketAddr]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let mut deduped: BTreeSet<SocketAddr> = BTreeSet::new();
    deduped.extend(peers.iter().copied());

    let tmp = path.with_extension("json.tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        writeln!(
            f,
            "# aiid peer cache — written by aii-node v0.0.69; format is one SocketAddr per line"
        )?;
        for addr in &deduped {
            writeln!(f, "{addr}")?;
        }
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Merge an authoritative set of peers (`current`) into the
/// already-loaded `cached` set and return the deduplicated union.
///
/// Stable order: cache first (so existing entries keep their
/// position), then new entries from `current` that aren't yet in the
/// cache. This matters because the dialer iterates the returned Vec
/// in order — keeping known-good peers first lets the node bond
/// quickly on restart instead of waiting for a fresh handshake with
/// every new peer.
#[must_use]
pub fn merge(cached: &[SocketAddr], current: &[SocketAddr]) -> Vec<SocketAddr> {
    let mut seen: BTreeSet<SocketAddr> = BTreeSet::new();
    let mut out = Vec::with_capacity(cached.len() + current.len());
    for addr in cached.iter().chain(current.iter()) {
        if seen.insert(*addr) {
            out.push(*addr);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn tempdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "aii-peer-cache-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn sa(ip: [u8; 4], port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(ip[0], ip[1], ip[2], ip[3])), port)
    }

    #[test]
    fn load_missing_file_returns_empty() {
        let dir = tempdir();
        let path = cache_path(&dir);
        assert!(load(&path).unwrap().is_empty());
    }

    #[test]
    fn save_then_load_round_trip() {
        let dir = tempdir();
        let path = cache_path(&dir);
        let peers = vec![
            sa([8, 211, 135, 234], 30311),
            sa([106, 14, 223, 128], 30311),
        ];
        save(&path, &peers).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(loaded.contains(&peers[0]));
        assert!(loaded.contains(&peers[1]));
    }

    #[test]
    fn load_tolerates_comments_and_garbage() {
        let dir = tempdir();
        let path = cache_path(&dir);
        fs::write(
            &path,
            "# header\n\n8.211.135.234:30311\nnot-an-addr\n\n# tail\n106.14.223.128:30311\n",
        )
        .unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.len(), 2);
    }

    #[test]
    fn save_dedupes_and_sorts() {
        let dir = tempdir();
        let path = cache_path(&dir);
        let peers = vec![
            sa([10, 0, 0, 3], 30311),
            sa([10, 0, 0, 1], 30311),
            sa([10, 0, 0, 2], 30311),
            sa([10, 0, 0, 1], 30311), // dup
        ];
        save(&path, &peers).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.len(), 3);
        // BTreeSet ensures sorted output.
        assert_eq!(loaded[0], sa([10, 0, 0, 1], 30311));
        assert_eq!(loaded[2], sa([10, 0, 0, 3], 30311));
    }

    #[test]
    fn save_is_atomic_no_tmp_left_behind() {
        let dir = tempdir();
        let path = cache_path(&dir);
        save(&path, &[sa([1, 1, 1, 1], 30311)]).unwrap();
        let tmp = path.with_extension("json.tmp");
        assert!(path.exists(), "final file must exist after save");
        assert!(!tmp.exists(), "temp file must be renamed away");
    }

    #[test]
    fn merge_preserves_cache_order_and_appends_new() {
        let cached = vec![sa([1, 1, 1, 1], 30311), sa([2, 2, 2, 2], 30311)];
        let current = vec![
            sa([2, 2, 2, 2], 30311), // dup
            sa([3, 3, 3, 3], 30311), // new
        ];
        let merged = merge(&cached, &current);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0], sa([1, 1, 1, 1], 30311));
        assert_eq!(merged[1], sa([2, 2, 2, 2], 30311));
        assert_eq!(merged[2], sa([3, 3, 3, 3], 30311));
    }
}
