# aii-storage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement `crates/aii-storage` — a backend-abstracted KV store with RocksDB + in-memory backends, predefined column families, atomic write batches, read-only snapshots, and a ≥50k op/s sequential-write benchmark gate (M0 §3.1 #4).

**Architecture:** A `KvBackend` trait exposes sync `get / put / delete / write / snapshot / iter / iter_prefix` over `&[u8]` keys and values, parameterized by a closed `ColumnFamily` enum. Two backends implement it: `RocksDbBackend` (production, statically links librocksdb via the `rocksdb` crate) and `MemoryBackend` (BTreeMap-per-CF, used by downstream unit tests). `WriteBatch` is a backend-agnostic `Vec<Op>` that each backend replays into its native batch type. `Snapshot` exposes only `get` + `iter` — read-only consistent views.

**Tech Stack:** Rust 1.94.1, `rocksdb` 0.22 (TiKV Rust binding), `tempfile` 3 (test sandboxing), `proptest` 1, `criterion` 0.5.

**Branch:** `feat/aii-storage` (already created, currently contains the spec doc commit on top of v0.0.3 release).

**Spec:** `docs/superpowers/specs/2026-05-24-aii-storage-design.md`

---

## File Structure

```
crates/aii-storage/
├── Cargo.toml
├── README.md
├── src/
│   ├── lib.rs           # re-exports + module map (≤40 lines)
│   ├── error.rs         # StorageError + impl Display/From<io::Error>
│   ├── cf.rs            # ColumnFamily enum (closed) + as_str + ALL
│   ├── batch.rs         # WriteBatch + Op (backend-agnostic)
│   ├── backend.rs       # KvBackend trait
│   ├── snapshot.rs      # Snapshot trait
│   ├── memory.rs        # MemoryBackend (Arc<RwLock<HashMap<CF, BTreeMap>>>)
│   └── rocksdb.rs       # RocksDbBackend (Arc<rocksdb::DB>) — gated by feature `rocksdb`
├── tests/
│   ├── conformance.rs   # macro-parametrized: 8 tests × 2 backends = 16
│   └── proptest.rs      # 2 property tests (Op-sequence equivalence + snapshot isolation)
└── benches/
    └── write_throughput.rs  # criterion 100k seq writes; assert ≥50k op/s
```

**Modify:**
- `Cargo.toml` (workspace root) — add member + version bump to 0.0.4
- `CHANGELOG.md` — add v0.0.4 entry

---

## Task 1: Crate scaffold (workspace member + empty src + README skeleton)

**Files:**
- Create: `crates/aii-storage/Cargo.toml`
- Create: `crates/aii-storage/src/lib.rs`
- Create: `crates/aii-storage/README.md`
- Modify: `Cargo.toml` (workspace root)

- [ ] **Step 1: Create the crate directory**

Run: `mkdir -p crates/aii-storage/src crates/aii-storage/tests crates/aii-storage/benches`

- [ ] **Step 2: Write `crates/aii-storage/Cargo.toml`**

```toml
[package]
name = "aii-storage"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
authors.workspace = true
description = "Key-value storage abstraction for the AII protocol (RocksDB + in-memory backends)"
readme = "README.md"

[lints]
workspace = true

[features]
default = ["rocksdb"]
rocksdb = ["dep:rocksdb"]

[dependencies]
aii-types = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
rocksdb = { version = "0.22", default-features = false, features = ["lz4"], optional = true }

[dev-dependencies]
proptest = { workspace = true }
tempfile = "3"

[[bench]]
name = "write_throughput"
harness = false
```

- [ ] **Step 3: Write `crates/aii-storage/src/lib.rs` (minimal placeholder so it compiles)**

```rust
//! # AII Storage
//!
//! Key-value storage abstraction used across the AII protocol stack.
//!
//! See `docs/superpowers/specs/2026-05-24-aii-storage-design.md` for the
//! design rationale. Public items will land in the next commits.

#![cfg_attr(not(test), forbid(unsafe_code))]
#![warn(missing_docs)]
```

- [ ] **Step 4: Write `crates/aii-storage/README.md` (skeleton)**

```markdown
# aii-storage

Key-value storage abstraction for the AII protocol.

Backends: RocksDB (production) + in-memory (testing).

API and roadmap land in v0.0.4 — see the spec at
`docs/superpowers/specs/2026-05-24-aii-storage-design.md`.
```

- [ ] **Step 5: Register the crate in the workspace root `Cargo.toml`**

Open `Cargo.toml` (workspace root). In the `[workspace] members = [...]` array add `"crates/aii-storage",` after `"crates/aii-crypto",`. In `[workspace.dependencies]` add this line below the existing `aii-crypto` entry:

```toml
aii-storage = { path = "crates/aii-storage", version = "0.0.3" }
```

- [ ] **Step 6: Verify build**

Run: `cargo build -p aii-storage`
Expected: `Finished ...` with no errors. First build downloads rocksdb + ~50 transitive crates and compiles librocksdb — first build takes 3-5 minutes; subsequent builds are seconds.

- [ ] **Step 7: Commit**

```bash
git add crates/aii-storage Cargo.toml
git commit -m "$(cat <<'EOF'
chore(storage): scaffold aii-storage crate + register in workspace

Empty crate stub with rocksdb 0.22 feature-gated behind the default
"rocksdb" feature. lib.rs is a placeholder; modules land in subsequent
commits per the spec.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: ColumnFamily enum (cf.rs)

**Files:**
- Create: `crates/aii-storage/src/cf.rs`
- Modify: `crates/aii-storage/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/aii-storage/src/cf.rs` with these tests at the bottom (the implementation comes in step 3):

```rust
//! ColumnFamily — closed enum of every CF the AII protocol uses.
//!
//! Adding a variant requires a spec revision (`docs/superpowers/specs/
//! 2026-05-24-aii-storage-design.md` §3). Names are stable wire strings
//! used by both backends and any out-of-band tooling (e.g. `rocksdb-tool`).

use core::fmt;

/// Every column family in the AII protocol. Closed set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColumnFamily {
    /// RocksDB default CF (required, almost never written by AII).
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
    ];

    /// Stable wire name (snake_case). Used as the RocksDB column-family name.
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
        // Trick: each variant goes into a HashSet — if any duplicate or
        // missing variant exists, the set size won't match.
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
                | ColumnFamily::MicroChain => {}
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
```

- [ ] **Step 2: Wire the module into `lib.rs`**

Replace `crates/aii-storage/src/lib.rs` with:

```rust
//! # AII Storage
//!
//! Key-value storage abstraction used across the AII protocol stack.
//!
//! See `docs/superpowers/specs/2026-05-24-aii-storage-design.md` for design.
//!
//! ## Module map
//!
//! | Module     | Purpose                                                        |
//! |------------|----------------------------------------------------------------|
//! | [`cf`]     | [`ColumnFamily`] closed enum + stable wire names.              |

#![cfg_attr(not(test), forbid(unsafe_code))]
#![warn(missing_docs)]

pub mod cf;

pub use cf::ColumnFamily;
```

- [ ] **Step 3: Run the tests — they should pass since the implementation is in the same file as the tests**

Run: `cargo test -p aii-storage cf::`
Expected: `test result: ok. 5 passed; 0 failed`

- [ ] **Step 4: Commit**

```bash
git add crates/aii-storage/src/cf.rs crates/aii-storage/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(storage): ColumnFamily enum (10 variants, closed set)

Adds the closed CF enum used everywhere in the storage layer. Each
variant has a stable snake_case wire name; ColumnFamily::ALL is the
canonical iteration order used by RocksDbBackend::open to create
every CF in one shot. Adding variants requires a spec revision.

5 unit tests cover variant coverage, name uniqueness, casing, the
RocksDB default-CF compatibility, and Display.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: StorageError (error.rs)

