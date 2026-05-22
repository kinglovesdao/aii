//! Serde adapters for ETH JSON-RPC hex conventions.
//!
//! Three reusable modules:
//!
//! - [`bytes_hex`]: byte sequences as `0x`-prefixed lowercase hex (length
//!   preserved). Apply via `#[serde(with = "aii_codec::json::bytes_hex")]`.
//! - [`quantity`]: `U256` quantities as `0x`-prefixed minimal hex. Apply via
//!   `#[serde(with = "aii_codec::json::quantity")]`.
//! - [`hex_h256`] / [`hex_address`]: convenience wrappers for the two most
//!   common AII byte newtypes.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Serde helper: `&[u8]` ↔ `"0x<lowercase hex>"`. Length is preserved.
pub mod bytes_hex {
    use super::{Deserialize, Deserializer, Serialize, Serializer};
    use crate::hex::{decode_bytes, encode_bytes};

    /// Serialize bytes as `0x`-prefixed lowercase hex.
    pub fn serialize<S, B>(bytes: B, ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        B: AsRef<[u8]>,
    {
        encode_bytes(bytes.as_ref()).serialize(ser)
    }

    /// Deserialize `0x`-prefixed hex into a `Vec<u8>`.
    pub fn deserialize<'de, D>(de: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(de)?;
        decode_bytes(&s).map_err(serde::de::Error::custom)
    }
}

/// Serde helper: `U256` ↔ `"0x<minimal hex>"`. Zero is `"0x0"`.
pub mod quantity {
    use super::{Deserialize, Deserializer, Serialize, Serializer};
    use crate::hex::{decode_quantity, encode_quantity};
    use aii_types::U256;

    /// Serialize a `U256` quantity.
    pub fn serialize<S>(n: &U256, ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        encode_quantity(*n).serialize(ser)
    }

    /// Deserialize a `U256` quantity.
    pub fn deserialize<'de, D>(de: D) -> Result<U256, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(de)?;
        decode_quantity(&s).map_err(serde::de::Error::custom)
    }
}

/// Serde helper: [`aii_types::H256`] ↔ `"0x<64 lowercase hex chars>"`.
pub mod hex_h256 {
    use super::{Deserialize, Deserializer, Serialize, Serializer};
    use crate::hex::{decode_bytes, encode_bytes};
    use aii_types::H256;

    /// Serialize an `H256`.
    pub fn serialize<S>(h: &H256, ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        encode_bytes(h.as_bytes()).serialize(ser)
    }

    /// Deserialize an `H256` (must be exactly 32 bytes).
    pub fn deserialize<'de, D>(de: D) -> Result<H256, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(de)?;
        let bytes = decode_bytes(&s).map_err(serde::de::Error::custom)?;
        if bytes.len() != 32 {
            return Err(serde::de::Error::custom(format!(
                "expected 32-byte H256, got {} bytes",
                bytes.len()
            )));
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        Ok(H256::new(out))
    }
}

/// Serde helper: [`aii_types::Address`] ↔ `"0x<40 lowercase hex chars>"`.
pub mod hex_address {
    use super::{Deserialize, Deserializer, Serialize, Serializer};
    use crate::hex::{decode_bytes, encode_bytes};
    use aii_types::Address;

    /// Serialize an `Address`.
    pub fn serialize<S>(a: &Address, ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        encode_bytes(a.as_bytes()).serialize(ser)
    }

    /// Deserialize an `Address` (must be exactly 20 bytes).
    pub fn deserialize<'de, D>(de: D) -> Result<Address, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(de)?;
        let bytes = decode_bytes(&s).map_err(serde::de::Error::custom)?;
        if bytes.len() != 20 {
            return Err(serde::de::Error::custom(format!(
                "expected 20-byte Address, got {} bytes",
                bytes.len()
            )));
        }
        let mut out = [0u8; 20];
        out.copy_from_slice(&bytes);
        Ok(Address::new(out))
    }
}

