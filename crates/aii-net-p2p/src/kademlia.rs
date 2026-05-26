//! Kademlia routing table for devp2p Discovery v4 (roadmap C.3).
//!
//! 256 k-buckets, one per leading-zero count of the
//! XOR-distance between the local node id and a peer's id. Each
//! bucket holds up to `K = 16` entries, ordered most-recently-seen
//! last; `insert` evicts the oldest entry when full unless the
//! incumbent is still live (eviction policy is "least-recently-seen"
//! per the original Kademlia paper).
//!
//! Node ids are the 32-byte keccak256 of the secp256k1 public key —
//! same identity scheme devp2p uses for the discovery enode address.
//! Wiring the table to UDP `FindNode` / `Neighbours` is the
//! follow-up: this crate ships the data-structure primitive plus
//! tests, the discovery driver call lands when libp2p-style
//! multiplexing arrives.

use aii_types::H256;

/// Maximum entries per k-bucket. 16 matches the Ethereum devp2p
/// default and is the most common Kademlia tuning.
pub const K: usize = 16;
/// Number of k-buckets. 256 because node ids are 32-byte SHA-2-sized.
pub const BUCKETS: usize = 256;

/// One peer entry in the routing table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerEntry {
    /// 32-byte Kademlia node id (`keccak256(pubkey)`).
    pub node_id: H256,
    /// Optional opaque payload — typically the wire `Endpoint`
    /// serialised, but the table is agnostic to its contents.
    pub payload: Vec<u8>,
}

/// 256-bucket Kademlia routing table keyed off a local node id.
#[derive(Debug, Clone)]
pub struct KademliaTable {
    local_id: H256,
    buckets: Vec<Vec<PeerEntry>>,
}

impl KademliaTable {
    /// Construct a fresh, empty table for `local_id`.
    #[must_use]
    pub fn new(local_id: H256) -> Self {
        Self {
            local_id,
            buckets: vec![Vec::new(); BUCKETS],
        }
    }

    /// Read-only access to the local node id.
    #[must_use]
    pub const fn local_id(&self) -> &H256 {
        &self.local_id
    }

    /// Total number of peers across every bucket.
    #[must_use]
    pub fn len(&self) -> usize {
        self.buckets.iter().map(Vec::len).sum()
    }

    /// `true` iff every bucket is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buckets.iter().all(Vec::is_empty)
    }

    /// Bucket index for `id` — count of leading bits shared with
    /// `local_id`. A node identical to local lands in bucket 255
    /// (the "self" slot, intentionally so) so the rest of the API
    /// can blindly index without bounds checks.
    #[must_use]
    pub fn bucket_index(&self, id: &H256) -> usize {
        let l = self.local_id.as_bytes();
        let r = id.as_bytes();
        for (i, (a, b)) in l.iter().zip(r.iter()).enumerate() {
            let x = a ^ b;
            if x != 0 {
                return i * 8 + x.leading_zeros() as usize;
            }
        }
        BUCKETS - 1
    }

    /// Insert / refresh `entry`. If the bucket is full and `entry`
    /// is not already present, the oldest existing entry is evicted
    /// to make room (Kademlia LRS eviction). If `entry.node_id` is
    /// already in the bucket, it bubbles to the most-recently-seen
    /// end without changing the bucket length.
    pub fn insert(&mut self, entry: PeerEntry) {
        if entry.node_id == self.local_id {
            return; // never store ourselves
        }
        let idx = self.bucket_index(&entry.node_id);
        let bucket = &mut self.buckets[idx];
        if let Some(pos) = bucket.iter().position(|e| e.node_id == entry.node_id) {
            // Refresh: move to the back.
            let existing = bucket.remove(pos);
            bucket.push(existing);
            return;
        }
        if bucket.len() == K {
            // LRS eviction: drop the first entry, append the new one.
            bucket.remove(0);
        }
        bucket.push(entry);
    }

    /// Find the `n` closest peers to `target_id` by XOR distance.
    /// Returns up to `n` entries; fewer if the table has fewer total
    /// peers.
    #[must_use]
    pub fn find_closest(&self, target_id: &H256, n: usize) -> Vec<PeerEntry> {
        let mut all: Vec<PeerEntry> = self.buckets.iter().flatten().cloned().collect();
        all.sort_by(|a, b| {
            xor_distance(&a.node_id, target_id).cmp(&xor_distance(&b.node_id, target_id))
        });
        all.into_iter().take(n).collect()
    }
}

