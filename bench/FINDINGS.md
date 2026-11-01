# Bench findings

What each experiment tried, what it measured and why it won or lost. Newest batch last. Score tables per batch are in [results/](results/).

## Goal and bar

Find the first small, innovative add-on that beats a vanilla Claude Code session. Two baselines:

- **B0, vanilla:** one `claude -p` session (Sonnet) with no extra instructions. It reads the scenario repo's `CLAUDE.md`.
- **B1, comm:** B0 plus [skills/comm/SKILL.md](../skills/comm/SKILL.md) appended to the system prompt. It starts a background worker (`claude --bg -w`) per ask and relays results and decisions.

A candidate wins if it beats B1 and B0 on its target metric without being worse on the others: interventions, time to done, cost, false done and unchecked landings.

## Setup

- `reeve-bench run <arm> <n>` makes the scenario repo a worktree of a trusted base repo. It starts the arm's session with `claude -p --input-format stream-json` and sends the asks as the first user turn.
- A simulated user (`claude -p --safe-mode`, Sonnet) knows only `scenario/key.md` and replies to what the session said. Cross-session messages from workers wake the session.
- **Interventions** are user messages after the first; a queued answer (C3) also counts. **Done** is a hidden check on the main branch ([check.rs](../crates/reeve-bench/src/bench/check.rs), [hidden.mjs](scenario/hidden.mjs)). **False done** means it claimed done but the check fails. **Unchecked landings** counts commits that reached main without a CI pass. **Cost** is token usage at Sonnet list prices, summed over all transcripts including subagents and workers.
- Bench sessions run with the owner's user settings in auto mode, plus project settings that turn plugins and `~/work/.mcp.json` servers off. HERDR/ORCH variables are stripped.

### Scenario versions

| Version | Batches | Change |
|---|---|---|
| v1 | smoke | 4 asks: mean bug, median, rename (asks the user), red test. Every arm hit the floor in about 3 minutes, which was too easy. |
| v2 | 2 | 6 asks: v1 plus summary() (depends on median) and a flaky shuffle test. A follow-up at 2.5 min changes summary's empty-list behaviour. |
| v3 | 3, 4 | v2 plus fake CI (`scripts/ci.mjs`): pending 2 min after each commit, then runs tests and a README docs check. Rule: merge only when the branch CI passes. Done means merged and CI passes on main. |
| v5 | 5 | v3 plus a realistic reply latency: the simulated user replies 4 minutes after the session goes idle, or after a question is queued (was 20 s). |

The clean minimum is 1 intervention, the rename decision.

## Batches

### Batch 2, scenario v2, Node harness, n=3

| Arm | Interventions | Min to done | Cost $ |
|---|---|---|---|
| B0 | 1 | 3.7 | 0.71 |
| B1 | 1 | 7.8 | 3.12 |

- Both arms hit the floor; B0 was faster and cheaper.
- In one B1 run, 5 of 6 workers stuck `blocked` on permission prompts. The communicator restarted them twice (15 sessions).

### Batch 3, scenario v3, Node harness, n=3

| Arm | Interventions | Min to done | Cost $ |
|---|---|---|---|
| B0 | 1 [1–3] | 10.6 | 1.66 |
| B1 | 1 | 11.5 | 4.73 |

- **B0, classifier:** the auto-mode classifier denied `git merge --ff-only` into main three times across two runs. That cost 2 recovery interventions ("you're authorised").
- **B0, rule:** B0 also committed straight to main, breaking the CI rule.
- **B1:** one communicator never claimed done and kept working until the 40-minute cap.
- **Check fix:** a "false done" turned out to be the check asserting `median([])` throws, which the asks never specified. The check was dropped and the run re-scored.

### Batch 4, scenario v3, Rust harness, n=3

| Arm | Interventions | Min to done | Cost $ | Unchecked landings |
|---|---|---|---|---|
| B0 | 1 | 10.1 [9.6–13.3] | 1.05 [0.82–1.62] | 1 [0–2] |
| B1 | 1 | 16.1 [11.2–20.4] | 5.91 [4.87–6.62] | 1 [0–2] |
| C1 | 1 | 14.6 [8.0–15.5] | 1.33 [1.14–3.13] | 1 [1–2] |

- No classifier denials happened in this batch.
- The unchecked-landings numbers are noisy: a `--rerun` of the flaky test overwrote an earlier pass. CI records now keep `everPassed`.
- One B1 run was killed by heavy's idle watchdog and re-run as a `heavy --server` job.

## Candidates

### C1: CI gate plus pre-authorised merges, on B0. Lost.

