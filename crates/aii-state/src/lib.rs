//! # aii-state
//!
//! World-state primitives for the AII protocol — `Account`, `StateDb`,
//! and an `mpt_root` placeholder (full MPT in v0.0.7).
//!
//! ## Public API
//! - [`Account`] (nonce / balance / code_hash / storage_root) + Ethereum-
//!   compatible RLP and Keccak hash
//! - [`StateDb`] — KV-backed `Address → Account` store via
//!   [`aii_storage::KvBackend`]
//! - [`EMPTY_CODE_HASH`] (= `keccak256(b"")`) and the re-export
//!   [`EMPTY_TRIE_HASH`] from `aii-block`
//! - [`mpt_root`] — empty-input fast path only in v0.0.6; non-empty input
//!   panics (full impl in v0.0.7)
//! - [`StateError`] umbrella
//!
//! ## Internal
//! - All RLP delegation is `pub(crate)` and built on top of `aii_codec`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod account;
pub mod db;
pub mod error;
pub mod trie;

pub use account::{Account, EMPTY_CODE_HASH};
pub use aii_block::EMPTY_TRIE_HASH;
pub use db::StateDb;
pub use error::StateError;
pub use trie::mpt_root;
