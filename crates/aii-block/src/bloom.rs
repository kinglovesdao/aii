//! Ethereum-style 2048-bit logs bloom filter.

use aii_crypto::keccak::keccak256;
use alloy_rlp::{Decodable, Encodable};

/// 2048-bit (256-byte) bloom filter, Ethereum-compatible.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Bloom(pub [u8; 256]);

impl Bloom {
    /// All-zero bloom (no entries).
    pub const ZERO: Self = Self([0u8; 256]);

    /// XOR the bloom bits of `data` into `self` (Ethereum Yellow Paper §4.4.2).
    pub fn accrue(&mut self, data: &[u8]) {
        let hash = keccak256(data);
        let h = hash.as_bytes();
        for i in [0usize, 2, 4] {
            let bit_index = ((u16::from(h[i]) << 8) | u16::from(h[i + 1])) & 0x07FF;
            #[allow(clippy::cast_possible_truncation)]
            let byte = 255 - (bit_index / 8) as usize;
            #[allow(clippy::cast_possible_truncation)]
            let mask = 1u8 << (bit_index % 8) as u8;
            self.0[byte] |= mask;
        }
    }

    /// Return true if every bit asserted by `data` is set in `self`.
    #[must_use]
    pub fn contains(&self, data: &[u8]) -> bool {
        let mut probe = Self::ZERO;
        probe.accrue(data);
        (0..256).all(|i| (self.0[i] & probe.0[i]) == probe.0[i])
    }
}

impl core::fmt::Debug for Bloom {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut s = String::with_capacity(16);
        for x in &self.0[..8] {
            s.push_str(&format!("{x:02x}"));
        }
        write!(f, "Bloom(0x{s}…)")
    }
}

impl Default for Bloom {
    fn default() -> Self {
        Self::ZERO
    }
}

impl Encodable for Bloom {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        self.0.as_slice().encode(out);
    }
    fn length(&self) -> usize {
        self.0.as_slice().length()
    }
}

impl Decodable for Bloom {
    fn decode(buf: &mut &[u8]) -> Result<Self, alloy_rlp::Error> {
        let v = <alloy_rlp::bytes::Bytes as Decodable>::decode(buf)?;
        if v.len() != 256 {
            return Err(alloy_rlp::Error::UnexpectedLength);
        }
        let mut out = [0u8; 256];
        out.copy_from_slice(&v);
        Ok(Self(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_is_all_zero() {
        assert_eq!(Bloom::ZERO.0, [0u8; 256]);
    }

    #[test]
    fn accrue_sets_some_bits() {
        let mut b = Bloom::ZERO;
        b.accrue(b"hello");
        let popcount: u32 = b.0.iter().map(|x| x.count_ones()).sum();
        assert!((1..=3).contains(&popcount));
    }

    #[test]
    fn accrued_bytes_are_contained() {
        let mut b = Bloom::ZERO;
        b.accrue(b"hello");
        assert!(b.contains(b"hello"));
    }

    #[test]
    fn rlp_round_trip_zero() {
        let original = Bloom::ZERO;
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        original.encode(&mut buf);
        let mut s: &[u8] = &buf;
        let decoded = Bloom::decode(&mut s).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn rlp_round_trip_with_data() {
        let mut original = Bloom::ZERO;
        original.accrue(b"hello");
        original.accrue(b"world");
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        original.encode(&mut buf);
        let mut s: &[u8] = &buf;
        let decoded = Bloom::decode(&mut s).unwrap();
        assert_eq!(decoded, original);
    }
}
