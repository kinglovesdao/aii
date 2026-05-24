//! Federated multisig bridge `Vault` (v0.0.21).
//!
//! A `FederationSet` is `n` BLS public keys plus a threshold `t ≤ n`.
//! To release funds, the federation signs a `LockReceipt` (proof that
//! the asset has been locked on the source chain); a `Vault` accepts
//! the release iff
//!
//!  1. at least `t` distinct signers (via `signer_bitmap`) participated,
//!  2. the BLS aggregated signature verifies against the selected
//!     subset of pubkeys over `receipt.digest()`,
//!  3. the receipt's `nonce` has not been used before.
//!
//! This module is the **on-chain state machine** only — it does NOT:
//!
//! - listen to source chains,
//! - move assets (the caller handles transfer on a successful release),
//! - rotate the federation set (static for v0.0.21; rotation is a later
//!   release),
//! - run an off-chain aggregator daemon.
//!
//! ## Signing format
//!
//! All signers sign the same 32-byte `receipt.digest()`. The digest is
//! domain-separated by the federation's content-addressed `id`, so a
//! signature produced for one federation can never be replayed against
//! another federation (even one sharing pubkeys with a different
//! threshold).
//!
//! ## Federation size cap
//!
//! `signer_bitmap` is a `u64`, so `n ≤ 64`. Practical federation
//! deployments are usually 5–21 signers; a 64-cap is comfortable.

use aii_crypto::bls::{self, PublicKey, Signature};
use aii_crypto::keccak256;
use aii_types::{Address, H256, U256};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use thiserror::Error;

/// Maximum federation size enforced by the `u64` signer bitmap.
pub const MAX_FEDERATION_SIZE: usize = 64;

/// A static set of BLS validators with a `t`-of-`n` release threshold.
#[derive(Clone, Debug)]
pub struct FederationSet {
    pubkeys: Vec<PublicKey>,
    threshold: usize,
}

impl FederationSet {
    /// Build a new federation set.
    ///
    /// Rejects:
    /// - `pubkeys.is_empty()` → [`BridgeError::EmptyFederation`]
    /// - `pubkeys.len() > MAX_FEDERATION_SIZE` → [`BridgeError::FederationTooLarge`]
    /// - `threshold == 0 || threshold > pubkeys.len()` →
    ///   [`BridgeError::InvalidThreshold`]
    pub fn new(pubkeys: Vec<PublicKey>, threshold: usize) -> Result<Self, BridgeError> {
        if pubkeys.is_empty() {
            return Err(BridgeError::EmptyFederation);
        }
        if pubkeys.len() > MAX_FEDERATION_SIZE {
            return Err(BridgeError::FederationTooLarge {
                got: pubkeys.len(),
                max: MAX_FEDERATION_SIZE,
            });
        }
        if threshold == 0 || threshold > pubkeys.len() {
            return Err(BridgeError::InvalidThreshold {
                threshold,
                federation_size: pubkeys.len(),
            });
        }
        Ok(Self { pubkeys, threshold })
    }

    /// Total number of validators in this set.
    #[must_use]
    pub fn size(&self) -> usize {
        self.pubkeys.len()
    }

    /// Release threshold (`t` in `t`-of-`n`).
    #[must_use]
    pub const fn threshold(&self) -> usize {
        self.threshold
    }

    /// Borrow the validators' public keys in canonical order.
    #[must_use]
    pub fn pubkeys(&self) -> &[PublicKey] {
        &self.pubkeys
    }

    /// Content-addressed identifier:
    /// `keccak256(threshold_be8 ‖ pubkey1_compressed ‖ ... ‖ pubkeyN_compressed)`.
    ///
    /// Two federations with the same threshold and the same pubkeys in
    /// the same order have the same id. Reordering pubkeys changes the
    /// id — the order is part of the public commitment.
    #[must_use]
    pub fn id(&self) -> H256 {
        let mut buf = Vec::with_capacity(8 + self.pubkeys.len() * 48);
        buf.extend_from_slice(&(self.threshold as u64).to_be_bytes());
        for pk in &self.pubkeys {
            buf.extend_from_slice(&pk.to_compressed());
        }
        keccak256(&buf)
    }
}

