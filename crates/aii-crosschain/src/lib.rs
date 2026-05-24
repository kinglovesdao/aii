//! # aii-crosschain
//!
//! Cross-chain primitives for AII (v0.0.18 — scoped HTLC).
//!
//! Hash Time-Locked Contracts (HTLCs) are the building block for
//! trustless atomic swaps between AII and external chains. A sender
//! locks funds against a secret hash; the recipient claims by revealing
//! the preimage before the timelock expires, otherwise the sender
//! refunds after expiry.
//!
//! ## Scope
//!
//! This crate provides the **on-chain state machine** only:
//!
//! - [`HtlcState`] — `Locked` → `Claimed` / `Refunded`
//! - [`Htlc`] — the lock record + transition rules
//! - [`htlc_id`] — content-addressed lock identifier
//!
//! Multi-sig bridges (Aii ↔ Ethereum federation), IBC light clients,
//! and full Polkadot XCM adapters are explicit non-goals here. They
//! will land in later releases that build on this state machine.
//!
//! ## State machine
//!
//! ```text
//!                  ┌──────────────────────────┐
//!                  │       Locked             │
//!                  └────┬──────────────┬──────┘
//!     claim(preimage)   │              │   refund() iff now ≥ timeout
//!     iff keccak(p)==h  │              │
//!                       ▼              ▼
//!                  ┌─────────┐     ┌──────────┐
//!                  │ Claimed │     │ Refunded │
//!                  └─────────┘     └──────────┘
//! ```
//!
//! Claim and Refund are terminal. Either-side double-spend is rejected.
//!
//! ## Hash function
//!
//! AII uses Keccak-256 throughout (per design doc §3.1). Cross-chain
//! peers that prefer SHA-256 (e.g. Bitcoin / classic HTLCs) MUST agree
//! on the digest in their swap protocol; this crate only validates
//! Keccak-256.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use aii_crypto::keccak256;
use aii_types::{Address, H256, U256};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Lifecycle state of an [`Htlc`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HtlcState {
    /// Funds locked, awaiting preimage reveal or timeout.
    Locked,
    /// Preimage revealed, funds transferred to recipient.
    Claimed,
    /// Timeout reached, funds returned to sender.
    Refunded,
}

/// Hash Time-Locked Contract record.
///
/// `timeout` is a unix-style monotonic timestamp in seconds. The
/// caller is responsible for plumbing a clock — this crate is pure
/// logic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Htlc {
    /// Sender (refund beneficiary).
    pub sender: Address,
    /// Recipient (claim beneficiary).
    pub recipient: Address,
    /// Amount locked, in the smallest unit (wei-equivalent).
    pub amount: U256,
    /// `keccak256(preimage)` — preimage is revealed to claim.
    pub secret_hash: H256,
    /// Earliest timestamp at which `refund` is permitted.
    pub timeout: u64,
    /// Current state.
    pub state: HtlcState,
}

impl Htlc {
    /// Create a new HTLC in the `Locked` state.
    ///
    /// Returns `Err(HtlcError::AmountZero)` if `amount == 0`, or
    /// `Err(HtlcError::SelfLock)` if `sender == recipient`.
    pub fn new(
        sender: Address,
        recipient: Address,
        amount: U256,
        secret_hash: H256,
        timeout: u64,
    ) -> Result<Self, HtlcError> {
        if amount == U256::ZERO {
            return Err(HtlcError::AmountZero);
        }
        if sender == recipient {
            return Err(HtlcError::SelfLock);
        }
        Ok(Self {
            sender,
            recipient,
            amount,
            secret_hash,
            timeout,
            state: HtlcState::Locked,
        })
    }

    /// Attempt to claim the lock by revealing `preimage`.
    ///
    /// Transitions `Locked` → `Claimed` iff `keccak256(preimage) == secret_hash`.
    ///
    /// `now` is passed so callers can integrate any clock source; the
    /// claim path itself is **not** time-gated — only refund is.
    pub fn claim(&mut self, preimage: &[u8]) -> Result<(), HtlcError> {
        if self.state != HtlcState::Locked {
            return Err(HtlcError::NotLocked(self.state));
        }
        let h = keccak256(preimage);
        if h != self.secret_hash {
            return Err(HtlcError::WrongPreimage);
        }
        self.state = HtlcState::Claimed;
        Ok(())
    }

    /// Attempt to refund the lock to the sender.
    ///
    /// Transitions `Locked` → `Refunded` iff `now >= timeout`.
    pub fn refund(&mut self, now: u64) -> Result<(), HtlcError> {
        if self.state != HtlcState::Locked {
            return Err(HtlcError::NotLocked(self.state));
        }
        if now < self.timeout {
            return Err(HtlcError::TooEarly {
                now,
                timeout: self.timeout,
            });
        }
        self.state = HtlcState::Refunded;
        Ok(())
    }

    /// True iff the HTLC is in `Locked` state.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        matches!(self.state, HtlcState::Locked)
    }
}

/// Deterministic content-addressed identifier for an HTLC.
///
/// `id = keccak256(sender ‖ recipient ‖ amount_be32 ‖ secret_hash ‖ timeout_be8)`.
///
/// Stable across nodes — two parties that build the same HTLC always
/// derive the same `id`, which is what lets cross-chain protocols
/// reference the lock without an extra index.
#[must_use]
pub fn htlc_id(htlc: &Htlc) -> H256 {
    let mut buf = Vec::with_capacity(20 + 20 + 32 + 32 + 8);
    buf.extend_from_slice(htlc.sender.as_bytes());
    buf.extend_from_slice(htlc.recipient.as_bytes());
    buf.extend_from_slice(&htlc.amount.to_be_bytes::<32>());
    buf.extend_from_slice(htlc.secret_hash.as_bytes());
    buf.extend_from_slice(&htlc.timeout.to_be_bytes());
    keccak256(&buf)
}