**Files:**
- Create: `crates/aii-storage/src/error.rs`
- Modify: `crates/aii-storage/src/lib.rs`

- [ ] **Step 1: Write the failing test (file with implementation + tests)**

Create `crates/aii-storage/src/error.rs`:

```rust
//! Unified error type for `aii-storage`.
//!
//! `StorageError::Backend` wraps the backend-native error message as a
//! string so the public surface stays free of `rocksdb` types — this is
//! what lets `aii-state` / `aii-block` swap backends in tests without
//! conditional compilation.

use thiserror::Error;

use crate::cf::ColumnFamily;

/// Umbrella error returned by every `aii-storage` API.
#[derive(Debug, Error)]
pub enum StorageError {
    /// Backend-native error (e.g. RocksDB), captured as its `Display` text.
    #[error("backend error: {0}")]
    Backend(String),

    /// Backend reports it does not know the named column family.
    #[error("column family not registered: {0}")]
    InvalidColumnFamily(ColumnFamily),

    /// I/O failure outside the backend (e.g. opening the DB directory).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_display_includes_inner_text() {
        let e = StorageError::Backend("disk full".to_string());
        assert!(format!("{e}").contains("disk full"));
    }

    #[test]
    fn invalid_cf_includes_cf_name() {
        let e = StorageError::InvalidColumnFamily(ColumnFamily::State);
        assert!(format!("{e}").contains("state"));
    }

    #[test]
    fn io_error_converts_via_from() {
        let inner = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "nope");
        let outer: StorageError = inner.into();
        assert!(matches!(outer, StorageError::Io(_)));
    }
}
```

- [ ] **Step 2: Add the module to `lib.rs`**

Edit `crates/aii-storage/src/lib.rs`. Add `pub mod error;` after `pub mod cf;`, add `| [`error`]  | [`StorageError`] umbrella over per-backend errors. |` to the module map table, and add `pub use error::StorageError;` after the `cf::` re-export.

Final `lib.rs`:

```rust
//! # AII Storage
//!
//! Key-value storage abstraction used across the AII protocol stack.
//!
//! See `docs/superpowers/specs/2026-05-24-aii-storage-design.md` for design.
//!
//! ## Module map
//!
//! | Module     | Purpose                                                        |
//! |------------|----------------------------------------------------------------|
//! | [`cf`]     | [`ColumnFamily`] closed enum + stable wire names.              |
//! | [`error`]  | [`StorageError`] umbrella over per-backend errors.             |

#![cfg_attr(not(test), forbid(unsafe_code))]
#![warn(missing_docs)]

pub mod cf;
pub mod error;

pub use cf::ColumnFamily;
pub use error::StorageError;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p aii-storage error::`
Expected: `test result: ok. 3 passed; 0 failed`

- [ ] **Step 4: Commit**

```bash
git add crates/aii-storage/src/error.rs crates/aii-storage/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(storage): StorageError umbrella

3 variants: Backend (wraps backend-native error message as String to
keep rocksdb types out of the trait surface), InvalidColumnFamily
(when a backend does not know a registered CF), and Io (with From
impl for std::io::Error).

3 unit tests cover display formatting and From conversion.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: WriteBatch (batch.rs)

**Files:**
- Create: `crates/aii-storage/src/batch.rs`
- Modify: `crates/aii-storage/src/lib.rs`

- [ ] **Step 1: Write `crates/aii-storage/src/batch.rs`**

```rust
//! Backend-agnostic write batch.
//!
//! Callers build a [`WriteBatch`] of [`Op`] entries and hand it to
//! [`crate::backend::KvBackend::write`]. Each backend replays the ops
//! into its native batch type (RocksDB `WriteBatch` / BTreeMap mutation)
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
        assert!(matches!(ops[0], Op::Put { cf: ColumnFamily::State, .. }));
        assert!(matches!(ops[1], Op::Delete { cf: ColumnFamily::State, .. }));
        assert!(matches!(ops[2], Op::Put { cf: ColumnFamily::Meta, .. }));
    }

    #[test]
    fn fluent_chaining_works() {
        let mut b = WriteBatch::new();
        let n = b.put(ColumnFamily::State, b"a", b"1")
                 .put(ColumnFamily::State, b"b", b"2")
                 .len();
        assert_eq!(n, 2);
    }
}
```

- [ ] **Step 2: Add the module to `lib.rs`**

Edit `crates/aii-storage/src/lib.rs`. Add `pub mod batch;` after `pub mod error;`, append `| [`batch`]  | [`WriteBatch`] backend-agnostic op log. |` to the module-map, add `pub use batch::{Op, WriteBatch};` after the existing re-exports.

- [ ] **Step 3: Run tests**

Run: `cargo test -p aii-storage batch::`
Expected: `test result: ok. 5 passed; 0 failed`

- [ ] **Step 4: Commit**

```bash
git add crates/aii-storage/src/batch.rs crates/aii-storage/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(storage): WriteBatch (backend-agnostic Op log)

Pure-Rust struct: Vec<Op> where Op = Put|Delete tagged by ColumnFamily.
Fluent put/delete API, len/is_empty, iter() over insertion order. Each
backend replays ops in its KvBackend::write impl to get cross-CF
atomic semantics.

5 unit tests.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: KvBackend + Snapshot traits

**Files:**
- Create: `crates/aii-storage/src/snapshot.rs`
- Create: `crates/aii-storage/src/backend.rs`
- Modify: `crates/aii-storage/src/lib.rs`

- [ ] **Step 1: Write `crates/aii-storage/src/snapshot.rs`**

```rust
//! Read-only consistent view of a [`crate::backend::KvBackend`].
//!
//! Created via [`KvBackend::snapshot`]; reads see the database state at the
//! moment of creation regardless of concurrent writes. Snapshots are not
//! mutable — callers who need to "write on top of a snapshot" should build
//! a [`crate::WriteBatch`] and commit it via the parent backend.

use crate::{cf::ColumnFamily, error::StorageError};

/// Trait alias for the boxed-iterator type backends hand out, to keep
/// the trait signatures readable.
pub type KvIter<'a> =
    Box<dyn Iterator<Item = Result<(Vec<u8>, Vec<u8>), StorageError>> + 'a>;

/// A read-only point-in-time view.
pub trait Snapshot: Send + Sync {
    /// Read a single value.
    ///
    /// # Errors
    /// Returns [`StorageError`] if the backend fails.
    fn get(&self, cf: ColumnFamily, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError>;

    /// Iterate every `(key, value)` pair in `cf` in ascending key order.
    fn iter<'a>(&'a self, cf: ColumnFamily) -> KvIter<'a>;
}
```

- [ ] **Step 2: Write `crates/aii-storage/src/backend.rs`**

