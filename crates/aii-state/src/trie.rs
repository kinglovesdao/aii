//! Modified Merkle Patricia Tree root.
//!
//! Algorithm follows Yellow Paper Appendix D / EIP-2929:
//! - Keys are first converted to nibbles (4-bit values).
//! - Nodes are one of: leaf `[hex_prefix(path, true), value]`,
//!   extension `[hex_prefix(path, false), child_ref]`, or branch
//!   `[c0, c1, …, c15, value]` (17 elements).
//! - A child reference is either the inlined node (if its RLP encoding is
//!   strictly < 32 bytes) or `keccak256(rlp(node))`.
//! - The root is `keccak256(rlp(root_node))`, regardless of node size.

use crate::EMPTY_TRIE_HASH;
use aii_block::BlockBody;
use aii_crypto::keccak::keccak256;
use aii_types::H256;
use alloy_rlp::Encodable;

/// Compute the Yellow Paper `transactions_root` of a block body.
///
/// Keys are RLP-encoded tx indices (`rlp(i)`), values are the EIP-2718
/// envelope bytes of each tx. Empty bodies return [`EMPTY_TRIE_HASH`].
#[must_use]
pub fn transactions_root(body: &BlockBody) -> H256 {
    if body.transactions.is_empty() {
        return EMPTY_TRIE_HASH;
    }
    let pairs: Vec<(Vec<u8>, Vec<u8>)> = body
        .transactions
        .iter()
        .enumerate()
        .map(|(i, tx)| {
            let mut k = alloy_rlp::bytes::BytesMut::new();
            (i as u64).encode(&mut k);
            let mut v = alloy_rlp::bytes::BytesMut::new();
            tx.encode_2718(&mut v);
            (k.to_vec(), v.to_vec())
        })
        .collect();
    mpt_root(pairs)
}

/// Compute the Merkle Patricia Tree root of an ordered KV set.
///
/// Empty input returns `EMPTY_TRIE_HASH`. Otherwise computes the full
/// Ethereum-compatible MPT root.
pub fn mpt_root<I, K, V>(items: I) -> H256
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<[u8]>,
    V: AsRef<[u8]>,
{
    let mut pairs: Vec<(Vec<u8>, Vec<u8>)> = items
        .into_iter()
        .map(|(k, v)| (key_to_nibbles(k.as_ref()), v.as_ref().to_vec()))
        .collect();
    if pairs.is_empty() {
        return EMPTY_TRIE_HASH;
    }
    pairs.sort_by(|a, b| a.0.cmp(&b.0));

    let pair_refs: Vec<(&[u8], &[u8])> = pairs
        .iter()
        .map(|(k, v)| (k.as_slice(), v.as_slice()))
        .collect();
    let node = build_node(&pair_refs);
    keccak256(&node)
}

fn key_to_nibbles(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(b >> 4);
        out.push(b & 0x0f);
    }
    out
}

/// Hex-prefix encoding (Yellow Paper §B / EIP-2929 §D.1).
///
/// Returns the encoded path bytes. First nibble carries two flag bits:
///   bit 1 = leaf-or-extension (1 = leaf)
///   bit 0 = odd-length (1 = odd remaining nibbles)
/// Followed by zero-padded or unpadded nibble stream.
fn hex_prefix(nibbles: &[u8], is_leaf: bool) -> Vec<u8> {
    let flag = if is_leaf { 0b0010 } else { 0b0000 };
    let odd = nibbles.len() % 2 == 1;
    let mut out = Vec::with_capacity(nibbles.len() / 2 + 1);
    if odd {
        out.push((flag | 0b0001) << 4 | nibbles[0]);
        let mut i = 1;
        while i + 1 < nibbles.len() {
            out.push((nibbles[i] << 4) | nibbles[i + 1]);
            i += 2;
        }
    } else {
        out.push(flag << 4);
        let mut i = 0;
        while i + 1 < nibbles.len() {
            out.push((nibbles[i] << 4) | nibbles[i + 1]);
            i += 2;
        }
    }
    out
}