/// Errors produced by HTLC transitions.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum HtlcError {
    /// Locked amount must be non-zero.
    #[error("HTLC amount must be > 0")]
    AmountZero,

    /// Sender and recipient cannot be the same address.
    #[error("HTLC sender and recipient must differ")]
    SelfLock,

    /// Tried to claim/refund a non-`Locked` HTLC.
    #[error("HTLC is not in Locked state (current: {0:?})")]
    NotLocked(HtlcState),

    /// `keccak256(preimage)` did not match `secret_hash`.
    #[error("preimage does not match secret hash")]
    WrongPreimage,

    /// `refund` called before `timeout`.
    #[error("refund too early: now={now} < timeout={timeout}")]
    TooEarly {
        /// Caller-supplied clock value.
        now: u64,
        /// Timeout configured on the HTLC.
        timeout: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alice() -> Address {
        Address::new([0xaa; 20])
    }
    fn bob() -> Address {
        Address::new([0xbb; 20])
    }

    fn fixture() -> (Htlc, Vec<u8>) {
        let preimage = b"a-secret-known-only-to-the-recipient".to_vec();
        let secret_hash = keccak256(&preimage);
        let htlc = Htlc::new(alice(), bob(), U256::from(1_000u64), secret_hash, 100).unwrap();
        (htlc, preimage)
    }

    #[test]
    fn new_rejects_zero_amount() {
        let err = Htlc::new(alice(), bob(), U256::ZERO, H256::ZERO, 10).unwrap_err();
        assert_eq!(err, HtlcError::AmountZero);
    }

    #[test]
    fn new_rejects_self_lock() {
        let err = Htlc::new(alice(), alice(), U256::from(1u64), H256::ZERO, 10).unwrap_err();
        assert_eq!(err, HtlcError::SelfLock);
    }

    #[test]
    fn new_creates_locked_state() {
        let (h, _) = fixture();
        assert_eq!(h.state, HtlcState::Locked);
        assert!(h.is_open());
    }

    #[test]
    fn claim_with_correct_preimage_succeeds() {
        let (mut h, p) = fixture();
        h.claim(&p).unwrap();
        assert_eq!(h.state, HtlcState::Claimed);
        assert!(!h.is_open());
    }

    #[test]
    fn claim_with_wrong_preimage_rejected() {
        let (mut h, _) = fixture();
        let err = h.claim(b"wrong-secret").unwrap_err();
        assert_eq!(err, HtlcError::WrongPreimage);
        assert_eq!(
            h.state,
            HtlcState::Locked,
            "state must not change on bad claim"
        );
    }

    #[test]
    fn double_claim_rejected() {
        let (mut h, p) = fixture();
        h.claim(&p).unwrap();
        let err = h.claim(&p).unwrap_err();
        assert_eq!(err, HtlcError::NotLocked(HtlcState::Claimed));
    }

    #[test]
    fn refund_before_timeout_rejected() {
        let (mut h, _) = fixture();
        let err = h.refund(99).unwrap_err();
        assert_eq!(
            err,
            HtlcError::TooEarly {
                now: 99,
                timeout: 100,
            }
        );
        assert_eq!(h.state, HtlcState::Locked);
    }

    #[test]
    fn refund_at_or_after_timeout_succeeds() {
        let (mut h, _) = fixture();
        h.refund(100).unwrap();
        assert_eq!(h.state, HtlcState::Refunded);

        let (mut h2, _) = fixture();
        h2.refund(u64::MAX).unwrap();
        assert_eq!(h2.state, HtlcState::Refunded);
    }

    #[test]
    fn refund_after_claim_rejected() {
        let (mut h, p) = fixture();
        h.claim(&p).unwrap();
        let err = h.refund(1_000_000).unwrap_err();
        assert_eq!(err, HtlcError::NotLocked(HtlcState::Claimed));
    }

    #[test]
    fn claim_after_refund_rejected() {
        let (mut h, p) = fixture();
        h.refund(100).unwrap();
        let err = h.claim(&p).unwrap_err();
        assert_eq!(err, HtlcError::NotLocked(HtlcState::Refunded));
    }

    #[test]
    fn htlc_id_is_deterministic() {
        let (h1, _) = fixture();
        let (h2, _) = fixture();
        assert_eq!(htlc_id(&h1), htlc_id(&h2));
    }

    #[test]
    fn htlc_id_changes_with_any_field() {
        let (base, _) = fixture();
        let base_id = htlc_id(&base);

        let mut other = base.clone();
        other.amount = U256::from(2_000u64);
        assert_ne!(htlc_id(&other), base_id);

        let mut other = base.clone();
        other.timeout = 101;
        assert_ne!(htlc_id(&other), base_id);

        let mut other = base.clone();
        other.recipient = Address::new([0xcc; 20]);
        assert_ne!(htlc_id(&other), base_id);

        let mut other = base;
        other.secret_hash = H256::new([0x42; 32]);
        assert_ne!(htlc_id(&other), base_id);
    }

    #[test]
    fn state_does_not_affect_htlc_id() {
        let (mut h, p) = fixture();
        let id_before = htlc_id(&h);
        h.claim(&p).unwrap();
        let id_after = htlc_id(&h);
        assert_eq!(
            id_before, id_after,
            "id is content-addressed by the lock, not its lifecycle"
        );
    }

    #[test]
    fn known_keccak_round_trip() {
        // sanity check that we use the same keccak the test fixture does
        let h = keccak256(b"");
        // keccak256("") = 0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470
        assert_eq!(
            hex::encode(h.as_bytes()),
            "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
        );
    }
}
