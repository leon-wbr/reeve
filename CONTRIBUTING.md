# Contributing

Code, features, fixes, ideas and use cases are all welcome as PRs. If it's good, it gets merged.

## Use cases

Knowing how people would use reeve helps decide what to build next. Add a short file to [docs/use-cases/](docs/use-cases/) describing the kind of work you hand to agents, where you track it, what done means in your projects, and any steps you'd want between your request and the result.

## Code and features

Send the PR, even if it builds something the docs haven't designed yet; a working piece is a fine way to propose a design. It helps if it:

- **checks the native option.** Say which agent CLI or existing tool you looked at, and why it doesn't already cover this.
- **names the number.** For a new capability, say which metric in [docs/metrics.md](docs/metrics.md) it should move. Until the benchmark is public, the maintainer runs it on your PR. Fixes, docs and simplifications don't need a number.
- **stays optional.** It plugs in like everything else around the core (principle 2) and keeps reeve local by default.

CI runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` on every PR; run them locally first.
