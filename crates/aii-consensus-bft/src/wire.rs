//! BFT-PoS stage 4: wire-format codec for consensus messages.
//!
//! [`BftMessage`] is the typed envelope a validator emits / receives
//! over the network. Vote messages keep their fixed-layout byte packing
//! so a malformed message is detected by length alone. The `Proposal`
//! variant carries a variable-length, length-prefixed RLP-encoded block
//! body so multi-validator BFT can replicate the full block (with its
//! transactions) across peers — empty-body proposals stay backwards
//! compatible in shape (4 zero bytes after the fixed header). Every
//! variant has the same first byte (the tag) so a peer can route without
//! decoding the rest.
//!
//! ## On-the-wire layout
//!
//! | Variant     | Bytes | Layout |
//! |-------------|-------|--------|
//! | `Proposal`  | 197 + body_len | `0x00 ‖ height_be8 ‖ round_be4 ‖ block[32] ‖ vrf_preout[32] ‖ vrf_proof[64] ‖ vrf_output[32] ‖ coinbase[20] ‖ body_len_be4 ‖ body_bytes[body_len]` |
//! | `Prevote`   | 145   | `0x01 ‖ block[32] ‖ height_be8 ‖ round_be4 ‖ index_be4 ‖ bls_sig[96]` |
//! | `Precommit` | 145   | `0x02 ‖ block[32] ‖ height_be8 ‖ round_be4 ‖ index_be4 ‖ bls_sig[96]` |
//!
//! The `coinbase` field carries the proposer's coinbase address so
//! followers can reconstruct the block header with the same
//! `beneficiary` field the leader signed — without it, a follower
//! would slot in *its own* `--coinbase`, get a different block hash,
//! and reject the proposal.
//!
//! `body_bytes` is the RLP-encoded [`aii_block::BlockBody`] the leader
//! proposed. Followers RLP-decode it, pair it with the
//! engine-reconstructed header, and verify the resulting block's hash
//! matches the `block_hash` field of the proposal.
//!
//! Decode validates:
//! 1. Buffer length matches the variant's expected size (fixed for
//!    votes; ≥ [`PROPOSAL_MIN_LEN`] and `== PROPOSAL_HEADER_LEN + 4 + body_len`
//!    for proposals; body_len capped at [`MAX_PROPOSAL_BODY_LEN`]).
//! 2. BLS signature decompresses to a valid G2 point (rejects garbage).
//! 3. VRF pre-output and proof are accepted as raw bytes — semantic VRF
//!    verification happens later at the consumer (e.g. `LeaderProof::verify`).
//!
//! This module does NOT touch the network — it only knows how to turn
//! a typed message into bytes and back. The gossip driver does the
//! networking and RLP body coercion.

use aii_crypto::bls;
use aii_crypto::vrf::VrfProof;
use aii_types::{Address, H256};
use thiserror::Error;

use crate::bft::{LeaderProof, PrecommitCertificate, PrecommitVote, PrevoteVote};

/// Tag byte for [`BftMessage::Proposal`].
pub const TAG_PROPOSAL: u8 = 0x00;
/// Tag byte for [`BftMessage::Prevote`].
pub const TAG_PREVOTE: u8 = 0x01;
/// Tag byte for [`BftMessage::Precommit`].
pub const TAG_PRECOMMIT: u8 = 0x02;
/// Tag byte for [`BftMessage::BlockRequest`] (v0.0.93 block-sync).
pub const TAG_BLOCK_REQUEST: u8 = 0x03;
/// Tag byte for [`BftMessage::BlockResponse`] (v0.0.93 block-sync).
pub const TAG_BLOCK_RESPONSE: u8 = 0x04;

