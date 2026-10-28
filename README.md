# reeve

A small, local tool for handing many tasks to coding agents at once and following each one until it meets your definition of done.

> Status: early. Named workers, a shared queue for heavy commands, sandboxes and the Claude Code setup work today; items and rules for done don't exist yet.

```console
$ git clone https://github.com/leon-wbr/reeve && cd reeve && cargo install --path .
…
   Installed package `reeve v0.1.0 (/private/tmp/reeve)` (executable `reeve`)

$ reeve run --name summary -- "Summarise ./README.md in two sentences. The report is just those two sentences."
backgrounded · d38de0ff · summary
  claude agents             list sessions
  claude attach d38de0ff    open in this terminal
  claude logs d38de0ff      show recent output
  claude stop d38de0ff      stop this session
reeve: worker summary started; `reeve wait summary` blocks until its report is written

$ reeve ls
NAME     STATE    BACKEND  AGE  CWD
summary  done     bg        7s  ~/work/reeve/.claude/worktrees/entry

$ reeve report summary
reeve is a small, local tool that gives many tasks to coding agents and tracks each one as an item until it meets that project's own definition of done. Only a core of items, agents and a done-check is fixed and everything else can be swapped out, because it aims to add what plain Claude Code lacks: items that last beyond a single session, one persistent queue of decisions across repositories, and per-project rules and pipelines.

$ reeve setup claude
backed up ~/.claude/settings.json to ~/.claude/settings.json.reeve-1793022575.bak
wrote ~/.claude/settings.json: hooks `reeve ls --brief` (session start) and `reeve hook context` (prompts and tool calls)
wrote ~/.claude/skills/reeve/SKILL.md
`reeve setup claude --remove` takes it all out again
```

`reeve setup claude` touches only those two files. One hook prints a line about your workers at the start of each Claude Code session, and nothing when there are none. The other tells a session once it has used more context than its handoff threshold: 350,000 tokens, or `REEVE_HANDOFF_TOKENS`, or else `handoff_tokens` in `~/.config/reeve/config.json`. The skill stub says when to use reeve (heavy commands, long or parallel work as named workers, handoffs when a context check says so) and points agents at `reeve --skill`, the manual in [docs/skill.md](docs/skill.md); it carries no command syntax, so it can't go stale while the binary always prints the current manual. `--claude-md` also adds a short marked block with the same text to `~/.claude/CLAUDE.md` (under `CLAUDE_CONFIG_DIR` when set), so every session knows reeve is there; setup never touches text outside its markers. `reeve doctor` checks the install.

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

- `reeve run`: start an agent, in the foreground or the background, on the host or in a sandbox; `--name` makes it a worker any session can find, and `--cwd DIR` starts it in its project. Inside Herdr a worker gets its own tab, where you can see and answer it, and `--bg` is refused unless you pass `--force`, which skips reeve's safety checks; elsewhere it is a background session.
- `reeve ls`, `reeve wait`, `reeve report`, `reeve rm`: list named workers, wait for one to finish, print its report, drop a gone one from the list.
- `reeve handoff`: hand off to a successor started as a named worker.
- `reeve ask`, `reeve asks`, `reeve answer`: queue a question for the user, list open ones, answer one.
- `reeve sandbox`: build, list, use and remove sandboxes.
- `reeve heavy`: queue builds and test suites so agents take turns ([docs/heavy.md](docs/heavy.md)).
- `reeve stats`: sum up reeve's local event log ([docs/events.md](docs/events.md)).
- `reeve feedback "text"`: save a note about reeve itself and offer to post it as an issue ([docs/events.md](docs/events.md)).
- `reeve setup claude [--claude-md] [--check] [--remove]`: add, check or remove the hooks, the skill stub and, with `--claude-md`, the CLAUDE.md block.
- `reeve --skill`: print the manual for agents.
- `reeve doctor`: check PATH, the setup's version, the state dir, the agent CLIs and gone workers.
- `reeve bench entry`: run fresh sessions on four cases and report whether each chose well, which can mean not using reeve.
- `reeve bench block`: run isolated sessions with only a CLAUDE.md block, as an orchestrator and as a worker, and score whether each used reeve where it fits ([bench/FINDINGS.md](bench/FINDINGS.md)).

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
