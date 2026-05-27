//! # AII Crypto
//!
//! Cryptographic primitives used across the AII protocol stack.
//!
//! Scope (per `docs/superpowers/specs/2026-05-21-aii-core-design.md` §3.1):
//!
//! | Module     | Purpose                                                         | Status |
//! |------------|-----------------------------------------------------------------|--------|
//! | [`keccak`] | Keccak-256 hashing — message digests, MPT leaves, address hash. | ✅     |
//! | [`secp`]   | secp256k1 ECDSA sign / verify / public-key recovery (ETH-style).| ✅     |
//! | [`bls`]    | BLS12-381 single + aggregate sign / verify (V-node consensus).  | ✅     |
//! | [`vrf`]    | Schnorrkel VRF for V-node leader election.                      | ✅     |
//! | [`error`]  | [`CryptoError`] umbrella over the per-module errors.            | ✅     |
//!
//! ## Public API
//!
//! Re-exports the most-used entry points: [`keccak256`], [`CryptoError`].
//!
//! ## Internal
//!
//! Anything marked `pub(crate)` is for cross-module wiring; downstream crates
//! must reach for the per-module API instead. Backwards-incompatible churn in
//! `pub(crate)` items is permitted under the workspace 0.0.x stability policy.

#![cfg_attr(not(test), forbid(unsafe_code))]
#![warn(missing_docs)]

pub mod bls;
pub mod ed25519;
pub mod error;
pub mod keccak;
pub mod secp;
pub mod vrf;

pub use error::CryptoError;
pub use keccak::keccak256;
