//! Cross-node propagation for verified release manifests (v0.0.77).
//!
//! Pairs with [`crate::release_store`] (binary cache) and the v0.0.75
//! `aii_announceRelease` / `aii_latestRelease` RPC endpoints. Owns
//! the *outbound* half:
//!
//! 1. **Announcement flood** — every node that accepts a new manifest
//!    via `aii_announceRelease` re-broadcasts that same manifest to
//!    every configured update-peer (`aiid --update-peers`). Receivers
//!    re-verify against their own pinned pubkey, so the manifest
//!    cannot mutate in transit. Duplicate manifests get rejected at
//!    the receiver with `accepted: false` (their
//!    `record_release_announcement` rule requires strictly-newer
//!    `(timestamp, version)`), so the flood terminates within one
//!    hop per peer link.
//!
//! 2. **Binary auto-fetch** — if the receiving node accepted a
//!    manifest but doesn't have a binary for that version cached,
//!    it polls each peer's `aii_getReleaseBinary(version)` in
//!    sequence. On the first non-null response, it pipes the hex
//!    bytes back through its own `aii_importReleaseBinary` path,
//!    which re-hashes against the manifest before persisting.
//!
//! v0.0.77 ships only the outbound propagation. Atomic install +
//! self-restart land in v0.0.78+ — the bytes that make it to
//! `<data-dir>/releases/<version>` are still inert until an
//! operator (or a future release) actually executes them.

use std::sync::Arc;

use aii_crypto::release::ReleaseManifest;
use jsonrpsee::core::client::ClientT;
use jsonrpsee::http_client::HttpClientBuilder;
use jsonrpsee::rpc_params;
use serde::Deserialize;

use crate::{AnnounceReleaseResult, ReleaseManifestView, RpcState};

/// Per-peer outcome of a propagate run.
#[derive(Debug, Clone)]
pub struct PeerOutcome {
    /// JSON-RPC URL the call targeted.
    pub peer: String,
    /// `true` if the peer responded `accepted: true` to our
    /// `aii_announceRelease` call.
    pub announce_accepted: bool,
    /// `true` if we successfully fetched the binary from this peer
    /// and imported it locally. `false` either means we already
    /// had the binary, didn't need to fetch from this peer, or the
    /// fetch+import failed.
    pub binary_imported: bool,
    /// Human-readable reason for any failure on this peer (empty
    /// on success).
    pub note: String,
}

/// Aggregate outcome of [`propagate_release`].
#[derive(Debug, Clone, Default)]
pub struct PropagateOutcome {
    /// One entry per peer the call attempted.
    pub peers: Vec<PeerOutcome>,
}

