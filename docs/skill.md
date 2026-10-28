# reeve, for agents

reeve is a small local CLI that runs coding agents beside your session. Two parts aren't optional: heavy commands go through `reeve heavy` in the background, and at a handoff threshold you run `reeve handoff`. Use the rest where it helps.

## Named workers: work that outlives this session

`reeve run --name NAME --bg -- "PROMPT"` starts a Claude Code background session under a name. It keeps going when your session ends, compacts or hands off, and writes its report to `~/.local/state/reeve/reports/NAME.md`, where any later session finds it.

```sh
reeve run --name fix-flaky --bg -- "Find and fix the flaky test in tests/api; run it 20 times to confirm."
reeve ls                 # every worker: running, idle, done or gone
reeve wait fix-flaky     # blocks until the report exists; fails if the worker goes idle or away
reeve report fix-flaky   # prints the report
```

Inside Herdr a worker gets its own pane in a new tab labelled NAME instead (`--pane`, the default there). Your focus stays where it is; there is nothing to move.

Workers help with work that may outlast this session, independent tasks that can run side by side without filling your context, and anything a successor should pick up. For short work whose answer you need now, a subagent is simpler.

## Handoffs

When your session reaches its handoff threshold, hand off yourself; don't wait to be asked:

```sh
reeve handoff --name NAME-2 --notes "goal, what's done, what's next, open questions"
```

It writes the handoff file with your workers and starts the successor as a named worker told to read it. Stop once it returns. Workers keep running; the successor checks `reeve ls` and `reeve report NAME` before redoing anything.

## Heavy commands on a shared machine

Agents share this machine. Run heavy commands through `reeve heavy`: test suites, e2e (even one spec), local CI, workspace typecheck or lint, builds and installs. It waits for a slot and exits with the command's code. Waits can be long, so never run it in the foreground: start it in the background (Claude Code: `run_in_background`; Codex: a background terminal) and keep working; you're told when it ends.

```sh
reeve heavy -- cargo test
reeve heavy --label e2e -- pnpm test:e2e
reeve heavy ls           # who runs and who waits
```

You decide what's heavy; when in doubt, queue it. Only quick commands such as `git status` or one unit test skip it.

## Sandbox

`reeve run --sandbox [NAME] -- ARGS` runs Claude Code with `--dangerously-skip-permissions` in a container that sees only the workspace, which suits long unattended work that would otherwise stop at permission prompts. It needs Docker. `reeve sandbox ls`, `exec` and `down` manage the sandboxes.

## When reeve itself gets in the way

```sh
reeve feedback "what you ran, what happened, what you expected"
```

This saves a note with reeve's version and recent events and offers to file it as an issue, never without a yes.

## When not to bother

- A task you'll finish in this session: do it directly or with a subagent.
- `reeve` isn't on PATH: carry on without it.

`reeve doctor` checks the install, and `reeve --help` lists every command.
