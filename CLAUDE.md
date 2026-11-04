# reeve

reeve is a small, local tool for handing many tasks to coding agents and following each one until it meets the project's definition of done. The repository is at the idea stage: documents only, no design and no code yet.

Before changing anything, read [PHILOSOPHY.md](PHILOSOPHY.md). The principles there are the constraints for every change:

- use what the agent CLIs and existing tools already do, instead of rebuilding it;
- keep everything around the core optional and swappable;
- stay local by default;
- let done be defined per project;
- show that a new capability beats both plain Claude Code and the best existing setup, measured on a metric in [docs/metrics.md](docs/metrics.md); add the metric or scenario if none fits (see [CONTRIBUTING.md](CONTRIBUTING.md)).

When working here:

- Build one small piece at a time, and don't write up designs for pieces nobody is building yet.
- Keep the docs short and plain. Say each thing in one place and link to it from elsewhere.
- Use conventional commits (`docs:`, `feat:`, `fix:`, `chore:`).
- Propose changes as a PR that follows [CONTRIBUTING.md](CONTRIBUTING.md).
- Before merging, run the local gate, `reeve heavy -- scripts/ci-local.sh`; CI checks only fmt and clippy, plus check-public and gitleaks.
- This repo is public: raw transcripts, personal paths and client or private-project names never go in it ([CONTRIBUTING.md](CONTRIBUTING.md)).
