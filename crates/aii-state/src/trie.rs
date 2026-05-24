//! Merkle Patricia Trie root — v0.0.6 placeholder (empty input only).
//!
//! Full implementation (hex-prefix + branch / extension / leaf nodes +
//! RLP-length pruning) lands in v0.0.7 along with `transactions_root` /
//! `receipts_root` / `withdrawals_root` helpers.

use crate::EMPTY_TRIE_HASH;
use aii_types::H256;

/// Compute the Merkle Patricia Tree root of an ordered KV set.
///
/// v0.0.6 only handles the empty case (returns `EMPTY_TRIE_HASH`).
/// Calling with non-empty input panics — full algorithm in v0.0.7.
///
/// # Panics
/// If `items` is non-empty.
pub fn mpt_root<I, K, V>(items: I) -> H256
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<[u8]>,
    V: AsRef<[u8]>,
{
    let mut iter = items.into_iter();
    if iter.next().is_none() {
        return EMPTY_TRIE_HASH;
    }
    unimplemented!("aii-state mpt_root: non-empty input requires v0.0.7");
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
    #[should_panic(expected = "v0.0.7")]
    fn non_empty_input_panics_until_v0_0_7() {
        let items = vec![(b"a".to_vec(), b"v".to_vec())];
        let _ = mpt_root(items);
    }
}
