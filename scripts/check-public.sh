#!/bin/sh
# Fails when a tracked file holds something that shouldn't be in this public repo: an agent
# transcript, a listing of ~/.claude or ~/.codex, a real home or macOS temp path, or a term from
# the local, gitignored .private-terms file (see .private-terms.example). Run by
# scripts/ci-local.sh and CI. Needs git built with PCRE (`git grep -P`).
# Usage: check-public.sh [REV]   (default: the tracked files in the working tree)
set -eu
cd "$(dirname "$0")/.."
rev=${1:-}
fail=0
self=':!scripts/check-public.sh'

hit() { # what, then git grep args; prints matches and marks the run failed
    what=$1
    shift
    # shellcheck disable=SC2086 # $rev is empty or one revision
    if out=$(git grep -n -I "$@" $rev -- . "$self" ':!.private-terms.example'); then
        printf 'check-public: %s\n%s\n' "$what" "$out" >&2
        fail=1
    fi
}

# Transcripts and raw bench output never go in git, redacted or not.
if [ -n "$rev" ]; then files=$(git ls-tree -r --name-only "$rev"); else files=$(git ls-files); fi
if out=$(printf '%s\n' "$files" | grep -E '^bench/results/(transcripts|raw)/|\.jsonl$'); then
    printf 'check-public: tracked transcripts or raw bench output\n%s\n' "$out" >&2
    fail=1
fi

# Home dirs. The placeholders the tests and examples use (u, x, me, you, jdoe) and the container
# and CI users (node, runner) are allowed.
hit 'a real home path' -P '/Users/(?!(?:u|x|me|you|jdoe)\b)[a-z]|-Users-(?!jdoe\b)[A-Za-z]|/home/(?!(?:u|x|me|you|jdoe|node|runner)\b)[a-z]+/'
# macOS per-user temp dirs are stable per account.
hit 'a macOS per-user temp path' -P '/private/var/folders/|/var/folders/[a-z0-9_]{2}/'
# A listing of an agent's config dir: `ls` run on it, or `ls -l` output.
hit 'a listing of ~/.claude or ~/.codex' -P '\bls(\s+-\w+)*\s+("?(~|\$HOME|\$\{HOME\})"?)/\.(claude|codex)\b'
hit 'ls -l output' -P '(^|[\s"]|\\n)[-dl][r-][w-][xsS-][r-][w-][xsS-][r-][w-][xtT-][@+.]?\s+\d+\s+\S+\s+\S+\s+\d+\s'

# Local private terms: client and private-project names, never committed themselves.
if [ -f .private-terms ]; then
    terms=$(mktemp)
    grep -v -E '^[[:space:]]*(#|$)' .private-terms >"$terms" || true
    if [ -s "$terms" ]; then
        hit 'a term from .private-terms' -i -F -f "$terms"
    fi
    rm -f "$terms"
fi

[ "$fail" = 0 ] || exit 1
echo "check-public: ok"
