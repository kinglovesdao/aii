//! # aii-erc20
//!
//! Ethereum-compatible ERC-20 ABI helpers. ABI / selectors only — this
//! crate does *not* embed a reference contract bytecode. Pair it with
//! any solc-compiled token (e.g. OpenZeppelin's `ERC20Mock`) and use
//! these helpers to encode calldata for, and decode results from,
//! `eth_sendRawTransaction` / `eth_call`.
//!
//! ## Public API
//! - `SELECTOR_*` — 4-byte function selectors as compile-time consts.
//! - `encode_balance_of / encode_transfer / encode_approve /
//!   encode_allowance / encode_transfer_from / encode_total_supply` —
//!   build the canonical ABI calldata.
//! - `decode_uint256_result` — turn the 32-byte return data into a
//!   `U256`.
//! - `decode_bool_result` — turn the 32-byte return data into a bool
//!   (Solidity `bool` is right-aligned `0` / `1` in a 32-byte word).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use aii_types::{Address, U256};

/// `keccak256("totalSupply()")[..4]`.
pub const SELECTOR_TOTAL_SUPPLY: [u8; 4] = [0x18, 0x16, 0x0d, 0xdd];
/// `keccak256("balanceOf(address)")[..4]`.
pub const SELECTOR_BALANCE_OF: [u8; 4] = [0x70, 0xa0, 0x82, 0x31];
/// `keccak256("transfer(address,uint256)")[..4]`.
pub const SELECTOR_TRANSFER: [u8; 4] = [0xa9, 0x05, 0x9c, 0xbb];
/// `keccak256("approve(address,uint256)")[..4]`.
pub const SELECTOR_APPROVE: [u8; 4] = [0x09, 0x5e, 0xa7, 0xb3];
/// `keccak256("allowance(address,address)")[..4]`.
pub const SELECTOR_ALLOWANCE: [u8; 4] = [0xdd, 0x62, 0xed, 0x3e];
/// `keccak256("transferFrom(address,address,uint256)")[..4]`.
pub const SELECTOR_TRANSFER_FROM: [u8; 4] = [0x23, 0xb8, 0x72, 0xdd];

/// Pad a 20-byte address to a 32-byte ABI word (zero-prefix).
fn abi_pad_addr(addr: &Address) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[12..].copy_from_slice(addr.as_bytes());
    out
}

/// Encode a U256 as a 32-byte big-endian ABI word.
const fn abi_pad_u256(v: U256) -> [u8; 32] {
    v.to_be_bytes::<32>()
}

/// `balanceOf(address)` — 4-byte selector + 32-byte address word.
#[must_use]
pub fn encode_balance_of(addr: &Address) -> Vec<u8> {
    let mut data = Vec::with_capacity(4 + 32);
    data.extend_from_slice(&SELECTOR_BALANCE_OF);
    data.extend_from_slice(&abi_pad_addr(addr));
    data
}

/// `totalSupply()` — selector only (no arguments).
#[must_use]
pub fn encode_total_supply() -> Vec<u8> {
    SELECTOR_TOTAL_SUPPLY.to_vec()
}

/// `transfer(address,uint256)` — selector + recipient + amount.
#[must_use]
pub fn encode_transfer(to: &Address, amount: U256) -> Vec<u8> {
    let mut data = Vec::with_capacity(4 + 32 + 32);
    data.extend_from_slice(&SELECTOR_TRANSFER);
    data.extend_from_slice(&abi_pad_addr(to));
    data.extend_from_slice(&abi_pad_u256(amount));
    data
}

/// `approve(address,uint256)` — selector + spender + amount.
#[must_use]
pub fn encode_approve(spender: &Address, amount: U256) -> Vec<u8> {
    let mut data = Vec::with_capacity(4 + 32 + 32);
    data.extend_from_slice(&SELECTOR_APPROVE);
    data.extend_from_slice(&abi_pad_addr(spender));
    data.extend_from_slice(&abi_pad_u256(amount));
    data
}

/// `allowance(address,address)` — selector + owner + spender.
#[must_use]
pub fn encode_allowance(owner: &Address, spender: &Address) -> Vec<u8> {
    let mut data = Vec::with_capacity(4 + 32 + 32);
    data.extend_from_slice(&SELECTOR_ALLOWANCE);
    data.extend_from_slice(&abi_pad_addr(owner));
    data.extend_from_slice(&abi_pad_addr(spender));
    data
}

