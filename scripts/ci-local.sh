#!/bin/sh
# The gate before merging: everything CI used to run, on this machine. GitHub Actions keeps only
# the cheap fmt and clippy check on PRs. Run it through `reeve heavy -- scripts/ci-local.sh`.
set -eu
cd "$(dirname "$0")/.."
scripts/check-public.sh
if command -v gitleaks >/dev/null; then
    # Every commit reachable from HEAD; .gitleaks.toml allowlists the redaction test's fakes.
    gitleaks git --redact --no-banner --log-level warn .
else
    echo "ci-local: gitleaks not installed, skipped (CI runs it)" >&2
fi
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
python3 -m unittest discover -s bench -p 'test_*.py'
echo "ci-local: ok"
