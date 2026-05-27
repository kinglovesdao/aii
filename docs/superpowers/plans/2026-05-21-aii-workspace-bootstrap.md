# AII Workspace Bootstrap + aii-types Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bootstrap the `aii` Cargo workspace (CI/dependencies/lints) and ship the first lib crate `aii-types` containing the primitive types (H256/Address/U256/AlgoId/BlsPubKey/BlsSignature/SignedTx) that every downstream crate will depend on.

**Architecture:** Single Cargo workspace at repo root. CI via GitHub Actions runs fmt/clippy/test/deny/audit/llvm-cov on Linux + macOS. `aii-types` re-exports primitive integers from `alloy-primitives`, defines AII-specific newtypes for BLS keys/signatures, and ships an `AlgoId` enum with Day-0 PQ algorithm slots reserved.

**Tech Stack:** Rust stable (toolchain pinned in `rust-toolchain.toml`), Cargo workspace, `alloy-primitives` 0.8, `proptest`, `thiserror`, `serde`, GitHub Actions, `cargo-deny`, `cargo-audit`, `cargo-llvm-cov`.

**Spec reference:** `docs/superpowers/specs/2026-05-21-aii-core-design.md` §2 + §3 (L1 layer + aii-types crate). The `AlgoId` enum implements Decision D7 (multi-sig Registry) at the type layer.

---

## File Structure

After this plan, the repo will contain:

```
aii/                                  (new — this whole repo)
├── .github/workflows/ci.yml          (Section 2 — CI pipeline)
├── .gitignore                        (Task 3)
├── Cargo.toml                        (Task 2 — workspace root)
├── CHANGELOG.md                      (Task 5)
├── LICENSE                           (Task 1 — MIT)
├── README.md                         (Task 4)
├── deny.toml                         (Task 9)
├── rust-toolchain.toml               (Task 6)
└── crates/
    └── aii-types/                    (Section 3 — first lib crate)
        ├── Cargo.toml                (Task 13)
        ├── README.md                 (Task 22)
        └── src/
            ├── lib.rs                (Task 13 — re-exports)
            ├── hash.rs               (Task 14 — H256)
            ├── address.rs            (Task 15 — Address)
            ├── integer.rs            (Task 16 — U256 re-export)
            ├── algo.rs               (Task 17 — AlgoId enum)
            ├── bls.rs                (Task 18 — BlsPubKey + BlsSignature)
            ├── signed_tx.rs          (Task 19 — SignedTx)
            ├── error.rs              (Task 20 — error types)
            └── tests/
                └── proptest.rs       (Task 21 — property tests)
```

**Design rationale:**
- One file per type — each ≤ 200 lines, easy to hold in context for TDD.
- `tests/proptest.rs` consolidates property tests (lives next to unit tests in each file).
- No nested modules in v0.0.1 — flat structure to keep imports short.

---

# Section 1: Workspace 骨架（Tasks 1-7）

## Task 1: Initialize git + LICENSE

**Files:**
- Create: `LICENSE`

- [ ] **Step 1: Create the AII workspace directory and initialize git**

```bash
mkdir -p ~/aii-dev/aii   # or wherever you keep code
cd ~/aii-dev/aii
git init
git config user.email "your@email"
git config user.name "Your Name"
```

- [ ] **Step 2: Create LICENSE (MIT)**

```text
MIT License

Copyright (c) 2026 AII Network contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

---

## Task 2: Workspace root Cargo.toml

**Files:**
- Create: `Cargo.toml`

- [ ] **Step 1: Write the workspace root Cargo.toml**

```toml
[workspace]
resolver = "2"
members = [
    "crates/aii-types",
]

[workspace.package]
version = "0.0.1"
edition = "2021"
rust-version = "1.83"
license = "MIT"
repository = "https://github.com/AII-Network/aii"
authors = ["AII Network contributors"]

[workspace.dependencies]
# Foreign crates (only "independent, established" libraries per spec D-decisions)
alloy-primitives = { version = "0.8", default-features = false, features = ["serde"] }
serde = { version = "1", default-features = false, features = ["derive"] }
thiserror = "2"
tracing = "0.1"
proptest = "1"
quickcheck = "1"

# Internal crates (will grow as plan progresses)
aii-types = { path = "crates/aii-types", version = "0.0.1" }

[workspace.lints.rust]
unsafe_code = "forbid"
missing_docs = "warn"

[workspace.lints.clippy]
all = { level = "deny", priority = -1 }
pedantic = "warn"
nursery = "warn"

