# Plan: a throughput bench and field metrics

Written 23 Oct 2026, to run from 23 Oct. Why it exists: the six-ask bench in [FINDINGS.md](FINDINGS.md) floors at 1 intervention and is too noisy at n=2–3 to show small effects. reeve is meant for many tasks across many repos, and that bench doesn't exercise that.

## What the field data says (last 7 days, rough heuristics)

Mined locally from `~/.claude/projects/*/*.jsonl`. Bench sessions are excluded. Nothing is sent anywhere.

| Per weekday, 16–22 Oct | Range |
|---|---|
| Typed user messages (not peer or system) | 107–320 |
| Auto-mode classifier denials | 16–70 (272 in the week) |
| PRs created / merged | 17–72 / 13–91 |
| Worker sessions started | 33–73 |

Over the week: 104 assistant questions got a direct answer, with a wait of p50 54 s and p90 330 s. That counts only questions the user saw while sitting at that session. The whole set is 1309 sessions: 125 conversational sessions, averaging 10.9 typed messages each, and 403 worker sessions.

What it suggests:

- **Classifier denials are a daily cost** (about 40 a day). Each one is a stall and often a question about the tool's own trouble. In the bench they were sporadic; in the field they are not. This revives C1's allow-rule half (pre-authorising landing commands behind a mechanical gate) as worth measuring at scale.
- **Volume is about 40 PRs a day.** A useful bench has to run tens of tasks, not six.

## Phase 1: field metrics, cheap, first

1. Port the mining script to `reeve-bench field [--days N]` (Rust, local, read-only).
   - Per day: typed messages, peer messages, polls, trouble messages, questions and their wait, AskUserQuestion uses, denials by kind (classifier, permission, sandbox), PRs created and merged, worker starts, sessions per task.
   - Exclude bench directories.
   - Print aggregates only, never message text.
2. Tighten the heuristics against a hand-labelled sample of 50 messages: is it a poll, an answer, a go-ahead, trouble, or a new ask? Report precision.
3. Add two measures:
   - **Unseen time:** a question written mid-turn, and how long until the user's next message in that session.
   - **Denial cost:** the time from a denial to the next successful tool call, and whether a user message was needed to clear it.

Output: `bench/results/field-<date>.txt`, aggregates only, because the repo is public.

## Phase 2: backlog bench, an endless backlog with a fixed time budget

### Fixtures

- Three or four small fixture repos (JS, no dependencies, plus fake CI). Each has enough surface (10–20 modules) that parallel work conflicts sometimes, not always.

### Task generator

`reeve-bench backlog --seed S --count 300` writes `backlog.jsonl`. Each task comes from about 15 parameterised templates, and every task carries its own hidden check:

| Template | Example | Hidden check |
|---|---|---|
| bug fix | an edge case in function F | behaviour assertion |
| small feature | add G to module M, with tests | behaviour + tests mention G |
| rename | key K, needs the user's decision | new name from the key |
| red CI on main | a planted failure | suite green |
| flaky test | a random-dependent test | 10 green runs |
| docs drift | export not documented | docs check |
| conflicting pair | two tasks editing the same function | both behaviours hold |
| mid-work decision | fix reveals a second caller wanting another format | the key's choice holds |
| dependent task | H uses G from another task | H works after both land |
| no-op / already done | the ask is already true | nothing changed, reported as such |

Mix: about 70% autonomous, about 20% with a decision found mid-work, and about 10% with a decision up front. The field data from phase 1 sets the final mix.

### Run

`reeve-bench backlog-run <arm> --minutes 30 --seed S`.

- The arm gets the backlog as a file plus one message: "work through backlog.jsonl in order, as much as you can".
- **The simulated user has an attention budget,** for example 5 minutes per 30. It reads only when something needs it: an idle session, a queued question, or a message. Each read costs its own reading time plus a fixed 30 s. Each reply comes after a latency drawn from the field wait distribution. When the budget runs out, questions wait.
- At the end: stop everything, run every task's hidden check on main, and score.

### Score

- **Headline:** tasks done per 30 minutes, where done means the hidden check passes on main.
- **Also:** cost per done task, interventions per done task, attention minutes used, false done (claimed but failing), broken main (time main was red), denials, and failed starts.
- Report median and range over n runs, each with a different seed.

### Arms

- **B0:** vanilla, one session.
- **B0-par:** vanilla told it may use subagents in worktrees. This is native parallelism, the fair vanilla ceiling.
- **B1:** the comm skill.
- **Candidates:**
  - **C3:** `reeve ask`, the decision queue.
  - **C1a:** narrow landing allow rules plus the gate.
  - **C4:** a merge queue (`reeve train`, batches green branches into one CI run). It only becomes interesting at volume.

### Cost control

- Calibrate first: 10 minutes, a backlog of 30, B0 and B1, n=1, about $10.
- Then 30-minute runs at n=2 per arm. Expect $20–60 per parallel arm.
- Run at most 2 at a time, through `heavy --server`.
- Stop any arm whose cost per done task is above 3x B0's after calibration.

## Phase 3: decide

- **A candidate wins if:** at least 20% more done tasks per 30 minutes than the best baseline, interventions per done task no higher, cost per done task within 1.2x, and no extra false done or broken main.
- **If nothing wins at volume:** reeve's v0 is the comm skill plus whatever field metrics show, and the bench result is the finding.

## Order for tomorrow

1. Phase 1, steps 1–2 (Rust port of the miner, labelled sample), about 1.5 h, no model cost.
2. Phase 2: fixtures and generator with 6 templates first, plus the hidden checks, about 3 h.
3. The backlog driver: attention-budgeted simulated user and throughput score, reusing `reeve-bench run`'s driver, about 2 h.
4. Calibration run, about 20 min, about $10.
5. C3 against B0 against B0-par, 30 minutes, n=2, if usage allows.
