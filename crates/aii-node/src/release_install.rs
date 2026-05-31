//! Atomic install of a verified release binary over the currently
//! running `aiid`, plus in-place `execve` self-restart (v0.0.78).
//!
//! Pairs with [`crate::release_store`] (binary cache). Owns the
//! last mile of the auto-update flow:
//!
//! 1. **Atomic install** — copy `<data-dir>/releases/<version>`
//!    to `<current-aiid>.new`, set mode `0o755`, then `rename(2)`
//!    over the running binary. On Linux `rename(2)` is allowed to
//!    replace a currently-executing file because the kernel keeps
//!    the inode alive for the running process via its open mmap;
//!    only new `execve(2)` calls see the replacement.
//!
//! 2. **execve self-restart** — `Command::new(current_exe).args(…).exec()`
//!    replaces the running process image with the new binary while
//!    preserving the PID. Systemd does NOT respawn the unit — the
//!    upgrade is invisible to the supervisor, which is the whole
//!    point. Stale BFT timers, in-flight RPC sockets, mempool: all
//!    discarded; a freshly-built `NodeState` takes over.
//!
//! Designed for Linux. The module is gated `#[cfg(unix)]`; on
//! non-Unix targets [`crate::lib`] swaps in a stub that returns
//! `"install not supported on this target"`.

#![cfg(unix)]

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

/// Suffix appended to the target path while staging the new binary.
///
/// Stays consistent so a stale `.new` from a crashed previous
/// attempt is harmless — [`install_binary`] deletes any
/// pre-existing `.new` before staging.
pub const NEW_SUFFIX: &str = ".new";

/// File mode applied to the staged binary before rename.
///
/// `0o755` = owner rwx + group/other rx — matches what
/// `cargo build` produces and what systemd-managed binaries
/// typically have under `/usr/local/bin`.
pub const INSTALLED_MODE: u32 = 0o755;

/// File name of the pre-install snapshot inside the release
/// store (v0.0.80).
///
/// Lives at `<data-dir>/releases/<PREVIOUS_NAME>` and holds a
/// byte-for-byte copy of the binary that was running just
/// before the most recent `install_binary` call. The rollback
/// path reads it back via [`rollback_to_previous`].
///
/// The dot-prefix avoids collision with a hypothetical release
/// version literally named `previous` — release versions are
/// semver-ish (`0.0.80`) and never start with `.`.
pub const PREVIOUS_NAME: &str = ".previous";

/// Resolve the on-disk path of the pre-install snapshot.
#[must_use]
pub fn previous_path(releases_dir: &Path) -> PathBuf {
    releases_dir.join(PREVIOUS_NAME)
}

/// Snapshot the currently-running aiid binary at `current_exe`
/// into `<releases_dir>/<PREVIOUS_NAME>` (v0.0.80).
///
/// Call this BEFORE [`install_binary`] so the rollback path
/// has something to restore. Writes atomically via
/// `<target>.new` + `rename(2)` so a crash mid-copy never
/// leaves a half-written `.previous` that pretends to be a
/// valid binary.
///
/// # Errors
///
/// I/O failures from mkdir, copy, set_permissions, or rename.
pub fn save_previous(current_exe: &Path, releases_dir: &Path) -> io::Result<PathBuf> {
    fs::create_dir_all(releases_dir)?;
    let target = previous_path(releases_dir);
    let staging = path_with_suffix(&target, NEW_SUFFIX);
    if staging.exists() {
        fs::remove_file(&staging)?;
    }
    fs::copy(current_exe, &staging)?;
    let mut perm = fs::metadata(&staging)?.permissions();
    perm.set_mode(INSTALLED_MODE);
    fs::set_permissions(&staging, perm)?;
    fs::rename(&staging, &target)?;
    Ok(target)
}