```rust
//! [`KvBackend`] — the abstract storage trait every backend implements.

use crate::{batch::WriteBatch, cf::ColumnFamily, error::StorageError, snapshot::{KvIter, Snapshot}};

/// Backend-abstracted KV store.
///
/// Implementors must be safe to share across threads (`Send + Sync`) and
/// outlive `&self` (`'static` bound enables `Arc<dyn KvBackend>` patterns
/// downstream).
pub trait KvBackend: Send + Sync + 'static {
    /// Snapshot type returned by [`KvBackend::snapshot`].
    type Snapshot: Snapshot;

    /// Read a single value.
    ///
    /// # Errors
    /// Returns [`StorageError`] on backend failure.
    fn get(&self, cf: ColumnFamily, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError>;

    /// Insert / overwrite a single key.
    ///
    /// # Errors
    /// Returns [`StorageError`] on backend failure.
    fn put(&self, cf: ColumnFamily, key: &[u8], value: &[u8]) -> Result<(), StorageError>;

    /// Delete a single key. No-op if absent.
    ///
    /// # Errors
    /// Returns [`StorageError`] on backend failure.
    fn delete(&self, cf: ColumnFamily, key: &[u8]) -> Result<(), StorageError>;

    /// Atomically apply `batch`. All ops land together or none do.
    ///
    /// # Errors
    /// Returns [`StorageError`] on backend failure.
    fn write(&self, batch: WriteBatch) -> Result<(), StorageError>;

    /// Take a consistent read-only snapshot of the current DB state.
    fn snapshot(&self) -> Self::Snapshot;

    /// Iterate every `(key, value)` pair in `cf` in ascending key order.
    fn iter<'a>(&'a self, cf: ColumnFamily) -> KvIter<'a>;

    /// Iterate `(key, value)` pairs in `cf` whose key starts with `prefix`,
    /// in ascending order.
    fn iter_prefix<'a>(&'a self, cf: ColumnFamily, prefix: &'a [u8]) -> KvIter<'a>;
}
```

- [ ] **Step 3: Wire the modules into `lib.rs`**

Replace `lib.rs` with:

```rust
//! # AII Storage
//!
//! Key-value storage abstraction used across the AII protocol stack.
//!
//! See `docs/superpowers/specs/2026-05-24-aii-storage-design.md` for design.
//!
//! ## Module map
//!
//! | Module      | Purpose                                                        |
//! |-------------|----------------------------------------------------------------|
//! | [`cf`]      | [`ColumnFamily`] closed enum + stable wire names.              |
//! | [`error`]   | [`StorageError`] umbrella over per-backend errors.             |
//! | [`batch`]   | [`WriteBatch`] backend-agnostic op log.                        |
//! | [`backend`] | [`KvBackend`] trait — the public abstraction.                  |
//! | [`snapshot`]| [`Snapshot`] trait — read-only consistent view.                |

#![cfg_attr(not(test), forbid(unsafe_code))]
#![warn(missing_docs)]

pub mod backend;
pub mod batch;
pub mod cf;
pub mod error;
pub mod snapshot;

pub use backend::KvBackend;
pub use batch::{Op, WriteBatch};
pub use cf::ColumnFamily;
pub use error::StorageError;
pub use snapshot::{KvIter, Snapshot};
```

- [ ] **Step 4: Verify build**

Run: `cargo build -p aii-storage`
Expected: `Finished ...` (no traits are implemented yet, but they should typecheck)

- [ ] **Step 5: Commit**

```bash
git add crates/aii-storage/src/backend.rs crates/aii-storage/src/snapshot.rs crates/aii-storage/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(storage): KvBackend + Snapshot traits (the public abstraction)

KvBackend exposes sync get/put/delete/write/snapshot/iter/iter_prefix
over &[u8] keys/values. Owned Vec<u8> returns avoid leaking
backend-specific PinnedSlice lifetimes into the trait. Iterator is a
Box<dyn> (GATs still maturing) — fine for the workspace, the
hot path goes through put/get rather than iteration.

Snapshot is the read-only sibling: get + iter, Send + Sync so
downstream can clone it into rayon tasks for parallel state reads.

Implementations land in the next two commits (memory + rocksdb).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: MemoryBackend (memory.rs)

**Files:**
- Create: `crates/aii-storage/src/memory.rs`
- Modify: `crates/aii-storage/src/lib.rs`

- [ ] **Step 1: Write `crates/aii-storage/src/memory.rs`**

```rust
//! In-process [`KvBackend`] backed by `BTreeMap` per column family.
//!
//! Intended for unit tests in downstream crates (aii-state, aii-block, ...)
//! that need a real storage backend without spinning up RocksDB. Snapshot
//! semantics are achieved by cloning the entire per-CF map into an `Arc`
//! at snapshot time — O(N) but acceptable for test data sizes.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};

use crate::{
    backend::KvBackend,
    batch::{Op, WriteBatch},
    cf::ColumnFamily,
    error::StorageError,
    snapshot::{KvIter, Snapshot},
};

type CfMap = BTreeMap<Vec<u8>, Vec<u8>>;
type Store = HashMap<ColumnFamily, CfMap>;

/// In-memory KV backend. Cheap to construct; loses data on drop.
#[derive(Clone, Default)]
pub struct MemoryBackend {
    inner: Arc<RwLock<Store>>,
}

impl MemoryBackend {
    /// New empty backend, with one empty `BTreeMap` per [`ColumnFamily`].
    #[must_use]
    pub fn new() -> Self {
        let mut store = Store::with_capacity(ColumnFamily::ALL.len());
        for cf in ColumnFamily::ALL {
            store.insert(*cf, CfMap::new());
        }
        Self {
            inner: Arc::new(RwLock::new(store)),
        }
    }
}

impl KvBackend for MemoryBackend {
    type Snapshot = MemorySnapshot;

    fn get(&self, cf: ColumnFamily, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        let store = self.inner.read().expect("memory backend lock poisoned");
        let cf_map = store
            .get(&cf)
            .ok_or(StorageError::InvalidColumnFamily(cf))?;
        Ok(cf_map.get(key).cloned())
    }

    fn put(&self, cf: ColumnFamily, key: &[u8], value: &[u8]) -> Result<(), StorageError> {
        let mut store = self.inner.write().expect("memory backend lock poisoned");
        let cf_map = store
            .get_mut(&cf)
            .ok_or(StorageError::InvalidColumnFamily(cf))?;
        cf_map.insert(key.to_vec(), value.to_vec());
        Ok(())
    }

    fn delete(&self, cf: ColumnFamily, key: &[u8]) -> Result<(), StorageError> {
        let mut store = self.inner.write().expect("memory backend lock poisoned");
        let cf_map = store
            .get_mut(&cf)
            .ok_or(StorageError::InvalidColumnFamily(cf))?;
        cf_map.remove(key);
        Ok(())
    }

    fn write(&self, batch: WriteBatch) -> Result<(), StorageError> {
        let mut store = self.inner.write().expect("memory backend lock poisoned");
        for op in batch.iter() {
            match op {
                Op::Put { cf, key, value } => {
                    let cf_map = store
                        .get_mut(cf)
                        .ok_or(StorageError::InvalidColumnFamily(*cf))?;
                    cf_map.insert(key.clone(), value.clone());
                }
                Op::Delete { cf, key } => {
                    let cf_map = store
                        .get_mut(cf)
                        .ok_or(StorageError::InvalidColumnFamily(*cf))?;
                    cf_map.remove(key);
                }
            }
        }
        Ok(())
    }

    fn snapshot(&self) -> Self::Snapshot {
        let store = self.inner.read().expect("memory backend lock poisoned");
        MemorySnapshot {
            store: Arc::new(store.clone()),
        }
    }

    fn iter<'a>(&'a self, cf: ColumnFamily) -> KvIter<'a> {
        let store = self.inner.read().expect("memory backend lock poisoned");
        let items: Vec<Result<(Vec<u8>, Vec<u8>), StorageError>> = match store.get(&cf) {
            Some(map) => map.iter().map(|(k, v)| Ok((k.clone(), v.clone()))).collect(),
            None => vec![Err(StorageError::InvalidColumnFamily(cf))],
        };
        Box::new(items.into_iter())
    }

    fn iter_prefix<'a>(&'a self, cf: ColumnFamily, prefix: &'a [u8]) -> KvIter<'a> {
        let store = self.inner.read().expect("memory backend lock poisoned");
        let items: Vec<Result<(Vec<u8>, Vec<u8>), StorageError>> = match store.get(&cf) {
            Some(map) => map
                .range(prefix.to_vec()..)
                .take_while(|(k, _)| k.starts_with(prefix))
                .map(|(k, v)| Ok((k.clone(), v.clone())))
                .collect(),
            None => vec![Err(StorageError::InvalidColumnFamily(cf))],
        };
        Box::new(items.into_iter())
    }
}

/// Snapshot of a [`MemoryBackend`] taken at construction time.
#[derive(Clone)]
pub struct MemorySnapshot {
    store: Arc<Store>,
}

impl Snapshot for MemorySnapshot {
    fn get(&self, cf: ColumnFamily, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        let cf_map = self
            .store
            .get(&cf)
            .ok_or(StorageError::InvalidColumnFamily(cf))?;
        Ok(cf_map.get(key).cloned())
    }

    fn iter<'a>(&'a self, cf: ColumnFamily) -> KvIter<'a> {
        let items: Vec<Result<(Vec<u8>, Vec<u8>), StorageError>> = match self.store.get(&cf) {
            Some(map) => map.iter().map(|(k, v)| Ok((k.clone(), v.clone()))).collect(),
            None => vec![Err(StorageError::InvalidColumnFamily(cf))],
        };
        Box::new(items.into_iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_then_get_round_trips() {
        let b = MemoryBackend::new();
        b.put(ColumnFamily::State, b"k", b"v").unwrap();
        assert_eq!(b.get(ColumnFamily::State, b"k").unwrap().as_deref(), Some(&b"v"[..]));
    }

    #[test]
    fn delete_removes_key() {
        let b = MemoryBackend::new();
        b.put(ColumnFamily::State, b"k", b"v").unwrap();
        b.delete(ColumnFamily::State, b"k").unwrap();
        assert_eq!(b.get(ColumnFamily::State, b"k").unwrap(), None);
    }

    #[test]
    fn snapshot_is_isolated_from_later_writes() {
        let b = MemoryBackend::new();
        b.put(ColumnFamily::State, b"k", b"v1").unwrap();
        let snap = b.snapshot();
        b.put(ColumnFamily::State, b"k", b"v2").unwrap();
        assert_eq!(snap.get(ColumnFamily::State, b"k").unwrap().as_deref(), Some(&b"v1"[..]));
        assert_eq!(b.get(ColumnFamily::State, b"k").unwrap().as_deref(), Some(&b"v2"[..]));
    }
}
```

