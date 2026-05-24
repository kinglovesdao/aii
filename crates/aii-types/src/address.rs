//! Address — 20-byte EVM-compatible account address.

use crate::H256;
use alloy_rlp::{Decodable, Encodable};
use serde::{Deserialize, Serialize};

/// 20-byte address. Lowercase hex serialization with `0x` prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(transparent)]
pub struct Address(pub [u8; 20]);

impl Address {
    /// All-zero address.
    pub const ZERO: Self = Self([0u8; 20]);

    /// Construct from raw 20-byte array.
    #[must_use] 
    pub const fn new(bytes: [u8; 20]) -> Self {
        Self(bytes)
    }

    /// Underlying byte view.
    #[must_use] 
    pub const fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }

    /// Derive an EOA address from a 32-byte secp256k1 public-key hash.
    ///
    /// EVM convention: last 20 bytes of `Keccak256(uncompressed_pubkey[1..])`
    /// — we trust the caller to have already hashed.
    #[must_use] 
    pub fn from_pubkey_hash(hash: H256) -> Self {
        let mut out = [0u8; 20];
        out.copy_from_slice(&hash.as_bytes()[12..]);
        Self(out)
    }
}

impl From<[u8; 20]> for Address {
    fn from(b: [u8; 20]) -> Self {
        Self(b)
    }
}

impl Encodable for Address {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        self.0.as_slice().encode(out);
    }
    fn length(&self) -> usize {
        self.0.as_slice().length()
    }
}

impl Decodable for Address {
    fn decode(buf: &mut &[u8]) -> Result<Self, alloy_rlp::Error> {
        let bytes = <alloy_rlp::bytes::Bytes as Decodable>::decode(buf)?;
        if bytes.len() != 20 {
            return Err(alloy_rlp::Error::UnexpectedLength);
        }
        let mut out = [0u8; 20];
        out.copy_from_slice(&bytes);
        Ok(Self(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_zero_is_all_zero() {
        assert_eq!(Address::ZERO.0, [0u8; 20]);
    }

    #[test]
    fn from_pubkey_hash_takes_last_20_bytes() {
        let mut hash_bytes = [0u8; 32];
        for (i, b) in hash_bytes.iter_mut().enumerate().skip(12) {
            *b = (i - 11) as u8;
        }
        let addr = Address::from_pubkey_hash(H256::new(hash_bytes));
        let expected: [u8; 20] = std::array::from_fn(|i| (i + 1) as u8);
        assert_eq!(addr.0, expected);
    }

    #[test]
    fn address_equality_is_bytewise() {
        assert_eq!(Address::new([0xAB; 20]), Address::new([0xAB; 20]));
        assert_ne!(Address::ZERO, Address::new([1u8; 20]));
    }
}