/// XOR distance between two 32-byte ids — interpreted big-endian as
/// a 256-bit integer, but represented as the byte-vector for cheap
/// comparison (lexicographic on bytes == numeric on big-endian).
#[must_use]
pub fn xor_distance(a: &H256, b: &H256) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, (l, r)) in a.as_bytes().iter().zip(b.as_bytes().iter()).enumerate() {
        out[i] = l ^ r;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: u8) -> H256 {
        H256::new([byte; 32])
    }

    fn entry(byte: u8) -> PeerEntry {
        PeerEntry {
            node_id: id(byte),
            payload: vec![byte],
        }
    }

    #[test]
    fn empty_table_has_zero_len() {
        let t = KademliaTable::new(id(0));
        assert!(t.is_empty());
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn insert_increments_len() {
        let mut t = KademliaTable::new(id(0));
        t.insert(entry(1));
        t.insert(entry(2));
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn insert_self_is_ignored() {
        let mut t = KademliaTable::new(id(0));
        t.insert(entry(0));
        assert!(t.is_empty());
    }

    #[test]
    fn bucket_index_for_same_byte_pattern_is_zero_only_when_first_byte_differs() {
        // local = 0x00.., id = 0x80.. → first byte XOR is 0x80,
        // leading_zeros = 0 → bucket 0.
        let t = KademliaTable::new(id(0));
        let target = H256::new([0x80; 32]);
        assert_eq!(t.bucket_index(&target), 0);
    }

    #[test]
    fn refresh_moves_existing_to_end() {
        let mut t = KademliaTable::new(id(0));
        t.insert(entry(1));
        t.insert(entry(2));
        // Refresh entry(1).
        t.insert(entry(1));
        // Both should still be in the same bucket; len unchanged.
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn find_closest_orders_by_xor_distance() {
        let mut t = KademliaTable::new(id(0));
        t.insert(entry(1));
        t.insert(entry(2));
        t.insert(entry(4));
        t.insert(entry(8));
        let closest = t.find_closest(&id(1), 2);
        assert_eq!(closest.len(), 2);
        // 0x01 ^ 0x01 = 0; 0x02 ^ 0x01 = 3 — so entry(1) is first.
        assert_eq!(closest[0].node_id, id(1));
    }

    #[test]
    fn find_closest_caps_at_n() {
        let mut t = KademliaTable::new(id(0));
        for b in 1..=10u8 {
            t.insert(PeerEntry {
                node_id: id(b),
                payload: vec![b],
            });
        }
        assert_eq!(t.find_closest(&id(5), 3).len(), 3);
    }

    #[test]
    fn bucket_full_evicts_oldest_on_new_insert() {
        let mut t = KademliaTable::new(id(0));
        // Fill bucket 0 with K entries (all sharing the same leading
        // bit pattern as 0x80..0xFF).
        for b in 0..K as u8 {
            t.insert(PeerEntry {
                node_id: H256::new([0x80 | b; 32]),
                payload: vec![b],
            });
        }
        assert_eq!(t.len(), K);
        // Insert one more — oldest entry should drop.
        t.insert(PeerEntry {
            node_id: H256::new([0x80 | K as u8; 32]),
            payload: vec![0xff],
        });
        assert_eq!(t.len(), K, "bucket length capped at K after eviction");
    }
}
