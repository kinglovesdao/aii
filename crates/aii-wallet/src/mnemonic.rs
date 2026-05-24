//! BIP-39 mnemonic + BIP-32 HD derivation.
//!
//! ## What's here
//! - [`MnemonicPhrase`] — 12/15/18/21/24-word English-wordlist mnemonic
//!   per BIP-39. `generate` / `from_phrase` / `to_phrase` / `to_seed`.
//! - [`MnemonicPhrase::to_wallet`] — derive an Ethereum-compatible
//!   `LocalWallet` from the mnemonic at BIP-44 path
//!   `m/44'/60'/0'/0/{index}` (the path MetaMask + most ETH wallets use).
//!
//! BIP-44 coin type `60` is **Ethereum mainnet**. AII chose this path
//! deliberately for wallet interop: a seed imported into MetaMask
//! produces the exact same addresses on AII and ETH. If the protocol
//! later requires a chain-specific path we can add a separate
//! `to_wallet_aii(...)` helper at coin type 9999 or similar.

use crate::wallet::LocalWallet;
use bip32::{DerivationPath, XPrv};
use bip39::{Language, Mnemonic};
use rand::RngCore;
use thiserror::Error;
use zeroize::Zeroize;

/// Default BIP-44 derivation path prefix for ETH-compatible wallets.
/// Concrete derivations append `/{index}` to address one account.
pub const ETH_BIP44_PREFIX: &str = "m/44'/60'/0'/0";

/// A validated BIP-39 mnemonic phrase.
pub struct MnemonicPhrase {
    inner: Mnemonic,
}

impl MnemonicPhrase {
    /// Generate a fresh `word_count`-word mnemonic from OS RNG.
    /// `word_count` must be 12, 15, 18, 21, or 24.
    pub fn generate(word_count: usize) -> Result<Self, MnemonicError> {
        let entropy_bytes = match word_count {
            12 => 16,
            15 => 20,
            18 => 24,
            21 => 28,
            24 => 32,
            other => return Err(MnemonicError::BadWordCount(other)),
        };
        let mut entropy = vec![0u8; entropy_bytes];
        rand::thread_rng().fill_bytes(&mut entropy);
        let m = Mnemonic::from_entropy_in(Language::English, &entropy)
            .map_err(|e| MnemonicError::Bip39(e.to_string()))?;
        entropy.zeroize();
        Ok(Self { inner: m })
    }

    /// Generate a fresh 12-word mnemonic (the most common choice).
    pub fn generate_12() -> Result<Self, MnemonicError> {
        Self::generate(12)
    }

    /// Parse and validate a mnemonic phrase string (whitespace-separated
    /// English words). Returns [`MnemonicError::InvalidPhrase`] on
    /// checksum or wordlist failure.
    pub fn from_phrase(phrase: &str) -> Result<Self, MnemonicError> {
        let m = Mnemonic::parse_in_normalized(Language::English, phrase)
            .map_err(|e| MnemonicError::InvalidPhrase(e.to_string()))?;
        Ok(Self { inner: m })
    }

    /// Canonical space-separated phrase.
    pub fn to_phrase(&self) -> String {
        self.inner.to_string()
    }

    /// Number of words.
    pub fn word_count(&self) -> usize {
        self.inner.word_count()
    }

    /// Derive the 64-byte BIP-39 seed under `passphrase`. Use an empty
    /// string for the standard "no passphrase" case (MetaMask default).
    pub fn to_seed(&self, passphrase: &str) -> [u8; 64] {
        self.inner.to_seed(passphrase)
    }

    /// Derive an [`LocalWallet`] at BIP-44 path `m/44'/60'/0'/0/{index}`.
    ///
    /// `passphrase` is the optional BIP-39 passphrase ("25th word"); use
    /// `""` for the default MetaMask path.
    pub fn to_wallet(&self, passphrase: &str, index: u32) -> Result<LocalWallet, MnemonicError> {
        let mut seed = self.to_seed(passphrase);
        let result = derive_from_seed(&seed, index);
        seed.zeroize();
        result
    }
}

fn derive_from_seed(seed: &[u8; 64], index: u32) -> Result<LocalWallet, MnemonicError> {
    let path_str = format!("{ETH_BIP44_PREFIX}/{index}");
    let path: DerivationPath = path_str
        .parse()
        .map_err(|e: bip32::Error| MnemonicError::Bip32(e.to_string()))?;
    let xprv =
        XPrv::derive_from_path(seed, &path).map_err(|e| MnemonicError::Bip32(e.to_string()))?;
    let secret_bytes = xprv.private_key().to_bytes();
    let arr: [u8; 32] = secret_bytes.into();
    LocalWallet::from_secret_bytes(&arr).map_err(|e| MnemonicError::Wallet(format!("{e:?}")))
}

/// Errors produced by the BIP-39 / BIP-32 layer.
#[derive(Debug, Error)]
pub enum MnemonicError {
    /// Internal BIP-39 failure (bad entropy length, RNG failure, etc.).
    #[error("bip39: {0}")]
    Bip39(String),

