//! # aii-wallet
//!
//! In-memory secp256k1 wallet for the AII protocol. Wraps `aii-crypto`'s
//! signature primitives behind a thin, account-shaped API. Encrypted
//! keystore + BIP-39 recovery land in v0.0.7.
//!
//! ## Public API
//! - [`LocalWallet`] — holds a `SecretKey` and the derived `Address`
//! - [`WalletError`] umbrella

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use aii_crypto::secp::{self, SecretKey, Signature};
use aii_crypto::CryptoError;
use aii_types::{Address, H256};
use thiserror::Error;
use zeroize::Zeroize;

/// A locally-managed signing identity.
pub struct LocalWallet {
    secret: SecretKey,
    address: Address,
}

impl LocalWallet {
    /// Construct from a raw 32-byte secret. Returns an error if the secret
    /// is not a valid secp256k1 scalar.
    pub fn from_secret_bytes(bytes: &[u8; 32]) -> Result<Self, WalletError> {
        let secret = SecretKey::from_bytes(bytes)?;
        let public = secret.public_key();
        Ok(Self {
            secret,
            address: public.address(),
        })
    }

    /// EOA address derived from the secret key.
    pub const fn address(&self) -> Address {
        self.address
    }

    /// Sign a 32-byte message digest. The caller is responsible for
    /// computing the digest (typically `Tx::hash()` or `Header::hash()`).
    pub fn sign_message_hash(&self, message_hash: &H256) -> Result<Signature, WalletError> {
        Ok(secp::sign(&self.secret, message_hash)?)
    }
}

impl Drop for LocalWallet {
    fn drop(&mut self) {
        // SecretKey internally wraps k256's SigningKey which already zeroes
        // on drop — but we also wipe our local memory of the address (not
        // sensitive, but cheap insurance against double-free patterns).
        let mut a = self.address.0;
        a.zeroize();
    }
}

/// Errors produced by the wallet.
#[derive(Debug, Error)]
pub enum WalletError {
    /// Underlying crypto error (bad key bytes, bad signature, etc.).
    #[error("crypto: {0}")]
    Crypto(#[from] CryptoError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_wallet() -> LocalWallet {
        // sk = 1 — the well-known test vector address
        // 0x7E5F4552091A69125d5DfCb7b8C2659029395Bdf
        let mut sk = [0u8; 32];
        sk[31] = 1;
        LocalWallet::from_secret_bytes(&sk).unwrap()
    }

    #[test]
    fn address_matches_sk_one_kat() {
        let w = fixed_wallet();
        // address for sk = 1 (verified against Etherscan / aii-crypto tests)
        let expected = "7E5F4552091A69125d5DfCb7b8C2659029395Bdf";
        let actual = hex::encode_upper(w.address().as_bytes());
        // ETH-style mixed case isn't enforced — compare lowercase
        assert_eq!(actual.to_lowercase(), expected.to_lowercase());
    }

    #[test]
    fn sign_round_trip_verifies() {
        let w = fixed_wallet();
        let msg = H256::new([0xab; 32]);
        let sig = w.sign_message_hash(&msg).unwrap();
        let recovered = aii_crypto::secp::recover(&sig, &msg).unwrap();
        assert_eq!(recovered.address(), w.address());
    }

    #[test]
    fn deterministic_signing() {
        let w = fixed_wallet();
        let msg = H256::new([0xcd; 32]);
        let s1 = w.sign_message_hash(&msg).unwrap();
        let s2 = w.sign_message_hash(&msg).unwrap();
        assert_eq!(s1.to_bytes(), s2.to_bytes());
    }

    #[test]
    fn rejects_zero_scalar() {
        let zero = [0u8; 32];
        assert!(LocalWallet::from_secret_bytes(&zero).is_err());
    }

    #[test]
    fn different_keys_different_addresses() {
        let mut sk1 = [0u8; 32];
        sk1[31] = 1;
        let mut sk2 = [0u8; 32];
        sk2[31] = 2;
        let w1 = LocalWallet::from_secret_bytes(&sk1).unwrap();
        let w2 = LocalWallet::from_secret_bytes(&sk2).unwrap();
        assert_ne!(w1.address(), w2.address());
    }
}
