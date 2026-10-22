# Metrics

How reeve is measured against the setups people already use.

> First draft. The tasks, the setups, the bar and the metrics are all expected to change as it becomes clearer what matters.

## The benchmark

### Tasks

The first draft has a fixed, versioned set of 12 real tasks:

- 4 PR rebases, one with a conflict;
- 2 merges where CI fails at first;
- 2 small fixes, one of which needs screenshots;
- 1 research question answered as a written page;
- 1 lookup across a ticket tracker and chat;
- 1 change to a public repository;
- 1 task that needs a decision halfway through.

The repositories are sandbox forks frozen at fixed commits, so a rebase or a failing check plays out the same way every time and nothing touches real projects. A few fresh tasks from recent work are added each round as a held-out check, so reeve isn't tuned to the fixed set. The set includes tasks where reeve shouldn't help at all. Anyone adopting reeve is expected to swap in tasks from their own work.

### Setups

- **Plain Claude Code**, set up by someone who knows it well. It gets the same definition of done in its project instructions, hooks where they help, and `/loop` or scheduled tasks for "merge when green". This is the main baseline.
- **Other setups**, such as Conductor, the Codex app and Claude Squad.
- **reeve**, on top of plain Claude Code.

All setups use the same model, effort and permission mode where they can. A setup that can't use the same model (Codex) is reported as a separate comparison.

### Running it

- **The person's answers come from a written key** of facts and decisions per task. Someone other than reeve's author gives them, with a fixed reply delay. A question the key doesn't cover gets "your call" and still counts.
- **An intervention is any action the person takes:** a message, an answer, a permission approval, a click to merge, overriding an automation. Time spent only looking is recorded separately, as attention time.
- **Answers an agent gives on the person's behalf** are counted in their own column and checked against the key. A wrong one counts against the setup.
- **Repetition:** each setup runs each task at least 3 times. Results are paired per task and reported as median and range.

### The bar

For each metric, reeve is compared with the best other setup on that metric. It has to show:

- at least 40% fewer interventions;
- time to done no more than 10% slower;
- no false "done";
- no questions about its own trouble.

## Benchmark metrics

The words below are neutral so every setup can be measured. An *item* is one benchmark task. A setup's *done* is its final claim, or the point it marks the work ready. The *done-check* is the project's own definition, applied by the scorer.

| Metric | Meaning | Target |
| --- | --- | --- |
| **Interventions per item** (headline) | Actions by the person until the done-check passes | the bar |
| **Hands-off share** | Items that pass the done-check with no intervention after the request | > 50% |
| **Time to done** | Request → done-check passes, split into waiting on the person and waiting on the machine | the bar |
| **False done** | Items claimed done that fail the done-check | 0 |
| **Questions about the setup's own trouble** | Questions about disk, failed starts, permissions or stuck sessions | 0 |
| **Failed starts** | Agent sessions that never get going | < 2% |
| **Cost** | Tokens, or the share of the plan's limit used, per task | ≤ 1.2× the best setup |

## Field metrics

These come from everyday use, not the benchmark. They're measured locally, and nothing is sent anywhere.

| Metric | Meaning | Target |
| --- | --- | --- |
| **Stale items** | Items still open although their work has landed | 0 |
| **Questions per day** | Decisions asked of the person, by kind | ≤ 20, all real decisions |
| **Sessions per item** | Sessions started per item, and who restarted them | median 1 |
| **Rule outcomes** | Per automation: fired, succeeded, failed, overridden | overridden < 2% |
| **Throughput** | Items done per working day | tracked, no target |

## Starting point

The author's previous setup was a custom orchestrator built on Claude Code (not plain Claude Code). It was measured over its last few days in October 2026. Its records were thin, so these numbers are rough:

| | Before reeve |
| --- | --- |
| Failed starts | 23% |
| Time to done, PRs only | median 2.1 h, p90 13.3 h |
| Questions about its own trouble | 44% of all questions |
| Questions per day | 100–150 |
| False done | ~1% of finished items |
| Stale items | 42% of open items with a PR |
| Sessions per item | median 5 |
| Interventions, cost | not measurable |

The last row is the lesson: record who acted, and when, from the start.
