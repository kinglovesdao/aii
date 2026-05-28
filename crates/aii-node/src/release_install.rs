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
}
