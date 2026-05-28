//! Runtime head-stall watchdog (v0.0.84).
//!
//! The v0.0.69 cold-sync (`bootstrap_sync_from_peer`) and v0.0.83
//! implicit-bootnode fallback together let any node catch up at
//! **startup** as long as it has a peer URL to pull blocks from.
//! But a node that has been running fine for hours and then falls
//! behind (transient network partition, peer outage, BFT engine
//! state divergence) currently has no automatic recovery — the
//! BFT engine just waits forever for a proposal that already
//! reached quorum on its peers.
//!
//! This module ships a small tokio task that:
//!
//! 1. Polls `NodeState::head_block_number_sync()` every
//!    `--stall-poll-secs` (default 10 s).
//! 2. Tracks how long the head has been stuck at the same value.
//! 3. When `stalled_for_secs >= --stall-recover-secs`, calls
//!    [`aii_release_install::exec_self`] for a kernel-level
//!    same-PID restart. The new process image runs the standard
//!    startup path — which, via v0.0.83 implicit-bootnode
//!    fallback, automatically cold-syncs from the first
//!    `--update-peers` URL.
//!
//! Off by default; an operator opts in by setting
//! `--stall-recover-secs N`. Recommend `N` >= 5× the BFT slot
//! interval so single-slot hiccups don't trigger restarts.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::NodeState;

/// Read the current unix-seconds clock for rate-limit
/// accounting. Returns `0` if the system clock predates the
/// epoch (won't happen in practice; safe fallback).
fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Try to record a restart event under the rolling-window
/// policy (v0.0.86). Returns `true` if the action is allowed
/// (in which case the event is appended); `false` if the cap
/// would be exceeded (in which case nothing is written).
///
/// A `max_per_window == 0` cap disables the gate (always
/// allow). Use that when the operator explicitly turns
/// rate-limiting off.
fn try_register_restart(
    releases_dir: &std::path::Path,
    window_secs: u64,
    max_per_window: u32,
) -> bool {
    let log = crate::release_install::read_restart_log(releases_dir);
    let now = now_unix_secs();
    if !crate::release_install::restart_allowed(&log, now, window_secs, max_per_window) {
        return false;
    }
    if let Err(e) = crate::release_install::append_restart_event(releases_dir, now, window_secs) {
        tracing::warn!(
            error = %e,
            "restart-log append failed; proceeding with action anyway (one less data point next time)",
        );
    }
    true
}

/// Outcome of one [`StallDetector::observe`] tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StallStatus {
    /// Head advanced since the previous tick — counters reset.
    Healthy {
        /// The new head height.
        head: u64,
    },
    /// Head did not advance, but we haven't hit the recovery
    /// threshold yet. `stalled_for_secs` is how long the head
    /// has been at its current value.
    StalledBelowThreshold {
        /// The stuck head height.
        head: u64,
        /// Seconds since head last changed.
        stalled_for_secs: u64,
    },
    /// Head has been stuck >= `stall_recover_secs` — caller
    /// should trigger the recovery action (exec_self).
    StallTriggered {
        /// The stuck head height.
        head: u64,
        /// Seconds since head last changed.
        stalled_for_secs: u64,
    },
}

/// In-memory state machine for the runtime stall watchdog.
///
/// Pure logic — no I/O, no exec. The async task wraps this
/// with the actual sleep + exec.
#[derive(Debug)]
pub struct StallDetector {
    last_head: u64,
    stalled_for_secs: u64,
    stall_recover_secs: u64,
    poll_secs: u64,
    /// Set to `true` after the first observation, so the
    /// initial poll doesn't immediately fire on `head == 0`
    /// during early startup.
    seeded: bool,
}

impl StallDetector {
    /// Construct a detector. `stall_recover_secs` is the
    /// threshold; `poll_secs` is how often the wrapping task
    /// will call [`Self::observe`].
    #[must_use]
    pub const fn new(stall_recover_secs: u64, poll_secs: u64) -> Self {
        Self {
            last_head: 0,
            stalled_for_secs: 0,
            stall_recover_secs,
            poll_secs,
            seeded: false,
        }
    }

