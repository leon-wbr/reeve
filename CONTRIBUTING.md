# Contributing

## Share your use case

There is no code yet, so the most useful contribution is how you would use reeve. Open a PR that adds a short file to [docs/use-cases/](docs/use-cases/). Describe the kind of work you hand to agents, where you track it, what done means in your projects, and any steps you'd want between your request and the result. Good ones get merged and shape what gets built.

## What a change needs

- **The native option, checked.** Say which agent CLI or existing tool you looked at, and why it doesn't already cover the change.
- **A number, for new capabilities.** Say which metric in [docs/metrics.md](docs/metrics.md) it should move, and show it moving across repeated benchmark runs. Fixes, docs and simplifications don't need one. Until the benchmark is public, open an issue first and the maintainer runs it.
- **Nothing new becomes required.** It plugs in like everything else around the core (see principle 2), and it keeps reeve local by default.

Design happens one piece at a time, when that piece is about to be built, so there is no big design document to follow.
