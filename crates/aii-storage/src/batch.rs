//! Backend-agnostic write batch.
//!
//! Callers build a [`WriteBatch`] of [`Op`] entries and hand it to
//! [`crate::backend::KvBackend::write`]. Each backend replays the ops
//! into its native batch type (`RocksDB` `WriteBatch` / `BTreeMap` mutation)
//! atomically — all ops in the batch land together or not at all.

use crate::cf::ColumnFamily;

/// A single op inside a [`WriteBatch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// Insert / overwrite `(cf, key) → value`.
    Put {
        /// Target column family.
        cf: ColumnFamily,
        /// Key bytes.
        key: Vec<u8>,
        /// Value bytes.
        value: Vec<u8>,
    },
    /// Delete `(cf, key)`. No-op if absent.
    Delete {
        /// Target column family.
        cf: ColumnFamily,
        /// Key bytes.
        key: Vec<u8>,
    },
}

/// Backend-agnostic atomic write set.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct WriteBatch {
    ops: Vec<Op>,
}

impl WriteBatch {
    /// Empty batch.
    #[must_use]
    pub const fn new() -> Self {
        Self { ops: Vec::new() }
    }

    /// Queue a put op.
    pub fn put(&mut self, cf: ColumnFamily, key: &[u8], value: &[u8]) -> &mut Self {
        self.ops.push(Op::Put {
            cf,
            key: key.to_vec(),
            value: value.to_vec(),
        });
        self
    }

    /// Queue a delete op.
    pub fn delete(&mut self, cf: ColumnFamily, key: &[u8]) -> &mut Self {
        self.ops.push(Op::Delete {
            cf,
            key: key.to_vec(),
        });
        self
    }

    /// Number of queued ops.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// True iff no ops have been queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Iterate the queued ops in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = &Op> {
        self.ops.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_batch_is_empty() {
        let b = WriteBatch::new();
        assert_eq!(b.len(), 0);
        assert!(b.is_empty());
        assert_eq!(b.iter().count(), 0);
    }

    #[test]
    fn put_pushes_one_op() {
        let mut b = WriteBatch::new();
        b.put(ColumnFamily::State, b"k", b"v");
        assert_eq!(b.len(), 1);
        assert!(!b.is_empty());
    }

    #[test]
    fn delete_pushes_one_op() {
        let mut b = WriteBatch::new();
        b.delete(ColumnFamily::Headers, b"h");
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn iter_preserves_insertion_order() {
        let mut b = WriteBatch::new();
        b.put(ColumnFamily::State, b"a", b"1")
            .delete(ColumnFamily::State, b"b")
            .put(ColumnFamily::Meta, b"c", b"3");
        let ops: Vec<_> = b.iter().collect();
        assert_eq!(ops.len(), 3);
        assert!(matches!(
            ops[0],
            Op::Put {
                cf: ColumnFamily::State,
                ..
            }
        ));
        assert!(matches!(
            ops[1],
            Op::Delete {
                cf: ColumnFamily::State,
                ..
            }
        ));
        assert!(matches!(
            ops[2],
            Op::Put {
                cf: ColumnFamily::Meta,
                ..
            }
        ));
    }

    #[test]
    fn fluent_chaining_works() {
        let mut b = WriteBatch::new();
        let n = b
            .put(ColumnFamily::State, b"a", b"1")
            .put(ColumnFamily::State, b"b", b"2")
            .len();
        assert_eq!(n, 2);
    }
}