/// Build an RLP-encoded node from sorted nibble-keyed pairs.
fn build_node(pairs: &[(&[u8], &[u8])]) -> Vec<u8> {
    debug_assert!(!pairs.is_empty(), "build_node must receive non-empty input");

    if pairs.len() == 1 {
        // Single entry — leaf with the entire remaining key.
        let (k, v) = pairs[0];
        return rlp_leaf_or_extension(k, v, true);
    }

    // Longest common prefix across all sorted keys.
    let common = common_prefix_len(pairs);
    if common > 0 {
        // Extension: pull off the common prefix, recurse on the trimmed
        // suffix.
        let trimmed: Vec<(&[u8], &[u8])> = pairs.iter().map(|(k, v)| (&k[common..], *v)).collect();
        let child = build_node(&trimmed);
        return rlp_extension(&pairs[0].0[..common], &child);
    }

    // Branch node: split by first nibble (0..16), with leftover at slot 16
    // for any pair whose key is empty at this level.
    let mut children: [Vec<u8>; 17] = Default::default();
    let mut value_at_branch: &[u8] = b"";

    // Group pairs by first nibble.
    let mut i = 0;
    while i < pairs.len() {
        let (k, v) = pairs[i];
        if k.is_empty() {
            value_at_branch = v;
            i += 1;
            continue;
        }
        let first = k[0] as usize;
        let mut j = i + 1;
        while j < pairs.len() && (!pairs[j].0.is_empty()) && (pairs[j].0[0] as usize == first) {
            j += 1;
        }
        let group: Vec<(&[u8], &[u8])> = pairs[i..j].iter().map(|(k, v)| (&k[1..], *v)).collect();
        let node = build_node(&group);
        children[first] = node;
        i = j;
    }

    rlp_branch(&children, value_at_branch)
}

fn common_prefix_len(pairs: &[(&[u8], &[u8])]) -> usize {
    if pairs.is_empty() {
        return 0;
    }
    let first = pairs[0].0;
    let mut max = first.len();
    for (k, _) in pairs.iter().skip(1) {
        max = max.min(k.len());
    }
    let mut i = 0;
    while i < max {
        let b = first[i];
        if pairs.iter().any(|(k, _)| k[i] != b) {
            return i;
        }
        i += 1;
    }
    max
}

fn rlp_leaf_or_extension(path: &[u8], value: &[u8], is_leaf: bool) -> Vec<u8> {
    let hp = hex_prefix(path, is_leaf);
    let mut buf = alloy_rlp::bytes::BytesMut::new();
    let inner_len = hp.as_slice().length() + value.length();
    alloy_rlp::Header {
        list: true,
        payload_length: inner_len,
    }
    .encode(&mut buf);
    hp.as_slice().encode(&mut buf);
    value.encode(&mut buf);
    buf.to_vec()
}

fn rlp_extension(path: &[u8], child_node_rlp: &[u8]) -> Vec<u8> {
    let hp = hex_prefix(path, false);
    let child_ref = child_ref(child_node_rlp);
    let mut buf = alloy_rlp::bytes::BytesMut::new();
    let inner_len = hp.as_slice().length() + child_ref_encoded_length(&child_ref);
    alloy_rlp::Header {
        list: true,
        payload_length: inner_len,
    }
    .encode(&mut buf);
    hp.as_slice().encode(&mut buf);
    write_child_ref(&child_ref, &mut buf);
    buf.to_vec()
}

fn rlp_branch(children: &[Vec<u8>; 17], value: &[u8]) -> Vec<u8> {
    let mut refs: Vec<ChildRef> = Vec::with_capacity(17);
    for c in children.iter().take(16) {
        refs.push(if c.is_empty() {
            ChildRef::Empty
        } else {
            child_ref(c)
        });
    }
    // The 17th element is the value at this branch (may be empty).
    refs.push(ChildRef::Value(value.to_vec()));

    let inner_len: usize = refs.iter().map(child_ref_encoded_length).sum();
    let mut buf = alloy_rlp::bytes::BytesMut::new();
    alloy_rlp::Header {
        list: true,
        payload_length: inner_len,
    }
    .encode(&mut buf);
    for r in &refs {
        write_child_ref(r, &mut buf);
    }
    buf.to_vec()
}

#[derive(Debug)]
enum ChildRef {
    Empty,
    Inlined(Vec<u8>),
    Hashed(H256),
    Value(Vec<u8>),
}

fn child_ref(node_rlp: &[u8]) -> ChildRef {
    if node_rlp.len() < 32 {
        ChildRef::Inlined(node_rlp.to_vec())
    } else {
        ChildRef::Hashed(keccak256(node_rlp))
    }
}

fn child_ref_encoded_length(r: &ChildRef) -> usize {
    match r {
        ChildRef::Empty => 1,            // 0x80 (empty string)
        ChildRef::Inlined(b) => b.len(), // already RLP-encoded
        ChildRef::Hashed(_) => 33,       // 0xa0 + 32 bytes
        ChildRef::Value(v) => v.as_slice().length(),
    }
}