/// `transferFrom(address,address,uint256)` — selector + from + to + amount.
#[must_use]
pub fn encode_transfer_from(from: &Address, to: &Address, amount: U256) -> Vec<u8> {
    let mut data = Vec::with_capacity(4 + 32 + 32 + 32);
    data.extend_from_slice(&SELECTOR_TRANSFER_FROM);
    data.extend_from_slice(&abi_pad_addr(from));
    data.extend_from_slice(&abi_pad_addr(to));
    data.extend_from_slice(&abi_pad_u256(amount));
    data
}

/// Decode a 32-byte ABI uint256 return value. Shorter / longer payloads
/// return `U256::ZERO` (matches Solidity's "uninitialised → 0").
#[must_use]
pub fn decode_uint256_result(bytes: &[u8]) -> U256 {
    if bytes.len() < 32 {
        return U256::ZERO;
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes[..32]);
    U256::from_be_bytes(arr)
}

/// Decode a 32-byte ABI bool return value. Solidity returns `1` for
/// true and `0` for false, left-padded into a 32-byte word.
#[must_use]
pub fn decode_bool_result(bytes: &[u8]) -> bool {
    !decode_uint256_result(bytes).is_zero()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aii_crypto::keccak::keccak256;

    fn selector_of(sig: &str) -> [u8; 4] {
        let h = keccak256(sig.as_bytes());
        let mut out = [0u8; 4];
        out.copy_from_slice(&h.as_bytes()[..4]);
        out
    }

    #[test]
    fn selectors_match_keccak_signatures() {
        assert_eq!(SELECTOR_TOTAL_SUPPLY, selector_of("totalSupply()"));
        assert_eq!(SELECTOR_BALANCE_OF, selector_of("balanceOf(address)"));
        assert_eq!(SELECTOR_TRANSFER, selector_of("transfer(address,uint256)"));
        assert_eq!(SELECTOR_APPROVE, selector_of("approve(address,uint256)"));
        assert_eq!(
            SELECTOR_ALLOWANCE,
            selector_of("allowance(address,address)")
        );
        assert_eq!(
            SELECTOR_TRANSFER_FROM,
            selector_of("transferFrom(address,address,uint256)")
        );
    }

    #[test]
    fn encode_balance_of_layout() {
        let alice = Address::new([0xa1; 20]);
        let d = encode_balance_of(&alice);
        assert_eq!(d.len(), 36);
        assert_eq!(d[..4], SELECTOR_BALANCE_OF);
        // Left-padded address: 12 zero bytes + 20 address bytes.
        assert!(d[4..16].iter().all(|b| *b == 0));
        assert_eq!(d[16..36], alice.as_bytes()[..]);
    }

    #[test]
    fn encode_transfer_layout() {
        let bob = Address::new([0xb2; 20]);
        let amount = U256::from(1_000_000u64);
        let d = encode_transfer(&bob, amount);
        assert_eq!(d.len(), 68);
        assert_eq!(d[..4], SELECTOR_TRANSFER);
        assert_eq!(d[16..36], bob.as_bytes()[..]);
        // amount big-endian in the last 32 bytes.
        let mut expected_amt = [0u8; 32];
        expected_amt[24..].copy_from_slice(&1_000_000u64.to_be_bytes());
        assert_eq!(d[36..68], expected_amt);
    }

    #[test]
    fn encode_transfer_from_layout() {
        let from = Address::new([0xf1; 20]);
        let to = Address::new([0x72; 20]);
        let amount = U256::from(42u64);
        let d = encode_transfer_from(&from, &to, amount);
        assert_eq!(d.len(), 4 + 96);
        assert_eq!(d[..4], SELECTOR_TRANSFER_FROM);
        assert_eq!(d[16..36], from.as_bytes()[..]);
        assert_eq!(d[48..68], to.as_bytes()[..]);
        let mut expected_amt = [0u8; 32];
        expected_amt[31] = 42;
        assert_eq!(d[68..100], expected_amt);
    }

    #[test]
    fn decode_uint256_round_trip() {
        let v = U256::from(0xdead_beefu64);
        let bytes = abi_pad_u256(v);
        assert_eq!(decode_uint256_result(&bytes), v);
    }

    #[test]
    fn decode_uint256_too_short_returns_zero() {
        assert_eq!(decode_uint256_result(&[0xff; 16]), U256::ZERO);
    }

    #[test]
    fn decode_bool_handles_solidity_padding() {
        // True = 0x00..01.
        let mut one = [0u8; 32];
        one[31] = 1;
        assert!(decode_bool_result(&one));
        // False = 0x00..00.
        let zero = [0u8; 32];
        assert!(!decode_bool_result(&zero));
    }
}