/// Encoded size of a [`BftMessage::BlockRequest`] — `tag ‖ height_be8`.
pub const BLOCK_REQUEST_LEN: usize = 1 + 8;
/// Fixed-prefix size of a [`BftMessage::BlockResponse`] — `tag ‖ len_be4`.
pub const BLOCK_RESPONSE_HEADER_LEN: usize = 1 + 4;
/// Minimum encoded size of a [`BftMessage::BlockResponse`] (empty body cap).
pub const BLOCK_RESPONSE_MIN_LEN: usize = BLOCK_RESPONSE_HEADER_LEN + BLOCK_CERT_LEN;
/// Fixed encoded size of a [`PrecommitCertificate`] inside `BlockResponse`.
pub const BLOCK_CERT_LEN: usize = 32 + 8 + 4 + 16 + 96;
/// Hard cap on the RLP block bytes carried in a `BlockResponse`. Shares
/// the proposal-body ceiling — a full block is body + a small header.
pub const MAX_BLOCK_RESPONSE_LEN: usize = MAX_PROPOSAL_BODY_LEN + 64 * 1024;

/// Fixed-prefix size of a `Proposal` message — tag + height + round +
/// block_hash + leader proof + coinbase. Constant across releases.
pub const PROPOSAL_HEADER_LEN: usize = 1 + 8 + 4 + 32 + 32 + 64 + 32 + 20;
/// Minimum encoded size of a `Proposal` — header + the 4-byte body
/// length prefix, with an empty body. A well-formed proposal is always
/// at least this long.
pub const PROPOSAL_MIN_LEN: usize = PROPOSAL_HEADER_LEN + 4;
/// Hard cap on the RLP body bytes carried in a `Proposal`.
///
/// 16 MiB is well above today's block-body ceiling (1428 txs × ~400
/// bytes = ~570 kB at gas_limit 30M / 21k-per-tx) and small enough
/// that a malicious peer cannot exhaust memory by claiming a giant
/// `body_len`.
pub const MAX_PROPOSAL_BODY_LEN: usize = 16 * 1024 * 1024;
/// Encoded size in bytes of a `Prevote` / `Precommit` message.
pub const VOTE_LEN: usize = 1 + 32 + 8 + 4 + 4 + 96;

/// One BFT consensus message — the unit of network exchange.
#[derive(Clone, Debug)]
pub enum BftMessage {
    /// Proposer announcing their proposal + VRF leader proof + block
    /// body for the current `(height, round)`. `body_bytes` is the
    /// RLP encoding of [`aii_block::BlockBody`]; followers decode it,
    /// pair it with the engine-reconstructed header, and check that
    /// the resulting block hash matches `block_hash`.
    Proposal {
        /// Height being proposed for.
        height: u64,
        /// Round being proposed in.
        round: u32,
        /// Block hash being proposed.
        block_hash: H256,
        /// Leader proof binding this proposer to `(height, round, seed)`.
        leader_proof: LeaderProof,
        /// Proposer's `--coinbase` — used as the block's `beneficiary`
        /// in the header. Carried on the wire because followers cannot
        /// otherwise know which coinbase the leader signed under (and
        /// would otherwise reconstruct the header with their own
        /// coinbase, yielding a different block hash).
        coinbase: Address,
        /// RLP-encoded `BlockBody` (transactions + ommers + withdrawals).
        /// Empty `Vec` is valid and means "no transactions".
        body_bytes: Vec<u8>,
    },
    /// One validator's PRE-VOTE.
    Prevote(PrevoteVote),
    /// One validator's PRE-COMMIT.
    Precommit(PrecommitVote),
    /// v0.0.93 block-sync: "send me the committed block at `height`".
    ///
    /// Broadcast by a validator that has fallen behind (its head is
    /// below a height it sees peers voting/proposing at). Any peer
    /// whose recent-block cache holds that height answers with a
    /// [`BftMessage::BlockResponse`]. This closes the "restart →
    /// 1 block behind → can never catch up over gossip → stall" hole:
    /// before v0.0.93 gossip carried only current-round messages and a
    /// lagging validator could only recover via an HTTP `--bootnode`
    /// cold-sync.
    BlockRequest {
        /// Committed height the requester wants.
        height: u64,
    },
    /// v0.0.93 block-sync: a committed block, in answer to a
    /// [`BftMessage::BlockRequest`]. `block_bytes` is the RLP encoding
    /// of [`aii_block::Block`]. The certificate must verify against the
    /// current validator set before the requester may adopt the block.
    BlockResponse {
        /// RLP-encoded committed [`aii_block::Block`].
        block_bytes: Vec<u8>,
        /// 2/3+ stake BFT finality proof for `block_bytes`.
        certificate: PrecommitCertificate,
    },
}