    /// Record an observation of the current head and return what
    /// the caller should do.
    ///
    /// Behavior:
    /// - First call: seeds `last_head` and reports `Healthy`.
    /// - Subsequent calls where head advanced: reset stall
    ///   counter, report `Healthy`.
    /// - Subsequent calls where head did NOT advance: bump stall
    ///   counter by `poll_secs`; report `StalledBelowThreshold`
    ///   until it crosses `stall_recover_secs`, then
    ///   `StallTriggered`.
    pub const fn observe(&mut self, current_head: u64) -> StallStatus {
        // Defensive: a detector constructed with
        // stall_recover_secs == 0 must NEVER trigger. The
        // wrapping task short-circuits on this input, but the
        // detector should be safe on its own too.
        if self.stall_recover_secs == 0 {
            self.last_head = current_head;
            self.stalled_for_secs = 0;
            self.seeded = true;
            return StallStatus::Healthy { head: current_head };
        }
        if !self.seeded {
            self.seeded = true;
            self.last_head = current_head;
            self.stalled_for_secs = 0;
            return StallStatus::Healthy { head: current_head };
        }
        if current_head != self.last_head {
            self.last_head = current_head;
            self.stalled_for_secs = 0;
            return StallStatus::Healthy { head: current_head };
        }
        self.stalled_for_secs = self.stalled_for_secs.saturating_add(self.poll_secs);
        if self.stalled_for_secs >= self.stall_recover_secs {
            StallStatus::StallTriggered {
                head: current_head,
                stalled_for_secs: self.stalled_for_secs,
            }
        } else {
            StallStatus::StalledBelowThreshold {
                head: current_head,
                stalled_for_secs: self.stalled_for_secs,
            }
        }
    }
}

/// Spawn the watchdog task. Returns the `JoinHandle` so the
/// host can abort on shutdown.
///
/// Cadence: first poll fires `poll_secs` AFTER spawn (matches
/// the v0.0.81 release poller's "burn the immediate tick"
/// pattern — keeps a fast restart loop from immediately
/// re-triggering).
///
/// Spawning with `stall_recover_secs == 0` is a no-op (the
/// task logs and returns immediately). Same for an `Arc<NodeState>`
/// that never advances — the caller is expected to gate this on
/// "BFT or PoA producer is enabled".
#[cfg(unix)]
#[must_use]
pub fn start_head_watchdog(
    state: Arc<NodeState>,
    stall_recover_secs: u64,
    poll_secs: u64,
    releases_dir: PathBuf,
    restart_window_secs: u64,
    restart_max_per_window: u32,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if stall_recover_secs == 0 || poll_secs == 0 {
            tracing::info!(
                stall_recover_secs,
                poll_secs,
                "head watchdog no-op (disabled by config)",
            );
            return;
        }
        tracing::info!(
            stall_recover_secs,
            poll_secs,
            restart_window_secs,
            restart_max_per_window,
            "head watchdog armed",
        );
        let mut detector = StallDetector::new(stall_recover_secs, poll_secs);
        let mut tick = tokio::time::interval(Duration::from_secs(poll_secs));
        tick.tick().await; // burn immediate
        loop {
            tick.tick().await;
            let head = state.head_block_number_sync();
            match detector.observe(head) {
                StallStatus::Healthy { .. } => {}
                StallStatus::StalledBelowThreshold {
                    head,
                    stalled_for_secs,
                } => {
                    tracing::warn!(
                        head,
                        stalled_for_secs,
                        stall_recover_secs,
                        "head stalled (below recovery threshold)",
                    );
                }
                StallStatus::StallTriggered {
                    head,
                    stalled_for_secs,
                } => {
                    // v0.0.86: check the rolling-window
                    // rate limit before triggering exec_self.
                    // Prevents crash-loops where the
                    // restarted binary stalls again
                    // immediately.
                    if !try_register_restart(
                        &releases_dir,
                        restart_window_secs,
                        restart_max_per_window,
                    ) {
                        tracing::error!(
                            head,
                            stalled_for_secs,
                            restart_window_secs,
                            restart_max_per_window,
                            "head stall threshold crossed BUT restart rate limit exceeded; refusing exec_self (operator must intervene)",
                        );
                        // Stay on the current binary. Reset
                        // detector so we don't immediately
                        // re-fire on the next tick.
                        detector = StallDetector::new(stall_recover_secs, poll_secs);
                        continue;
                    }
                    tracing::error!(
                        head,
                        stalled_for_secs,
                        "head stall threshold crossed; exec'ing self to trigger cold-sync recovery",
                    );
                    let err = crate::release_install::exec_self();
                    tracing::error!(
                        error = %err,
                        "exec_self failed; watchdog cannot recover automatically",
                    );
                    // If exec_self returned, we're still alive
                    // on the old binary. Reset the detector so
                    // the next stall window starts fresh, rather
                    // than continuously re-exec'ing.
                    detector = StallDetector::new(stall_recover_secs, poll_secs);
                }
            }
        }
    })
}

