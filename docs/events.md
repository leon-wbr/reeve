# Events, stats and feedback

reeve keeps a record of what it does, so the field metrics in [metrics.md](metrics.md) can be measured from the start. The record stays on your machine (principle 3); nothing is sent anywhere.

```sh
reeve stats [--days N] [--json]
reeve feedback "text" [--with-paths] [--open]
```

## The event log

Every reeve command appends one JSON line to `~/.local/state/reeve/events.jsonl` (or `$XDG_STATE_HOME/reeve/events.jsonl`, or the file named by `REEVE_EVENTS`; `REEVE_EVENTS=/dev/null` turns it off). Each line is one `write` to a file opened for appending, so concurrent commands never mix their lines. If the log can't be written, reeve warns and the command runs as usual.

| Field | Meaning |
| --- | --- |
| `ts` | When the command started, in ms since the epoch |
| `version` | reeve's version |
| `command` | The subcommand, e.g. `ls`, `heavy`, `sandbox down`; never its arguments |
| `worker` | The named worker, for `run --name`, `wait` and `report` |
| `label` | A heavy command's `--label`, else only its program's name; or a sandbox name |
| `exit` | reeve's exit code; missing for a nested `reeve heavy`, which replaces itself with its command |
| `error` | A fixed kind such as `no_worker`, `worker_gone`, `timeout`, `usage`, `herdr`; never the message |
| `ms` | How long the command took |
| `heavy_wait_ms`, `heavy_run_ms` | Time in the heavy queue, and the command's run time |
| `handoff_ms` | From `reeve run --name` until the new worker's agent is up |
| `actor`, `agent` | `agent` or `person`, from the variables agent CLIs set (`CLAUDECODE`, `CODEX_SANDBOX`, `AI_AGENT`); `agent` names a known one, or says `other` |

**The privacy rule:** the log never holds prompt text, file contents, report text, command arguments, error messages or environment values.

## `reeve stats`

Sums up the last 7 days (or `--days N`): commands; workers started, failed starts, and how many of the started ones are done, gone or still running now; heavy runs with wait and run time at p50 and p90; handoff latency; errors by kind; and the split between people and agents. `--json` prints the same as JSON.

## `reeve feedback`

For when reeve itself gets in the way. It works in three steps:

1. **It saves the note** to `~/.local/state/reeve/feedback/<time>.md`, with reeve's version, the OS and the last 20 events. Paths in the events are replaced with `<path>` unless you pass `--with-paths`.
2. **It offers to post it**, and never posts without a yes:
   - in a terminal with `gh` signed in, it shows the exact issue (repo, title, body) and asks before running `gh issue create`;
   - run by an agent or a script (no terminal), it prints the ready issue and the `gh` command, so the agent can offer to post it through `gh` or a GitHub MCP, and does so only with your OK.
3. **Otherwise you post it yourself:** it prints the prefilled new-issue URL for `leon-wbr/reeve`; `--open` opens it in the browser (`$BROWSER`, else `open` or `xdg-open`). A long note is cut to fit the URL and points to the saved file.

## Later

- Export to your own collector over OpenTelemetry, opt-in, as Claude Code does. The fields map to OpenTelemetry events as they are.
- Record which worker ran a command, so sessions per item and restarts can be counted.
