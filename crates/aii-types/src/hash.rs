//! 32-byte cryptographic hash (Keccak-256 output, secp256k1 message hash, MPT node, ...).

use alloy_rlp::{Decodable, Encodable};
use serde::{Deserialize, Serialize};

/// 32-byte hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(transparent)]
pub struct H256(pub [u8; 32]);

impl H256 {
    /// All-zero hash. Useful as default / sentinel value.
    pub const ZERO: Self = Self([0u8; 32]);

    /// Construct from raw 32-byte array.
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Return a reference to the underlying bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl From<[u8; 32]> for H256 {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl AsRef<[u8]> for H256 {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl Encodable for H256 {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        self.0.as_slice().encode(out);
    }
    fn length(&self) -> usize {
        self.0.as_slice().length()
    }
}

impl Decodable for H256 {
    fn decode(buf: &mut &[u8]) -> Result<Self, alloy_rlp::Error> {
        let bytes = <alloy_rlp::bytes::Bytes as Decodable>::decode(buf)?;
        if bytes.len() != 32 {
            return Err(alloy_rlp::Error::UnexpectedLength);
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        Ok(Self(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h256_zero_is_all_zero_bytes() {
        assert_eq!(H256::ZERO.0, [0u8; 32]);
    }

    #[test]
    fn h256_new_round_trips() {
        let mut b = [0u8; 32];
        b[0] = 0xAA;
        b[31] = 0xFF;
        let h = H256::new(b);
        assert_eq!(*h.as_bytes(), b);
    }

    #[test]
    fn h256_from_array_equals_new() {
        let b = [0x42u8; 32];
        assert_eq!(H256::from(b), H256::new(b));
    }

    #[test]
    fn h256_equality_is_bytewise() {
        assert_eq!(H256::ZERO, H256::new([0u8; 32]));
        assert_ne!(H256::ZERO, H256::new([1u8; 32]));
    }
}
