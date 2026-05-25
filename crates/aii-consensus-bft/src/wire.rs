//! BFT-PoS stage 4: wire-format codec for consensus messages (v0.0.27).
//!
//! [`BftMessage`] is the typed envelope a validator emits / receives
//! over the network. Encoding is a fixed-layout byte packing — no RLP,
//! no SSZ — so a malformed message is detected by length alone before
//! any cryptographic check runs. Every variant has the same first byte
//! (the tag) so a peer can route without decoding the rest.
//!
//! ## On-the-wire layout
//!
//! | Variant     | Bytes | Layout |
//! |-------------|-------|--------|
//! | `Proposal`  | 173   | `0x00 ‖ height_be8 ‖ round_be4 ‖ block[32] ‖ vrf_preout[32] ‖ vrf_proof[64] ‖ vrf_output[32]` |
//! | `Prevote`   | 145   | `0x01 ‖ block[32] ‖ height_be8 ‖ round_be4 ‖ index_be4 ‖ bls_sig[96]` |
//! | `Precommit` | 145   | `0x02 ‖ block[32] ‖ height_be8 ‖ round_be4 ‖ index_be4 ‖ bls_sig[96]` |
//!
//! Decode validates:
//! 1. Buffer length matches the variant's expected size.
//! 2. BLS signature decompresses to a valid G2 point (rejects garbage).
//! 3. VRF pre-output and proof are accepted as raw bytes — semantic VRF
//!    verification happens later at the consumer (e.g. `LeaderProof::verify`).
//!
//! This module does NOT touch the network — it only knows how to turn
//! a typed message into bytes and back. Networking lands in a later
//! release alongside an actual gossip layer.

use aii_crypto::bls;
use aii_crypto::vrf::VrfProof;
use aii_types::H256;
use thiserror::Error;

use crate::bft::{LeaderProof, PrecommitVote, PrevoteVote};

/// Tag byte for [`BftMessage::Proposal`].
pub const TAG_PROPOSAL: u8 = 0x00;
/// Tag byte for [`BftMessage::Prevote`].
pub const TAG_PREVOTE: u8 = 0x01;
/// Tag byte for [`BftMessage::Precommit`].
pub const TAG_PRECOMMIT: u8 = 0x02;

/// Encoded size in bytes of a `Proposal` message.
pub const PROPOSAL_LEN: usize = 1 + 8 + 4 + 32 + 32 + 64 + 32;
/// Encoded size in bytes of a `Prevote` / `Precommit` message.
pub const VOTE_LEN: usize = 1 + 32 + 8 + 4 + 4 + 96;

/// One BFT consensus message — the unit of network exchange.
#[derive(Clone, Debug)]
pub enum BftMessage {
    /// Proposer announcing their proposal + VRF leader proof for the
    /// current `(height, round)`.
    Proposal {
        /// Height being proposed for.
        height: u64,
        /// Round being proposed in.
        round: u32,
        /// Block hash being proposed.
        block_hash: H256,
        /// Leader proof binding this proposer to `(height, round, seed)`.
        leader_proof: LeaderProof,
    },
    /// One validator's PRE-VOTE.
    Prevote(PrevoteVote),
    /// One validator's PRE-COMMIT.
    Precommit(PrecommitVote),
}

impl BftMessage {
    /// First byte of the encoding — usable for cheap routing.
    #[must_use]
    pub const fn tag(&self) -> u8 {
        match self {
            Self::Proposal { .. } => TAG_PROPOSAL,
            Self::Prevote(_) => TAG_PREVOTE,
            Self::Precommit(_) => TAG_PRECOMMIT,
        }
    }

