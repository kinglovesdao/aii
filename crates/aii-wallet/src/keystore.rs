//! Web3 Secret Storage v3 encrypted keystore.
//!
//! Encrypts a 32-byte secp256k1 secret under a user-chosen password.
//! Format matches the Ethereum reference (geth `account import` /
//! MetaMask), so a keystore generated here can be opened by external
//! tools and vice versa.
//!
//! ## Wire format (JSON)
//! ```json
//! {
//!   "version": 3,
//!   "id": "uuid",
//!   "address": "hex-no-0x (20 bytes)",
//!   "crypto": {
//!     "ciphertext": "hex (32 bytes)",
//!     "cipherparams": { "iv": "hex (16 bytes)" },
//!     "cipher": "aes-128-ctr",
//!     "kdf": "scrypt",
//!     "kdfparams": { "dklen": 32, "salt": "hex", "n": 8192, "r": 8, "p": 1 },
//!     "mac": "hex (32 bytes)"
//!   }
//! }
//! ```
//!
//! - `n`, `r`, `p` are scrypt cost parameters (defaults match geth).
//! - The MAC is `keccak256(derived_key[16..32] ‖ ciphertext)`.
//! - Decryption verifies the MAC **before** running AES-CTR (key-attack
//!   detection); a wrong password produces `KeystoreError::BadMac` rather
//!   than a misleading "decryption produced garbage" outcome.

use crate::wallet::LocalWallet;
use aes::Aes128;
use aii_crypto::keccak::keccak256;
use aii_crypto::CryptoError;
use aii_types::Address;
use cipher::{KeyIvInit, StreamCipher};
use rand::RngCore;
use scrypt::scrypt;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroize;

type Aes128Ctr = ctr::Ctr64BE<Aes128>;

/// Scrypt cost parameters. Defaults match geth's `--lightkdf` mode:
/// n=4096, r=8, p=6 (~64 ms on a 2024 laptop). For high-security use set
/// `[Self::geth_default]` (n=262144) — costs ~1s per decrypt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScryptParams {
    /// CPU/memory cost (must be a power of two; ≥ 1024).
    pub n: u32,
    /// Block size.
    pub r: u32,
    /// Parallelism.
    pub p: u32,
}

impl ScryptParams {
    /// Light KDF — fast (~64 ms), suitable for CI tests and dev keystores.
    pub const fn light() -> Self {
        Self {
            n: 4096,
            r: 8,
            p: 6,
        }
    }

    /// Production-strength KDF (~1 s per decrypt on a 2024 CPU).
    pub const fn geth_default() -> Self {
        Self {
            n: 262_144,
            r: 8,
            p: 1,
        }
    }
}

impl Default for ScryptParams {
    fn default() -> Self {
        Self::light()
    }
}

/// Scrypt-KDF parameters serialised inside [`Crypto`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct KdfParams {
    pub dklen: u32,
    pub salt: String,
    pub n: u32,
    pub r: u32,
    pub p: u32,
}

/// AES-CTR cipher parameters serialised inside [`Crypto`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct CipherParams {
    pub iv: String,
}

/// Encrypted ciphertext block. Public because callers of
/// [`EncryptedBytes`] may need to inspect ciphertext / mac fields,
/// but normal usage is `encrypt(...).to_json()` and never touches it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct Crypto {
    pub ciphertext: String,
    pub cipherparams: CipherParams,
    pub cipher: String,
    pub kdf: String,
    pub kdfparams: KdfParams,
    pub mac: String,
}

/// Encrypted keystore record. Round-trips through JSON via serde.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedKeystore {
    version: u32,
    id: Uuid,
    /// Recipient address, hex-encoded without the `0x` prefix.
    pub address: String,
    crypto: Crypto,
}

impl EncryptedKeystore {
    /// Encrypt a `LocalWallet` under `password` with the supplied scrypt
    /// parameters. Returns the keystore record.
    ///
    /// # Errors
    /// - `Scrypt` if the parameters are invalid (non-power-of-two n, etc.).
    pub fn encrypt(
        wallet: &LocalWallet,
        password: &str,
        params: ScryptParams,
    ) -> Result<Self, KeystoreError> {
        let mut salt = [0u8; 32];
        let mut iv = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut salt);
        rand::thread_rng().fill_bytes(&mut iv);

