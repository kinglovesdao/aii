# aii-consensus-bft

Main-chain BFT-PoS consensus engine for AII.

**v0.0.15 scope:** single-node **dev-mode** block production. Each slot
(default 3s), the engine builds an empty block on top of the current
head, signs the header trivially, and advances the head. This lets
`aii_status` / `eth_blockNumber` return a monotonically-increasing
height — useful for end-to-end testing of RPC / MCP / sync / wallet
without yet wiring up a real validator set.

**Later versions** ship the full protocol:

- v0.0.16+: VRF-based proposer selection
  (`aii_crypto::vrf` already lands the primitive)
- v0.0.17+: PRE-VOTE / PRE-COMMIT gossip via `aii-net-p2p`
- v0.0.18+: BLS aggregate signature over PRE-COMMITs
- v0.0.19+: ⅔ stake threshold → single-block instant finality

The `Engine` / `Proposer` / `Voter` traits from `aii-consensus-iface`
are already wired so multi-validator implementations slot in without
breaking the embedder API.

## Usage

```rust
use aii_consensus_bft::{DevModeEngine, EngineConfig};

let mut engine = DevModeEngine::new(EngineConfig::default(), genesis_block);
engine.step()?;  // produces block N + 1
```
