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
}
