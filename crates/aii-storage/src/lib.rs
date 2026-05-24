//! # AII Storage
//!
//! Key-value storage abstraction used across the AII protocol stack.
//!
//! See `docs/superpowers/specs/2026-05-24-aii-storage-design.md` for design.
//!
//! ## Module map
//!
//! | Module     | Purpose                                                        |
//! |------------|----------------------------------------------------------------|
//! | [`cf`]     | [`ColumnFamily`] closed enum + stable wire names.              |

#![cfg_attr(not(test), forbid(unsafe_code))]
#![warn(missing_docs)]

pub mod cf;

pub use cf::ColumnFamily;