        let derived = derive_key(password, &salt, params)?;

        // AES-128-CTR encryption of the 32-byte secret.
        let mut secret = wallet.secret_bytes();
        let mut cipher = Aes128Ctr::new((&derived[..16]).into(), (&iv).into());
        cipher.apply_keystream(&mut secret);

        // MAC = keccak256(derived[16..32] ‖ ciphertext)
        let mac_input = [&derived[16..32], &secret[..]].concat();
        let mac = keccak256(&mac_input);

        let out = Self {
            version: 3,
            id: Uuid::new_v4(),
            address: hex::encode(wallet.address().as_bytes()),
            crypto: Crypto {
                ciphertext: hex::encode(secret),
                cipherparams: CipherParams {
                    iv: hex::encode(iv),
                },
                cipher: "aes-128-ctr".to_string(),
                kdf: "scrypt".to_string(),
                kdfparams: KdfParams {
                    dklen: 32,
                    salt: hex::encode(salt),
                    n: params.n,
                    r: params.r,
                    p: params.p,
                },
                mac: hex::encode(mac.as_bytes()),
            },
        };

        // Wipe the plaintext secret copy from this stack frame.
        secret.zeroize();
        Ok(out)
    }

    /// Decrypt the keystore and return the embedded `LocalWallet`.
    pub fn decrypt(&self, password: &str) -> Result<LocalWallet, KeystoreError> {
        if self.version != 3 {
            return Err(KeystoreError::UnsupportedVersion(self.version));
        }
        if self.crypto.cipher != "aes-128-ctr" {
            return Err(KeystoreError::UnsupportedCipher(self.crypto.cipher.clone()));
        }
        if self.crypto.kdf != "scrypt" {
            return Err(KeystoreError::UnsupportedKdf(self.crypto.kdf.clone()));
        }

        let salt = hex::decode(&self.crypto.kdfparams.salt).map_err(KeystoreError::Hex)?;
        let iv = hex::decode(&self.crypto.cipherparams.iv).map_err(KeystoreError::Hex)?;
        let ciphertext = hex::decode(&self.crypto.ciphertext).map_err(KeystoreError::Hex)?;
        let expected_mac = hex::decode(&self.crypto.mac).map_err(KeystoreError::Hex)?;

        if iv.len() != 16 {
            return Err(KeystoreError::BadIvLength(iv.len()));
        }
        if ciphertext.len() != 32 {
            return Err(KeystoreError::BadCiphertextLength(ciphertext.len()));
        }

        let params = ScryptParams {
            n: self.crypto.kdfparams.n,
            r: self.crypto.kdfparams.r,
            p: self.crypto.kdfparams.p,
        };
        let derived = derive_key(password, &salt, params)?;

        // Verify MAC first — wrong password should error cleanly here.
        let mac_input = [&derived[16..32], &ciphertext[..]].concat();
        let actual_mac = keccak256(&mac_input);
        if actual_mac.as_bytes() != expected_mac.as_slice() {
            return Err(KeystoreError::BadMac);
        }

        // Decrypt.
        let mut secret = [0u8; 32];
        secret.copy_from_slice(&ciphertext);
        let mut iv_arr = [0u8; 16];
        iv_arr.copy_from_slice(&iv);
        let mut cipher = Aes128Ctr::new((&derived[..16]).into(), (&iv_arr).into());
        cipher.apply_keystream(&mut secret);

        let wallet = LocalWallet::from_secret_bytes(&secret).map_err(|e| match e {
            crate::wallet::WalletError::Crypto(c) => KeystoreError::WalletConstruct(c),
        })?;
        secret.zeroize();
        Ok(wallet)
    }

    /// Address embedded in the keystore — returned as a parsed `Address`.
    pub fn parsed_address(&self) -> Result<Address, KeystoreError> {
        let bytes = hex::decode(&self.address).map_err(KeystoreError::Hex)?;
        if bytes.len() != 20 {
            return Err(KeystoreError::BadAddressLength(bytes.len()));
        }
        let mut arr = [0u8; 20];
        arr.copy_from_slice(&bytes);
        Ok(Address::new(arr))
    }

    /// Serialise to canonical JSON bytes (one line).
    pub fn to_json(&self) -> String {
        // `unwrap` is safe: our schema cannot fail serde.
        serde_json::to_string(self).expect("EncryptedKeystore is always serializable")
    }

    /// Parse from JSON.
    pub fn from_json(s: &str) -> Result<Self, KeystoreError> {
        serde_json::from_str(s).map_err(KeystoreError::Json)
    }
}