#[cfg(test)]
mod tests {
    use aii_types::{Address, H256, U256};
    use serde::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Wrapper {
        #[serde(with = "super::bytes_hex")]
        bytes: Vec<u8>,
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct QtyWrapper {
        #[serde(with = "super::quantity")]
        n: U256,
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct HashWrapper {
        #[serde(with = "super::hex_h256")]
        h: H256,
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct AddrWrapper {
        #[serde(with = "super::hex_address")]
        a: Address,
    }

    #[test]
    fn bytes_hex_serializes_with_0x_prefix() {
        let w = Wrapper {
            bytes: vec![0xDE, 0xAD, 0xBE, 0xEF],
        };
        let json = serde_json::to_string(&w).unwrap();
        assert_eq!(json, r#"{"bytes":"0xdeadbeef"}"#);
    }

    #[test]
    fn bytes_hex_round_trips() {
        let w = Wrapper {
            bytes: vec![0x00, 0x01, 0x02, 0xFF],
        };
        let json = serde_json::to_string(&w).unwrap();
        let back: Wrapper = serde_json::from_str(&json).unwrap();
        assert_eq!(back, w);
    }

    #[test]
    fn bytes_hex_empty_serializes_to_0x() {
        let w = Wrapper { bytes: Vec::new() };
        assert_eq!(serde_json::to_string(&w).unwrap(), r#"{"bytes":"0x"}"#);
    }

    #[test]
    fn bytes_hex_decode_rejects_missing_prefix() {
        let json = r#"{"bytes":"deadbeef"}"#;
        let err = serde_json::from_str::<Wrapper>(json).unwrap_err();
        assert!(err.to_string().contains("missing `0x` prefix"));
    }

    #[test]
    fn quantity_zero_is_0x0() {
        let w = QtyWrapper { n: U256::ZERO };
        assert_eq!(serde_json::to_string(&w).unwrap(), r#"{"n":"0x0"}"#);
    }

    #[test]
    fn quantity_non_zero_strips_leading_zeros() {
        let w = QtyWrapper {
            n: U256::from(0x00FFu64),
        };
        assert_eq!(serde_json::to_string(&w).unwrap(), r#"{"n":"0xff"}"#);
    }

    #[test]
    fn quantity_round_trips_for_large_value() {
        let w = QtyWrapper {
            n: U256::from(u64::MAX),
        };
        let json = serde_json::to_string(&w).unwrap();
        let back: QtyWrapper = serde_json::from_str(&json).unwrap();
        assert_eq!(back, w);
    }

    #[test]
    fn quantity_rejects_padded_form() {
        let json = r#"{"n":"0x01"}"#;
        let err = serde_json::from_str::<QtyWrapper>(json).unwrap_err();
        assert!(err.to_string().contains("invalid hex"));
    }

    #[test]
    fn h256_serializes_to_64_hex_chars() {
        let w = HashWrapper { h: H256::ZERO };
        let json = serde_json::to_string(&w).unwrap();
        assert!(json.contains(&"0".repeat(64)));
        assert!(json.contains("0x"));
    }

    #[test]
    fn h256_round_trips() {
        let w = HashWrapper {
            h: H256::new([0x42; 32]),
        };
        let json = serde_json::to_string(&w).unwrap();
        let back: HashWrapper = serde_json::from_str(&json).unwrap();
        assert_eq!(back, w);
    }

    #[test]
    fn h256_rejects_wrong_length() {
        let json = r#"{"h":"0x0102030405060708090a0b0c0d0e0f"}"#;
        let err = serde_json::from_str::<HashWrapper>(json).unwrap_err();
        assert!(err.to_string().contains("expected 32-byte H256"));
    }

    #[test]
    fn address_round_trips() {
        let w = AddrWrapper {
            a: Address::new([0xAB; 20]),
        };
        let json = serde_json::to_string(&w).unwrap();
        let back: AddrWrapper = serde_json::from_str(&json).unwrap();
        assert_eq!(back, w);
    }

    #[test]
    fn address_rejects_wrong_length() {
        let json = r#"{"a":"0xab"}"#;
        let err = serde_json::from_str::<AddrWrapper>(json).unwrap_err();
        assert!(err.to_string().contains("expected 20-byte Address"));
    }
}
