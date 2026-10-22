# Metrics

reeve exists only if it does better than plain Claude Code in a terminal. Every number below is collected automatically and compared against that baseline.

## The benchmark

A set of 12 real tasks, drawn fresh each time from recent work:

- 4 PR rebases, one of them with a conflict;
- 2 squash-merges where CI is red at first;
- 2 small fixes, one of which needs screenshots;
- 1 research question answered as a written page;
- 1 lookup across the ticket tracker and chat;
- 1 change to a public repository (branch pushed, no PR);
- 1 task that needs a decision from the user halfway through.

The tasks are run two ways, with the same opening message and the same answers from the user:

- **A, plain Claude Code:** one `claude` session in a terminal, free to use every built-in (background sessions, subagents, worktrees, agent view).
- **B, reeve:** plain Claude Code plus reeve's commands, hooks and rules.

Run A once to set the bar, then run both after every feature lands and after every Claude Code release.

**The bar.** Against A, B needs:

- at least 40% fewer user touches;
- no slower to verified done;
- zero false "done";
- zero questions about reeve's own trouble.

If B misses the bar, building stops until it clears it. A feature that doesn't move a number is removed.

## Definitions

| Metric | Definition | Collected from | Target |
| --- | --- | --- | --- |
| **User touches per item** (headline) | Prompts and answers the user gives about an item, from creation to verified done. Agents acting for the user don't count | `UserPromptSubmit` hook and reeve commands, each event tagged with its actor | ≤ 60% of A |
| **Zero-touch share** (headline) | Share of items verified done with no user input after the message that created them | Event log | rising; > 50% |
| **Request to merged PR** | Item created → the PR's `mergedAt` on GitHub, split into time waiting on the user and time waiting on the machine | Event log + GitHub | not slower than A |
| **Created to done** | Item created → its done-check holds (all kinds of item) | Event log | not slower than A |
| **False done** | Items marked done whose done-check fails | Sync with GitHub on every reeve run | 0 |
| **Stale items** | Open items whose PR is already merged or closed | Same | 0 after any reeve run |
| **Questions about reeve's own trouble** | Questions to the user about disk, failed starts, permissions, held branches or stuck sessions, tagged by kind when raised | Event log | 0; each one is a bug |
| **Questions per day** | Merge decisions (reeve) and worker questions (sessions needing input), counted separately | Event log, `claude agents --json` | ≤ 15 a day |
| **Answer time** | Question raised → answered | Event log | track only |
| **Permission stalls per item** | Permission prompts a session waited on | `Notification` hook | falling; each points at a rule to add |
| **Failed starts** | Background sessions with no first response within 2 minutes | Launcher, plus the first line in the transcript | < 2% |
| **Sessions per item** | Sessions started per item, and who restarted them | Event log | median 1, p90 2 |
| **Rule outcomes** | Per rule: fired, succeeded (confirmed on GitHub), failed, skipped, overridden (the user stepped in within 24 h) | Event log | overridden < 2% |
| **Trigger to done** | E.g. CI green → merged, base moved → rebased | Event log | median ≤ 10 min |
| **Sessions avoided** | Actions done by a fixed command instead of an agent session, and the tokens saved | Event log, transcripts | track only |
| **Throughput** | PRs merged and items done per working day | GitHub, event log | ≥ before reeve |
| **Heavy-command wait** | Queue wait per build or test command; every refusal for disk or load logged | The heavy-command queue's log | p90 ≤ 120 s |
| **Cost** | Tokens per item and per benchmark task | Session transcripts | ≤ 1.2× A |

## Starting point

The system reeve replaces was measured over its last four days (20–22 Oct 2026). Its records were thin, so these numbers are rough:

| | Before reeve |
| --- | --- |
| Failed starts | 23% (98 of 428) |
| Request to merged PR | median 2.1 h, p90 13.3 h (25 PRs) |
| Created to done | median 1.0 h, p90 11 h |
| Questions about its own trouble | 44% of 440 |
| Questions per day | 100–150 |
| False done | 1.3% of done items with a PR |
| Stale items | 42% of open items with a PR |
| Sessions per item | median 5, p90 9 |
| Throughput | about 15 PRs merged a day |
| Heavy-command wait | p90 75 s; refusals not logged |
| User touches, cost | not measurable: actors weren't told apart, cost wasn't recorded |

That last row is the main lesson: reeve records the actor, the time, the first response, the GitHub state and every refusal from day one.
