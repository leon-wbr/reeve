# Bench findings

What each experiment tried, what it measured and why it won or lost. Newest batch last. Score tables per batch are in [results/](results/).

## Goal and bar

Find the first small, innovative add-on that beats a vanilla Claude Code session. Two baselines:

- **B0, vanilla:** one `claude -p` session (Sonnet) with no extra instructions. It reads the scenario repo's `CLAUDE.md`.
- **B1, comm:** B0 plus [skills/comm/SKILL.md](../skills/comm/SKILL.md) appended to the system prompt. It starts a background worker (`claude --bg -w`) per ask and relays results and decisions.

A candidate wins if it beats B1 and B0 on its target metric without being worse on the others: interventions, time to done, cost, false done and unchecked landings.

## Setup

- `reeve bench run <arm> <n>` makes the scenario repo a worktree of a trusted base repo. It starts the arm's session with `claude -p --input-format stream-json` and sends the asks as the first user turn.
- A simulated user (`claude -p --safe-mode`, Sonnet) knows only `scenario/key.md` and replies to what the session said. Cross-session messages from workers wake the session.
- **Interventions** are user messages after the first; a queued answer (C3) also counts. **Done** is a hidden check on the main branch ([check.rs](../src/bench/check.rs), [hidden.mjs](scenario/hidden.mjs)). **False done** means it claimed done but the check fails. **Unchecked landings** counts commits that reached main without a CI pass. **Cost** is token usage at Sonnet list prices, summed over all transcripts including subagents and workers.
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

### C2: `reeve watch`, an asyncRewake Stop hook that wakes the session when CI finishes, on B0. Dropped before running.

- **What:** about 70 lines of Rust; the patch is [candidates/c2-watch.patch](candidates/c2-watch.patch). It was verified that asyncRewake wakes a stream-json `claude -p` session about 5 s after the hook exits 2.
- **Hypothesis:** with 8-minute CI, vanilla ends its turn while CI is pending, so the user has to poll.
- **Why dropped:** a critic read the B0 transcripts. Vanilla never sits idle on pending CI. It uses foreground `until` loops, `run_in_background` loops, the Monitor tool and scheduled wakeups. A wake hook would tie, and the bench's 6-minute poll was the only way it could win.

### C3: `reeve ask`, a decision queue that doesn't block the agent, on B0. Batch 5.

- **What:** `reeve ask "<question, options, recommendation>"`, run with `run_in_background`, writes the question to `$REEVE_HOME/asks/` and waits for the answer. The agent keeps working, and the answer lands in the session as a background-task notification. `reeve asks` lists open questions; `reeve answer <id> <text>` answers one ([src/ask.rs](../src/ask.rs)). One appended system-prompt line tells the agent about it ([arms/c3/system.md](arms/c3/system.md)). In the bench, the simulated user reads the queue even while the session is busy, with the same 4-minute latency.
- **Hypothesis:** in batch 4, vanilla always finished the rename last. It wrote the question inside a long CI-waiting turn, and the user only saw it when the turn ended. A queue the user reads across sessions surfaces it at once, and the agent keeps working while it waits, so time to done should fall at 1 intervention and B0's cost.
- **Control, C3b:** B0 plus one line and no code: "ask as soon as you know, then end your turn".
- Results follow below.
