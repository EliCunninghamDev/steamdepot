#!/usr/bin/env bash
# Run the ignored, network-dependent tests against real Steam infrastructure.
#
# Live tests never run in normal `cargo test`. Requires network access and,
# optionally, credentials for the authenticated flows. See `.env.example`.
set -euo pipefail

cd "$(dirname "$0")/.."

if [[ -f .env ]]; then
    set -a
    # shellcheck disable=SC1091
    source .env
    set +a
fi

echo "==> live tests (network)"
cargo test --workspace --all-targets --locked -- --ignored --nocapture "$@"
