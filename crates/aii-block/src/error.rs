//! Umbrella error for `aii-block`.

use thiserror::Error;

/// Errors produced while encoding or decoding block-layer values.
#[derive(Debug, Error)]
pub enum BlockError {
    /// RLP encode/decode failure (delegated to `alloy_rlp`).
    #[error("rlp: {0}")]
    Rlp(#[from] alloy_rlp::Error),

    /// Encountered an EIP-2718 transaction-type byte that is not recognised.
    #[error("unknown tx type byte: 0x{0:02x}")]
    UnknownTxType(u8),

    /// Receipt envelope malformed (missing type byte / payload mismatch).
    #[error("invalid receipt envelope")]
    InvalidReceiptEnvelope,

    /// `Bloom` field length not 256 bytes.
    #[error("invalid bloom length: expected 256, got {0}")]
    InvalidBloomLength(usize),

    /// `Header::extra_data` longer than the 32-byte ETH ceiling.
    #[error("extra_data too long: {0} > 32")]
    ExtraDataTooLong(usize),
}
