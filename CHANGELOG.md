# Changelog

All notable changes to AII workspace follow [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
