# reeve

A small local CLI for running many coding-agent tasks at once from the terminal, and following each one through to a verified finish.

> Status: design only. This repository holds the plan; there is no code yet.

## What it is for

You work across many repositories and hand out many small jobs at once: rebase these PRs, squash-merge that one when CI is green, fix the failing check over there, research this and write it up. reeve turns each request into an item and starts one background agent session per item. It keeps the item open until something outside the agent confirms it is finished: the PR merged, the page exists, the branch is pushed. You hear about results and about decisions that are yours to make, and nothing else.

reeve doesn't plan the work, reword your requests, or show you its own infrastructure trouble. It runs on your machine when you call it or when a trigger fires, then exits.

## Why not just Claude Code?

Plain Claude Code in a terminal is the baseline reeve has to beat. It already has a lot:

- background sessions (`claude --bg`) that survive closing the terminal, and restart if they crash;
- one view of all sessions (`claude agents`) grouped by what needs you;
- worktree isolation (`-w`), subagents and messages between sessions;
- `/loop` and scheduled prompts.

reeve uses all of that as it is and builds only what is still missing:

1. **Verified done.** Each request carries its own finish line, checked against GitHub or the file system, not against what the agent says.
2. **Following through.** A PR is tracked after it is opened: rebased when its base moves, noticed when someone merges it by hand, closed when it lands.
3. **Decisions across days.** What waits on you is kept per request, with options and a recommendation, not scattered across sessions.
4. **Rules.** A sentence like "merge it when CI is green" becomes a standing automation.
5. **Measurement.** Every feature is benchmarked against plain Claude Code on the same tasks (see [docs/metrics.md](docs/metrics.md)).

Other tools in this space each cover a part of it:

| Tool | What it covers | What it doesn't |
| --- | --- | --- |
| Claude Code agent view and Projects | Session status, PR badges; Projects resolves threads on merge | Projects is cloud and browser first |
| Codex app and cloud | An inbox of results | Its own subscription and limits |
| Conductor | A checks → PR → merge lane per workspace | Mac app, no shared list of decisions |
| Gas Town | A durable work ledger | Many agents in tmux panes, alpha |
| Claude Squad, Superset, cmux | Pane management | No tracking to done |

None of them checks done from outside, or keeps following a PR after it is opened.

## How it works (planned)

reeve is a few commands, some Claude Code hooks, and one small SQLite file.

- `reeve add "<message>"`: splits the message into requests and keeps each one's words. It starts one background session per request (`claude --bg -w`) with a brief that names its done-check.
- `reeve`: prints what waits on you (decisions with a recommendation, and sessions that need input), after a quick sync with GitHub.
- `reeve merge | rebase | close | drop <item>`: fixed actions that run `gh` directly, with no agent session.
- `reeve open <item>`: opens the live session in a Herdr (terminal workspace) tab.
- `reeve rule "<sentence>"`: turns a sentence into a standing rule.
- `reeve metrics`: prints the numbers in [docs/metrics.md](docs/metrics.md).

Claude Code hooks feed the event log, and Herdr shows notifications. GitHub stays the source of truth for PRs. Heavy commands (builds, tests) go through a separate queueing CLI, so parallel sessions don't overload the machine.

## Documents

- [PHILOSOPHY.md](PHILOSOPHY.md): what reeve believes, and its principles.
- [docs/metrics.md](docs/metrics.md): how it is measured against plain Claude Code.
- [CONTRIBUTING.md](CONTRIBUTING.md): how it is built, step by step.
