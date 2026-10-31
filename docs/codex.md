# Codex

reeve works with the Codex CLI in two ways: Codex as a worker that a Claude Code (or Codex) session starts, and Codex as the main session that uses reeve itself. Claude stays the default everywhere. Written against codex-cli 0.164.0.

## Codex as a worker

```sh
reeve run --name NAME --cwd DIR --agent codex -- "PROMPT"
reeve run --name NAME --cwd DIR --agent codex --worktree -- -m gpt-6-astra "PROMPT"
```

Everything after `--` goes to codex, the prompt last, as with claude. The worker shows in `reeve ls`, `reeve wait`, `watch`, `report` and `rm` like a Claude one, and gets the same report instructions appended to its prompt.

- **Inside Herdr** it gets its own tab labelled NAME, started with `herdr agent start --kind codex`, which runs the interactive `codex`. Herdr's codex integration reports the pane's `agent` as `codex` and its `agent_status` (working, idle, blocked, done), which reeve maps to running, idle and blocked as for claude. That integration's session hook can claim the pane before `herdr agent start` returns, which then says the worker's name no longer owns it; reeve treats that like the timeout a busy launch prompt causes, finds codex in the pane and names it.
- **Outside Herdr** there is no `codex --bg`, so reeve runs `codex exec --json` detached in its own process group, its JSON events in `~/.local/state/reeve/logs/NAME.jsonl`, and records its pid. `exec` never stops to ask, so such a worker is never blocked; while the process lives it is running, and once it exits it is done (report written) or gone. A gone one can be continued with `codex exec resume THREAD "…"`, where THREAD is the `thread_id` of the log's `thread.started` event; `reeve wait` and `reeve watch` name it.
- **Sandbox.** Codex's sandbox would keep the worker from writing its report and from `reeve heavy`'s queue, so reeve adds `--add-dir` for its state dir. That needs a writable sandbox, which neither `codex exec` nor a folder Codex doesn't trust yet gives by default: codex then refuses `--add-dir` and exits. So reeve adds `-s workspace-write` unless the arguments already choose a sandbox. Network stays off in that sandbox; pass `-s danger-full-access` or set it in Codex's config for work that needs it.
- **Worktree.** Codex's own `--worktree` makes a managed worktree outside the repo. reeve's `--worktree` (for either agent) instead runs `git worktree add -b worktree-NAME REPO/.claude/worktrees/NAME` from HEAD, where REPO is the main checkout, and starts the agent there. Claude's `-w NAME` still works after `--`.
- **Herdr pane length.** Herdr types the command into the pane's shell, so the same 1000-byte limit applies; a longer prompt goes through `reports/NAME.prompt.md` as for claude.
- In a folder Codex hasn't trusted yet, the interactive codex asks first, and Herdr reports it as blocked during startup. reeve doesn't answer that for the user: the tab stays open and the worker shows as blocked until it is answered there. Any agent that stops at a question on start is treated the same way.

`--agent codex` doesn't combine with `--sandbox` (the container image has claude only) or with `--bg` outside a named worker.

## Codex as the main session

`reeve setup codex` installs, and `--check` and `--remove` mirror `reeve setup claude`:

- **Hooks** in `~/.codex/hooks.json` (under `CODEX_HOME` when set), the same format as Claude Code's: `reeve ls --brief` at session start, and `reeve hook context` on prompts and tool calls. Codex runs a new or changed hook only after the user trusts it in `/hooks`, and reeve doesn't write that trust for them; setup says so.
- **Skill stub** at `~/.agents/skills/reeve/SKILL.md`, where Codex looks for user skills. Same text as Claude's, pointing at `reeve --skill`.
- **`--agents-md`** adds the marked reeve block to `~/.codex/AGENTS.md`. When that file links to `~/.claude/CLAUDE.md` and the block is already there, nothing changes.
- **Rules** in `~/.codex/rules/reeve.rules` let reeve's own bookkeeping commands (`ls`, `wait`, `watch`, `report`, `rm`, `run`, `handoff`, `ask`, `asks`) run outside the sandbox, which can't write `~/.local/state/reeve`. `reeve heavy` isn't among them, since it runs any command, nor `reeve answer`, whose answers come from the user; Codex asks for those as for any other command that needs more than the sandbox.

`reeve doctor` reports the Codex setup next to Claude's when codex is installed.

### Context check

Codex hooks get no token count, but their `transcript_path` is the session's rollout file, whose `token_count` events carry `last_token_usage.input_tokens` (cached input included) and `model_context_window`. `reeve hook context` reads either transcript format. Codex's window (258K for gpt-6-astra) is below Claude's 350K default threshold, so for a transcript that names its window the threshold is at most 35% of it, the same share as Claude's 350K of a 1M window.

### Heavy commands

No PreToolUse hook: Codex's shell tool (`exec_command`) already returns after at most 30 seconds and leaves the command running in a background terminal, which the session polls. A `reeve heavy` wait never blocks a Codex session, so there is nothing for a hook to fix; Codex's hooks also can't set that tool's options, only rewrite `command`.

### Handoff

`reeve handoff` starts the successor with the same agent as the session that calls it: Codex when `CODEX_THREAD_ID` is set (Codex sets it in every shell command) and `CLAUDECODE` isn't. `--agent` overrides it. reeve clears both variables when it starts an agent, so a worker doesn't inherit its starter's.