    /// `word_count` not in {12, 15, 18, 21, 24}.
    #[error("invalid word count: {0} (allowed: 12/15/18/21/24)")]
    BadWordCount(usize),

    /// Phrase failed validation — bad word or wrong checksum.
    #[error("invalid phrase: {0}")]
    InvalidPhrase(String),

    /// BIP-32 derivation failure.
    #[error("bip32: {0}")]
    Bip32(String),

    /// Wallet construction failed (essentially never — secp256k1 scalar from
    /// derivation is non-zero with overwhelming probability).
    #[error("wallet: {0}")]
    Wallet(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Official BIP-39 test vector (Trezor):
    /// entropy = 00000000…00, passphrase = "TREZOR"
    /// 12 words: "abandon abandon … about"
    const TREZOR_PHRASE: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    const TREZOR_SEED_HEX: &str = "c55257c360c07c72029aebc1b53c05ed0362ada38ead3e3e9efa3708e53495531f09a6987599d18264c1e1c92f2cf141630c7a3c4ab7c81b2f001698e7463b04";

    /// Well-known BIP-44 test vector. Mnemonic = `abandon × 11, about`,
    /// **empty passphrase** (the MetaMask default), path `m/44'/60'/0'/0/0`
    /// → address `0x9858EfFD232B4033E47d90003D41EC34EcaEda94`. This is
    /// the canonical fixture used across ethers-rs, web3.js,
    /// MetaMask docs, etc.
    const ABANDON_EMPTY_PP_INDEX_0_ADDRESS: &str = "9858EfFD232B4033E47d90003D41EC34EcaEda94";

    #[test]
    fn generate_12_words() {
        let m = MnemonicPhrase::generate_12().unwrap();
        assert_eq!(m.word_count(), 12);
    }

    #[test]
    fn generate_24_words() {
        let m = MnemonicPhrase::generate(24).unwrap();
        assert_eq!(m.word_count(), 24);
    }

    #[test]
    fn generate_rejects_bad_word_count() {
        // Only 12/15/18/21/24 are valid.
        assert!(MnemonicPhrase::generate(11).is_err());
        assert!(MnemonicPhrase::generate(13).is_err());
        assert!(MnemonicPhrase::generate(0).is_err());
    }

    #[test]
    fn generate_then_parse_round_trip() {
        let m1 = MnemonicPhrase::generate_12().unwrap();
        let phrase = m1.to_phrase();
        let m2 = MnemonicPhrase::from_phrase(&phrase).unwrap();
        assert_eq!(m1.to_phrase(), m2.to_phrase());
    }

    #[test]
    fn parse_rejects_bad_checksum() {
        // Swapping last word breaks the checksum.
        let bad = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon";
        assert!(MnemonicPhrase::from_phrase(bad).is_err());
    }

    #[test]
    fn parse_rejects_non_wordlist_word() {
        let bad = "notaword abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        assert!(MnemonicPhrase::from_phrase(bad).is_err());
    }

    #[test]
    fn trezor_official_vector_seed_matches() {
        let m = MnemonicPhrase::from_phrase(TREZOR_PHRASE).unwrap();
        let seed = m.to_seed("TREZOR");
        assert_eq!(hex::encode(seed), TREZOR_SEED_HEX);
    }

    #[test]
    fn abandon_vector_index_0_address_matches_canonical_eth_reference() {
        // The MetaMask default: empty passphrase, m/44'/60'/0'/0/0.
        let m = MnemonicPhrase::from_phrase(TREZOR_PHRASE).unwrap();
        let w = m.to_wallet("", 0).unwrap();
        let actual = hex::encode_upper(w.address().as_bytes());
        assert_eq!(
            actual.to_lowercase(),
            ABANDON_EMPTY_PP_INDEX_0_ADDRESS.to_lowercase(),
            "BIP-44 derivation diverged from canonical ethers/MetaMask fixture"
        );
    }

    #[test]
    fn different_indices_produce_different_addresses() {
        let m = MnemonicPhrase::from_phrase(TREZOR_PHRASE).unwrap();
        let w0 = m.to_wallet("", 0).unwrap();
        let w1 = m.to_wallet("", 1).unwrap();
        assert_ne!(w0.address(), w1.address());
    }

    #[test]
    fn different_passphrases_produce_different_addresses() {
        let m = MnemonicPhrase::from_phrase(TREZOR_PHRASE).unwrap();
        let w_a = m.to_wallet("", 0).unwrap();
        let w_b = m.to_wallet("TREZOR", 0).unwrap();
        assert_ne!(w_a.address(), w_b.address());
    }

    #[test]
    fn no_passphrase_is_default_metamask_path() {
        // MetaMask uses the empty passphrase by default. We just verify that
        // empty passphrase gives a deterministic, non-zero address.
        let m = MnemonicPhrase::from_phrase(TREZOR_PHRASE).unwrap();
        let w = m.to_wallet("", 0).unwrap();
        assert_ne!(w.address(), aii_types::Address::ZERO);
    }
}
