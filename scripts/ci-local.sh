#!/bin/sh
# The gate before merging: everything CI used to run, on this machine. GitHub Actions keeps only
# the cheap fmt and clippy check on PRs. Run it through `reeve heavy -- scripts/ci-local.sh`.
set -eu
cd "$(dirname "$0")/.."
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
echo "ci-local: ok"
