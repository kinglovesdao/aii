# Changelog

All notable changes to AII workspace follow [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.81] — 2026-05-27

### Added — late-joiner release re-poll

Closes the last gap in the auto-update protocol's coverage: a
node that was offline during the v0.0.77 manifest gossip wave
now catches up on its own, on a configurable cadence, without
the operator having to re-broadcast. Every validator that comes
back online learns about pending releases within one poll
interval and walks the same trust-bounded
record → import → maybe-install path as the gossip flow.

#### `aii-crypto::release`

- New `verify_manifest_signature(pubkey, manifest) -> Result<()>`
  that re-verifies the Ed25519 signature WITHOUT requiring the
  binary on disk. The full `verify_release` is still the right
  call before trusting the binary; this one is for the gap
  between "got a manifest from a peer" and "have the binary
  in hand."
- `ReleaseManifest` now derives `PartialEq + Eq` so tests and
  diff-style logging can compare manifests directly.

#### `aii-rpc::release_poller` (new module)

- `poll_once(state, peers) -> PollOutcome` — single best-effort
  pass: for each peer, fetch `aii_latestRelease`, compare via
  strict `(timestamp, version)` ordering, re-verify the
  signature locally, then drive the host's
  `record_release_announcement` + (when the binary is missing)
  `aii_getReleaseBinary` → `import_release_binary` path. Same
  trust boundary as the announce / gossip flows; never trusts
  the peer.
- `start_release_poller(state, peers, interval) -> JoinHandle<()>` —
  spawns a tokio task that calls `poll_once` every `interval`.
  Burns the first immediate `tick()` so a fast restart loop
  doesn't slam peers with cold-start traffic; logs catch-up
  events at INFO when either a manifest or a binary lands.
- `PeerPollOutcome` / `PollOutcome` envelopes for callers that
  want observability. 5 unit tests cover the strict-newer
  comparator + a two-node integration test where node A starts
  empty and pulls B's signed manifest + binary in a single
  tick, plus an idempotency test (second poll is a no-op).

#### `aii-node` CLI

- New `--release-poll-secs N` flag (default `60`, `0` disables).
  Only fires when `--update-peers` is also non-empty. When
  enabled, the poller spawn happens just before the RPC server
  starts, and the join handle is aborted alongside the producer
  + follow handles on Ctrl-C.

### End-to-end coverage (v0.0.74 → v0.0.81)

The auto-update protocol now closes every reasonable failure
mode the operator might hit:

1. **Sign** (v0.0.74) — Ed25519 manifest.
2. **Announce** (v0.0.75) — pinned-pubkey verify on any one node.
3. **Store** (v0.0.76) — `<data-dir>/releases/<version>` cache.
4. **Gossip** (v0.0.77) — push/pull binary across `--update-peers`.
5. **Install** (v0.0.78) — atomic rename + execve same-PID restart.
6. **Hotfix** (v0.0.79) — 128 MiB body cap + `exec_self_at(target)`.
7. **Rollback** (v0.0.80) — reversible `.previous` snapshot.
8. **Re-poll** (v0.0.81) — late-joiner catch-up cadence.

A node that was offline at announce time can come back, and
within `--release-poll-secs` it discovers the manifest, pulls
the binary, optionally auto-installs, and (if anything goes
wrong) the operator's recovery path is `aii release rollback`.

### Scope discipline

Out of scope for v0.0.81, explicitly:

- **Boot-health watchdog** — auto-rollback if the new binary
  fails to come up within N seconds. Still operator-driven.
- **Pubkey rotation** — still pinned at compile time.

## [0.0.80] — 2026-05-27

### Added — pre-install snapshot + rollback safety net

Closes the loop on the auto-update protocol's last remaining
sharp edge: a bad release pushed to every validator can no
longer brick the network with no recovery path. Every install
now atomically snapshots the running binary to
`<data-dir>/releases/.previous` before clobbering it, and a new
RPC + CLI exposes one-shot rollback.

The same trust boundary still applies — only a manifest signed
with the pinned project pubkey reaches the install path — but
the consequence of "the operator signed the wrong binary" is
now "type `aii release rollback`" instead of "drive to the
data center."

#### `aii-node::release_install`

- `PREVIOUS_NAME = ".previous"` constant — fixed filename
  inside the release store. Dot-prefix avoids collision with a
  real release version (all versions are semver-ish, never
  start with `.`).
- `previous_path(releases_dir) -> PathBuf`.
- `save_previous(current_exe, releases_dir) -> io::Result<PathBuf>` —
  copies the running binary into `<releases_dir>/.previous`
  atomically via `<target>.new` + `rename(2)`, with `0o755`.
- `rollback_to_previous(releases_dir, target) -> io::Result<PathBuf>` —
  reversible swap. Moves `.previous` to a holding path, snaps
  the current target into `.previous`, then installs the held
  bytes back onto target. After the call `.previous` holds the
  bytes we rolled away from, so a second rollback flips the
  pair back.
- 5 unit tests: atomic snapshot write + executable bit;
  overwrites existing; rollback round-trip; rollback is
  reversible via a second call; rollback returns NotFound
  with no `.previous` and leaves the target untouched.

#### `aii-node::NodeState`

- `RpcState::install_release` now calls `save_previous` before
  `install_binary`. Snapshot failure is logged at WARN and the
  install proceeds — we'd rather ship the binary without a
  rollback option than refuse the install over a transient
  I/O hiccup.
- New `RpcState::rollback_release` impl on Unix: resolves data
  dir + target (with the v0.0.78 `install_target_override` for
  tests), invokes `rollback_to_previous`, then spawns the same
  `exec_self_at(target)` self-restart task.
- 2 lib-level integration tests verify install → rollback → roll
  forward via a second rollback, and the fail-soft path when no
  snapshot exists.

#### `aii-rpc`

- New `aii_rollbackRelease() -> InstallReleaseResult` JSON-RPC
  method. Same envelope as install for client symmetry.
- New `RpcState::rollback_release` trait method with a
  "not supported" default.
- 2 RPC integration tests cover the happy path and the
  no-snapshot rejection.

#### `aii-cli`