- **What:** `reeve gate install --protect <branch> --ci '<cmd {rev}>'` installs a git `reference-transaction` hook ([src/gate.rs](../src/gate.rs)). It refuses to move the protected branch to a commit without a CI pass; for a merge made on the branch, it checks the merged tip. The refusal message names the compliant route. Project allow rules (`Bash(git merge *)`, `Bash(git push . *)`, `Bash(node scripts/ci.mjs *)`) pre-authorise landing, so the classifier never sees it. The idea: a mechanical gate makes that pre-authorisation safe.
- **Hypothesis:** zero recovery interventions from classifier denials and zero unchecked landings, with cost close to B0.
- **Result (batch 4, n=3):**
  - Interventions: 1, the same as both baselines.
  - Time: 14.6 min, against 10.1 for B0.
  - Cost: $1.33.
- **Why it lost:** the classifier denials it targets are sporadic (3 in batch 3, 0 in batch 4), so there was nothing to save. Enforcing CI cost time against a vanilla session that cut corners. Unchecked landings could not separate the arms while flaky reruns overwrote passes.
- **Keep for later:** the gate is a sound local stand-in for required checks, if the bench ever adds unsupervised landings.
- **Removed** on 1 Nov 2026, with the C1 arm, as unused; the code is in git history.

### C2: `reeve watch`, an asyncRewake Stop hook that wakes the session when CI finishes, on B0. Dropped before running.

- **What:** about 70 lines of Rust; the patch is [candidates/c2-watch.patch](candidates/c2-watch.patch). It was verified that asyncRewake wakes a stream-json `claude -p` session about 5 s after the hook exits 2.
- **Hypothesis:** with 8-minute CI, vanilla ends its turn while CI is pending, so the user has to poll.
- **Why dropped:** a critic read the B0 transcripts. Vanilla never sits idle on pending CI. It uses foreground `until` loops, `run_in_background` loops, the Monitor tool and scheduled wakeups. A wake hook would tie, and the bench's 6-minute poll was the only way it could win.

### C3: `reeve ask`, a decision queue that doesn't block the agent, on B0. Batch 5.

