# reeve

A small local CLI for running several coding agents (Claude Code or Codex) side by side on one machine. Three things work today:

- **A queue for heavy commands.** `reeve heavy -- cargo test` waits for a free slot, so parallel agents take turns on test suites, builds and installs instead of running them all at once ([docs/heavy.md](docs/heavy.md)).
- **Named workers with reports.** `reeve run --name NAME -- "PROMPT"` starts an agent that outlives your session and writes its report to a known file. `reeve ls`, `reeve watch` and `reeve report` follow it from any session, later ones included.
- **Handoff before the context runs out.** A hook tells a session when it passes its context threshold, and `reeve handoff` starts a named successor with its notes and the list of running workers.

Around those: Docker sandboxes for unattended work, a message box for other machines, and a setup that teaches Claude Code and Codex when to use reeve.

> Status: early. Tracking each request as an item until it meets the project's definition of done is where reeve is going ([below](#where-its-going)); none of that exists yet.

```console
$ git clone https://github.com/leon-wbr/reeve && cd reeve && cargo install --path .
…
   Installed package `reeve v0.1.0 (/home/you/reeve)` (executable `reeve`)

$ reeve run --name summary -- "Summarise ./README.md in two sentences. The report is just those two sentences."
reeve: worker summary is starting; its report goes to ~/.local/state/reeve/reports/summary.md
reeve: follow it with `reeve watch summary` (Claude Code: the Monitor tool, timeout_ms 1800000, re-armed when it expires; elsewhere a background shell). A line per change says what to do: done, read the report, then `reeve close summary`; idle, read `claude logs summary`: nothing if it waits on its own job, else nudge it, let it carry on, or ask the user; blocked, answer it with `claude attach d38de0ff` if you can, else ask the user.

$ reeve ls
NAME     STATE     AGENT   BACKEND  AGE  CWD
summary  done      claude  bg        7s  ~/reeve

$ reeve report summary
reeve is a small local CLI for running several coding agents side by side on one machine. It queues heavy commands so agents take turns, runs named workers that leave reports, and hands a session off to a successor before its context runs out.

$ reeve setup claude
backed up ~/.claude/settings.json to ~/.claude/settings.json.reeve-1793022575.bak
wrote ~/.claude/settings.json: hooks `reeve ls --brief` (session start), `reeve hook context` (prompts and tool calls) and `reeve hook background` (Bash calls)
wrote ~/.claude/settings.json: allows reeve's own subcommands, which auto mode's classifier then skips
wrote ~/.claude/skills/reeve/SKILL.md
`reeve setup claude --remove` takes it all out again
```

Claude Code starts a background session only in a folder it trusts. In a fresh clone, run `claude` there once and accept the trust prompt first; otherwise `reeve run` fails with Claude Code's reason. `reeve doctor` checks the install.

`reeve setup claude` touches only those two files. One hook prints a line about your workers at the start of each Claude Code session, and nothing when there are none. Another tells a session once it has used more context than its handoff threshold: 350,000 tokens, or `REEVE_HANDOFF_TOKENS`, or else `handoff_tokens` in `~/.config/reeve/config.json`. A third sends any Bash call that starts with `reeve heavy` to the background, so a wait for a slot never blocks the session. The allow rules (`Bash(reeve ls:*)`, `Bash(reeve run:*)` and the like) let sessions run reeve's own bookkeeping without a prompt. They leave out `reeve heavy`, which runs any command, and `reeve answer`, whose answers come from you. On the host, `reeve run` and `reeve handoff` refuse agent flags that skip permission checks, such as `--dangerously-skip-permissions`; `reeve run --sandbox` is the place for those. The skill stub says when to use reeve (heavy commands, long or parallel work as named workers, handoffs when a context check says so) and points agents at `reeve --skill`, the manual in [docs/skill.md](docs/skill.md); it carries no command syntax, so it can't go stale while the binary always prints the current manual. `--claude-md` also adds a short marked block with the same text to `~/.claude/CLAUDE.md` (under `CLAUDE_CONFIG_DIR` when set), so every session knows reeve is there; setup never touches text outside its markers.

For Codex, `reeve setup codex` does the same with Codex's hooks, skills and rules, and `reeve run --agent codex` starts a Codex worker; [docs/codex.md](docs/codex.md) says what differs.

## Commands

