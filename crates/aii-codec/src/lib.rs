//! # AII Codec
//!
//! Encoding and decoding of AII protocol types in three wire formats:
//!
//! - **RLP** — Ethereum-compatible recursive-length prefix.
//! - **SSZ** — Simple Serialize (consensus-aggregated objects).
//! - **JSON** — Ethereum-compatible JSON-RPC hex conventions.
//!
//! ## Hex conventions (ETH-compatible)
//!
//! - Byte arrays: `0x` prefix + lowercase hex, even length, length preserved.
//! - Quantities (numbers): `0x` prefix + minimal hex; zero is `"0x0"`.
//!
//! ## Module map
//!
//! | Module    | Format       | Type covered                                  |
//! |-----------|--------------|-----------------------------------------------|
//! | [`hex`]   | shared       | byte / quantity hex                           |
//! | [`rlp`]   | RLP          | `H256`, `Address`, `AlgoId`, BLS, `SignedTx`  |
//! | [`ssz`]   | SSZ          | same set as RLP                               |
//! | [`json`]  | JSON helpers | `U256` quantity + byte hex serde              |
//! | [`error`] | unified      | [`CodecError`]                                |

#![cfg_attr(not(test), forbid(unsafe_code))]
#![warn(missing_docs)]

pub mod error;
pub mod hex;
pub mod json;
pub mod rlp;
pub mod ssz;

pub use error::CodecError;