/// Proof that an asset has been locked on the source chain.
///
/// The federation signs over `receipt.digest()` to authorise release on
/// this chain. The actual asset transfer is the caller's responsibility
/// — `Vault::release` returns the receipt on success and the caller
/// performs the transfer (mint / unlock / etc.).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockReceipt {
    /// EIP-155–style chain id of the source chain (e.g. 1 for Ethereum).
    pub src_chain_id: u64,
    /// Asset identifier on the source chain (token contract, or all-zero
    /// for native asset).
    pub asset: H256,
    /// Locked amount in the asset's smallest unit.
    pub amount: U256,
    /// Address to receive the released asset on this chain.
    pub recipient: Address,
    /// Per-federation monotonic nonce; the `Vault` rejects reuse.
    pub nonce: u64,
}

impl LockReceipt {
    /// 32-byte digest the federation signs over.
    ///
    /// `keccak256(federation_id ‖ src_chain_id_be8 ‖ asset ‖ amount_be32 ‖ recipient ‖ nonce_be8)`.
    ///
    /// The `federation_id` prefix domain-separates digests across
    /// different federation sets — a signature produced for federation
    /// A cannot release the same receipt at federation B's vault.
    #[must_use]
    pub fn digest(&self, federation_id: &H256) -> H256 {
        let mut buf = Vec::with_capacity(32 + 8 + 32 + 32 + 20 + 8);
        buf.extend_from_slice(federation_id.as_bytes());
        buf.extend_from_slice(&self.src_chain_id.to_be_bytes());
        buf.extend_from_slice(self.asset.as_bytes());
        buf.extend_from_slice(&self.amount.to_be_bytes::<32>());
        buf.extend_from_slice(self.recipient.as_bytes());
        buf.extend_from_slice(&self.nonce.to_be_bytes());
        keccak256(&buf)
    }
}

/// A federation-signed authorisation to release one lock.
#[derive(Clone, Debug)]
pub struct AttestationBundle {
    /// The lock being released.
    pub receipt: LockReceipt,
    /// Aggregated BLS signature over `receipt.digest(federation_id)`.
    pub aggregated_sig: Signature,
    /// Bit `i` set iff validator `i` participated. Bits ≥ federation
    /// size MUST be zero.
    pub signer_bitmap: u64,
}

/// Outcome of a successful release — the caller transfers the asset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Released {
    /// The receipt whose lock was just released.
    pub receipt: LockReceipt,
}

/// Federation-backed release vault. Stateful — tracks consumed nonces
/// to prevent replay.
pub struct Vault {
    federation: FederationSet,
    used_nonces: BTreeSet<u64>,
}

impl Vault {
    /// Construct a fresh vault bound to `federation`.
    pub const fn new(federation: FederationSet) -> Self {
        Self {
            federation,
            used_nonces: BTreeSet::new(),
        }
    }

    /// Borrow the federation set bound to this vault.
    pub const fn federation(&self) -> &FederationSet {
        &self.federation
    }

    /// Attempt to release the lock described by `bundle`.
    ///
    /// Validation order (first match wins, state unchanged on failure):
    ///
    /// 1. **Bitmap bounds** — bit set ≥ federation size →
    ///    [`BridgeError::SignerBitmapOversized`].
    /// 2. **Threshold** — popcount < threshold →
    ///    [`BridgeError::BelowThreshold`].
    /// 3. **Replay** — nonce previously released →
    ///    [`BridgeError::NonceReused`].
    /// 4. **BLS** — aggregated signature does not verify against the
    ///    selected signer subset → [`BridgeError::InvalidAggregateSig`].
    ///
    /// On success, the nonce is recorded and [`Released`] is returned
    /// — the caller performs the actual asset transfer.
    pub fn release(&mut self, bundle: &AttestationBundle) -> Result<Released, BridgeError> {
        let n = self.federation.size();

        // 1. Bitmap bounds.
        if bundle.signer_bitmap != 0 {
            let highest_bit = bundle.signer_bitmap.ilog2();
            if (highest_bit as usize) >= n {
                return Err(BridgeError::SignerBitmapOversized {
                    highest_bit,
                    federation_size: n,
                });
            }
        }

        // 2. Threshold.
        let signers = bundle.signer_bitmap.count_ones() as usize;
        if signers < self.federation.threshold {
            return Err(BridgeError::BelowThreshold {
                signers,
                threshold: self.federation.threshold,
            });
        }

        // 3. Replay.
        if self.used_nonces.contains(&bundle.receipt.nonce) {
            return Err(BridgeError::NonceReused(bundle.receipt.nonce));
        }

        // 4. BLS aggregate verify over the signer subset.
        let fed_id = self.federation.id();
        let digest = bundle.receipt.digest(&fed_id);
        let signer_pks: Vec<PublicKey> = (0..n)
            .filter(|i| (bundle.signer_bitmap >> i) & 1 == 1)
            .map(|i| self.federation.pubkeys[i].clone())
            .collect();
        bls::fast_aggregate_verify(&bundle.aggregated_sig, digest.as_bytes(), &signer_pks)
            .map_err(|_| BridgeError::InvalidAggregateSig)?;

        // 5. Commit.
        self.used_nonces.insert(bundle.receipt.nonce);
        Ok(Released {
            receipt: bundle.receipt.clone(),
        })
    }
}