- `reeve run`: start an agent, in the foreground or the background, on the host or in a sandbox; `--name` makes it a worker any session can find, and `--cwd DIR` starts it in its project, `--worktree` in its own git worktree and `--agent codex` as Codex. Inside Herdr a worker gets its own tab, where you can see and answer it, in the workspace named after its project (the main checkout of `--cwd`, or `--cwd` outside git), which reeve creates unfocused when it is missing and Herdr closes with its last tab. There `--bg` is refused unless you pass `--force`, which skips reeve's safety checks; elsewhere a worker is a background session.
- `reeve ls`, `reeve wait`, `reeve watch`, `reeve report`, `reeve close`, `reeve rm`: list named workers, wait for one to finish, print a line each time one is blocked, idle, gone or done (with the next step), print its report, close a finished one's tab (or stop its background process) and its clean, pushed worktree, drop a gone one from the list.
- `reeve handoff`: hand off to a successor started as a named worker. Inside Herdr `--bg` is refused here too, unless you pass `--force`.
- `reeve ask`, `reeve asks`, `reeve answer`: queue a question for the user, list open ones, answer one.
- `reeve remote`, `reeve send`, `reeve inbox`: a plain message box on any machine you can ssh to, with a queue for when the connection drops ([docs/remotes.md](docs/remotes.md)).
- `reeve sandbox`: build, list, use and remove sandboxes.
- `reeve heavy`: queue builds and test suites so agents take turns ([docs/heavy.md](docs/heavy.md)).
- `reeve stats`: sum up reeve's local event log ([docs/events.md](docs/events.md)).
- `reeve feedback "text"`: save a note about reeve itself and offer to post it as an issue ([docs/events.md](docs/events.md)).
- `reeve setup claude [--claude-md] [--check] [--remove]`: add, check or remove the hooks, the allow rules, the skill stub and, with `--claude-md`, the CLAUDE.md block.
- `reeve setup codex [--agents-md] [--check] [--remove]`: the same for Codex ([docs/codex.md](docs/codex.md)).
- `reeve --skill`: print the manual for agents.
- `reeve update [--dry-run]`: upgrade Claude Code and Codex with whatever installed each (a Homebrew cask or formula, global npm, or `claude update` for Claude's own installer), through `reeve heavy`, and print the versions before and after. It tells the install from where the executable on PATH really lives, leaves one it doesn't know alone, and skips an agent while a worker of it is still starting. To run it on a schedule, use cron or launchd with a PATH that finds reeve, the CLIs and their installer, e.g. the crontab line `0 9 * * * PATH=$HOME/.local/bin:/opt/homebrew/bin:/usr/bin:/bin reeve update`.
- `reeve doctor`: check PATH, the setup's version, the state dir, the agent CLIs and gone workers.
- `reeve bench entry`: run fresh sessions on four cases and report whether each chose well, which can mean not using reeve.

## Where it's going

None of this section exists yet.

### What it is for

A developer working across many repositories hands out many small jobs at once: rebase these PRs, merge that one when CI is green, fix the failing check over there, pick up this ticket, research that and write it up. reeve is meant to turn each request into an item, give it to an agent, and keep it open until it is done by the rules set for that project. The aim is that every question you get is a real decision, never one about the tool's own trouble.

It started as a rewrite of the author's own orchestrator built on Claude Code, which failed about one agent start in four and asked 100–150 questions a day. The numbers are in [docs/metrics.md](docs/metrics.md).

### Optional by design

Only the core is fixed: items, agents, and a check for done. Everything around it can be swapped or left out:

- **Where items live:** local by default. Jira or GitHub Issues are meant to plug in the same way.
- **How you see them:** a terminal command, a TUI, or something else entirely.
- **What done means:** set per project. It could be a merged PR, a pushed branch, a written page or your own sign-off.
- **The pipeline:** by default a request goes to an agent and the result comes back to you. You can add steps, for example something that sharpens the prompt first or a reviewer that checks the result.

Nothing is written to a shared tracker, chat or PR unless you set it up to be. Coworkers see what you choose to share.

### Why not just Claude Code?

Plain Claude Code is strong and is the main thing reeve has to beat. It runs sessions in the background and keeps them alive. Its agent view groups them into "needs input" and "ready for review", with PR status, and its worktrees keep them apart. Hooks can stop a session from finishing before a check passes, and `/loop` and scheduled tasks can watch a PR.

Today reeve adds the heavy queue that parallel workers share, reports in a known place, and handoff to a named successor, for Claude Code and Codex alike. What is still missing is everything that has to outlive a single session:

- an item that stays open across sessions, restarts and days, and reopens if its work regresses;
- one queue of decisions across repositories that persists, each with options and a recommendation;
- per-project rules for done and for automation, with a record of what each rule did;
- pipelines put together from the native pieces (subagents, skills, review commands), set per project.

reeve builds only those, and is meant to be measured against plain Claude Code and other setups before any feature ships.

### Words used here

- **Item:** one request, tracked until done.
- **Done:** whatever the project's rule says finishes an item.
- **Pipeline:** the steps an item passes through, from request to result.
- **Rule:** a standing automation, such as "merge once CI is green".

## Other tools (as of October 2026)

| Tool | Strong at | Gap for this use |
| --- | --- | --- |
| Claude Code agent view | Many local sessions, grouped by what needs you, with PR status | No item that lives beyond its session |
| Claude Projects | A coordinator with threads that close when their PR merges | Cloud-first; needs the GitHub App |
| Codex app and CLI | Local and cloud tasks, an inbox of results | No tracking across repositories with your own rules |
| Conductor | A checks → PR → merge lane per workspace; runs Claude Code and Codex | No decision list or rules across workspaces |
| Gas Town | A durable record of work across many agents | Heavyweight; many agents in tmux |
| Claude Squad, Superset, cmux | Managing many agent terminals | No tracking to done |

## Documents

- [PHILOSOPHY.md](PHILOSOPHY.md)
- [CONTRIBUTING.md](CONTRIBUTING.md)
- [docs/use-cases/](docs/use-cases/)
- [docs/metrics.md](docs/metrics.md)
- [docs/events.md](docs/events.md)
- [docs/codex.md](docs/codex.md)
- [docs/remotes.md](docs/remotes.md)

## License

MIT; see [LICENSE](LICENSE).
