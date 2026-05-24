//! World-state account record (nonce / balance / code_hash / storage_root).

use crate::EMPTY_TRIE_HASH;
use aii_block::Hashable;
use aii_crypto::keccak::keccak256;
use aii_types::{H256, U256};
use alloy_rlp::{Decodable, Encodable};

/// Keccak-256 of the empty byte string — used as the default `code_hash` for
/// externally-owned accounts (EOAs).
pub const EMPTY_CODE_HASH: H256 = H256::new([
    0xc5, 0xd2, 0x46, 0x01, 0x86, 0xf7, 0x23, 0x3c, 0x92, 0x7e, 0x7d, 0xb2, 0xdc, 0xc7, 0x03, 0xc0,
    0xe5, 0x00, 0xb6, 0x53, 0xca, 0x82, 0x27, 0x3b, 0x7b, 0xfa, 0xd8, 0x04, 0x5d, 0x85, 0xa4, 0x70,
]);

/// An account in the world-state trie.
///
/// Field order on the wire (RLP) is `[nonce, balance, storage_root, code_hash]`
/// — matches Ethereum mainnet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// Outgoing-transaction count for EOAs; contract-creation counter for
    /// contract accounts.
    pub nonce: u64,
    /// Account balance, in Wei.
    pub balance: U256,
    /// Root of this account's storage trie; `EMPTY_TRIE_HASH` for EOAs.
    pub storage_root: H256,
    /// Keccak-256 of the account's bytecode; `EMPTY_CODE_HASH` for EOAs.
    pub code_hash: H256,
}

impl Account {
    /// EOA with `nonce = 0` and `balance = 0`.
    pub const EMPTY: Self = Self {
        nonce: 0,
        balance: U256::ZERO,
        storage_root: EMPTY_TRIE_HASH,
        code_hash: EMPTY_CODE_HASH,
    };

    /// Returns `true` iff this account has no bytecode (i.e. is an EOA, not
    /// a contract).
    pub fn is_eoa(&self) -> bool {
        self.code_hash == EMPTY_CODE_HASH
    }
}

fn u256_length(v: &U256) -> usize {
    let bytes: [u8; 32] = v.to_be_bytes();
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
    bytes[start..].length()
}

fn encode_u256(v: &U256, out: &mut dyn alloy_rlp::BufMut) {
    let bytes: [u8; 32] = v.to_be_bytes();
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
    bytes[start..].encode(out);
}

fn decode_u256(buf: &mut &[u8]) -> Result<U256, alloy_rlp::Error> {
    let v = <alloy_rlp::bytes::Bytes as Decodable>::decode(buf)?;
    if v.len() > 32 {
        return Err(alloy_rlp::Error::UnexpectedLength);
    }
    let mut padded = [0u8; 32];
    padded[32 - v.len()..].copy_from_slice(&v);
    Ok(U256::from_be_bytes(padded))
}

impl Encodable for Account {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        let payload_length = self.nonce.length()
            + u256_length(&self.balance)
            + self.storage_root.length()
            + self.code_hash.length();
        alloy_rlp::Header {
            list: true,
            payload_length,
        }
        .encode(out);
        self.nonce.encode(out);
        encode_u256(&self.balance, out);
        self.storage_root.encode(out);
        self.code_hash.encode(out);
    }
    fn length(&self) -> usize {
        let payload = self.nonce.length()
            + u256_length(&self.balance)
            + self.storage_root.length()
            + self.code_hash.length();
        alloy_rlp::length_of_length(payload) + payload
    }
}

impl Decodable for Account {
    fn decode(buf: &mut &[u8]) -> Result<Self, alloy_rlp::Error> {
        let h = alloy_rlp::Header::decode(buf)?;
        if !h.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }
        let nonce = u64::decode(buf)?;
        let balance = decode_u256(buf)?;
        let storage_root = H256::decode(buf)?;
        let code_hash = H256::decode(buf)?;
        Ok(Self {
            nonce,
            balance,
            storage_root,
            code_hash,
        })
    }
}

impl Hashable for Account {
    fn hash(&self) -> H256 {
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        self.encode(&mut buf);
        keccak256(&buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_code_hash_is_keccak_of_empty() {
        assert_eq!(EMPTY_CODE_HASH, keccak256(b""));
    }

    #[test]
    fn empty_account_is_eoa() {
        let a = Account::EMPTY;
        assert!(a.is_eoa());
        assert_eq!(a.nonce, 0);
        assert_eq!(a.balance, U256::ZERO);
    }

    #[test]
    fn rlp_round_trip_empty_account() {
        let original = Account::EMPTY;
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        original.encode(&mut buf);
        let mut s: &[u8] = &buf;
        let decoded = Account::decode(&mut s).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn rlp_round_trip_populated_account() {
        let original = Account {
            nonce: 42,
            balance: U256::from(1_000_000_000_000_000_000u64),
            storage_root: H256::new([0x11; 32]),
            code_hash: H256::new([0x22; 32]),
        };
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        original.encode(&mut buf);
        let mut s: &[u8] = &buf;
        let decoded = Account::decode(&mut s).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn contract_account_is_not_eoa() {
        let a = Account {
            code_hash: H256::new([0xaa; 32]),
            ..Account::EMPTY
        };
        assert!(!a.is_eoa());
    }

    #[test]
    fn hash_changes_with_nonce() {
        let a1 = Account::EMPTY;
        let a2 = Account {
            nonce: 1,
            ..Account::EMPTY
        };
        assert_ne!(a1.hash(), a2.hash());
    }
}
