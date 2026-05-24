//! # AII Storage
//!
//! Key-value storage abstraction used across the AII protocol stack.
//!
//! See `docs/superpowers/specs/2026-05-24-aii-storage-design.md` for design.
//!
//! ## Module map
//!
//! | Module      | Purpose                                                        |
//! |-------------|----------------------------------------------------------------|
//! | [`cf`]      | [`ColumnFamily`] closed enum + stable wire names.              |
//! | [`error`]   | [`StorageError`] umbrella over per-backend errors.             |
//! | [`batch`]   | [`WriteBatch`] backend-agnostic op log.                        |
//! | [`backend`] | [`KvBackend`] trait — the public abstraction.                  |
//! | [`snapshot`]| [`Snapshot`] trait — read-only consistent view.                |
//! | [`memory`]  | [`MemoryBackend`] — `BTreeMap` per CF, for tests.              |
//! | [`rocksdb`] | [`RocksDbBackend`] — production RocksDB backend (feature `rocksdb`). |

#![cfg_attr(not(any(test, feature = "rocksdb")), forbid(unsafe_code))]
#![warn(missing_docs)]

pub mod backend;
pub mod batch;
pub mod cf;
pub mod error;
pub mod memory;
#[cfg(feature = "rocksdb")]
pub mod rocksdb;
pub mod snapshot;

pub use backend::KvBackend;
pub use batch::{Op, WriteBatch};
pub use cf::ColumnFamily;
pub use error::StorageError;
pub use memory::{MemoryBackend, MemorySnapshot};
#[cfg(feature = "rocksdb")]
pub use rocksdb::{RocksDbBackend, RocksDbSnapshot};
pub use snapshot::{KvItem, KvIter, Snapshot};