/// Errors produced by federation construction and `Vault::release`.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum BridgeError {
    /// `FederationSet::new` called with an empty pubkey list.
    #[error("federation must have at least one validator")]
    EmptyFederation,

    /// `FederationSet::new` called with too many pubkeys.
    #[error("federation size {got} exceeds maximum {max}")]
    FederationTooLarge {
        /// Pubkeys supplied.
        got: usize,
        /// Maximum permitted.
        max: usize,
    },

    /// `threshold == 0` or `threshold > pubkeys.len()`.
    #[error("invalid threshold {threshold} for federation of size {federation_size}")]
    InvalidThreshold {
        /// Requested threshold.
        threshold: usize,
        /// Federation size at construction.
        federation_size: usize,
    },

    /// `signer_bitmap` has bits set beyond the federation size.
    #[error("signer bitmap has bit {highest_bit} set, federation size is only {federation_size}")]
    SignerBitmapOversized {
        /// Highest-set bit position.
        highest_bit: u32,
        /// Federation size.
        federation_size: usize,
    },

    /// Fewer than `threshold` signers participated.
    #[error("only {signers} signed, threshold is {threshold}")]
    BelowThreshold {
        /// Signers indicated by the bitmap.
        signers: usize,
        /// Threshold required by the federation.
        threshold: usize,
    },

    /// `fast_aggregate_verify` rejected the bundle.
    #[error("aggregated signature failed to verify against selected signer subset")]
    InvalidAggregateSig,

    /// A bundle for a previously-released nonce was submitted.
    #[error("nonce {0} already released")]
    NonceReused(u64),
}

#[cfg(test)]
mod tests {
    use super::*;
    use aii_crypto::bls::{aggregate_signatures, SecretKey};

    fn sk_from_seed(seed: u8) -> SecretKey {
        let ikm = [seed; 32];
        SecretKey::from_ikm(&ikm, b"AII-CROSSCHAIN-TEST").unwrap()
    }

    /// Build a federation of `n` validators with the given `threshold`.
    /// Returns the set along with the secret keys for test signing.
    fn make_federation(n: u8, threshold: usize) -> (FederationSet, Vec<SecretKey>) {
        let sks: Vec<SecretKey> = (0..n).map(|i| sk_from_seed(i + 1)).collect();
        let pks: Vec<PublicKey> = sks.iter().map(SecretKey::public_key).collect();
        (FederationSet::new(pks, threshold).unwrap(), sks)
    }

    fn sample_receipt(nonce: u64) -> LockReceipt {
        LockReceipt {
            src_chain_id: 1,
            asset: H256::new([0xee; 32]),
            amount: U256::from(1_000u64),
            recipient: Address::new([0xab; 20]),
            nonce,
        }
    }

    /// Build a valid attestation bundle from the given signer indices.
    fn sign_with(
        federation: &FederationSet,
        sks: &[SecretKey],
        signer_indices: &[usize],
        receipt: LockReceipt,
    ) -> AttestationBundle {
        let digest = receipt.digest(&federation.id());
        let sigs: Vec<Signature> = signer_indices
            .iter()
            .map(|&i| sks[i].sign(digest.as_bytes()))
            .collect();
        let agg = aggregate_signatures(&sigs).unwrap();
        let bitmap = signer_indices
            .iter()
            .fold(0u64, |acc, &i| acc | (1u64 << i));
        AttestationBundle {
            receipt,
            aggregated_sig: agg,
            signer_bitmap: bitmap,
        }
    }

    #[test]
    fn federation_new_empty_rejected() {
        let err = FederationSet::new(vec![], 1).unwrap_err();
        assert_eq!(err, BridgeError::EmptyFederation);
    }

    #[test]
    fn federation_new_threshold_zero_rejected() {
        let pk = sk_from_seed(1).public_key();
        let err = FederationSet::new(vec![pk], 0).unwrap_err();
        assert_eq!(
            err,
            BridgeError::InvalidThreshold {
                threshold: 0,
                federation_size: 1,
            }
        );
    }

    #[test]
    fn federation_new_threshold_exceeds_size_rejected() {
        let pk = sk_from_seed(1).public_key();
        let err = FederationSet::new(vec![pk], 2).unwrap_err();
        assert_eq!(
            err,
            BridgeError::InvalidThreshold {
                threshold: 2,
                federation_size: 1,
            }
        );
    }