// ─────────────────────────── v0.0.85: boot-health ───────────────────────────

/// Spawn the boot-health confirm task (v0.0.85).
///
/// Pairs with the boot-pending sentinel written by
/// [`crate::release_install::write_boot_pending`]. The new
/// process — booted via `execve` after an install — calls this
/// once at startup. The task then:
///
/// 1. Reads `<data-dir>/releases/.boot-pending`. Returns
///    immediately if the sentinel is missing (no install
///    happened — this is a normal startup).
/// 2. Sleeps `confirm_secs` (configurable via
///    `--boot-health-secs`).
/// 3. Reads the head block number. If it advanced past
///    `pending.pre_install_head`, the new binary is healthy:
///    clear the sentinel and exit.
/// 4. If the head did NOT advance, the new binary failed to
///    rejoin consensus. Log ERROR and call
///    `RpcState::rollback_release` to restore `.previous` +
///    execve into it. The restored binary's startup will then
///    pass through this same function — if it's also unhealthy
///    we'd loop, but that's outside this slice's scope (rate
///    limiting lands in v0.0.86+).
///
/// Off by default; spawning with `confirm_secs == 0` is a
/// no-op (the task logs and returns).
#[cfg(unix)]
#[must_use]
pub fn start_boot_health_confirm(
    state: Arc<NodeState>,
    releases_dir: std::path::PathBuf,
    confirm_secs: u64,
    restart_window_secs: u64,
    restart_max_per_window: u32,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if confirm_secs == 0 {
            tracing::info!("boot-health confirm no-op (disabled by config)");
            return;
        }
        let pending = match crate::release_install::read_boot_pending(&releases_dir) {
            Ok(Some(p)) => p,
            Ok(None) => {
                // No install in flight; this is just a normal startup.
                return;
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not read .boot-pending sentinel; skipping boot-health confirm");
                return;
            }
        };

        // v0.0.86 stale-sentinel shortcut: if the sentinel's
        // install_ts is older than 2× confirm_secs, a previous
        // boot of this binary likely crashed before clearing
        // it. Skip the sleep and go straight to rollback —
        // waiting another full confirm window won't change the
        // outcome (no head movement is going to happen) and
        // delays operator notification.
        let now = now_unix_secs();
        let stale_threshold = confirm_secs.saturating_mul(2);
        let sentinel_age = now.saturating_sub(pending.install_ts);
        let is_stale = sentinel_age >= stale_threshold;
        if is_stale {
            tracing::error!(
                sentinel_age_secs = sentinel_age,
                stale_threshold_secs = stale_threshold,
                version = %pending.version,
                "boot-pending sentinel is stale (previous boot crashed before confirm); rolling back immediately",
            );
        } else {
            tracing::info!(
                version = %pending.version,
                pre_install_head = pending.pre_install_head,
                install_ts = pending.install_ts,
                confirm_secs,
                "boot-health confirm armed; will check head advancement after grace window",
            );
            tokio::time::sleep(Duration::from_secs(confirm_secs)).await;
            let head = state.head_block_number_sync();
            if head > pending.pre_install_head {
                tracing::info!(
                    head,
                    pre_install_head = pending.pre_install_head,
                    version = %pending.version,
                    "boot-health confirm: head advanced; clearing .boot-pending",
                );
                if let Err(e) = crate::release_install::clear_boot_pending(&releases_dir) {
                    tracing::warn!(error = %e, "boot-health confirm: clear failed (file may linger)");
                }
                return;
            }
            tracing::error!(
                head,
                pre_install_head = pending.pre_install_head,
                version = %pending.version,
                "boot-health confirm: head did NOT advance within {confirm_secs}s; triggering rollback",
            );
        }

        // v0.0.86 rate-limit gate: stale shortcut and normal
        // unhealthy path both funnel through here. If the
        // rolling window is full, refuse to roll back — the
        // operator needs to investigate why we keep hitting
        // this path.
        if !try_register_restart(&releases_dir, restart_window_secs, restart_max_per_window) {
            tracing::error!(
                restart_window_secs,
                restart_max_per_window,
                "boot-health rollback BUT restart rate limit exceeded; refusing rollback (operator must intervene)",
            );
            return;
        }

        // Trigger rollback via the trait method so the path is
        // exactly the same one operators get via the
        // `aii_rollbackRelease` RPC.
        let outcome = <NodeState as aii_rpc::RpcState>::rollback_release(&state).await;
        if outcome.scheduled {
            tracing::error!(
                version = %pending.version,
                restart_in_secs = outcome.restart_in_secs,
                "boot-health confirm: rollback scheduled; node will restart shortly",
            );
            // Leave .boot-pending in place — the rolled-back
            // binary's next startup will see it, but since
            // the rollback restored the previous bytes,
            // head advancement should succeed and the
            // sentinel will be cleared on that pass.
            // (If you'd rather not loop the watchdog on the
            // restored binary, clear it here. We choose
            // observability over a clean slate.)
        } else {
            tracing::error!(
                reason = %outcome.reason,
                "boot-health confirm: rollback rejected; manual intervention required",
            );
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_observation_seeds_and_is_healthy() {
        let mut d = StallDetector::new(30, 10);
        assert_eq!(d.observe(42), StallStatus::Healthy { head: 42 });
    }

    #[test]
    fn advancing_head_resets_stall_counter() {
        let mut d = StallDetector::new(30, 10);
        let _ = d.observe(100);
        let _ = d.observe(100); // stalled 10s
        let _ = d.observe(101); // moved
        let s = d.observe(101); // stalled 10s again (FRESH counter)
        assert_eq!(
            s,
            StallStatus::StalledBelowThreshold {
                head: 101,
                stalled_for_secs: 10,
            }
        );
    }

    #[test]
    fn stall_crosses_threshold_after_enough_ticks() {
        let mut d = StallDetector::new(30, 10);
        let _ = d.observe(50); // seed, healthy
        let s1 = d.observe(50);
        assert!(matches!(
            s1,
            StallStatus::StalledBelowThreshold {
                stalled_for_secs: 10,
                ..
            }
        ));
        let s2 = d.observe(50);
        assert!(matches!(
            s2,
            StallStatus::StalledBelowThreshold {
                stalled_for_secs: 20,
                ..
            }
        ));
        let s3 = d.observe(50);
        assert!(matches!(
            s3,
            StallStatus::StallTriggered {
                stalled_for_secs: 30,
                ..
            }
        ));
    }

    #[test]
    fn stall_stays_triggered_until_head_moves() {
        let mut d = StallDetector::new(20, 10);
        let _ = d.observe(7);
        let _ = d.observe(7);
        let _ = d.observe(7);
        let triggered = d.observe(7);
        assert!(matches!(triggered, StallStatus::StallTriggered { .. }));
        // Same head again — still triggered (caller is
        // responsible for resetting via re-construction after
        // taking action).
        let still = d.observe(7);
        assert!(matches!(still, StallStatus::StallTriggered { .. }));
    }

    #[test]
    fn head_advance_after_trigger_returns_to_healthy() {
        let mut d = StallDetector::new(20, 10);
        let _ = d.observe(7);
        let _ = d.observe(7);
        let _ = d.observe(7); // triggered
        let healed = d.observe(8);
        assert_eq!(healed, StallStatus::Healthy { head: 8 });
    }

    #[test]
    fn stall_recover_secs_zero_never_triggers() {
        let mut d = StallDetector::new(0, 10);
        let _ = d.observe(7);
        for _ in 0..50 {
            let s = d.observe(7);
            assert_eq!(s, StallStatus::Healthy { head: 7 });
        }
    }

    #[test]
    fn poll_secs_zero_means_stall_secs_never_advances() {
        // Edge case: a misconfigured detector with poll_secs=0
        // shouldn't accidentally accumulate stall time on every
        // tick. (The wrapping task short-circuits on
        // poll_secs==0; this test asserts the detector itself
        // is safe under that input.)
        let mut d = StallDetector::new(30, 0);
        let _ = d.observe(99);
        for _ in 0..100 {
            let s = d.observe(99);
            assert_eq!(
                s,
                StallStatus::StalledBelowThreshold {
                    head: 99,
                    stalled_for_secs: 0,
                },
                "stall counter must not move with poll_secs=0"
            );
        }
    }
}