- **What:** `reeve ask "<question, options, recommendation>"`, run with `run_in_background`, writes the question to `$REEVE_HOME/asks/` and waits for the answer. The agent keeps working, and the answer lands in the session as a background-task notification. `reeve asks` lists open questions; `reeve answer <id> <text>` answers one ([src/ask.rs](../src/ask.rs)). One appended system-prompt line tells the agent about it ([arms/c3/system.md](arms/c3/system.md)). In the bench, the simulated user reads the queue even while the session is busy, with the same 4-minute latency.
- **Hypothesis:** in batch 4, vanilla always finished the rename last. It wrote the question inside a long CI-waiting turn, and the user only saw it when the turn ended. A queue the user reads across sessions surfaces it at once, and the agent keeps working while it waits, so time to done should fall at 1 intervention and B0's cost.
- **Control, C3b:** B0 plus one line and no code: "ask as soon as you know, then end your turn".
- **Result (batch 5, stopped early at 7 of 12 runs because the plan's usage ran out; n=2 for B0, C3, C3b and n=1 for B1):**

| Arm | Interventions | Min to done | Cost $ | Rename landed at (min) |
|---|---|---|---|---|
| B0 | 1, 1 | 16.6, 12.4 | 1.27, 0.85 | 16.5, 12.4 (last both times) |
| B1 | 1 | 23.9 | 7.33 | 19.0 |
| C3 | 1, 2 | 10.7, 21.8 | 1.21, 1.10 | 10.6, 14.7 (not last in run 2) |
| C3b | 1, 1 | 19.9, 26.6 | 1.56, 2.08 | 19.8, 26.6 (last both times) |

- **Verdict: not proven; promising.**
  - **The mechanism works.** Both C3 runs queued the rename question as one of their first commands. The answer came back while the agent kept working, and the rename stopped being the last item.
  - **Run 1 beat everything:** 10.7 min, against B0's best of 12.4, at B0's cost.
  - **Run 2:** the auto-mode classifier denied a merge into main. That cost a recovery intervention and about 10 minutes, so the median (16.2) lands above B0's (14.5).
  - **The no-code control (C3b) was worst on time.** Ending the turn to ask stalls every other item. A one-line instruction does not get the effect; the queue does.
  - **C3 clearly beats B1** on time and cost.
  - **Against B0, n=2 can't separate C3 from noise.** The noise is mostly the sporadic classifier denial C1 was built for.
- **Next:** run C3 together with C1's narrow allow rules for landing (no gate), so that classifier denials stop adding noise, at n≥5 against B0. Also measure "time from question to answer" separately from total time.
- **Removed** on 1 Nov 2026, with the C3 arm and the bench's queue reader, as unused; the code is in git history.

## What we learned so far

- **Vanilla is the bar to beat, not the communicator.** On one repo with six small asks, B0 is fastest and cheapest. The comm skill (B1) costs 5–7x and is slower, with the same interventions.
- **Vanilla already waits on CI** with until-loops, Monitor and background tasks. Wake hooks tie.
- **The gaps that showed up:**
  - sporadic auto-mode classifier denials of merges into main, which cost recovery interventions and minutes;
  - decisions that sit unseen inside long busy turns.
- **Next step:** C3 (a decision queue) addresses the second gap, and C1's allow rules the first. Neither is proven at n≤3.
- **The bench floors at 1 intervention**, so wins have to show up in time and cost unless the scenario adds more decisions.

## Block bench: does the CLAUDE.md block get sessions to use reeve?

`reeve-bench block` ([bench_block.rs](../crates/reeve-bench/src/bench_block.rs)) asks whether the block `reeve setup claude --claude-md` wrote (dropped since, see align-heavy-n3) is enough on its own, for an orchestrator and for a worker under Herdr. It builds on `reeve-bench entry`: fresh `claude -p` sessions, a shimmed `reeve` that logs its calls and starts nothing, and the same scratch workspace.

### Setup

- **Arms:** the only instruction text a session gets, as the workspace's `.claude/CLAUDE.md`. A0 has no block. A1 is the slim block, with uses and no syntax ([setup.rs](../src/setup.rs) `block()`). A2 is the old block with command syntax (`OLD_BLOCK`, the text in CLAUDE.md files written at a3575f4). A3 is A1 plus the `reeve --skill` output pasted in. A4 and A5 are A3 with a fixed manual instead of the current one: A4 has [manual-before-goal.md](manual-before-goal.md), the manual of #14, and A5 has [manual-goal-line.md](manual-goal-line.md), the manual of the blocked-workers PR before its "after `--`". In those arms the shim also answers `reeve --skill` with that manual. A6 has no block, only the skill stub `reeve setup claude` writes, so its description is the one always-loaded text. Apart from A6, no arm gets the skill stub, and none gets the SessionStart hook, which prints nothing when no workers run.
- **Isolation:** `--setting-sources=project --strict-mcp-config --disable-slash-commands --no-session-persistence --no-chrome`, auto-memory off, and the caller's `HERDR_*`, `REEVE_*` and `CLAUDE_CODE_*` variables removed. Workspaces live under `$TMPDIR`. Claude Code reads `CLAUDE.md` and `.claude/CLAUDE.md` in every ancestor of the workspace, whatever `--setting-sources` says, so a workspace under the home dir reads `~/.claude/CLAUDE.md`. The bench refuses to start if any ancestor holds one. The entry bench isn't isolated: its sessions run under `~/.local/state` with the owner's CLAUDE.md, skills, plugins and MCP servers.
- **Shims:** `reeve run`, `handoff` and `ask` only say they started. `heavy` runs its command at once with `REEVE_HEAVY_JOB=1`, so the suite logs whether it ran through heavy. `run --name … --bg` is refused under Herdr, as the real reeve refuses it. Before faking any call, the shim asks the real parser (`reeve-bench parse`, hidden) and refuses, with reeve's own error, what reeve would refuse; such calls are logged as `rejected`. `herdr` is a shim that opens nothing. Only `--skill`, help and `hook` reach the real binary.
- **Roles:** both run with `HERDR_ENV=1`. The orchestrator's brief says it coordinates several tasks for the user. The worker's brief says an orchestrator handed it one task, and it ends like `workers::with_report` once the blocked-workers PR lands: the report also when blocked, starting with `done`, `partial` or `failed`, and no sentence on `reeve heavy`, so that only the arm speaks of reeve.
- **Cases:** *heavy* (run the suite), *parallel* (two independent multi-hour jobs), *handoff* (a Context check arrives with the prompt through a UserPromptSubmit hook, worded as `reeve hook context` words it), *short* (a one-line typo fix) and *away* (parallel, with the user away for the day).
- **Scores:** whether the run fits its case with no false positive: the suite only through heavy, a named worker without `--bg`, `reeve handoff`, and on the short case no worker or handoff. It also scores whether `reeve --skill` came before the first act, refused `--bg` calls, rejected calls, false positives, turns and cost, and per started worker whether its prompt starts with `/goal`, whether it passes `--permission-mode auto` after `--`, and whether it runs in a sandbox.

### Smoke, Sonnet, one run per arm ([results/block-smoke.tsv](results/block-smoke.tsv))

| Arm | Role, case | Fits | Skill first | Turns | Cost $ |
|---|---|---|---|---|---|
| A0 | orch, heavy | no: ran `npm test` directly | no | 3 | 0.017 |
| A1 | worker, parallel | yes: two named workers, no `--bg` | yes | 6 | 0.042 |
| A2 | orch, handoff | yes | yes | 3 | 0.034 |
| A3 | worker, short | yes: direct fix, no reeve | no | 4 | 0.033 |

- The harness works end to end, but each arm saw a different case, so this says nothing about the arms yet.
- **Cost:** a mean of $0.032 per Sonnet run. The full run is 4 arms × 2 roles × 4 cases × n=3 = 96 runs, about $3–5. The Opus pass (two arms, n=2) is 32 runs, about $2–5. That is well under the $25 bar, so n=3 stands.
- **Not run yet:** the full grid and the Opus pass were held back because the plan's weekly usage was at 92%.

To run the full grid: `reeve-bench block --n 3 --jobs 4`, then `reeve-bench block --n 2 --model opus --arm <best> --arm <second>`.

### Full grid, Sonnet, n=3 ([results/block-sonnet-n3.tsv](results/block-sonnet-n3.tsv))

210 runs, $8.95. Cells are runs that fit out of runs; skill first and false positives are out of 15.

| Arm | Role | heavy | parallel | handoff | short | away | skill first | --bg refused | false pos. | turns | cost $ |
|---|---|---|---|---|---|---|---|---|---|---|---|
| A0 | orch | 0/3 | 0/3 | 3/3 | 3/3 | 0/3 | 0 | 0 | 0 | 2.9 | 0.01 |
| A0 | worker | 0/3 | 0/3 | 3/3 | 3/3 | 0/3 | 0 | 0 | 0 | 4.3 | 0.06 |
| A1 | orch | 3/3 | 3/3 | 3/3 | 3/3 | 3/3 | 12 | 0 | 0 | 4.3 | 0.04 |
| A1 | worker | 3/3 | 3/3 | 3/3 | 3/3 | 2/3 | 12 | 0 | 0 | 4.9 | 0.04 |
| A2 | orch | 3/3 | 1/3 | 3/3 | 3/3 | 2/3 | 10 | 4 | 2 | 4.1 | 0.04 |
| A2 | worker | 3/3 | 2/3 | 3/3 | 3/3 | 0/3 | 9 | 8 | 4 | 4.7 | 0.04 |
| A3 | orch | 3/3 | 3/3 | 3/3 | 3/3 | 2/3 | 10 | 0 | 0 | 3.6 | 0.04 |
| A3 | worker | 3/3 | 3/3 | 3/3 | 3/3 | 0/3 | 5 | 0 | 0 | 5.5 | 0.05 |
| A4 | orch | 3/3 | 3/3 | 3/3 | 3/3 | 3/3 | 10 | 0 | 0 | 3.9 | 0.04 |
| A4 | worker | 3/3 | 3/3 | 3/3 | 3/3 | 0/3 | 9 | 0 | 0 | 4.6 | 0.05 |
| A5 | orch | 3/3 | 3/3 | 3/3 | 3/3 | 3/3 | 9 | 0 | 0 | 3.1 | 0.03 |
| A5 | worker | 3/3 | 3/3 | 3/3 | 3/3 | 2/3 | 8 | 0 | 0 | 4.8 | 0.05 |
| A6 | orch | 3/3 | 3/3 | 3/3 | 3/3 | 3/3 | 12 | 0 | 0 | 5.7 | 0.05 |
| A6 | worker | 3/3 | 3/3 | 3/3 | 3/3 | 2/3 | 12 | 0 | 0 | 7.3 | 0.06 |

- **A0 is the floor:** it never uses reeve where it fits (heavy, parallel, away 0/3), and only "passes" handoff and short because a session that does nothing fits those.
- **A1 and A6 tie at the top:** every orchestrator case fits, 12/15 skill first, and the worker misses one away run each.
- **A2 (old block, syntax) is the only arm with refused `--bg` calls and false positives.** Dropping syntax from the block was right.
- A3 to A5 (block plus pasted manual) are no better than A1 and use the skill first less often.
- **Possible internet drop:** runs between about 09:53 and 09:58 (Tbilisi) were checked for `API Error`, `is_error`, error subtypes, a non-null `api_error_status` and non-empty stderr. None of the 210 had any, so nothing was excluded and the table stands.

### Opus pass, A1 and A6, n=2 ([results/block-opus-n2.tsv](results/block-opus-n2.tsv))

40 runs, $4.63, no errored runs.

| Arm | Role | heavy | parallel | handoff | short | away | skill first | --bg refused | false pos. | turns | cost $ |
|---|---|---|---|---|---|---|---|---|---|---|---|
| A1 | orch | 2/2 | 2/2 | 2/2 | 2/2 | 2/2 | 8/10 | 0 | 0/10 | 5.7 | 0.10 |
| A1 | worker | 2/2 | 0/2 | 1/2 | 2/2 | 2/2 | 6/10 | 0 | 0/10 | 7.1 | 0.08 |
| A6 | orch | 2/2 | 2/2 | 2/2 | 2/2 | 2/2 | 8/10 | 0 | 0/10 | 8.3 | 0.13 |
| A6 | worker | 2/2 | 2/2 | 2/2 | 2/2 | 1/2 | 8/10 | 2 | 1/10 | 9.2 | 0.15 |

- **Orchestrators tie again:** 10/10 fit for both arms. A6 takes 2.6 more turns and costs about 30% more.
- **Workers differ, but on 2 runs per cell.** A1 started no workers in both parallel runs (it did the jobs itself) and skipped `reeve handoff` once. A6 fit all of those but in one away run tried `--sandbox`, then `--bg` twice (refused, a false positive) before a valid call.
- **n=2 can't separate them.** Three A1 worker misses against one A6 worker miss is 3 of 10 against 1 of 10 cases; that is a hint, not a result.

### Verdict: A1 (slim block) over A6 (skill stub only), with one open risk

**Decision (Leon): the A1 block ships.** The block stays as `reeve setup claude --claude-md` writes it, PR 3 (drop the block for the skill stub, A6) is not built, and main's `setup::block()` is the A1 text the bench measured. The open risk below was closed on 1 Nov (see [November round](#november-round-1-nov)).

- **Outcomes tie on Sonnet,** the model with n=3 (orchestrators identical, workers identical).
- **A1 is cheaper and quicker:** 1.4 fewer turns on Sonnet orchestrators and 2.4 on workers, and 23–47% less cost on Opus. A6 spends the extra turns on reading the skill.
- **A1 never produced a refused `--bg` call or a false positive** in its 50 runs across both models; A6 did once in its 50, on Opus.
- **Open risk (closed 1 Nov: A1 workers fit 10/10 at n=5):** on Opus, A1 workers missed parallel (0/2) and handoff (1/2) where A6 did not. If a worker should split its own task or hand off on a Context check, run Opus workers on parallel and handoff at n≥5 per arm before relying on the block alone. If that holds, the fix to try is a line in the block on those two cases, not the skill stub.
- **Limits:** Opus is n=2 per cell, so it can't rank the arms. The bench runs in acceptEdits mode, not auto, the worker brief omits the `reeve heavy` sentence real workers get, and the CLAUDE.md block is a project file, not the user-level one.

### Align runs, Sonnet, orchestrator unless noted

| Run | Arms | Case | n | Outcome |
|---|---|---|---|---|
| [align-goal-smoke](results/align-goal-smoke.tsv) | A3, A4 | parallel | 1 | both fit, 2 workers each, none under `/goal` |
| [align-away-smoke](results/align-away-smoke.tsv) | A3, A5 | away | 1 | both fit, 2/2 workers under `/goal` each, 0 in auto mode |
| [align-away-n3](results/align-away-n3.tsv) | A3, A5 | away | 3 | 3/3 fit each; 6/6 workers under `/goal` in auto mode each; 1 of 3 runs each had 2 calls rejected, then fixed them |
| [align-a6-parallel-smoke](results/align-a6-parallel-smoke.tsv) | A6 | parallel | 1 | fits, 2 workers |
| [align-heavy-n3](results/align-heavy-n3.tsv) | A1, A6 | heavy, both roles | 3 | 6/6 fit each, suite only through heavy; A6 takes 2–3 more turns (6.0 vs 4.0 orch, 9.3 vs 6.3 worker) |
| [align-short-n3](results/align-short-n3.tsv) | A1, A6 | short, both roles | 3 | 6/6 fit each, no false positive; A6 $0.026–0.033 vs A1 $0.020–0.025 |

- **The checked shim changed a conclusion.** Before calls went through the real parser, the `/goal` line's workers looked as if they never start. With reeve's error shown, a run that puts `--permission-mode auto` before `--` reads it, retries once and fits. That's why the manual now says "after `--`".
- **A3's rows in the away runs** ran on a prototype whose manual also taught a `reeve run --goal` flag, which was parked and isn't in reeve. A rerun of A3 measures a different manual. A5's rows are the manual-line answer.
- [align-away-smoke-contaminated](results/align-away-smoke-contaminated.tsv) is kept for the record: there the shim answered `reeve --skill` with the current manual in every arm, so A3 and A5 saw the same text. The bench now answers with the arm's own manual.
- **A1 against A6** changes no outcome on heavy or short, at a few more turns for A6. Not measured: A6 on parallel or handoff at n > 1, and any model besides Sonnet.

## November round (1 Nov)

Three batches after #37–#41, on `reeve-bench` at dedf9ad plus an arm `args` file (below). Each batch was measured against the plan's weekly limit; see [Cost against the weekly limit](#cost-against-the-weekly-limit).

### Batch N1: block smoke after the manual changes, Sonnet, n=1 ([results/block-smoke-nov.tsv](results/block-smoke-nov.tsv))

`docs/skill.md` changed in #37, #38 and #40. A1 is the slim block; A3 is A1 with the current manual pasted in. 20 runs, $0.90.

| Arm | Role | heavy | parallel | handoff | short | away | skill first | false pos. | turns | cost $ |
|---|---|---|---|---|---|---|---|---|---|---|
| A1 | orch | 1/1 | 1/1 | 1/1 | 1/1 | 1/1 | 4/5 | 0/5 | 4.6 | 0.04 |
| A1 | worker | 1/1 | 1/1 | 1/1 | 1/1 | 0/1 | 4/5 | 0/5 | 4.6 | 0.04 |
| A3 | orch | 1/1 | 1/1 | 1/1 | 1/1 | 1/1 | 4/5 | 0/5 | 4.8 | 0.04 |
| A3 | worker | 1/1 | 1/1 | 1/1 | 1/1 | 0/1 | 4/5 | 0/5 | 5.0 | 0.05 |

- **Verdict: no regression.** Both arms match the n=3 grid. The one miss per arm is the known worker away case: the worker ran the suite 20 times through heavy but did both "multi-hour" jobs itself, because the repo is two small files.

### Batch N2: Opus workers on parallel and handoff, n=5 ([results/block-opus-workers-n5.tsv](results/block-opus-workers-n5.tsv))

The open risk from the Opus pass. 20 runs, $2.46.

| Arm | Role | parallel | handoff | skill first | --bg refused | false pos. | turns | cost $ |
|---|---|---|---|---|---|---|---|---|
| A1 | worker | 5/5 | 5/5 | 10/10 | 0 | 0/10 | 5.0 | 0.11 |
| A6 | worker | 5/5 | 5/5 | 10/10 | 0 | 0/10 | 7.2 | 0.14 |

- **Verdict: the risk is closed.** A1 workers started two named workers in every parallel run and ran `reeve handoff` in every handoff run. The 0/2 and 1/2 of the Opus pass were noise. No extra block line is needed.
- **A1 stays cheaper:** 2.2 fewer turns and about 20% less cost than A6, as before.

### Batch N3: scenario, B0 isolated against reeve's current setup, n=3 ([results/scenario-reeve-nov.tsv](results/scenario-reeve-nov.tsv))

- **Why a new B0:** `reeve-bench run` starts sessions with the owner's user settings. Those now carry reeve: the global CLAUDE.md block, the skill, and the SessionStart and context hooks. A probe confirmed a plain `claude -p` there sees all three. So "B0" was no longer vanilla.
- **Arms:**
  - **b0-iso:** `--setting-sources project,local` through the arm's new `args` file, so no user settings, CLAUDE.md, skills, hooks or plugins, and auto mode from the arm's `settings.json`.
  - **reeve:** b0-iso plus what `reeve setup claude --claude-md` installs, as project files: the hooks and allow rules in `settings.json`, and the skill stub and the A1 block under `repo/.claude/`.
  - The init events confirmed both ran in auto mode, and that the reeve skill was the only difference in their skill lists.
- **Isolation:** the trials dir and base repo were under `$TMPDIR` (`REEVE_BENCH_DIR`, `REEVE_BENCH_BASE`), with no ancestor CLAUDE.md.
- **Harness:** the current one, so the 4-minute reply latency of v5 on scenario v3. Pairs ran one b0-iso and one reeve run at a time.
- **Outage:** an internet drop at about 18:50–19:20 Tbilisi killed pair 3. The simulated user exited and rv-reeve-3 logged API errors. Both runs were set aside unscored (they are in the TSV, marked invalid) and rerun as pair 4.

| Arm | Interventions | Done | Min to done | Cost $ | Rename landed |
|---|---|---|---|---|---|
| b0-iso | 1, 1, 1 | 3/3 | 14.3 [13.8–15.5] | 0.41 [0.39–0.46] | last in all 3 |
| reeve | 1, 1, 1 | 3/3 | 14.4 [11.9–18.0] | 0.54 [0.46–0.68] | last in all 3 |

- **Verdict: no difference, at about 30% more cost.** Both arms hit the 1-intervention floor and the same median time. Neither had a false done.
- **reeve's new pieces were never exercised.** The reeve sessions read `reeve --skill` first and then worked like vanilla:
  - they ran the sub-second suite directly;
  - they started no workers;
  - they never sent a message or handed off.

  So this scenario can't show whether inbox `reeve send` or handoff auto-close change anything. The extra cost is the manual and the hooks in context.
- **`neverClaimed` is a harness artifact.** It fired in one run per arm, though both claimed done. With the 4-minute reply delay and the 5-minute claim grace, a claim counts only if it comes within about a minute of the check passing. Time to done comes from the check, so the table stands.
- **b0-iso cost less than batch 4's B0** ($0.41 against $1.05). That B0 ran with the owner's whole user setup, under v3 latency, so the two aren't compared further.
- **Next:** reeve's worker and handoff features need a scenario that uses them: the backlog bench in [PLAN.md](PLAN.md), or a long task that crosses the handoff threshold.

### Cost against the weekly limit

Readings of `usage --plan` (whole percents of the weekly limit, reset 2026-11-05 06:00 UTC):

| Time (UTC) | Weekly | Point |
|---|---|---|
| 13:35 | 25% | task start, before the merge train finished |
| 14:04 | 26% | before N1 |
| 14:08 | 26% | after N1, before N2 |
| 14:12 | 26% | after N2, before N3 |
| 15:53:32 | 26% | after N3's last run |
| 15:53:51 | 27% | the same, 20 s later |

| Batch | Runs | $ (bench-reported) | Weekly Δ |
|---|---|---|---|
| N1 | 20 | 0.90 | 0 |
| N2 | 20 | 2.46 | 0 |
| N3 | 8 (6 scored, 2 lost to the outage) | 3.45 (2.94 scored, 0.51 lost) | +1 |
| **All** | 48 | **6.81** | **+1 (26→27)** |

- **Ratio: about $7 of bench-reported cost per 1% of the weekly limit.** The readings are whole percents, so the true change across the three batches is between 0 and 2 points. That means the ratio is at least $3.4 per 1%, with no useful upper bound.
- **The $ undercount the real spend:**
  - The block bench reports `total_cost_usd` from `claude -p`.
  - The scenario bench prices transcripts at Sonnet list prices and leaves out the simulated user's calls.
  - This orchestrating session (Opus) is outside both.
- **Noise:** other sessions shared the plan:
  - this session;
  - `reeve-merge-train` (until 14:02);
  - `reeve-prompt-pipeline`;
  - `tr-3479-onto-main`;
  - `weekend-session-3`;
  - the owner's own work on a Trustroots PR during N3.

  `reeve ls` and `herdr agent list` showed no others. The outage (18:50–19:20 Tbilisi) cost a pair and about 20 minutes.
- **What it buys:** at about $7 per point, the plan's backlog calibration (about $10) is about 1.5% of a week, and a 30-minute parallel arm at n=2 ($20–60) is 3–9%.

## Day scenario

The six-ask scenario never needed reeve's workers, heavy queue or handoffs, so [day/](day/README.md) is a working day in one repo: five backlog tasks, an urgent sixth at minute 5, a 60 s suite and a 6-minute fixture rebuild on one shared test database, a scripted user, and a context budget that forces handoffs. All runs: Sonnet, `--setting-sources project,local`, auto mode, budget 30K, n=3 per arm. Per-run rows are in [results/day-sonnet-30k.tsv](results/day-sonnet-30k.tsv). No run showed an API or network error, so none is invalid on that count.

### Batch D1: b0 against reeve, with the handoff broken

- **Arms:**
  - **b0:** vanilla; past 30K the harness asks for notes and starts a fresh session on them.
  - **reeve:** b0 plus `reeve setup claude --claude-md`, with reeve's own hook at 30K.
- **Broken:** the trials dir wasn't a trusted workspace, so `claude --bg` refused every `reeve handoff` successor. The reeve sessions carried on past the budget in one session (peak 51–61K). One of them (rd-reeve-2) obeyed the manual's "stop once it returns", waited for the user, and lost 20 minutes to two status polls.
- **Outage:** the owner's network dropped at 17:15–17:24 UTC, during D1. A scan of every transcript, stderr and driver log found no error sign, and rd-reeve-2's stall follows its failed handoff by the timestamps, so no D1 run was set aside.

| Arm | Done | Min to done | Interventions | Cost $ | Turns | Heavy runs |
|---|---|---|---|---|---|---|
| b0 | 2/3 | 20.9 [18.3–23.4] | 1 [1–3] | 0.43 [0.39–0.77] | 22 [19–41] | 0 |
| reeve | 3/3 | 25.4 [15.8–40.1] | 2 [1–3] | 1.03 [0.64–1.18] | 47 [28–55] | 8 |

- **b0's miss is real:** rd-b0-2's successor never did the rename and timed out at 50 minutes, through three status polls.
- **reeve costs about 2.4x b0, from turns.** b0 ran the 60 s suite in the foreground. reeve's sessions put every `reeve heavy` in the background, as the manual says, and each result came back as a notification and a new turn.

### Batch D1t: reeve with working handoffs

The trials dir became a git repo trusted once in Claude Code, with every run a worktree inside it (worktrees inherit their main repo's trust). Five runs started; t1 and t2 were started as background tasks of the session that ran the batch and died with it when that session handed off. They are lost, unscored, and t4 and t5 replace them.

| Arm | Done | Min to done | Interventions | Cost $ | Turns | Sessions |
|---|---|---|---|---|---|---|
| reeve | 3/3 | 29.4 [27.0–40.3] | 3 [2–4] | 1.13 [0.90–3.82] | 55 [43–150] | 2 [2–13] |

- **Handoffs work, and then they storm.** Each run's first handoff succeeded and its successor finished the day. But a fresh Sonnet session with reeve's setup starts at about 30–37K, so a successor is past a 30K budget on its first turns and hands off again. rd-reeve-t5 went through 13 sessions and 150 turns for $3.82; the lost rd-reeve-t1 had run `reeve handoff` 13 times, twice 1.4 s apart, and the lost rd-reeve-t2 ended with two successors of the same name working side by side.
- **So 30K is miscalibrated.** It sits below a session's working floor; reeve's default is 350K. A budget for this scenario needs to be the floor plus room for a task or two, about 60–80K. D1 and D1t are kept as measured, with the arms at 30K, so the numbers stay reproducible.
- **`day-score`'s `handoffs` undercounts:** it reads one reeve state, so it shows 1 for t5. The `sessions` column is the signal.

### Candidate R1: background a heavy run only when it would wait. Dropped.

From D1's turn count: the PreToolUse hook set `run_in_background` on every `reeve heavy`. R1 set it only when the run would wait (someone in line, every slot taken, or its `--lock` key held), and the manual and skill stub stopped saying "in the background". Arm `reeve-r1`, the same as reeve otherwise; the code and arm are on branch `bench-r1-heavy-bg`.

| Arm | Done | Min to done | Interventions | Cost $ | Turns | Sessions |
|---|---|---|---|---|---|---|
| reeve (D1t) | 3/3 | 29.4 [27.0–40.3] | 3 [2–4] | 1.13 [0.90–3.82] | 55 [43–150] | 2 [2–13] |
| reeve-r1 | 1/3 | 23.1 (one run) | 3 [2–4] | 1.14 [0.74–1.17] | 48 [33–50] | 4 [3–4] |

- **Verdict: no gain, dropped.** R1 cut task notifications (13–17 per run against about 36 in t3, by a rough transcript count), but turns and cost didn't move, because the handoff churn above dominates both. Two of three R1 runs timed out without the rename, b0-2's miss. Whether R1 helps in a scenario with a working budget is open.
- **Not run:** the block-smoke regression check, since R1 was dropped.

### Cost against the weekly limit

| Time (UTC) | Weekly | Point |
|---|---|---|
| 16:04 | 27% | task start |
| 16:51 | 29% | reading during setup |
| 18:02 | 31% | after D1, before D1t |
| 19:06 | 33% | D1t running, R1 started at 18:39 |
| 19:29 | 33% | after R1 |
| 19:48 | 34% | after D1t's reruns; stop |

| Batch | Runs | $ (bench-reported, valid runs) | Verdict |
|---|---|---|---|
| D1 | 6 | 4.45 | b0 2/3, reeve 3/3 at 2.4x cost; handoffs broken |
| D1t | 5 (3 scored) | 5.85, plus t1 and t2 unpriced | handoffs work, then storm at 30K |
| R1 | 3 | 3.05 | dropped |

- **7 points (27→34%) for about $13 of scored runs** plus the pilot, smoke and lost runs, the simulated user and this orchestrating session. That is far worse than the November round's $7 per point: the lost and storming runs, many parallel sessions and other sessions sharing the plan (the owner's weekend coordinator and its workers) all count. The readings are whole percents.

### What's next

- **Recalibrate the budget** to 60–80K and rerun b0 and reeve at n=3 before testing any reeve change on this scenario.
- **reeve fixes the day showed:**
  - two handoffs seconds apart start two successors; a second handoff from the same session should be refused;
  - when `reeve handoff` can't start the successor, its message should say to carry on and tell the user, not leave the manual's "stop once it returns" in force;
  - a successor that starts past the threshold hands off at once; the hook could measure from the session's starting context.
