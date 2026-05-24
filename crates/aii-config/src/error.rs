//! Error type for `aii-config`.

use thiserror::Error;

/// Errors produced while parsing a chain spec or constructing a genesis block.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// JSON parse failure.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    /// Hex decoding failure.
    #[error("hex: {0:?}")]
    Hex(hex::FromHexError),

    /// I/O failure reading a spec file.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// Validation failure (bad chain-id, bad pre-allocation, etc.).
    #[error("invalid spec: {0}")]
    Invalid(&'static str),
}
