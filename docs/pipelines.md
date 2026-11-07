# Pipelines

`reeve run --name NAME --pipeline PRESET -- "PROMPT"` runs a named worker as a short list of agent steps instead of one session. Each step is an agent (Claude Code or Codex) prompted in a fixed way. It gets the user's prompt plus the previous step's output, and the last step writes the report as any worker does. Only plumbing runs between steps: reeve passes the text along and records each step. It never judges, filters or rewrites anything itself.

```sh
reeve pipelines                                   # the presets and their steps
reeve run --name fix-mean --pipeline plan -- "mean() of [] should throw a clear error"
reeve run --name fix-mean --agent codex --pipeline review -- "…"
```

## Presets

Built in ([src/pipelines.json](../src/pipelines.json)):

| Preset | Steps |
|---|---|
| `plain` | work. One agent, as the control the others are measured against |
| `brief` | brief (read-only) → work |
| `plan` | plan (read-only, Opus) → implement |
| `review` | work → review (Opus): reviews the changes, fixes what's wrong and writes the report |

Add your own, or replace one with the same name, in `~/.config/reeve/pipelines.json`, in the same shape:

```json
{
  "tdd": {
    "about": "tests first, then the code",
    "steps": [
      { "name": "tests", "permission": "auto", "model": { "claude": "sonnet" },
        "prompt": "Write failing tests for the task below. Don't implement it." },
      { "name": "code", "agent": "codex", "permission": "auto",
        "prompt": "Make the failing tests from the previous step pass." }
    ]
  }
}
```

- `agent`: `claude` or `codex`. If it's unset, the step uses the worker's `--agent`.
- `model`: per agent. A Codex step ignores the `claude` model, and the other way round. If unset, each agent uses its own default.
- `permission`: `read-only` (Claude's plan mode, Codex's read-only sandbox) or `auto` (Claude's auto mode, Codex's workspace-write sandbox).
- `prompt`: the step's instructions. reeve appends a line saying the step runs unattended, then the user's prompt in `<ask>` and the previous step's output in `<previous step="NAME">`.

An unknown preset or invalid file stops `reeve run` before anything starts. A pipeline worker takes only its prompt after `--`. The steps' flags come from the preset.

## How it runs

- Each step is one headless call: `claude -p --output-format json` or `codex exec --output-last-message`. The calls run one after another in the worker's directory, under a detached `reeve pipeline-run`. `reeve ls` shows the worker as `pipe` until its report exists.
- Step outputs, logs and `steps.jsonl` (step, agent, exit code, time, session id) go to `~/.local/state/reeve/logs/NAME.pipeline/`.
- A step that fails or prints nothing ends the run with a `failed` report that names the step. If the last step didn't write the report, its output becomes the report, marked `partial` unless it already starts with a status. The plumbing never writes `done`.
- Steps run unattended. Nobody answers them, and `reeve send` doesn't reach them. A step leaves a decision that only you can make undone and names it, so it shows up in the report. Use a plain worker for work that needs you along the way.

## Measuring a preset

Every preset can run as a reeve-bench arm on the scenario, beside the plain session: [bench/pipelines.md](../bench/pipelines.md).
