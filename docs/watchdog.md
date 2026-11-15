# Watchdog

A worker nobody watches can stop at its plan's usage limit, or finish a turn and wait for a person who is away. `reeve watchdog` looks at each opted-in worker without a report every 15 minutes and, in this order:

1. tells it once to stop new work and write its report when its agent's weekly usage is over a cap you set;
2. tells it to carry on, re-check git and PR state, and never redo a merge, when its pane shows a usage-limit stop;
3. tells it to carry on when it has been idle at two checks in a row without a `reeve heavy` job of its own.

A worker it told to wrap up is left alone after that. Done, gone and blocked workers are never nudged: a blocked one waits on a question that `reeve watch` passes on. A pane worker gets the text typed into its prompt (Codex needs a second Enter); a background worker gets it in its inbox.

## Opting in

It is off until reeve's config file, `~/.config/reeve/config.json` (under `XDG_CONFIG_HOME` when set), names workers or turns it on for all of them:

```json
{
  "watchdog": {
    "workers": ["nightly-fix"],
    "all": false,
    "caps": ["codex=50", "claude=80"],
    "every_min": 15
  }
}
```

A worker a watched one started, such as its successor after a handoff, is watched too. `caps` take what `reeve usage --max` takes, and only the weekly window counts. The file is read again before each pass, so you can add a worker without restarting anything. Each nudge is appended to `watchdog.log` in reeve's state dir (`~/.local/state/reeve`), and what the next pass needs to remember is kept in `watchdog.json` beside it.

## Running it

Run one of these, not two:

- **A loop.** `reeve watchdog` runs a pass every `every_min` minutes until it is stopped; start it in a terminal tab, under `tmux`, or with `nohup reeve watchdog >/dev/null 2>&1 &`.
- **launchd (macOS).** `reeve watchdog --once` every 900 seconds from `~/Library/LaunchAgents/local.reeve.watchdog.plist`, with `StartInterval` 900 and a `PATH` that finds reeve, `herdr` and the agent CLIs, as in [schedule.md](schedule.md#launchd).
- **systemd (Linux).** A user service running `reeve watchdog --once` with a timer set to `OnUnitActiveSec=15min`, or a user service running `reeve watchdog` with `Restart=on-failure`.
- **cron.** `*/15 * * * * PATH=… reeve watchdog --once`.

With `--once` the pass count comes from `watchdog.json`, so "idle at two checks" still means two scheduled runs. It has to run as the user whose Herdr session the workers live in.
