//! Recover the sender (signer) of a transaction.
//!
//! Implements EIP-155 (chain-id-bound legacy) and EIP-2718 typed-tx
//! (EIP-1559 v=0|1) recovery against the existing
//! [`aii_crypto::secp`] primitives.
//!
//! ## Pre-EIP-155 legacy (v ∈ {27, 28})
//! ```text
//!     msg = keccak256(rlp([nonce, gas_price, gas_limit, to, value, data]))
//!     recid = v - 27
//! ```
//!
//! ## EIP-155 legacy (v = chain_id * 2 + 35 | 36)
//! ```text
//!     msg = keccak256(rlp([nonce, gas_price, gas_limit, to, value, data, chain_id, 0, 0]))
//!     recid = v - (chain_id * 2 + 35)
//! ```
//!
//! ## EIP-1559 (typed, v ∈ {0, 1})
//! ```text
//!     msg = keccak256(0x02 || rlp([chain_id, nonce, max_priority_fee, max_fee,
//!                                   gas_limit, to, value, data, access_list]))
//!     recid = v
//! ```

use aii_crypto::keccak::keccak256;
use aii_crypto::secp::{self, Signature};
use aii_types::{Address, AlgoId, H256, U256};
use alloy_rlp::Encodable;
use thiserror::Error;

use crate::header::{encode_u256, u256_length};
use crate::tx::eip1559::TxEip1559;
use crate::tx::legacy::{encode_to, encoded_to_length, TxLegacy};
use crate::tx::Tx;

