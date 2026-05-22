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

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Wrapper {
        #[serde(with = "super::bytes_hex")]
        bytes: Vec<u8>,
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
}
