# reeve

A small, local tool for handing many tasks to coding agents at once and following each one until it meets your definition of done.

> Status: idea and principles. No design and no code yet.

## What it is for

A developer working across many repositories hands out many small jobs at once: rebase these PRs, merge that one when CI is green, fix the failing check over there, pick up this ticket, research that and write it up. reeve is meant to turn each request into an item, give it to an agent, and keep it open until it is done by the rules set for that project. The aim is that every question you get is a real decision, never one about the tool's own trouble.

It started as a rewrite of the author's own orchestrator built on Claude Code, which failed about one agent start in four and asked 100–150 questions a day. The numbers are in [docs/metrics.md](docs/metrics.md).

## Optional by design

Only the core is fixed: items, agents, and a check for done. Everything around it can be swapped or left out:

- **Where items live:** local by default. Jira or GitHub Issues are meant to plug in the same way.
- **How you see them:** a terminal command, a TUI, or something else entirely.
- **What done means:** set per project. It could be a merged PR, a pushed branch, a written page or your own sign-off.
- **The pipeline:** by default a request goes to an agent and the result comes back to you. You can add steps, for example something that sharpens the prompt first or a reviewer that checks the result.

Nothing is written to a shared tracker, chat or PR unless you set it up to be. Coworkers see what you choose to share.

## Why not just Claude Code?

Plain Claude Code is strong and is the main thing reeve has to beat. It runs sessions in the background and keeps them alive. Its agent view groups them into "needs input" and "ready for review", with PR status, and its worktrees keep them apart. Hooks can stop a session from finishing before a check passes, and `/loop` and scheduled tasks can watch a PR.

What is still missing is everything that has to outlive a single session:

- an item that stays open across sessions, restarts and days, and reopens if its work regresses;
- one queue of decisions across repositories that persists, each with options and a recommendation;
- per-project rules for done and for automation, with a record of what each rule did;
- pipelines put together from the native pieces (subagents, skills, review commands), set per project.

reeve builds only those, and is meant to be measured against plain Claude Code and other setups before any feature ships.

## Other tools (as of October 2026)

| Tool | Strong at | Gap for this use |
| --- | --- | --- |
| Claude Code agent view | Many local sessions, grouped by what needs you, with PR status | No item that lives beyond its session |
| Claude Projects | A coordinator with threads that close when their PR merges | Cloud-first; needs the GitHub App |
| Codex app and CLI | Local and cloud tasks, an inbox of results | No tracking across repositories with your own rules |
| Conductor | A checks → PR → merge lane per workspace; runs Claude Code and Codex | No decision list or rules across workspaces |
| Gas Town | A durable record of work across many agents | Heavyweight; many agents in tmux |
| Claude Squad, Superset, cmux | Managing many agent terminals | No tracking to done |

## Commands

- `reeve run`: start an agent, in the foreground or the background, on the host or in a sandbox; `--name` makes it a worker any session can find.
- `reeve ls`, `reeve wait`, `reeve report`: list named workers, wait for one to finish, print its report.
- `reeve sandbox`: build, list, use and remove sandboxes.
- `reeve heavy`: queue builds and test suites so agents take turns ([docs/heavy.md](docs/heavy.md)).
- `reeve stats`: sum up reeve's local event log ([docs/events.md](docs/events.md)).
- `reeve feedback "text"`: save a note about reeve itself and offer to post it as an issue ([docs/events.md](docs/events.md)).

## Words used here

- **Item:** one request, tracked until done.
- **Done:** whatever the project's rule says finishes an item.
- **Pipeline:** the steps an item passes through, from request to result.
- **Rule:** a standing automation, such as "merge once CI is green".

## Documents

- [PHILOSOPHY.md](PHILOSOPHY.md)
- [CONTRIBUTING.md](CONTRIBUTING.md)
- [docs/use-cases/](docs/use-cases/)
- [docs/metrics.md](docs/metrics.md)
- [docs/events.md](docs/events.md)
