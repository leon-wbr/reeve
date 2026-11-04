# Contributing

Code, features, fixes, ideas and use cases are all welcome as PRs. If it's good, it gets merged.

## Use cases

Knowing how people would use reeve helps decide what to build next. Add a short file to [docs/use-cases/](docs/use-cases/) describing the kind of work you hand to agents, where you track it, what done means in your projects, and any steps you'd want between your request and the result.

## Code and features

Send the PR, even if it builds something the docs haven't designed yet; a working piece is a fine way to propose a design. It helps if it:

- **checks the native option.** Say which agent CLI or existing tool you looked at, and why it doesn't already cover this.
- **proves the number.** A new capability or change in behaviour has to show, in a "Benchmark" section of the PR, that it moves a metric in [docs/metrics.md](docs/metrics.md). If no metric or bench scenario captures what the PR is for, add one in the same PR. It has to beat both plain Claude Code (the `b0` arms: Claude Code set up well, doing the same work its native way) and the best existing setup on that metric (reeve's main or another setup, whichever is better), at n≥3 per arm. Raw transcripts stay private and outside the repo, even redacted. The section may publish only an allow-listed summary of each run: tool name, command string, exit status, token counts, timing and the final answer, with each tool result reduced to its length and a hash. A human reads that summary before it is committed, and it has to pass `scripts/check-public.sh` once that script exists. A PR that can't show this doesn't get merged. Fixes, docs and simplifications don't need a number.
- **stays optional.** It plugs in like everything else around the core (principle 2) and keeps reeve local by default.

CI runs only `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings`, on PRs that change code and aren't drafts, plus a public check on every PR: `scripts/check-public.sh` (no transcripts, personal paths or listings of `~/.claude`/`~/.codex`) and gitleaks. The gate before merging is `scripts/ci-local.sh`, which runs both of those too and adds `cargo test --workspace`; run it as `reeve heavy -- scripts/ci-local.sh` and say in the PR that it passed. To keep client and private-project names out, list them in a local `.private-terms` (see [.private-terms.example](.private-terms.example)); it's gitignored and check-public fails on any of them.
