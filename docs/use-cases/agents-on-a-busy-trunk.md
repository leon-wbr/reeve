# Agents on a busy trunk

A workday where one orchestrating Claude Code session hands out 20 to 30 tasks across five product repos: fixing flaky tests, reworking a PR after review, gating and merging PRs, re-running a benchmark, cleaning up branches. Most run as subagents of that session, and a few longer ones run as separate sessions in terminal tabs. Several run at once, and merges keep landing on the trunk the whole time.

The work is tracked in GitHub PRs and Jira. Done means a green local CI run and the project's e2e suite on the exact head that gets merged, with any flaky test fixed by its root cause, never retried until it passes.

## Where it falls short

Two subagents ran for over an hour each, and neither was stuck. The time went to things the orchestrator caused or couldn't see:

- **The trunk moved under them.** One PR rework was rebased four times because other PRs kept merging into files it touched. Each rebase meant resolving conflicts in the lockfile and spec files again, and another full e2e and CI run before it could push. The orchestrator merged those PRs itself, without knowing they overlapped.
- **Scope grew mid-run.** A flaky-test agent started with four tests. New failures found by other agents were sent to it three times, and each addition sent it back to reproduce, rebase and re-prove stability. Proving a flake fixed takes repeated full runs, minutes each.
- **Shared machine load.** Several agents ran full e2e suites and CI at once, each with its own database container. Every run got slower, and the load produced the very timeouts the flaky-test agent was fixing.
- **The "known red" list drifted.** Each gating agent was told which tests were already failing on the trunk. The list was out of date within an hour, so agents either held merges over failures that weren't theirs or had to compare against the trunk themselves.
- **Background waits looked like silence.** An agent waiting on its own background test runs reported nothing for long stretches, and the orchestrator could only tell it was alive, not what it was waiting on or how long was left.

## What would help

- Knowing which open tasks touch which files, so a merge that overlaps a running task waits, or the task is told before it starts its final run.
- A fixed scope per task: new findings start a new task instead of extending a running one.
- A limit on heavy runs (full e2e, CI) at once on one machine, queued rather than all started together.
- One current list of failures on the trunk that every task reads, instead of a list copied into each brief.
- Progress from a waiting task: what it is waiting on, and roughly how long.
