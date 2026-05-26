//! # aii-wallet
//!
//! Local secp256k1 wallet for the AII protocol.
//!
//! ## Public API
//! - [`LocalWallet`] — holds a `SecretKey` and the derived `Address`
//! - [`EncryptedKeystore`] — Web3 Secret Storage v3 JSON envelope
//!   (scrypt KDF + AES-128-CTR cipher + Keccak-256 MAC). Compatible
//!   byte-for-byte with `geth account import` / MetaMask JSON keystores.
//! - [`WalletError`] umbrella
//!
//! BIP-39 mnemonic + BIP-32 HD derivation land in a later version
//! alongside `aii-cli`'s `account import-mnemonic` command.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod keystore;
pub mod mnemonic;
mod wallet;

pub use keystore::{EncryptedBytes, EncryptedKeystore, ScryptParams};
pub use mnemonic::{MnemonicError, MnemonicPhrase, ETH_BIP44_PREFIX};
pub use wallet::{LocalWallet, WalletError};