- [ ] **Step 2: Add the module to `lib.rs`**

Edit `crates/aii-storage/src/lib.rs`. Add `pub mod memory;` after `pub mod error;` (alphabetical order; the file is `memory.rs` so it goes after `error`). Add a row to the module-map table:

```
//! | [`memory`]  | [`MemoryBackend`] — `BTreeMap` per CF, for tests.              |
```

Add `pub use memory::{MemoryBackend, MemorySnapshot};` after the existing re-exports.

- [ ] **Step 3: Run tests**

Run: `cargo test -p aii-storage memory::`
Expected: `test result: ok. 3 passed; 0 failed`

- [ ] **Step 4: Commit**

```bash
git add crates/aii-storage/src/memory.rs crates/aii-storage/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(storage): MemoryBackend (BTreeMap per CF, snapshot via Arc clone)

In-process backend for downstream-crate unit tests. Arc<RwLock<HashMap
<CF, BTreeMap>>> internally; snapshot is an Arc<HashMap<...>> clone of
the current state — O(N) but only test data sizes pass through.

3 unit tests cover round-trip, delete, and snapshot isolation. Full
conformance suite lands in a later commit (covers both memory + rocks
together).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: RocksDbBackend basics (open, put, get, delete, iter)

**Files:**
- Create: `crates/aii-storage/src/rocksdb.rs`
- Modify: `crates/aii-storage/src/lib.rs`

- [ ] **Step 1: Write `crates/aii-storage/src/rocksdb.rs`**

```rust
//! RocksDB-backed [`KvBackend`] — the production backend.
//!
//! Gated behind the `rocksdb` cargo feature (on by default). Disabling lets
//! downstream crates compile a slim version that only uses
//! [`crate::memory::MemoryBackend`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rocksdb::{ColumnFamilyDescriptor, IteratorMode, Options, ReadOptions, DB};

use crate::{
    backend::KvBackend,
    batch::{Op, WriteBatch},
    cf::ColumnFamily,
    error::StorageError,
    snapshot::{KvIter, Snapshot},
};

/// Production RocksDB-backed KV store.
#[derive(Clone)]
pub struct RocksDbBackend {
    db: Arc<DB>,
    // Keep path alive for diagnostics + `open_in_temp`'s tempdir lifecycle.
    _path: PathBuf,
}

impl RocksDbBackend {
    /// Open the DB at `path`, creating it (and every column family in
    /// [`ColumnFamily::ALL`]) if absent.
    ///
    /// # Errors
    /// Returns [`StorageError::Backend`] if RocksDB fails to open / create.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);
        opts.set_use_fsync(false);
        opts.set_bytes_per_sync(1 << 20); // 1 MiB
        opts.set_compression_type(rocksdb::DBCompressionType::Lz4);

        let cf_descs: Vec<ColumnFamilyDescriptor> = ColumnFamily::ALL
            .iter()
            .map(|cf| ColumnFamilyDescriptor::new(cf.as_str(), Options::default()))
            .collect();

        let db = DB::open_cf_descriptors(&opts, path.as_ref(), cf_descs)
            .map_err(|e| StorageError::Backend(e.to_string()))?;

        Ok(Self {
            db: Arc::new(db),
            _path: path.as_ref().to_path_buf(),
        })
    }

    /// Open a fresh DB in a private tempdir — used by unit/integration tests.
    /// The tempdir is leaked intentionally; tests run in `target/tmp/...`
    /// which the OS reaps eventually.
    ///
    /// # Errors
    /// Returns [`StorageError::Backend`] / [`StorageError::Io`] on failure.
    pub fn open_in_temp() -> Result<Self, StorageError> {
        let dir = tempfile::tempdir().map_err(StorageError::Io)?;
        // Leak the tempdir guard so it outlives this fn; the OS reaps
        // `target/tmp/.../` eventually. into_path() is stable across all
        // tempfile 3.x; .keep() was added in 3.13 and is not load-bearing.
        let path = dir.into_path();
        Self::open(path)
    }

    fn cf_handle(&self, cf: ColumnFamily) -> Result<&rocksdb::ColumnFamily, StorageError> {
        self.db
            .cf_handle(cf.as_str())
            .ok_or(StorageError::InvalidColumnFamily(cf))
    }
}

impl KvBackend for RocksDbBackend {
    type Snapshot = RocksDbSnapshot;

    fn get(&self, cf: ColumnFamily, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        let handle = self.cf_handle(cf)?;
        self.db
            .get_cf(handle, key)
            .map_err(|e| StorageError::Backend(e.to_string()))
    }

    fn put(&self, cf: ColumnFamily, key: &[u8], value: &[u8]) -> Result<(), StorageError> {
        let handle = self.cf_handle(cf)?;
        self.db
            .put_cf(handle, key, value)
            .map_err(|e| StorageError::Backend(e.to_string()))
    }

    fn delete(&self, cf: ColumnFamily, key: &[u8]) -> Result<(), StorageError> {
        let handle = self.cf_handle(cf)?;
        self.db
            .delete_cf(handle, key)
            .map_err(|e| StorageError::Backend(e.to_string()))
    }

    fn write(&self, batch: WriteBatch) -> Result<(), StorageError> {
        let mut wb = rocksdb::WriteBatch::default();
        for op in batch.iter() {
            match op {
                Op::Put { cf, key, value } => {
                    let handle = self.cf_handle(*cf)?;
                    wb.put_cf(handle, key, value);
                }
                Op::Delete { cf, key } => {
                    let handle = self.cf_handle(*cf)?;
                    wb.delete_cf(handle, key);
                }
            }
        }
        self.db
            .write(wb)
            .map_err(|e| StorageError::Backend(e.to_string()))
    }

