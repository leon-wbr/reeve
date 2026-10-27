# reeve, for agents

reeve is a small local CLI that runs coding agents beside your session. Each part is optional: use what helps with the task in front of you and skip the rest.

## Named workers: work that outlives this session

`reeve run --name NAME --bg -- "PROMPT"` starts a Claude Code background session under a name. It keeps going when your session ends, compacts or hands off, and writes its report to `~/.local/state/reeve/reports/NAME.md`, where any later session finds it.

```sh
reeve run --name fix-flaky --bg -- "Find and fix the flaky test in tests/api; run it 20 times to confirm."
reeve ls                 # every worker: running, idle, done or gone
reeve wait fix-flaky     # blocks until the report exists; fails if the worker goes idle or away
reeve report fix-flaky   # prints the report
```

Inside Herdr a worker gets its own pane in a new tab labelled NAME instead (`--pane`, the default there). Your focus stays where it is; there is nothing to move.

Workers tend to help with work that may take longer than this session lasts, with several independent tasks that can run side by side without filling your context, and with anything a successor should be able to pick up. For short work whose answer you need in this session, a subagent is simpler and a fine choice.

## Handoffs

Workers keep running across a handoff. Naming them in the handoff note lets the next session check `reeve ls` and `reeve report NAME` before it redoes anything. With `reeve setup claude` installed, each new session also starts with a one-line summary of the workers, and nothing when there are none.

## Heavy commands on a shared machine

When several agents share one machine, their builds and test suites slow each other down. `reeve heavy` waits for a free slot (a quarter of the cores by default), runs the command and exits with its code:

```sh
reeve heavy -- cargo test
reeve heavy --label e2e -- pnpm test:e2e
reeve heavy ls           # who runs and who waits
```

It is worth it for builds, full test suites and installs. Quick commands such as `git status` or a single test don't need it.

## Sandbox

`reeve run --sandbox [NAME] -- ARGS` runs Claude Code with `--dangerously-skip-permissions` in a container that sees only the workspace, which suits long unattended work that would otherwise stop at permission prompts. It needs Docker. `reeve sandbox ls`, `exec` and `down` manage the sandboxes.

## When reeve itself gets in the way

```sh
reeve feedback "what you ran, what happened, what you expected"
```

This saves a note with reeve's version and recent events, then offers to file it as an issue. Nothing is posted without a yes.

## When not to bother

- A task you'll finish in this session: do it directly or with a subagent.
- A quick command: just run it.
- `reeve` isn't on PATH: carry on without it.

`reeve doctor` checks the install, and `reeve --help` lists every command.
