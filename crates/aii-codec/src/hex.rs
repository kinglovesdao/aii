//! ETH-compatible hex helpers.
//!
//! - `encode_bytes` / `decode_bytes`: arbitrary byte arrays — `0x`-prefixed
//!   lowercase hex, **no** leading-zero trimming (length is preserved).
//! - `encode_quantity` / `decode_quantity`: integer quantities — `0x`-prefixed
//!   lowercase hex with leading zeros stripped, except the value zero which
//!   serializes as `"0x0"` (the EVM JSON-RPC convention).

/// Encode an arbitrary byte slice as `0x`-prefixed lowercase hex.
/// Length is preserved (e.g. 32 input bytes → 66-character output including
/// the `0x` prefix).
#[must_use]
pub fn encode_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(2 + bytes.len() * 2);
    out.push_str("0x");
    out.push_str(&hex::encode(bytes));
    out
}

use thiserror::Error;

/// Error decoding an ETH-style hex string.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HexError {
    /// String did not start with the required `0x` prefix.
    #[error("missing `0x` prefix")]
    MissingPrefix,
    /// String contained an odd number of hex digits after the prefix.
    #[error("odd-length hex (got {0} chars after prefix)")]
    OddLength(usize),
    /// String contained a non-hex character.
    #[error("invalid hex character {ch:?} at position {pos}")]
    InvalidChar {
        /// Offending character.
        ch: char,
        /// 0-based position in the input string.
        pos: usize,
    },
}

/// Decode a `0x`-prefixed hex string into raw bytes. Mirrors `encode_bytes`.
pub fn decode_bytes(s: &str) -> Result<Vec<u8>, HexError> {
    let body = s.strip_prefix("0x").ok_or(HexError::MissingPrefix)?;
    if body.len() % 2 != 0 {
        return Err(HexError::OddLength(body.len()));
    }
    hex::decode(body).map_err(|e| match e {
        hex::FromHexError::InvalidHexCharacter { c, index } => HexError::InvalidChar {
            ch: c,
            pos: index + 2,
        },
        hex::FromHexError::OddLength => HexError::OddLength(body.len()),
        hex::FromHexError::InvalidStringLength => HexError::OddLength(body.len()),
    })
}

use aii_types::U256;

/// Encode a `U256` quantity in ETH JSON-RPC format:
///
/// - The value zero serializes as `"0x0"` (exactly one digit).
/// - Any non-zero value serializes as `0x` + minimal lowercase hex with **no**
///   leading zeros (e.g. `U256::from(255)` → `"0xff"`).
#[must_use]
pub fn encode_quantity(n: U256) -> String {
    if n.is_zero() {
        return String::from("0x0");
    }
    let raw = format!("{n:x}");
    let mut out = String::with_capacity(2 + raw.len());
    out.push_str("0x");
    out.push_str(&raw);
    out
}

/// Decode a `0x`-prefixed minimal-hex quantity. Accepts `"0x0"`; rejects any
/// other leading-zero form per the ETH JSON-RPC quantity convention.
pub fn decode_quantity(s: &str) -> Result<U256, HexError> {
    let body = s.strip_prefix("0x").ok_or(HexError::MissingPrefix)?;
    if body.is_empty() {
        return Err(HexError::OddLength(0));
    }
    if body.len() > 1 && body.starts_with('0') {
        return Err(HexError::InvalidChar { ch: '0', pos: 2 });
    }
    U256::from_str_radix(body, 16).map_err(|_| {
        let ch = body.chars().next().unwrap_or('?');
        HexError::InvalidChar { ch, pos: 2 }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_bytes_encodes_to_0x() {
        assert_eq!(encode_bytes(&[]), "0x");
    }

    #[test]
    fn single_byte_encodes_two_hex_chars() {
        assert_eq!(encode_bytes(&[0x00]), "0x00");
        assert_eq!(encode_bytes(&[0xAB]), "0xab");
    }

    #[test]
    fn output_is_lowercase_even_for_high_bytes() {
        assert_eq!(encode_bytes(&[0xDE, 0xAD, 0xBE, 0xEF]), "0xdeadbeef");
    }

    #[test]
    fn leading_zeros_are_preserved_in_byte_mode() {
        assert_eq!(encode_bytes(&[0x00, 0x00, 0x01]), "0x000001");
    }

    #[test]
    fn decode_bytes_round_trip() {
        let original = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let encoded = encode_bytes(&original);
        assert_eq!(decode_bytes(&encoded).unwrap(), original);
    }

    #[test]
    fn decode_empty_0x_yields_empty_vec() {
        assert_eq!(decode_bytes("0x").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn decode_rejects_missing_prefix() {
        assert_eq!(decode_bytes("deadbeef"), Err(HexError::MissingPrefix));
    }

    #[test]
    fn decode_rejects_odd_length() {
        assert_eq!(decode_bytes("0xabc"), Err(HexError::OddLength(3)));
    }

    #[test]
    fn decode_rejects_invalid_chars() {
        let err = decode_bytes("0xzz").unwrap_err();
        assert_eq!(err, HexError::InvalidChar { ch: 'z', pos: 2 });
    }

    #[test]
    fn quantity_zero_encodes_as_0x0() {
        assert_eq!(encode_quantity(U256::ZERO), "0x0");
    }

    #[test]
    fn quantity_strips_leading_zeros() {
        assert_eq!(encode_quantity(U256::from(0x0FFu64)), "0xff");
        assert_eq!(encode_quantity(U256::from(1u64)), "0x1");
        assert_eq!(encode_quantity(U256::from(0x1234u64)), "0x1234");
    }

    #[test]
    fn quantity_round_trip_for_typical_values() {
        for n in [0u64, 1, 0xFF, 0x1234, u64::MAX] {
            let v = U256::from(n);
            assert_eq!(decode_quantity(&encode_quantity(v)).unwrap(), v);
        }
    }

    #[test]
    fn quantity_rejects_padded_leading_zero() {
        assert!(matches!(
            decode_quantity("0x01"),
            Err(HexError::InvalidChar { .. })
        ));
        assert!(matches!(
            decode_quantity("0x00"),
            Err(HexError::InvalidChar { .. })
        ));
    }

    #[test]
    fn quantity_rejects_empty_body() {
        assert_eq!(decode_quantity("0x"), Err(HexError::OddLength(0)));
    }
}