    /// Encoded byte length for this message.
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        match self {
            Self::Proposal { .. } => PROPOSAL_LEN,
            Self::Prevote(_) | Self::Precommit(_) => VOTE_LEN,
        }
    }

    /// Encode to a fresh `Vec<u8>` in the layout documented at the
    /// module level.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(self.encoded_len());
        match self {
            Self::Proposal {
                height,
                round,
                block_hash,
                leader_proof,
            } => {
                buf.push(TAG_PROPOSAL);
                buf.extend_from_slice(&height.to_be_bytes());
                buf.extend_from_slice(&round.to_be_bytes());
                buf.extend_from_slice(block_hash.as_bytes());
                buf.extend_from_slice(&leader_proof.vrf_proof.pre_output);
                buf.extend_from_slice(&leader_proof.vrf_proof.proof);
                buf.extend_from_slice(&leader_proof.vrf_output);
            }
            Self::Prevote(v) => {
                buf.push(TAG_PREVOTE);
                encode_vote_body(&mut buf, v.block_hash, v.height, v.round, v.validator_index);
                buf.extend_from_slice(&v.bls_sig.to_compressed());
            }
            Self::Precommit(v) => {
                buf.push(TAG_PRECOMMIT);
                encode_vote_body(&mut buf, v.block_hash, v.height, v.round, v.validator_index);
                buf.extend_from_slice(&v.bls_sig.to_compressed());
            }
        }
        buf
    }

    /// Decode from the raw byte stream. Returns [`CodecError`] for any
    /// length, tag, or signature-point error; semantic checks (VRF
    /// validity, BLS aggregate verification) happen at higher layers.
    pub fn decode(bytes: &[u8]) -> Result<Self, CodecError> {
        let tag = *bytes.first().ok_or(CodecError::Empty)?;
        match tag {
            TAG_PROPOSAL => decode_proposal(bytes),
            TAG_PREVOTE => decode_vote(bytes, TAG_PREVOTE),
            TAG_PRECOMMIT => decode_vote(bytes, TAG_PRECOMMIT),
            other => Err(CodecError::UnknownTag(other)),
        }
    }
}

fn encode_vote_body(
    buf: &mut Vec<u8>,
    block_hash: H256,
    height: u64,
    round: u32,
    validator_index: u32,
) {
    buf.extend_from_slice(block_hash.as_bytes());
    buf.extend_from_slice(&height.to_be_bytes());
    buf.extend_from_slice(&round.to_be_bytes());
    buf.extend_from_slice(&validator_index.to_be_bytes());
}

fn decode_proposal(bytes: &[u8]) -> Result<BftMessage, CodecError> {
    if bytes.len() != PROPOSAL_LEN {
        return Err(CodecError::WrongLength {
            expected: PROPOSAL_LEN,
            got: bytes.len(),
        });
    }
    let mut cur = 1; // skip tag
    let height = u64::from_be_bytes(bytes[cur..cur + 8].try_into().unwrap());
    cur += 8;
    let round = u32::from_be_bytes(bytes[cur..cur + 4].try_into().unwrap());
    cur += 4;
    let block_hash = H256::new(bytes[cur..cur + 32].try_into().unwrap());
    cur += 32;
    let pre_output: [u8; 32] = bytes[cur..cur + 32].try_into().unwrap();
    cur += 32;
    let proof: [u8; 64] = bytes[cur..cur + 64].try_into().unwrap();
    cur += 64;
    let vrf_output: [u8; 32] = bytes[cur..cur + 32].try_into().unwrap();
    Ok(BftMessage::Proposal {
        height,
        round,
        block_hash,
        leader_proof: LeaderProof {
            vrf_proof: VrfProof { pre_output, proof },
            vrf_output,
        },
    })
}

fn decode_vote(bytes: &[u8], tag: u8) -> Result<BftMessage, CodecError> {
    if bytes.len() != VOTE_LEN {
        return Err(CodecError::WrongLength {
            expected: VOTE_LEN,
            got: bytes.len(),
        });
    }
    let mut cur = 1; // skip tag
    let block_hash = H256::new(bytes[cur..cur + 32].try_into().unwrap());
    cur += 32;
    let height = u64::from_be_bytes(bytes[cur..cur + 8].try_into().unwrap());
    cur += 8;
    let round = u32::from_be_bytes(bytes[cur..cur + 4].try_into().unwrap());
    cur += 4;
    let validator_index = u32::from_be_bytes(bytes[cur..cur + 4].try_into().unwrap());
    cur += 4;
    let sig_bytes: [u8; 96] = bytes[cur..cur + 96].try_into().unwrap();
    let bls_sig =
        bls::Signature::from_compressed(&sig_bytes).map_err(|_| CodecError::InvalidBlsSignature)?;
    if tag == TAG_PREVOTE {
        Ok(BftMessage::Prevote(PrevoteVote {
            block_hash,
            height,
            round,
            validator_index,
            bls_sig,
        }))
    } else {
        Ok(BftMessage::Precommit(PrecommitVote {
            block_hash,
            height,
            round,
            validator_index,
            bls_sig,
        }))
    }
}