/// Web3-Secret-Storage-style encrypted container for arbitrary-length
/// secret payloads — used for validator keystores (which contain BLS
/// + VRF secrets that exceed the 32-byte wallet shape).
///
/// Same scrypt+AES-128-CTR recipe as [`EncryptedKeystore`]; only the
/// `ciphertext` length is variable and the optional `address` field
/// is replaced by a free-form `label` to disambiguate keystores.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedBytes {
    /// Schema version. Always 1.
    pub version: u32,
    /// UUIDv4 for cross-referencing in operational tooling.
    pub id: Uuid,
    /// Human-readable label (e.g. `"aii-validator"`).
    pub label: String,
    /// Crypto block (matches the wallet keystore's Crypto shape).
    pub crypto: Crypto,
}

impl EncryptedBytes {
    /// Encrypt `payload` under `password`. Returns a self-contained
    /// JSON-serialisable record.
    ///
    /// # Errors
    /// Returns [`KeystoreError::Scrypt`] if the scrypt KDF rejects the
    /// supplied parameters (non-power-of-two n, etc.).
    pub fn encrypt(
        payload: &[u8],
        password: &str,
        params: ScryptParams,
        label: &str,
    ) -> Result<Self, KeystoreError> {
        let mut salt = [0u8; 32];
        let mut iv = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut salt);
        rand::thread_rng().fill_bytes(&mut iv);

        let derived = derive_key(password, &salt, params)?;
        let mut ciphertext = payload.to_vec();
        let mut cipher = Aes128Ctr::new((&derived[..16]).into(), (&iv).into());
        cipher.apply_keystream(&mut ciphertext);

        // Same MAC recipe as the wallet keystore.
        let mac_input = [&derived[16..32], &ciphertext[..]].concat();
        let mac = keccak256(&mac_input);

        Ok(Self {
            version: 1,
            id: Uuid::new_v4(),
            label: label.to_string(),
            crypto: Crypto {
                ciphertext: hex::encode(&ciphertext),
                cipherparams: CipherParams {
                    iv: hex::encode(iv),
                },
                cipher: "aes-128-ctr".to_string(),
                kdf: "scrypt".to_string(),
                kdfparams: KdfParams {
                    dklen: 32,
                    salt: hex::encode(salt),
                    n: params.n,
                    r: params.r,
                    p: params.p,
                },
                mac: hex::encode(mac.as_bytes()),
            },
        })
    }

    /// Decrypt the payload. Verifies the MAC before deciphering so a
    /// wrong password fails fast with [`KeystoreError::BadMac`].
    ///
    /// # Errors
    /// `BadMac` on wrong password; `UnsupportedCipher` / `UnsupportedKdf`
    /// if the JSON was produced by a future schema this build doesn't
    /// understand; `Hex` on malformed hex fields.
    pub fn decrypt(&self, password: &str) -> Result<Vec<u8>, KeystoreError> {
        if self.version != 1 {
            return Err(KeystoreError::UnsupportedVersion(self.version));
        }
        if self.crypto.cipher != "aes-128-ctr" {
            return Err(KeystoreError::UnsupportedCipher(self.crypto.cipher.clone()));
        }
        if self.crypto.kdf != "scrypt" {
            return Err(KeystoreError::UnsupportedKdf(self.crypto.kdf.clone()));
        }
        let salt = hex::decode(&self.crypto.kdfparams.salt).map_err(KeystoreError::Hex)?;
        let iv = hex::decode(&self.crypto.cipherparams.iv).map_err(KeystoreError::Hex)?;
        let mut ciphertext = hex::decode(&self.crypto.ciphertext).map_err(KeystoreError::Hex)?;
        let expected_mac = hex::decode(&self.crypto.mac).map_err(KeystoreError::Hex)?;
        if iv.len() != 16 {
            return Err(KeystoreError::BadIvLength(iv.len()));
        }
        let params = ScryptParams {
            n: self.crypto.kdfparams.n,
            r: self.crypto.kdfparams.r,
            p: self.crypto.kdfparams.p,
        };
        let derived = derive_key(password, &salt, params)?;
        let mac_input = [&derived[16..32], &ciphertext[..]].concat();
        let actual_mac = keccak256(&mac_input);
        if actual_mac.as_bytes() != expected_mac.as_slice() {
            return Err(KeystoreError::BadMac);
        }
        let mut iv_arr = [0u8; 16];
        iv_arr.copy_from_slice(&iv);
        let mut cipher = Aes128Ctr::new((&derived[..16]).into(), (&iv_arr).into());
        cipher.apply_keystream(&mut ciphertext);
        Ok(ciphertext)
    }

    /// Serialise to canonical JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("EncryptedBytes is always serializable")
    }

    /// Parse from JSON.
    ///
    /// # Errors
    /// Returns [`KeystoreError::Json`] if the input is not a valid
    /// `EncryptedBytes` record.
    pub fn from_json(s: &str) -> Result<Self, KeystoreError> {
        serde_json::from_str(s).map_err(KeystoreError::Json)
    }
}