    fn snapshot(&self) -> Self::Snapshot {
        RocksDbSnapshot {
            db: Arc::clone(&self.db),
            // SAFETY: 'static-like: snapshot holds a ref-counted DB clone
            // so the snapshot pointer cannot outlive the DB.
            snap: unsafe { std::mem::transmute::<rocksdb::Snapshot<'_>, rocksdb::Snapshot<'static>>(self.db.snapshot()) },
        }
    }

    fn iter<'a>(&'a self, cf: ColumnFamily) -> KvIter<'a> {
        let handle = match self.cf_handle(cf) {
            Ok(h) => h,
            Err(e) => return Box::new(std::iter::once(Err(e))),
        };
        Box::new(self.db.iterator_cf(handle, IteratorMode::Start).map(|kv| {
            kv.map(|(k, v)| (k.to_vec(), v.to_vec()))
                .map_err(|e| StorageError::Backend(e.to_string()))
        }))
    }

    fn iter_prefix<'a>(&'a self, cf: ColumnFamily, prefix: &'a [u8]) -> KvIter<'a> {
        let handle = match self.cf_handle(cf) {
            Ok(h) => h,
            Err(e) => return Box::new(std::iter::once(Err(e))),
        };
        let mut read_opts = ReadOptions::default();
        read_opts.set_iterate_lower_bound(prefix.to_vec());
        let upper = next_prefix_upper_bound(prefix);
        if let Some(ub) = upper {
            read_opts.set_iterate_upper_bound(ub);
        }
        Box::new(
            self.db
                .iterator_cf_opt(handle, read_opts, IteratorMode::Start)
                .map(|kv| {
                    kv.map(|(k, v)| (k.to_vec(), v.to_vec()))
                        .map_err(|e| StorageError::Backend(e.to_string()))
                }),
        )
    }
}

/// Compute the lexicographic upper bound of all keys starting with `prefix`.
/// `None` means "no finite upper bound" (the prefix is all-0xFF bytes).
fn next_prefix_upper_bound(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut out = prefix.to_vec();
    while let Some(b) = out.last_mut() {
        if *b == 0xFF {
            out.pop();
        } else {
            *b += 1;
            return Some(out);
        }
    }
    None
}

/// Read-only snapshot of a [`RocksDbBackend`].
pub struct RocksDbSnapshot {
    db: Arc<DB>,
    snap: rocksdb::Snapshot<'static>,
}

impl RocksDbSnapshot {
    fn cf_handle(&self, cf: ColumnFamily) -> Result<&rocksdb::ColumnFamily, StorageError> {
        self.db
            .cf_handle(cf.as_str())
            .ok_or(StorageError::InvalidColumnFamily(cf))
    }
}

impl Snapshot for RocksDbSnapshot {
    fn get(&self, cf: ColumnFamily, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        let handle = self.cf_handle(cf)?;
        self.snap
            .get_cf(handle, key)
            .map_err(|e| StorageError::Backend(e.to_string()))
    }

    fn iter<'a>(&'a self, cf: ColumnFamily) -> KvIter<'a> {
        let handle = match self.cf_handle(cf) {
            Ok(h) => h,
            Err(e) => return Box::new(std::iter::once(Err(e))),
        };
        Box::new(self.snap.iterator_cf(handle, IteratorMode::Start).map(|kv| {
            kv.map(|(k, v)| (k.to_vec(), v.to_vec()))
                .map_err(|e| StorageError::Backend(e.to_string()))
        }))
    }
}

// SAFETY: `Snapshot<'static>` holds a pointer derived from `Arc<DB>`; the
// snapshot's lifetime is bound to the DB clone we keep, not the original
// borrow. RocksDB snapshots are documented to be `Send + Sync` and safe to
// hand across threads as long as the DB outlives them — which `Arc<DB>`
// guarantees.
unsafe impl Send for RocksDbSnapshot {}
unsafe impl Sync for RocksDbSnapshot {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_in_temp_then_put_get_round_trips() {
        let b = RocksDbBackend::open_in_temp().unwrap();
        b.put(ColumnFamily::State, b"k", b"v").unwrap();
        assert_eq!(b.get(ColumnFamily::State, b"k").unwrap().as_deref(), Some(&b"v"[..]));
    }

    #[test]
    fn delete_removes_key() {
        let b = RocksDbBackend::open_in_temp().unwrap();
        b.put(ColumnFamily::State, b"k", b"v").unwrap();
        b.delete(ColumnFamily::State, b"k").unwrap();
        assert_eq!(b.get(ColumnFamily::State, b"k").unwrap(), None);
    }

    #[test]
    fn snapshot_is_isolated_from_later_writes() {
        let b = RocksDbBackend::open_in_temp().unwrap();
        b.put(ColumnFamily::State, b"k", b"v1").unwrap();
        let snap = b.snapshot();
        b.put(ColumnFamily::State, b"k", b"v2").unwrap();
        assert_eq!(snap.get(ColumnFamily::State, b"k").unwrap().as_deref(), Some(&b"v1"[..]));
        assert_eq!(b.get(ColumnFamily::State, b"k").unwrap().as_deref(), Some(&b"v2"[..]));
    }

    #[test]
    fn next_prefix_upper_bound_works() {
        assert_eq!(next_prefix_upper_bound(b"abc"), Some(b"abd".to_vec()));
        assert_eq!(next_prefix_upper_bound(&[0xFF, 0x00]), Some(vec![0xFF, 0x01]));
        assert_eq!(next_prefix_upper_bound(&[0xFF, 0xFF]), None);
    }
}
```

- [ ] **Step 2: This module uses `unsafe` for one `mem::transmute` of the snapshot lifetime. Allow it by adding `#![allow(unsafe_code)]` at the top of `rocksdb.rs` AND add a `#[cfg_attr(feature = "rocksdb", allow(unsafe_code))]` comment at the lib.rs module declaration. The workspace `unsafe_code = "forbid"` is overridden per-module-declaration.**

Add to the very top of `crates/aii-storage/src/rocksdb.rs`:

```rust
#![allow(unsafe_code)]
```

