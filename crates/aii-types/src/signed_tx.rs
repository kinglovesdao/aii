//! SignedTx — generic signed-transaction envelope.
//!
//! A `SignedTx` is the wire-format unit consumed by the mempool and the
//! consensus engine. It is intentionally agnostic to the signature algorithm
//! used: `algo_id` tells `aii-registry` (later plan) which verifier to
//! invoke; `pubkey` and `signature` are opaque byte vectors sized per algo.
//!
//! Implements spec decision D7 and D9 (account abstraction at the algorithm
//! level — same wire format works for secp256k1 EOAs, BLS validators, and
//! future PQ schemes).

use crate::algo::AlgoId;
use serde::{Deserialize, Serialize};

/// Signed-transaction envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedTx {
    /// Algorithm used to sign `payload`.
    pub algo_id: AlgoId,
    /// Public key whose private counterpart produced `signature`.
    pub pubkey: Vec<u8>,
    /// Signature over `payload`. Size depends on `algo_id`.
    pub signature: Vec<u8>,
    /// Opaque transaction payload (RLP-encoded; decoded later by `aii-codec`).
    pub payload: Vec<u8>,
}

impl SignedTx {
    /// Construct a new envelope.
    pub fn new(algo_id: AlgoId, pubkey: Vec<u8>, signature: Vec<u8>, payload: Vec<u8>) -> Self {
        Self {
            algo_id,
            pubkey,
            signature,
            payload,
        }
    }

    /// Total wire size: 1 (algo_id) + len(pubkey) + len(signature) + len(payload).
    pub fn wire_size(&self) -> usize {
        1 + self.pubkey.len() + self.signature.len() + self.payload.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_secp256k1_tx() -> SignedTx {
        SignedTx::new(
            AlgoId::Secp256k1,
            vec![0xAA; 33],
            vec![0xBB; 65],
            vec![0xCC; 100],
        )
    }

    #[test]
    fn signed_tx_holds_all_fields() {
        let tx = dummy_secp256k1_tx();
        assert_eq!(tx.algo_id, AlgoId::Secp256k1);
        assert_eq!(tx.pubkey.len(), 33);
        assert_eq!(tx.signature.len(), 65);
        assert_eq!(tx.payload.len(), 100);
    }

    #[test]
    fn wire_size_sums_components() {
        let tx = dummy_secp256k1_tx();
        assert_eq!(tx.wire_size(), 199);
    }

    #[test]
    fn signed_tx_equality_compares_all_fields() {
        let a = dummy_secp256k1_tx();
        let b = dummy_secp256k1_tx();
        assert_eq!(a, b);

        let mut c = a.clone();
        c.algo_id = AlgoId::Ed25519;
        assert_ne!(a, c);
    }

    #[test]
    fn different_algo_id_in_same_struct() {
        let pq_tx = SignedTx::new(AlgoId::MlDsa65, vec![0x00; 1952], vec![0x00; 3309], vec![]);
        assert!(pq_tx.algo_id.quantum_safe());
        assert_eq!(pq_tx.wire_size(), 1 + 1952 + 3309);
    }
}
