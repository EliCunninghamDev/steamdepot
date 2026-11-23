#!/usr/bin/env bash
# Run the same checks as CI, locally, in the same order.
set -euo pipefail

cd "$(dirname "$0")/.."

echo "==> cargo fmt --check"
cargo fmt --all --check

echo "==> cargo clippy"
cargo clippy --workspace --all-targets --locked -- -D warnings

echo "==> cargo test"
cargo test --workspace --all-targets --locked

echo "==> cargo doc"
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked

echo "All checks passed."
