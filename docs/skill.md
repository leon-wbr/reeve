# reeve, for agents

reeve is a small local CLI that runs coding agents beside your session. Two rules aren't optional:

- Run test suites, e2e, builds and installs through `reeve heavy -- <command>`, started in the background.
- When a "Context check" tells you that you passed the handoff threshold, run `reeve handoff --name NAME --notes FILE` and stop.

Use the rest where it helps.

## Named workers: work that outlives this session

`reeve run --name NAME --cwd DIR -- "PROMPT"` starts a named worker. It outlives your session and writes its report to `~/.local/state/reeve/reports/NAME.md`, where any later session finds it. Pass the project root as `--cwd` so it loads its `.claude/` setup.

Inside Herdr each worker gets its own tab, labelled NAME, where the user can see and answer it. Start workers there with `reeve run`, not Herdr's agent commands. Outside Herdr it is a Claude Code background session; `--bg` is for that case only.

`reeve run --force` skips reeve's safety checks, such as that one.

```sh
reeve run --name fix-flaky --cwd ~/app -- "Find and fix the flaky test in tests/api; run it 20 times to confirm."
reeve run --name review --cwd ~/app -- --model sonnet "Review it."
reeve ls                 # every worker: starting, running, blocked, idle, done or gone
reeve wait fix-flaky     # exits on the report, or on blocked, idle or gone
reeve watch fix-flaky    # a line per change: blocked, idle, gone, done
reeve report fix-flaky   # prints the report
```

The last argument after `--` is the prompt; claude flags such as `--model` go before it. It works in that checkout; for its own, pass claude's `-w NAME`.

For unattended work with a clear end, start the prompt with `/goal` and pass `--permission-mode auto` after `--`: a separate model checks it after each turn, so name the check.

A **blocked** worker waits for the user, such as a permission prompt: answer it in its tab, or with `claude attach ID` for a background one. If `reeve wait` says idle without a report, it ended its turn early: send it a message, then wait again.

A **gone** worker stopped without a report. See what it left (`claude logs NAME`, or its transcript under `~/.claude/projects/`), then restart it under the same name saying where to pick up, or report it lost. `reeve rm NAME` drops it from `reeve ls`.

Workers suit long or parallel work, and anything a successor should pick up. For short work whose answer you need now, a subagent is simpler.

## Handoffs

You don't see your own context usage: the hook `reeve setup claude` installs adds a "Context check" line to your prompt, or to the tool result that crossed it. Then finish the step in hand, write your notes and hand off; don't wait to be asked:

```sh
reeve handoff --name NAME-2 --notes handoff.md   # goal, what's done, what's next, open questions
```

It writes the handoff file with your workers and starts the successor as a named worker told to read it. Stop once it returns. Workers keep running; the successor checks `reeve ls` and `reeve report NAME` before redoing anything.

## Heavy commands on a shared machine

Agents share this machine. Run heavy commands through `reeve heavy`: test suites, e2e (even one spec), local CI, workspace typecheck or lint, builds and installs. It waits for a slot and exits with the command's code. Waits can be long, so never run it in the foreground: start it in the background (Claude Code: `run_in_background`, set by reeve's hook; Codex: a background terminal) and keep working; you're told when it ends.

```sh
reeve heavy -- cargo test
reeve heavy --label e2e -- pnpm test:e2e
reeve heavy ls           # who runs and who waits
```

You decide what's heavy; when in doubt, queue it. Only quick commands such as `git status` or one unit test skip it. `reeve heavy` limits load on the machine; it is not a lock: two commands that share one e2e database or port can still run at once.

## Questions for the user

`reeve ask "QUESTION"` queues a question and blocks until it is answered, then prints the answer; run it in the background and keep working. `reeve asks` lists open questions and `reeve answer ID "TEXT"` answers one.

## Sandbox

`reeve run --sandbox [NAME] -- ARGS` runs Claude Code with `--dangerously-skip-permissions` in a container that sees only the workspace, which suits long unattended work that would otherwise stop at permission prompts. It needs Docker. `reeve sandbox ls`, `exec` and `down` manage the sandboxes.

## When reeve itself gets in the way

```sh
reeve feedback "what you ran, what happened, what you expected"
```

It saves a note with reeve's version and recent events, and offers to file it.

## When not to bother

- `reeve` isn't on PATH: carry on without it.

`reeve doctor` checks the install, and `reeve --help` lists every command.