    #[test]
    fn federation_too_large_rejected() {
        let pks: Vec<PublicKey> = (0..=MAX_FEDERATION_SIZE)
            .map(|i| sk_from_seed(u8::try_from(i).unwrap_or(255)).public_key())
            .collect();
        let err = FederationSet::new(pks, 1).unwrap_err();
        assert_eq!(
            err,
            BridgeError::FederationTooLarge {
                got: MAX_FEDERATION_SIZE + 1,
                max: MAX_FEDERATION_SIZE,
            }
        );
    }

    #[test]
    fn federation_id_is_content_addressed() {
        let (a, _) = make_federation(3, 2);
        let (b, _) = make_federation(3, 2);
        assert_eq!(a.id(), b.id(), "same pubkeys + threshold → same id");
    }

    #[test]
    fn federation_id_depends_on_threshold() {
        let (a, _) = make_federation(3, 2);
        let (b, _) = make_federation(3, 3);
        assert_ne!(
            a.id(),
            b.id(),
            "same pubkeys but different threshold must differ"
        );
    }

    #[test]
    fn lock_receipt_digest_is_deterministic() {
        let r = sample_receipt(42);
        let fid = H256::new([0x33; 32]);
        assert_eq!(r.digest(&fid), r.digest(&fid));
    }

    #[test]
    fn release_with_valid_2_of_3_signature_succeeds() {
        let (fed, sks) = make_federation(3, 2);
        let mut vault = Vault::new(fed.clone());
        let bundle = sign_with(&fed, &sks, &[0, 2], sample_receipt(7));
        let out = vault.release(&bundle).unwrap();
        assert_eq!(out.receipt.nonce, 7);
    }

    #[test]
    fn release_below_threshold_rejected() {
        let (fed, sks) = make_federation(3, 2);
        let mut vault = Vault::new(fed.clone());
        let bundle = sign_with(&fed, &sks, &[1], sample_receipt(8));
        let err = vault.release(&bundle).unwrap_err();
        assert_eq!(
            err,
            BridgeError::BelowThreshold {
                signers: 1,
                threshold: 2,
            }
        );
    }

    #[test]
    fn release_with_invalid_aggregate_sig_rejected() {
        let (fed, sks) = make_federation(3, 2);
        let mut vault = Vault::new(fed.clone());
        // Sign over the WRONG digest, then claim it's a bundle for nonce=9.
        let wrong = sample_receipt(999);
        let real = sample_receipt(9);
        let wrong_bundle = sign_with(&fed, &sks, &[0, 1], wrong);
        let forged = AttestationBundle {
            receipt: real,
            aggregated_sig: wrong_bundle.aggregated_sig,
            signer_bitmap: wrong_bundle.signer_bitmap,
        };
        let err = vault.release(&forged).unwrap_err();
        assert_eq!(err, BridgeError::InvalidAggregateSig);
    }

    #[test]
    fn nonce_replay_rejected() {
        let (fed, sks) = make_federation(3, 2);
        let mut vault = Vault::new(fed.clone());
        let bundle = sign_with(&fed, &sks, &[0, 1], sample_receipt(10));
        vault.release(&bundle).unwrap();
        let err = vault.release(&bundle).unwrap_err();
        assert_eq!(err, BridgeError::NonceReused(10));
    }

    #[test]
    fn two_different_nonces_both_release() {
        let (fed, sks) = make_federation(3, 2);
        let mut vault = Vault::new(fed.clone());
        let b1 = sign_with(&fed, &sks, &[0, 1], sample_receipt(11));
        let b2 = sign_with(&fed, &sks, &[1, 2], sample_receipt(12));
        vault.release(&b1).unwrap();
        vault.release(&b2).unwrap();
    }

    #[test]
    fn signer_bitmap_oversized_rejected() {
        let (fed, sks) = make_federation(3, 2);
        let mut vault = Vault::new(fed.clone());
        let valid = sign_with(&fed, &sks, &[0, 1], sample_receipt(13));
        let bad = AttestationBundle {
            receipt: valid.receipt,
            aggregated_sig: valid.aggregated_sig,
            signer_bitmap: valid.signer_bitmap | (1u64 << 5), // bit 5 ≥ size=3
        };
        let err = vault.release(&bad).unwrap_err();
        assert_eq!(
            err,
            BridgeError::SignerBitmapOversized {
                highest_bit: 5,
                federation_size: 3,
            }
        );
    }
}
