//! Periodic poll for release manifests on update-peers (v0.0.81).
//!
//! Closes the late-joiner gap: a node that was offline during the
//! v0.0.77 manifest gossip wave currently has no way to discover
//! a release was made. This module ships a small tokio task that
//! periodically asks every `--update-peers` URL for its
//! `aii_latestRelease`, and — when a peer reports something
//! strictly newer than the local view AND the manifest's
//! signature verifies against the pinned project pubkey — drives
//! the same `record_release_announcement` + binary pull path the
//! gossip flow uses.
//!
//! Trust boundary: identical to the announce / gossip paths.
//! The poller does not trust the peer it polled; it re-verifies
//! the Ed25519 signature locally before any state mutation.
//!
//! ### Loop shape
//!
//! [`start_release_poller`] spawns a tokio task that:
//! 1. Skips the first immediate `tick()` (avoids hammering peers
//!    on a fast restart loop).
//! 2. Every `interval`, calls [`poll_once`].
//!
//! [`poll_once`] is the unit testable bit: it returns a
//! [`PollOutcome`] describing what each peer reported and what
//! the local node did about it. The spawned loop ignores the
//! outcome (best-effort), but tests assert on it.

use std::sync::Arc;
use std::time::Duration;

use aii_crypto::release::{pinned_release_pubkey, verify_manifest_signature, ReleaseManifest};
use jsonrpsee::core::client::ClientT;
use jsonrpsee::http_client::HttpClientBuilder;
use jsonrpsee::rpc_params;

use crate::{ReleaseManifestView, RpcState};

/// Per-peer outcome of one [`poll_once`] tick.
#[derive(Debug, Clone, Default)]
pub struct PeerPollOutcome {
    /// JSON-RPC URL the call targeted.
    pub peer: String,
    /// `true` if the peer reported a manifest strictly newer than
    /// our local view AND the signature verified locally AND
    /// `record_release_announcement` accepted it. Note: the peer
    /// can hold a manifest that was already accepted on this node
    /// via gossip — that case yields `accepted_manifest = false`
    /// (not strictly newer) which is the correct no-op.
    pub accepted_manifest: bool,
    /// `true` if a binary was missing locally and successfully
    /// pulled + imported from this peer on this tick.
    pub imported_binary: bool,
    /// Human-readable reason on failure / no-op. Empty on
    /// fully-successful catch-up.
    pub note: String,
}

/// Aggregate outcome of [`poll_once`].
#[derive(Debug, Clone, Default)]
pub struct PollOutcome {
    /// One entry per peer in the input list.
    pub peers: Vec<PeerPollOutcome>,
}

/// One pass over `peers`. Best-effort; never panics, never
/// bubbles errors — every failure is captured in the per-peer
/// `note` field.
///
/// Sequence per peer:
/// 1. `aii_latestRelease` to fetch the peer's view.
/// 2. Compare `(timestamp, version)` against our local
///    `state.latest_release()` snapshot. Stop early if not
///    strictly newer.
/// 3. Verify the Ed25519 signature against the pinned project
///    pubkey using [`verify_manifest_signature`]. Reject on
///    failure.
/// 4. `state.record_release_announcement(m)` — accepts iff the
///    host agrees this manifest is strictly newer.
/// 5. If the local binary store is missing this version, pull
///    via `aii_getReleaseBinary` and hand off to
///    `state.import_release_binary` (which re-verifies SHA-256
///    + may trigger v0.0.78 auto-install).
pub async fn poll_once<S>(state: Arc<S>, peers: &[String]) -> PollOutcome
where
    S: RpcState + ?Sized,
{
    let mut outcome = PollOutcome::default();
    let local_latest = state.latest_release().await;

    for peer in peers {
        let mut po = PeerPollOutcome {
            peer: peer.clone(),
            ..Default::default()
        };
        let client = match HttpClientBuilder::default()
            .max_request_size(crate::MAX_REQUEST_BODY_SIZE)
            .max_response_size(crate::MAX_RESPONSE_BODY_SIZE)
            .build(peer)
        {
            Ok(c) => c,
            Err(e) => {
                po.note = format!("client: {e}");
                outcome.peers.push(po);
                continue;
            }
        };

        // 1. Fetch the peer's latest manifest view.
        let view: Option<ReleaseManifestView> =
            match client.request("aii_latestRelease", rpc_params![]).await {
                Ok(v) => v,
                Err(e) => {
                    po.note = format!("latest rpc: {e}");
                    outcome.peers.push(po);
                    continue;
                }
            };
        let Some(view) = view else {
            po.note = "peer has no manifest".into();
            outcome.peers.push(po);
            continue;
        };
        let remote: ReleaseManifest = view.into();

        // 2. Strictly newer than local?
        if !is_strictly_newer(&remote, local_latest.as_ref()) {
            po.note = "not newer than local".into();
            outcome.peers.push(po);
            continue;
        }

        // 3. Verify signature locally — never trust the peer.
        let pk = pinned_release_pubkey();
        if let Err(e) = verify_manifest_signature(&pk, &remote) {
            po.note = format!("sig verify: {e}");
            outcome.peers.push(po);
            continue;
        }

        // 4. Hand the manifest to the host.
        let accepted = state.record_release_announcement(remote.clone()).await;
        po.accepted_manifest = accepted;
        if !accepted {
            // Host rejected (race: another path landed it first).
            // Fall through to the binary pull anyway — we may
            // still be missing bytes.
            po.note = "host rejected announcement (race?)".into();
        }

        // 5. Pull the binary if we don't have it.
        let have_local = state.release_binary_bytes(&remote.version).await.is_some();
        if !have_local {
            let bin_resp: Result<Option<String>, _> = client
                .request("aii_getReleaseBinary", rpc_params![remote.version.clone()])
                .await;
            match bin_resp {
                Ok(Some(hex_str)) => match hex::decode(hex_str.trim_start_matches("0x")) {
                    Ok(bytes) => {
                        let (ok, reason) =
                            state.import_release_binary(&remote.version, bytes).await;
                        if ok {
                            po.imported_binary = true;
                            if po.note.is_empty() {
                                po.note = String::new();
                            }
                        } else {
                            po.note = format!("import: {reason}");
                        }
                    }
                    Err(e) => {
                        po.note = format!("hex decode binary: {e}");
                    }
                },
                Ok(None) => {
                    if po.note.is_empty() {
                        po.note = "peer missing binary".into();
                    }
                }
                Err(e) => {
                    if po.note.is_empty() {
                        po.note = format!("get-binary rpc: {e}");
                    }
                }
            }
        }

        outcome.peers.push(po);
    }
    outcome
}