(Rust's `forbid` cannot be overridden by `allow` in inner attributes — but `forbid` is set in workspace lints as a *warn level*, not a *deny level* via `#![forbid]` in lib.rs. Verify: the lib.rs only has `forbid(unsafe_code)` *outside `cfg(test)`* — that is a crate-level attribute that DOES forbid override. To unblock this module, change the lib.rs to `#![cfg_attr(not(any(test, feature = "rocksdb")), forbid(unsafe_code))]`. This allows unsafe in rocksdb.rs when the feature is on, while still forbidding it elsewhere.)

Update `crates/aii-storage/src/lib.rs` top line:

```rust
#![cfg_attr(not(any(test, feature = "rocksdb")), forbid(unsafe_code))]
```

- [ ] **Step 3: Add the module to `lib.rs`**

Edit `crates/aii-storage/src/lib.rs`. Add (alphabetical position is after `memory`):

```rust
#[cfg(feature = "rocksdb")]
pub mod rocksdb;
```

Add `#[cfg(feature = "rocksdb")]\npub use rocksdb::{RocksDbBackend, RocksDbSnapshot};` after the memory re-exports.

Add this row to the module-map table:

```
//! | [`rocksdb`] | [`RocksDbBackend`] — production RocksDB backend (feature `rocksdb`). |
```

- [ ] **Step 4: Verify build (this will take 3-5 minutes the first time as librocksdb compiles)**

Run: `cargo build -p aii-storage`
Expected: `Finished ...`

- [ ] **Step 5: Run RocksDB tests**

Run: `cargo test -p aii-storage rocksdb::`
Expected: `test result: ok. 4 passed; 0 failed`

- [ ] **Step 6: Commit**

```bash
git add crates/aii-storage/src/rocksdb.rs crates/aii-storage/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(storage): RocksDbBackend — production backend over librocksdb

Wraps rocksdb 0.22 (TiKV binding) with the KvBackend trait. open() and
open_in_temp() create every CF in ColumnFamily::ALL with sane defaults
(create_if_missing, lz4 compression, bytes_per_sync=1MiB, no fsync —
suitable for testnet; mainnet will tune via aii-config).

WriteBatch is replayed into rocksdb's native WriteBatch for cross-CF
atomicity. iter_prefix uses ReadOptions iterate_lower_bound +
iterate_upper_bound (computed by walking the prefix's lex successor)
for SST-level pushdown.

One unsafe transmute is required to lift the snapshot lifetime to
'static — the Arc<DB> we keep alive guarantees correctness. The lib.rs
forbid(unsafe_code) gate is loosened to allow it specifically when the
rocksdb feature is on; the other modules keep the forbid.

4 unit tests (in-module) cover open/get/put/delete/snapshot. Full
conformance + property tests land later.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Conformance test suite (run shared tests against both backends)

**Files:**
- Create: `crates/aii-storage/tests/conformance.rs`

- [ ] **Step 1: Write the conformance suite**

```rust
//! Conformance tests parametrized over every backend.
//!
//! The `backend_tests!($name, $factory_expr)` macro generates the same
//! battery of tests against whatever backend `$factory_expr` returns.
//! Both backends are tested with the same body so any divergence in
//! semantics surfaces immediately.

use aii_storage::{ColumnFamily, KvBackend, MemoryBackend, Snapshot, WriteBatch};

macro_rules! backend_tests {
    ($mod_name:ident, $factory:expr) => {
        mod $mod_name {
            use super::*;

            fn make_backend() -> impl KvBackend {
                $factory
            }

            #[test]
            fn get_returns_none_on_missing() {
                let db = make_backend();
                assert!(db.get(ColumnFamily::State, b"missing").unwrap().is_none());
            }

            #[test]
            fn put_then_get_round_trips() {
                let db = make_backend();
                db.put(ColumnFamily::State, b"k", b"v").unwrap();
                assert_eq!(
                    db.get(ColumnFamily::State, b"k").unwrap().as_deref(),
                    Some(&b"v"[..])
                );
            }

            #[test]
            fn delete_removes_key() {
                let db = make_backend();
                db.put(ColumnFamily::State, b"k", b"v").unwrap();
                db.delete(ColumnFamily::State, b"k").unwrap();
                assert!(db.get(ColumnFamily::State, b"k").unwrap().is_none());
            }

            #[test]
            fn write_batch_atomic_across_cfs() {
                let db = make_backend();
                let mut wb = WriteBatch::new();
                wb.put(ColumnFamily::State, b"s1", b"sv")
                    .put(ColumnFamily::Headers, b"h1", b"hv")
                    .delete(ColumnFamily::Meta, b"absent");
                db.write(wb).unwrap();
                assert_eq!(db.get(ColumnFamily::State, b"s1").unwrap().as_deref(), Some(&b"sv"[..]));
                assert_eq!(db.get(ColumnFamily::Headers, b"h1").unwrap().as_deref(), Some(&b"hv"[..]));
            }

            #[test]
            fn snapshot_sees_consistent_view() {
                let db = make_backend();
                db.put(ColumnFamily::State, b"k", b"v1").unwrap();
                let snap = db.snapshot();
                db.put(ColumnFamily::State, b"k", b"v2").unwrap();
                assert_eq!(snap.get(ColumnFamily::State, b"k").unwrap().as_deref(), Some(&b"v1"[..]));
            }

            #[test]
            fn iter_returns_sorted_keys() {
                let db = make_backend();
                db.put(ColumnFamily::State, b"b", b"2").unwrap();
                db.put(ColumnFamily::State, b"a", b"1").unwrap();
                db.put(ColumnFamily::State, b"c", b"3").unwrap();
                let keys: Vec<Vec<u8>> = db
                    .iter(ColumnFamily::State)
                    .map(|r| r.unwrap().0)
                    .collect();
                assert_eq!(keys, vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]);
            }

            #[test]
            fn iter_prefix_filters_correctly() {
                let db = make_backend();
                db.put(ColumnFamily::State, b"foo:1", b"a").unwrap();
                db.put(ColumnFamily::State, b"foo:2", b"b").unwrap();
                db.put(ColumnFamily::State, b"bar:3", b"c").unwrap();
                let keys: Vec<Vec<u8>> = db
                    .iter_prefix(ColumnFamily::State, b"foo:")
                    .map(|r| r.unwrap().0)
                    .collect();
                assert_eq!(keys, vec![b"foo:1".to_vec(), b"foo:2".to_vec()]);
            }

            #[test]
            fn cross_cf_keys_dont_collide() {
                let db = make_backend();
                db.put(ColumnFamily::State, b"k", b"state-value").unwrap();
                db.put(ColumnFamily::Headers, b"k", b"headers-value").unwrap();
                assert_eq!(
                    db.get(ColumnFamily::State, b"k").unwrap().as_deref(),
                    Some(&b"state-value"[..])
                );
                assert_eq!(
                    db.get(ColumnFamily::Headers, b"k").unwrap().as_deref(),
                    Some(&b"headers-value"[..])
                );
            }
        }
    };
}

backend_tests!(memory, MemoryBackend::new());

#[cfg(feature = "rocksdb")]
backend_tests!(rocksdb, aii_storage::RocksDbBackend::open_in_temp().unwrap());
```

- [ ] **Step 2: Run conformance tests**

Run: `cargo test -p aii-storage --test conformance`
Expected: `test result: ok. 16 passed; 0 failed` (8 tests × 2 backends)

- [ ] **Step 3: Commit**

```bash
git add crates/aii-storage/tests/conformance.rs
git commit -m "$(cat <<'EOF'
test(storage): conformance suite — 8 tests x 2 backends = 16 runs

backend_tests! macro generates the same battery against MemoryBackend
and RocksDbBackend (when rocksdb feature is on). Catches semantic
divergence between backends immediately — any future "works on memory
but not rocksdb" bug fails both modules with the same name.

Tests: get-on-missing, put/get round-trip, delete, cross-CF batch
atomicity, snapshot isolation, iter sorted-key invariant, prefix
filter, cross-CF key isolation.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: Property tests (Op sequence equivalence)

**Files:**
- Create: `crates/aii-storage/tests/proptest.rs`

- [ ] **Step 1: Write the property test**

