//! # aii-config
//!
//! Chain spec, genesis, and runtime parameter loader for AII.
//!
//! ## Public API
//! - [`ChainSpec`] — immutable chain identifier + protocol constants
//! - [`Genesis`] — initial allocation + parameters + `to_header()` helper
//! - [`ConfigError`] umbrella

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod chain_spec;
pub mod error;
pub mod genesis;

pub use chain_spec::{ChainSpec, AII_CHAIN_ID, AII_MAINNET, AII_TESTNET};
pub use error::ConfigError;
pub use genesis::{Genesis, GenesisAlloc, GenesisValidator};