fn derive_key(
    password: &str,
    salt: &[u8],
    params: ScryptParams,
) -> Result<[u8; 32], KeystoreError> {
    // scrypt::Params::new takes log2(n), r, p, len.
    let log_n = u8::try_from(params.n.trailing_zeros())
        .map_err(|_| KeystoreError::InvalidScryptN(params.n))?;
    if 1u32 << u32::from(log_n) != params.n {
        return Err(KeystoreError::InvalidScryptN(params.n));
    }
    let scrypt_params = scrypt::Params::new(log_n, params.r, params.p, 32)
        .map_err(|e| KeystoreError::Scrypt(e.to_string()))?;
    let mut out = [0u8; 32];
    scrypt(password.as_bytes(), salt, &scrypt_params, &mut out)
        .map_err(|e| KeystoreError::Scrypt(e.to_string()))?;
    Ok(out)
}

/// Errors produced by the keystore.
#[derive(Debug, Error)]
pub enum KeystoreError {
    /// `version` field was not 3.
    #[error("unsupported keystore version {0} (this crate handles v3 only)")]
    UnsupportedVersion(u32),

    /// `cipher` field was not `aes-128-ctr`.
    #[error("unsupported cipher: {0}")]
    UnsupportedCipher(String),

    /// `kdf` field was not `scrypt`.
    #[error("unsupported kdf: {0}")]
    UnsupportedKdf(String),

    /// scrypt `n` is not a positive power of two.
    #[error("invalid scrypt n parameter: {0} (must be a power of two ≥ 2)")]
    InvalidScryptN(u32),

    /// scrypt parameter rejection from the underlying crate.
    #[error("scrypt: {0}")]
    Scrypt(String),

    /// MAC verification failed — usually means wrong password.
    #[error("bad MAC (wrong password or corrupted keystore)")]
    BadMac,

    /// IV must be 16 bytes.
    #[error("bad IV length: expected 16, got {0}")]
    BadIvLength(usize),

    /// Ciphertext must be 32 bytes.
    #[error("bad ciphertext length: expected 32, got {0}")]
    BadCiphertextLength(usize),

    /// Address must be 20 bytes.
    #[error("bad address length: expected 20, got {0}")]
    BadAddressLength(usize),

    /// Hex decode failure on any of the hex-encoded fields.
    #[error("hex decode: {0:?}")]
    Hex(hex::FromHexError),

    /// The decrypted bytes were not a valid secp256k1 scalar.
    #[error("wallet-construct: {0}")]
    WalletConstruct(CryptoError),

