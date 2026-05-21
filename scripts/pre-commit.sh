#!/usr/bin/env bash
# AII pre-commit hook — runs locally before each commit.
# To enable: ln -sf ../../scripts/pre-commit.sh .git/hooks/pre-commit
set -euo pipefail

echo "Running cargo fmt check..."
cargo fmt --all -- --check

echo "Running clippy..."
cargo clippy --workspace --all-targets -- -D warnings

echo "Pre-commit checks passed."