/// Spawn the periodic poller. Returns the `JoinHandle` so the
/// host can abort on shutdown (the typical aiid loop doesn't
/// abort and lets tokio drop the task on process exit).
///
/// Cadence: first poll fires AFTER `interval`, so a fast
/// restart loop doesn't slam every peer with cold-start traffic.
/// Pass `Duration::from_secs(0)` and the call is a no-op (the
/// host shouldn't spawn the poller in that mode).
#[must_use]
pub fn start_release_poller<S>(
    state: Arc<S>,
    peers: Vec<String>,
    interval: Duration,
) -> tokio::task::JoinHandle<()>
where
    S: RpcState + ?Sized,
{
    tokio::spawn(async move {
        if interval.is_zero() || peers.is_empty() {
            tracing::info!(
                interval_secs = interval.as_secs(),
                peers = peers.len(),
                "release poller no-op (interval=0 or no peers)",
            );
            return;
        }
        let mut tick = tokio::time::interval(interval);
        // Burn the first immediate tick.
        tick.tick().await;
        loop {
            tick.tick().await;
            let outcome = poll_once(state.clone(), &peers).await;
            let imported = outcome.peers.iter().filter(|p| p.imported_binary).count();
            let accepted = outcome.peers.iter().filter(|p| p.accepted_manifest).count();
            if imported > 0 || accepted > 0 {
                tracing::info!(
                    peers = outcome.peers.len(),
                    accepted_manifests = accepted,
                    imported_binaries = imported,
                    "release poll catch-up",
                );
            }
        }
    })
}

fn is_strictly_newer(remote: &ReleaseManifest, local: Option<&ReleaseManifest>) -> bool {
    match local {
        None => true,
        Some(l) => {
            remote.timestamp_unix > l.timestamp_unix
                || (remote.timestamp_unix == l.timestamp_unix && remote.version > l.version)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_manifest(version: &str, ts: u64) -> ReleaseManifest {
        ReleaseManifest {
            version: version.into(),
            sha256_hex: "00".repeat(32),
            timestamp_unix: ts,
            ed25519_sig_hex: "00".repeat(64),
        }
    }

    #[test]
    fn strictly_newer_handles_none_local() {
        assert!(is_strictly_newer(&dummy_manifest("0.0.81", 1), None));
    }

    #[test]
    fn strictly_newer_compares_timestamp_first() {
        let local = dummy_manifest("0.0.99", 100);
        let remote_older = dummy_manifest("0.0.99", 99);
        let remote_newer = dummy_manifest("0.0.81", 101);
        assert!(!is_strictly_newer(&remote_older, Some(&local)));
        assert!(is_strictly_newer(&remote_newer, Some(&local)));
    }

    #[test]
    fn strictly_newer_breaks_timestamp_tie_with_version() {
        let local = dummy_manifest("0.0.80", 100);
        let older_version = dummy_manifest("0.0.79", 100);
        let newer_version = dummy_manifest("0.0.81", 100);
        let equal_version = dummy_manifest("0.0.80", 100);
        assert!(!is_strictly_newer(&older_version, Some(&local)));
        assert!(is_strictly_newer(&newer_version, Some(&local)));
        assert!(
            !is_strictly_newer(&equal_version, Some(&local)),
            "equal pair is not strictly newer"
        );
    }
}
