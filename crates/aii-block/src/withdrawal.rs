//! EIP-4895 — beacon-chain withdrawal entry.

use aii_types::Address;
use alloy_rlp::{RlpDecodable, RlpEncodable};

/// A single beacon-chain withdrawal, included in the block body.
///
/// `amount` is in **Gwei** (per EIP-4895), not Wei.
#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct Withdrawal {
    /// Monotonically-increasing beacon-chain withdrawal index.
    pub index: u64,
    /// Beacon-chain validator index that this withdrawal pays.
    pub validator_index: u64,
    /// Execution-layer recipient.
    pub address: Address,
    /// Amount in Gwei (1 Gwei = 1e9 Wei).
    pub amount: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_rlp::{Decodable, Encodable};

    #[test]
    fn rlp_round_trip() {
        let w = Withdrawal {
            index: 42,
            validator_index: 1337,
            address: Address::new([0xaa; 20]),
            amount: 32_000_000_000,
        };
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        w.encode(&mut buf);
        let mut s: &[u8] = &buf;
        let decoded = Withdrawal::decode(&mut s).unwrap();
        assert_eq!(decoded, w);
    }
}