impl BftMessage {
    /// First byte of the encoding — usable for cheap routing.
    #[must_use]
    pub const fn tag(&self) -> u8 {
        match self {
            Self::Proposal { .. } => TAG_PROPOSAL,
            Self::Prevote(_) => TAG_PREVOTE,
            Self::Precommit(_) => TAG_PRECOMMIT,
            Self::BlockRequest { .. } => TAG_BLOCK_REQUEST,
            Self::BlockResponse { .. } => TAG_BLOCK_RESPONSE,
        }
    }

    /// Encoded byte length for this message. Variable for `Proposal`
    /// (depends on body size); fixed for the vote variants.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        match self {
            Self::Proposal { body_bytes, .. } => PROPOSAL_MIN_LEN + body_bytes.len(),
            Self::Prevote(_) | Self::Precommit(_) => VOTE_LEN,
            Self::BlockRequest { .. } => BLOCK_REQUEST_LEN,
            Self::BlockResponse { block_bytes, .. } => {
                BLOCK_RESPONSE_HEADER_LEN + block_bytes.len() + BLOCK_CERT_LEN
            }
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
                coinbase,
                body_bytes,
            } => {
                buf.push(TAG_PROPOSAL);
                buf.extend_from_slice(&height.to_be_bytes());
                buf.extend_from_slice(&round.to_be_bytes());
                buf.extend_from_slice(block_hash.as_bytes());
                buf.extend_from_slice(&leader_proof.vrf_proof.pre_output);
                buf.extend_from_slice(&leader_proof.vrf_proof.proof);
                buf.extend_from_slice(&leader_proof.vrf_output);
                buf.extend_from_slice(coinbase.as_bytes());
                // SAFETY: callers that build a `Proposal` are responsible for
                // keeping the body within `MAX_PROPOSAL_BODY_LEN`. Decode rejects
                // anything larger, so a too-big encode would round-trip to an
                // explicit `WrongLength` error rather than silently truncating.
                let body_len = u32::try_from(body_bytes.len()).unwrap_or(u32::MAX);
                buf.extend_from_slice(&body_len.to_be_bytes());
                buf.extend_from_slice(body_bytes);
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
            Self::BlockRequest { height } => {
                buf.push(TAG_BLOCK_REQUEST);
                buf.extend_from_slice(&height.to_be_bytes());
            }
            Self::BlockResponse {
                block_bytes,
                certificate,
            } => {
                buf.push(TAG_BLOCK_RESPONSE);
                let len = u32::try_from(block_bytes.len()).unwrap_or(u32::MAX);
                buf.extend_from_slice(&len.to_be_bytes());
                buf.extend_from_slice(block_bytes);
                encode_precommit_certificate(&mut buf, certificate);
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
            TAG_BLOCK_REQUEST => decode_block_request(bytes),
            TAG_BLOCK_RESPONSE => decode_block_response(bytes),
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
    if bytes.len() < PROPOSAL_MIN_LEN {
        return Err(CodecError::WrongLength {
            expected: PROPOSAL_MIN_LEN,
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
    cur += 32;
    let coinbase_bytes: [u8; 20] = bytes[cur..cur + 20].try_into().unwrap();
    let coinbase = Address::new(coinbase_bytes);
    cur += 20;
    debug_assert_eq!(cur, PROPOSAL_HEADER_LEN);
    let body_len = u32::from_be_bytes(bytes[cur..cur + 4].try_into().unwrap()) as usize;
    cur += 4;
    if body_len > MAX_PROPOSAL_BODY_LEN {
        return Err(CodecError::ProposalBodyTooLarge {
            max: MAX_PROPOSAL_BODY_LEN,
            got: body_len,
        });
    }
    let expected_total = PROPOSAL_MIN_LEN + body_len;
    if bytes.len() != expected_total {
        return Err(CodecError::WrongLength {
            expected: expected_total,
            got: bytes.len(),
        });
    }
    let body_bytes = bytes[cur..cur + body_len].to_vec();
    Ok(BftMessage::Proposal {
        height,
        round,
        block_hash,
        leader_proof: LeaderProof {
            vrf_proof: VrfProof { pre_output, proof },
            vrf_output,
        },
        coinbase,
        body_bytes,
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

fn decode_block_request(bytes: &[u8]) -> Result<BftMessage, CodecError> {
    if bytes.len() != BLOCK_REQUEST_LEN {
        return Err(CodecError::WrongLength {
            expected: BLOCK_REQUEST_LEN,
            got: bytes.len(),
        });
    }
    let height = u64::from_be_bytes(bytes[1..9].try_into().unwrap());
    Ok(BftMessage::BlockRequest { height })
}

fn decode_block_response(bytes: &[u8]) -> Result<BftMessage, CodecError> {
    if bytes.len() < BLOCK_RESPONSE_MIN_LEN {
        return Err(CodecError::WrongLength {
            expected: BLOCK_RESPONSE_MIN_LEN,
            got: bytes.len(),
        });
    }
    let len = u32::from_be_bytes(bytes[1..5].try_into().unwrap()) as usize;
    if len > MAX_BLOCK_RESPONSE_LEN {
        return Err(CodecError::BlockResponseTooLarge {
            max: MAX_BLOCK_RESPONSE_LEN,
            got: len,
        });
    }
    let expected_total = BLOCK_RESPONSE_HEADER_LEN + len + BLOCK_CERT_LEN;
    if bytes.len() != expected_total {
        return Err(CodecError::WrongLength {
            expected: expected_total,
            got: bytes.len(),
        });
    }
    let block_bytes = bytes[BLOCK_RESPONSE_HEADER_LEN..BLOCK_RESPONSE_HEADER_LEN + len].to_vec();
    let cert_start = BLOCK_RESPONSE_HEADER_LEN + len;
    let certificate = decode_precommit_certificate(&bytes[cert_start..])?;
    Ok(BftMessage::BlockResponse {
        block_bytes,
        certificate,
    })
}

fn encode_precommit_certificate(buf: &mut Vec<u8>, cert: &PrecommitCertificate) {
    buf.extend_from_slice(cert.block_hash.as_bytes());
    buf.extend_from_slice(&cert.height.to_be_bytes());
    buf.extend_from_slice(&cert.round.to_be_bytes());
    buf.extend_from_slice(&cert.signer_bitmap.to_be_bytes());
    buf.extend_from_slice(&cert.aggregated_sig.to_compressed());
}

fn decode_precommit_certificate(bytes: &[u8]) -> Result<PrecommitCertificate, CodecError> {
    if bytes.len() != BLOCK_CERT_LEN {
        return Err(CodecError::WrongLength {
            expected: BLOCK_CERT_LEN,
            got: bytes.len(),
        });
    }
    let block_hash = H256::new(bytes[0..32].try_into().unwrap());
    let height = u64::from_be_bytes(bytes[32..40].try_into().unwrap());
    let round = u32::from_be_bytes(bytes[40..44].try_into().unwrap());
    let signer_bitmap = u128::from_be_bytes(bytes[44..60].try_into().unwrap());
    let sig_bytes: [u8; 96] = bytes[60..156].try_into().unwrap();
    let aggregated_sig =
        bls::Signature::from_compressed(&sig_bytes).map_err(|_| CodecError::InvalidBlsSignature)?;
    Ok(PrecommitCertificate {
        block_hash,
        height,
        round,
        signer_bitmap,
        aggregated_sig,
    })
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

    /// Proposal body would be larger than [`MAX_PROPOSAL_BODY_LEN`]. A
    /// safety cap so a peer cannot exhaust memory by claiming a huge
    /// `body_len`.
    #[error("BFT proposal body too large: max {max}, got {got}")]
    ProposalBodyTooLarge {
        /// Maximum bytes accepted (== [`MAX_PROPOSAL_BODY_LEN`]).
        max: usize,
        /// Length the encoded body claimed.
        got: usize,
    },

    /// `BlockResponse` block bytes would exceed [`MAX_BLOCK_RESPONSE_LEN`].
    #[error("BFT block response too large: max {max}, got {got}")]
    BlockResponseTooLarge {
        /// Maximum bytes accepted (== [`MAX_BLOCK_RESPONSE_LEN`]).
        max: usize,
        /// Length the encoded block claimed.
        got: usize,
    },
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

    fn sample_certificate() -> PrecommitCertificate {
        let vote = sample_precommit();
        PrecommitCertificate {
            block_hash: vote.block_hash,
            height: vote.height,
            round: vote.round,
            signer_bitmap: 1u128 << vote.validator_index,
            aggregated_sig: vote.bls_sig,
        }
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
            coinbase: aii_types::Address::new([0xC0; 20]),
            body_bytes: Vec::new(),
        }
    }

    fn sample_proposal_with_body(body: Vec<u8>) -> BftMessage {
        let sk = vrf_sk();
        let seed = [0x22; 32];
        let leader_proof = LeaderProof::produce(&sk, 9, 1, &seed);
        BftMessage::Proposal {
            height: 9,
            round: 1,
            block_hash: H256::new([0xab; 32]),
            leader_proof,
            coinbase: aii_types::Address::new([0xC1; 20]),
            body_bytes: body,
        }
    }

    #[test]
    fn proposal_tag_and_length_empty_body() {
        let m = sample_proposal();
        assert_eq!(m.tag(), TAG_PROPOSAL);
        assert_eq!(m.encoded_len(), PROPOSAL_MIN_LEN);
        assert_eq!(PROPOSAL_HEADER_LEN, 193);
        assert_eq!(PROPOSAL_MIN_LEN, 197);
    }

    #[test]
    fn proposal_length_scales_with_body() {
        let m = sample_proposal_with_body(vec![0u8; 500]);
        assert_eq!(m.encoded_len(), PROPOSAL_MIN_LEN + 500);
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
        assert_eq!(bytes.len(), PROPOSAL_MIN_LEN);
        assert_eq!(bytes[0], TAG_PROPOSAL);
        // The 4 trailing bytes are the body-length prefix (== 0 for empty body).
        assert_eq!(&bytes[PROPOSAL_HEADER_LEN..], &[0u8; 4]);
    }

    #[test]
    fn encode_proposal_with_body_appends_length_prefix_and_bytes() {
        let body = (0..37u8).collect::<Vec<_>>();
        let bytes = sample_proposal_with_body(body.clone()).encode();
        assert_eq!(bytes.len(), PROPOSAL_MIN_LEN + body.len());
        let len_prefix = u32::from_be_bytes(
            bytes[PROPOSAL_HEADER_LEN..PROPOSAL_HEADER_LEN + 4]
                .try_into()
                .unwrap(),
        );
        assert_eq!(len_prefix as usize, body.len());
        assert_eq!(&bytes[PROPOSAL_HEADER_LEN + 4..], body.as_slice());
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
                    coinbase: cb1,
                    body_bytes: bb1,
                },
                BftMessage::Proposal {
                    height: h2,
                    round: r2,
                    block_hash: b2,
                    leader_proof: lp2,
                    coinbase: cb2,
                    body_bytes: bb2,
                },
            ) => {
                assert_eq!(h1, h2);
                assert_eq!(r1, r2);
                assert_eq!(b1, b2);
                assert_eq!(lp1.vrf_proof, lp2.vrf_proof);
                assert_eq!(lp1.vrf_output, lp2.vrf_output);
                assert_eq!(cb1, cb2);
                assert_eq!(bb1, bb2);
                assert!(bb1.is_empty());
            }
            _ => panic!("variant mismatch"),
        }
    }

    #[test]
    fn proposal_with_body_round_trips() {
        let body: Vec<u8> = (0..=255u8).cycle().take(2048).collect();
        let m = sample_proposal_with_body(body.clone());
        let bytes = m.encode();
        let decoded = BftMessage::decode(&bytes).unwrap();
        let BftMessage::Proposal {
            body_bytes,
            block_hash,
            height,
            round,
            leader_proof,
            coinbase,
        } = decoded
        else {
            panic!("variant mismatch");
        };
        assert_eq!(body_bytes, body);
        assert_eq!(block_hash, H256::new([0xab; 32]));
        assert_eq!(height, 9);
        assert_eq!(round, 1);
        assert_eq!(coinbase, aii_types::Address::new([0xC1; 20]));
        // Smoke check the leader proof round-tripped intact.
        assert_eq!(leader_proof.vrf_output.len(), 32);
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
    fn block_request_round_trips() {
        let m = BftMessage::BlockRequest { height: 359_215 };
        assert_eq!(m.tag(), TAG_BLOCK_REQUEST);
        assert_eq!(m.encoded_len(), BLOCK_REQUEST_LEN);
        let bytes = m.encode();
        assert_eq!(bytes.len(), BLOCK_REQUEST_LEN);
        assert_eq!(bytes[0], TAG_BLOCK_REQUEST);
        let BftMessage::BlockRequest { height } = BftMessage::decode(&bytes).unwrap() else {
            panic!("variant mismatch");
        };
        assert_eq!(height, 359_215);
    }

    #[test]
    fn block_response_round_trips() {
        let block_bytes: Vec<u8> = (0..=255u8).cycle().take(3000).collect();
        let certificate = sample_certificate();
        let m = BftMessage::BlockResponse {
            block_bytes: block_bytes.clone(),
            certificate: certificate.clone(),
        };
        assert_eq!(m.tag(), TAG_BLOCK_RESPONSE);
        assert_eq!(
            m.encoded_len(),
            BLOCK_RESPONSE_HEADER_LEN + block_bytes.len() + BLOCK_CERT_LEN,
        );
        let bytes = m.encode();
        assert_eq!(bytes[0], TAG_BLOCK_RESPONSE);
        let BftMessage::BlockResponse {
            block_bytes: out,
            certificate: out_cert,
        } = BftMessage::decode(&bytes).unwrap()
        else {
            panic!("variant mismatch");
        };
        assert_eq!(out, block_bytes);
        assert_eq!(out_cert.block_hash, certificate.block_hash);
        assert_eq!(out_cert.height, certificate.height);
        assert_eq!(out_cert.round, certificate.round);
        assert_eq!(out_cert.signer_bitmap, certificate.signer_bitmap);
        assert_eq!(
            out_cert.aggregated_sig.to_compressed(),
            certificate.aggregated_sig.to_compressed(),
        );
    }

    #[test]
    fn block_request_wrong_length_rejected() {
        let mut bytes = BftMessage::BlockRequest { height: 1 }.encode();
        bytes.pop();
        assert_eq!(
            BftMessage::decode(&bytes).unwrap_err(),
            CodecError::WrongLength {
                expected: BLOCK_REQUEST_LEN,
                got: BLOCK_REQUEST_LEN - 1,
            },
        );
    }

    #[test]
    fn block_response_length_mismatch_rejected() {
        let mut bytes = BftMessage::BlockResponse {
            block_bytes: vec![0u8; 16],
            certificate: sample_certificate(),
        }
        .encode();
        let total = bytes.len();
        bytes.pop();
        assert_eq!(
            BftMessage::decode(&bytes).unwrap_err(),
            CodecError::WrongLength {
                expected: total,
                got: total - 1,
            },
        );
    }

    #[test]
    fn block_response_oversized_rejected() {
        let mut bytes = BftMessage::BlockResponse {
            block_bytes: vec![0u8; 8],
            certificate: sample_certificate(),
        }
        .encode();
        let claimed = u32::try_from(MAX_BLOCK_RESPONSE_LEN + 1).unwrap();
        bytes[1..5].copy_from_slice(&claimed.to_be_bytes());
        assert_eq!(
            BftMessage::decode(&bytes).unwrap_err(),
            CodecError::BlockResponseTooLarge {
                max: MAX_BLOCK_RESPONSE_LEN,
                got: MAX_BLOCK_RESPONSE_LEN + 1,
            },
        );
    }

    #[test]
    fn real_block_response_round_trips_through_rlp() {
        use aii_block::{Block, BlockBody};
        use alloy_rlp::{Decodable, Encodable};
        // Build a minimal real block, RLP-encode it, ship it in a
        // BlockResponse, decode the message, RLP-decode back to a Block.
        let block = sample_block_for_sync();
        let mut rlp = Vec::new();
        block.encode(&mut rlp);
        let msg = BftMessage::BlockResponse {
            block_bytes: rlp.clone(),
            certificate: sample_certificate(),
        };
        let wire = msg.encode();
        let BftMessage::BlockResponse { block_bytes, .. } = BftMessage::decode(&wire).unwrap()
        else {
            panic!("variant mismatch");
        };
        let mut s: &[u8] = &block_bytes;
        let decoded = Block::decode(&mut s).unwrap();
        assert_eq!(decoded, block);
        let _ = BlockBody::default(); // keep import used if block has empty body
    }

    fn sample_block_for_sync() -> aii_block::Block {
        use aii_block::{
            consts::{EMPTY_LIST_HASH, EMPTY_TRIE_HASH},
            Block, BlockBody, Bloom, Header,
        };
        use aii_types::{Address, U256};
        Block {
            header: Header {
                parent_hash: H256::new([0x11; 32]),
                ommers_hash: EMPTY_LIST_HASH,
                beneficiary: Address::new([0x22; 20]),
                state_root: EMPTY_TRIE_HASH,
                transactions_root: EMPTY_TRIE_HASH,
                receipts_root: EMPTY_TRIE_HASH,
                logs_bloom: Bloom::ZERO,
                difficulty: U256::from(0u64),
                number: 359_215,
                gas_limit: 30_000_000,
                gas_used: 0,
                timestamp: 1_700_000_000,
                extra_data: vec![],
                mix_hash: H256::new([0x33; 32]),
                nonce: [0u8; 8],
                base_fee_per_gas: U256::from(1_000_000_000u64),
                withdrawals_root: EMPTY_TRIE_HASH,
                blob_gas_used: None,
                excess_blob_gas: None,
                parent_beacon_block_root: None,
            },
            body: BlockBody::default(),
        }
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
    fn decode_truncated_proposal_below_min_rejected() {
        // Truncate inside the fixed header so we trip the MIN check.
        let mut bytes = sample_proposal().encode();
        bytes.truncate(PROPOSAL_MIN_LEN - 1);
        assert_eq!(
            BftMessage::decode(&bytes).unwrap_err(),
            CodecError::WrongLength {
                expected: PROPOSAL_MIN_LEN,
                got: PROPOSAL_MIN_LEN - 1,
            },
        );
    }

    #[test]
    fn decode_proposal_body_length_mismatch_rejected() {
        // Encode with a 16-byte body, then chop the last byte. The
        // body_len prefix still claims 16 bytes so the total-length
        // check fires.
        let body = (0..16u8).collect::<Vec<_>>();
        let mut bytes = sample_proposal_with_body(body).encode();
        let original_total = bytes.len();
        bytes.pop();
        assert_eq!(
            BftMessage::decode(&bytes).unwrap_err(),
            CodecError::WrongLength {
                expected: original_total,
                got: original_total - 1,
            },
        );
    }

    #[test]
    fn decode_proposal_oversized_body_rejected() {
        // Hand-craft a header that claims body_len = MAX_PROPOSAL_BODY_LEN + 1
        // without actually allocating that many bytes (just a 4-byte spoof).
        let m = sample_proposal();
        let mut bytes = m.encode();
        let claimed = u32::try_from(MAX_PROPOSAL_BODY_LEN + 1).unwrap();
        bytes[PROPOSAL_HEADER_LEN..PROPOSAL_HEADER_LEN + 4].copy_from_slice(&claimed.to_be_bytes());
        // No need to pad: ProposalBodyTooLarge is checked before the
        // total-length check.
        assert_eq!(
            BftMessage::decode(&bytes).unwrap_err(),
            CodecError::ProposalBodyTooLarge {
                max: MAX_PROPOSAL_BODY_LEN,
                got: MAX_PROPOSAL_BODY_LEN + 1,
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