fn write_child_ref(r: &ChildRef, out: &mut alloy_rlp::bytes::BytesMut) {
    match r {
        ChildRef::Empty => {
            out.extend_from_slice(&[0x80]);
        }
        ChildRef::Inlined(b) => {
            out.extend_from_slice(b);
        }
        ChildRef::Hashed(h) => {
            h.encode(out);
        }
        ChildRef::Value(v) => {
            v.as_slice().encode(out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_equals_empty_trie_hash() {
        let empty: Vec<(Vec<u8>, Vec<u8>)> = vec![];
        assert_eq!(mpt_root(empty), EMPTY_TRIE_HASH);
    }

    #[test]
    fn single_entry_is_deterministic() {
        let items = vec![(b"key".to_vec(), b"value".to_vec())];
        let r1 = mpt_root(items.clone());
        let r2 = mpt_root(items);
        assert_eq!(r1, r2);
        assert_ne!(r1, EMPTY_TRIE_HASH);
    }

    #[test]
    fn order_independent() {
        let a = vec![
            (b"key1".to_vec(), b"v1".to_vec()),
            (b"key2".to_vec(), b"v2".to_vec()),
            (b"key3".to_vec(), b"v3".to_vec()),
        ];
        let b = vec![
            (b"key3".to_vec(), b"v3".to_vec()),
            (b"key1".to_vec(), b"v1".to_vec()),
            (b"key2".to_vec(), b"v2".to_vec()),
        ];
        assert_eq!(mpt_root(a), mpt_root(b));
    }

    #[test]
    fn different_inputs_yield_different_roots() {
        let a = vec![(b"k".to_vec(), b"a".to_vec())];
        let b = vec![(b"k".to_vec(), b"b".to_vec())];
        assert_ne!(mpt_root(a), mpt_root(b));
    }

    #[test]
    fn hex_prefix_leaf_even_length() {
        assert_eq!(
            hex_prefix(&[0x01, 0x02, 0x03, 0x04], true),
            vec![0x20, 0x12, 0x34]
        );
    }

    #[test]
    fn hex_prefix_leaf_odd_length() {
        assert_eq!(hex_prefix(&[0x01, 0x02, 0x03], true), vec![0x31, 0x23]);
    }

    #[test]
    fn hex_prefix_extension_even_length() {
        assert_eq!(
            hex_prefix(&[0x01, 0x02, 0x03, 0x04], false),
            vec![0x00, 0x12, 0x34]
        );
    }

    #[test]
    fn hex_prefix_extension_odd_length() {
        assert_eq!(hex_prefix(&[0x01, 0x02, 0x03], false), vec![0x11, 0x23]);
    }

    #[test]
    fn growing_input_changes_root() {
        let small = vec![(b"k1".to_vec(), b"v1".to_vec())];
        let bigger = vec![
            (b"k1".to_vec(), b"v1".to_vec()),
            (b"k2".to_vec(), b"v2".to_vec()),
        ];
        assert_ne!(mpt_root(small), mpt_root(bigger));
    }

    #[test]
    fn many_keys_terminates() {
        // 100 keys — exercise branch-split, extension, and large-payload paths.
        let items: Vec<(Vec<u8>, Vec<u8>)> = (0..100u64)
            .map(|i| (i.to_be_bytes().to_vec(), format!("v{i}").into_bytes()))
            .collect();
        let root = mpt_root(items.clone());
        // Re-compute — must match.
        assert_eq!(root, mpt_root(items));
    }

    #[test]
    fn transactions_root_empty_body_is_empty_trie_hash() {
        let body = BlockBody::default();
        assert_eq!(transactions_root(&body), EMPTY_TRIE_HASH);
    }

    #[test]
    fn transactions_root_shifts_on_body_change() {
        use aii_block::tx::{Tx, TxLegacy};
        use aii_types::{AlgoId, U256};
        let tx = |nonce: u64| {
            Tx::Legacy(TxLegacy {
                nonce,
                gas_price: U256::from(1_000_000_000u64),
                gas_limit: 21_000,
                to: Some(aii_types::Address::new([0x99; 20])),
                value: U256::from(1u64),
                data: vec![],
                v: 27,
                r: H256::new([0xaa; 32]),
                s: H256::new([0xbb; 32]),
                algo_id: AlgoId::Secp256k1,
            })
        };
        let b1 = BlockBody {
            transactions: vec![tx(0)],
            ..Default::default()
        };
        let b2 = BlockBody {
            transactions: vec![tx(0), tx(1)],
            ..Default::default()
        };
        let r1 = transactions_root(&b1);
        let r2 = transactions_root(&b2);
        assert_ne!(r1, EMPTY_TRIE_HASH);
        assert_ne!(r1, r2);
    }

    #[test]
    fn duplicate_keys_last_wins() {
        // Sorting puts duplicates next to each other; latter input overrides
        // earlier — caller is expected to dedupe, but the result should at
        // least be deterministic.
        let a = vec![
            (b"k".to_vec(), b"v1".to_vec()),
            (b"k".to_vec(), b"v2".to_vec()),
        ];
        let b = a.clone();
        assert_eq!(mpt_root(a), mpt_root(b));
    }
}
