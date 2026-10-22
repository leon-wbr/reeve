# Contributing

## Right now

There is no code yet. The most useful help is an issue that describes how you'd use reeve: your tracker, what done means in your projects, a pipeline you'd want, or a task the benchmark should include.

## What a change needs

- **The native option, checked.** Say which agent CLI or existing tool you looked at, and why it doesn't already cover the change.
- **A number, for new capabilities.** Say which metric in [docs/metrics.md](docs/metrics.md) it should move, and show it moving across repeated benchmark runs. Fixes, docs and simplifications don't need one. Until the benchmark is public, open an issue first and the maintainer runs it.
- **Nothing new becomes required.** It plugs in like everything else around the core (see principle 2), and it keeps reeve local by default.

## Order

1. These documents.
2. Measurement, then the benchmark: record who acted and when, then run plain Claude Code and the other setups to set the bar.
3. The smallest capability that beats the bar, then the next.

Each piece is designed when its step comes up, not before.