/// Roll the binary at `target` back to the snapshot stored at
/// `<releases_dir>/<PREVIOUS_NAME>` (v0.0.80).
///
/// Composes [`save_previous`] (snapshot current as the *new*
/// `.previous` before clobbering it) with [`install_binary`]
/// (atomic rename of the old `.previous` over the running
/// binary). After this call:
///
/// 1. The previously-running bytes are now at `target`.
/// 2. The bytes that were at `target` going in are now at
///    `<releases_dir>/<PREVIOUS_NAME>`, so you can roll
///    forward again with another rollback if needed.
///
/// Returns the absolute path of the now-installed
/// (previously-snapshotted) binary on success.
///
/// # Errors
///
/// - `io::ErrorKind::NotFound` if `.previous` doesn't exist
///   (typical on a node that has never installed a release).
/// - Any I/O failure from the underlying copy / chmod /
///   rename steps.
pub fn rollback_to_previous(releases_dir: &Path, target: &Path) -> io::Result<PathBuf> {
    let previous = previous_path(releases_dir);
    if !previous.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "no pre-install snapshot at {} — nothing to roll back to",
                previous.display()
            ),
        ));
    }
    // We need to swap target ↔ .previous atomically-ish. Move
    // .previous to a holding path FIRST so the upcoming
    // save_previous (which writes `.previous`) doesn't clobber
    // the bytes we want to restore. After the swap the operator
    // can roll back a second time to flip back to whatever was
    // running at the start of this call.
    let holding = path_with_suffix(&previous, ".roll");
    if holding.exists() {
        fs::remove_file(&holding)?;
    }
    fs::rename(&previous, &holding)?;
    // Snapshot the about-to-be-replaced bytes as the new
    // `.previous` so a second rollback flips the pair back.
    let _: PathBuf = save_previous(target, releases_dir)?;
    // Overlay the held snapshot onto `target`.
    let installed = install_binary(&holding, target);
    // Clean up holding regardless of install outcome.
    let _ = fs::remove_file(&holding);
    installed
}

/// Replace `target` with the bytes of `staged` atomically.
///
/// Steps:
/// 1. Remove any stale `<target>.new` from a previous attempt.
/// 2. Copy `staged` → `<target>.new`.
/// 3. `chmod 0o755` on `<target>.new`.
/// 4. `rename(<target>.new, target)`.
///
/// Returns the absolute path the binary now lives at.
///
/// # Errors
///
/// I/O errors from copy, chmod, or rename. On any failure either
/// `<target>.new` may exist or `target` was already replaced —
/// the function never leaves a partially-written `target`.
pub fn install_binary(staged: &Path, target: &Path) -> io::Result<PathBuf> {
    let new_path = path_with_suffix(target, NEW_SUFFIX);
    if new_path.exists() {
        fs::remove_file(&new_path)?;
    }
    fs::copy(staged, &new_path)?;
    let mut perm = fs::metadata(&new_path)?.permissions();
    perm.set_mode(INSTALLED_MODE);
    fs::set_permissions(&new_path, perm)?;
    fs::rename(&new_path, target)?;
    Ok(target.to_path_buf())
}

fn path_with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Resolve the currently-running aiid binary path via
/// [`std::env::current_exe`].
///
/// On Linux this returns the realpath of `/proc/self/exe` — the
/// actual file backing the running process, which is what
/// [`install_binary`] needs to replace.
///
/// # Errors
///
/// I/O from `current_exe()` (rare — usually `EACCES` on hardened
/// kernels that hide `/proc/self/exe`).
pub fn current_aiid_path() -> io::Result<PathBuf> {
    std::env::current_exe()
}