/// Errors produced by [`BftMessage::decode`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CodecError {
    /// Buffer is empty.
    #[error("BFT message buffer is empty")]
    Empty,

    /// First byte is not one of [`TAG_PROPOSAL`] / [`TAG_PREVOTE`] /
    /// [`TAG_PRECOMMIT`].
    #[error("unknown BFT message tag 0x{0:02x}")]
    UnknownTag(u8),

    /// Encoded length does not match the variant's fixed size.
    #[error("BFT message wrong length: expected {expected}, got {got}")]
    WrongLength {
        /// Expected length given the tag.
        expected: usize,
        /// Length actually supplied.
        got: usize,
    },

    /// BLS signature bytes do not decompress to a valid G2 point.
    #[error("BLS signature decompression failed")]
    InvalidBlsSignature,
}

#[cfg(test)]
mod tests {
    use super::*;
    use aii_crypto::bls::SecretKey as BlsSecretKey;
    use aii_crypto::vrf::SecretKey as VrfSecretKey;

    fn bls_sk(seed: u8) -> BlsSecretKey {
        BlsSecretKey::from_ikm(&[seed; 32], b"AII-WIRE-TEST").unwrap()
    }

    fn vrf_sk() -> VrfSecretKey {
        VrfSecretKey::generate()
    }

    fn sample_prevote() -> PrevoteVote {
        let sk = bls_sk(1);
        PrevoteVote::sign(&sk, H256::new([0xab; 32]), 7, 0, 0)
    }

    fn sample_precommit() -> PrecommitVote {
        let sk = bls_sk(2);
        PrecommitVote::sign(&sk, H256::new([0xcd; 32]), 7, 3, 1)
    }

    fn sample_proposal() -> BftMessage {
        let sk = vrf_sk();
        let seed = [0x11; 32];
        let leader_proof = LeaderProof::produce(&sk, 7, 0, &seed);
        BftMessage::Proposal {
            height: 7,
            round: 0,
            block_hash: H256::new([0xee; 32]),
            leader_proof,
        }
    }

    #[test]
    fn proposal_tag_and_length() {
        let m = sample_proposal();
        assert_eq!(m.tag(), TAG_PROPOSAL);
        assert_eq!(m.encoded_len(), PROPOSAL_LEN);
        assert_eq!(PROPOSAL_LEN, 173);
    }

    #[test]
    fn prevote_tag_and_length() {
        let m = BftMessage::Prevote(sample_prevote());
        assert_eq!(m.tag(), TAG_PREVOTE);
        assert_eq!(m.encoded_len(), VOTE_LEN);
        assert_eq!(VOTE_LEN, 145);
    }

    #[test]
    fn precommit_tag_and_length() {
        let m = BftMessage::Precommit(sample_precommit());
        assert_eq!(m.tag(), TAG_PRECOMMIT);
        assert_eq!(m.encoded_len(), VOTE_LEN);
    }

    #[test]
    fn encode_proposal_produces_correct_length() {
        let bytes = sample_proposal().encode();
        assert_eq!(bytes.len(), PROPOSAL_LEN);
        assert_eq!(bytes[0], TAG_PROPOSAL);
    }

    #[test]
    fn encode_prevote_produces_correct_length() {
        let bytes = BftMessage::Prevote(sample_prevote()).encode();
        assert_eq!(bytes.len(), VOTE_LEN);
        assert_eq!(bytes[0], TAG_PREVOTE);
    }

    #[test]
    fn encode_precommit_produces_correct_length() {
        let bytes = BftMessage::Precommit(sample_precommit()).encode();
        assert_eq!(bytes.len(), VOTE_LEN);
        assert_eq!(bytes[0], TAG_PRECOMMIT);
    }