/// Errors raised by [`Tx::recover_signer`].
#[derive(Debug, Error)]
pub enum RecoveryError {
    /// Tx uses a post-quantum signature algorithm — out of scope here.
    #[error("non-secp256k1 algo_id ({0:?})")]
    NotSecp256k1(AlgoId),
    /// `v` does not encode a valid recovery id for this tx + chain.
    #[error("invalid v field: {0}")]
    InvalidV(u64),
    /// secp256k1 layer rejected the signature scalars or recovery.
    #[error("secp256k1: {0}")]
    Secp(#[from] aii_crypto::error::CryptoError),
    /// EIP-4844 signer recovery is not implemented (no v0.0.37 use case).
    #[error("EIP-4844 signer recovery not implemented")]
    Eip4844Unsupported,
}

impl Tx {
    /// Recover the Ethereum-style 20-byte address of the signer.
    ///
    /// `chain_id` is used only for legacy EIP-155 v-mixing — pre-EIP-155
    /// signatures (`v ∈ {27, 28}`) ignore it, and EIP-1559 carries its
    /// own `chain_id` in the body.
    pub fn recover_signer(&self, chain_id: u64) -> Result<Address, RecoveryError> {
        match self {
            Self::Legacy(t) => recover_legacy(t, chain_id),
            Self::Eip1559(t) => recover_eip1559(t),
            Self::Eip4844(_) => Err(RecoveryError::Eip4844Unsupported),
        }
    }
}

fn recover_legacy(t: &TxLegacy, chain_id: u64) -> Result<Address, RecoveryError> {
    if t.algo_id != AlgoId::Secp256k1 {
        return Err(RecoveryError::NotSecp256k1(t.algo_id));
    }
    // Determine recovery id + which signing-hash form to use.
    let (recid, hash) = if t.v == 27 || t.v == 28 {
        let recid = u8::try_from(t.v - 27).expect("v in {27,28}");
        (recid, legacy_signing_hash_pre155(t))
    } else if t.v >= 35 {
        // EIP-155: v = chain_id * 2 + 35 + recid
        let expected_base = chain_id
            .checked_mul(2)
            .and_then(|x| x.checked_add(35))
            .ok_or(RecoveryError::InvalidV(t.v))?;
        let delta =
            t.v.checked_sub(expected_base)
                .ok_or(RecoveryError::InvalidV(t.v))?;
        if delta > 1 {
            return Err(RecoveryError::InvalidV(t.v));
        }
        let recid = u8::try_from(delta).expect("delta ∈ {0,1}");
        (recid, legacy_signing_hash_eip155(t, chain_id))
    } else {
        return Err(RecoveryError::InvalidV(t.v));
    };
    recover_from_rs(recid, &t.r, &t.s, &hash)
}

fn recover_eip1559(t: &TxEip1559) -> Result<Address, RecoveryError> {
    if t.algo_id != AlgoId::Secp256k1 {
        return Err(RecoveryError::NotSecp256k1(t.algo_id));
    }
    if t.v > 1 {
        return Err(RecoveryError::InvalidV(u64::from(t.v)));
    }
    let recid = t.v;
    let hash = eip1559_signing_hash(t);
    recover_from_rs(recid, &t.r, &t.s, &hash)
}

fn recover_from_rs(recid: u8, r: &H256, s: &H256, hash: &H256) -> Result<Address, RecoveryError> {
    let mut bytes = [0u8; 65];
    bytes[..32].copy_from_slice(r.as_bytes());
    bytes[32..64].copy_from_slice(s.as_bytes());
    bytes[64] = recid;
    let sig = Signature::from_bytes(&bytes)?;
    let pk = secp::recover(&sig, hash)?;
    Ok(pk.address())
}

/// `keccak256(rlp([nonce, gas_price, gas_limit, to, value, data]))`.
fn legacy_signing_hash_pre155(t: &TxLegacy) -> H256 {
    let mut buf = alloy_rlp::bytes::BytesMut::new();
    let payload_length = t.nonce.length()
        + u256_length(&t.gas_price)
        + t.gas_limit.length()
        + encoded_to_length(&t.to)
        + u256_length(&t.value)
        + t.data.as_slice().length();
    alloy_rlp::Header {
        list: true,
        payload_length,
    }
    .encode(&mut buf);
    t.nonce.encode(&mut buf);
    encode_u256(&t.gas_price, &mut buf);
    t.gas_limit.encode(&mut buf);
    encode_to(&t.to, &mut buf);
    encode_u256(&t.value, &mut buf);
    t.data.as_slice().encode(&mut buf);
    keccak256(&buf)
}

/// `keccak256(rlp([nonce, gas_price, gas_limit, to, value, data, chain_id, 0, 0]))`.
fn legacy_signing_hash_eip155(t: &TxLegacy, chain_id: u64) -> H256 {
    let mut buf = alloy_rlp::bytes::BytesMut::new();
    let zero = U256::ZERO;
    let payload_length = t.nonce.length()
        + u256_length(&t.gas_price)
        + t.gas_limit.length()
        + encoded_to_length(&t.to)
        + u256_length(&t.value)
        + t.data.as_slice().length()
        + chain_id.length()
        + u256_length(&zero)
        + u256_length(&zero);
    alloy_rlp::Header {
        list: true,
        payload_length,
    }
    .encode(&mut buf);
    t.nonce.encode(&mut buf);
    encode_u256(&t.gas_price, &mut buf);
    t.gas_limit.encode(&mut buf);
    encode_to(&t.to, &mut buf);
    encode_u256(&t.value, &mut buf);
    t.data.as_slice().encode(&mut buf);
    chain_id.encode(&mut buf);
    encode_u256(&zero, &mut buf);
    encode_u256(&zero, &mut buf);
    keccak256(&buf)
}

/// `keccak256(0x02 || rlp([chain_id, nonce, max_priority_fee, max_fee,
///                           gas_limit, to, value, data, access_list]))`.
fn eip1559_signing_hash(t: &TxEip1559) -> H256 {
    let access_list_inner: usize = t.access_list.iter().map(Encodable::length).sum();
    let access_list_outer = alloy_rlp::length_of_length(access_list_inner) + access_list_inner;
    let mut body = alloy_rlp::bytes::BytesMut::new();
    let payload_length = t.chain_id.length()
        + t.nonce.length()
        + u256_length(&t.max_priority_fee_per_gas)
        + u256_length(&t.max_fee_per_gas)
        + t.gas_limit.length()
        + encoded_to_length(&t.to)
        + u256_length(&t.value)
        + t.data.as_slice().length()
        + access_list_outer;
    alloy_rlp::Header {
        list: true,
        payload_length,
    }
    .encode(&mut body);
    t.chain_id.encode(&mut body);
    t.nonce.encode(&mut body);
    encode_u256(&t.max_priority_fee_per_gas, &mut body);
    encode_u256(&t.max_fee_per_gas, &mut body);
    t.gas_limit.encode(&mut body);
    encode_to(&t.to, &mut body);
    encode_u256(&t.value, &mut body);
    t.data.as_slice().encode(&mut body);
    alloy_rlp::Header {
        list: true,
        payload_length: access_list_inner,
    }
    .encode(&mut body);
    for item in &t.access_list {
        item.encode(&mut body);
    }

    let mut out = alloy_rlp::bytes::BytesMut::with_capacity(body.len() + 1);
    out.extend_from_slice(&[0x02]);
    out.extend_from_slice(&body);
    keccak256(&out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aii_crypto::secp::{sign, SecretKey};

    fn deterministic_sk(seed: u8) -> SecretKey {
        let mut bytes = [0u8; 32];
        bytes[31] = seed.max(1);
        SecretKey::from_bytes(&bytes).unwrap()
    }

    /// Build an EIP-155 signed legacy tx with a known secret key and
    /// recover the address; round-trip must yield sk.public_key().address().
    #[test]
    fn legacy_eip155_round_trip_recovers_signer() {
        let sk = deterministic_sk(7);
        let expected_addr = sk.public_key().address();
        let chain_id = 9999u64;
        let mut tx = TxLegacy {
            nonce: 0,
            gas_price: U256::from(1_000_000_000u64),
            gas_limit: 21_000,
            to: Some(Address::new([0x12; 20])),
            value: U256::from(1u64),
            data: vec![],
            v: 0,
            r: H256::ZERO,
            s: H256::ZERO,
            algo_id: AlgoId::Secp256k1,
        };
        // Sign over the EIP-155 hash.
        let hash = legacy_signing_hash_eip155(&tx, chain_id);
        let sig = sign(&sk, &hash).unwrap();
        let raw = sig.to_bytes();
        tx.r = H256::new(raw[..32].try_into().unwrap());
        tx.s = H256::new(raw[32..64].try_into().unwrap());
        tx.v = chain_id * 2 + 35 + u64::from(raw[64]);

        let wrapped = Tx::Legacy(tx);
        let recovered = wrapped.recover_signer(chain_id).unwrap();
        assert_eq!(recovered, expected_addr);
    }

    #[test]
    fn legacy_pre_eip155_round_trip_recovers_signer() {
        let sk = deterministic_sk(9);
        let expected_addr = sk.public_key().address();
        let mut tx = TxLegacy {
            nonce: 1,
            gas_price: U256::from(20_000_000_000u64),
            gas_limit: 21_000,
            to: Some(Address::new([0xab; 20])),
            value: U256::ZERO,
            data: vec![],
            v: 0,
            r: H256::ZERO,
            s: H256::ZERO,
            algo_id: AlgoId::Secp256k1,
        };
        let hash = legacy_signing_hash_pre155(&tx);
        let sig = sign(&sk, &hash).unwrap();
        let raw = sig.to_bytes();
        tx.r = H256::new(raw[..32].try_into().unwrap());
        tx.s = H256::new(raw[32..64].try_into().unwrap());
        tx.v = 27 + u64::from(raw[64]);

        // chain_id doesn't matter for pre-EIP-155.
        let recovered = Tx::Legacy(tx).recover_signer(9999).unwrap();
        assert_eq!(recovered, expected_addr);
    }

    #[test]
    fn eip1559_round_trip_recovers_signer() {
        let sk = deterministic_sk(11);
        let expected_addr = sk.public_key().address();
        let chain_id = 9999u64;
        let mut tx = TxEip1559 {
            chain_id,
            nonce: 0,
            max_priority_fee_per_gas: U256::from(1u64),
            max_fee_per_gas: U256::from(20u64),
            gas_limit: 21_000,
            to: Some(Address::new([0x33; 20])),
            value: U256::from(1u64),
            data: vec![],
            access_list: vec![],
            v: 0,
            r: H256::ZERO,
            s: H256::ZERO,
            algo_id: AlgoId::Secp256k1,
        };
        let hash = eip1559_signing_hash(&tx);
        let sig = sign(&sk, &hash).unwrap();
        let raw = sig.to_bytes();
        tx.r = H256::new(raw[..32].try_into().unwrap());
        tx.s = H256::new(raw[32..64].try_into().unwrap());
        tx.v = raw[64];

        let recovered = Tx::Eip1559(tx).recover_signer(chain_id).unwrap();
        assert_eq!(recovered, expected_addr);
    }

    #[test]
    fn pq_algo_id_rejected() {
        let tx = TxLegacy {
            nonce: 0,
            gas_price: U256::from(1u64),
            gas_limit: 21_000,
            to: None,
            value: U256::ZERO,
            data: vec![],
            v: 27,
            r: H256::new([0xab; 32]),
            s: H256::new([0xcd; 32]),
            algo_id: AlgoId::MlDsa65,
        };
        let err = Tx::Legacy(tx).recover_signer(0).unwrap_err();
        assert!(matches!(err, RecoveryError::NotSecp256k1(AlgoId::MlDsa65)));
    }

    #[test]
    fn legacy_v_too_small_rejected() {
        let tx = TxLegacy {
            nonce: 0,
            gas_price: U256::from(1u64),
            gas_limit: 21_000,
            to: None,
            value: U256::ZERO,
            data: vec![],
            v: 5, // not 27/28, not >= 35
            r: H256::ZERO,
            s: H256::ZERO,
            algo_id: AlgoId::Secp256k1,
        };
        let err = Tx::Legacy(tx).recover_signer(9999).unwrap_err();
        assert!(matches!(err, RecoveryError::InvalidV(5)));
    }

    #[test]
    fn eip1559_v_too_large_rejected() {
        let tx = TxEip1559 {
            chain_id: 9999,
            nonce: 0,
            max_priority_fee_per_gas: U256::from(1u64),
            max_fee_per_gas: U256::from(2u64),
            gas_limit: 21_000,
            to: None,
            value: U256::ZERO,
            data: vec![],
            access_list: vec![],
            v: 2,
            r: H256::ZERO,
            s: H256::ZERO,
            algo_id: AlgoId::Secp256k1,
        };
        let err = Tx::Eip1559(tx).recover_signer(9999).unwrap_err();
        assert!(matches!(err, RecoveryError::InvalidV(2)));
    }
}