```rust
//! Property: any random sequence of `Op`s applied to MemoryBackend and
//! RocksDbBackend leaves the two with identical contents.

use aii_storage::{ColumnFamily, KvBackend, MemoryBackend, Op, WriteBatch};
use proptest::prelude::*;

fn cf_strategy() -> impl Strategy<Value = ColumnFamily> {
    prop_oneof![
        Just(ColumnFamily::State),
        Just(ColumnFamily::Headers),
        Just(ColumnFamily::Meta),
    ]
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        (cf_strategy(), proptest::collection::vec(any::<u8>(), 1..16), proptest::collection::vec(any::<u8>(), 0..32))
            .prop_map(|(cf, key, value)| Op::Put { cf, key, value }),
        (cf_strategy(), proptest::collection::vec(any::<u8>(), 1..16))
            .prop_map(|(cf, key)| Op::Delete { cf, key }),
    ]
}

fn apply(db: &impl KvBackend, ops: &[Op]) {
    let mut wb = WriteBatch::new();
    for op in ops {
        match op {
            Op::Put { cf, key, value } => { wb.put(*cf, key, value); }
            Op::Delete { cf, key } => { wb.delete(*cf, key); }
        }
    }
    db.write(wb).unwrap();
}

fn dump(db: &impl KvBackend, cf: ColumnFamily) -> Vec<(Vec<u8>, Vec<u8>)> {
    db.iter(cf).map(|r| r.unwrap()).collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]
    #[test]
    fn memory_and_rocksdb_agree_under_random_ops(
        ops in proptest::collection::vec(op_strategy(), 0..50)
    ) {
        let mem = MemoryBackend::new();
        #[cfg(feature = "rocksdb")]
        let rocks = aii_storage::RocksDbBackend::open_in_temp().unwrap();

        apply(&mem, &ops);
        #[cfg(feature = "rocksdb")]
        apply(&rocks, &ops);

        for cf in [ColumnFamily::State, ColumnFamily::Headers, ColumnFamily::Meta] {
            let mem_dump = dump(&mem, cf);
            #[cfg(feature = "rocksdb")]
            {
                let rocks_dump = dump(&rocks, cf);
                prop_assert_eq!(mem_dump.clone(), rocks_dump, "CF {} divergence", cf);
            }
            // smoke: each CF dump is itself sorted
            for w in mem_dump.windows(2) {
                prop_assert!(w[0].0 <= w[1].0);
            }
        }
    }

    #[test]
    fn snapshot_unchanged_under_concurrent_writer(
        seed_pairs in proptest::collection::vec(
            (proptest::collection::vec(any::<u8>(), 1..16), proptest::collection::vec(any::<u8>(), 0..32)),
            0..10
        )
    ) {
        let db = MemoryBackend::new();
        for (k, v) in &seed_pairs {
            db.put(ColumnFamily::State, k, v).unwrap();
        }
        let snap = db.snapshot();

        // Mutate after the snapshot.
        for (k, _) in &seed_pairs {
            db.put(ColumnFamily::State, k, b"OVERWRITTEN").unwrap();
        }

        // Snapshot must still report the original values.
        use aii_storage::Snapshot;
        for (k, v) in &seed_pairs {
            prop_assert_eq!(snap.get(ColumnFamily::State, k).unwrap().as_deref(), Some(&v[..]));
        }
    }
}
```

- [ ] **Step 2: Run the property test**

Run: `cargo test -p aii-storage --test proptest`
Expected: `test result: ok. 2 passed; 0 failed` (32 cases per property)

- [ ] **Step 3: Commit**

```bash
git add crates/aii-storage/tests/proptest.rs
git commit -m "$(cat <<'EOF'
test(storage): proptest — Op-sequence equivalence + snapshot isolation

Two properties:

1. memory_and_rocksdb_agree_under_random_ops — generate up to 50 random
   Put/Delete ops over 3 CFs, apply identically to both backends, assert
   per-CF dump equality (32 cases per run to keep the RocksDB
   open_in_temp cost reasonable).

2. snapshot_unchanged_under_concurrent_writer — seed a memory backend,
   snapshot it, overwrite every key, assert the snapshot still returns
   the pre-overwrite values.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: Benchmark — sequential write throughput ≥50k op/s

**Files:**
- Create: `crates/aii-storage/benches/write_throughput.rs`
- Modify: `crates/aii-storage/Cargo.toml` (add `criterion` as dev-dep)

- [ ] **Step 1: Add `criterion` to dev-deps**

Edit `crates/aii-storage/Cargo.toml`. In `[dev-dependencies]` add:

```toml
criterion = { version = "0.5", default-features = false, features = ["cargo_bench_support"] }
```

- [ ] **Step 2: Write the benchmark**

```rust
//! Sequential write throughput benchmark — the M0 exit gate for aii-storage.
//!
//! 100k records, each: 32-byte deterministic key (`u64::to_be_bytes` zero
//! padded) + 256-byte value. Run as individual `put_cf` calls (not batched)
//! so the number reported is the worst case the protocol layer hits when it
//! has to write one record at a time.
//!
//! The target is 50k op/s on commodity NVMe; failing that, the M0 exit
//! criterion is not met. `scripts/check_storage_perf.sh` (added later)
//! parses criterion's CSV output and asserts the threshold for CI.

use aii_storage::{ColumnFamily, KvBackend, RocksDbBackend};
use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};

const N: usize = 100_000;
const VALUE: [u8; 256] = [0xABu8; 256];

fn key_for(i: usize) -> [u8; 32] {
    let mut k = [0u8; 32];
    k[24..].copy_from_slice(&(i as u64).to_be_bytes());
    k
}

