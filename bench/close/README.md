# The close scenario

`python3 -I bench/close/run.py ARM N --bin DIR --trials DIR` runs a coordinator that hands eight small tasks to parallel workers, as run `rc-ARM-N`. It asks one question: does closing finished workers lose work? It exists for automatic closing (#61), which closes a done worker 2 minutes after its report, with no one calling `reeve close`. See [FINDINGS.md](../FINDINGS.md#close-scenario) for results.

## What the run holds

The coordinator gets the tasks in one message ([run.py](run.py), `TASKS`), each for a named worker in its own git worktree. Every task's work carries a random token. The tasks leave the states a close can lose:

- three **normal** workers: write a note, commit, push;
- **uncommitted:** write a draft and don't commit it, the user reviews it first;
- **unpushed:** commit a note and don't push it;
- **git-ignored under `.claude/`:** write `.claude/notes/scratch.md`, nothing to commit;
- **a job still running:** start `node scripts/job.mjs`, about 4 minutes, in the background, and report at once; the job writes `out/job.txt` (untracked, not ignored) when it finishes;
- **a late message:** once the late worker has pushed, the user tells the coordinator to have that worker append one more line. A reeve coordinator relays it with `reeve send`, so it can sit unread in the worker's inbox.

Each worker ends by running `node scripts/stamp.mjs NAME`, which prints a random stamp code and records it. The code is in no prompt, so it reaches the coordinator only through the worker's report.

The coordinator is asked to follow the workers until each has reported, then list every stamp code and say `ALL DONE`. Nothing in the prompt mentions closing or tidying up: each arm does what its own setup says.

## Arms

`arms/ARM.json`: `kind` (`vanilla` or `reeve`, which picks the one-line hint in the prompt), `setup` (a dir under `bench/` with `settings.json` and a `repo/` overlay, as in the [day scenario](../day/README.md#arms)), `model` (the coordinator's), `workerModel` (every other session's), `env` and `tag` (added to worker names so runs' branches don't meet in the shared origin).

- **vanilla:** plain Claude Code, `arms/b0-iso`: none of the owner's user setup, auto mode. The hint says to use background subagents with worktree isolation; `CLAUDE_CODE_SUBAGENT_MODEL` makes them Haiku. No reeve is on its PATH.
- **reeve-main / reeve-pr61:** `arms/reeve`, what `reeve setup claude --claude-md` installs. The hint says to use `reeve run --worktree`. `--bin` picks the reeve build: main's manual says to `reeve close` each done worker; #61's says a done worker's tab closes by itself.

The coordinator is Sonnet; workers are Haiku. Every session starts through a `claude` wrapper that adds the arm's flags and the run's settings, whose `env` carries the run's own `XDG_STATE_HOME` and `REEVE_HOME`, so `claude --bg` workers keep their reeve state in the run.

## When it ends

Once the coordinator says `ALL DONE` after the late message, the driver waits 11 more minutes, then measures. If it never says so, the run stops at 25 minutes (`timeout`).

## Measures

- **Lost work** (headline, target 0): an item that existed and is gone. The driver looks for every token every 10 s and at the end, in a file of the expected name anywhere under the run's repo, worktrees included, or in a commit reachable from a ref of the repo or the origin. Also lost: the job killed before it finished, or its output gone, and a stamp code that never reached the coordinator's transcript or stream.
- **Not done:** an item nobody made. That's a task failure, not a close's, so it's counted apart.
- **Closes by the coordinator:** its tool calls that close or tidy up a worker: `reeve close`, `git worktree remove|prune`, `claude stop|kill|rm`, `TaskStop`, `KillShell`.
- **Tabs open 10 minutes after done:** Claude Code sessions, other than the coordinator and its children, whose working directory is in the run, 11 minutes after `ALL DONE`.
- **Coordinator tokens and cost:** its own transcript, subagents excluded. Total cost covers every transcript of the run.

`python3 -I bench/close/run.py --score --trials DIR [ARM...]` prints median [min–max] per arm, then one row per run.

## Running it

The trials dir is a git repo trusted once in Claude Code, and each run a worktree of it, as for the [day scenario](../day/README.md#running-it). The runs share its config, so they share one bare `origin.git` in it. Start runs detached, under `reeve heavy --lock reeve-bench`.
