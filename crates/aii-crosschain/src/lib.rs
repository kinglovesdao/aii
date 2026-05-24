//! # aii-crosschain
//!
//! Cross-chain primitives for AII.
//!
//! ## Scope
//!
//! - [`htlc`] (v0.0.18) — Hash Time-Locked Contracts for trustless
//!   atomic swaps. Pure state machine; no asset movement.
//! - [`federation`] (v0.0.21) — BLS-aggregated threshold multisig
//!   `Vault` for federated bridges. On-chain verification of release
//!   attestations; no off-chain attester / aggregator daemon, no
//!   federation set rotation.
//!
//! IBC light clients, full Polkadot XCM adapters, and federation set
//! rotation are explicit non-goals for now. They will land in later
//! releases that build on these state machines.
//!
//! ## Hash function
//!
//! AII uses Keccak-256 throughout (per design doc §3.1). Cross-chain
//! peers MUST agree on the digest in their swap protocol.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod federation;
pub mod htlc;
