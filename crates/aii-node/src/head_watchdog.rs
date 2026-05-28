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

use std::sync::Arc;
use std::time::Duration;

use crate::NodeState;

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
        tracing::info!(stall_recover_secs, poll_secs, "head watchdog armed",);
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
