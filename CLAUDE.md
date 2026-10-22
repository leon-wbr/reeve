# reeve

reeve is a small, local tool for handing many tasks to coding agents and following each one until it meets the project's definition of done. The repository is at the idea stage: documents only, no design and no code yet.

Before changing anything, read [PHILOSOPHY.md](PHILOSOPHY.md). The principles there are the constraints for every change:

- use what the agent CLIs and existing tools already do, instead of rebuilding it;
- keep everything around the core optional and swappable;
- stay local by default;
- let done be defined per project;
- show that a new capability beats the best setup it's measured against ([docs/metrics.md](docs/metrics.md)).

When working here:

- Don't add technical design or code ahead of its step; each piece is designed when it's about to be built.
- Keep the docs short and plain. Say each thing in one place and link to it from elsewhere.
- Use conventional commits (`docs:`, `feat:`, `fix:`, `chore:`).
- Propose changes as a PR that follows [CONTRIBUTING.md](CONTRIBUTING.md).
