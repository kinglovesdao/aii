//! BLS12-381 public-key / signature wire types.
//!
//! AII uses BLS on G1 (compressed, 48 bytes) for public keys and on G2
//! (compressed, 96 bytes) for signatures, matching the Ethereum 2.0 spec
//! conventions. Concrete verification lives in `aii-crypto` (later plan).
//!
//! Serde representation is **lowercase hex with `0x` prefix** for both
//! types — matches the Ethereum / Beacon-Chain convention and keeps
//! genesis JSON human-readable.

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

/// Compressed BLS12-381 G1 public key (48 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct BlsPubKey(pub [u8; 48]);

impl BlsPubKey {
    /// All-zero placeholder.
    pub const ZERO: Self = Self([0u8; 48]);

    /// Construct from raw bytes.
    #[must_use]
    pub const fn new(bytes: [u8; 48]) -> Self {
        Self(bytes)
    }

    /// Underlying view.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 48] {
        &self.0
    }
}

/// Compressed BLS12-381 G2 signature (96 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct BlsSignature(pub [u8; 96]);

impl BlsSignature {
    /// All-zero placeholder.
    pub const ZERO: Self = Self([0u8; 96]);

    /// Construct from raw bytes.
    #[must_use]
    pub const fn new(bytes: [u8; 96]) -> Self {
        Self(bytes)
    }

    /// Underlying view.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 96] {
        &self.0
    }
}

// ───────────── serde: lowercase hex with `0x` prefix ─────────────

impl Serialize for BlsPubKey {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&format!("0x{}", hex::encode(self.0)))
    }
}

impl<'de> Deserialize<'de> for BlsPubKey {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = <&str>::deserialize(d)?;
        let s = s.strip_prefix("0x").unwrap_or(s);
        let raw = hex::decode(s).map_err(de::Error::custom)?;
        let arr: [u8; 48] = raw.try_into().map_err(|v: Vec<u8>| {
            de::Error::custom(format!("BlsPubKey: 48 bytes, got {}", v.len()))
        })?;
        Ok(Self(arr))
    }
}

impl Serialize for BlsSignature {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&format!("0x{}", hex::encode(self.0)))
    }
}

impl<'de> Deserialize<'de> for BlsSignature {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = <&str>::deserialize(d)?;
        let s = s.strip_prefix("0x").unwrap_or(s);
        let raw = hex::decode(s).map_err(de::Error::custom)?;
        let arr: [u8; 96] = raw.try_into().map_err(|v: Vec<u8>| {
            de::Error::custom(format!("BlsSignature: 96 bytes, got {}", v.len()))
        })?;
        Ok(Self(arr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bls_pubkey_is_48_bytes_zero() {
        assert_eq!(BlsPubKey::ZERO.0.len(), 48);
        assert!(BlsPubKey::ZERO.0.iter().all(|b| *b == 0));
    }

    #[test]
    fn bls_signature_is_96_bytes_zero() {
        assert_eq!(BlsSignature::ZERO.0.len(), 96);
        assert!(BlsSignature::ZERO.0.iter().all(|b| *b == 0));
    }

    #[test]
    fn bls_pubkey_new_round_trips() {
        let mut b = [0u8; 48];
        b[0] = 0xAA;
        b[47] = 0xBB;
        let k = BlsPubKey::new(b);
        assert_eq!(*k.as_bytes(), b);
    }

    #[test]
    fn bls_signature_new_round_trips() {
        let b = [0x55u8; 96];
        let s = BlsSignature::new(b);
        assert_eq!(*s.as_bytes(), b);
    }
}