/// `execve(2)` the current process with its original args and
/// env, using `exe` as the new program image.
///
/// On success this function does NOT return — the process is
/// replaced. On failure the returned [`io::Error`] describes
/// why; the caller (typically [`crate::lib::NodeState`]) is
/// responsible for logging it and continuing to serve from the
/// old binary.
///
/// PID is preserved across the exec, so systemd does NOT
/// respawn the unit. From the supervisor's perspective the
/// upgrade is invisible.
///
/// **Why explicit `exe`**: after [`install_binary`] replaces
/// the running binary via `rename(2)`, the kernel marks
/// `/proc/self/exe` with a `" (deleted)"` suffix (because the
/// originally-loaded inode is detached from its directory
/// entry). [`current_aiid_path`] then returns a literal path
/// ending in `" (deleted)"`, which `execve` rejects with
/// `ENOENT`. The install path is the one place that already
/// holds the correct on-disk path, so it passes it in
/// directly.
///
/// # Errors
///
/// `execve` failure: missing binary, non-executable file,
/// ENOMEM, etc.
pub fn exec_self_at(exe: &Path) -> io::Error {
    use std::os::unix::process::CommandExt as _;
    use std::process::Command;
    let mut args = std::env::args_os();
    args.next();
    let mut cmd = Command::new(exe);
    cmd.args(args);
    cmd.exec()
}

/// Convenience wrapper around [`exec_self_at`] that resolves
/// the target path via [`current_aiid_path`].
///
/// Safe to call BEFORE [`install_binary`] runs. Once the
/// running binary has been replaced via rename, prefer
/// [`exec_self_at`] with the install-time target path —
/// `/proc/self/exe` may then resolve with a `" (deleted)"`
/// suffix that `execve` rejects.
///
/// # Errors
///
/// `current_exe()` failure or `execve` failure.
pub fn exec_self() -> io::Error {
    let exe = match current_aiid_path() {
        Ok(p) => p,
        Err(e) => return e,
    };
    exec_self_at(&exe)
}

// ─────────────────────────── v0.0.85: boot-health ───────────────────────────

/// File name of the boot-health sentinel inside the release store.
///
/// Written by [`write_boot_pending`] just before `install_binary`
/// clobbers the running binary; consumed by the post-execve
/// startup path. Presence of this file means "a previous
/// incarnation triggered an install but the post-install boot
/// has not yet confirmed it reached a healthy state".
pub const BOOT_PENDING_NAME: &str = ".boot-pending";

/// On-disk record of an in-flight binary install.
///
/// Written atomically (`.tmp` + rename) by [`write_boot_pending`]
/// before the install completes; cleared by [`clear_boot_pending`]
/// once the new process confirms it reached a healthy state
/// (head advanced past `pre_install_head`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BootPending {
    /// Version string from the manifest that drove this install
    /// (purely diagnostic — the rollback path doesn't compare
    /// version strings).
    pub version: String,
    /// Head block number captured right before the install. The
    /// boot-health confirm task watches for the head to advance
    /// past this; if it doesn't within the grace window, the
    /// boot is considered failed and rollback is triggered.
    pub pre_install_head: u64,
    /// Unix-seconds timestamp the install was initiated. Lets
    /// downstream tooling decide whether a long-lingering
    /// `.boot-pending` represents a crash loop or a slow boot.
    pub install_ts: u64,
}

/// Resolve the on-disk path of the boot-pending sentinel.
#[must_use]
pub fn boot_pending_path(releases_dir: &Path) -> PathBuf {
    releases_dir.join(BOOT_PENDING_NAME)
}

/// Atomically write a boot-pending sentinel at
/// `<releases_dir>/<BOOT_PENDING_NAME>`.
///
/// Always overwrites any existing sentinel (a new install
/// always supersedes the previous one).
///
/// # Errors
///
/// I/O failures from mkdir, JSON serialize, write, or rename.
pub fn write_boot_pending(releases_dir: &Path, record: &BootPending) -> io::Result<PathBuf> {
    fs::create_dir_all(releases_dir)?;
    let target = boot_pending_path(releases_dir);
    let json =
        serde_json::to_vec(record).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let staging = path_with_suffix(&target, NEW_SUFFIX);
    if staging.exists() {
        fs::remove_file(&staging)?;
    }
    fs::write(&staging, &json)?;
    fs::rename(&staging, &target)?;
    Ok(target)
}