/// Best-effort cross-node release propagation.
///
/// Announces `manifest` to every peer, then either pushes the
/// binary (if local has it) or pulls it (if local needs it).
/// Returns a per-peer outcome breakdown for logging /
/// observability. Never panics, never bubbles errors — every
/// failure is captured in the outcome's `note` field.
///
/// Designed to be spawned as a fire-and-forget tokio task from
/// inside `RpcState::record_release_announcement`'s success path:
///
/// ```ignore
/// tokio::spawn(async move {
///     let outcome = propagate_release(state, manifest, peers).await;
///     tracing::info!(?outcome, "release propagation done");
/// });
/// ```
#[allow(clippy::too_many_lines)]
pub async fn propagate_release<S>(
    state: Arc<S>,
    manifest: ReleaseManifest,
    peers: Vec<String>,
) -> PropagateOutcome
where
    S: RpcState + ?Sized,
{
    let view: ReleaseManifestView = manifest.clone().into();
    let mut outcome = PropagateOutcome::default();

    // Two-direction propagation:
    //   - If we HAVE the binary locally, push it to peers that don't.
    //   - If we DON'T have the binary, pull from the first peer that does.
    let local_binary = state.release_binary_bytes(&manifest.version).await;
    let mut still_need_binary = local_binary.is_none();

    for peer_url in peers {
        let mut po = PeerOutcome {
            peer: peer_url.clone(),
            announce_accepted: false,
            binary_imported: false,
            note: String::new(),
        };
        let client = match HttpClientBuilder::default()
            .max_request_size(crate::MAX_REQUEST_BODY_SIZE)
            .max_response_size(crate::MAX_RESPONSE_BODY_SIZE)
            .build(&peer_url)
        {
            Ok(c) => c,
            Err(e) => {
                po.note = format!("client: {e}");
                outcome.peers.push(po);
                continue;
            }
        };
        // 1. Forward the announcement. Receiver re-verifies the
        //    signature locally — this hop carries no extra trust.
        match client
            .request::<AnnounceReleaseResult, _>("aii_announceRelease", rpc_params![view.clone()])
            .await
        {
            Ok(r) => {
                po.announce_accepted = r.accepted;
                if !r.accepted && !r.note_or_reason().is_empty() {
                    po.note = format!("announce: {}", r.note_or_reason());
                }
            }
            Err(e) => {
                po.note = format!("announce rpc: {e}");
            }
        }

        // 2. Binary direction:
        //
        //    a. If we have the binary AND the peer doesn't, push it.
        //    b. If we don't have the binary AND the peer does, pull.
        if let Some(local_bytes) = local_binary.as_ref() {
            // Push mode — check what the peer has.
            let peer_has = client
                .request::<Option<String>, _>(
                    "aii_getReleaseBinary",
                    rpc_params![manifest.version.clone()],
                )
                .await;
            match peer_has {
                Ok(Some(_)) => {
                    // Peer already has it — nothing to push.
                }
                Ok(None) => {
                    // Peer is missing — push.
                    let hex_bytes = format!("0x{}", hex::encode(local_bytes));
                    let push: Result<crate::ImportReleaseResult, _> = client
                        .request(
                            "aii_importReleaseBinary",
                            rpc_params![manifest.version.clone(), hex_bytes],
                        )
                        .await;
                    match push {
                        Ok(r) if r.accepted => {
                            po.binary_imported = true;
                        }
                        Ok(r) => {
                            if po.note.is_empty() {
                                po.note = format!("push import: {}", r.reason);
                            }
                        }
                        Err(e) => {
                            if po.note.is_empty() {
                                po.note = format!("push rpc: {e}");
                            }
                        }
                    }
                }
                Err(e) => {
                    if po.note.is_empty() {
                        po.note = format!("probe rpc: {e}");
                    }
                }
            }
        } else if still_need_binary {
            // Pull mode — fetch from this peer if it has it.
            match client
                .request::<Option<String>, _>(
                    "aii_getReleaseBinary",
                    rpc_params![manifest.version.clone()],
                )
                .await
            {
                Ok(Some(hex_bytes)) => {
                    let bytes = match hex::decode(hex_bytes.trim_start_matches("0x")) {
                        Ok(b) => b,
                        Err(e) => {
                            po.note = format!("hex decode binary: {e}");
                            outcome.peers.push(po);
                            continue;
                        }
                    };
                    let (ok, reason) = state.import_release_binary(&manifest.version, bytes).await;
                    if ok {
                        po.binary_imported = true;
                        still_need_binary = false;
                    } else if po.note.is_empty() {
                        po.note = format!("import: {reason}");
                    }
                }
                Ok(None) => {
                    // peer doesn't have the binary — try the next.
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

/// Helper that flattens the v0.0.75 `AnnounceReleaseResult`'s
/// `reason` into a single `&str` for log messages. Keeps the
/// propagate fn agnostic to upstream wire shape changes.
trait ReasonFlatten {
    fn note_or_reason(&self) -> &str;
}

impl ReasonFlatten for AnnounceReleaseResult {
    fn note_or_reason(&self) -> &str {
        &self.reason
    }
}

/// Stable HTTP-RPC peer list shared between [`propagate_release`]
/// and the CLI / node startup. Wrap in
/// [`std::sync::RwLock`] so the host can publish/refresh at
/// runtime.
#[must_use]
pub fn parse_update_peers(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.starts_with("http://") || s.starts_with("https://") {
                s.to_string()
            } else {
                format!("http://{s}")
            }
        })
        .collect()
}

/// Raw deserializer for `aii_getReleaseBinary` JSON envelopes.
///
/// The public typed call path above is preferred; this struct
/// exists for downstream callers that want to parse raw bytes
/// from a non-jsonrpsee transport (curl + serde, etc.).
#[derive(Debug, Deserialize)]
pub struct ReleaseBinaryReply {
    /// `0x…` hex of the release bytes, or `None` if the peer has
    /// no cached copy for the requested version.
    pub result: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_update_peers_splits_and_normalises() {
        let v = parse_update_peers("http://a:8545 , b:8545,,c.example:8545");
        assert_eq!(v.len(), 3);
        assert_eq!(v[0], "http://a:8545");
        assert_eq!(v[1], "http://b:8545");
        assert_eq!(v[2], "http://c.example:8545");
    }

    #[test]
    fn parse_update_peers_keeps_https() {
        let v = parse_update_peers("https://rpc.example.com");
        assert_eq!(v[0], "https://rpc.example.com");
    }

    #[test]
    fn parse_update_peers_empty_returns_empty() {
        assert!(parse_update_peers("").is_empty());
        assert!(parse_update_peers(" , ,").is_empty());
    }
}