    /// JSON parse failure.
    #[error("json: {0}")]
    Json(serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_wallet() -> LocalWallet {
        let mut sk = [0u8; 32];
        sk[31] = 1;
        LocalWallet::from_secret_bytes(&sk).unwrap()
    }

    #[test]
    fn encrypt_decrypt_round_trip() {
        let w = fixed_wallet();
        let original_addr = w.address();
        let ks = EncryptedKeystore::encrypt(&w, "swordfish", ScryptParams::light()).unwrap();
        let recovered = ks.decrypt("swordfish").unwrap();
        assert_eq!(recovered.address(), original_addr);
    }

    #[test]
    fn wrong_password_rejects_with_bad_mac() {
        let w = fixed_wallet();
        let ks = EncryptedKeystore::encrypt(&w, "swordfish", ScryptParams::light()).unwrap();
        let err = ks.decrypt("wrong").err().unwrap();
        assert!(matches!(err, KeystoreError::BadMac), "got {err:?}");
    }

    #[test]
    fn json_round_trip() {
        let w = fixed_wallet();
        let ks = EncryptedKeystore::encrypt(&w, "pw", ScryptParams::light()).unwrap();
        let json = ks.to_json();
        let parsed = EncryptedKeystore::from_json(&json).unwrap();
        let recovered = parsed.decrypt("pw").unwrap();
        assert_eq!(recovered.address(), w.address());
    }

    #[test]
    fn parsed_address_matches_wallet() {
        let w = fixed_wallet();
        let ks = EncryptedKeystore::encrypt(&w, "pw", ScryptParams::light()).unwrap();
        assert_eq!(ks.parsed_address().unwrap(), w.address());
    }

    #[test]
    fn distinct_encryptions_produce_distinct_ciphertexts() {
        // Same secret + same password should still yield distinct ciphertexts
        // because salt + iv are randomised every time.
        let w = fixed_wallet();
        let a = EncryptedKeystore::encrypt(&w, "pw", ScryptParams::light()).unwrap();
        let b = EncryptedKeystore::encrypt(&w, "pw", ScryptParams::light()).unwrap();
        assert_ne!(a.crypto.ciphertext, b.crypto.ciphertext);
        assert_ne!(a.crypto.cipherparams.iv, b.crypto.cipherparams.iv);
        assert_ne!(a.crypto.kdfparams.salt, b.crypto.kdfparams.salt);
    }

    #[test]
    fn keystore_serializes_to_v3() {
        let w = fixed_wallet();
        let ks = EncryptedKeystore::encrypt(&w, "pw", ScryptParams::light()).unwrap();
        let json: serde_json::Value = serde_json::from_str(&ks.to_json()).unwrap();
        assert_eq!(json["version"], 3);
        assert_eq!(json["crypto"]["cipher"], "aes-128-ctr");
        assert_eq!(json["crypto"]["kdf"], "scrypt");
        assert_eq!(json["crypto"]["kdfparams"]["dklen"], 32);
    }

    #[test]
    fn rejects_unsupported_version() {
        let w = fixed_wallet();
        let mut ks = EncryptedKeystore::encrypt(&w, "pw", ScryptParams::light()).unwrap();
        ks.version = 1;
        let err = ks.decrypt("pw").err().unwrap();
        assert!(matches!(err, KeystoreError::UnsupportedVersion(1)));
    }

    #[test]
    fn rejects_corrupted_ciphertext() {
        let w = fixed_wallet();
        let mut ks = EncryptedKeystore::encrypt(&w, "pw", ScryptParams::light()).unwrap();
        // Flip the first byte of ciphertext — MAC must catch it.
        let mut bytes = hex::decode(&ks.crypto.ciphertext).unwrap();
        bytes[0] ^= 0xff;
        ks.crypto.ciphertext = hex::encode(bytes);
        let err = ks.decrypt("pw").err().unwrap();
        assert!(matches!(err, KeystoreError::BadMac));
    }

    #[test]
    fn encrypted_bytes_roundtrips_arbitrary_payload() {
        let payload = b"validator-secret-128-byte-blob".to_vec();
        let enc = EncryptedBytes::encrypt(&payload, "hunter2", ScryptParams::light(), "aii-test")
            .unwrap();
        let dec = enc.decrypt("hunter2").unwrap();
        assert_eq!(dec, payload);
    }

    #[test]
    fn encrypted_bytes_wrong_password_fails_mac() {
        let payload = vec![1u8, 2, 3, 4];
        let enc = EncryptedBytes::encrypt(&payload, "right", ScryptParams::light(), "x").unwrap();
        let err = enc.decrypt("wrong").err().unwrap();
        assert!(matches!(err, KeystoreError::BadMac));
    }

    #[test]
    fn encrypted_bytes_json_round_trip() {
        let payload = b"another-payload".to_vec();
        let enc = EncryptedBytes::encrypt(&payload, "pw", ScryptParams::light(), "x").unwrap();
        let json = enc.to_json();
        let back = EncryptedBytes::from_json(&json).unwrap();
        let dec = back.decrypt("pw").unwrap();
        assert_eq!(dec, payload);
    }
}