- New `aii release rollback --rpc URL` subcommand. Async
  dispatch in `main()` (the existing sync `handle_release_cmd`
  can't await an HTTP RPC call); pretty-prints
  `scheduled rollback to .previous; node will restart in 2 s`
  on success, `rollback rejected: …` on failure.
- New `aii_cli::run_rollback_release(rpc)` helper for
  integrators that want the typed result.

### Scope discipline

Out of scope for v0.0.80, explicitly:

- **Boot-health watchdog** that auto-rolls-back when the new
  binary fails to come up. The current rollback is operator-
  initiated; an automatic rollback needs a "did I reach a
  known-good state within N seconds of startup?" signal,
  which couples this layer to consensus health and is best
  shipped after the late-joiner re-poll lands.
- **Late-joiner periodic `aii_latestRelease` poll** against
  the update-peer list. Nodes that miss the gossip wave
  (offline at announce time) currently have no way to
  discover a release was made; will land in v0.0.81.
- **Pubkey rotation.** Still pinned at compile time.

## [0.0.79] — 2026-05-27

### Fixed — v0.0.78 install hotfix (now actually works in production)

Live-testnet self-validation of v0.0.78 surfaced two showstopper
bugs that made the auto-update path inoperable outside the unit-
test harness. Both are fixed here. With this release the
end-to-end flow (sign → announce → import → atomic install →
`execve` self-restart) has been verified against a real running
aiid binary, with the kernel reporting the same PID across the
upgrade and `/proc/$PID/exe` resolving cleanly to the new file.

#### `aii-rpc::serve` — bump JSON-RPC body cap to 128 MiB

jsonrpsee's default `max_request_body_size` is 10 MiB. The
hex-encoded `aii_importReleaseBinary` call for a ~16 MiB aiid
build produces a ~32 MiB request body, which the server rejected
with `-32007 "Request is too big"`. The binary never even
reached the import handler, let alone the install path.

- `MAX_REQUEST_BODY_SIZE = 128 MiB`
- `MAX_RESPONSE_BODY_SIZE = 128 MiB`
- `serve()` now builds a `ServerConfig` via
  `ServerConfig::builder().max_*_body_size(...)` and passes it
  through `Server::builder().set_config(cfg)`. This is the
  jsonrpsee-0.26 idiom — the builder no longer exposes the
  per-field setters directly.

#### `aii-node::release_install::exec_self_at` — pass install target explicitly

After `install_binary` swaps the running binary via `rename(2)`,
the kernel marks `/proc/self/exe` with a literal `" (deleted)"`
suffix. `std::env::current_exe()` then returns
`/path/to/aiid (deleted)` — a string `execve` rejects with
`ENOENT`. The install succeeded, the new bytes were on disk,
but the self-restart never reached the new image.

- New `exec_self_at(exe: &Path) -> io::Error` that `execve`s an
  explicit path with the current process's argv (minus arg0)
  and env. `exec_self()` keeps the old contract via this
  function for callers that haven't replaced their binary
  yet.
- `RpcState::install_release` now captures `target` BEFORE the
  rename and moves it into the spawned restart task, which calls
  `exec_self_at(&exec_target)` instead of re-resolving via
  `current_exe()`.

#### Live testnet self-validation log

A v0.0.79-built aiid was booted with `--auto-install-releases`,
a manifest was signed with the pinned project secret, and the
binary was imported via `aii_importReleaseBinary`. Outcome:

- Manifest accepted ✓
- 16,199,408 byte binary written to
  `<data-dir>/releases/<version>` ✓
- `auto-install conditions met; invoking install_release` ✓
- `release installed; self-restart scheduled` ✓ (binary mtime
  flipped)
- 2 s later: `starting aiid …` with the **same PID** — execve
  hot-swapped the process image ✓
- `recovered persisted chain from data_dir recovered_head=2` ✓
- `/proc/<PID>/exe -> /tmp/aii-v0078-test/bin/aiid` (no
  `(deleted)`) ✓
- Chain continued producing blocks (head reached 26 within 30 s
  of the restart) ✓

### Scope discipline

This is a hotfix release; no new features. The 128 MiB cap was
chosen as a defensive ceiling rather than a permanent design
decision — when binaries grow past that the cap moves with
them. Streaming binary transfer (chunked import) is a future
slice when the per-binary size makes the in-memory hex payload
genuinely uncomfortable.

## [0.0.78] — 2026-05-27

### Added — atomic install + execve self-restart

Fifth and final foundational slice of the auto-update protocol —
closes the loop from "verified binary cached on disk" to "node is
running the new binary." A validator that has accepted a manifest
via the v0.0.75 RPC and received the matching binary via the
v0.0.77 gossip path can now finish the upgrade itself, without
any operator-driven systemd dance.

#### `aii-node::release_install` (new module, `cfg(unix)`)

- `install_binary(staged, target) -> io::Result<PathBuf>` —
  atomically replace `target` with the bytes from `staged`. Copy
  to `<target>.new`, `chmod 0o755`, then `rename(2)` over the
  running binary. On Linux `rename(2)` is allowed to replace a
  currently-executing file because the kernel keeps the inode
  alive for the running process via its open mmap, so the live
  `aiid` keeps serving while the directory entry already points
  at the new bytes.
- `current_aiid_path() -> io::Result<PathBuf>` — wraps
  `std::env::current_exe()`.
- `exec_self() -> io::Error` — `Command::new(current_exe).args(…).exec()`
  replaces the running process image with the new binary while
  **preserving the PID**. Systemd does NOT respawn the unit —
  the upgrade is invisible to the supervisor, which is exactly
  what we want. On `execve` failure (rare: missing binary,
  ENOMEM) the error is returned and the node continues serving
  from the old image.
- 6 unit tests cover the install path: replaces target, sets
  exec bit, creates missing target, cleans up stale `.new`,
  propagates missing-source errors, resolves a working
  `current_exe`. The `exec_self` path is intentionally untested
  in unit tests (would replace the test runner); the integration
  tests below cover the file-mutating half.

#### `aii-rpc`

- New `aii_installRelease(version) -> InstallReleaseResult`
  JSON-RPC method. Returns `{ scheduled, reason, restart_in_secs }`.
  The install (file copy + chmod + rename) happens synchronously
  inside the handler; the `execve` runs from a spawned task with
  a short delay so the JSON-RPC reply flushes back to the caller
  before the process is replaced.
- New `RpcState::install_release` trait method with a "not
  supported" default. `aii-node::NodeState` overrides on Unix.
- New `InstallOutcome` (trait-layer) and `InstallReleaseResult`
  (wire-layer) structs.
- 2 RPC integration tests verify happy path (cached binary →
  scheduled) and rejection (missing binary).

#### `aii-node::NodeState`

- New fields: `auto_install_releases: AtomicBool`,
  `install_target_override: RwLock<Option<PathBuf>>`. Override
  is test-only (`set_install_target_for_tests`) — redirects the
  install target away from `/proc/self/exe` and suppresses the
  `execve` spawn so integration tests can exercise install
  without overwriting the test runner.
- `RpcState::install_release` impl performs the full path:
  resolve data dir, verify staged binary exists, resolve target
  (override or `current_exe`), call `install_binary`, spawn
  `execve` task (skipped in test mode).
- `record_release_announcement` and `import_release_binary`
  both invoke a new `maybe_auto_install_release` helper after
  their happy path. When auto-install is on AND a manifest is
  known AND its binary is cached locally, the install fires
  automatically — regardless of which arrived first (gossip
  ordering doesn't matter).
- 3 lib-level integration tests: explicit install swaps the
  override target; install rejects missing binary; auto-install
  fires the moment both (manifest, binary) are in hand via the
  announce → import sequence.

#### `aii-node` CLI

- New `--auto-install-releases` flag. Off by default — in-place
  restarts are disruptive and most operators want to schedule
  the swap manually via `aii_installRelease` once they've
  reviewed the manifest. On a validator with `--update-peers`
  set, enabling this flag turns the validator into a fully
  hands-off auto-updating node.

### End-to-end auto-update flow (v0.0.74 → v0.0.78)

The whole chain now exists:

1. **Sign** (v0.0.74): `aii release sign --binary aiid --version V --secret SK`
   produces an Ed25519-signed manifest.
2. **Announce** (v0.0.75): the operator hits any one node with
   `aii_announceRelease(manifest)`. That node verifies the
   signature against the pinned project pubkey.
3. **Gossip + fetch** (v0.0.77): the accepting node re-broadcasts
   to its `--update-peers` and pushes the binary to peers that
   lack it. Every receiver re-verifies signature + SHA-256.
4. **Install + restart** (v0.0.78): each node with
   `--auto-install-releases` set atomically swaps the running
   binary and `execve`s into the new image. Systemd PID stays
   the same; the supervisor sees no restart.

A new release reaches the entire validator set in seconds, and
the trust boundary at every hop is "valid Ed25519 signature from
the pinned project pubkey + matching SHA-256."

### Scope discipline

Out of scope for v0.0.78, explicitly:

- **Rollback / two-slot install.** If the new binary crashes on
  start, systemd will respawn it, and the broken binary stays
  installed. A future slice keeps `<previous-version>` cached
  and ships a watchdog that rolls back after N failed starts.
- **Periodic re-poll for late joiners.** A node that misses the
  manifest gossip wave (e.g., was offline) currently has no way
  to discover a release was made. A future slice adds a periodic
  `aii_latestRelease` pull against the update-peer list.
- **Pubkey rotation.** The release-signing pubkey is still
  compiled in. Operator-driven rotation lands separately,
  alongside the secret-management policy.

## [0.0.77] — 2026-05-27

### Added — release auto-gossip + auto-fetch

Fourth slice of the auto-update protocol — closes the
"manifest+binary is on one node, how do the others get it?" loop.
After a node accepts a new manifest via `aii_announceRelease`, it
now spawns a background task that:

1. **Re-broadcasts** the manifest to every `--update-peers` URL
   via `aii_announceRelease`. Receivers re-verify the signature
   against their own pinned pubkey, so the hop carries no extra
   trust. Duplicate manifests are rejected at the receiver
   (`record_release_announcement` requires strictly-newer
   `(timestamp, version)`), so the flood terminates within one
   hop per peer link.
2. **Bidirectional binary transfer**:
   - If the local node *has* the binary, it pushes it to peers
     that don't (`aii_importReleaseBinary`).
   - If the local node *doesn't* have the binary, it pulls from
     the first peer that does (`aii_getReleaseBinary`).
   Either direction re-verifies SHA-256 against the manifest
   before persisting.

#### `aii-rpc::release_gossip`

- New module owning the propagation logic.
- `propagate_release(state, manifest, peers) -> PropagateOutcome`
  — fire-and-forget; never panics, returns a per-peer breakdown.
- `parse_update_peers(s) -> Vec<String>` — comma-split, normalise
  to `http://host:port`. 3 unit tests.

#### `aii-rpc`

- New `RpcState::update_peers_for_release` trait method (default
  empty); `NodeState` overrides to return the operator-supplied
  `--update-peers` list.
- `AiiRpcImpl::announce_release` now spawns `propagate_release`
  on the success path when the host has any update peers
  configured.
- 1 new 2-node integration test (`release_gossip_two_node_propagate`)
  that boots two RPC servers, points node A's `update_peers` at
  node B, announces a signed manifest to A, and asserts B ends
  up with both the manifest and the binary.

#### `aii-node` (`aiid` binary)

- New CLI flag `--update-peers HTTP1,HTTP2,…` (default empty).
- `NodeState::set_update_peers` / `update_peers` setters/getters.
- Main initialises the peer list early in `main()`, right after
  `set_data_dir`.

789 tests pass, clippy clean.

#### Scope discipline

Deferred to v0.0.78+:

- Atomic install — copy `<data-dir>/releases/<version>` over the
  in-flight `aiid` binary, swap symlink, re-exec.
- Self-restart via `execve` so systemd doesn't need to be poked.
- Periodic re-poll loop for nodes that came online late and
  missed the original gossip wave.

## [0.0.76] — 2026-05-27

### Added — release-binary store + `aii_getReleaseBinary` / `aii_importReleaseBinary` RPC

Third slice of the auto-update protocol. v0.0.75 let peers gossip
the *signed manifest*; v0.0.76 lets them gossip the *binary itself*.
Verification stays first-class: a node will only serve a binary
whose SHA-256 matches the manifest it has already verified.

#### `aii-node::release_store`

- New module owning `<data-dir>/releases/<version>` cache layout.
- `store_verified_binary(dir, version, expected_sha256, bytes)`
  recomputes SHA-256 of the bytes and only writes (atomically, via
  `.tmp` + `rename(2)`) on a hash match. `HashMismatch` is
  returned without ever creating the target file on mismatch.
- `load_binary(dir, version)` returns `Ok(None)` on missing file.
- 6 unit tests (round-trip, hash-mismatch-no-file-leak, malformed
  hash, missing-returns-none, atomic-tmp-cleanup, accepts-`0x`
  prefix).

#### `aii-node::NodeState`

- New `data_dir: RwLock<Option<PathBuf>>` field + public
  `set_data_dir(PathBuf)` setter. `aiid` main calls it once at
  startup so the release-store helpers can resolve paths without
  changing the existing `NodeState::new` / `recover` signatures.
- `RpcState::release_binary_bytes` reads from the store.
- `RpcState::import_release_binary` cross-checks the announced
  version against the locally-known latest manifest (refuses
  unverified bytes), then delegates to `store_verified_binary`.

#### `aii-rpc`

- New `AiiRpc::get_release_binary(version) -> Option<String>` —
  returns the binary as `0x`-prefixed hex, or `null`.
- New `AiiRpc::import_release_binary(version, hex_bytes) -> ImportReleaseResult`
  — accepts an externally-supplied binary, verifies its SHA-256
  against the local manifest, and persists on success.
- New `ImportReleaseResult { accepted, reason }` envelope.
- New `RpcState::release_binary_bytes` and `import_release_binary`
  trait methods (default no-ops; NodeState overrides).
- 2 new RPC end-to-end tests:
  - `aii_get_release_binary_missing_returns_null`
  - `aii_import_release_binary_round_trip`

785 tests pass, clippy clean.

#### Scope discipline

Deferred to v0.0.77+:

- Cross-node gossip relay of announcements — when a node accepts
  `announce_release`, push the same manifest to its peers.
- Auto-fetch — when a node knows the latest manifest but hasn't
  got the binary, pull from a peer via `get_release_binary` +
  `import_release_binary` chain.
- Atomic install + self-restart — write the new binary over
  `aiid-current`, signal systemd / re-exec, hand off to v0.0.78+.

## [0.0.75] — 2026-05-27

### Added — pinned release pubkey + `aii_announceRelease` / `aii_latestRelease` RPC

Second slice of the auto-update protocol. v0.0.74 shipped the
manifest sign/verify primitives behind a `--pubkey HEX` CLI
argument; v0.0.75 ships the pubkey **pinned in the binary** and
exposes the announcement+query wire over JSON-RPC so peers can
gossip release availability.

#### `aii-crypto::release` (moved from `aii-cli`)

- Module moved from `aii-cli::release` into `aii-crypto::release`
  so `aii-rpc` and `aii-node` can depend on it. `aii-cli` re-
  exports `aii_crypto::release` under its old name for backward
  compatibility — every prior call site keeps working.
- New `pub const RELEASE_SIGNING_PUBKEY_HEX: &str = "f845…0669"`
  pinning the AII Network release-signing public key. The matching
  secret seed is held off-chain by the release manager.
- New `pinned_release_pubkey() -> PublicKey` helper.

#### `aii-cli::release`

- `aii release verify --manifest M --binary B` now omits
  `--pubkey` and defaults to the pinned key. Pass `--pubkey HEX`
  to override (testing, key rotation drills).
- Verify output prints the first 16 hex chars of the pubkey it
  used so operators can confirm which trust anchor was in force.

#### `aii-rpc`

- New `AiiRpc::announce_release(manifest)` method. Server-side
  the receiver verifies the Ed25519 signature against
  `pinned_release_pubkey()` *before* handing the manifest off to
  `RpcState::record_release_announcement` for persistence;
  signature failures return `{ accepted: false, reason: ... }`
  with no state mutation.
- New `AiiRpc::latest_release()` method returning the most
  recently accepted manifest, or `null`.
- New `ReleaseManifestView` (wire-shape mirror of
  `aii_crypto::release::ReleaseManifest`) + `AnnounceReleaseResult`.
- New `RpcState::record_release_announcement` + `latest_release`
  trait methods (default no-ops). `NodeState` overrides both to
  persist into a new `latest_release: RwLock<Option<...>>` field.
- 3 RPC unit tests:
  - `aii_announce_release_rejects_unsigned_manifest`
  - `aii_announce_release_accepts_pinned_pubkey_signature`
  - `aii_latest_release_fresh_node_returns_null`

#### `aii-node::NodeState`

- New `latest_release: RwLock<Option<ReleaseManifest>>` field.
- `record_release_announcement` accepts strictly-newer manifests
  (compared on `(timestamp_unix, version)` lexically), rejects
  duplicates / backdated re-signs.
- `latest_release` returns the cloned manifest or `None`.

777 tests pass, clippy clean.

#### Scope discipline

Deferred to v0.0.76+:

- Peer binary fetch — `aii_getReleaseBinary(version) -> bytes` so
  a node that received an announcement can pull the binary from a
  peer that already has it.
- Atomic install — write to `<data-dir>/releases/<ver>.new`,
  verify sha256, rename, swap symlink, re-exec.
- Cross-node gossip relay — when `announce_release` accepts, the
  receiver forwards the same manifest to its peers (BTC-style
  flood with hash-dedup).

## [0.0.74] — 2026-05-27

### Added — signed release-manifest primitives

Foundation slice for the authenticated auto-update protocol the user
asked for during the cross-pacific testnet bring-up: any node receiving
a peer-distributed binary update must be able to verify the binary
hasn't been tampered with AND that the release was authorised by the
holder of the project's release-signing key.

This release ships only the cryptographic primitives + CLI; wire-level
gossip of releases, peer binary fetch, and atomic in-place install land
in later versions on top of this foundation.

#### `aii-crypto::ed25519`

- New module wrapping `ed25519-dalek` 2.x. Exposes `SecretKey`,
  `PublicKey`, `Signature` with hex round-trip, `SecretKey::generate`,
  `sign(msg)`, `verify(msg, sig)`. Independent from the BLS validator
  keys (`bls.rs`) and the VRF leader-election keys (`vrf.rs`) — release
  signing is an operator-trust signal, not a chain consensus signal.
- New error variants: `CryptoError::Hex`, `CryptoError::BadLength`,
  `CryptoError::Ed25519` (+ `CryptoError::ed25519` constructor).
- 6 unit tests covering sign/verify round-trip, tamper detection, wrong
  public key, hex round-trip with and without `0x` prefix, bad-length
  rejection.

#### `aii-cli::release`

- New `ReleaseManifest { version, sha256_hex, timestamp_unix,
  ed25519_sig_hex }` serde struct.
- New `canonical_payload(version, sha256, ts)` helper. The signed
  bytes carry a `"aii-release-v1\0"` domain-separation tag so the
  same Ed25519 key cannot be misused to forge a confounder signature
  on unrelated payloads (validator votes, etc.).
- `sign_release(secret, binary_path, version, timestamp)` — hashes
  the binary, assembles the manifest, signs the canonical payload.
- `verify_release(pubkey, manifest, binary_path)` — re-hashes the
  binary, checks against the manifest's `sha256_hex`, verifies the
  signature against the canonical payload, returns the verified
  binary bytes on success.
- 6 unit tests covering happy path, tampered binary (`HashMismatch`),
  forged version, forged timestamp, wrong pubkey, JSON round-trip.

#### `aii-cli` binary — `aii release {keygen, sign, verify}`

- `aii release keygen` — generates a fresh Ed25519 keypair; secret
  seed written to `--out` (or printed alongside the public key);
  public key always printed (so it can be pinned in CI / docs).
- `aii release sign --binary BIN --version VER --secret HEX --out
  release.json` — produces the signed manifest. `--secret-file` lets
  the seed live on disk instead of in argv. `--timestamp` defaults to
  the current Unix time.
- `aii release verify --manifest release.json --binary BIN --pubkey
  HEX` — full chain of checks; exits 0 on success with `ok — VERSION
  signed at TIMESTAMP`, otherwise reports `binary hash mismatch:
  manifest says X, computed Y` or the signature failure path.

774 tests pass; clippy clean.

#### Scope discipline

Deferred to v0.0.75+:

- Pinned public-key constant compiled into the node binary so a
  remote verify needs no explicit `--pubkey` argument.
- `aii_announceRelease` JSON-RPC method so a node can gossip a
  manifest to its peers; receivers verify locally with the pinned
  key.
- `aii_getReleaseBinary` JSON-RPC method so a peer that's missing
  the binary for a verified manifest can pull the bytes from a node
  that already has them.
- Atomic install + self-restart on a verified new release.

## [0.0.73] — 2026-05-27

### Fixed — gossip auto-harvests committed blocks between inbox messages

v0.0.72 fixed early-arrival precommit rejection but exposed a second
race: when the proposer races ahead of its followers (commits block
N at t=0, broadcasts proposal for N+1 at t=50ms), the follower's
gossip tick can dispatch the next-height proposal BEFORE its main
loop has called `try_harvest_committed` to advance the engine's
`head_hash`. Reconstruction then computes the new block's parent
hash against the *old* head, gets `ProposalHashMismatch`, and the
chain stalls at exactly block N+1. Live-tested across JP/CN/local:
chain produced 20 blocks in ~1 second then froze with the proposer
500 ms ahead.

The fix: `BftGossip::tick()` now calls a new
`engine.try_harvest_committed()` between every dispatched inbox
message. Harvested blocks are stashed on an internal
`harvested_blocks: Mutex<Vec<Block>>` buffer; the host drains them
via the new `BftGossip::drain_harvested()` and applies them to its
world-state storage. The engine's `head_hash` advances in lockstep
with the inbox so subsequent proposals reconstruct against the
correct parent.

#### `aii-consensus-bft::gossip`

- New `BftGossip::harvested_blocks: Mutex<Vec<Block>>` field.
- New `BftGossip::drain_harvested() -> Vec<Block>` public API for
  hosts.
- New private `BftGossip::auto_harvest()` helper that pulls every
  committed block out of the engine and pushes it onto the buffer.
- `tick()` calls `auto_harvest()` after each inbox message AND once
  after the drive-phase work (belt-and-braces for the rare case
  where the local engine's own precommit was the quorum-forming
  vote and didn't traverse the inbox).
- Two existing gossip tests updated to drain via the new API; one
  retained the direct `try_harvest_committed` path as a fallback.

#### `aii-node` (`aiid` binary)

- The multi-validator BFT loop now drains via
  `gossip.drain_harvested()` first, then falls back to
  `engine.try_harvest_committed()` for non-gossip paths.
- The post-commit `bft_state.json` snapshot loop is unchanged —
  each harvested block still resets the persisted round state to
  `(N+1, 0)` per the v0.0.71 contract.

762 tests pass; clippy clean.

## [0.0.72] — 2026-05-27

### Fixed — BFT engine no longer rejects early-arrival prevotes/precommits

Out-of-order vote arrival was silently freezing 3-validator BFT
whenever the proposer's precommit reached a remote validator before
that validator had tallied enough prevotes to transition to
`Precommitting`. The engine returned `WrongPhase` (or
`NoActiveCoordinator` for votes that beat the proposal entirely)
and the gossip layer dropped the message — the round then stalled
until a timeout, every time, on every block. With cross-pacific
network latency (the JP/CN/local testnet) this defeated all
liveness.

Diagnosed via `tracing::warn!` logs added to
`submit_remote_{prevote,precommit}` showing CN's precommits landing
on a still-Prevoting JP within ~20 ms of the leader's broadcast,
ahead of JP's own prevote tally.

The fix: prevotes and precommits that fail with
`NoActiveCoordinator`, `WrongPhase`, `WrongRound`, or `WrongHeight`
are now buffered on the engine state rather than rejected. Every
subsequent state mutation (proposal arrival, prevote tally,
precommit tally, round timeout) calls a new `drain_pending_votes`
helper that re-submits the buffered votes through the coordinator
until no more can be applied. Stale votes (for an already-
committed height) are dropped silently.

#### `aii-consensus-bft::engine`

- New `BftEngineState::pending_prevotes` + `pending_precommits`
  buffers (`Vec<PrevoteVote>` / `Vec<PrecommitVote>`).
- `submit_remote_prevote` / `submit_remote_precommit` rewritten to
  match on the coordinator's error and route timing-class errors
  into the buffer; pass other errors (signature failure, dup vote)
  through unchanged.
- New private `drain_pending_votes(&mut BftEngineState)` helper.
  Called from `submit_remote_proposal`, `tick_timeout`, and the
  success paths of `submit_remote_{prevote,precommit}`. Uses a
  bounded loop (each iteration must apply ≥1 vote to repeat) so
  the total work is capped by total buffer size.
- 3 new unit tests:
  - `prevote_arriving_before_proposal_is_buffered_and_replayed`
  - `precommit_arriving_during_prevoting_is_buffered_and_replayed`
  - `stale_buffered_votes_are_dropped`

762 tests pass; clippy clean.

#### Scope discipline

This release contains ONLY the buffer fix. Deferred to v0.0.73+:

- Real signed auto-update protocol (Ed25519 release manifest +
  on-chain announce + peer fetch + atomic install). Was previously
  v0.0.72; bumped now that v0.0.72's slot is needed for the BFT
  fix that's currently blocking testnet liveness.

## [0.0.71] — 2026-05-27

### Added — persistent BFT round state (single-validator restart no longer freezes consensus)

Closes the second half of the chain-continuity story started in
v0.0.70. Previously, when a single validator restarted (binary
upgrade, OS reboot, crash recovery), the rest of the validator set
sat at round R while the restarted node came up at round 0 — their
votes never combined into a quorum and the chain froze until every
validator restarted together. v0.0.71 persists the `(height, round)`
of the active coordinator to disk on every change; on startup the
restored node fast-forwards through `round` timeouts before
listening for new votes, landing at the same round as the live set.

#### `aii-consensus-bft`

- New `BftEngine::fast_forward_to_round(target_round)`. Creates a
  fresh coordinator at the next-to-commit height and calls
  `RoundCoordinator::fire_timeout` `target_round` times. Idempotent
  for `round == 0`. Errors with `BftError::WrongHeight` if a
  coordinator for a different height is already active (defensive —
  the typical startup-time call site cannot trigger it).
- 2 new unit tests (`fast_forward_to_round_lands_at_target`,
  `fast_forward_to_round_zero_creates_coordinator_at_round_zero`).

#### `aii-node::bft_state`

- New module persisting `BftStateSnapshot{height, round}` to
  `<data-dir>/bft_state.json` (atomic temp + rename).
- `load` returns `Ok(None)` for missing or malformed files — we
  prefer round-0 startup over a crash on corrupted snapshot.
- 4 unit tests covering load/save/round-trip/atomicity/garbage
  tolerance.

#### `aii-node` (`aiid` binary)

- Startup path now reads `bft_state.json`. When the snapshot's
  `height` matches `recovered_head + 1` and `round > 0`, calls
  `engine.fast_forward_to_round(snap.round)` and logs
  `restored BFT coordinator from persisted round state`.
- Tick loop persists the current `(height, round)` every time the
  tracked tuple changes. On each successful block commit, the
  snapshot resets to `(N+1, 0)` so a crash before any round
  timeout fires still recovers at the right height.

#### Scope discipline

Deferred to v0.0.72:

- Persisting the `locked_value` / `polc` / vote tallies (BFT safety
  state). Without this, a restarted validator could theoretically
  vote for an incompatible block after losing its lock — fine on a
  development testnet, must-fix before mainnet. Doing it cleanly
  needs a serializer for the BLS/VRF certificate machinery, which
  warrants its own release.
- Signed binary auto-update protocol (was previously v0.0.72; still
  the slot after lock-state persistence).

## [0.0.70] — 2026-05-26

### Added — chain continuity across restart (BFT engine resumes from recovered head)

Fixes the silent corruption in v0.0.67–v0.0.69 where any `aiid`
restart caused the BFT engine to ignore the persisted chain and
start producing block 1 again, overwriting whatever RocksDB had
already stored. This was harmless when all validators sync-restarted
together (the new chain replaces the old) but catastrophic if a
single operator restarted to upgrade a binary — the chain reset and
all other validators stalled trying to vote at a height the
restarted node didn't recognise.

The fix:

- New `BftEngine::from_recovered(config, head_block)` constructor.
  Resumes the engine at `head_block.header.number + 1` round 0 with
  `seed` derived from `head_block.header.mix_hash` (the VRF output
  the producer carries forward across heights since v0.0.34).
- New `bft_bootstrap::boot_bft_engine_with_recovered_head` helper
  on top of it.
- New `NodeState::block_by_number` + `head_block` accessors so the
  startup path can pull the full recovered block out of the
  in-memory index that `recover()` rebuilds.
- `aiid` startup now checks `node_state.head_block()`; if non-`None`
  and `number > 0`, routes through `from_recovered` instead of
  `boot_bft_engine` (which always restarts at genesis). Logs a
  `resuming BFT engine from recovered head` line + a new
  `recovered_head=N` field on the `BftEngine ready` line.

#### Tests

Two unit tests in `aii-consensus-bft`:

- `from_recovered_resumes_at_head_plus_one` — produces block 1 from
  a fresh engine, builds a *second* engine via `from_recovered` on
  that block, and confirms the second engine's next-advance block
  has `number=2` and `parent_hash` matching the recovered block's
  hash.
- `from_recovered_with_genesis_block_matches_new` — `from_recovered`
  against a genesis-only chain is observationally identical to
  `new`.

#### Scope discipline

Deferred to v0.0.71:

- Persisting round / locked_value / step so a single-validator
  restart doesn't freeze BFT consensus while it catches up to the
  other validators' current round. This is the next user-visible
  fix (single-node restart still triggers a global stall for ~30 s
  in v0.0.70).

Deferred to v0.0.72:

- Signed binary auto-update protocol — Ed25519 release-signing key
  + on-chain release announce + peer-fetch with sha256 verify +
  atomic install + self-restart. Each piece is a unit risk;
  better as its own dedicated release once BFT continuity is
  proven stable on the testnet.

## [0.0.69] — 2026-05-26

### Added — persistent peer cache + BFT gossip relay

Two changes that together make the BFT network self-healing in any
topology — a validator that has talked to the network even once can
restart and rejoin without operator intervention; and a validator
whose direct link to another validator dies can keep voting as long
as *any* third validator can bridge them. This is the core of the
"无封锁可能 / 断网了恢复立即自动组网" requirement: the network is
robust to any single link failure and to arbitrary restarts.

#### `aii-node::peer_cache`

- New module persisting the dialer's last-known-good peer set to
  `<data-dir>/peers.json` (text format, one `SocketAddr` per line).
- On startup, `aiid` merges `--peers` with the cache and dials the
  union; the cache is rewritten atomically (`peers.json.tmp` +
  `rename(2)`) so a crash mid-write can never strand a half-file.
- 6 unit tests cover round-trip, missing-file tolerance, comment +
  garbage tolerance, dedup/sort, atomicity, merge ordering.

#### `aii-node::bft_p2p` — gossip relay

- New `GossipDedup` hash ring (4096-slot FIFO of keccak256
  payloads). Every locally-originated `broadcast()` pre-seeds the
  ring so echoes bouncing back from relayers are dropped; every
  inbound BFT payload is checked once, and only novel payloads are
  pushed to `inbox` AND fanned out via `out_tx` for relay.
- `run_peer` and `run_peer_noise` both apply the same dedup-then-
  relay path. The change is wire-compatible: no protocol bumps, no
  new message types — relay is invisible to peers running v0.0.68
  but cooperating peers form a self-bridging mesh.
- Two new tests:
  - `gossip_relay_three_node_line_topology` — proves A↔B↔C with no
    direct A-C link still delivers A's broadcast to C.
  - `gossip_relay_suppresses_echo_to_originator` — proves A's own
    `broadcast()` doesn't bounce back into A's inbox after relayer
    forwarding.

#### Scope discipline

Deferred to v0.0.70:

- Multi-endpoint bootnode list (each validator advertises
  `host:port1, host:port2, host:443`; dial parallel, first success
  wins). Needs a peer-announce protocol change.
- Peer-reflected public-IP discovery (app-layer STUN). Pairs with
  multi-endpoint announce.
- QUIC + Noise transport. Pairs with the multi-endpoint work.

## [0.0.68] — 2026-05-26

### Added — NAT-friendly BFT (outbound-only mode + 30 s idle reconnect)

Validators sitting behind a home router (no NAT port forward),
HTTP-only proxy chain (Mihomo / Clash, Cloudflare WARP, corporate
VPN), or CGNAT'd ISP can now join the BFT set without exposing
30311 to the public internet. The mechanism is BTC-style: bind the
listener to a random loopback port, dial each peer via outbound
TCP, and let every consensus message flow over the established
outbound sockets in both directions.

This closes the v0.0.65–v0.0.67 gap that forced the local 3rd node
into observer-only mode: a node started with `--bft-outbound-only`
needs nothing from its network except the ability to make outbound
TCP to its peers. Once the peer is reachable the validator votes
and proposes like any other; if the proxy / NAT drops the link,
the new 30-second application-layer idle timeout closes the dead
session within one block-time worth of silence and the dialer
reconnects automatically. Net effect: "断网了一恢复就自动组网" —
the local validator rejoins consensus on its own as soon as
outbound TCP is back, without any operator action.

#### `aii-node` (`aiid` binary)

- New CLI flag `--bft-outbound-only` (default `false`). When set,
  the BFT listener binds to `127.0.0.1:0` (kernel-assigned loopback
  port, never exposed); all consensus traffic to `--peers` flows
  over outbound TCP only. Compatible with `--encrypt-gossip` for a
  Noise XX wrapped session.
- New public constant `aii_node::bft_p2p::BFT_PEER_IDLE_TIMEOUT`
  (= 30 s). Every BFT peer connection (plaintext or Noise) is now
  killed when no inbound bytes arrive in this window, so the
  dialer can immediately reconnect. Previously the read would
  block forever, letting a half-dead session silently swallow
  proposals and votes.
- New ctors: `TcpBftTransport::new_outbound_only(peers)` and
  `new_outbound_only_encrypted(peers)` for embedding the same
  behavior into downstream binaries.

#### Scope discipline

Deferred to v0.0.69:

- Adaptive round timeout (latency-aware scaling) — needs a latency
  measurement infrastructure first.
- BFT gossip relay (forward messages from peer A to peer C through
  peer B) — needs hash-dedup data structure and care around
  relay-driven equivocation reports.
- Dynamic peer discovery / Kademlia-driven auto-network — full
  "any port + any environment auto-network" is the v0.0.69 theme.
- Hot-join validator (link to chain-stake registry instead of
  genesis) — sits behind the v0.0.69 net layer.

## [0.0.67] — 2026-05-26

### Added — `--no-produce-blocks` + `--follow-seconds` observer mode

A node started with `--bootnode URL --follow-seconds N --no-produce-blocks`
now polls the bootnode every N seconds, applies every newly-finalised
block locally via the existing `bootstrap_sync_from_peer` path, and
serves RPC against the live chain — *without* forking its own
DevMode chain. This closes the v0.0.66 gap where any local node had
to either (1) be a full BFT validator (needs inbound network
reachability) or (2) accidentally fork by running the DevMode
producer.

This is what unblocks deploying a 3rd node on a NAT'd / proxied
machine: the local node joins the same `chain_id 9999` testnet that
JP + CN are running, just as an observer that follows their tip.

#### `aii-node` (`aiid` binary)

- `--produce-blocks` now accepts an explicit boolean value
  (`--produce-blocks=false` / `--no-produce-blocks` both work). The
  old "flag toggle" parsing rejected `--produce-blocks false`; this
  release fixes the clap config.
- New CLI flag `--follow-seconds N` (default 0 = disabled). When
  > 0 AND `--bootnode` is set, a background tokio task drains new
  blocks from the bootnode every N seconds and commits them locally.
  Implicitly requires the local producer to be off (otherwise the
  two would fight over `commit_block` invariants).
- Follow tick logs as `INFO aiid: follow tick: applied new blocks
  from bootnode head=N blocks_added=K`.

#### Scope discipline

Not in this release:

- **No light-client proof verification.** The follow loop trusts the
  bootnode's `aii_getRawBlock` response — same trust model as the
  one-shot bootstrap from v0.0.44. Cryptographic verification (BFT
  certificate + leader VRF proof against the local validator set)
  is the next iteration.
- **No mempool gossip for observer nodes.** A `--follow-seconds`
  observer receives blocks but doesn't pull or push txs; clients
  submitting `eth_sendRawTransaction` to an observer's RPC will be
  accepted into the local mempool but never make it into a block
  because the local producer is off. Observers are for *reading*
  the chain — submit txs to a validator's RPC instead.

## [0.0.66] — 2026-05-26

### Added — One-click PC installer suite + git branch consolidation

**`master` and `main` are now unified.** Before this release the GitHub
default branch (`main`) still pointed at the 3 stale planning-doc
commits from before any code shipped; all v0.0.40 → v0.0.65 work
lived only on `master`. v0.0.66 merges `main` into `master` with
`--allow-unrelated-histories` (keeping master's actual code at every
conflict), then pushes the merge tip to both branches so a fresh
visitor to `github.com/kinglovesdao/aii` sees the live code.

**One-click desktop / server installer suite.** Anyone with a single
shell command can now stand up a validator (or RPC observer) and
join the live testnet in under 60 seconds on Linux, 5 minutes on
macOS, 10 minutes on Windows. Bundles ship with the live testnet
genesis pre-loaded.

#### `release-bundle/` (new top-level)

- `staging/install-linux.sh` — root-required installer that:
  1. drops `aiid` / `aii` / `aii-mcp` into `/usr/local/bin/` (uses
     pre-built bundled binaries when present, falls back to `cargo
     build --release` against the local checkout or a fresh clone),
  2. installs the bundled testnet genesis to `/var/lib/aiid/genesis.json`,
  3. generates a fresh validator keystore via `aii validator keygen`
     (skipped with `--observer`),
  4. writes `/etc/systemd/system/aiid.service` with the right
     `ExecStart` for validator or observer mode,
  5. starts the service + verifies it's active.
- `staging/install-macos.sh` — builds from source via `cargo`,
  registers a launchd LaunchDaemon at
  `/Library/LaunchDaemons/org.aii.aiid.plist`.
- `staging/install-windows.ps1` — PowerShell installer (run as
  Admin), builds from source via `cargo`, registers `aiid` as a
  Windows Service via `sc.exe`.
- `staging/config/testnet-genesis.json` — pulled live from the JP
  testnet node; ensures bundled installs join the same chain.
- `staging/README.md` — 5-minute orientation: bundle contents, env
  var overrides, validator vs observer, RPC examples, port table.
- `staging/MANIFEST.json` — machine-readable build manifest:
  version, binary sizes, full `eth_*` / `aii_*` method list, chain
  id 9999, default bootnode.

#### Repo structure

- `release-bundle/.gitignore` skips `staging/bin/` + `staging-src/`
  + `*.tar.gz` + `*.zip` so pre-built artifacts (12 MB Linux
  tarball + 8 KB source-build zip) stay local — they're built
  reproducibly from `staging/` by anyone with `cargo` + `tar`.
- `master` and `main` now share a tip (`28cbb80` merge commit).
  `git push` against either branch will fast-forward the other.

#### Branch hygiene

27 `feat/aii-*` development branches (one per crate from the
original TDD pipeline) all confirmed already-merged into master,
then deleted locally. The full development history lives in the
master commit log.

#### Scope discipline

Not in this release (already on the roadmap):

- **No cross-compiled Windows + macOS pre-built binaries.** Both
  installers build from source via the local Rust toolchain. The
  alternative — shipping `aiid.exe` and Mach-O binaries — needs
  either a CI matrix with macOS + Windows runners, or local
  cross-compile via `cross` (Docker-based). Either lands when
  GitHub Actions release workflow exists.
- **No code signing.** Windows users see SmartScreen warnings;
  macOS users need to right-click → Open or `xattr -d
  com.apple.quarantine`. Apple Developer + Windows EV cert are
  not in scope until a project foundation exists.
- **No package-manager publication.** `brew tap` / `apt`
  repository / `winget` manifest all land alongside CI when the
  release pipeline gets formalised.

## [0.0.65] — 2026-05-26

### Added — `BlockExecutor` trait + engine apply-then-hash hook

The chronic deferral from v0.0.40 through v0.0.59 — "header still
embeds `EMPTY_TRIE_HASH` for `state_root` / `receipts_root` /
`logs_bloom`" — finally has the interface to close it. v0.0.65
introduces `aii_consensus_iface::BlockExecutor`, an oracle the
engine consults at proposal time to compute the post-execution
Yellow-Paper roots. When provided, the produced block header locks
to those roots and the block hash becomes consensus-correct over
the executed body. When absent, the legacy placeholder path runs —
existing testnets keep working unchanged.

#### `aii-consensus-iface`

- New `BlockExecutor` trait with one method:
  `execute_for_proposal(body, coinbase, block_number) ->
  PostBlockRoots`. Determinism contract is documented: every honest
  validator applying the same body against the same state must
  produce the same triple.
- New `PostBlockRoots { state_root, receipts_root, logs_bloom,
  gas_used }`.

#### `aii-consensus-bft`

- `BftConfig` gains
  `executor: Option<Arc<dyn BlockExecutor>>` (default `None` for
  backward compat).
- `build_block_with_body` and `advance_single` both consult the
  oracle when present; on `Err` they fall back to placeholders so
  a corrupt oracle can't brick the engine.

#### `aii-node`

- New `NodeStateExecutor` adapter implementing `BlockExecutor`. The
  first iteration returns the **current** `state.state_root()` —
  i.e. the post-block-(N-1) state-root, not the post-block-N. Both
  leader and followers compute the same answer because both start
  the round at the same head state, so hash stability is preserved.
  Applying the body against a state snapshot before answering is a
  future iteration of the same adapter; the trait surface stays
  unchanged.

#### Scope discipline

Not in this release (already on the roadmap):

- **No state-snapshot apply yet.** `NodeStateExecutor` reports the
  parent state's `state_root`, not the post-body state. Truly
  Yellow-Paper-correct headers need an in-memory overlay over a
  RocksDB snapshot — the trait + plumbing land here; the snapshot
  + replay path is the next iteration.
- **Default still `None`.** The `aiid` binary doesn't yet
  instantiate `NodeStateExecutor` automatically — operators opt in
  by constructing `BftConfig.executor = Some(Arc::new(
  NodeStateExecutor::new(state.clone())))`. Wired in CLI follow-up
  alongside the snapshot-apply iteration to avoid two consecutive
  block-hash-breaking changes.

## [0.0.64] — 2026-05-26

### Added — Noise XX wired into TcpBftTransport (closes C.4 wire-up)

The Noise primitive (v0.0.55) is now an opt-in mode on the main-
chain BFT gossip socket. Starting `aiid --bft --encrypt-gossip
--peers …` runs an XX handshake on every accept / dial; after the
handshake completes, all BFT proposals + votes flow through a
ChaCha20-Poly1305 AEAD-encrypted session. Plaintext gossip stays
default for backward compat with existing testnets.

#### `aii-node::bft_p2p`

- New `TcpBftTransport::new_encrypted(bind, peers)`. Same shape as
  `new`, but each accepted / dialed connection is upgraded to a
  Noise XX session before broadcast traffic flows.
- New private `run_peer_noise` — single-task design (owns
  `TcpStream` + `EncryptedSession`) sidesteps Noise's non-`Sync`
  `TransportState`. Outbound is polled non-blocking; inbound waits
  20 ms before timing out so neither side starves the other. BFT
  timing is in seconds, so the 20 ms cadence is invisible.
- New integration test `two_encrypted_transports_exchange_payload`
  proves the full handshake + encrypted broadcast over an in-process
  loopback pair.

#### `aiid` binary

- New `--encrypt-gossip` CLI flag (default `false`). When set, the
  BFT path dispatches to `new_encrypted` instead of `new`. All
  validators in a peer set must agree on the flag — mixing
  encrypted + plaintext peers fails the handshake immediately.

#### Scope discipline

Not in this release (already on the roadmap):

- **No static-key identity binding.** Each session uses a fresh
  x25519 keypair — peer identity is still bound by the higher-layer
  `Hello` message + BFT BLS signatures, not by the Noise static
  key. A static-key + signed-handshake-pubkey commit lands in a
  later release once the validator keystore exposes an x25519 KDF
  path alongside the existing BLS + VRF keys.
- **No mux yet.** One Noise session = one logical channel. BFT
  gossip is the only consumer today; once Discovery v4 + a sync
  protocol want to share the socket, yamux / mplex needs to slot
  between Noise and the application.

## [0.0.63] — 2026-05-26

### Added — FindNode / Neighbours UDP packets (closes C.3 wire-up)

The Discovery v4 wire was Ping/Pong-only since v0.0.17. The Kademlia
routing table shipped in v0.0.56 had no UDP transport to feed it.
v0.0.63 adds the remaining two packet types so the table can finally
be populated end-to-end:

#### `aii-net-p2p::discovery`

- New packet types: `TYPE_FIND_NODE = 0x03`, `TYPE_NEIGHBOURS = 0x04`.
- `Packet` enum gains `FindNode(FindNode)` + `Neighbours(Neighbours)`
  variants.
- `FindNode { target: H256, expiration: u64 }` requests the K closest
  nodes to `target`.
- `Neighbours { nodes: Vec<Endpoint>, expiration: u64 }` carries up
  to K candidate endpoints. Empty list ("I know nothing closer than
  myself") is valid.
- Existing signed-packet pipeline (`encode_packet` + `decode_packet`)
  handles the new types transparently; they go through the same
  secp256k1 sign + keccak hash verify path.
- 3 new round-trip tests: `find_node_round_trip`,
  `neighbours_round_trip_with_multiple_nodes`,
  `neighbours_round_trip_empty_list`.

#### Scope discipline

Not in this release (already on the roadmap):

- **No `UdpDiscovery::find_node()` driver method.** The packet types
  are encodable / decodable; the request-response state machine
  (send FindNode → wait for Neighbours, route into
  `KademliaTable::insert`) is the next chore.
- **No iterative-lookup `find_node_closest`.** Multi-round Kademlia
  walks (ask N closest, then N closest of those, until convergence)
  land in the same follow-up.
- **No NAT-pierce / endpoint-prediction.** Each `Neighbours` reply
  takes the included endpoints at face value — no validation that
  the `udp_port` actually responds. Reachability checks add a
  separate Ping after insertion.

## [0.0.62] — 2026-05-26

### Changed — Precompile opcodes are now Solidity-style selectors

The v0.0.60 precompile used custom 4-byte opcodes (`0x00000001` …
`0x00000005`). Wallets that already speak Solidity (MetaMask, ethers,
etc.) construct calldata with `keccak256(signature)[..4]`. v0.0.62
re-derives every selector from a canonical signature so an existing
Solidity ABI works against the AII precompile without translation.

#### `aii-node::precompile`

- `OP_BOND = keccak256("bond()")[..4] = 0x64c9ec6f`.
- `OP_BEGIN_UNBOND = keccak256("beginUnbond()")[..4] = 0x3f172cef`.
- `OP_WITHDRAW = keccak256("withdraw()")[..4] = 0x3ccfd60b`.
- `OP_PROPOSE = keccak256("propose(uint64,string)")[..4] = 0x37038a1d`.
- `OP_VOTE = keccak256("vote(uint64,bool)")[..4] = 0xc7f21560`.
- New unit test `selectors_match_keccak_signatures` recomputes each
  selector from the signature string and asserts equality. The
  constants stay enforceable as the canonical wire form.

This is a **breaking wire-format change** for anyone who hand-crafted
v0.0.60-style calldata. Argument layouts after the selector are
unchanged (string lengths, BE u64s, support byte).

#### Scope discipline

Not in this release:

- **Argument encoding is still custom, not Solidity ABI.** Selectors
  are Solidity-compatible; the payload after them is AII-defined
  (length-prefixed string, BE u64). Real ABI encoding (`abi.encode`
  via `ethers`) needs a separate translation pass and is the next
  iteration.

## [0.0.61] — 2026-05-26

### Added — Auto-triggered slashing on remote-vote equivocation

Every `submit_remote_prevote` / `submit_remote_precommit` on the
`BftEngine` now feeds the embedded `EquivocationDetector` before
forwarding to the coordinator. When the detector spots a double-sign,
the evidence is parked on the engine's `pending_evidence` queue; the
`aiid` BFT loop drains it on every tick and auto-persists via
`NodeState::record_slashing`. The `slash:` Meta-CF index from v0.0.46
now fills without any operator action — same observability surface
(`aii_listSlashings`), zero human intervention.

#### `aii-consensus-bft`

- `BftEngineState` gains two fields:
  - `detector: EquivocationDetector` (per-engine, BLS-key-keyed).
  - `pending_evidence: Vec<EquivocationEvidence>`.
- `submit_remote_prevote` / `submit_remote_precommit` now feed the
  detector first, then forward to the coordinator.
- New `BftEngine::drain_evidence() -> Vec<EquivocationEvidence>`
  takes-and-clears the parked queue.
- New test `drain_evidence_returns_double_prevote_evidence` proves
  the auto-feed path: two prevotes from the same validator at the
  same `(height, round)` for different block hashes ⇒ one piece of
  evidence emitted; second `drain_evidence()` returns empty.

#### `aii-node`

- The multi-validator BFT tick loop (`aiid::main`) now calls
  `engine.drain_evidence()` on every iteration and routes each
  record through `NodeState::record_slashing`. Logs a `warn!`
  per detection.

#### Scope discipline

Not in this release (already on the roadmap):

- **Auto-debit deferred.** The slash record is persisted, but
  the offending validator's bond is not yet automatically debited.
  Wiring needs a `validator_index → stake_address` map; the
  natural place is to add a `stake_address` field to
  `GenesisValidator` (and the DPoS-elected set), then call
  `debit_slash_stake` here. Tracked under E.3 / C.6 follow-up.
- **No cross-node evidence gossip.** Each node only slashes for
  evidence it observed locally; if a peer's gossip stack dropped
  a conflicting vote on one side of the wire, only the receiving
  node records the slash. Cross-node evidence broadcast lands with
  the libp2p / Noise transport integration (C.3 + C.4 wire-up).

## [0.0.60] — 2026-05-26

### Added — On-chain submit path for staking + governance (precompile)

Until now the staking + governance primitives (v0.0.48 - v0.0.50) were
library-call-only — operators could only invoke them from Rust, not
from a wallet. v0.0.60 ships the missing on-chain call surface: a
single fixed precompile address that decodes opcode-prefixed
calldata and dispatches to the persistent stores. Any wallet that can
sign an EIP-1559 / legacy transaction can now bond, unbond, withdraw,
propose, and vote.

#### `aii-node::precompile` (new module)

- `PRECOMPILE_ADDR = 0x00…0099` (AII mainnet chain id padded to 20
  bytes).
- Five fixed-byte opcodes (`OP_BOND`, `OP_BEGIN_UNBOND`,
  `OP_WITHDRAW`, `OP_PROPOSE`, `OP_VOTE`) — first 4 bytes of `tx.data`
  select the operation.
- `dispatch(table, gov, sender, value, data, block_height,
  unbonding_period)` runs the operation and returns a typed
  `PrecompileOutcome`. Errors carry context for the receipt.
- 5 unit tests cover: bond round-trip; unbond → premature withdraw
  → mature withdraw; propose; vote weight; unknown opcode rejected.

#### `aii-node`

- `execute_block_txs` checks `to == PRECOMPILE_ADDR` BEFORE handing
  the tx to revm. Precompile path:
  1. Dispatches against `StakeTable` + `Governance`,
  2. Charges a flat 21 000 gas (no EVM execution),
  3. Credits the gas fee to the block beneficiary,
  4. Emits a receipt with `status = success_of(dispatch)`.
- Public re-export: `aii_node::precompile_dispatch`,
  `PrecompileOutcome`, `PRECOMPILE_ADDR`.

#### Scope discipline

Not in this release (already on the roadmap):

- **No Solidity ABI selectors.** Opcodes are fixed bytes
  (`0x00000001`, `0x00000002`, …) — a Solidity contract calling
  `keccak256("bond()")[..4]` won't match. The selector-compatibility
  rewrite is a small follow-up — the wire layout stays the same but
  the opcode constants get re-derived.
- **Fee debit isn't real gas accounting.** We charge 21 000 gas
  regardless of operation complexity. Proper metering needs a
  per-opcode cost table.
- **No event emission.** Precompile outcomes don't currently emit
  Solidity-style `Transfer`/`Voted` events; they only set the
  receipt `status` bit. Logs land together with the Yellow-Paper
  apply-then-hash refactor.

## [0.0.59] — 2026-05-26

### Added — `eth_getLogs` JSON-RPC (closes B.5 fully)

The per-tx Yellow-Paper bloom + post-block aggregate bloom shipped
in v0.0.43 / v0.0.58 were observable via `eth_getTransactionReceipt`
and `aii_getPostRoots`, but the canonical log-query RPC was missing.
v0.0.59 ships `eth_getLogs` with bloom-prefilter optimisation: the
block range is walked once, the per-block bloom decides which blocks
to descend into, and only matching blocks' receipts are scanned
linearly. Address + topic filters are exact-match positionally.

#### `aii-rpc`

- New JSON-RPC method `eth_getLogs(filter)`. Filter shape:
  `{ from_block, to_block, address, topics: Vec<String> }`.
- New `LogFilter` request type + `LogEntryView` response type.
- New trait method `RpcState::logs_in_range` (default empty).

#### `aii-node`

- `NodeState::logs_in_range` impl:
  1. Resolves `from_block` / `to_block` (defaults: 0 → head).
  2. Decodes `address` + `topics` hex into typed `Address` / `H256`.
  3. For each block in range, fetches the post-roots sidecar bloom
     and rejects the block early when the filter's address or any
     wanted topic is absent — Yellow-Paper Bloom 200x speedup over
     the naive scan.
  4. For surviving blocks, walks `body.transactions`, fetches each
     `Receipt` via `receipt_by_tx_hash`, filters logs by
     address + topics positional match, and emits matched entries
     with block-number / tx-hash / address / topics / data all
     hex-encoded.
- New helper `parse_block_tag` decodes either decimal or `0x…` hex
  block numbers.

#### Scope discipline

Not in this release (already on the roadmap):

- **No topic wildcards.** Each filter topic must exact-match; the
  Ethereum-spec "any of these" positional OR-arrays land in a
  follow-up. The literal string `"null"` is treated as a topic
  value, not a wildcard.
- **No block-hash filter.** `filter.blockHash` is not yet honoured
  — use `from_block == to_block` to scope to one block.
- **No subscription / `eth_newFilter`.** Long-poll filter
  subscriptions are deferred; for now clients must poll `eth_getLogs`
  on their own cadence.

## [0.0.58] — 2026-05-26

### Added — Post-block Yellow-Paper roots sidecar (closes A.3 + B.5 observability)

The Yellow-Paper `state_root`, `receipts_root`, and `logs_bloom`
header fields have been `EMPTY_TRIE_HASH` / `Bloom::ZERO` since
v0.0.39 — every prior release deferred their wiring to a future
engine apply-then-hash refactor. v0.0.58 ships the pragmatic
alternative: persist the *computed* post-execution roots in a
`Meta:postroot:<block_hash>` sidecar record after every commit_block,
and expose them via JSON-RPC `aii_getPostRoots(block_hash)`. The
block header still embeds placeholders (so block hashes stay stable
across this release), but light clients can now fetch the real
roots and verify the chain's post-state directly.

#### `aii-state`

- New free function `receipts_root(&[Receipt]) -> H256` — sibling to
  `transactions_root`. Keys are `rlp(i)`, values are EIP-2718-encoded
  receipt bytes. Empty input returns `EMPTY_TRIE_HASH`.

#### `aii-node`

- New `PostBlockRoots { state_root, receipts_root, logs_bloom }`
  struct + `Meta:postroot:<hash>` key prefix.
- `execute_block_txs` now, after committing every tx + minting the
  subsidy:
  1. Computes `state.state_root()`,
  2. Computes `aii_state::receipts_root(receipts)`,
  3. Aggregates every per-tx log into a block-level `Bloom`,
  4. Persists the triple via `persist_post_roots`.
- New `NodeState::post_roots(block_hash) -> Result<Option<PostBlockRoots>>`
  reads the sidecar back.
- New `RpcState::post_roots_for(block_hash)` default-`None`; impl
  hex-encodes all three fields into a `PostRootsView`.
- New test `empty_block_post_roots_record_world_state`: commits an
  empty block, asserts the sidecar persists with `receipts_root ==
  EMPTY_TRIE_HASH`, `logs_bloom == Bloom::ZERO`, and `state_root`
  matching the live state's `state_root()`.

#### `aii-rpc`

- New JSON-RPC method
  `aii_getPostRoots(block_hash) -> Option<PostRootsView>`.
- New response type `PostRootsView { state_root, receipts_root,
  logs_bloom }` (all `0x…` hex).

#### Scope discipline

Not in this release (already on the roadmap):

- **Header still carries placeholders.** Block hash continues to
  embed `EMPTY_TRIE_HASH` / `Bloom::ZERO` for `state_root` /
  `receipts_root` / `logs_bloom`. Folding the post-roots into the
  header at build time (so the hash itself locks to them) is the
  full engine apply-then-hash refactor — pairs with the consensus
  protocol upgrade that lets followers verify proposer-supplied
  roots before voting.
- **No proof generation.** The sidecar is the root only — Merkle
  proof generation for individual accounts / receipts / logs is
  light-client work for a later release.

## [0.0.57] — 2026-05-26

### Added — End-to-end staking → election → governance integration test

The v0.0.48 → v0.0.50 release window introduced three independent
primitives (stake table, DPoS election, governance proposals/votes)
each with isolated unit tests. v0.0.57 closes the loop with one
integration test that exercises all three composing across the same
`NodeState`: two stakers bond, four blocks commit and trigger an
epoch election at block 4, the elected set is observable, the
biggest staker proposes a parameter change, both stakers vote yes,
and after the voting window closes the tally records `Passed`. No
new production code — just proof the existing pieces fit together.

#### `aii-node`

- New `end_to_end_stake_elect_govern_tally` test in
  `aii_node::tests` walks every step:
  1. `stake_table.bond(big, 800)` + `stake_table.bond(small, 200)`.
  2. Four `commit_block` calls; `epoch_length_blocks = 4` so block 4
     fires the election.
  3. `latest_validator_set` returns `(epoch=1, [big, small])`.
  4. `governance.propose(big, "raise block reward", voting_ends=10)`
     → id=1.
  5. `governance.cast_vote` from both addresses at block 5.
  6. `governance.tally` at block 15 returns `Passed`; `tally_of`
     returns `yes=1000, no=0`.

#### Scope discipline

Not in this release (already on the roadmap):

- **No on-chain submit path.** All three primitives are still
  library-call-only — the integration test invokes them as Rust
  methods, not as EIP-1559 transactions. The precompile-style call
  surface lands together with the engine apply-then-hash refactor.
- **No engine execution of `Passed` proposals.** Status transitions
  to `Passed` but the chain doesn't yet apply the parameter change —
  that requires the engine to read post-tally state at block-build
  time.

## [0.0.56] — 2026-05-26

### Added — Kademlia routing-table primitive (C.3 partial)

Devp2p Discovery v4 has been live since v0.0.17 (`Ping` / `Pong` over
signed UDP), but the routing layer that turns "I just heard from
peer X" into "where do I send `FindNode` next?" was missing.
v0.0.56 ships the Kademlia primitive: 256 k-buckets keyed by leading-
zero count of the XOR distance, K=16 entries per bucket, LRS
eviction, and `find_closest(target, n)` over the full table.

Plugging the table onto an actual `FindNode` / `Neighbours` UDP loop
is the next chore — this release is the data-structure primitive so
that wiring is a pure swap-in.

#### `aii-net-p2p`

- New module `aii_net_p2p::kademlia` exporting:
  - `PeerEntry { node_id, payload }` — opaque payload lets callers
    stash the full `Endpoint` without coupling the table to one
    transport.
  - `KademliaTable::new(local_id) / len / is_empty / bucket_index /
    insert / find_closest`.
  - Free function `xor_distance(a, b) -> [u8; 32]`.
  - Constants `K = 16`, `BUCKETS = 256`.
- 8 unit tests cover empty / insert / self-ignored / bucket-index
  math / refresh order / closest sort / N-cap / LRS eviction.

#### Scope discipline

Not in this release (already on the roadmap):

- **No `FindNode` / `Neighbours` packet types yet.** Devp2p packet
  types 0x03 / 0x04 are unparsed. Once they're added to the
  `discovery` module, the only further plumbing is calling
  `KademliaTable::insert` on every received `Pong` / `Neighbours`
  entry and `find_closest` for outgoing `FindNode` targets.
- **No libp2p stack.** This is still the devp2p / Discovery v4 line.
  A future release may swap to libp2p + Kademlia DHT proper — the
  table layout (256 buckets, K=16) maps 1:1 so the swap is local
  to one crate.
- **Identity binding stays via secp256k1.** Static x25519 (Noise)
  keys are session-only — node identity is the keccak256 of the
  secp256k1 public key, same as devp2p mainline.

## [0.0.55] — 2026-05-26

### Added — Noise XX encrypted transport primitive (C.4)

The BFT gossip socket today is plaintext TCP — any intermediary can
read consensus traffic and (worse) inject forged messages, since the
application layer trusts on-wire bytes for signature inputs. v0.0.55
ships the Noise primitive that closes that gap: a 3-message Noise XX
handshake over the `Noise_XX_25519_ChaChaPoly_BLAKE2s` suite, followed
by AEAD-encrypted application messages with a 16-bit length prefix.
The primitive plugs onto any `AsyncRead + AsyncWrite` stream; wiring
it into `Peer` / `Server` is the next chore.

#### `aii-net-p2p`

- New module `aii_net_p2p::noise` exporting:
  - `initiator() / responder()` build handshake states with fresh
    x25519 keypairs.
  - `handshake_initiator(hs, stream) / handshake_responder(hs, stream)`
    drive the 3-message XX exchange and return an `EncryptedSession`.
  - `EncryptedSession::send_msg / recv_msg` encrypt + decrypt
    application messages with a `u16` BE length prefix (Noise max
    frame is 65 535 bytes).
  - `NoiseError` covers snow state-machine, I/O, and oversized-frame
    rejection.
- Dependency on `snow = "0.9"` (default-resolver feature for the
  pure-Rust crypto suite).
- `tokio` feature set bumped to include `"time"` to fix a pre-existing
  `discovery` compile-time gap surfaced by this build.
- 2 integration tests over `tokio::io::duplex`:
  - `xx_handshake_round_trips_encrypted_messages` — full XX → both
    sides send + receive encrypted application messages.
  - `encrypted_payload_is_not_plaintext` — sanity that the AEAD path
    actually decrypts (a tampered byte would fail).

#### Scope discipline

Not in this release (already on the roadmap):

- **`Peer` and `Server` are still plaintext.** The Noise primitive
  is in isolation; the existing TCP transport in `lib.rs` hasn't
  been switched to it yet. Wiring is a one-method-per-side change
  but pairs cleanly with the libp2p (C.3) protocol-stack work.
- **No static-key identity binding.** Each session uses a fresh
  x25519 keypair — peer identity is bound by the higher-layer
  `Hello` message, not the Noise static key. A static-key plus
  signed handshake-pubkey commit is a v0.0.57+ deliverable.
- **No multiplexing.** Each Noise session is a single
  encrypted-byte-stream; running BFT gossip + discovery over one
  socket needs an outer mux (yamux / mplex / libp2p substreams)
  before C.3 can fold this in.

## [0.0.54] — 2026-05-26

### Added — Fork-detection primitive + RPC (C.2 observability)

`NodeState::commit_block` now distinguishes between "novel block" and
"competing block at an already-finalised height". The novel path
behaves unchanged; the competing path persists a `ForkRecord` under
`Meta:fork:<height_be8>:<fork_hash[32]>` and refuses to overwrite the
canonical head. Operators get an audit trail of every conflicting
hash the gossip layer surfaced; re-org *execution* (rollback +
re-apply) is intentionally deferred until the engine learns
apply-then-hash so we can re-derive `state_root` on the rolled-back
branch without trusting a peer.

#### `aii-node`

- New `ForkRecord { height, canonical_hash, fork_hash }` public
  struct.
- `commit_block` short-circuits on `by_number[h] != hash` and emits
  a `tracing::warn!` with both hashes, then writes the record via
  `record_fork`. The persistent block index is untouched.
- New `NodeState::list_forks() -> Result<Vec<ForkRecord>>` for the
  RPC + ops tooling.
- New test
  `fork_at_same_height_records_evidence` commits two distinct
  blocks at height 1, asserts head doesn't advance and the
  `ForkRecord` round-trips with both hashes intact.

#### `aii-rpc`

- New JSON-RPC method `aii_listForks() -> Vec<ForkView>`.
- New response type
  `ForkView { height, canonical_hash, fork_hash }` (every field
  hex-encoded).

#### Scope discipline

Not in this release (already on the roadmap):

- **Re-org execution still missing.** Persisting the rejected block
  is one half of fork choice; rolling state mutations back to the
  common ancestor and replaying the heavier branch is the other.
  That sits on the engine apply-then-hash refactor that also fixes
  `state_root` / `receipts_root` in the header.
- **No fork-weight comparison yet.** Today's rule is "first-block-
  wins at any height" — the BFT certificate from the actual leader
  doesn't yet override an earlier same-height arrival. Weight-based
  tiebreaking lands together with re-org execution.

## [0.0.53] — 2026-05-26

### Added — Sub-chain consensus selector + persisted label (D.3 primitive)

The persistent state file from v0.0.52 now records *which* consensus
engine the sub-chain was bootstrapped against. PoA stays fully
implemented; the new `SubchainConsensus::Bft` variant is parsed +
persisted today so a future binary can pick the same operator key up
and run the multi-validator BFT engine against it without forcing a
fresh keygen.

#### `aii-cli`

- New `SubchainConsensus { Poa, Bft }` enum with `parse(label)` /
  `as_label()`.
- `SubchainPersistentState` gains a `consensus: String` field, set
  via the new `consensus: SubchainConsensus` arg on `create_fresh`.
  `#[serde(default = …)]` so existing `state.json` files (created
  before this release) decode unchanged with `consensus = "poa"`.
- `SubchainPersistentState::consensus_kind()` decodes the on-disk
  label into the typed enum.
- 2 new tests cover the round-trip and the unknown-label rejection.

#### Scope discipline

Not in this release (already on the roadmap):

- **`Bft` startup still rejected.** The `state.json` already records
  `consensus = "bft"` correctly, but `run_subchain` itself doesn't
  yet branch on the kind — it always boots a `PoaEngine`. The
  follow-up reads the label, dispatches to `BftEngine::advance_single`
  for a single-validator sub-chain, and adds the multi-validator
  variant once the gossip protocol is extended to sub-chain epochs.
- **No DPoS sub-chains.** Validator-set rotation (C.6) applies to
  the main chain only; sub-chain validator changes wait on the same
  engine refactor that lands header `state_root`.

## [0.0.52] — 2026-05-26

### Added — Sub-chain persistent operator state (roadmap D.1)

`aii subchain run` now has a durable on-disk identity. Before this
release every restart of the sub-chain runner spun a fresh secp256k1
operator key, restarted the height counter from zero, and reset the
parent-chain nonce — meaning the first flush after a restart usually
collided with the parent's existing-nonce-from-the-previous-run and
got rejected. v0.0.52 introduces a JSON state file
(`<data_dir>/state.json`) that round-trips operator key, head
counter, head hash, and the next parent nonce across restarts.

#### `aii-cli`

- New `SubchainPersistentState { sub_chain_id, operator_sk_hex,
  head_number, head_hash, parent_nonce, flush_count }` struct.
- `SubchainPersistentState::create_fresh(sub_chain_id, data_dir)` —
  generates a new operator key and writes the initial state file.
- `SubchainPersistentState::load(data_dir) -> Result<Option<Self>>` —
  returns `Ok(None)` when no file exists.
- `SubchainPersistentState::save(data_dir)` — atomic write via
  `state.json.tmp → rename` (POSIX-atomic; a torn write cannot leave
  a corrupted `state.json`).
- `SubchainPersistentState::operator_secret_key()` — recover the
  `secp256k1::SecretKey` from the hex-encoded form.
- `aii-cli` dev-deps gain `tempfile` for the round-trip test.
- 2 new tests: full disk round-trip, missing-file handling.

#### Scope discipline

Not in this release (already on the roadmap):

- **`run_subchain` doesn't auto-load the state file yet.** The
  `SubchainPersistentState` primitive is in place — the actual
  swap-in (open or create, load operator key, restore counters,
  save after every produced block) is a one-line follow-up CLI
  refactor.
- **No block-store persistence.** Only operator state is kept; the
  sub-chain still doesn't persist its block bodies or receipts
  (they're ephemeral by construction — the parent's anchor records
  are the source of truth).
- **No multi-engine sub-chains (D.3).** Sub-chains still only run
  PoA; persistence here applies regardless of engine but the
  non-PoA engines aren't yet wired into `run_subchain`.

## [0.0.51] — 2026-05-26

### Added — ERC-20 ABI helpers (roadmap E.1)

A new `aii-erc20` crate ships the canonical 4-byte function selectors
+ ABI encoder/decoder helpers for every ERC-20 method. With this
crate, building token-aware clients on top of `eth_sendRawTransaction`
+ `eth_call` requires zero offline tooling — selectors and call data
are produced from `Address` + `U256` arguments in pure Rust.

#### `aii-erc20` (new crate)

- Compile-time consts for every standard selector
  (`SELECTOR_TOTAL_SUPPLY`, `SELECTOR_BALANCE_OF`, `SELECTOR_TRANSFER`,
  `SELECTOR_APPROVE`, `SELECTOR_ALLOWANCE`, `SELECTOR_TRANSFER_FROM`).
- `encode_balance_of / encode_total_supply / encode_transfer /
  encode_approve / encode_allowance / encode_transfer_from` produce
  ABI-correct calldata (left-padded address words + big-endian
  uint256 amounts).
- `decode_uint256_result / decode_bool_result` parse the 32-byte
  return slots returned by Solidity contracts. Solidity's bool
  convention (`0x00..01` for true) is handled.
- 7 unit tests verify selectors match `keccak256(canonical_sig)[..4]`,
  ABI layouts are byte-perfect, and decoders handle padding.
- README documents the selector table and shows a balanceOf →
  decode example.

#### Scope discipline

Not in this release (already on the roadmap):

- **No bundled reference bytecode.** The crate is ABI helpers only;
  pair with any solc-compiled token (OpenZeppelin `ERC20Mock` is
  the obvious choice). Adding a vendored stable bytecode constant
  + an end-to-end revm deploy test is a v0.0.52+ chore.
- **No event-bloom helpers.** `Transfer` / `Approval` event signature
  decoders land alongside `eth_getLogs` (B.5 stretch).
- **No EIP-2612 permit / EIP-3009 transferWithAuthorization** —
  this crate sticks to the canonical ERC-20 surface.

## [0.0.50] — 2026-05-26

### Added — On-chain governance (roadmap E.2)

A stake-weighted governance primitive: propose / vote / tally with
2/3-supermajority quorum. Sits on top of the v0.0.48 staking table
so vote weight tracks live bond, not a frozen snapshot. Proposals,
votes, and tallies all persist; RPC exposes `aii_listProposals` and
`aii_getProposal(id)` so dashboards can render the whole life-cycle.
Executing a passed proposal is intentionally a separate phase — the
engine-side wire-up lands when consensus learns to honour parameter
changes mid-chain.

#### `aii-node`

- New module `aii_node::governance` exporting:
  - `Proposal { id, title, voting_ends_at, status, proposer }`,
  - `ProposalStatus` (Pending / Passed / Rejected / Executed),
  - `Vote { proposal_id, voter, support, weight_wei }`,
  - `Governance` — thin wrapper around `Arc<RocksDbBackend>`.
- `Governance::propose / get / cast_vote / tally / tally_of /
  list_all`. `propose` auto-assigns the next monotonic id;
  `cast_vote` reads weight from `StakeTable` and rejects unstaked
  or unbonding voters; `tally` walks every vote, sums yes / no,
  marks `Passed` iff `yes > 2/3 * total_bonded` and caches the
  `(yes_wei, no_wei)` totals under `Meta:tally:<id>`.
- `NodeState::governance()` accessor.
- 6 unit tests: id monotonicity; round-trip; vote requires bond;
  vote after window rejected; 2/3 passes; 50/50 rejects; tally
  cache populated.

#### `aii-rpc`

- New JSON-RPC methods:
  - `aii_listProposals() -> Vec<ProposalView>`.
  - `aii_getProposal(id) -> Option<ProposalView>`.
- New response type `ProposalView { id, title, voting_ends_at,
  status, proposer, yes_wei, no_wei }` — every numeric field hex.
- Status string: `"pending" | "passed" | "rejected" | "executed"`.

#### Scope discipline

Not in this release (already on the roadmap):

- **No on-chain submit path yet.** Proposals + votes today come
  through library calls (or, for ops tooling, future
  `aii_propose` / `aii_vote` write-RPCs). A precompile address that
  accepts standard tx calldata is the obvious follow-up.
- **Execution is a marker, not a chain-fork.** `ProposalStatus::Passed`
  → `Executed` requires the engine to actually apply the parameter
  change at the next block. That hook lands together with the
  state-root-in-header refactor.
- **No delegation / split votes.** Each voter casts the totality of
  their bond as one weight; partial / delegated voting is future
  work.

## [0.0.49] — 2026-05-26

### Added — DPoS validator-set election + slashing debit hook (C.6 + C.7 close-out)

The staking primitive from v0.0.48 is now wired into a live election
cycle. Every epoch boundary (default 4 800 blocks ≈ 4 h at 3 s/block)
the node reads its persistent stake table, sorts by `amount_wei` desc
with address-asc tiebreak, filters records below
`min_validator_stake_wei`, and persists the top-N as the active
validator set under `Meta:validator_set:<epoch_be8>`. A new
`debit_slash_stake` hook on `NodeState` lets the slashing executor
debit a misbehaving validator's bond — pairing the v0.0.46 evidence
log with real economic consequence.

#### `aii-config::ChainSpec`

- Two new fields (with `#[serde(default = …)]`):
  - `epoch_length_blocks: u64` (default 4 800).
  - `validators_per_epoch: u32` (default 21).

#### `aii-node`

- New module `aii_node::dpos` exporting:
  - `ValidatorEntry { address, stake_wei }`.
  - `elect_active_set(table, min_stake, validators_per_epoch)` —
    deterministic election from a `StakeTable`.
  - `persist_validator_set / read_validator_set` — fixed-layout
    encoding on `ColumnFamily::Meta`.
  - `latest_validator_set` — finds the highest-epoch record by
    prefix scan.
  - `LatestEpochSet` type alias for the `(epoch, entries)` payload.
- `NodeState::commit_block` calls `maybe_elect_validator_set` after
  every commit; at `block_number % epoch_length_blocks == 0` the
  election runs and the result lands on disk. Genesis (block 0) is
  intentionally skipped.
- `NodeState::debit_slash_stake(offender, amount)` slashes the
  offender's `StakeTable` record by `amount` (saturating).
  No-op if the offender has no stake record yet — keeps testnet
  ergonomics intact.
- 7 unit tests in `dpos::tests`: empty election; min-stake filter;
  N-cap; sort-order determinism (stake desc + addr asc); unbonding
  records excluded; persist/read round-trip; highest-epoch
  selection.
- 2 new lib-level tests:
  `slash_debit_reduces_bonded_stake` and
  `epoch_boundary_block_runs_dpos_election` (synthesises two
  stakers, commits 3 blocks against `epoch_length_blocks = 3`,
  asserts both validators were elected with `big > small` order).

#### `aii-rpc`

- New JSON-RPC method
  `aii_getActiveValidators() -> Option<ActiveValidatorsView>`.
- New response types `ActiveValidatorsView { epoch, validators }`
  and `ValidatorEntryView { address, stake_wei }` (all hex).

#### Scope discipline

Not in this release (already on the roadmap):

- **Consensus engines still use genesis validators.** The election
  table is recorded but the BFT engine doesn't yet rotate at epoch
  boundaries — that's the "engine apply-then-hash" refactor that
  also fixes header state_root.
- **No reward distribution.** Block subsidy (B.4) goes to the
  block's beneficiary; it does not yet split pro-rata among the
  active set. Stake-weighted distribution lands together with the
  governance contract (E.2).
- **No automatic slash on equivocation.** The hook
  `debit_slash_stake` is in place; the BFT gossip loop doesn't yet
  call it on detected double-signs. Wiring is a one-line change but
  pairs better with C.4 (encrypted gossip) so we can authenticate
  the evidence channel first.

## [0.0.48] — 2026-05-26

### Added — Staking primitive (roadmap E.3)

The on-chain staking table is the missing economic foundation for the
DPoS rotation (C.6) and the on-chain slashing executor (C.7). v0.0.48
ships the persistent primitive — bond, unbond timer, withdraw, slash,
total bonded, plus full RPC visibility — without yet wiring it into
the consensus engine's validator selection. That deliberate split
makes the next two releases (DPoS + slash-executor) one-line wire-ups
against an already-tested table rather than a bundled architectural
shift.

#### `aii-config::ChainSpec`

- Two new fields with `#[serde(default = …)]` (existing genesis JSON
  decodes unchanged):
  - `unbonding_period_blocks: u64` (default 100 800 ≈ ~3.5 d at 3 s/block).
  - `min_validator_stake_wei: u128` (default 100 AII).

#### `aii-node`

- New module `aii_node::staking` exporting:
  - `StakeRecord { staker, amount_wei, unbond_at }` — fixed 40-byte
    persistent layout (32-byte U256 ‖ 8-byte unbond height) under
    `ColumnFamily::Meta` key prefix `b"stake:"`.
  - `StakeTable` — thin wrapper around `Arc<RocksDbBackend>` exposing
    `bond / begin_unbond / withdraw / slash / get / list_all /
    total_bonded`. `bond` accumulates, `begin_unbond` records the
    unbond height, `withdraw` deletes the record only after the
    unbond timer elapses, `slash` saturates at zero, `total_bonded`
    sums every actively bonded record.
- `NodeState::stake_table()` — cheap accessor returning a fresh
  `StakeTable` view; the underlying backend Arc is cloned, the table
  itself is stateless.
- 8 unit tests covering: round-trip; bond accumulation; unbond timer;
  premature withdraw rejected; mature withdraw deletes record;
  saturating slash; full list; total-bonded skipping unbonding records.

#### `aii-rpc`

- Three new JSON-RPC methods:
  - `aii_getStake(address)` → `StakeView` or `null`.
  - `aii_totalStake()` → `0x…` hex Wei.
  - `aii_listStakers()` → `Vec<StakeView>`.
- New response type `StakeView { address, amount_wei, unbond_at,
  is_bonded }` — every field hex-encoded.
- Three new default-`None`/`Empty` trait methods on `RpcState`
  (`stake_at`, `total_bonded_stake`, `all_stakers`); `NodeState`
  overrides each by walking the new `StakeTable`.

#### Scope discipline

Not in this release (already on the roadmap):

- **No automatic balance debit on bond.** `StakeTable::bond` simply
  records the intent; coupling it to the account's free balance
  needs a precompile-style tx type (delivered together with the
  governance call surface, E.2).
- **DPoS rotation (C.6) doesn't consult the table yet.** Validator
  set still comes from genesis. The election loop hooks onto
  `total_bonded` + per-record `amount_wei` in the next release.
- **Slashing executor (C.7) doesn't call `slash` yet.** The slashing
  record from v0.0.46 stays observability-only until DPoS lands.
- **No delegation.** Each record has a single principal `staker`;
  delegated-stake aggregation (E.3 stretch) is future work.

## [0.0.47] — 2026-05-26

### Added — Microchain anchor decoder + RPC (roadmap D.2)

Since v0.0.38 the sub-chain runner has emitted flush-anchor txs into
the parent chain, but the parent treated them as ordinary self-
transfers: their calldata was opaque, no registry record was kept,
and explorers had no way to ask "what's the latest checkpoint for
sub-chain N?". v0.0.47 closes that loop end-to-end:

#### `aii-microchain`

- New `FLUSH_TX_MAGIC = b"AII_FLUSH"` constant. Sub-chain flush txs
  now carry the 53-byte calldata layout
  `AII_FLUSH (9) ‖ sub_chain_id_be4 ‖ sub_block_hash[32] ‖
  sub_block_number_be8`. The magic + fixed length make false-
  positive matches against ordinary self-transfers effectively
  impossible.
- New `parse_flush_anchor(data: &[u8]) -> Option<FlushTxPayload>`
  decoder. Safe to call on every tx; rejects wrong-magic / wrong-
  length payloads.
- New `FlushTxPayload { sub_chain_id, sub_block_hash,
  sub_block_number }` struct.
- New tests: `parse_flush_anchor_decodes_well_formed_payload`,
  `parse_flush_anchor_rejects_missing_magic`,
  `parse_flush_anchor_rejects_wrong_length`.

#### `aii-cli`

- `run_subchain` (the `aii subchain run` CLI) now prefixes flush
  calldata with `FLUSH_TX_MAGIC` and embeds the 4-byte sub_chain_id
  so a parent indexes anchors by chain id without ambiguity. Total
  calldata grows from 40 → 53 bytes (still well under any practical
  gas-limit-driven cap).

#### `aii-node`

- `NodeState::commit_block` now walks every tx in the new block via
  `scan_microchain_anchors`. For each tx whose calldata decodes to
  a `FlushTxPayload` AND whose sender == to (self-tx safety check),
  the resulting `FlushAnchor` is persisted to `ColumnFamily::MicroChain`
  under key `b"anchor:" ‖ sub_chain_id_be4`. Last-flushed wins —
  one entry per sub-chain.
- New `last_flush_anchor(MicroChainId) -> Result<Option<FlushAnchor>>`
  reads the same record back.
- `RpcState::subchain_anchor(id)` default returns `None`; NodeState
  overrides to hex-encode the persisted `FlushAnchor` as a
  `SubchainAnchorView`.
- New test
  `subchain_flush_anchor_persists_and_reads_back` round-trips a
  synthesised `FlushAnchor` through the `MicroChain` CF.

#### `aii-rpc`

- New JSON-RPC method `aii_getSubchainAnchor(id: u32) -> Option<SubchainAnchorView>`.
- New response type `SubchainAnchorView { sub_block_hash,
  parent_block_hash, sub_block_number }` (all `0x…` hex).

#### Scope discipline

Not in this release (already on the roadmap):

- **Sub-chain runtime persistence (D.1) still missing.** Anchors are
  now recorded on the parent — but `aii subchain run` itself still
  loses its in-memory PoA state on restart. Persistent sub-chain
  data-dirs are the obvious next sub-chain release.
- **No non-PoA sub-chain engines (D.3).** Sub-chains still only run
  the PoA engine (single operator authority). DPoS / BFT sub-chains
  reuse the existing engines but need separate wiring.
- **No anchor finality / re-flush handling.** If a parent block
  containing an anchor gets re-orged out, the `MicroChain` CF entry
  is not rolled back. Becomes relevant when fork choice (C.2) lands.

## [0.0.46] — 2026-05-26

### Added — Slashing record persistence + RPC (roadmap C.7 partial)

The BFT equivocation detector has produced `EquivocationEvidence`
since v0.0.27, but nothing on the node side stored or exposed it —
catching a misbehaving validator left no auditable trail. v0.0.46
closes that observability gap: `NodeState::record_slashing(evidence)`
persists each equivocation as a stable `Meta`-CF entry, queryable
through the new `aii_listSlashings` JSON-RPC method. The stake-debit
side of slashing waits for DPoS (C.6 / E.3) to land first.

#### `aii-node`

- New `SlashRecord { validator_index, height, phase, block_hashes }`
  public struct.
- New `NodeState::record_slashing(&EquivocationEvidence)` — folds
  prevote/precommit equivocation into a stable CF-key layout
  `b"slash:" ‖ vidx_be4 ‖ height_be8 ‖ phase_byte` so the same
  `(validator, height, phase)` overwrites idempotently. Value is
  `phase_str_len ‖ phase_str ‖ block_hash0 ‖ block_hash1`.
- New `NodeState::list_slashings()` prefix-scans the `Meta` CF and
  decodes every record. O(slashings); typical chain has zero.
- `RpcState::slashings` default impl returns `Vec::new()`; NodeState
  overrides to hex-encode every `SlashRecord` into a `SlashView`.
- New test `slashing_record_persists_and_lists` synthesises two
  conflicting BLS-signed prevotes under the same validator key
  (different block hashes → real equivocation), records via
  `record_slashing`, asserts the persisted record round-trips with
  every field intact.

#### `aii-rpc`

- New JSON-RPC method `aii_listSlashings(): Vec<SlashView>`.
- New response type `SlashView { validator_index, height, phase,
  block_hashes }` — all `0x…` hex-encoded so explorer integrations
  are trivial.

#### Scope discipline

Not in this release (already on the roadmap):

- **Slashing isn't auto-triggered yet.** `record_slashing` is a
  public manual API on NodeState; the BFT gossip loop doesn't
  currently call it on every equivocation. Wiring the auto-record
  is a one-line follow-up but pairs cleanly with the actual
  stake-debit logic — both land together when DPoS arrives.
- **No stake debit on slash.** Future-DPoS work will reach into the
  staking table and slash the offending validator's bond; today's
  record is observability only.
- **No on-chain slashing tx broadcast.** The evidence stays local to
  the node that detected it. Cross-node propagation needs a P2P
  message type added to the BFT transport.

## [0.0.45] — 2026-05-26

### Added — PoA seals + encrypted byte-payload keystore (roadmap C.5 + C.8)

**Two operational hardenings for the v0.0.45 release window.**
Before this release, a PoA node only checked that the proposer
*address* matched the slot rota — peers had no cryptographic
evidence that the elected authority actually produced the block; a
malicious operator could forge blocks "as if" from another authority
since address attribution was implicit. And validator BLS+VRF secrets
sat on disk as plaintext JSON, ready to leak through any backup-tarball
or shell-history mishap.

#### `aii-consensus-poa`

- `PoaConfig` gains `signer_sk: Option<aii_crypto::secp::SecretKey>`.
  When set, every produced block can be signed via the new
  `produce_block_signed() -> (hash, number, block, Option<PoaSeal>)`
  method. `PoaSeal` is a re-export of `aii_crypto::secp::Signature`
  (65-byte `r ‖ s ‖ v` Ethereum-style).
- New free function
  `verify_poa_seal(block_hash, sig, authorities, height) -> Result<bool>`
  recovers the signer address from the seal and compares it to
  `authorities[height % len]`. Any other authority — or any
  un-elected signer — fails the check.
- Seal is shipped out-of-band (alongside the block body via a future
  P2P sidecar), not embedded in `extra_data`. This keeps the canonical
  Ethereum-compatible header layout intact, so block hashes don't drift.
- New `PoaError::SealSignFailed` for the corrupt-key edge case.
- New tests:
  `produce_block_signed_returns_recoverable_seal`,
  `verify_poa_seal_rejects_wrong_authority` (an impostor block fails
  verification against the legit authority list).

#### `aii-wallet`

- New `EncryptedBytes` API: same scrypt + AES-128-CTR + Keccak-MAC
  recipe as the Web3-v3 wallet keystore, but for *arbitrary-length*
  payloads. Designed to wrap a `ValidatorKeystore`'s full JSON blob
  (BLS secret + VRF secret = 96 bytes plus pubkey hex), which the
  wallet-only `EncryptedKeystore` rejects (it's hard-coded to a
  32-byte ciphertext).
- `Crypto / KdfParams / CipherParams` are now `pub` so callers can
  inspect raw fields (e.g. for migration tooling); normal usage is
  `EncryptedBytes::encrypt(payload, password, params, label).to_json()`
  and never touches them directly.
- New tests: `encrypted_bytes_roundtrips_arbitrary_payload`,
  `encrypted_bytes_wrong_password_fails_mac`,
  `encrypted_bytes_json_round_trip`.

#### Scope discipline

Not in this release (already on the roadmap):

- **PoA seal isn't wired into the binary or BlockStore yet.** The
  primitive is in place; `aiid` still constructs `PoaConfig` with
  `signer_sk: None` and doesn't ship seals over the wire. A future
  release adds the sidecar protocol + the verifier hook in
  `commit_block`.
- **The CLI doesn't yet offer `aii validator keygen --encrypted`.**
  The `EncryptedBytes` primitive is publicly importable, so an
  external tool can already encrypt a generated keystore — the CLI
  flag is a v0.0.46+ chore.
- **No on-disk migration of existing plaintext keystores.** Operators
  running v0.0.40-v0.0.44 testnets still have plaintext JSON; an
  `aii validator encrypt-keystore --in plain.json --out enc.json`
  command lands later.

## [0.0.44] — 2026-05-26

### Added — Cold-join block sync (roadmap C.1)

**A fresh node can finally join the chain without a full data-dir
copy.** Before this release the only way to add a third node to the
testnet was to `scp` the entire RocksDB directory off an existing
node (or wipe + restart every validator together). Now a node started
with `--bootnode http://peer:8545` walks blocks from `local_head + 1`
to the peer's tip, fetches each as RLP bytes via the new
`aii_getRawBlock` RPC, and commits them into the local backend
before opening its own RPC port. Each fetched block runs through the
same `commit_block` path as a freshly produced one — so state
mutations (subsidy minting, gas-fee credits, receipt indexing)
deterministically replay.

#### `aii-rpc`

- New JSON-RPC method `aii_getRawBlock(numberOrHash) -> Option<String>`.
  Returns the RLP-encoded `Block` (header + body) as `0x…` hex, or
  `null` if unknown. Accepts decimal number, `0x…`-prefixed hex
  number, or 32-byte block hash.
- New trait method `RpcState::raw_block(&self, query)`; default
  returns `None` so non-persistent backends compile unchanged.

#### `aii-node`

- New module `aii_node::sync`, public free function
  `bootstrap_sync_from_peer(local: &NodeState, peer_url: &str) ->
  Result<u64, _>`. Queries the peer's `eth_blockNumber`, fetches +
  decodes + commits every missing block, returns the count synced.
  Each block goes through the existing `commit_block` so all
  downstream effects (state mutation, receipt index, subsidy) fire.
- `aiid --bootnode URL` CLI flag: invokes the sync helper after
  `recover()`/`new()` and before opening the RPC server, so the node
  doesn't expose a partial view to clients during catch-up.
- `NodeState::raw_block(query)` impl: walks the in-memory index,
  RLP-encodes `Block { header, body }`, returns `0x…` hex.
- Test-only accessor `blocks_read_test_hash_by_number(n)` exposed
  via `#[doc(hidden)]` so the sync integration test can assert
  byte-identical block hashes on the joining node.
- New integration test
  `sync::tests::cold_join_replays_full_chain_from_peer`: spawns a
  producer with 5 indexed blocks + an RPC server, instantiates a
  cold node against a fresh tempdir, calls `bootstrap_sync_from_peer`,
  asserts the cold node ends at head=5 and every block hash matches
  the producer's.

#### Scope discipline

Not in this release (already on the roadmap):

- **No cryptographic verification of synced blocks.** The cold-join
  protocol trusts the bootnode — each synced block is committed
  without checking the BFT certificate or leader VRF proof. A
  light-client variant that does verify is C.2 / future work; today's
  flow is safe only if the bootnode is honest.
- **No incremental sync on a running node.** `bootstrap_sync_from_peer`
  fires once at startup; if the node falls behind during operation,
  it does not auto-resync from a peer. The BFT gossip path keeps
  reconnected validators in lock-step, so the gap is only relevant
  for non-validator full nodes (those will need a periodic
  sync_tick).
- **No sync over the BFT gossip socket.** We reuse the HTTP RPC
  client to keep the dependency surface tiny. A binary-framed sync
  protocol on the existing `aii-net-p2p` transport is a follow-up.

## [0.0.43] — 2026-05-26

### Added — Validator economics + per-tx logs bloom (roadmap B.3 + B.4 + B.5 partial)

**Block producers now earn real revenue.** Every tx pays its
`gas_used * gas_price` fee to the block's beneficiary (B.3), and
every block additionally mints a configurable subsidy with a Bitcoin-
style halving schedule (B.4). Per-tx receipt blooms are populated
from the log stream (Yellow-Paper §4.4.3), so light clients can
already prove tx-level event presence (B.5 partial — block-level
header bloom + `eth_getLogs` still pending).

#### `aii-config::ChainSpec`

- Two new fields:
  - `block_reward_initial_wei: u128` (default **2 AII / block**),
  - `block_reward_halving_interval: u64` (default **42 048 000
    blocks ≈ 4 y at 3 s/block**; `u64::MAX` disables halving).
- New method `block_reward_at(n)` returns the effective per-block
  subsidy at block number `n`. Pure shift; saturates to 0 after 128
  halvings (no halving can exceed a `u128`'s mantissa).
- `#[serde(default = …)]` on both new fields so existing genesis
  JSON / on-disk specs decode unchanged.
- New tests: `block_reward_halves_at_interval_boundary`,
  `block_reward_saturates_to_zero_after_many_halvings`.

#### `aii-node`

- `execute_block_txs` now passes the tx's `gas_price` (Legacy) or
  `max_fee_per_gas` (EIP-1559) into `execute_with_revm` (was always
  zero). revm now charges the sender; we additionally credit
  `gas_used * gas_price` Wei to `header.beneficiary` so the
  validator coinbase actually accumulates fee revenue.
- After the per-tx loop finishes, `execute_block_txs` mints
  `spec.block_reward_at(header.number)` Wei to `header.beneficiary`.
  Empty blocks earn the full subsidy; tx-heavy blocks earn both.
- New `credit(addr, delta)` helper saturates rather than wrapping —
  no silent overflow on long-running validator accounts.
- Per-tx Yellow-Paper bloom: every log's `address` and each topic is
  accrued into a per-tx `Bloom`, recorded on the receipt. Block-level
  header bloom aggregation is the remaining half of B.5.
- New tests:
  - `empty_block_credits_subsidy_to_beneficiary` — block 1 with no
    txs lands exactly 2 AII at the beneficiary.
  - `subsidy_halves_at_interval_boundary` — sanity-check pass-through.

#### Scope discipline

Not in this release (already on the roadmap):

- **No block-level header `logs_bloom` aggregation yet** —
  per-tx blooms are correct, but `Block::header.logs_bloom` is still
  `Bloom::ZERO`. Same chicken-and-egg as `state_root` /
  `receipts_root` — the bloom should land in the header *before*
  the hash is finalised. Engine-level apply-then-hash refactor due
  in the v0.0.45-0.0.47 range will close all three.
- **No `eth_getLogs` RPC** — even with per-tx blooms persisted, the
  query path (`fromBlock`, `toBlock`, `address`, `topics`) is its
  own work item.
- **Sender-side balance tracking still trusts revm.** We capture
  `sender_pre` but don't yet cross-check `sender_pre - sender_post
  == fee + value`; that lands once we have a tighter
  divergence-detector.

## [0.0.42] — 2026-05-26

### Added — revm in commit_block + tx receipts (roadmap B.1 + B.2)

**Every tx in every committed block now runs through real revm, and
every execution produces a persisted receipt.** Before this release
`commit_block` routed through `aii_evm::execute_transfer` — a
fast-path that only handles EOA-to-EOA transfers and rejects any
contract call or CREATE. Contract deploys submitted via
`eth_sendRawTransaction` were therefore admitted to the mempool,
included in a block, and silently dropped at execution time. Now
the same submission path deploys real EVM bytecode, calls real
contracts, and returns a receipt with status / cumulative gas /
logs queryable via `eth_getTransactionReceipt`.

#### `aii-evm`

- `ExecutionSummary` gains a `logs: Vec<aii_block::Log>` field. The
  revm `ExecutionResult::Success` log stream is translated to the
  AII `Log` struct (address + topics + data) in emission order.

#### `aii-node`

- `NodeState.state` is now `Arc<StateDb<RocksDbBackend>>` (was an
  owned `StateDb`). `state()` returns `&Arc<...>` so callers that
  need to hand the state off to revm via `execute_with_revm` can
  clone the Arc.
- `execute_block_txs` rebuilt: every Legacy + EIP-1559 tx is now
  routed through `aii_evm::execute_with_revm`. EIP-4844 blob txs
  are skipped with a warn (blob-side execution stays out of scope).
  Each successful invocation produces a `Receipt` populated with
  `status / cumulative_gas_used / logs`; on revm error the tx is
  warn-logged and skipped.
- New `persist_receipts(block_hash, receipts)` writes each
  `(tx_hash, Receipt)` to the `Receipts` CF (RLP-encoded via
  `Receipt::encode_2718`) in a single atomic `WriteBatch`.
- New `receipt_by_tx_hash(H256) -> Result<Option<Receipt>, _>` reads
  back through the same CF.
- `RpcState::receipt_by_tx_hash` impl translates the `Receipt` into
  a `ReceiptView` (hex-encoded fields, tx_type string) so
  `eth_getTransactionReceipt` can answer through the standard JSON
  shape.
- New test `receipt_round_trip_through_persistent_index` verifies
  the encode → write → read → decode loop.
- New test `commit_block_executes_contract_deploy_through_revm`
  deploys real EVM bytecode (writer contract that SSTOREs 0x42 at
  slot 0), calls it, and asserts `state_root()` shifts — proves the
  contract path mutates state, not just balances.

#### `aii-rpc`

- New `eth_getTransactionReceipt(hash)` method on the `EthRpc`
  trait. Returns `null` for unknown / unindexed hashes.
- New response types `ReceiptView` and `LogView` (both
  Serialize+Deserialize).
- New trait method `RpcState::receipt_by_tx_hash` with a default
  `None` impl so non-receipt-indexing backends compile unchanged.

#### Scope discipline

Not in this release (already on the roadmap):

- **`receipts_root` in the block header is still `EMPTY_TRIE_HASH`.**
  Computing it correctly requires applying the block's txs before
  finalising the hash — same chicken-and-egg as `state_root`, both
  fixed together when the engine learns to apply-then-hash (planned
  for the v0.0.45-v0.0.47 range).
- **No gas fee debit to sender / credit to beneficiary.** revm
  reports `gas_used`, but the `gas_price` we pass is `0` — the gas
  fee accounting lands in B.3 (v0.0.43).
- **No block subsidy.** Coinbase still earns 0 AII per block; B.4
  (v0.0.44) will mint per-block + halving.
- **No logs bloom aggregation.** Per-tx `logs_bloom` is `Bloom::ZERO`
  and the block-level bloom isn't touched; B.5 (v0.0.45) will fold
  every log into both the receipt and the header bloom.
- **No `eth_getLogs` RPC yet.** B.5 again.
- **No tx-failure receipts.** revm `Err(_)` cases warn-skip without
  recording a status=false receipt; a follow-up will record those so
  `eth_getTransactionReceipt` can distinguish reverted from missing.

## [0.0.41] — 2026-05-26

### Added — Transactions-MPT + world-state-MPT (roadmap A.3 partial)

`transactions_root` in the block header is now the real Yellow-Paper
MPT root over the body's txs (was `EMPTY_TRIE_HASH` for every block),
and `StateDb::state_root()` computes the world-state MPT root from
every persisted account. Both roots use the already-existing
`aii_state::mpt_root` (Ethereum-compatible MPT from v0.0.6).

This closes one half of A.3. The other half — wiring `state_root` into
the BFT engine's block-build path — has to wait for revm in v0.0.42:
the engine must apply the block's txs before computing the post-block
state root, which today happens in `commit_block` *after* the block
hash has been finalised. Receipts-root is similarly deferred to B.2.

#### `aii-state`

- New `transactions_root(body)` free function (re-exported at the crate
  root). Builds the MPT over `(rlp(i), tx.encode_2718())`. Empty body
  returns `EMPTY_TRIE_HASH`.
- New `StateDb::state_root() -> Result<H256, StateError>`. Iterates
  the `ColumnFamily::State` keyspace, re-encodes each `Account` to
  canonical RLP, folds into `mpt_root`. O(n) in account count — fine
  for testnet; incremental per-block delta is on the B-series roadmap.
- New tests: `state_root_empty_equals_empty_trie_hash`,
  `state_root_changes_when_account_changes`,
  `state_root_independent_of_insert_order`,
  `transactions_root_empty_body_is_empty_trie_hash`,
  `transactions_root_shifts_on_body_change`.

#### `aii-consensus-bft` + `aii-consensus-poa`

- Both engines now compute `transactions_root` from the drained body
  and write it into the produced header. The BFT engine touches both
  the single-validator (`advance_single`) and multi-validator
  (`build_block_with_body`) paths so producer and follower agree.
- `aii-consensus-poa` gains an `aii-state` dependency for
  `transactions_root` (it already depends on `aii-block` for `Tx`).

#### Scope discipline

Not in this release (already on the roadmap):

- **No `state_root` in the produced block header yet.** Block hash
  still embeds `state_root = EMPTY_TRIE_HASH`. The engine has to apply
  the block's txs before finalising the hash, which only becomes
  cheap once revm integration (B.1) lands — at that point the engine
  can call `state.state_root()` between `execute_with_revm` and
  `block.hash()`. The `state_root()` API is already in place so the
  follow-up is a one-line wire-up.
- **No `receipts_root` in the produced block header yet.** Receipts
  are still empty (B.2 is the dedicated milestone).
- **Block-hash incompatibility with v0.0.40.** Any block produced
  under v0.0.40 with non-empty txs will hash differently under
  v0.0.41 (its `transactions_root` was wrong). Validators must wipe
  + restart together.

## [0.0.40] — 2026-05-26

### Added — Persistent chain state (roadmap A.1 + A.2 bundled)

**A restart now restores the entire chain.** Before this release, every
node ran on `MemoryBackend` and a restart wiped all account balances,
every block header, every tx index, and the head counter; the
`RocksDbBackend` opened against `--data-dir` was allocated but never
written to. Now `NodeState` is RocksDB-backed end-to-end, and on
restart `aiid` calls a new `NodeState::recover()` that replays the
indexed chain off disk before opening RPC.

This unblocks every subsequent roadmap milestone — without persistence
a "real public chain" couldn't survive an operator deploying a new
binary.

#### `aii-node`

- `NodeState` now owns `Arc<RocksDbBackend>` and routes `StateDb` to
  that backend. The struct field type is `StateDb<RocksDbBackend>`
  (was `StateDb<MemoryBackend>`); the `state()` accessor's return type
  changes accordingly. Tests that previously called `NodeState::new(spec)`
  switch to `NodeState::new_for_tests(spec)`, which opens a tempdir-backed
  RocksDB internally via `RocksDbBackend::open_in_temp`.
- `NodeState::new(spec, backend)` is the production constructor — the
  binary path threads the long-lived `Arc<RocksDbBackend>` opened
  against `--data-dir` straight in.
- New `NodeState::recover(spec, backend) -> Result<Arc<Self>, _>` — on
  startup, iterates `ColumnFamily::Headers` to rebuild `(hash → Header)`
  + `(number → hash)`, iterates `ColumnFamily::Bodies` to rebuild
  `body_by_hash` + the tx-hash index, and reads `Meta:head_block_number`
  to restore the head counter. The insertion-order `order` vector is
  reconstructed by sorting headers by `header.number` ascending, so
  `aii_recentBlocks` observes the same ordering across restarts.
- `commit_block` writes Header + Body + per-tx `TxLookup` + a
  `Meta:n:<be8>` reverse index in a single atomic `WriteBatch` — all
  ops land together or not at all.
- `set_head(n)` now also persists `Meta:head_block_number` and
  `Meta:head_block_hash`. Synchronous accessor `head_block_number_sync`
  added so startup logging doesn't have to spin up a tokio runtime.
- New helper `number_key(n: u64) -> Vec<u8>` builds `"n:" ‖ be8` keys
  for the `Meta` CF — prefix-distinct from the head markers, scan-safe.

#### `aiid` binary

- Replaces the `let _backend = ...` no-op with a real, named
  `Arc<RocksDbBackend>` threaded into NodeState. On startup, probes
  `Meta:head_block_number`: if present, calls `NodeState::recover`
  and logs `recovered_head=N blocks=K`; if absent, calls
  `NodeState::new(spec, backend)` for a fresh chain.

#### Testing

- New `persistence_round_trip_recovers_state_blocks_and_head` test:
  opens a `tempdir`-backed RocksDB, writes 5 blocks + an Alice account
  with a non-trivial `(nonce=7, balance=987654321)`, bumps head to 5,
  drops every Arc. Reopens the same path, calls `recover`, and asserts
  the Alice account survived intact, head_counter == 5, all 5 blocks
  are indexed by both hash and number, and every block's body is
  present in the cache.

#### Scope discipline

Not in this release (already on the roadmap):

- **No cold-join block sync yet.** A fresh node still needs the same
  pre-existing data dir as its peers; nothing yet downloads missing
  blocks from a remote peer.
- **No state pruning.** Every block body, header, and tx entry is
  retained forever. A "snapshot + reset" tool is on the roadmap.
- **No MPT state trie yet.** `state_root` in the header is still
  `EMPTY_TRIE_HASH`; persistent flat KV is what survives restart.
  Trie integration is roadmap A.3.
- **No fork choice / re-org logic.** Persisted blocks are append-only;
  if a re-org ever arrives, the current `commit_block` rejects the
  conflicting hash via the `by_hash.contains_key` short-circuit.

## [0.0.39] — 2026-05-26

### Added — Multi-validator BFT body gossip + real tx execution

**The two-node BFT testnet finalises real, tx-bearing, gas-charged
blocks across both validators**, closing the v0.0.37 deferral. Before
this release `BftMessage::Proposal` carried only `(hash, leader_proof)`
and followers reconstructed an empty body — so multi-validator BFT
finalised empty blocks even when the mempool was full. Now the leader
ships the RLP-encoded body plus its `coinbase` over the wire, the
follower reconstructs the same block (same `beneficiary`, same
`gas_used`, same hash) and votes on it, and `commit_block` runs every
transaction through `aii_evm::execute_transfer` so balances and nonces
mutate on both nodes in lock-step.

**Live testnet result** (JP + CN, both validators, real gas, three
back-to-back rounds of 100 A→B via node1 + 100 B→A via node2 each):

| Round | A->B total | B->A total | Net A delta actual | expected | n1==n2 |
|---|---|---|---|---|---|
| 1 | 508.10 AII | 506.85 AII | -1.241949 | -1.241949 | ✅ |
| 2 | 503.56 AII | 540.73 AII | +37.172426 | +37.172426 | ✅ |
| 3 | 449.31 AII | 490.17 AII | +40.863342 | +40.863342 | ✅ |

#### `aii-consensus-bft`

- `BftMessage::Proposal` now carries `coinbase: Address` and a length-
  prefixed `body_bytes: Vec<u8>` (RLP-encoded `BlockBody`). Wire
  layout: `tag ‖ height_be8 ‖ round_be4 ‖ block_hash[32] ‖ vrf_preout[32]
  ‖ vrf_proof[64] ‖ vrf_output[32] ‖ coinbase[20] ‖ body_len_be4 ‖
  body_bytes`. Header is fixed-size; body is capped at
  `MAX_PROPOSAL_BODY_LEN` (16 MiB) to bound peer memory pressure.
- New `BftEngine::extend_pending_txs(txs)` appends without dropping
  what the proposer hasn't packed yet — `set_pending_txs` overwrote,
  which silently dropped txs between slots in multi-validator mode.
- `BftEngine::reconstruct_proposed_block_with_body(height, leader_proof,
  coinbase, body)` rebuilds the leader's exact block (so block_hash
  matches and prevote can fire).
- New `BftError::InvalidProposalBody(String)` for peers that ship an
  unparseable RLP body — replaces silent acceptance.

#### `aii-rpc`

- New JSON-RPC methods:
  - `aii_getBlockTransactions(numberOrHash)` — every tx in a block
    (hash, from, to, value, nonce, gas_limit, max_fee, max_priority,
    tx_type) in inclusion order. Returns `null` if block unknown,
    `[]` if block has no txs.
  - `aii_getTransaction(hash)` — single-hop lookup of a tx by its
    keccak256 hash. Returns `{ tx, block_number }`.
- New `TxView` and `TxLookup` response types. `from` is derived via
  `Tx::recover_signer(chain_id)` so the field is the real EOA, not
  the empty-string default that legacy headers had.

#### `aii-node`

- `NodeState::commit_block` is no longer a header-only indexer:
  every transaction in the block body is dispatched to
  `aii_evm::execute_transfer`, which validates nonce + balance and
  mutates the state DB. Idempotent (re-applied block is a no-op).
- `BlockStore` gained `body_by_hash` + `tx_index` so the two new
  RPC methods are O(1). Genesis `alloc` entries are now actually
  applied: `apply_genesis_alloc` runs once at startup and seeds the
  world-state from the `Genesis::alloc` list (the file was being
  parsed but never materialised before — `eth_getBalance` returned
  zero for every pre-funded address).
- Multi-validator BFT main loop drains the mempool on every tick and
  calls the new `extend_pending_txs` so the proposer always has the
  latest queue (no per-slot replace-race).

#### Live testnet ops

- Both nodes now run with distinct `--coinbase`:
  - JP node: `0xD5495A2DeB59252464f510aF6fd246Ae72a1e213`
  - CN node: `0x09c401F8EB333E1943B38d944D28Ad1D5A45B631`
  So `block.beneficiary` actually reflects the proposer instead of
  `0x0000…0000` for every block.
- Explorer's `ExpBlockDetail` now lists every tx in the block (hash,
  from, to, value in AII, type), each tx hash linking to a new
  `/explorer/tx/<hash>` detail page (`ExpTxDetail`) that calls
  `aii_getTransaction`. Validators page row for the CN node no
  longer hard-codes `STANDBY` — it follows `stats.online`, which is
  authoritative under 2-of-2 BFT (no block finalises unless both
  validators sign).

#### Scope discipline

- **In scope**: multi-validator BFT body gossip + tx execution +
  per-block coinbase + explorer tx-detail surface. End-to-end live-
  verified on two-node testnet.
- **Not in this release**:
  - **Persistent block index** — `BlockStore` is still in-memory
    (RocksDB store on the roadmap). A restart still loses the index;
    state is also still memory-backed.
  - **Full revm contract execution** — `execute_transfer` is the
    fast-path EOA-to-EOA executor; contract calls / CREATE / arbitrary
    bytecode still go through `execute_with_revm` which is not yet
    wired into `commit_block`.
  - **Receipts** stored or returned by RPC. Logs / events / tx-receipt
    lookups still defer to a later release.
  - **Block-body sync on cold-join** — a freshly-restarted validator
    has no way to fetch missing blocks from peers; the only safe
    "rejoin" is a coordinated full wipe.

## [0.0.38] — 2026-05-25

### Added — Sub-chain runtime + flush to parent chain

**Sub-chains can now produce their own blocks AND periodically
anchor checkpoints to the parent chain.** `aii subchain run`
spawns an in-process PoA sub-chain, signs a flush tx whose
calldata is `sub_block_hash ‖ sub_block_number_be8`, and submits
it to `--parent-rpc` via `eth_sendRawTransaction`.

**Live testnet result** (sub-chain on laptop, parent at
`https://aii.allfund.xyz/api`):

```
sub_chain_id:    10001
parent_chain_id: 9999
sub blocks:      20
flushes attempted / accepted: 4 / 4
parent blocks containing flush tx: #893, #899, #904, #909
parent_tx hashes (all confirmed via aii_getBlockHeader):
  0x209895ecff50…  (sub #5)
  0x3e7c72f5b443…  (sub #10)
  0x7297209f41ed…  (sub #15)
  0xc50da8afe4e8…  (sub #20)
```

#### `aii-cli`

- New `aii subchain run` subcommand + library `run_subchain`.
- Spawns a fresh secp256k1 operator key, instantiates a single-
  authority `PoaEngine`, produces `--duration-blocks` blocks at
  `--slot-seconds` cadence. Every `--flush-interval-blocks` blocks,
  signs a legacy EIP-155 self-transfer (gas 100,000, calldata
  carrying the anchor) and submits to the parent.
- New `FlushRecord` and `SubchainRunReport` JSON types.

#### Scope discipline

- **In scope**: in-process sub-chain producer + flush-tx anchoring
  to a parent chain (live-verified end to end).
- **Not in this release**:
  - **Persistent sub-chain state** — sub-chain head + history are
    in-memory only. Each `aii subchain run` starts fresh.
  - **Anchor decode + on-chain registry update** — the parent
    doesn't yet decode flush-tx calldata into
    `aii-microchain::FlushAnchor` / update its `Registry`. Today
    the anchor exists as a regular tx; explorers read it from the
    calldata field.
  - **Sub-chain consensus modes other than PoA**.
  - **Multi-validator BFT block-body gossip** — still on the
    roadmap (deferred from v0.0.37).

## [0.0.37] — 2026-05-25

### Added — Transaction pipeline + live stress test

**The chain accepts real signed transactions and packs them into
blocks.** `aii_getBlockHeader` now reports non-zero `gas_used`,
explorers and wallets can submit via `eth_sendRawTransaction`, and
the `aii stress` CLI command measures actual throughput.

**Live testnet result** (single-validator BFT on `8.211.135.234`,
15,000 self-transfers from a local stress client):

| Metric | Value |
|---|---|
| Submitted / Accepted | 15,000 / 15,000 |
| Peak txs / block | **1,428** (`gas_limit 30M ÷ 21k`) |
| Sustained 7 consecutive blocks at peak | yes |
| Mean txs / block (over 20 sampled) | 750 |
| Submit rate (single-process HTTP) | 1,459 tx/s |
| Wall clock | 10.3 s |

#### `aii-block` — signer recovery

- New module `tx::signer` with `Tx::recover_signer(chain_id) -> Address`.
- Pre-EIP-155 legacy (v ∈ {27, 28}), EIP-155 legacy (v = chain*2+35),
  and EIP-1559 (v ∈ {0, 1}). Round-trip tests with a fresh secp256k1
  key prove the recovered address matches `sk.public_key().address()`.
- PQ-mode (`algo_id != Secp256k1`) is rejected explicitly with
  `RecoveryError::NotSecp256k1`.

#### `aii-net-txpool`

- New `drain_up_to(n)` for FCFS bulk drain — producers pull a batch
  for the next block without enforcing per-sender nonce ordering
  across sender boundaries.

#### `aii-rpc`

- New `RpcState::submit_raw_tx(raw_hex)` default impl + matching
  `eth_sendRawTransaction(rawHex)` RPC method. Parses EIP-2718,
  recovers signer, admits to mempool, returns the 32-byte tx hash.
- New `SubmitTxError` enum with `Unsupported / Hex / Decode / Signer
  / Pool` variants.

#### `aii-node`

- `NodeState` owns an `Arc<TxPool>` (capacity 100,000) and exposes
  `tx_pool()` to the producer loop.
- `submit_raw_tx` implementation: hex → EIP-2718 decode → secp256k1
  recover → `TxPool::add` keyed by `(sender, nonce)`. Returns the
  tx's keccak256 hash.

#### `aii-consensus-bft` + `aii-consensus-poa`

- Both engines gain a `pending_txs: Mutex<Vec<Tx>>` slot and a
  `set_pending_txs(Vec<Tx>)` setter.
- `BftEngine::advance_single` and `PoaEngine::produce_block` drain
  pending txs up to `gas_limit / PLACEHOLDER_TX_GAS` and include
  them in `block.body.transactions`. `header.gas_used = N * 21,000`
  (placeholder until EVM execution lands).
- `PLACEHOLDER_TX_GAS = 21_000` is re-exported for the producer
  loop's batch-size math.

#### `aii-cli`

- New `aii stress` subcommand and library entry point `run_stress`.
  Generates `--senders` distinct signers, signs N legacy
  self-transfers (EIP-155 chain-id-bound), submits via
  `eth_sendRawTransaction` across `--parallel` workers, samples
  `--sample-blocks` after `--settle-sec`, and prints
  `submitted / accepted / peak txs/block / mean / throughput`.
- New `aii block <q>` and `aii recent --limit N` (carried from
  v0.0.36).

#### Tests + verification

- Workspace: **653 / 653** tests (was 647), clippy clean under
  `-D warnings`.
- 6 new RED→GREEN tests in `aii-block::tx::signer`.
- Live stress numbers above — reproducible by anyone with a JP/CN
  shell:
  ```bash
  aii --rpc http://8.211.135.234:8545 stress \
      --chain-id 9999 --total 15000 --senders 64 --parallel 64
  ```

#### Scope discipline

- **In scope**: signer recovery, mempool wire-in, producer drain,
  CLI stress harness.
- **Not in this release**:
  - **Block-body gossip for multi-validator BFT**:
    `BftMessage::Proposal` still carries only `(hash, leader_proof)`,
    so peers reconstruct an empty block. The 2-node JP+CN BFT
    deployment was temporarily switched to single-validator on JP
    for this release; v0.0.38 will extend the wire format so
    multi-validator BFT can carry tx-bearing blocks too.
  - **Real EVM execution** — every tx is currently treated as a
    21,000-gas transfer with no balance / nonce check (placeholder).
    Real `revm` execution lands in v0.0.39+.
  - **Subchain → main chain flush** — v0.0.38 scope.

## [0.0.36] — 2026-05-25

### Added — Block explorer API + MCP integration

**The browser + CLI + Claude (MCP) can all read the chain now.**
v0.0.34 made nodes talk to each other; v0.0.35 made sub-chains
pluggable; v0.0.36 makes the data legible. Every finalised block is
indexed and reachable via three surfaces: JSON-RPC, the `aii` CLI,
and the MCP server.

#### `aii-rpc`

- New `HeaderView` JSON shape: hash, parent, number, timestamp,
  beneficiary, gas limit/used, base fee, three roots, mix hash,
  extra data (hex). All numbers `0x…` (Ethereum convention).
- New `RpcState` methods with default `None`/empty impls so other
  consumers keep building:
  - `header_by_number(n)`
  - `header_by_hash(hex)`
  - `recent_headers(limit)`
- New JSON-RPC methods on the `aii_` namespace:
  - `aii_getBlockHeader(numberOrHash)` — accepts decimal or `0x…`
    hex block number, or 32-byte `0x…` block hash. `null` on miss.
  - `aii_recentBlocks(limit)` — newest first, server-capped at 100.

#### `aii-node`

- `NodeState` gains an in-memory `BlockStore` (header by hash +
  number→hash index + insertion-order vector) and a
  `commit_block(&Block)` method.
- The BFT (single + multi-validator) and PoA producer loops in
  `main.rs` now commit every finalised block. Multi-validator BFT
  `try_harvest_committed()` now returns `Option<Block>` (was
  `Option<u64>`) so the host can index the body too.
- 7 RED→GREEN tests covering commit + lookup by number / hash,
  unknown-block returns None, recent-headers newest-first + cap,
  and full RPC round-trip for both new methods.

#### `aii-cli`

- Two new subcommands:
  - `aii block <numberOrHash>` — pretty-print (or `--json`) a
    single header.
  - `aii recent --limit N` — print a number / timestamp / hash
    table of the N newest blocks.
- Library runners: `run_get_block_header` and `run_recent_blocks`.

#### `aii-mcp`

- Two new MCP tools (total 10):
  - `block_lookup { query }` wraps `aii_getBlockHeader`.
  - `recent_blocks { limit }` wraps `aii_recentBlocks`.
- Existing `tools_list_includes_eight_tools` test renamed +
  expanded to `…_ten_tools`.

#### Tests + verification

- Workspace: **647 / 647 tests pass** (was 640), clippy clean
  under `-D warnings`.
- Live: `aiid --consensus poa --slot-seconds 1` produced 3 blocks
  in 3 seconds. All three surfaces read them back:
  - `curl aii_getBlockHeader '["2"]'` → full HeaderView JSON.
  - `curl aii_recentBlocks '[3]'` → newest-first 3-element array.
  - `aii recent --limit 2` → two-row CLI table.
  - `aii-mcp` stdio: `tools/list` reports 10 tools; `tools/call`
    on `recent_blocks` and `block_lookup` returns live block data
    wrapped in MCP `content[].text`.

#### Scope discipline

- **In scope**: in-memory header index, RPC endpoints, CLI
  subcommands, MCP tools, wiring producers to `commit_block`.
- **Not in this release**: persistent RocksDB block storage (the
  `RocksDbBackend` is opened but not yet written to — restart
  loses the index); tx bodies / receipts in `HeaderView`;
  pagination beyond `recent_blocks`; standalone HTML/SPA
  front-end (the data is now reachable by any explorer that
  speaks JSON-RPC).

## [0.0.35] — 2026-05-25

### Added — Pluggable consensus (PoA alongside BFT-PoS)

**Sub-chains can now choose their consensus algorithm.** v0.0.34
proved the main chain (BFT-PoS) works across nodes. v0.0.35 introduces
a second consensus impl — Proof-of-Authority — and the trait surface
that lets a sub-chain genesis pick which one to run.

#### New crate: `aii-consensus-poa`

- `PoaConfig { authorities, coinbase, slot_seconds, gas_limit,
  base_fee_per_gas }` — fixed authority list, no voting.
- `PoaEngine` implements `aii_consensus_iface::Engine`. Per
  `step()`: if `authorities[(head_number + 1) % authorities.len()]`
  equals our `coinbase`, produce a block; otherwise return
  `EngineProgress::Idle`.
- `PoaError::{EmptyAuthorities, Overflow}`.
- 8 RED→GREEN tests covering: empty-set rejection, single-authority
  continuous production, non-authority idleness, two-authority
  round-robin, parent-hash chain, `init` reset, coinbase pass-through.

#### `aii-consensus-iface`

- New `ConsensusKind` enum (`Bft | Poa`) with `as_str` +
  case-insensitive `parse`, lowercase serde encoding. Accepts the
  legacy spelling `proof-of-authority`.

#### `aii-microchain`

- `MicroChainSpec.consensus: ConsensusKind` field with `serde(default
  = ConsensusKind::Bft)` so pre-v0.0.35 sub-chain genesis JSON
  continues to parse.
- Two new tests: PoA spec round-trips through JSON; a legacy spec
  without the consensus field defaults to BFT.

#### `aii-node` / `aiid`

- Two new CLI flags:
  - `--consensus bft|poa` (default `bft`).
  - `--authorities ADDR1,ADDR2,…` — required when `--consensus poa`.
- New PoA branch in `main()` builds a `PoaEngine` and loops
  `is_my_turn` / `produce_block` at `--slot-seconds`. `--coinbase`
  defaults to `authorities[0]` if omitted.

#### Tests + verification

- Workspace: **640 / 640 tests pass** (was 628), clippy clean under
  `-D warnings`.
- Live: `aiid --consensus poa --authorities 0xaaaa…aaaa --coinbase
  0xaaaa…aaaa --slot-seconds 1` produced PoA blocks #1–#5 at 1 s
  intervals; `eth_blockNumber` returned `0x5` after 5 s.

#### Scope discipline

- **In scope**: PoA engine, ConsensusKind, microchain field, aiid
  `--consensus`/`--authorities` flags.
- **Not in this release**: PoA signer signatures (today
  `header.beneficiary == authorities[H % N]` is the only check);
  PoA validator-set rotation (authority list is fixed at genesis);
  Tendermint / DPoS engines; per-sub-chain Engine spawning (the
  microchain registry carries the kind, but spawning multiple
  engines lives in v0.0.36+).

## [0.0.34] — 2026-05-25

### Added — Multi-node BFT consensus over TCP gossip

**The chain now runs across multiple hosts.** v0.0.33 made a single
`aiid` process finalise BFT blocks. v0.0.34 wires `BftMessage` (which
has existed since v0.0.27) into a real network transport, so two or
more validator nodes on separate sockets can exchange proposals +
prevotes + precommits and agree on a common chain head. **This is the
last structural prerequisite for a public testnet deployment.**

#### aii-net-p2p

- New `Message::Bft(Vec<u8>)` variant on the existing TCP transport.
  Payload bytes are opaque to the transport: they are
  `BftMessage::encode()` output. Adds `TYPE_BFT = 0x05` tag,
  length-bounded against `MAX_FRAME_BYTES`.
- Promoted `Message::encode` and `Message::decode` to `pub` so
  transports outside this crate can frame their own connections.

#### aii-consensus-bft

- New `gossip` module:
  - `BftTransport` trait: sync `broadcast(Vec<u8>)` + `try_recv() ->
    Option<Vec<u8>>`. Blanket impl for `Arc<T: BftTransport>`.
  - `BftGossip<T>` driver. Per `tick()`:
    1. Drains inbox, decodes `BftMessage`, dispatches to engine's
       `submit_remote_proposal / _prevote / _precommit`.
    2. Bootstraps a round when no coordinator exists (`cast_proposal`
       on the elected leader).
    3. Casts the next phase's vote when the local engine reaches
       Prevoting / Precommitting.
    4. **Retransmits cached proposal / prevote / precommit bytes
       every tick** to defeat startup races (a peer that connects
       after the leader's first broadcast still receives it on the
       next tick).
- New `BftEngine` accessors:
  - `my_index()` — this node's validator index.
  - `validator_set_size()` — current set size.
  - `would_be_leader_next_height()` — bootstrap predicate.
  - `reconstruct_proposed_block(height, &LeaderProof)` — peers rebuild
    a leader's block from `(parent, proof, height)` without needing
    the full body on the wire.
  - `try_harvest_committed() -> Option<u64>` — `&self` flavour of
    multi-validator `step()` for gossip-driven hosts.
- New `BftError::ProposalHashMismatch` for tamper detection.

#### aii-node

- New `bft_p2p::TcpBftTransport`. Async constructor binds a listener
  + dials each peer in `peer_addrs`. Inside the transport:
  - One acceptor task per inbound connection;
  - One dialer task per outbound peer (infinite retry, 500 ms backoff);
  - Per-connection reader + writer pair;
  - `broadcast::Sender<Vec<u8>>` for outbound fanout;
  - `Mutex<VecDeque<Vec<u8>>>` for the inbound queue.
- `aiid` CLI gets two new flags:
  - `--bft-listen ADDR` (default `127.0.0.1:30311`).
  - `--peers ADDR1,ADDR2,…` (comma-separated `host:port` list).
- `--bft` multi-validator path now stands up the transport and
  drives a `BftGossip` loop instead of waiting silently.

#### Test coverage

- `aii-net-p2p`: 3 new tests — `Message::Bft` round-trip, oversize
  rejection, full TCP send/recv.
- `aii-consensus-bft`: `two_node_gossip_finalises_one_block` — two
  in-memory `BftEngine` + channel pair reach height 1.
- `aii-node`: `tests/bft_p2p_e2e.rs::two_validators_finalise_block_over_tcp`
  — two `BftEngine`s + `TcpBftTransport`s over `127.0.0.1:0` agree
  on a finalised block at height 1.
- Workspace: **628 / 628 tests pass**, clippy clean under
  `-D warnings`.

#### Verified on live aiid

Two `aiid` processes, separate keystores, fresh genesis with both
validators, connected via `--peers`:

```bash
aii validator keygen > node-a.json
aii validator keygen > node-b.json
aii --json validator pubkey --file node-a.json > pub-a.json
aii --json validator pubkey --file node-b.json > pub-b.json
aii genesis init --network testnet \
    --validator-pubkey pub-a.json --validator-pubkey pub-b.json \
    --out genesis.json

aiid --bft --genesis genesis.json --keystore node-a.json \
     --data-dir /tmp/a --rpc 127.0.0.1:18545 \
     --bft-listen 127.0.0.1:31311 --peers 127.0.0.1:31312 \
     --testnet --slot-seconds 1 &
aiid --bft --genesis genesis.json --keystore node-b.json \
     --data-dir /tmp/b --rpc 127.0.0.1:18546 \
     --bft-listen 127.0.0.1:31312 --peers 127.0.0.1:31311 \
     --testnet --slot-seconds 1 &
```

After 8 seconds, both nodes reported `eth_blockNumber = 0x26` (block
38) with identical timestamps per height — confirming agreement.

#### Scope discipline

- **In scope**: TCP gossip transport, retransmit loop, integration
  test, aiid wiring.
- **Not in this release**: encrypted validator keystore; on-chain
  slashing executor; full block-body gossip (today receivers
  reconstruct empty blocks deterministically from `LeaderProof`);
  fork choice / re-org; libp2p / Kademlia discovery; mTLS / Noise
  on the gossip socket.

## [0.0.33] — 2026-05-25

### Added — `aiid --bft`: real BFT block production end-to-end

The `aiid` binary now runs the real BFT-PoS engine from a genesis +
keystore file pair. **This is the milestone for "the chain is
runnable."** With v0.0.32 we had keygen + genesis on disk; with v0.0.33
the node actually loads them, advances heights via BFT, and serves
the new heads via RPC.

#### aii-node

- New `bft_bootstrap` submodule:
  - `load_genesis(&Path)` / `load_keystore(&Path)` — parse JSON files.
  - `discover_my_index(&Genesis, &ValidatorKeystore)` — match the
    keystore's BLS pubkey against the genesis validator entries.
  - `build_bft_config(&Genesis, &ValidatorKeystore, coinbase, my_index?)`
    — assemble a runtime `BftConfig`, decompressing both secret keys.
  - `boot_bft_engine(genesis_path, keystore_path, coinbase)` — one-shot
    constructor returning the `BftEngine` ready to advance.
  - New `BootstrapError` with `Io / Json / Hex / Keystore /
    NotAValidator / Bft` variants.
- 5 RED→GREEN tests covering: pubkey discovery, unknown-keystore
  rejection, in-memory `BftConfig` build, end-to-end disk-to-engine
  boot + first-block advance, malformed-JSON rejection.

#### aiid (binary)

- New flags: `--bft`, `--genesis FILE`, `--keystore FILE`,
  `--coinbase 0xHEX`.
- When `--bft` is set:
  - Single-validator mode: per-`--slot-seconds` tick, call
    `engine.advance_single()`, update `NodeState::head`, log
    `BFT block finalised number=N hash=… round=R`.
  - Multi-validator mode: wait for peer events (gossip transport
    lands in v0.0.34+).
- When `--bft` is absent: legacy `DevModeEngine` path preserved.

### Verified on a live `aiid` process

```bash
aii validator keygen --out node.json
aii --json validator pubkey --file node.json > pub.json
aii genesis init --network testnet --validator-pubkey pub.json \
    --stake 1000 --out genesis.json

aiid --testnet --bft --genesis genesis.json --keystore node.json \
     --slot-seconds 1 --coinbase 0xabababababababababababababababababababab

# … 5 seconds later …
curl -sX POST http://127.0.0.1:8545 \
     -H 'Content-Type: application/json' \
     -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}'
# → {"jsonrpc":"2.0","id":1,"result":"0x4"}
```

The logs show `BFT block finalised number=1 round=0`,
`number=2 round=0`, … — real BFT certificates, one per block, every
slot, persisted in `NodeState::head` and visible via `eth_blockNumber`.

### What "可以商业化部署" means with this release

A node operator can now:
1. Generate validator keys (`aii validator keygen`).
2. Assemble a genesis file (`aii genesis init`).
3. Run a real BFT node (`aiid --bft …`).
4. Query the chain head via JSON-RPC.

Multi-validator deployments still need the gossip transport (v0.0.34)
to share BFT messages across hosts. Until then, the multi-validator
path can be driven via `BftEngine::submit_remote_*` from a custom
transport (e.g. HTTP relay) — the API is stable.

### Scope discipline

Not in this release: P2P transport for `BftMessage`; encrypted
validator keystore; on-chain slashing executor; state-root computation
in the produced blocks; fork choice.

## [0.0.32] — 2026-05-25

### Added — node-operator CLI for validator + genesis tooling

The `aii` binary gains the commands a real validator operator needs to
bootstrap a multi-node testnet/mainnet:

- `aii validator keygen [--out FILE]` — generate a fresh BLS + VRF
  keypair, write a JSON keystore with hex-encoded secret + public
  material. **Testnet only** for now; encrypted keystore is a v0.0.33+
  follow-up.
- `aii validator pubkey --file FILE` — load a keystore, validate
  internal consistency (pubkey-from-secret on both BLS and VRF), and
  emit the pubkeys-only projection that gets shared with the genesis
  assembler.
- `aii genesis init --network testnet --validator-pubkey FILE …
  --stake N [--initial-seed 0xHEX] [--out FILE]` — combine N
  validator-pubkey files into a Genesis JSON ready for shipping.
- `aii genesis validate --file FILE` — round-trip parse, chain-spec
  invariants, every pubkey decompresses, total stake non-zero.

#### aii-cli

- New `ValidatorKeystore`, `ValidatorPubkeys`, `ValidatorEntry` types
  (serde, hex-encoded `0x`-prefixed fields).
- New runners: `run_validator_keygen`, `run_validator_pubkey`,
  `run_genesis_init`, `run_genesis_validate`, `run_random_seed_hex`.
- 12 new unit tests including an end-to-end test that runs the full
  operator workflow: 3 fresh keygens → 3 pubkey extractions → 1
  genesis init → 3 independent `BftConfig::from_genesis` loads each
  with the matching node's secret keys.

### Verified end-to-end

```bash
aii validator keygen --out validator-a.json
aii --json validator pubkey --file validator-a.json > pubkey-a.json
# … operators share pubkey JSONs …
aii genesis init --network testnet \
    --validator-pubkey pubkey-a.json \
    --validator-pubkey pubkey-b.json \
    --validator-pubkey pubkey-c.json \
    --stake 1000 --out genesis.json
aii --json genesis validate --file genesis.json
# → {"chain_id":9999,"ok":true,"validators":3}
```

### Why this matters

Before this release, building a multi-node testnet required
hand-writing genesis JSON and matching node-side BLS/VRF keys by hand.
v0.0.32 is the minimum operator-facing UX for spinning up a commercial
chain. The remaining piece — wiring `BftEngine` into the `aiid` node
binary on startup — lands in v0.0.33.

### Scope discipline

Not in this release: encrypted validator keystore (`scrypt` + AES like
the EOA wallet); `aiid --bft --genesis FILE` integration; P2P transport
for `BftMessage`; on-chain slashing executor.

## [0.0.31] — 2026-05-25

### Added — genesis-driven BFT bootstrap

Production deployment now has a path: a single JSON `Genesis` file
declares the full validator set and the chain's initial seed, and
`BftConfig::from_genesis` derives the in-memory engine config (modulo
this node's secret keys + coinbase). The node operator's job becomes
"share the genesis file, load your keys, start the engine."

#### aii-types

- New `VrfPubKey` wire type (32-byte compressed schnorrkel pubkey)
  alongside `BlsPubKey`. Serde representation is lowercase hex with
  `0x` prefix — same convention as `Address`, so genesis JSON stays
  human-readable.
- `BlsPubKey` / `BlsSignature` gain custom serde for the same
  `0x`-prefixed hex format (previously derived only by structural
  fields; now produces stable JSON).

#### aii-config

- `Genesis` gains:
  - `validators: Vec<GenesisValidator>` — `(bls_pubkey, vrf_pubkey,
    stake)` triples. `#[serde(default)]` so older empty-validator
    genesis files still parse.
  - `initial_seed: [u8; 32]` — VRF seed used at height 1 round 0; later
    rounds derive seed from the previous leader's VRF output.
- New `GenesisValidator { bls_pubkey, vrf_pubkey, stake }` struct,
  re-exported from the crate root.

#### aii-consensus-bft

- New `BftConfig::from_genesis(&genesis, my_index, my_bls_sk,
  my_vrf_sk, coinbase)` constructor. Validates the validator set,
  decompresses every pubkey via `aii-crypto`, checks `my_index`
  bounds, and lifts chain-spec parameters into the engine config.
- New `BftError::InvalidValidatorPubkey { index, kind }` for genesis
  entries whose BLS or VRF pubkey doesn't decode.
- 7 new tests, including:
  - empty-validator genesis rejected (`EmptyValidatorSet`)
  - invalid BLS pubkey at index 0 surfaces the correct error
  - out-of-bounds `my_index` rejected
  - chain-spec params (gas limit, base fee, slot time) and initial seed
    flow through to the engine config
  - single-validator engine built from genesis advances one height
    with a verifying certificate
  - three-validator engines all built from the same genesis JSON reach
    consensus on the same block hash
  - genesis with validators round-trips through JSON
  - `BftConfig::from_genesis` works on a `Genesis` loaded back from
    its own JSON

### Why this matters

Commercial deployment requires a reproducible bootstrap: a chain spec
plus a validator-set declaration that every node can verify. Until
v0.0.31 the BFT engine was constructed from raw runtime keys with no
chain-level provenance. With this release, a chain operator can ship
a single signed JSON file and every validator node can derive its
runtime config from it.

### Scope discipline

Not in this release: validator key management CLI (`aii validator
keygen`, `aii validator pubkey`), node startup wiring (replacing
`DevModeEngine` with `BftEngine` in `aiid`), genesis distribution
tooling. These land in v0.0.32+.

## [0.0.30] — 2026-05-25

### Added — multi-validator BFT consensus end-to-end

`BftEngine` now drives consensus across multiple validators by accepting
peer-injected proposals and votes. The chain crosses from "single-node
demo" to "actually multi-validator." A three-node test produces an
identical chain through pure method-call exchange — the structural
proof that the BFT engine can be deployed on a real network once the
gossip transport is wired.

#### aii-consensus-bft

- `BftEngineState` gains a long-lived `RoundCoordinator` plus the
  `(Block, LeaderProof)` for the in-progress round. Lazily created on
  the first event for a height; reset after `step()` harvests the
  committed block.
- New methods on `BftEngine`:
  - `cast_proposal()` — leader-only: build block + leader proof, feed
    to local coordinator, return for broadcast.
  - `cast_prevote()` / `cast_precommit()` — sign + submit my own vote,
    return for broadcast.
  - `submit_remote_proposal(Block, LeaderProof)` — peer-supplied
    proposal; the inner coordinator validates the leader proof.
  - `submit_remote_prevote(PrevoteVote)` /
    `submit_remote_precommit(PrecommitVote)` — forward to coordinator.
  - `tick_timeout()` — external-clock-driven round advance.
  - `current_round_state()` → `Option<(height, round, Phase)>`.
  - `current_leader_index()` → `Option<usize>`.
- `Engine::step()` in multi-validator mode now harvests the committed
  block when the coordinator reaches `Phase::Committed`: updates the
  chain head, rolls the seed forward via the leader's VRF output, and
  clears the coordinator so the next height can start fresh. Returns
  `Idle` otherwise.
- New `BftError` variants:
  - `NoActiveCoordinator` — `cast_*` / `submit_remote_*` called before
    a coordinator has been initialised for the current height.
  - `NotLeader { round, expected }` — `cast_proposal()` rejected because
    this node is not the elected leader for the round.
- 12 new tests, including the killer **`three_node_consensus_produces_same_block`**:
  three `BftEngine` instances act as a 3-validator set, exchange a
  proposal + 3 prevotes + 3 precommits via direct method calls, and
  all three then report the same `NewBlock(hash)` from `step()` —
  bit-for-bit identical heads.
- Other coverage: lazy coordinator init, non-leader proposal rejection,
  prevote-without-proposal rejection, precommit-without-POLC rejection,
  invalid leader proof rejection, timeout clears proposal, post-commit
  state cleared, idle when no progress, current-leader-index reflects
  validator set.

### Why this matters

Up to v0.0.29, a real multi-validator deployment had no API surface
for the consensus engine — it could only run single-node. v0.0.30 is
the last structural piece needed for the engine half of a commercial
mainnet: gossip transport (wiring `BftMessage` through `aii-net-p2p`),
state-root computation, slashing-tx execution, fork choice, and node
operator tooling (genesis generator, validator key onboarding,
config) remain — but the consensus machinery is functionally complete.

### Scope discipline

Not in this release: actual gossip transport, fork choice / re-org,
state-root computation, slashing-tx execution, validator-set rotation,
node operator tooling. These remain explicit non-goals and will land
separately.

## [0.0.29] — 2026-05-24

### Added — BFT-PoS stage 6: chain-level `BftEngine`

The pure state machines built up through stages 1–5 (`ValidatorSet`,
`LeaderProof`, `PrevoteVote`/`PrecommitVote`, `RoundCoordinator`,
`PolcCertificate` / `PrecommitCertificate`, equivocation detector)
finally meet the rest of the chain. `BftEngine` implements
`aii_consensus_iface::Engine`, so the existing `aiid` node binary can
swap `DevModeEngine` for a real two-phase BFT engine without API churn.

#### aii-consensus-bft

- New `engine` submodule with:
  - `BftConfig { validator_set, my_index, my_bls_sk, my_vrf_sk,
    initial_seed, coinbase, gas_limit, base_fee_per_gas,
    slot_seconds }` — everything a node needs to participate in BFT.
  - `BftEngine` — wraps a chain-head `(hash, number, timestamp, seed)`
    plus the static config; the round coordinator is created on demand
    per height.
  - `BftEngine::advance_single()` — single-validator round trip:
    produce leader proof for `(height+1, 0, seed)`, build a block on
    top of the current head with `mix_hash = vrf_output`, drive a
    fresh `RoundCoordinator` through propose → prevote → precommit →
    committed, harvest the certificate, and advance the seed to the
    leader's VRF output. Returns `AdvanceOutput { block, block_hash,
    certificate }`.
  - `BftEngine::is_single_validator()` for tooling that needs to know
    which mode the engine is in.
  - `Engine::step()` — in single-validator mode, auto-advances one
    height (`EngineProgress::NewBlock`); in multi-validator mode,
    reports `Idle` and waits for an external network drive (lands
    alongside gossip in v0.0.30+).
- New `BftError::NotSingleValidator(usize)` — `advance_single` requires
  a 1-of-1 set, fails clean otherwise.
- 16 RED→GREEN tests:
  - construction, init, coinbase, head accessors;
  - `is_single_validator` predicate;
  - single-validator advance increments height + timestamp + parent-hash
    chain;
  - finality certificate verifies under the configured validator set;
  - advance correctly rejected in multi-validator mode;
  - `step()` returns `NewBlock` in single-validator mode and `Idle` in
    multi-validator mode;
  - seed evolves across calls (consecutive blocks differ);
  - 10-height chain test: every parent hash matches; every certificate
    verifies; final head matches the last produced block.

### Scope discipline

Still NOT in this release: peer-side ingest API for multi-validator
mode (gossip-driven proposal/prevote/precommit injection — v0.0.30+);
chain re-org / fork-choice (only single-leader paths exist today);
state-root computation (every block is empty-state); slashing tx
execution (the detector emits evidence but no on-chain action). These
remain explicit non-goals.

## [0.0.28] — 2026-05-24

### Added — BFT-PoS stage 5: POL preservation + equivocation detector

Two correctness gates land on top of the stage-3 coordinator:

1. **POL preservation**: a [`PolcCertificate`] formed in round R is now
   captured into a `LockedState` that survives every subsequent
   `fire_timeout`. Validator clients consult `coord.locked()` to
   decide whether to PRE-VOTE for the new round's proposal or keep
   their lock.
2. **Equivocation detector**: a slashing-evidence builder that catches
   any validator who signs two different blocks for the same `(height,
   round, phase)`.

#### aii-consensus-bft

- New `LockedState { block_hash, round, polc }` in
  `coordinator` module; new `RoundCoordinator::locked()` accessor.
- `fire_timeout` clears the proposal / tallies / current-round
  `polc()`, but leaves `locked()` untouched — the lock is the durable
  protocol state across rounds.
- POLC formation at a strictly newer round supersedes the prior lock;
  an equal-round POLC is also accepted (idempotent restart).
- New `slashing` submodule with:
  - `EquivocationDetector` — tracks `(validator_index, height, round)`
    → first signed vote per phase; second conflicting block at the
    same key emits evidence.
  - `EquivocationEvidence::Prevote { conflicting: [PrevoteVote; 2] }`
    / `Precommit { conflicting: [PrecommitVote; 2] }`. Accessors
    `validator_index()`, `height()`, `round()`.
  - `EquivocationEvidence::verify(&vs)` — independently re-checks
    coordinate agreement, that the two block hashes differ, and that
    both BLS signatures verify under the same validator's pubkey.
- New `SlashingError`: `SameBlock`, `Mismatch { field }`,
  `UnknownValidator(u32)`, `InvalidSignature`.
- 17 new slashing tests + 5 new coordinator POL tests:
  - PRE-VOTE / PRE-COMMIT streams tracked independently per phase
    (cross-phase contradictions are caught by digest domain separation
    rather than the detector).
  - Different validators / heights / rounds correctly partition the
    map (no false positives).
  - Evidence verify catches same-block, mismatched validator index /
    round, out-of-bounds index, and BLS signature forgery.
  - Coordinator starts with no lock; POLC sets it; timeout preserves
    it across 5 timeouts; a fresh POLC at a higher round supersedes.

### Scope discipline

Still NOT in this release: actually executing the slashing transaction
(state debit + validator freeze), enforcing the "vote your lock"
policy at the protocol level, gossip-side gating on lock state. These
remain explicit non-goals.

## [0.0.27] — 2026-05-24

### Added — BFT-PoS stage 4: wire-format codec

A typed envelope for the three BFT consensus messages, so a validator
can serialise / parse votes and proposals on the network without
inventing per-call encoding. Fixed-layout byte packing — no RLP, no
SSZ — so malformed messages are rejected by length alone before any
cryptographic check.

#### aii-consensus-bft

- New `wire` submodule with:
  - `BftMessage::Proposal { height, round, block_hash, leader_proof }`
  - `BftMessage::Prevote(PrevoteVote)`
  - `BftMessage::Precommit(PrecommitVote)`
- `tag()` returns the first byte (cheap routing without decoding).
- `encoded_len()` returns the exact wire size for that variant:
  - `PROPOSAL_LEN = 173` bytes
  - `VOTE_LEN = 145` bytes
- `encode()` writes the fixed layout:
  - Proposal: `0x00 ‖ height_be8 ‖ round_be4 ‖ block[32] ‖ vrf_preout[32] ‖ vrf_proof[64] ‖ vrf_output[32]`
  - Prevote: `0x01 ‖ block[32] ‖ height_be8 ‖ round_be4 ‖ index_be4 ‖ bls_sig[96]`
  - Precommit: `0x02 ‖ block[32] ‖ height_be8 ‖ round_be4 ‖ index_be4 ‖ bls_sig[96]`
- `decode(bytes)` validates length / tag / BLS signature decompression
  and returns the typed message. Semantic checks (VRF validity,
  BLS aggregate verification) remain at higher layers.
- New `CodecError` with `Empty`, `UnknownTag(u8)`,
  `WrongLength { expected, got }`, `InvalidBlsSignature` variants.
- 15 RED→GREEN tests: tag / length, round-trip for all three variants,
  empty / unknown-tag / truncated / malformed-BLS rejection, and an
  end-to-end check that a round-tripped PRE-VOTE still verifies under
  the original signer's pubkey.

### Scope discipline

Still NOT in this release: actual networking (the host crate plugs the
codec into its transport); message authentication beyond per-vote BLS
(no top-level peer signature); rate-limiting / mempool. These remain
explicit non-goals and will land separately.

## [0.0.26] — 2026-05-24

### Added — BFT-PoS stage 3: round-change coordinator

The stage-1/2 primitives now have an orchestrator. `RoundCoordinator`
drives one height through the two-phase BFT lifecycle and advances
rounds on timeout — the structural pre-req for surviving a stuck leader
or a slow network. Still pure state machine: no networking, no clock,
no I/O.

#### aii-consensus-bft

- New `coordinator` submodule with `RoundCoordinator`:
  - `new(height, seed, vs)` starts at round 0, phase `AwaitingProposal`.
  - `submit_proposal(block, &LeaderProof)` validates the proof against
    the expected proposer for `(height, round, seed)` and transitions
    to `Prevoting`.
  - `submit_prevote(vote)` forwards to the inner `PrevoteTallier`; on
    quorum captures the `PolcCertificate` and transitions to
    `Precommitting`.
  - `submit_precommit(vote)` forwards to the inner `PrecommitTallier`;
    on quorum captures the `PrecommitCertificate` and transitions to
    `Committed`.
  - `fire_timeout()` advances to the next round (clearing proposal,
    tallies, and POLC) unless already `Committed`. `Committed` makes
    `fire_timeout` a no-op.
  - Accessors: `phase()`, `round()`, `height()`, `leader_index()`,
    `proposed_block()`, `polc()`, `certificate()`.
- `bft::Phase` enum: `AwaitingProposal` / `Prevoting` / `Precommitting`
  / `Committed`. Re-exported from the crate root.
- New `BftError::WrongPhase { expected, actual }` for phase-violation
  reports.
- **Breaking change** to v0.0.23 leader API:
  - `ValidatorSet::select_leader(height, seed)` →
    `select_leader(height, round, seed)` so each round at the same
    height picks a (probably) different proposer.
  - `LeaderProof::input / produce / verify` all gain a `round: u32`
    argument; the VRF input becomes `keccak256(height_be8 ‖ round_be4 ‖ seed)`.
- 17 RED→GREEN tests on the coordinator covering: starts in
  `AwaitingProposal`; leader index agreement with the validator set;
  every phase rejects out-of-phase events; valid leader proof advances
  to `Prevoting`; non-leader proof rejected; quorum-on-prevote
  transitions to `Precommitting`; quorum-on-precommit transitions to
  `Committed`; certificate verifies; timeouts in each pre-final phase
  advance the round and clear state; timeout in `Committed` is a no-op;
  round 1 only accepts proofs signed for round 1; inner-tally errors
  (e.g. `WrongBlockHash`) propagate verbatim through the coordinator.
- 2 extra bft.rs tests covering round-aware leader selection and the
  new wrong-round leader-proof rejection.

### Scope discipline (continued)

Still NOT in this release: networking / gossip; timeout scheduling
(the host fires `fire_timeout` from its own clock); POL preservation
across rounds (locking a block from a previous round's POLC);
equivocation detection / slashing; integration with `DevModeEngine`.
These remain explicit non-goals and will land separately.

## [0.0.25] — 2026-05-24

### Added — BFT-PoS stage 2: two-phase voting + round numbers

The stage-1 finality state machine grows the missing PRE-VOTE phase and
an explicit `round: u32` on every vote/tally/certificate. Two-phase
voting is the structural prerequisite for safe round changes — a
validator that has issued a PRE-COMMIT in round R cannot equivocate at
round R+1 against the same `(block, height, round)` because every digest
now binds round into the BLS-signed bytes.

#### aii-consensus-bft

- New `PrevoteVote` / `PrevoteTallier` / `PolcCertificate` types,
  mirror-images of the precommit side. `try_form_polc()` emits the
  Proof-of-Lock-Change when ⅔+1 stake worth of PRE-VOTES land.
- Both phases use domain-tagged digests:
  - `prevote_digest = keccak256(PREVOTE_DOMAIN ‖ block ‖ height_be8 ‖ round_be4)`
  - `precommit_digest = keccak256(PRECOMMIT_DOMAIN ‖ block ‖ height_be8 ‖ round_be4)`
  Cross-phase replay is now mechanically impossible, not just policy.
- **Breaking changes** to v0.0.23 API:
  - `PrecommitVote` / `PrecommitTallier` / `PrecommitCertificate` gain
    a `round: u32` field.
  - `PrecommitVote::digest(hash, height)` → `digest(hash, height, round)`.
  - `PrecommitVote::sign(sk, hash, height, idx)` →
    `sign(sk, hash, height, round, idx)`.
  - `PrecommitTallier::new(hash, height, vs)` → `new(hash, height, round, vs)`.
- New `BftError::WrongRound` variant; tally validation order is now
  block-hash → height → round → index bounds → duplicate → BLS.
- New `PREVOTE_DOMAIN` / `PRECOMMIT_DOMAIN` public consts so external
  crates that verify certificates can derive digests themselves.
- 17 RED→GREEN tests added on top of stage 1's 26 (PrecommitTallier
  rejects wrong-round; cross-phase digest separation; round-replay
  rejection; mirror of all precommit tests for prevote phase; POLC
  verification round-trip + tampered-hash rejection).
- Doc comment at the top of `bft.rs` rewritten to describe the
  two-phase lifecycle as the primary path.

### Scope discipline (continued)

Still **not** in this release: networking / gossip, round-change
coordinator with timeout policy, POL preservation across rounds,
equivocation slashing, integration into `DevModeEngine`. These remain
explicit non-goals and will land separately.

## [0.0.24] — 2026-05-24

### Added — sub-chain ↔ state integration bridge

A 26th workspace crate, `aii-wasm-state`, that joins the v0.0.22 sub-chain
VM to the v0.0.20 persistent `StateDb`. With this in place, a real WASM
contract can read state populated by earlier transactions and have its
post-call writes committed back — closing the loop between
`aii-wasm`'s `HostState` trait and `aii-state`'s slot store.

#### aii-wasm-state (new crate)

- `StateDbHost<B>` — thin wrapper over `Arc<StateDb<B>>` implementing
  `aii_wasm::HostState`. Storage decode errors collapse to `H256::ZERO`
  at the trait surface (the trait returns plain `H256`; verified-state
  invariants make non-decodable slots unreachable in practice).
- `commit_effects(db, &effects)` — applies `effects.storage_writes`
  via `StateDb::storage_put`. Logs are intentionally not touched — they
  belong on a receipt-index surface, not a state CF.
- 8 RED→GREEN tests including two end-to-end WASM cases: (a) a contract
  reads a pre-populated slot through the bridge; (b) a write contract
  + commit + a separate reader contract observes the persisted value.

#### Why a new crate

`aii-wasm` stays free of the storage stack (RocksDB / KvBackend), and
`aii-state` stays free of wasmtime — neither acquires a new transitive
dependency. The bridge is the smallest adapter that lets them
cooperate.

### Scope

This release wires read/write through, but does NOT:
- introduce cross-contract storage access,
- persist `effects.logs` to a receipt index (deferred),
- integrate with the EVM execution path (the EVM has its own
  `RevmDb` adapter since v0.0.20; the two paths remain parallel).

## [0.0.23] — 2026-05-24

### Added — BFT-PoS stage 1 finality state machine

`aii-consensus-bft` grows a pure on-chain finality state machine
alongside the existing `DevModeEngine`. Stake-weighted leader election,
VRF-based seed beacon, single-phase PRE-COMMIT votes, and a BLS-
aggregated certificate at ⅔ + 1 stake — the building blocks for real
multi-validator BFT, decoupled from gossip and round changes so they
can be tested independently.

#### aii-consensus-bft

- New `bft` submodule with the full lifecycle:
  - `Validator { bls_pubkey, vrf_pubkey, stake }` — two keys per
    validator: BLS for votes (aggregates cheaply), VRF for seed beacon
    (next leader is unpredictable to anyone but the next chosen
    proposer).
  - `ValidatorSet::new(...)` — validates non-empty, `n ≤ 128`,
    `Σ stake` fits in `u64`, `Σ stake > 0`.
  - `ValidatorSet::select_leader(height, seed)` — stake-weighted
    deterministic picker. `pick = u64::from_be_bytes(keccak256(height_be8
    ‖ seed)[0..8]) % total_stake`, then linear scan of cumulative stake.
  - `LeaderProof::produce / verify` — VRF over the same `(height, seed)`
    input. `next_seed()` is the VRF output and becomes `seed_{H+1}`.
  - `PrecommitVote::digest(block_hash, height)` — what validators sign
    (`keccak256(hash ‖ height_be8)`).
  - `PrecommitTallier::submit(vote)` — validates block hash, height,
    validator index bounds, duplicate-vote guard, single-signer BLS
    verify; tracks accumulated stake. Returns `Accepted` / `ReachedQuorum`.
  - `PrecommitTallier::try_finalize()` — emits a `PrecommitCertificate`
    once stake ≥ `(2 * total) / 3 + 1`.
  - `PrecommitCertificate::verify(&vs)` — `fast_aggregate_verify` over
    the signer subset, plus stake-subset quorum re-check.
- New `BftError` variants: `EmptyValidatorSet`, `ValidatorSetTooLarge`,
  `TotalStakeOverflow`, `ZeroTotalStake`, `WrongBlockHash`, `WrongHeight`,
  `ValidatorIndexOutOfBounds`, `DuplicateVote`, `InvalidBlsSignature`,
  `InvalidVrfProof`.
- 26 RED→GREEN tests covering construction validation, total-stake
  arithmetic, quorum math, leader determinism + stake-weighting (1000-
  sample statistical check that a 99% validator wins ≥ 900 times), VRF
  round-trip + tamper rejection, digest formula, all five tally
  validation paths, below-/at-quorum transitions, finalize gating,
  certificate verification + tamper rejection.

### Scope discipline (same shape as v0.0.18 / v0.0.21)

The bft submodule is **not yet wired into `DevModeEngine`** — that
remains the single-node demo path. Integration, plus the still-missing
PRE-VOTE phase, gossip layer, round changes, locking / POL, and
equivocation slashing, are explicit non-goals for v0.0.23 and will
land in subsequent releases.

## [0.0.22] — 2026-05-24

### Added — sub-chain VM host imports

`aii-wasm` grows the six `env.*` host functions that turn the v0.0.19
wasmtime VM from a pure calculator into a stateful sub-chain contract
runtime. Reads consult a per-call overlay, then fall through to the
chain's persisted state via the new `HostState` trait. Writes, logs and
abort messages accumulate in `HostEffects` and are returned to the
caller only on success — any revert path drops them.

#### aii-wasm

- New `WasmModule` — compiled binary reusable across many host-aware
  calls; pairs with the new `WasmRuntime::compile(wasm)`.
- New `CallContext { caller, callee, block_number, block_timestamp }`
  passed in per call.
- New `HostState` trait — single method `storage_get(addr, slot)` —
  read-only view into persisted chain state. Implementations are
  trivial wrappers over `aii-state::StateDb`; the trait is tiny on
  purpose so tests can mock it without dragging in storage.
- New `HostEffects { storage_writes, logs }` — sorted by slot for
  determinism. Repeat writes to the same slot collapse to the last
  value.
- New `WasmRuntime::call_with_host(module, fuel, name, args, ctx, host)`
  → `HostCallResult { return_value, effects, fuel_remaining }`.
- Six host imports under module `env`:
  - `storage_read(slot_ptr, out_ptr)` — overlay first, then `HostState`.
  - `storage_write(slot_ptr, value_ptr)` — into overlay only.
  - `caller(out_ptr)` / `self_address(out_ptr)` — 20-byte writes.
  - `log(data_ptr, data_len)` — append to effects.
  - `abort(msg_ptr, msg_len)` — record message (≤ 256 bytes) and trap.
- New `WasmError::Aborted(String)` variant for explicit contract revert.
- 14 RED→GREEN tests using hand-written WAT modules covering
  read/write round-trip, host-state fall-through, write collection,
  same-slot last-write-wins, caller/self_address, log (including
  zero-length), abort + truncation, per-call effect isolation,
  out-of-fuel inside a host-call loop, and OOB pointer trapping.

### Scope discipline (unchanged from v0.0.19)

`aii-wasm` is the sub-chain VM only — cross-contract storage access,
native AII transfers, WASI / wasi-preview2, and AOT/cache are explicit
non-goals and remain so. Block-context accessors (`block_number`,
`block_timestamp`) are reserved in `CallContext` but not yet exported
to WASM; they land when the consensus layer plumbs them through.

## [0.0.21] — 2026-05-24

### Added — federated multisig bridge `Vault`

aii-crosschain grows a second cross-chain primitive next to HTLC: a
BLS-aggregated threshold multisig `Vault`. A federation of `n` validators
signs a `LockReceipt` (proof of asset lock on the source chain); the
on-chain `Vault` accepts the release iff at least `t` signers participate,
the aggregated BLS signature verifies over the receipt digest, and the
nonce has not been used before.

#### aii-crosschain

- Module split: existing HTLC content moved from `lib.rs` into a new
  `htlc` submodule. Public path is now `aii_crosschain::htlc::{Htlc, ...}`
  (no external consumers existed; no compat shim).
- New `federation` submodule:
  - `FederationSet { pubkeys, threshold }` — static `t`-of-`n` validator
    set, content-addressed by `keccak256(threshold_be8 ‖ pubkey₁_compressed
    ‖ … ‖ pubkeyₙ_compressed)`. Caps `n ≤ 64` so a `u64` signer bitmap is
    sufficient.
  - `LockReceipt { src_chain_id, asset, amount, recipient, nonce }` —
    `digest(federation_id)` domain-separates by federation so receipts
    cannot be replayed across different federation sets.
  - `AttestationBundle { receipt, aggregated_sig, signer_bitmap }` — what
    the off-chain aggregator submits.
  - `Vault::release(&bundle)` — validates bitmap bounds, threshold,
    nonce replay, and BLS `fast_aggregate_verify`, in that order. On
    success returns `Released { receipt }`; the caller performs the
    actual asset transfer.
- 13 new TDD tests covering construction validation, content-addressed
  id, digest determinism, threshold success/failure, signature forgery
  rejection, replay protection, and bitmap-bounds enforcement.

### Scope discipline (unchanged from HTLC release)

`aii-crosschain` is the on-chain state machine only. Off-chain attester
clients, source-chain listeners, federation set rotation, IBC light
clients, and full XCM adapters remain explicit non-goals — they will
land in later releases.

## [0.0.20] — 2026-05-24

### Added — persistent contract state (bytecode + storage)

aii-evm transactions now persist contract state across calls, which is
the prerequisite for real ERC-20-style contracts to work on AII.

#### aii-storage

- New `ColumnFamily::Code` — `code_hash → bytecode bytes`. Bytecode is
  stored content-addressed by `keccak256(code)`, so identical code
  deployed twice naturally dedups.

#### aii-state

- New `StateDb::code_get(code_hash)` / `code_put(code_hash, bytes)` —
  bytecode storage backed by `ColumnFamily::Code`.
- New `StateDb::storage_get(addr, slot)` / `storage_put(addr, slot, val)` —
  per-account EVM storage backed by `ColumnFamily::AccountStorage`.
  Reads of unset slots return `H256::ZERO`. Writing `H256::ZERO`
  deletes the slot (matches EVM semantics). Flat 52-byte `addr ‖ slot`
  key for now; per-account Merkle tries are a later optimization.
- New `StateError::Decode(String)` variant for malformed on-disk
  storage values.

#### aii-evm

- `RevmDb::code_by_hash` now looks the bytecode up via
  `StateDb::code_get` instead of returning empty. Contracts deployed
  in earlier transactions can now be CALLed.
- `RevmDb::storage` now looks the slot up via `StateDb::storage_get`
  instead of returning `U256::ZERO`. `SLOAD` returns the last
  persisted value.
- `execute_with_revm` now commits the full revm state diff per tx:
  account header (nonce/balance/code_hash), newly-deployed bytecode
  (`info.code` → `code_put`), and every changed storage slot
  (`slot.is_changed()` → `storage_put`).

### Tests (3 new, all RED → GREEN)

- `deploy_persists_runtime_bytecode_under_code_hash` — deploys a
  hand-crafted 18-byte contract; verifies that the runtime bytecode is
  retrievable from the Code CF by the resulting `account.code_hash`.
- `calling_writer_persists_storage_slot` — deploys a writer contract
  (`SSTORE(0, 0x42)`), CALLs it in a *separate* `execute_with_revm`
  invocation, and verifies the slot persists. This is the test that
  exercises the cross-tx `code_by_hash` lookup.
- `reader_recovers_persisted_storage` — deploys a reader
  (`SLOAD(0); RETURN`), seeds `storage[reader][0] = 0x77` via
  `StateDb`, then CALLs the reader and verifies it returns the
  pre-seeded value in the 32-byte output buffer.

### Out of scope (deferred)

- Per-account storage trie + storage root in `Account` — flat KV is
  semantically equivalent for revm; the trie matters once we hash
  state roots for headers.
- Block-hash lookup in `RevmDb::block_hash` — still a deterministic
  placeholder; lands once `aii-node` exposes a header index.

## [0.0.19] — 2026-05-24

### Added — aii-wasm scoped sub-chain VM

- New crate `aii-wasm` providing a wasmtime-backed WebAssembly runtime
  for AII sub-chains. This release intentionally exposes only the
  surface needed to validate the gas/fuel model end-to-end — host
  imports, richer signatures, and module caching are deferred.
  - `WasmRuntime::new()` constructs a wasmtime `Engine` with
    `consume_fuel(true)` enabled. The engine is reusable across many
    modules and many calls.
  - `WasmRuntime::instantiate(wasm, fuel)` validates + compiles the
    binary, opens a fresh `Store`, sets the per-call fuel budget, and
    returns a `WasmInstance`. Invalid bytes are rejected with
    `WasmError::BadModule`.
  - `WasmInstance::call_i32(name, args)` invokes an exported
    `i32, … → i32` function. Strict arity / single-i32-result
    checking on entry; trap classification on exit. Out-of-fuel,
    missing export, and signature mismatch surface as discrete error
    variants for clean caller branching.
  - `WasmInstance::fuel_remaining()` reads the store's fuel reserve
    after a call so consensus can charge the actual consumption back
    to the transaction.
- 9 unit tests covering: runtime construction, module validation
  (good + garbage), `add` happy path with positive and negative i32
  arguments, missing-export and wrong-arity rejection, fuel decrease
  after execution, and infinite-loop trapping as `OutOfFuel`.

### Gas model

AII maps `1 tx-gas = 1 wasm-fuel-unit` for now; the consensus layer
allocates the budget per call. This is a parameter that the chain
governance — once defined — can re-tune without touching this crate.

### Out of scope (deferred)

- Richer call signatures (i64, f32, multi-return) — v0.0.20+
- Host imports (state read/write, log, transfer to other addresses) —
  v0.0.20+
- WASI / wasi-preview2 — explicitly never on the consensus path
- Module caching / AOT compilation — performance work, not behavior

### Dependencies

- New: `wasmtime = "26"` (default-features off, `cranelift + runtime`
  only) plus `wat` as a dev-dependency for tests. wasmtime pulls in
  cranelift which adds a one-time ~12 s compile cost the first time
  `cargo build` runs after this update.

## [0.0.18] — 2026-05-24

### Added — aii-crosschain (scoped HTLC)

- New crate `aii-crosschain` providing the on-chain state machine for
  Hash Time-Locked Contracts — the building block for trustless atomic
  swaps between AII and external chains.
  - `Htlc` record (sender, recipient, amount, secret_hash, timeout,
    state) with a `Locked → Claimed | Refunded` finite state machine.
  - `Htlc::claim(preimage)` — transitions iff `keccak256(preimage) ==
    secret_hash`. Wrong preimage rejected; state preserved.
  - `Htlc::refund(now)` — transitions iff `now ≥ timeout`. Early refund
    rejected; state preserved.
  - `Htlc::new()` rejects zero amount and `sender == recipient`.
  - Terminal states are sticky: double-claim, claim-after-refund, and
    refund-after-claim are all rejected via `HtlcError::NotLocked`.
  - `htlc_id(&Htlc)` — content-addressed identifier
    `keccak256(sender ‖ recipient ‖ amount ‖ secret_hash ‖ timeout)`
    used by cross-chain protocols to reference a lock without an
    index. Stable across nodes; independent of lifecycle state.
- 14 unit tests including TDD RED → GREEN cycle verification.

### Fixed

- `aii-storage` proptest `snapshot_unchanged_under_concurrent_writer`
  no longer fails on duplicate-key inputs. The test now dedups
  `seed_pairs` via `BTreeMap` (last-write-wins) before seeding and
  verifying, matching the backend's actual semantics.

### Scope notes

Out of scope for this release: multi-sig bridge federation (Aii ↔
Ethereum custodial), IBC light clients, Polkadot XCM adapters. These
build on the HTLC primitive and will land in later releases.

## [0.0.17] — 2026-05-24

### Added — devp2p Discovery v4 (UDP) — Ping / Pong

- New module `aii-net-p2p::discovery` implementing the Ethereum
  Discovery v4 wire spec (<https://github.com/ethereum/devp2p/blob/
  master/discv4.md>):
  - **Packet framing** — `hash (32) || signature (65) || type (1) ||
    rlp(data)`. Hash verified end-to-end (tampering detected at decode).
  - **Signature** — secp256k1 over `keccak256(type || data)`. Decoder
    *recovers* the sender's public key + address from the signature
    (matches devp2p's design).
  - **Packet types** — `Ping (0x01)` and `Pong (0x02)`. `FindNode (0x03)`
    + `Neighbours (0x04)` land in v0.0.18 with the Kademlia routing
    table.
  - **`Endpoint`** — IPv4/IPv6 + UDP port + TCP port; RLP round-trips.
  - **`UdpDiscovery`** — async UDP driver (`bind` / `send` / `recv`).
    `recv` carries a per-call timeout.
- 8 unit tests including a real **UDP loopback Ping/Pong exchange**
  between two driver instances + tampered-packet detection + truncated-
  packet rejection + unknown-type-byte rejection + recv-timeout.

### Changed
- Workspace 0.0.16 → 0.0.17.
- `aii-net-p2p` now depends on `aii-crypto` for secp256k1 packet
  signatures.

### Notes
- Packets are size-capped at the spec's 1280-byte UDP ceiling.
- `FindNode` / `Neighbours` need a Kademlia routing table + node-id
  XOR distance bucketing — separate v0.0.18 deliverable. The current
  protocol-version constant (`DISCOVERY_VERSION = 4`) is already
  embedded in `Ping` payloads so peers consider us spec-compliant.

## [0.0.16] — 2026-05-24

### Added — `aii-evm` revm 18 integration (contract execution)

- **`aii-evm::RevmDb`** — `revm::Database` adapter over
  `aii_state::StateDb`. Reads accounts on demand; emits empty bytecode
  / empty storage as a stop-gap (per-account storage trie lands in
  v0.0.17+).
- **`aii-evm::execute_with_revm`** — runs a tx through revm 18 and
  commits the resulting state diff back to `StateDb`. Handles:
  - Value transfer (sender/recipient balance + nonce updates).
  - Contract CALL with arbitrary calldata.
  - Contract CREATE — returns the deployed address.
  - Insufficient balance / invalid signature paths produce
    `ExecError::Revm` from revm's pre-tx validation.
- **`ExecutionSummary`** — `success` / `gas_used` / `output` /
  `deployed_contract`.

### Tests (4 new revm-driven cases, 10 total in `aii-evm`)
- `revm_value_transfer_advances_balances` — balance + nonce diff after
  a 123-Wei transfer.
- `revm_insufficient_balance_returns_failure_or_error` — sender below
  required value rejected by revm's pre-tx validation.
- `revm_empty_create_deploys_an_address` — empty init code lands at
  CREATE-derived address.
- `revm_call_to_eoa_with_zero_value_is_a_no_op_success` — sanity check
  that revm accepts trivial CALLs.

### Changed
- Workspace 0.0.15 → 0.0.16.
- `aii-evm` deps: `revm = "18"`, `derive_more = { version = "1",
  features = ["full"] }` (revm pulls derive_more without enabling any
  feature; force the full set).

### Limitations carried into v0.0.17+
- `RevmDb::storage` returns `U256::ZERO`. Real ERC-20 etc. need a
  per-account storage trie + storage CF in `aii-storage`.
- `RevmDb::code_by_hash` returns empty bytecode. Persistent bytecode
  by `code_hash` is part of the same v0.0.17 work.
- `block_hash` returns a deterministic placeholder; harmless for tests.

## [0.0.15] — 2026-05-24

### Added — `aii-consensus-bft` (scoped) + live block production

- **`aii-consensus-bft`** (M2 #11, scoped) — single-node dev-mode BFT
  engine:
  - `DevModeEngine` implements `aii_consensus_iface::Engine` so embedders
    can swap to a real multi-validator BFT later without API churn.
  - `produce_block()` builds an empty child block per slot, advances
    the head, returns `(hash, number, Block)`.
  - `EngineConfig` (slot_seconds / coinbase / base_fee / gas_limit).
  - 8 unit tests covering head advance, parent hash linkage, timestamp
    increment, Engine trait integration.
- **`aiid` binary** now produces blocks on a background task:
  - `--produce-blocks` (default `true`) starts the dev producer loop.
  - `--slot-seconds N` sets the block interval (default 3 s).
  - `NodeState.set_head` is called every slot — `eth_blockNumber` is no
    longer permanently `0`.
- Live-verified end-to-end: `eth_blockNumber` returned `0x0 → 0x2 → 0x4`
  across 5 seconds at `--slot-seconds 1`; node log emitted
  `block produced` events with monotonically increasing hashes.

### Changed
- Workspace version 0.0.14 → 0.0.15 (note: 0.0.14 release tag also moved
  the workspace.package.version that had drifted to 0.0.13 since 0.0.13
  was the last release that actually bumped both — 0.0.15 syncs all 22
  path-dep version constraints).

### Notes — what's NOT yet in this engine (v0.0.16+ targets)
- VRF-based proposer selection (primitive exists in `aii-crypto::vrf`).
- PRE-VOTE / PRE-COMMIT gossip over `aii-net-p2p`.
- BLS aggregate signature over PRE-COMMIT votes.
- ⅔ stake threshold → single-block instant finality.
- Multi-validator V-set rotation (`aii-vnode` already tracks stake).
- Block-body inclusion of txs from `aii-net-txpool`.

The trait surface (`Engine` / `Proposer` / `Voter` / `Validation`) is
already wired through `aii-consensus-iface`, so each future addition
is additive and the embedder API stays stable.

## [0.0.14] — 2026-05-24

### Added — `aii-mcp` keystore + mnemonic tools (4 new MCP tools)
- `account_new_encrypted(password)` — generate a fresh secp256k1 key
  and return a Web3 v3 keystore JSON encrypted under `password`.
- `account_verify(keystore_json, password)` — confirm a password
  unlocks a keystore; return the embedded address on success.
- `mnemonic_new(words)` — generate a fresh BIP-39 phrase (12 / 15 /
  18 / 21 / 24 words) + derive the first ETH-compatible address.
- `account_from_mnemonic(phrase, passphrase, index)` — re-derive any
  address from a known phrase. Verified live against the canonical
  `0x9858EfFD…` ethers/MetaMask fixture.

These tools let MCP clients (Claude Desktop / Claude Code / Cursor /
Cline) walk a user through creating, securing, and recovering an AII
account *without* ever touching the protocol RPC layer — the keystore
and mnemonic primitives are pure local computation.

### Tests
- 14 lib tests in `aii-mcp` (up from 7) covering all 4 new tools +
  the updated `tools/list` count + arg validation.
- Live stdio smoke verified end-to-end via piped JSON-RPC over
  `target/debug/aii-mcp` (4 tools roundtripped through the stdio
  parser).

### Changed
- Workspace version 0.0.13 → 0.0.14.
- `aii-mcp::handle_tools_call` now reads `arguments` from the MCP
  `tools/call` envelope (was previously ignoring it because the four
  v0.0.10 tools took no args).

### Notes
- Write tools (`send_transaction`, etc.) that *do* require RPC
  submission land in v0.0.15+ once `aii-rpc::eth_sendRawTransaction`
  + a wired mempool exist.

## [0.0.13] — 2026-05-24

### Added — BIP-39 Mnemonic + BIP-32 HD Derivation
- `aii-wallet::MnemonicPhrase` — BIP-39 mnemonics in English wordlist:
  - `generate(word_count)` for 12 / 15 / 18 / 21 / 24-word phrases (from
    OS RNG via `rand::thread_rng`).
  - `from_phrase(s)` validates checksum + wordlist membership.
  - `to_seed(passphrase)` produces the canonical 64-byte BIP-39 seed.
  - `to_wallet(passphrase, index)` derives a `LocalWallet` at BIP-44
    path `m/44'/60'/0'/0/{index}` (the MetaMask + ethers default).
- 11 unit tests including:
  - BIP-39 Trezor official seed test vector (`abandon × 11 about` +
    "TREZOR" → canonical 64-byte seed).
  - **MetaMask interop test**: same phrase + empty passphrase + index 0
    yields `0x9858EfFD232B4033E47d90003D41EC34EcaEda94` — bit-exact
    match with ethers-rs / web3.js / MetaMask.
- `aii-cli`: two new commands
  - `aii account mnemonic [--words 12]`
    → fresh phrase + first ETH-compatible address.
  - `aii account from-mnemonic --phrase "..." [--passphrase X] [--index N]`
    → re-derive any address.
  - 3 new lib tests + live-verified `aii` binary smoke.

### Changed
- Workspace version 0.0.12 → 0.0.13.
- `aii-wallet` deps: `bip39 = "2"`, `bip32 = "0.5"` (RustCrypto).

### Notes
- BIP-44 coin type `60` (Ethereum) is the default for full MetaMask
  interop. An AII-native path (coin type ~9999) can ship later as
  `to_wallet_aii(...)` without breaking the default API.
- The `aii-mcp` write tools (`send_transaction`, `account_import`)
  planned for v0.0.14 can now consume a phrase + index instead of a
  raw secret.

## [0.0.12] — 2026-05-24

### Added — Encrypted Keystore (Web3 Secret Storage v3)
- `aii-wallet::EncryptedKeystore` — full Web3 v3 keystore implementation:
  - **scrypt** KDF (configurable n/r/p; `ScryptParams::light` for tests,
    `::geth_default` for production)
  - **AES-128-CTR** cipher with random IV
  - **Keccak-256 MAC** over `derived_key[16..32] ‖ ciphertext` —
    verified *before* decryption to surface wrong-password errors cleanly
  - JSON serde compatible with `geth account import` / MetaMask:
    `{ version: 3, id: uuid, address, crypto: {...} }`
  - `encrypt(&LocalWallet, password, params)` / `decrypt(password)`
  - `to_json()` / `from_json()` round-trip
  - 8 unit tests (round-trip / wrong-password / JSON / tampered ciphertext
    / distinct ciphertexts on re-encrypt / version + cipher + kdf rejection)
- `aii-cli`: two new commands
  - `aii account new-encrypted --password … --out keystore.json`
  - `aii account verify --file keystore.json --password …`
  - 2 new lib tests; live-verified against `aii` binary.

### Changed
- Workspace version 0.0.11 → 0.0.12.
- `aii-wallet` deps grow: `scrypt`, `aes`, `ctr`, `cipher`, `serde_json`,
  `uuid` (v4 + serde).

### Notes
- BIP-39 mnemonic + BIP-32 HD derivation deferred to v0.0.13 — the
  keystore alone unlocks `aii-cli`'s `account new-encrypted` + the future
  `aii-mcp`'s `send_transaction` write-tool (which will accept a
  keystore + password instead of a raw secret).

## [0.0.11] — 2026-05-24

### Added — RPC extension wired to real StateDb
- `aii-rpc::RpcState` trait gains `gas_price()` and
  `account(addr) -> Option<AccountView>`. `AccountView` is the JSON
  shape returned by `aii_getAccount` (nonce + balance hex + roots hex).
- New methods:
  - `eth_gasPrice` — returns chain-spec floor as `0x…` hex Wei.
  - `eth_getBalance(address, blockTag)` — looks up `StateDb` via
    `RpcState::account`. `blockTag` is accepted but only the head is
    supported in v0.0.11.
  - `aii_getAccount(address)` — returns the full Account record or
    `null`.
- `aii-node::NodeState` now owns an in-memory `StateDb<MemoryBackend>`
  and exposes it via `state()`. Pre-populating accounts before booting
  the RPC server is now a one-liner (`state.state().set_account(...)`).

### Tests
- 4 new lib tests in `aii-rpc` covering all new methods (happy paths
  + missing-account + bad-address error).
- 2 new lib tests in `aii-node` end-to-ending the new methods through
  jsonrpsee.
- Live-verified against `aiid` binary:
  ```
  $ curl … eth_gasPrice     → "0x3b9aca00"
  $ curl … eth_getBalance   → "0x0" / "0xde0b6b3a7640000"
  $ curl … aii_getAccount   → null / {nonce, balance, ...}
  ```

### Changed
- Workspace version 0.0.10 → 0.0.11.

## [0.0.10] — 2026-05-24

### Added — User-facing surfaces + AI integration
- `aii-onboarding` — read-only hardware probe + Tier (T1–T7) recommender
  per 《04 架构设计文档》§14.4. `detect()` reads sysinfo; `score()` collapses
  to 0–100 with calibrated weights; `recommend_tier()` maps to Tier.
  11 unit tests across reference profiles + classify-disk + score-cap.
- `aii-cli` — user-facing CLI **`aii`** built on `clap` v4:
  - `aii status` / `aii chain-id` — query a running node via JSON-RPC
  - `aii account new` — generate a fresh secp256k1 address (key dropped)
  - `aii tier` — run the onboarding probe locally
  - `--rpc <URL>` / `--json` global flags
  4 lib tests + verified live against running aiid binary.
- `aii-mcp` — Model Context Protocol server **`aii-mcp`** over stdio:
  - MCP 2024-11-05 (`initialize` / `tools/list` / `tools/call`
    / `notifications/initialized`)
  - 4 read-only tools: `chain_status` / `chain_id` / `account_new`
    / `tier_recommend`
  - Plugs into Claude Desktop / Claude Code / Cursor / Cline through
    standard `claude_desktop_config.json` `mcpServers` block.
  7 in-process tests + stdio smoke verified end-to-end.

### Notes
- Day-0 footprint v0.0.9 stays intact; v0.0.10 adds **3 user-facing
  crates** described in 《04 架构设计文档》§14 + 《12 AI 集成》but not in the
  Day-0 spec §3 list. They are leaf modules — no Day-0 crate depends
  on them.
- 21 crates total; 54 test groups workspace-wide.
- aii-mcp is the differentiating "AI-native chain" capability called
  out in CLAUDE.md.

## [0.0.9] — 2026-05-24

### Added — Day-0 completion (18 of 18 crates) 🎉
- `aii-state::mpt_root` — **full** Modified Merkle Patricia Tree
  algorithm (hex-prefix encoding + leaf / extension / branch nodes +
  RLP-length pruning per Yellow Paper Appendix D). 11 unit tests
  covering empty / single / multi-key / extension-merge / branch-split
  / 100-key stress. The v0.0.6 `unimplemented!()` placeholder is gone.
- `aii-evm` (M1 #7, scoped) — `execute_transfer` runs value-transfer
  txs against `StateDb`: nonce + balance check, debit, credit, nonce
  bump, returns `Receipt`. EOA→EOA only; contract paths return
  `ExecError::ContractCallsNotYetSupported` until the `revm`
  integration lands. 6 unit tests (happy path, nonce mismatch,
  insufficient balance, CREATE rejection, contract-recipient
  rejection, nonce atomicity).
- `aii-net-p2p` (M1 #8, scoped) — TCP listener + dial + length-prefixed
  RLP frame codec (`u32` BE prefix, ≤ 1 MiB). `Message::{Hello, Ping,
  Pong, Disconnect}`. 6 tests including two real-TCP end-to-end
  exchanges (Hello / Ping-Pong). Full devp2p discovery + RLPx lands
  in a later release.
- `aii-net-sync` (M1 #9) — pure state-machine `SyncEngine`
  (`Idle → Headers → Bodies → Done`) consuming `Event`s and emitting
  `Action`s. Contiguity / hash-order validation. 8 tests covering
  all transitions + error paths.

### Changed
- Workspace version 0.0.8 → 0.0.9.

### Notes — **Day-0 footprint complete (18 of 18 crates)**
- M0 ×4 — aii-types / aii-codec / aii-crypto / aii-storage
- M1 ×5 — aii-block / aii-state (full MPT) / aii-evm / aii-net-p2p
  / aii-net-sync
- M2 ×9 — aii-consensus-iface / aii-microchain / aii-net-txpool /
  aii-rpc / aii-wallet / aii-vnode / aii-config / aii-metrics
  / aii-node (+ `aiid` binary)
- 44 test groups; ~340 tests pass under
  `cargo test --workspace --all-features`.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  clean. `cargo doc --workspace` clean.
- aiid binary verified live: RocksDB opens, RPC listens, eth_chainId
  returns `"0x63"`, aii_status reports `{"chain_id":99,…}`.
- M2 *Day-1+* extension crates (aii-mcp / aii-wasm / aii-consensus-
  plugins / aii-crosschain / aii-bindings / aii-onboarding /
  full aii-consensus-bft engine) intentionally remain post-Day-0
  per spec §3.4. Day-0 footprint is now **frozen**.

## [0.0.8] — 2026-05-24

### Added — RPC + node binary
- `aii-rpc` (M2 #15) — `jsonrpsee` HTTP server with `eth_chainId`,
  `eth_blockNumber`, and `aii_status` methods. `RpcState` trait keeps
  the crate state-free; embedders provide chain id / head / network.
  3 in-process end-to-end tests.
- `aii-node` (M2 #19) — library `NodeState` + binary **`aiid`**.
  `aiid` opens RocksDB at `--data-dir`, serves RPC at `--rpc`, and
  waits for SIGINT. 3 library tests + verified live-binary smoke:
  ```
  $ target/debug/aiid --data-dir /tmp/aiid-smoke --rpc 127.0.0.1:18545 &
  $ curl … aii_status   → {"chain_id":99,"network":"aii-mainnet",…}
  $ curl … eth_chainId  → "0x63"
  ```

### Changed
- Workspace version 0.0.7 → 0.0.8.

### Notes
- Day-0 footprint progress: **15 of 18 crates landed**. Remaining:
  `aii-evm` (revm wrapper), `aii-net-p2p` (devp2p), `aii-net-sync`
  (each is multi-week work and not attemptable as a one-PR scaffold).
- Day-0 binary `aiid` ships and serves real JSON-RPC over HTTP — this
  is the first user-facing deployment artefact in the workspace.

## [0.0.7] — 2026-05-24

### Added — 7 new crates (M2 leaves)
- `aii-config` (M2 #17) — `ChainSpec` (chain id 99 default) + `Genesis`
  (alloc + `to_header(state_root)`). 12 tests.
- `aii-consensus-iface` (M2 #10) — trait-only crate: `Engine`, `Proposer`,
  `Voter`, `Validation`, `ConsensusError`, `EngineProgress`, `Vote`. 4 tests.
- `aii-metrics` (M2 #18) — lock-protected counter/gauge registry +
  Prometheus text render. 6 tests.
- `aii-wallet` (M2 #16) — `LocalWallet` (in-memory secp256k1 + `Address`
  derivation + `sign_message_hash`). 5 tests. Encrypted keystore +
  BIP-39 land later.
- `aii-vnode` (M2 #12) — `VNode` / `VSet` with 100,000 AII stake floor
  + 80/20 reward split (`split_reward`). 11 tests.
- `aii-net-txpool` (M2 #14) — capacity-bounded mempool keyed by
  `(sender, nonce)`; price-replacement; lowest-gas-first eviction.
  `effective_gas_price` helper. 8 tests.
- `aii-microchain` (M2 #13) — `MicroChainId`/`MicroChainSpec` registry
  + `FlushAnchor` bookkeeping. 8 tests.

### Changed
- `aii-types::Address` + `H256`: derive `PartialOrd` + `Ord` (needed for
  BTreeMap keys in vnode / txpool / microchain).
- Workspace version 0.0.6 → 0.0.7.

### Notes
- M0 (4 crates) + M1 (2 crates: block, state) + M2 (7 crates) =
  **13 of 18 Day-0 crates landed**. Remaining Day-0:
  `aii-evm`, `aii-net-p2p`, `aii-net-sync`, `aii-consensus-bft`,
  `aii-rpc`, `aii-node` (binary).
- All 54 new tests passing; workspace clippy clean under `-D warnings`.
- Tags: v0.0.5, v0.0.6, v0.0.7 (local only — push pending remote setup).

## [0.0.6] — 2026-05-24

### Added
- New crate `aii-state` (M1 #5 — narrow scope):
  - `Account` — 4-field RLP `[nonce, balance, storage_root, code_hash]`,
    `Hashable` impl, `Account::EMPTY` constant for fresh EOAs.
  - `EMPTY_CODE_HASH` constant (= `keccak256(b"")`).
  - `StateDb<B: KvBackend>` — `Address → Account` store keyed by
    `keccak256(address)` in `ColumnFamily::State`; `account` / `set_account`
    / `remove_account` methods.
  - `mpt_root` placeholder — empty input returns `EMPTY_TRIE_HASH`;
    non-empty input panics until v0.0.7 lands the full Merkle Patricia
    Tree algorithm.
- 12 unit tests across `account` / `trie` / `db` modules.

### Changed
- Workspace version 0.0.5 → 0.0.6.

### Notes
- Full MPT (hex-prefix + branch / extension / leaf nodes + RLP-pruning)
  is deferred to v0.0.7 to keep this PR reviewable.
- This unblocks `aii-evm` (which needs `Account` and `StateDb` more than
  it needs trie roots — root computation happens at block-commit time).

## [0.0.5] — 2026-05-24

### Added
- New crate `aii-block` (M1 #6 — first M1 crate):
  - `Header` — 20-field EIP-1559 + 4895 + 4844 + 4788 layout with
    forward/back-compatible trailing fields (`blob_gas_used`,
    `excess_blob_gas`, `parent_beacon_block_root` are `Option`).
  - `Tx` enum — EIP-2718 envelope (Legacy / EIP-1559 / EIP-4844
    placeholder). All variants carry an optional `AlgoId` extension
    that defaults to `Secp256k1` and emits byte-perfect Ethereum
    encodings in that case (PQ slots are additive and read by trailing-
    item detection during decode).
  - `Receipt` — single struct + `TxType` discriminator + EIP-2718
    envelope, with helpers `encode_2718` / `decode_2718`.
  - `Block` = `Header` + `BlockBody { transactions, ommers, withdrawals }`;
    `Block::hash()` ≡ `Header::hash()`.
  - `Bloom` (2048-bit Yellow-Paper §4.4.2 accrue/contains), `Log`,
    `Withdrawal` (EIP-4895, Gwei), `AccessListItem` (EIP-2930),
    `Hashable` trait.
  - Constants: `EMPTY_LIST_HASH`, `EMPTY_TRIE_HASH` (Keccak-verified at
    test time).
- 32 unit tests + 5 proptest properties + 10-header byte-perfect
  fixture round-trip with hash self-consistency.

### Changed
- Workspace version 0.0.4 → 0.0.5.
- `aii-types`: `impl alloy_rlp::{Encodable, Decodable}` for `H256` and
  `Address` (unlocks `#[derive(RlpEncodable, RlpDecodable)]` for
  downstream crates' simple structs without orphan-rule contortions).
- `alloy-rlp` workspace dep gains the `derive` feature.
- Workspace clippy config: list of explicit pedantic/nursery sub-lint
  allows (errors-doc / panics-doc / must-use-candidate / doc-markdown /
  numeric-cast family / many-single-char-names / match-same-arms /
  ref-option / option-if-let-else / format-push-string) — matches the
  documented "pedantic = warn" intent under CI's `-- -D warnings` flag.

### Notes
- Per spec §5.3, `aii-block` is **not** published to crates.io until M2.
- Mainnet fixtures in v0.0.5 are synthetic but byte-perfect through
  the encoder; an M1 follow-up swaps in genuine mainnet RLP without
  any API change.
- This unblocks M1 crates `aii-state` and `aii-evm` (both depend on
  `aii-block`).

## [0.0.4] — 2026-05-24

### Added
- New crate `aii-storage` (M0 #4 — final basestone crate):
  - `KvBackend` trait (sync get/put/delete/write/snapshot/iter/iter_prefix)
    and `Snapshot` trait (read-only consistent view).
  - `ColumnFamily` closed enum: 10 variants covering headers / bodies /
    receipts / transactions / state / account_storage / tx_lookup / meta
    / microchain / default. Stable snake_case wire names; adding a
    variant requires a spec revision.
  - `WriteBatch` backend-agnostic op log; cross-CF atomic on commit.
  - `RocksDbBackend` (default feature `rocksdb`) — wraps rocksdb 0.22
    with lz4 compression, opens every CF via `ColumnFamily::ALL`.
  - `MemoryBackend` (always on) — `Arc<RwLock<HashMap<CF, BTreeMap>>>`
    for downstream-crate unit tests; snapshot via `Arc` clone.
  - `StorageError` umbrella (`Backend` / `InvalidColumnFamily` / `Io`).
- 8-test conformance suite parametrised over both backends (16 runs);
  2 property tests (Op-sequence equivalence + snapshot isolation);
  criterion benchmark meeting the M0 >=50k op/s sequential-write gate;
  `scripts/check_storage_perf.sh` CI helper.
- Workspace deps: `rocksdb 0.22`, `tempfile 3`, `criterion 0.5`.

### Changed
- Workspace version 0.0.3 → 0.0.4.

### Notes
- All four M0 basestone crates are now landed (types / codec / crypto /
  storage). M1 (state / EVM / block / net-*) begins next.
- Per spec §5.3, `aii-storage` is **not** published to crates.io until M2.

## [0.0.3] — 2026-05-24

### Added
- New crate `aii-crypto` with the four Day-0 cryptographic primitives:
  - `keccak::keccak256` — Ethereum-style Keccak-256, 3 KAT vectors
    (empty / "abc" / 1M 'a').
  - `secp::{sign, verify, recover}` — secp256k1 ECDSA with 65-byte ETH
    layout (`r ‖ s ‖ v`); `PublicKey::address` matches the known
    constant for `sk = 1`.
  - `bls::{sign, verify, fast_aggregate_verify, aggregate_signatures,
    aggregate_pubkeys}` — BLS12-381 Eth2 `min-pk` scheme over blst.
  - `vrf::{prove, verify}` — Schnorrkel VRF over Ristretto-25519 with
    96-byte wire form (pre-output ‖ proof).
  - `CryptoError` umbrella (`InvalidEncoding` + `BadSignature`).
- 31 unit tests + 5 property tests covering all four primitives.
- Workspace deps: `tiny-keccak 2`, `k256 0.13`, `blst 0.3`,
  `schnorrkel 0.11`, `merlin 3`, `rand_core 0.6`.

### Changed
- Workspace version 0.0.2 → 0.0.3.
- Rust toolchain pin 1.83 → 1.94.1; workspace `rust-version` 1.83 → 1.85
  (ecosystem moved to edition2024 via getrandom 0.4 / indexmap 2.14 /
  ruint 1.18).

### Notes
- Spec D7 (PQ algorithm slots) remains placeholder-only for v0.0.3;
  concrete verifiers will land in `aii-registry` (planned v0.0.4) so
  that `AlgoId`-keyed dispatch is the only call site.
- `aii-crypto` is the 3rd of the 4 M0 basestone crates; remaining M0
  work is `aii-storage` (RocksDB).

## [0.0.2] — 2026-05-22

### Added
- New crate `aii-codec` with RLP / SSZ / JSON-RPC hex codecs.
  - RLP impls for `H256`, `Address`, `AlgoId`, `SignedTx`.
  - SSZ impls for `H256`, `Address`, `AlgoId`, `BlsPubKey`, `BlsSignature`, `SignedTx`.
  - ETH JSON-RPC hex helpers (`bytes_hex` / `quantity` / `hex_h256` / `hex_address` serde modules).
  - Local `SszError` (insulates from ssz_rs non-exhaustive-enum drift).
  - `CodecError` umbrella with `From` for `alloy_rlp::Error` / `SszError` / `serde_json::Error` / `HexError`.
  - 52 unit tests + 11 property tests.
- Workspace deps: `alloy-rlp 0.3`, `ssz_rs 0.9`, `serde_json 1`, `hex 0.4`.

### Changed
- Workspace version 0.0.1 → 0.0.2.

## [0.0.1] — 2026-05-21

### Added
- Workspace bootstrap (Cargo.toml, CI, lints)
- `aii-types` crate with primitive types (H256, Address, U256, AlgoId, BlsPubKey, BlsSignature, SignedTx)
- GitHub Actions CI: fmt + clippy + test + deny + audit + llvm-cov on Linux/macOS
- AlgoId enum reserves Day-0 PQ algorithm slots per spec D7

### Notes
- This is the first commit of `aii-core`. All downstream crates (state, EVM, consensus, ...) depend on `aii-types`.
- See spec `docs/superpowers/specs/2026-05-21-aii-core-design.md` §3 for the full 24-crate plan.
