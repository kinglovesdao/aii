//! # AII Types
//!
//! Primitive types for the AII protocol. Every downstream crate (state / EVM
//! / consensus / RPC / ...) depends on the types defined here.
//!
//! ## Re-exports
//!
//! - [`H256`] — 32-byte hash (Keccak-256 output)
//! - [`Address`] — 20-byte account address (EVM-compatible)
//! - [`U256`] — 256-bit unsigned integer (re-exported from `alloy-primitives`)
//!
//! ## AII-specific
//!
//! - [`AlgoId`] — signature-algorithm identifier (1 byte; reserves Day-0
//!   PQ slots per spec decision D7)
//! - [`BlsPubKey`] / [`BlsSignature`] — BLS12-381 G1/G2 keys & signatures
//! - [`SignedTx`] — generic signed transaction envelope dispatching on
//!   [`AlgoId`]

#![cfg_attr(not(test), forbid(unsafe_code))]
#![warn(missing_docs)]

mod address;
mod algo;
mod bls;
mod error;
mod hash;
mod integer;
mod signed_tx;
mod vrf;

pub use address::Address;
pub use algo::AlgoId;
pub use bls::{BlsPubKey, BlsSignature};
pub use error::TypesError;
pub use hash::H256;
pub use integer::U256;
pub use signed_tx::SignedTx;
pub use vrf::VrfPubKey;
