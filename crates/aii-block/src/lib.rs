//! # aii-block
//!
//! Block-layer data types for the AII protocol — `Header`, `Tx`, `Receipt`,
//! `Block`. All types encode byte-perfect with Ethereum mainnet via the
//! EIP-2718 envelope, with `AlgoId` extension fields that default to
//! Ethereum compatibility (see crate spec §3.2).
//!
//! ## Public API
//! - [`Header`], [`Tx`] (+ [`TxLegacy`], [`TxEip1559`], [`TxEip4844`]),
//!   [`Receipt`], [`Block`], [`BlockBody`]
//! - [`Log`], [`Bloom`], [`Withdrawal`], [`AccessListItem`]
//! - [`Hashable`] trait — `hash() -> H256`
//! - [`BlockError`] umbrella
//!
//! ## Internal
//! - RLP helpers are `pub(crate)` and built on top of `aii_codec::rlp`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod access;
pub mod bloom;
pub mod consts;
pub mod error;
pub mod log;
pub mod withdrawal;

pub use aii_types::{Address, AlgoId, H256, U256};
pub use access::AccessListItem;
pub use bloom::Bloom;
pub use consts::{EMPTY_LIST_HASH, EMPTY_TRIE_HASH};
pub use error::BlockError;
pub use log::Log;
pub use withdrawal::Withdrawal;

/// All AII block-layer values can produce a 32-byte Keccak-256 commitment.
pub trait Hashable {
    /// Return the canonical 32-byte hash of `self`.
    fn hash(&self) -> H256;
}