/// Read the boot-pending sentinel at
/// `<releases_dir>/<BOOT_PENDING_NAME>`, returning `Ok(None)`
/// if it doesn't exist.
///
/// # Errors
///
/// True I/O failures bubble up; missing-file is `Ok(None)`.
/// A malformed sentinel returns an I/O error (treat as
/// "unparseable, leave it for an operator to inspect").
pub fn read_boot_pending(releases_dir: &Path) -> io::Result<Option<BootPending>> {
    let path = boot_pending_path(releases_dir);
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<BootPending>(&bytes)
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Remove the boot-pending sentinel. Idempotent — succeeds
/// even if the file is already gone.
///
/// # Errors
///
/// True I/O failures bubble up; missing-file is silent success.
pub fn clear_boot_pending(releases_dir: &Path) -> io::Result<()> {
    let path = boot_pending_path(releases_dir);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

// ────────────────────── v0.0.86: restart rate limiting ──────────────────────

/// File name of the restart-log inside the release store.
///
/// Persistent, append-driven log of recent automatic restart
/// events (stall recovery via `exec_self`, boot-health
/// rollback). Used to break crash-loops: when too many
/// auto-restarts happen in a short window, the watchdogs back
/// off and leave the node in its current state for operator
/// inspection.
pub const RESTART_LOG_NAME: &str = ".restart-log";

/// On-disk record of recent automatic-restart timestamps.
///
/// Events are unix-seconds, sorted ascending. The list is
/// pruned to the trailing window on every read.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RestartLog {
    /// Timestamps of past automatic restarts, oldest first.
    pub events: Vec<u64>,
}

/// Resolve the on-disk path of the restart-log.
#[must_use]
pub fn restart_log_path(releases_dir: &Path) -> PathBuf {
    releases_dir.join(RESTART_LOG_NAME)
}

/// Read the restart-log, returning an empty log if the file
/// doesn't exist or is unparseable.
///
/// **Why permissive**: a missing or corrupt log is "we don't
/// have data about previous restarts." Treating that as "no
/// previous restarts" is the safe failure mode — the rate
/// limiter will allow the FIRST auto-restart, which is fine.
/// Treating it as "infinite previous restarts" would brick the
/// recovery path on a fresh node.
pub fn read_restart_log(releases_dir: &Path) -> RestartLog {
    let path = restart_log_path(releases_dir);
    let Ok(bytes) = fs::read(&path) else {
        return RestartLog::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

/// Append a restart event at unix-seconds `ts` and prune the
/// log to events within the trailing `window_secs`.
///
/// Atomic via `.tmp` + `rename(2)` (same pattern as
/// [`write_boot_pending`]).
///
/// # Errors
///
/// I/O failures from mkdir, write, or rename.
pub fn append_restart_event(releases_dir: &Path, ts: u64, window_secs: u64) -> io::Result<()> {
    fs::create_dir_all(releases_dir)?;
    let mut log = read_restart_log(releases_dir);
    log.events.push(ts);
    let cutoff = ts.saturating_sub(window_secs);
    log.events.retain(|t| *t >= cutoff);
    log.events.sort_unstable();
    let json =
        serde_json::to_vec(&log).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let target = restart_log_path(releases_dir);
    let staging = path_with_suffix(&target, NEW_SUFFIX);
    if staging.exists() {
        fs::remove_file(&staging)?;
    }
    fs::write(&staging, &json)?;
    fs::rename(&staging, &target)?;
    Ok(())
}

/// Decide whether a restart is allowed under the current
/// rolling-window policy.
///
/// `now` is unix-seconds, `window_secs` is the rolling window
/// width, `max_in_window` is the cap. A `0` cap disables the
/// gate (always allow); use that when the operator explicitly
/// turns rate-limiting off.
///
/// Pure function — no I/O, easily unit-testable.
#[must_use]
pub fn restart_allowed(log: &RestartLog, now: u64, window_secs: u64, max_in_window: u32) -> bool {
    if max_in_window == 0 {
        return true;
    }
    let cutoff = now.saturating_sub(window_secs);
    let in_window = log.events.iter().filter(|t| **t >= cutoff).count();
    (in_window as u32) < max_in_window
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt as _;

    fn tempdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "aii-release-install-test-{}-{}",
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
    fn install_replaces_target_with_staged_bytes() {
        let dir = tempdir();
        let staged = dir.join("v2");
        let target = dir.join("aiid");
        fs::write(&staged, b"NEW BINARY BYTES").unwrap();
        fs::write(&target, b"OLD").unwrap();
        let installed = install_binary(&staged, &target).unwrap();
        assert_eq!(installed, target);
        assert_eq!(fs::read(&target).unwrap(), b"NEW BINARY BYTES");
    }

    #[test]
    fn installed_binary_is_executable() {
        let dir = tempdir();
        let staged = dir.join("v");
        let target = dir.join("aiid");
        // Stage a non-executable source so we can prove
        // install_binary sets +x explicitly.
        fs::write(&staged, b"#!/bin/sh\necho hi\n").unwrap();
        let mut perm = fs::metadata(&staged).unwrap().permissions();
        perm.set_mode(0o600);
        fs::set_permissions(&staged, perm).unwrap();

        install_binary(&staged, &target).unwrap();

        let installed_mode = fs::metadata(&target).unwrap().mode() & 0o777;
        assert_eq!(
            installed_mode, INSTALLED_MODE,
            "installed binary must end up at exactly 0o755, got {installed_mode:o}"
        );
    }

    #[test]
    fn install_creates_target_if_missing() {
        let dir = tempdir();
        let staged = dir.join("v");
        let target = dir.join("aiid_first_install");
        fs::write(&staged, b"hello world").unwrap();
        assert!(!target.exists());
        install_binary(&staged, &target).unwrap();
        assert!(target.exists());
        assert_eq!(fs::read(&target).unwrap(), b"hello world");
    }

    #[test]
    fn install_cleans_up_stale_new_suffix() {
        let dir = tempdir();
        let staged = dir.join("v");
        let target = dir.join("aiid");
        let stale_new = path_with_suffix(&target, NEW_SUFFIX);
        fs::write(&staged, b"NEW").unwrap();
        fs::write(&target, b"OLD").unwrap();
        fs::write(&stale_new, b"STALE LEFTOVER FROM CRASH").unwrap();
        install_binary(&staged, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"NEW");
        assert!(
            !stale_new.exists(),
            "atomic rename should consume the .new staging file"
        );
    }

    #[test]
    fn install_propagates_missing_source_error() {
        let dir = tempdir();
        let staged = dir.join("does-not-exist");
        let target = dir.join("aiid");
        let err = install_binary(&staged, &target).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(!target.exists(), "no partial target on failure");
    }

    #[test]
    fn current_aiid_path_returns_test_runner() {
        // current_exe() in the test process points at the test
        // runner binary, which always exists.
        let p = current_aiid_path().unwrap();
        assert!(p.is_absolute(), "current_exe must be absolute, got {p:?}");
        assert!(p.exists(), "current_exe must exist at {p:?}");
    }

    #[test]
    fn save_previous_writes_atomic_executable_snapshot() {
        let dir = tempdir();
        let releases = dir.join("releases");
        let current = dir.join("aiid");
        fs::write(&current, b"RUNNING BYTES").unwrap();
        let snap = save_previous(&current, &releases).unwrap();
        assert_eq!(snap, releases.join(PREVIOUS_NAME));
        assert!(snap.exists());
        assert_eq!(fs::read(&snap).unwrap(), b"RUNNING BYTES");
        let mode = fs::metadata(&snap).unwrap().mode() & 0o777;
        assert_eq!(mode, INSTALLED_MODE, "snapshot must be +x");
        // .new staging file must not survive.
        let staging = path_with_suffix(&snap, NEW_SUFFIX);
        assert!(!staging.exists());
    }

    #[test]
    fn save_previous_overwrites_existing_snapshot() {
        let dir = tempdir();
        let releases = dir.join("releases");
        let current = dir.join("aiid");
        fs::write(&current, b"FIRST").unwrap();
        save_previous(&current, &releases).unwrap();
        // Update current and re-snapshot — should replace.
        fs::write(&current, b"SECOND").unwrap();
        let snap = save_previous(&current, &releases).unwrap();
        assert_eq!(fs::read(&snap).unwrap(), b"SECOND");
    }

    #[test]
    fn rollback_swaps_target_with_previous() {
        let dir = tempdir();
        let releases = dir.join("releases");
        let target = dir.join("aiid");

        // Set up: target = "NEW", .previous = "OLD".
        fs::write(&target, b"NEW").unwrap();
        fs::create_dir_all(&releases).unwrap();
        fs::write(releases.join(PREVIOUS_NAME), b"OLD").unwrap();

        let installed = rollback_to_previous(&releases, &target).unwrap();
        assert_eq!(installed, target);
        assert_eq!(fs::read(&target).unwrap(), b"OLD", "target restored");
        // After rollback, .previous now holds the bytes we
        // rolled away from, so the operator can roll forward
        // again by calling rollback a second time.
        assert_eq!(
            fs::read(releases.join(PREVIOUS_NAME)).unwrap(),
            b"NEW",
            ".previous holds the rolled-away-from bytes"
        );
        // Holding file must be cleaned up.
        let holding = releases.join(format!("{PREVIOUS_NAME}.roll"));
        assert!(!holding.exists());
    }

    #[test]
    fn rollback_is_reversible_via_second_call() {
        let dir = tempdir();
        let releases = dir.join("releases");
        let target = dir.join("aiid");
        fs::write(&target, b"VERSION_B").unwrap();
        fs::create_dir_all(&releases).unwrap();
        fs::write(releases.join(PREVIOUS_NAME), b"VERSION_A").unwrap();

        // First rollback: B → A
        rollback_to_previous(&releases, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"VERSION_A");
        // Second rollback: A → B (because .previous now holds B)
        rollback_to_previous(&releases, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"VERSION_B");
    }

    #[test]
    fn rollback_returns_not_found_when_previous_missing() {
        let dir = tempdir();
        let releases = dir.join("releases");
        let target = dir.join("aiid");
        fs::write(&target, b"UNCHANGED").unwrap();
        fs::create_dir_all(&releases).unwrap();

        let err = rollback_to_previous(&releases, &target).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert_eq!(
            fs::read(&target).unwrap(),
            b"UNCHANGED",
            "target must not be touched when there's nothing to roll back to"
        );
    }

    #[test]
    fn boot_pending_write_then_read_round_trip() {
        let dir = tempdir();
        let releases = dir.join("releases");
        let record = BootPending {
            version: "0.0.85".into(),
            pre_install_head: 12345,
            install_ts: 1_900_000_085,
        };
        let path = write_boot_pending(&releases, &record).unwrap();
        assert_eq!(path, releases.join(BOOT_PENDING_NAME));
        assert!(path.exists());
        let got = read_boot_pending(&releases).unwrap().unwrap();
        assert_eq!(got, record);
    }

    #[test]
    fn boot_pending_read_missing_returns_none() {
        let dir = tempdir();
        let releases = dir.join("releases");
        fs::create_dir_all(&releases).unwrap();
        assert_eq!(read_boot_pending(&releases).unwrap(), None);
    }

    #[test]
    fn boot_pending_write_overwrites_prior_record() {
        let dir = tempdir();
        let releases = dir.join("releases");
        let first = BootPending {
            version: "0.0.85".into(),
            pre_install_head: 100,
            install_ts: 100,
        };
        let second = BootPending {
            version: "0.0.86".into(),
            pre_install_head: 200,
            install_ts: 200,
        };
        write_boot_pending(&releases, &first).unwrap();
        write_boot_pending(&releases, &second).unwrap();
        let got = read_boot_pending(&releases).unwrap().unwrap();
        assert_eq!(got, second);
    }

    #[test]
    fn boot_pending_clear_is_idempotent() {
        let dir = tempdir();
        let releases = dir.join("releases");
        fs::create_dir_all(&releases).unwrap();
        // Clearing a non-existent file succeeds.
        clear_boot_pending(&releases).unwrap();
        // After write + clear, the file is gone.
        write_boot_pending(
            &releases,
            &BootPending {
                version: "0.0.85".into(),
                pre_install_head: 1,
                install_ts: 1,
            },
        )
        .unwrap();
        assert!(boot_pending_path(&releases).exists());
        clear_boot_pending(&releases).unwrap();
        assert!(!boot_pending_path(&releases).exists());
        // A second clear is still a success.
        clear_boot_pending(&releases).unwrap();
    }

    #[test]
    fn boot_pending_read_rejects_garbage_with_invalid_data() {
        let dir = tempdir();
        let releases = dir.join("releases");
        fs::create_dir_all(&releases).unwrap();
        fs::write(boot_pending_path(&releases), b"not valid json").unwrap();
        let err = read_boot_pending(&releases).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    // ───────────────── v0.0.86: restart rate-limit tests ─────────────────

    #[test]
    fn restart_allowed_with_empty_log() {
        let log = RestartLog::default();
        assert!(restart_allowed(&log, 1000, 60, 3));
    }

    #[test]
    fn restart_allowed_within_cap() {
        let log = RestartLog {
            events: vec![900, 950],
        };
        assert!(restart_allowed(&log, 1000, 600, 3));
    }

    #[test]
    fn restart_blocked_when_window_full() {
        let log = RestartLog {
            events: vec![900, 950, 980],
        };
        // Three events in trailing 600 s, cap 3 → blocked.
        assert!(!restart_allowed(&log, 1000, 600, 3));
    }

    #[test]
    fn restart_allowed_after_window_rolls_off() {
        let log = RestartLog {
            events: vec![10, 20, 30],
        };
        // Now is 1000, window 600 ⇒ cutoff 400. All three are
        // outside the window.
        assert!(restart_allowed(&log, 1000, 600, 3));
    }

    #[test]
    fn restart_max_in_window_zero_disables_gate() {
        let log = RestartLog {
            events: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
        };
        // Cap 0 = disabled, always allow even with a flooded log.
        assert!(restart_allowed(&log, 100, 1000, 0));
    }

    #[test]
    fn append_then_read_round_trip_with_prune() {
        let dir = tempdir();
        let releases = dir.join("releases");
        append_restart_event(&releases, 100, 60).unwrap();
        append_restart_event(&releases, 130, 60).unwrap();
        append_restart_event(&releases, 200, 60).unwrap();
        // At ts=200 with window=60, cutoff=140. Only 200 should
        // remain after pruning by the final append.
        let log = read_restart_log(&releases);
        assert_eq!(log.events, vec![200]);
    }

    #[test]
    fn read_restart_log_missing_returns_default() {
        let dir = tempdir();
        let releases = dir.join("releases");
        let log = read_restart_log(&releases);
        assert!(log.events.is_empty());
    }

    #[test]
    fn read_restart_log_garbage_returns_default_not_panic() {
        let dir = tempdir();
        let releases = dir.join("releases");
        fs::create_dir_all(&releases).unwrap();
        fs::write(restart_log_path(&releases), b"not valid json").unwrap();
        let log = read_restart_log(&releases);
        // Permissive read: corrupt log treated as no events,
        // so the first auto-restart still gets its chance.
        assert!(log.events.is_empty());
    }
}
