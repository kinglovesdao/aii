# aii-consensus-iface

Trait crate: `Engine`, `Proposer`, `Voter`, `ConsensusError`. All consensus
implementations (BFT-PoS for the main chain, plus per-subchain PoS/PBFT/DPoS
plugins) implement these traits.

This crate has **no transitive consensus code** — implementations live in
`aii-consensus-bft` and `aii-consensus-plugins`.
