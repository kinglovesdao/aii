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

/// `execve(2)` the current process with its original args, env,
/// and CWD, replacing the running image with whatever binary now
/// lives at [`current_aiid_path`].
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
/// # Errors
///
/// `execve` failure: missing binary, non-executable file,
/// ENOMEM, etc.
pub fn exec_self() -> io::Error {
    use std::os::unix::process::CommandExt as _;
    use std::process::Command;
    let exe = match current_aiid_path() {
        Ok(p) => p,
        Err(e) => return e,
    };
    let mut args = std::env::args_os();
    args.next();
    let mut cmd = Command::new(&exe);
    cmd.args(args);
    cmd.exec()
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
}