fn bench_rocksdb_sequential_put(c: &mut Criterion) {
    let mut group = c.benchmark_group("storage.rocksdb");
    group.throughput(Throughput::Elements(N as u64));
    group.sample_size(10);
    group.bench_function("sequential_put_100k_lz4", |b| {
        b.iter_batched(
            || RocksDbBackend::open_in_temp().expect("open temp db"),
            |db| {
                for i in 0..N {
                    db.put(ColumnFamily::State, &key_for(i), &VALUE).unwrap();
                }
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

criterion_group!(benches, bench_rocksdb_sequential_put);
criterion_main!(benches);
```

- [ ] **Step 3: Sanity-run the benchmark in `--quick` mode**

Run: `cargo bench -p aii-storage --bench write_throughput -- --quick`
Expected: criterion prints a `time:` and `throughput:` block. The throughput line should report something like `Throughput: 100.00 Kelem/s ± ...` (i.e. ≥ 50k ops/s on any modern SSD). If your local hardware is slower (mechanical disk, VM with throttled IO), record the actual figure; the CI gate runs on a known reference machine.

- [ ] **Step 4: Add the CI perf check script**

Create `scripts/check_storage_perf.sh`:

```bash
#!/usr/bin/env bash
# Parses criterion's estimates and asserts the storage write throughput
# meets the M0 exit gate (>= 50k op/s).

set -euo pipefail

cd "$(dirname "$0")/.."

cargo bench -p aii-storage --bench write_throughput -- --quick \
  --output-format bencher 2>&1 | tee /tmp/aii-storage-bench.out

# bencher format: `test sequential_put_100k_lz4 ... bench: <ns/iter> (...)`
NS=$(grep 'sequential_put_100k_lz4' /tmp/aii-storage-bench.out | \
     sed -E 's/.*bench:[[:space:]]+([0-9,]+).*/\1/' | tr -d ',')

if [ -z "$NS" ]; then
  echo "FAIL: could not parse benchmark output"
  exit 1
fi

OPS_PER_SEC=$(( 100000 * 1000000000 / NS ))
echo "throughput = $OPS_PER_SEC ops/sec (target >= 50000)"
if [ "$OPS_PER_SEC" -lt 50000 ]; then
  echo "FAIL: throughput $OPS_PER_SEC < 50000"
  exit 1
fi
echo "OK: $OPS_PER_SEC >= 50000"
```

Make it executable:

Run: `chmod +x scripts/check_storage_perf.sh`

- [ ] **Step 5: Commit**

```bash
git add crates/aii-storage/Cargo.toml crates/aii-storage/benches/write_throughput.rs scripts/check_storage_perf.sh
git commit -m "$(cat <<'EOF'
bench(storage): write_throughput criterion + 50k op/s CI gate

100k sequential put_cf calls (not batched) on a fresh tempdir DB per
sample. 32-byte keys + 256-byte values, lz4 compression (default
configuration). Throughput target is >=50k op/s per M0 exit criterion.

scripts/check_storage_perf.sh parses criterion bencher-format output
and exits non-zero if throughput < 50k. CI calls this script
post-merge; local dev can invoke directly.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: README + rustdoc cleanup + clippy pass

**Files:**
- Modify: `crates/aii-storage/README.md`
- Possibly modify any module file flagged by clippy

- [ ] **Step 1: Run clippy + rustdoc + format**

Run (sequentially — fix any warnings these flag):

```bash
cargo clippy -p aii-storage --all-targets --no-deps
~/.rustup/toolchains/1.94.1-x86_64-unknown-linux-gnu/bin/rustfmt crates/aii-storage/src/*.rs crates/aii-storage/tests/*.rs crates/aii-storage/benches/*.rs
cargo doc -p aii-storage --no-deps
```

Address any non-pedantic clippy warning by following its suggestion. If a `clippy::doc_markdown` warning fires, wrap the flagged token in backticks. If `must_use_candidate` fires on a public fn returning a value with no side effect, add `#[must_use]`. Re-run until clean.

- [ ] **Step 2: Replace `crates/aii-storage/README.md` with the full README**

```markdown
# aii-storage

Key-value storage abstraction for the AII protocol.

## Backends

| Backend          | Use case                                | Feature      |
|------------------|------------------------------------------|--------------|
| `RocksDbBackend` | Production / testnet / mainnet           | `rocksdb` ✅ default |
| `MemoryBackend`  | Downstream-crate unit tests              | always-on    |

Both implement the same `KvBackend` trait — downstream code is
parametric and never names a concrete backend.

## Column families

Closed set, 10 variants (`ColumnFamily::ALL`):

`Default`, `Headers`, `Bodies`, `Receipts`, `Transactions`, `State`,
`AccountStorage`, `TxLookup`, `Meta`, `MicroChain`.

Adding a CF requires a spec revision (see
`docs/superpowers/specs/2026-05-24-aii-storage-design.md`).

## Quickstart

```rust
use aii_storage::{ColumnFamily, KvBackend, RocksDbBackend, WriteBatch};

let db = RocksDbBackend::open("/tmp/aii-db")?;

// Single-op
db.put(ColumnFamily::State, b"key", b"value")?;
let v = db.get(ColumnFamily::State, b"key")?;

// Atomic batch (cross-CF)
let mut batch = WriteBatch::new();
batch.put(ColumnFamily::Headers, b"h1", b"...")
     .put(ColumnFamily::Bodies, b"h1", b"...");
db.write(batch)?;

// Read-only snapshot
let snap = db.snapshot();
// ... use `snap` while concurrent writes proceed on `db` ...
# Ok::<(), aii_storage::StorageError>(())
```

## Testing

```bash
cargo test -p aii-storage                       # unit tests (~20)
cargo test -p aii-storage --test conformance    # 8 tests x 2 backends
cargo test -p aii-storage --test proptest       # 2 properties
cargo bench -p aii-storage --bench write_throughput -- --quick
scripts/check_storage_perf.sh                   # asserts >= 50k op/s
```

## Stability

`0.0.x` — unstable; breaking changes allowed in any release until `0.1.0`.

## Roadmap

- v0.0.4 (this release): `KvBackend` + `Snapshot` traits + 10-CF enum +
  `WriteBatch` + `MemoryBackend` + `RocksDbBackend` + criterion bench
  meeting >=50k op/s gate.
- v0.0.5: TTL CF support (for `Transactions` mempool reap) + ReadCache.
- v0.1.0: Snapshot-promotion to WriteBatch (for `aii-evm` journal
  semantics); cargo-fuzz harness on the WriteBatch replay path.

## External dependencies

| crate      | version | role                                          |
|------------|---------|-----------------------------------------------|
| `rocksdb`  | 0.22    | TiKV Rust binding to librocksdb (lz4 feature) |
| `tempfile` | 3       | test sandbox dirs                             |
| `proptest` | 1       | property testing                              |
| `criterion`| 0.5     | benchmark harness                             |
```

- [ ] **Step 3: Run all tests one final time**

Run: `cargo test -p aii-storage`
Expected: every previous test still passes (~25-30 unit + 16 conformance + 2 proptest)

- [ ] **Step 4: Commit**

```bash
git add crates/aii-storage/README.md crates/aii-storage/src crates/aii-storage/tests crates/aii-storage/benches
git commit -m "$(cat <<'EOF'
docs(storage): crate README + rustdoc + clippy/fmt clean build

README covers backends, CF list, quickstart, test commands, roadmap,
external deps table. All clippy warnings addressed at workspace
default level; cargo doc emits no warnings.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 12: Release v0.0.4

**Files:**
- Modify: `Cargo.toml` (workspace root) — version 0.0.3 → 0.0.4
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Bump workspace version**

In `Cargo.toml` (workspace root), change `version = "0.0.3"` to `version = "0.0.4"` (one occurrence in `[workspace.package]`). Also change each `version = "0.0.3"` in the `[workspace.dependencies]` section's `aii-*` entries to `"0.0.4"`.

- [ ] **Step 2: Append a `## [0.0.4]` entry to `CHANGELOG.md`**

Insert this block immediately after `## [Unreleased]` and before `## [0.0.3] — 2026-05-24`:

```markdown
## [0.0.4] — YYYY-MM-DD  ← replace with today's actual date when running

### Added
- New crate `aii-storage` (M0 #4 — final basestone crate):
  - `KvBackend` trait (sync get/put/delete/write/snapshot/iter/iter_prefix)
    and `Snapshot` trait (read-only consistent view).
  - `ColumnFamily` closed enum: 10 variants covering headers / bodies /
    receipts / transactions / state / account_storage / tx_lookup / meta
    / microchain / default. Stable snake_case wire names; adding a
    variant requires a spec revision.
  - `WriteBatch` backend-agnostic op log; cross-CF atomic on commit.
  - `RocksDbBackend` (default feature `rocksdb`) — wraps rocksdb 0.22
    with lz4 compression, opens every CF via ColumnFamily::ALL.
  - `MemoryBackend` (always on) — Arc<RwLock<HashMap<CF, BTreeMap>>>
    for downstream-crate unit tests; snapshot via Arc clone.
  - `StorageError` umbrella (Backend / InvalidColumnFamily / Io).
- 8-test conformance suite parametrised over both backends (16 runs);
  2 property tests (Op-sequence equivalence + snapshot isolation);
  criterion benchmark meeting the M0 >=50k op/s sequential-write gate;
  scripts/check_storage_perf.sh CI helper.
- Workspace deps: `rocksdb 0.22`, `tempfile 3`, `criterion 0.5`.

### Changed
- Workspace version 0.0.3 → 0.0.4.

### Notes
- All four M0 basestone crates are now landed (types / codec / crypto /
  storage). M1 (state / EVM / block / net-*) begins next.
- Per spec §5.3, aii-storage is **not** published to crates.io until M2.
```

(When running this task, substitute today's UTC date for `YYYY-MM-DD`.)

- [ ] **Step 3: Final full-workspace sweep**

Run:
```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets --no-deps
cargo doc --workspace --no-deps
```

Expected: all green, no clippy warnings beyond the pre-existing pedantic ones in earlier crates.

- [ ] **Step 4: Commit the release**

```bash
git add Cargo.toml CHANGELOG.md
git commit -m "$(cat <<'EOF'
release: v0.0.4 — aii-storage (KvBackend + RocksDB + Memory)

Bumps workspace to 0.0.4 and lands the 4th and final M0 basestone
crate. CHANGELOG covers crate scope, conformance + proptest + bench
infra, dep additions. With aii-storage in place, all M0 exit criteria
from the spec are met: 4 basestone crates passing cargo test, doc
warning-free, RocksDB sequential write >=50k op/s on the bench
reference machine.

Next milestone: M1 (state / EVM / block / net-p2p / net-sync).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Acceptance criteria (whole plan)

- [ ] `cargo build --workspace` succeeds
- [ ] `cargo test --workspace` shows all crypto, codec, types tests still green AND new aii-storage tests pass (≥45 total in aii-storage between unit + conformance + proptest)
- [ ] `cargo doc -p aii-storage --no-deps` emits no warnings
- [ ] `scripts/check_storage_perf.sh` exits 0 on a developer machine with an SSD
- [ ] Workspace `Cargo.toml` version reads `0.0.4`
- [ ] `CHANGELOG.md` has a `## [0.0.4]` entry
- [ ] `git log --oneline` on `feat/aii-storage` shows 12 commits beyond the previous release (one per task above)
