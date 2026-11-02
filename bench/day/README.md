# The day scenario

`reeve-bench day <ARM> <N>` runs a working day in one repo, shop-kit ([repo/](repo/)), as run `rd-<ARM>-<N>`. It exists because the six-ask scenario never needed what reeve is for. See [FINDINGS.md](../FINDINGS.md#day-scenario) for results.

## What the day holds

- **A backlog of five independent tasks** ([asks.md](asks.md)): a bug, a small feature, a rename the user has to name, a red suite and a fixture rebuild.
- **A slow, shared test suite.** `npm test` takes a minute because its integration test holds the shared test database ([scripts/db.mjs](repo/scripts/db.mjs)). Every checkout on the machine, worktrees included, uses that one database, and a second job fails with "test database busy".
- **A long job.** `node scripts/rebuild-fixtures.mjs` holds the same database for six minutes.
- **A mid-run message.** Five minutes in, the user adds an urgent sixth task ([followups.json](followups.json)).
- **A context budget.** An arm's `contextBudget` sets where a session has to hand off: reeve's own hook (`REEVE_HANDOFF_TOKENS`), or, with `"handoff": "notes"`, the harness's own check, which asks for notes and starts a fresh session on them. Both arms use 60K. A fresh session starts at 18K (b0) or 23K (reeve) and works at 36–40K, so a budget near that hands off again and again. `reeve-bench day ARM N --budget TOKENS` overrides it for one run. Sessions now peak at 36–44K, so 45K and up never hands off, and 35K storms ([FINDINGS](../FINDINGS.md#batch-h-when-should-reeve-hand-off-partial-n2-resultsday-sonnet-thresholdstsv)).

Done means all six [hidden checks](hidden.mjs) pass on main, with the suite green on a private copy of the database. Every item fails if `test/db.json` changed. The fixture counts only if a rebuild finished on the shared database. [solution/](solution/) is a reference solution, which the tests use.

## The user

A scripted user ([user.json](user.json)) answers a minute after the session goes quiet:

- a rule answers a question line that holds one of its words, such as the rename;
- a question no rule covers goes to a cheap model (`--sim-model`, Haiku by default) with [key.md](key.md);
- after ten quiet minutes it asks for status.

The run ends when the session says `ALL DONE` (false done if the check fails), when the check has passed and the session has said nothing for five minutes since the later of the two (never claimed), or after 50 minutes.

## Arms

`arms/<ARM>/arm.json` sets every parameter of a run: `model`, `effort`, `settingSources`, `autocompact`, `contextBudget`, `handoff`, `manual` (a file `reeve --skill` prints instead of the built-in manual), `setup` (a dir under `bench/` with the arm's `settings.json` and a `repo/` overlay) and `system`. The run's sessions, workers and successors included, all start through a `claude` wrapper that adds the arm's flags. They run with their own `HOME`-level state: `XDG_STATE_HOME` and `REEVE_HOME` under the run's logs.

## Measures

`reeve-bench day-score [ARM...]` prints, per arm and round over valid runs, the round being the run's N without its trial number (`d2` for `rd-reeve-d2-3`): the budget, done, items, false done, never claimed, minutes to done, interventions, cost (all transcripts, priced per model), turns, wall minutes and the user model's cost, cache-read and output tokens, peak context of a turn, sessions, and reeve's own records: workers started, heavy runs and their wait, database collisions and handoffs. A run whose transcripts, stderr or driver log hold a sign of an API or network error (`API Error`, `ECONNRESET`, a failed call to the user model, …) is marked invalid and left out.

## Running it

- **A trusted trials dir.** `REEVE_BENCH_DIR` is a git repo, and each run a worktree inside it. `claude --bg`, which starts reeve's workers and successors, refuses a workspace that isn't trusted, and Claude Code trusts a worktree through its main repo, so open the trials dir in Claude Code once and accept.
- **Detached drivers.** Start `reeve-bench day` with `nohup`, not as a session's background task: a session that closes, after a handoff for instance, takes its background tasks with it, and the run is lost.

## Testing the harness

`cargo test -p reeve-bench` runs the whole driver against a fake `claude` and a fake `reeve`, and checks the reference solution against the hidden checks. No model is called.
