//! `ColumnFamily` — closed enum of every CF the AII protocol uses.
//!
//! Adding a variant requires a spec revision (`docs/superpowers/specs/
//! 2026-05-24-aii-storage-design.md` §3). Names are stable wire strings
//! used by both backends and any out-of-band tooling (e.g. `rocksdb-tool`).

use core::fmt;

/// Every column family in the AII protocol. Closed set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColumnFamily {
    /// `RocksDB` default CF (required, almost never written by AII).
    Default,
    /// `block_hash → header bytes`.
    Headers,
    /// `block_hash → tx list bytes`.
    Bodies,
    /// `block_hash → receipts bytes`.
    Receipts,
    /// `tx_hash → tx bytes`.
    Transactions,
    /// `node_hash → MPT node bytes` (state trie nodes; aii-state).
    State,
    /// Per-account storage trie nodes.
    AccountStorage,
    /// `tx_hash → (block_hash, index)`.
    TxLookup,
    /// Schema version + head/finalized markers.
    Meta,
    /// Subchain registry + flush anchors.
    MicroChain,
    /// `code_hash → contract bytecode bytes`.
    Code,
}

impl ColumnFamily {
    /// Every variant, in declaration order. Used to open all CFs at once.
    pub const ALL: &'static [Self] = &[
        Self::Default,
        Self::Headers,
        Self::Bodies,
        Self::Receipts,
        Self::Transactions,
        Self::State,
        Self::AccountStorage,
        Self::TxLookup,
        Self::Meta,
        Self::MicroChain,
        Self::Code,
    ];

    /// Stable wire name (`snake_case`). Used as the `RocksDB` column-family name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Headers => "headers",
            Self::Bodies => "bodies",
            Self::Receipts => "receipts",
            Self::Transactions => "transactions",
            Self::State => "state",
            Self::AccountStorage => "account_storage",
            Self::TxLookup => "tx_lookup",
            Self::Meta => "meta",
            Self::MicroChain => "microchain",
            Self::Code => "code",
        }
    }
}

impl fmt::Display for ColumnFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn all_covers_every_variant_once() {
        let set: HashSet<_> = ColumnFamily::ALL.iter().copied().collect();
        assert_eq!(set.len(), ColumnFamily::ALL.len());
        for cf in ColumnFamily::ALL {
            // Re-match exhaustively so compiler catches new variants we forgot
            // to add to ALL.
            match cf {
                ColumnFamily::Default
                | ColumnFamily::Headers
                | ColumnFamily::Bodies
                | ColumnFamily::Receipts
                | ColumnFamily::Transactions
                | ColumnFamily::State
                | ColumnFamily::AccountStorage
                | ColumnFamily::TxLookup
                | ColumnFamily::Meta
                | ColumnFamily::MicroChain
                | ColumnFamily::Code => {}
            }
        }
    }

    #[test]
    fn as_str_is_unique_per_variant() {
        let names: HashSet<&str> = ColumnFamily::ALL.iter().map(|cf| cf.as_str()).collect();
        assert_eq!(names.len(), ColumnFamily::ALL.len());
    }

    #[test]
    fn as_str_uses_snake_case() {
        for cf in ColumnFamily::ALL {
            let s = cf.as_str();
            assert!(!s.is_empty());
            assert!(s.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
        }
    }

    #[test]
    fn default_cf_name_matches_rocksdb_default() {
        // RocksDB uses the literal string "default" for its mandatory CF.
        assert_eq!(ColumnFamily::Default.as_str(), "default");
    }

    #[test]
    fn display_equals_as_str() {
        assert_eq!(format!("{}", ColumnFamily::State), "state");
    }
}
