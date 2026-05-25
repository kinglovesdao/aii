//! VRF public-key wire type.
//!
//! AII uses schnorrkel VRF over Ristretto255 (32-byte compressed
//! public keys). Concrete verification lives in `aii-crypto::vrf`.
//!
//! Serde representation is **lowercase hex with `0x` prefix** —
//! matches the [`crate::BlsPubKey`] convention for genesis JSON.

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

/// Compressed schnorrkel / Ristretto VRF public key (32 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct VrfPubKey(pub [u8; 32]);

impl VrfPubKey {
    /// All-zero placeholder.
    pub const ZERO: Self = Self([0u8; 32]);

    /// Construct from raw bytes.
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Underlying view.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl Serialize for VrfPubKey {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&format!("0x{}", hex::encode(self.0)))
    }
}

impl<'de> Deserialize<'de> for VrfPubKey {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = <&str>::deserialize(d)?;
        let s = s.strip_prefix("0x").unwrap_or(s);
        let raw = hex::decode(s).map_err(de::Error::custom)?;
        let arr: [u8; 32] = raw.try_into().map_err(|v: Vec<u8>| {
            de::Error::custom(format!("VrfPubKey: 32 bytes, got {}", v.len()))
        })?;
        Ok(Self(arr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vrf_pubkey_is_32_bytes_zero() {
        assert_eq!(VrfPubKey::ZERO.0.len(), 32);
        assert!(VrfPubKey::ZERO.0.iter().all(|b| *b == 0));
    }

    #[test]
    fn vrf_pubkey_new_round_trips() {
        let mut b = [0u8; 32];
        b[0] = 0xCC;
        b[31] = 0xDD;
        let k = VrfPubKey::new(b);
        assert_eq!(*k.as_bytes(), b);
    }

    #[test]
    fn vrf_pubkey_serde_round_trips_with_0x_prefix() {
        let mut b = [0u8; 32];
        b[0] = 0xAA;
        b[31] = 0xBB;
        let k = VrfPubKey::new(b);
        let json = serde_json::to_string(&k).unwrap();
        assert!(json.starts_with(r#""0xaa"#));
        assert!(json.ends_with(r#"bb""#));
        let back: VrfPubKey = serde_json::from_str(&json).unwrap();
        assert_eq!(back, k);
    }

    #[test]
    fn vrf_pubkey_deserialize_without_prefix() {
        let json = format!(r#""{}""#, "00".repeat(32));
        let k: VrfPubKey = serde_json::from_str(&json).unwrap();
        assert_eq!(k, VrfPubKey::ZERO);
    }

    #[test]
    fn vrf_pubkey_deserialize_rejects_wrong_length() {
        let json = r#""0xdeadbeef""#;
        assert!(serde_json::from_str::<VrfPubKey>(json).is_err());
    }
}