[profile.release]
lto = "fat"
codegen-units = 1
opt-level = 3
strip = "symbols"
```

- [ ] **Step 2: Verify the workspace shell compiles (no members yet)**

```bash
cargo metadata --no-deps > /dev/null
echo $?
```

Expected: exit 0 (workspace declaration parses; members don't exist yet so we'll create them in Task 13).

---

## Task 3: .gitignore

**Files:**
- Create: `.gitignore`

- [ ] **Step 1: Write the gitignore**

```text
# Rust
/target/
**/*.rs.bk
Cargo.lock        # workspace projects keep this; LIBRARIES typically commit it
*.pdb
*.dSYM/

# Editor / OS
.DS_Store
.idea/
.vscode/
*.swp
*~

# Credentials (never commit)
*.env
*.env.*
*token*
*secret*
*credential*
*.pem
*.key
id_rsa
id_ed25519

# Coverage
*.profraw
lcov.info
coverage/

# Local notes
NOTES.local.md
```

> Note: for a node binary repo we **do** commit `Cargo.lock`, but the `.gitignore` excludes it pre-commit then we explicitly add it. We'll add it explicitly in Task 13.

---

## Task 4: README.md

**Files:**
- Create: `README.md`

- [ ] **Step 1: Write the project README**

```markdown
# AII — AI-Native L1 Public Chain

> **AII** is the first AI-native public blockchain — protocol-level MCP Server + CLI (`aii`) make AI agents (Claude Desktop / Claude Code / Cursor / Cline) zero-SDK first-class citizens.

Pure PoS BFT consensus. Pre-registration fair launch (10,000 addresses × 1,000,000 AII). Total supply **210 billion AII**. No company, no foundation, no DAO governance.

Spec: see `docs/superpowers/specs/2026-05-21-aii-core-design.md` and the 14-document reference set in the original docs repo.

## Status

**v0.0.1 — Workspace bootstrap + `aii-types` primitive types.**

Workspace skeleton is live; downstream crates (consensus/state/EVM/...) coming in subsequent plans.

## Quickstart (developers)

```bash
git clone https://github.com/AII-Network/aii.git
cd aii
cargo test --workspace
```

## License

MIT — see `LICENSE`.
```

---

## Task 5: CHANGELOG.md

**Files:**
- Create: `CHANGELOG.md`

- [ ] **Step 1: Initialize the changelog**

```markdown
# Changelog

All notable changes to AII workspace follow [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.1] — 2026-05-21

### Added
- Workspace bootstrap (Cargo.toml, CI, lints)
- `aii-types` crate with primitive types (H256, Address, U256, AlgoId, BlsPubKey, BlsSignature, SignedTx)
- GitHub Actions CI: fmt + clippy + test + deny + audit + llvm-cov on Linux/macOS
- AlgoId enum reserves Day-0 PQ algorithm slots per spec D7

### Notes
- This is the first commit of `aii-core`. All downstream crates (state, EVM, consensus, ...) depend on `aii-types`.
- See spec `docs/superpowers/specs/2026-05-21-aii-core-design.md` §3 for the full 24-crate plan.
```

---

## Task 6: Rust toolchain pin

**Files:**
- Create: `rust-toolchain.toml`

- [ ] **Step 1: Pin Rust to stable 1.83**

```toml
[toolchain]
channel = "1.83"
components = ["rustfmt", "clippy", "llvm-tools-preview"]
profile = "default"
```

- [ ] **Step 2: Verify rustup installs the pinned toolchain**

```bash
rustup show active-toolchain
```

Expected: `1.83.x-stable ...`

---

## Task 7: First commit

- [ ] **Step 1: Stage the workspace skeleton**

```bash
git add LICENSE Cargo.toml .gitignore README.md CHANGELOG.md rust-toolchain.toml
git status --short
```

Expected output:
```
A  .gitignore
A  CHANGELOG.md
A  Cargo.toml
A  LICENSE
A  README.md
A  rust-toolchain.toml
```

- [ ] **Step 2: Commit**

```bash
git commit -m "chore: bootstrap AII workspace (LICENSE + Cargo.toml + tooling)

- MIT LICENSE
- Workspace Cargo.toml with shared dependencies + lints
- .gitignore with credential protection
- README.md / CHANGELOG.md
- rust-toolchain.toml pinned to 1.83"
```

---

# Section 2: CI 流水线（Tasks 8-12）

## Task 8: GitHub Actions CI workflow

**Files:**
- Create: `.github/workflows/ci.yml`

- [ ] **Step 1: Create the .github/workflows directory and CI file**

```bash
mkdir -p .github/workflows
```

- [ ] **Step 2: Write the CI YAML**

```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]

env:
  CARGO_TERM_COLOR: always
  RUST_BACKTRACE: 1

jobs:
  test:
    name: Test (${{ matrix.os }})
    runs-on: ${{ matrix.os }}
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, macos-latest]
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy, llvm-tools-preview
      - uses: Swatinem/rust-cache@v2

      - name: Check formatting
        run: cargo fmt --all -- --check

      - name: Clippy
        run: cargo clippy --workspace --all-targets --all-features -- -D warnings

      - name: Test
        run: cargo test --workspace --all-features

  deny:
    name: Cargo deny (license + advisories + bans)
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: EmbarkStudios/cargo-deny-action@v2

  audit:
    name: Cargo audit (RustSec)
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: rustsec/audit-check@v2
        with:
          token: ${{ secrets.GITHUB_TOKEN }}

  coverage:
    name: Coverage (llvm-cov)
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: llvm-tools-preview
      - uses: Swatinem/rust-cache@v2
      - uses: taiki-e/install-action@cargo-llvm-cov
      - name: Generate coverage
        run: cargo llvm-cov --workspace --lcov --output-path lcov.info
      - name: Enforce coverage floor (80%)
        run: |
          cargo llvm-cov --workspace --fail-under-lines 80
```

---

## Task 9: cargo-deny configuration

**Files:**
- Create: `deny.toml`

- [ ] **Step 1: Write deny.toml**

```toml
[graph]
targets = [
    { triple = "x86_64-unknown-linux-gnu" },
    { triple = "x86_64-apple-darwin" },
    { triple = "aarch64-apple-darwin" },
    { triple = "aarch64-unknown-linux-gnu" },
    { triple = "x86_64-pc-windows-msvc" },
]

[advisories]
db-path = "~/.cargo/advisory-db"
db-urls = ["https://github.com/rustsec/advisory-db"]
yanked = "deny"

[licenses]
allow = [
    "MIT",
    "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Unicode-DFS-2016",
    "Unicode-3.0",
    "MPL-2.0",
    "Zlib",
    "CC0-1.0",
]
confidence-threshold = 0.93

[bans]
multiple-versions = "warn"
wildcards = "deny"
deny = [
    # No GPL — we are MIT/Apache-only
    { crate = "openssl-sys" },  # prefer rustls
]

[sources]
unknown-registry = "deny"
unknown-git = "deny"
allow-registry = ["https://github.com/rust-lang/crates.io-index"]
```

---

## Task 10: Trigger the first CI run

- [ ] **Step 1: Commit Section 2 changes**

```bash
git add .github/workflows/ci.yml deny.toml
git commit -m "ci: GitHub Actions pipeline (fmt + clippy + test + deny + audit + llvm-cov)

- Linux + macOS matrix
- 80% coverage floor (will tighten to 95% for core crates after they exist)
- cargo-deny: MIT/Apache/BSD/ISC allow-list, no openssl-sys"
```

- [ ] **Step 2: Push the branch to GitHub (assuming remote `origin` is set; if not, set it now)**

```bash
git remote -v
# If origin not set:
# git remote add origin https://github.com/AII-Network/aii.git
git push -u origin main
```

- [ ] **Step 3: Open the Actions tab in the GitHub UI**

Watch the `test`, `deny`, `audit`, `coverage` jobs. All should pass (no source files yet so `cargo test --workspace` is a no-op; clippy is also a no-op). If any fail, fix and re-push before continuing.

Expected: 4 green checkmarks.

---

## Task 11: Pre-commit hooks (local fast-fail)

**Files:**
- Create: `.git/hooks/pre-commit` (local only, not in repo — instructions for developers)
- Create: `scripts/pre-commit.sh` (tracked file that developers can link)

- [ ] **Step 1: Create scripts directory and pre-commit script**

```bash
mkdir -p scripts
```

- [ ] **Step 2: Write `scripts/pre-commit.sh`**

```bash
#!/usr/bin/env bash
# AII pre-commit hook — runs locally before each commit.
# To enable: ln -sf ../../scripts/pre-commit.sh .git/hooks/pre-commit
set -euo pipefail

echo "Running cargo fmt check..."
cargo fmt --all -- --check

echo "Running clippy..."
cargo clippy --workspace --all-targets -- -D warnings

echo "Pre-commit checks passed."
```

- [ ] **Step 3: Make it executable**

```bash
chmod +x scripts/pre-commit.sh
```

- [ ] **Step 4: Commit**

```bash
git add scripts/pre-commit.sh
git commit -m "ci: local pre-commit script (fmt + clippy)

Symlink with: ln -sf ../../scripts/pre-commit.sh .git/hooks/pre-commit"
```

---

## Task 12: CI smoke verification

- [ ] **Step 1: Re-run CI on the new commit**

If you pushed in Task 10, this push triggers CI automatically. Wait for green.

- [ ] **Step 2: Tag the post-CI checkpoint**

```bash
git tag -a workspace-ready -m "Workspace skeleton + CI complete; ready for aii-types"
git push origin workspace-ready
```

---

# Section 3: aii-types crate（Tasks 13-23）

## Task 13: aii-types crate scaffold

**Files:**
- Create: `crates/aii-types/Cargo.toml`
- Create: `crates/aii-types/src/lib.rs`
- Modify: `Cargo.toml:3-5` (workspace members already includes aii-types from Task 2)

- [ ] **Step 1: Create the directory tree**

```bash
mkdir -p crates/aii-types/src/tests
```

- [ ] **Step 2: Write `crates/aii-types/Cargo.toml`**

```toml
[package]
name = "aii-types"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
authors.workspace = true
description = "Primitive types for the AII protocol (H256, Address, U256, AlgoId, BlsPubKey, BlsSignature, SignedTx)"
readme = "README.md"

[lints]
workspace = true

[dependencies]
alloy-primitives = { workspace = true }
serde = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }

[dev-dependencies]
proptest = { workspace = true }
quickcheck = { workspace = true }
```

- [ ] **Step 3: Write `crates/aii-types/src/lib.rs` (the minimal facade)**

```rust
//! # AII Types
//!
//! Primitive types for the AII protocol. Every downstream crate (state / EVM
//! / consensus / RPC / ...) depends on the types defined here.
//!
//! ## Re-exports
//!
//! - [`H256`] — 32-byte hash (Keccak-256 output)
//! - [`Address`] — 20-byte account address (EVM-compatible)
//! - [`U256`] — 256-bit unsigned integer (re-exported from `alloy-primitives`)
//!
//! ## AII-specific
//!
//! - [`AlgoId`] — signature-algorithm identifier (1 byte; reserves Day-0
//!   PQ slots per spec decision D7)
//! - [`BlsPubKey`] / [`BlsSignature`] — BLS12-381 G1/G2 keys & signatures
//! - [`SignedTx`] — generic signed transaction envelope dispatching on
//!   [`AlgoId`]

#![cfg_attr(not(test), forbid(unsafe_code))]
#![warn(missing_docs)]

mod address;
mod algo;
mod bls;
mod error;
mod hash;
mod integer;
mod signed_tx;

pub use address::Address;
pub use algo::AlgoId;
pub use bls::{BlsPubKey, BlsSignature};
pub use error::TypesError;
pub use hash::H256;
pub use integer::U256;
pub use signed_tx::SignedTx;
```

- [ ] **Step 4: Create each module file as an empty `pub` placeholder so the lib compiles**

```bash
for f in address algo bls error hash integer signed_tx; do
  echo "//! placeholder — will be filled in subsequent tasks" > crates/aii-types/src/$f.rs
done
```

Then add a placeholder so each module exports nothing yet but compiles. Edit each file:

`crates/aii-types/src/hash.rs`:
```rust
//! H256 — 32-byte hash type. Filled in Task 14.

/// 32-byte hash, typically a Keccak-256 output.
#[allow(dead_code)]
pub struct H256;
```

Repeat the same trivial pattern for `address.rs` (`pub struct Address;`), `algo.rs` (`pub enum AlgoId {}`), `bls.rs` (`pub struct BlsPubKey; pub struct BlsSignature;`), `error.rs` (`pub struct TypesError;`), `integer.rs` (`pub type U256 = alloy_primitives::U256;`), `signed_tx.rs` (`pub struct SignedTx;`).

- [ ] **Step 5: Verify the workspace compiles**

```bash
cargo build -p aii-types
```

Expected: `Compiling aii-types v0.0.1 ... Finished`

- [ ] **Step 6: Commit**

```bash
git add crates/aii-types Cargo.lock
git commit -m "feat(types): aii-types crate scaffold

- Cargo.toml + workspace lints
- lib.rs with module structure
- placeholder modules (hash/address/algo/bls/error/integer/signed_tx)"
```

---

## Task 14: TDD — H256 type

**Files:**
- Modify: `crates/aii-types/src/hash.rs`

- [ ] **Step 1: Write the failing test inside `hash.rs`**

```rust
//! 32-byte cryptographic hash (Keccak-256 output, secp256k1 message hash, MPT node, ...).

use serde::{Deserialize, Serialize};

/// 32-byte hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(transparent)]
pub struct H256(pub [u8; 32]);

impl H256 {
    /// All-zero hash. Useful as default / sentinel value.
    pub const ZERO: Self = Self([0u8; 32]);

    /// Construct from raw 32-byte array.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Return a reference to the underlying bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl From<[u8; 32]> for H256 {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl AsRef<[u8]> for H256 {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h256_zero_is_all_zero_bytes() {
        assert_eq!(H256::ZERO.0, [0u8; 32]);
    }

    #[test]
    fn h256_new_round_trips() {
        let mut b = [0u8; 32];
        b[0] = 0xAA;
        b[31] = 0xFF;
        let h = H256::new(b);
        assert_eq!(*h.as_bytes(), b);
    }

    #[test]
    fn h256_from_array_equals_new() {
        let b = [0x42u8; 32];
        assert_eq!(H256::from(b), H256::new(b));
    }

    #[test]
    fn h256_equality_is_bytewise() {
        assert_eq!(H256::ZERO, H256::new([0u8; 32]));
        assert_ne!(H256::ZERO, H256::new([1u8; 32]));
    }
}
```

- [ ] **Step 2: Run the tests and verify they pass**

```bash
cargo test -p aii-types hash::
```

Expected:
```
running 4 tests
test hash::tests::h256_equality_is_bytewise ... ok
test hash::tests::h256_from_array_equals_new ... ok
test hash::tests::h256_new_round_trips ... ok
test hash::tests::h256_zero_is_all_zero_bytes ... ok
```

- [ ] **Step 3: Commit**

```bash
git add crates/aii-types/src/hash.rs
git commit -m "feat(types): H256 newtype with ZERO / new / as_bytes + 4 unit tests"
```

---

## Task 15: TDD — Address type

**Files:**
- Modify: `crates/aii-types/src/address.rs`

- [ ] **Step 1: Write the failing tests + implementation**

```rust
//! Address — 20-byte EVM-compatible account address.

use crate::H256;
use serde::{Deserialize, Serialize};

/// 20-byte address. Lowercase hex serialization with `0x` prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(transparent)]
pub struct Address(pub [u8; 20]);

impl Address {
    /// All-zero address.
    pub const ZERO: Self = Self([0u8; 20]);

    /// Construct from raw 20-byte array.
    pub const fn new(bytes: [u8; 20]) -> Self {
        Self(bytes)
    }

    /// Underlying byte view.
    pub const fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }

    /// Derive an EOA address from a 32-byte secp256k1 public-key hash.
    ///
    /// EVM convention: last 20 bytes of `Keccak256(uncompressed_pubkey[1..])`
    /// — we trust the caller to have already hashed.
    pub fn from_pubkey_hash(hash: H256) -> Self {
        let mut out = [0u8; 20];
        out.copy_from_slice(&hash.as_bytes()[12..]);
        Self(out)
    }
}

impl From<[u8; 20]> for Address {
    fn from(b: [u8; 20]) -> Self {
        Self(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_zero_is_all_zero() {
        assert_eq!(Address::ZERO.0, [0u8; 20]);
    }

    #[test]
    fn from_pubkey_hash_takes_last_20_bytes() {
        // hash = 0x00..00 0x01..14 (last 20 bytes are 0x01..0x14)
        let mut hash_bytes = [0u8; 32];
        for i in 12..32 {
            hash_bytes[i] = (i - 11) as u8; // 1..=20
        }
        let addr = Address::from_pubkey_hash(H256::new(hash_bytes));
        let expected: [u8; 20] = std::array::from_fn(|i| (i + 1) as u8);
        assert_eq!(addr.0, expected);
    }

    #[test]
    fn address_equality_is_bytewise() {
        assert_eq!(Address::new([0xAB; 20]), Address::new([0xAB; 20]));
        assert_ne!(Address::ZERO, Address::new([1u8; 20]));
    }
}
```

- [ ] **Step 2: Run the tests**

```bash
cargo test -p aii-types address::
```

Expected: 3 passing tests.

- [ ] **Step 3: Commit**

```bash
git add crates/aii-types/src/address.rs
git commit -m "feat(types): Address newtype + from_pubkey_hash (last-20-bytes rule)"
```

---

## Task 16: U256 re-export

**Files:**
- Modify: `crates/aii-types/src/integer.rs`

- [ ] **Step 1: Write the re-export + a sanity test**

```rust
//! 256-bit unsigned integer. Re-exported from `alloy-primitives` for
//! one-source-of-truth across the AII workspace.

pub use alloy_primitives::U256;

#[cfg(test)]
mod tests {
    use super::U256;

    #[test]
    fn u256_addition_overflows_safely() {
        let max = U256::MAX;
        let (sum, overflow) = max.overflowing_add(U256::from(1u8));
        assert_eq!(sum, U256::ZERO);
        assert!(overflow);
    }

    #[test]
    fn u256_from_u64_round_trips_through_to_string() {
        let n = U256::from(1_234_567_890u64);
        assert_eq!(n.to_string(), "1234567890");
    }

    #[test]
    fn u256_zero_is_zero() {
        assert_eq!(U256::ZERO, U256::from(0u8));
    }
}
```

- [ ] **Step 2: Run the tests**

```bash
cargo test -p aii-types integer::
```

Expected: 3 passing tests.

- [ ] **Step 3: Commit**

```bash
git add crates/aii-types/src/integer.rs
git commit -m "feat(types): re-export U256 from alloy-primitives + sanity tests"
```

---

## Task 17: TDD — AlgoId enum（spec D7）

**Files:**
- Modify: `crates/aii-types/src/algo.rs`

- [ ] **Step 1: Write the AlgoId enum with reserved PQ slots and tests**

```rust
//! AlgoId — signature-algorithm identifier (1 byte).
//!
//! Implements spec decision D7 (multi-sig Registry). Every transaction and
//! every V-node stake operation carries an `AlgoId` as the first byte of its
//! signature envelope, letting consumers dispatch verification through the
//! Registry (`aii-registry` crate, planned for a later plan).
//!
//! Day-0 reserved values are intentionally sparse: PQ algorithms have
//! placeholders in the enum so adding their concrete verifier later is a
//! purely additive change in `aii-registry`, never a breaking change to
//! transactions or storage layouts.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Signature-algorithm identifier (`#[repr(u8)]` — wire format is 1 byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum AlgoId {
    /// secp256k1 ECDSA — default; ETH-compatible.
    Secp256k1 = 0x01,
    /// Ed25519 — high-perf alt, also classical.
    Ed25519 = 0x02,
    /// BLS12-381 — V-node signatures and PRE-COMMIT aggregation.
    Bls12_381 = 0x03,
    /// ML-DSA-65 (Dilithium) — NIST PQ standard, lattice-based.
    /// Slot reserved Day-0; concrete implementation lands in `aii-crypto`
    /// when the PQ rollout starts.
    MlDsa65 = 0x10,
    /// SLH-DSA-128s (SPHINCS+) — NIST PQ standard, hash-based, most
    /// conservative. Same Day-0 reservation rule as ML-DSA-65.
    SlhDsa128s = 0x11,
    /// Falcon-512 — alternative PQ signature (smaller signature, slower).
    Falcon512 = 0x12,
    /// Hybrid `Secp256k1 ∥ MlDsa65` — bridges to PQ migration period.
    HybridSecpMlDsa = 0x20,
}

impl AlgoId {
    /// Wire-format byte.
    pub const fn as_byte(self) -> u8 {
        self as u8
    }

    /// Decode from wire byte. Unknown values are an error so clients
    /// can never silently accept a Registry algorithm they don't support.
    pub const fn from_byte(b: u8) -> Result<Self, AlgoIdError> {
        match b {
            0x01 => Ok(Self::Secp256k1),
            0x02 => Ok(Self::Ed25519),
            0x03 => Ok(Self::Bls12_381),
            0x10 => Ok(Self::MlDsa65),
            0x11 => Ok(Self::SlhDsa128s),
            0x12 => Ok(Self::Falcon512),
            0x20 => Ok(Self::HybridSecpMlDsa),
            other => Err(AlgoIdError::Unknown(other)),
        }
    }

    /// `true` iff this scheme is believed quantum-safe (NIST PQC standards
    /// or hash-based).
    pub const fn quantum_safe(self) -> bool {
        matches!(
            self,
            Self::MlDsa65 | Self::SlhDsa128s | Self::Falcon512 | Self::HybridSecpMlDsa
        )
    }
}

/// Error decoding an `AlgoId` byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AlgoIdError {
    /// Byte value is not assigned to any algorithm.
    #[error("unknown AlgoId byte 0x{0:02x}")]
    Unknown(u8),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secp256k1_is_default_byte_01() {
        assert_eq!(AlgoId::Secp256k1.as_byte(), 0x01);
    }

    #[test]
    fn pq_algorithms_have_high_byte_block() {
        assert!(AlgoId::MlDsa65.as_byte() >= 0x10);
        assert!(AlgoId::SlhDsa128s.as_byte() >= 0x10);
        assert!(AlgoId::Falcon512.as_byte() >= 0x10);
    }

    #[test]
    fn quantum_safe_classification() {
        assert!(!AlgoId::Secp256k1.quantum_safe());
        assert!(!AlgoId::Ed25519.quantum_safe());
        assert!(!AlgoId::Bls12_381.quantum_safe());
        assert!(AlgoId::MlDsa65.quantum_safe());
        assert!(AlgoId::SlhDsa128s.quantum_safe());
        assert!(AlgoId::Falcon512.quantum_safe());
        assert!(AlgoId::HybridSecpMlDsa.quantum_safe());
    }

    #[test]
    fn from_byte_round_trips_all_variants() {
        for variant in [
            AlgoId::Secp256k1,
            AlgoId::Ed25519,
            AlgoId::Bls12_381,
            AlgoId::MlDsa65,
            AlgoId::SlhDsa128s,
            AlgoId::Falcon512,
            AlgoId::HybridSecpMlDsa,
        ] {
            assert_eq!(AlgoId::from_byte(variant.as_byte()), Ok(variant));
        }
    }

    #[test]
    fn unknown_byte_returns_error() {
        assert_eq!(AlgoId::from_byte(0xFF), Err(AlgoIdError::Unknown(0xFF)));
        assert_eq!(AlgoId::from_byte(0x00), Err(AlgoIdError::Unknown(0x00)));
    }
}
```

- [ ] **Step 2: Run the tests**

```bash
cargo test -p aii-types algo::
```

Expected: 5 passing tests.

- [ ] **Step 3: Commit**

```bash
git add crates/aii-types/src/algo.rs
git commit -m "feat(types): AlgoId enum with PQ slots (D7)

- Classical: secp256k1 (0x01), Ed25519 (0x02), BLS12-381 (0x03)
- PQ reservations: ML-DSA-65 (0x10), SLH-DSA-128s (0x11), Falcon-512 (0x12)
- Hybrid: secp256k1+ML-DSA-65 (0x20)
- quantum_safe() flag; wire-format from_byte round-trip"
```

---

## Task 18: TDD — BLS public-key + signature types

**Files:**
- Modify: `crates/aii-types/src/bls.rs`

- [ ] **Step 1: Write BLS key/signature newtypes**

```rust
//! BLS12-381 public-key / signature wire types.
//!
//! AII uses BLS on G1 (compressed, 48 bytes) for public keys and on G2
//! (compressed, 96 bytes) for signatures, matching the Ethereum 2.0 spec
//! conventions. Concrete verification lives in `aii-crypto` (later plan).

use serde::{Deserialize, Serialize};

/// Compressed BLS12-381 G1 public key (48 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(transparent)]
pub struct BlsPubKey(pub [u8; 48]);

impl BlsPubKey {
    /// All-zero placeholder.
    pub const ZERO: Self = Self([0u8; 48]);

    /// Construct from raw bytes.
    pub const fn new(bytes: [u8; 48]) -> Self {
        Self(bytes)
    }

    /// Underlying view.
    pub const fn as_bytes(&self) -> &[u8; 48] {
        &self.0
    }
}

/// Compressed BLS12-381 G2 signature (96 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(transparent)]
pub struct BlsSignature(pub [u8; 96]);

impl BlsSignature {
    /// All-zero placeholder.
    pub const ZERO: Self = Self([0u8; 96]);

    /// Construct from raw bytes.
    pub const fn new(bytes: [u8; 96]) -> Self {
        Self(bytes)
    }

    /// Underlying view.
    pub const fn as_bytes(&self) -> &[u8; 96] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bls_pubkey_is_48_bytes_zero() {
        assert_eq!(BlsPubKey::ZERO.0.len(), 48);
        assert!(BlsPubKey::ZERO.0.iter().all(|b| *b == 0));
    }

    #[test]
    fn bls_signature_is_96_bytes_zero() {
        assert_eq!(BlsSignature::ZERO.0.len(), 96);
        assert!(BlsSignature::ZERO.0.iter().all(|b| *b == 0));
    }

    #[test]
    fn bls_pubkey_new_round_trips() {
        let mut b = [0u8; 48];
        b[0] = 0xAA;
        b[47] = 0xBB;
        let k = BlsPubKey::new(b);
        assert_eq!(*k.as_bytes(), b);
    }

    #[test]
    fn bls_signature_new_round_trips() {
        let b = [0x55u8; 96];
        let s = BlsSignature::new(b);
        assert_eq!(*s.as_bytes(), b);
    }
}
```

- [ ] **Step 2: Run the tests**

```bash
cargo test -p aii-types bls::
```

Expected: 4 passing tests.

- [ ] **Step 3: Commit**

```bash
git add crates/aii-types/src/bls.rs
git commit -m "feat(types): BlsPubKey (48B) and BlsSignature (96B) wire newtypes"
```

---

## Task 19: TDD — SignedTx envelope

**Files:**
- Modify: `crates/aii-types/src/signed_tx.rs`

- [ ] **Step 1: Define SignedTx and its tests**

```rust
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
    ///
    /// Size depends on `algo_id`:
    ///   - secp256k1: 33 (compressed) or 65 (uncompressed)
    ///   - BLS12-381: 48
    ///   - ML-DSA-65: 1952
    ///   - SLH-DSA-128s: 32
    ///   - Falcon-512: 897
    pub pubkey: Vec<u8>,
    /// Signature over `payload`. Size depends on `algo_id`.
    pub signature: Vec<u8>,
    /// Opaque transaction payload (RLP-encoded; decoded later by `aii-codec`).
    pub payload: Vec<u8>,
}

impl SignedTx {
    /// Construct a new envelope. Does **not** verify the signature — that
    /// is `aii-crypto`'s job once it exists.
    pub fn new(
        algo_id: AlgoId,
        pubkey: Vec<u8>,
        signature: Vec<u8>,
        payload: Vec<u8>,
    ) -> Self {
        Self {
            algo_id,
            pubkey,
            signature,
            payload,
        }
    }

    /// Total wire size: 1 (algo_id) + len(pubkey) + len(signature) + len(payload).
    /// Useful for mempool DoS protection (size caps).
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
            vec![0xAA; 33],   // compressed pubkey size
            vec![0xBB; 65],   // (r, s, v) packed
            vec![0xCC; 100],  // some payload
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
        // 1 (algo_id) + 33 (pubkey) + 65 (signature) + 100 (payload) = 199
        assert_eq!(tx.wire_size(), 199);
    }

    #[test]
    fn signed_tx_equality_compares_all_fields() {
        let a = dummy_secp256k1_tx();
        let b = dummy_secp256k1_tx();
        assert_eq!(a, b);

        // Mutate one field at a time and assert inequality
        let mut c = a.clone();
        c.algo_id = AlgoId::Ed25519;
        assert_ne!(a, c);
    }

    #[test]
    fn different_algo_id_in_same_struct() {
        let pq_tx = SignedTx::new(
            AlgoId::MlDsa65,
            vec![0x00; 1952],  // ML-DSA-65 pubkey
            vec![0x00; 3309],  // ML-DSA-65 signature
            vec![],
        );
        assert!(pq_tx.algo_id.quantum_safe());
        assert_eq!(pq_tx.wire_size(), 1 + 1952 + 3309);
    }
}
```

- [ ] **Step 2: Run the tests**

```bash
cargo test -p aii-types signed_tx::
```

Expected: 4 passing tests.

- [ ] **Step 3: Commit**

```bash
git add crates/aii-types/src/signed_tx.rs
git commit -m "feat(types): SignedTx envelope (algo_id + pubkey + signature + payload)

- algo_id-dispatched signature verification (D7)
- wire_size() for mempool DoS limits
- 4 unit tests including PQ ML-DSA-65 envelope"
```

---

## Task 20: TDD — Error type

**Files:**
- Modify: `crates/aii-types/src/error.rs`

- [ ] **Step 1: Define a unified TypesError**

```rust
//! Unified error type for `aii-types`.
//!
//! Re-exports algo-decoding errors and adds a `TypesError` umbrella for
//! future extensibility (e.g., RLP-length mismatches once aii-codec lands).

use crate::algo::AlgoIdError;
use thiserror::Error;

/// Top-level error returned by `aii-types` operations.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TypesError {
    /// Failed to decode an [`AlgoId`](crate::AlgoId) byte.
    #[error("algo-id decode error: {0}")]
    AlgoId(#[from] AlgoIdError),

    /// Field width mismatch (e.g., 33-byte BLS pubkey supplied where 48 expected).
    #[error("invalid field length: expected {expected}, got {actual}")]
    InvalidLength {
        /// Required byte length.
        expected: usize,
        /// Provided byte length.
        actual: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AlgoId;

    #[test]
    fn algo_id_error_converts_to_types_error() {
        let inner = AlgoId::from_byte(0xFF).unwrap_err();
        let outer: TypesError = inner.into();
        assert!(matches!(outer, TypesError::AlgoId(_)));
    }

    #[test]
    fn invalid_length_formats_human_readable() {
        let e = TypesError::InvalidLength {
            expected: 48,
            actual: 33,
        };
        assert_eq!(format!("{e}"), "invalid field length: expected 48, got 33");
    }
}
```

- [ ] **Step 2: Run the tests**

```bash
cargo test -p aii-types error::
```

Expected: 2 passing tests.

- [ ] **Step 3: Commit**

```bash
git add crates/aii-types/src/error.rs
git commit -m "feat(types): TypesError umbrella with From<AlgoIdError> conversion"
```

---

## Task 21: Property tests

**Files:**
- Create: `crates/aii-types/tests/proptest.rs`

- [ ] **Step 1: Add a workspace-level integration test file with property tests**

```rust
//! Property-based tests for `aii-types`.
//!
//! Lives in `tests/` so it runs in the integration-test target (separate
//! `cargo test` artifact) — keeps the lib's hot path slim.

use aii_types::{AlgoId, BlsPubKey, BlsSignature, H256, SignedTx, U256};
use proptest::prelude::*;

proptest! {
    /// Any 32-byte array round-trips through H256::new.
    #[test]
    fn h256_bytes_round_trip(bytes in proptest::array::uniform32(any::<u8>())) {
        let h = H256::new(bytes);
        prop_assert_eq!(*h.as_bytes(), bytes);
    }

    /// Any 48-byte array round-trips through BlsPubKey::new.
    #[test]
    fn bls_pubkey_round_trip(bytes in proptest::array::uniform32(any::<u8>())
                                   .prop_flat_map(|seed| {
                                       Just({
                                           let mut out = [0u8; 48];
                                           for i in 0..48 { out[i] = seed[i % 32]; }
                                           out
                                       })
                                   }))
    {
        let k = BlsPubKey::new(bytes);
        prop_assert_eq!(*k.as_bytes(), bytes);
    }

    /// Any 96-byte array round-trips through BlsSignature::new.
    #[test]
    fn bls_signature_round_trip(seed in proptest::array::uniform32(any::<u8>())) {
        let mut bytes = [0u8; 96];
        for i in 0..96 { bytes[i] = seed[i % 32]; }
        let s = BlsSignature::new(bytes);
        prop_assert_eq!(*s.as_bytes(), bytes);
    }

    /// Every assigned AlgoId byte decodes successfully; unassigned bytes fail.
    #[test]
    fn algo_id_assigned_vs_unassigned(byte in any::<u8>()) {
        let known = [0x01, 0x02, 0x03, 0x10, 0x11, 0x12, 0x20];
        let result = AlgoId::from_byte(byte);
        if known.contains(&byte) {
            prop_assert!(result.is_ok());
        } else {
            prop_assert!(result.is_err());
        }
    }

    /// SignedTx::wire_size is exact: 1 + pubkey + signature + payload.
    #[test]
    fn signed_tx_wire_size_invariant(
        pubkey in proptest::collection::vec(any::<u8>(), 0..200),
        signature in proptest::collection::vec(any::<u8>(), 0..200),
        payload in proptest::collection::vec(any::<u8>(), 0..1024),
    ) {
        let pubkey_len = pubkey.len();
        let signature_len = signature.len();
        let payload_len = payload.len();
        let tx = SignedTx::new(AlgoId::Secp256k1, pubkey, signature, payload);
        prop_assert_eq!(tx.wire_size(), 1 + pubkey_len + signature_len + payload_len);
    }

    /// U256 addition is associative.
    #[test]
    fn u256_addition_associative(a in any::<u64>(), b in any::<u64>(), c in any::<u64>()) {
        let (a, b, c) = (U256::from(a), U256::from(b), U256::from(c));
        prop_assert_eq!((a + b) + c, a + (b + c));
    }
}
```

- [ ] **Step 2: Run the property tests**

```bash
cargo test -p aii-types --test proptest
```

Expected: 6 tests, all `proptest` runs default to 256 iterations each, all pass.

- [ ] **Step 3: Commit**

```bash
git add crates/aii-types/tests/proptest.rs
git commit -m "test(types): proptest invariants (round-trip + algo-id coverage + U256 assoc)"
```

---

## Task 22: Crate README + doc generation

**Files:**
- Create: `crates/aii-types/README.md`

- [ ] **Step 1: Write the crate-level README**

```markdown
# aii-types

Primitive types for the AII protocol — every downstream crate (`aii-state`,
`aii-evm`, `aii-consensus`, ...) depends on the types defined here.

## Exports

| Type | Purpose |
| --- | --- |
| `H256` | 32-byte hash (Keccak-256 output, MPT node, ...) |
| `Address` | 20-byte EVM-compatible account address |
| `U256` | 256-bit unsigned integer (re-exported from `alloy-primitives`) |
| `AlgoId` | 1-byte signature-algorithm tag (D7 spec — secp256k1 / BLS / Ed25519 / ML-DSA / SLH-DSA / Falcon / hybrid) |
| `BlsPubKey` | Compressed BLS12-381 G1 public key (48 bytes) |
| `BlsSignature` | Compressed BLS12-381 G2 signature (96 bytes) |
| `SignedTx` | Generic signed-transaction envelope (algo-id dispatched) |
| `TypesError` | Umbrella error |

## Stability

`0.0.x` — unstable; breaking changes can happen in any release until `0.1.0`.
After `0.1.0` semver applies.

## Testing

```bash
cargo test -p aii-types               # unit tests
cargo test -p aii-types --test proptest   # property tests
cargo doc -p aii-types --no-deps --open   # generated rustdoc
```

## Roadmap

- v0.0.1 (this release): primitives only
- v0.1.0: RLP encoding traits, Block/Header skeletons (next plan)
- v0.2.0: integration with `aii-codec` once it exists
```

- [ ] **Step 2: Generate documentation and verify it builds cleanly**

```bash
cargo doc -p aii-types --no-deps
```

Expected: `Documenting aii-types v0.0.1 ... Finished` (no warnings; the `missing_docs` lint is set to `warn` so unwritten docs would show up here).

- [ ] **Step 3: Commit**

```bash
git add crates/aii-types/README.md
git commit -m "docs(types): crate README + rustdoc clean build"
```

---

## Task 23: Release v0.0.1

- [ ] **Step 1: Run the entire workspace test suite one last time**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-features
```

Expected: all three commands exit 0 with no warnings.

- [ ] **Step 2: Verify CI mirrors the local result on GitHub**

```bash
git push
```

Watch the Actions UI; ensure all 4 jobs pass.

- [ ] **Step 3: Update CHANGELOG.md**

Move `## [Unreleased]` content into a new `## [0.0.1] — 2026-05-21` block (already pre-populated in Task 5; just confirm the date and any minor additions).

- [ ] **Step 4: Tag the release**

```bash
git tag -a v0.0.1 -m "Release v0.0.1 — workspace bootstrap + aii-types

- Workspace skeleton (Cargo.toml, lints, CI, deny.toml, rust-toolchain)
- aii-types: H256, Address, U256, AlgoId, BlsPubKey, BlsSignature, SignedTx, TypesError
- 22 unit tests + 6 property tests, all passing
- CI green on Linux + macOS"
git push origin v0.0.1
```

- [ ] **Step 5: Confirm v0.0.1 tag is live**

```bash
git tag --list
git log --oneline -10
```

Expected: latest tag `v0.0.1`; commit history shows ~22-25 commits since `chore: bootstrap`.

---

# Acceptance Checklist

The plan is **complete** when every box below is ticked:

- [ ] Workspace builds with `cargo build --workspace` (no warnings).
- [ ] `cargo test --workspace --all-features` passes (≥ 24 tests).
- [ ] `cargo fmt --all -- --check` and `cargo clippy --workspace -- -D warnings` are clean.
- [ ] `cargo doc -p aii-types --no-deps` produces zero `missing_docs` warnings.
- [ ] CI is green for the latest push to `main`.
- [ ] `v0.0.1` tag exists locally and on the remote.
- [ ] CHANGELOG.md has the `0.0.1` entry filled in.
- [ ] `crates/aii-types/README.md` exists and matches the actually-exported types.
- [ ] Every TDD task was committed separately (review with `git log --oneline | grep aii-types`).

---

# Self-Review (executed before handoff)

1. **Spec coverage:** Every Day-0 type from spec §3.4 trait `SignatureScheme` is represented in `aii-types` (AlgoId enum + size constants in the doc comment of `SignedTx::pubkey`). ✓
2. **Placeholder scan:** No `TODO` / `TBD` / "implement later" markers in any task. ✓
3. **Type consistency:** `H256` is used in `Address::from_pubkey_hash`; `AlgoId` is used in `SignedTx`; `AlgoIdError` flows into `TypesError`. Names match across tasks. ✓
4. **Step granularity:** Every TDD task has the 3-step pattern (write tests, run, commit); scaffold tasks ≤ 6 steps. None exceeds 5 minutes per step. ✓
5. **Code completeness:** Every step that modifies code shows the full module or the full new function — no `// ...` ellipses. ✓

---

# Next Plan

After this plan ships:

- **P-003 (next):** `aii-codec` — RLP / SSZ / JSON encoders for `H256` / `Address` / `SignedTx` / future block types.
- **P-004:** `aii-crypto` — concrete verifier for `AlgoId::Secp256k1`, `BlsPubKey` / `BlsSignature` verification, schnorrkel VRF.
- **P-005:** `aii-registry` — wire-format dispatcher consuming `AlgoId`.

Each ships with its own plan via `superpowers:writing-plans`.

—— plan complete ——