    #[test]
    fn proposal_round_trips() {
        let m = sample_proposal();
        let bytes = m.encode();
        let decoded = BftMessage::decode(&bytes).unwrap();
        match (m, decoded) {
            (
                BftMessage::Proposal {
                    height: h1,
                    round: r1,
                    block_hash: b1,
                    leader_proof: lp1,
                },
                BftMessage::Proposal {
                    height: h2,
                    round: r2,
                    block_hash: b2,
                    leader_proof: lp2,
                },
            ) => {
                assert_eq!(h1, h2);
                assert_eq!(r1, r2);
                assert_eq!(b1, b2);
                assert_eq!(lp1.vrf_proof, lp2.vrf_proof);
                assert_eq!(lp1.vrf_output, lp2.vrf_output);
            }
            _ => panic!("variant mismatch"),
        }
    }

    #[test]
    fn prevote_round_trips() {
        let v = sample_prevote();
        let bytes = BftMessage::Prevote(v.clone()).encode();
        let decoded = BftMessage::decode(&bytes).unwrap();
        let BftMessage::Prevote(out) = decoded else {
            panic!("variant mismatch");
        };
        assert_eq!(out.block_hash, v.block_hash);
        assert_eq!(out.height, v.height);
        assert_eq!(out.round, v.round);
        assert_eq!(out.validator_index, v.validator_index);
        assert_eq!(out.bls_sig.to_compressed(), v.bls_sig.to_compressed());
    }

    #[test]
    fn precommit_round_trips() {
        let v = sample_precommit();
        let bytes = BftMessage::Precommit(v.clone()).encode();
        let decoded = BftMessage::decode(&bytes).unwrap();
        let BftMessage::Precommit(out) = decoded else {
            panic!("variant mismatch");
        };
        assert_eq!(out.block_hash, v.block_hash);
        assert_eq!(out.height, v.height);
        assert_eq!(out.round, v.round);
        assert_eq!(out.validator_index, v.validator_index);
        assert_eq!(out.bls_sig.to_compressed(), v.bls_sig.to_compressed());
    }

    #[test]
    fn decode_empty_buffer_rejected() {
        assert_eq!(BftMessage::decode(&[]).unwrap_err(), CodecError::Empty);
    }

    #[test]
    fn decode_unknown_tag_rejected() {
        let mut bytes = vec![0xff_u8];
        bytes.extend_from_slice(&[0u8; 144]); // total 145 bytes
        assert_eq!(
            BftMessage::decode(&bytes).unwrap_err(),
            CodecError::UnknownTag(0xff),
        );
    }

    #[test]
    fn decode_truncated_proposal_rejected() {
        let mut bytes = sample_proposal().encode();
        bytes.pop();
        assert_eq!(
            BftMessage::decode(&bytes).unwrap_err(),
            CodecError::WrongLength {
                expected: PROPOSAL_LEN,
                got: PROPOSAL_LEN - 1,
            },
        );
    }

    #[test]
    fn decode_truncated_vote_rejected() {
        let mut bytes = BftMessage::Prevote(sample_prevote()).encode();
        bytes.pop();
        assert_eq!(
            BftMessage::decode(&bytes).unwrap_err(),
            CodecError::WrongLength {
                expected: VOTE_LEN,
                got: VOTE_LEN - 1,
            },
        );
    }

    #[test]
    fn decode_invalid_bls_signature_rejected() {
        let mut bytes = BftMessage::Prevote(sample_prevote()).encode();
        // Corrupt the last 96 bytes — BLS sig position for both vote variants.
        for b in bytes.iter_mut().skip(VOTE_LEN - 96) {
            *b = 0xff;
        }
        assert_eq!(
            BftMessage::decode(&bytes).unwrap_err(),
            CodecError::InvalidBlsSignature,
        );
    }

    #[test]
    fn round_tripped_prevote_verifies_against_signer() {
        // End-to-end: encode → decode → BLS verify with original pubkey.
        let sk = bls_sk(42);
        let pk = sk.public_key();
        let v = PrevoteVote::sign(&sk, H256::new([0x77; 32]), 100, 5, 9);
        let bytes = BftMessage::Prevote(v.clone()).encode();
        let BftMessage::Prevote(out) = BftMessage::decode(&bytes).unwrap() else {
            panic!("variant mismatch");
        };
        let d = PrevoteVote::digest(&v.block_hash, v.height, v.round);
        out.bls_sig.verify(d.as_bytes(), &pk).unwrap();
    }
}
