# Benchmarking pipeline presets

Each [pipeline preset](../docs/pipelines.md) can run as an arm of `reeve-bench run`, on the same scenario, done-check and scoring as the session arms in [FINDINGS.md](FINDINGS.md). That way preset A, preset B and a plain session can be compared directly.

## Arms

An arm whose directory holds a file `pipeline` runs that preset instead of a communicator session:

| Arm | Runs |
|---|---|
| `p-plain` | `reeve run --pipeline plain`: one unattended agent. The control for the presets |
| `p-brief` | `brief` |
| `p-plan` | `plan` |
| `p-review` | `review` |
| `b0` | the plain interactive session the presets have to beat |

To benchmark a preset of your own, add it to `~/.config/reeve/pipelines.json` and make an arm for it: `mkdir bench/arms/p-mine && echo mine > bench/arms/p-mine/pipeline`.

## Running

```sh
cargo build --workspace                       # reeve-bench runs the reeve beside it
for n in 1 2 3; do
  for arm in b0 p-plain p-brief p-plan p-review; do
    reeve heavy --label bench -- target/debug/reeve-bench run $arm $n
  done
done
target/debug/reeve-bench score b0 p-plain p-brief p-plan p-review
```

Each run costs model time, up to the 40-minute cap. Run them through `reeve heavy`, at most two at a time.

## What a pipeline arm does

- The scenario's asks go as one prompt to `reeve run --name rv-ARM-N --pipeline PRESET` in the scenario repo.
- The run ends when the pipeline writes its report, when its process is gone, or at the 40-minute cap. A report whose status line is `done` counts as the claim. False done means it claimed done but the hidden check fails. Never claimed means the check passes but the report doesn't say `done`.
- **Interventions are 0 by construction.** Steps run unattended, so the simulated user never hears from them. The rename decision has to be the pipeline's own call or left open, and the scenario's timed follow-up is logged as undelivered (`undeliveredFollowups` in `result.json`). So compare presets with each other and with `p-plain` on done, false done, time and cost. Against `b0`, the question is whether an unattended pipeline gets as far as an interactive session without anyone in the loop.
- Cost sums the step sessions found in `steps.jsonl`, at Sonnet list prices like the other arms, even for Opus steps. Codex steps print no session id, so their tokens aren't counted yet.
