# Auto-reported problems and `reeve autofix`

reeve can file the problems it finds in itself as issues on its own repo, and an agent can fix the approved ones. Both are opt-in, and nothing private leaves the machine.

## Reporting (off by default)

Set `REEVE_AUTOREPORT=1`, or `"autoreport": true` in `~/.config/reeve/config.json`, and these file an issue labelled `auto-reported` through `gh`:

- a panic (only where in reeve's source it happened);
- `reeve heavy` failing to record its own bookkeeping;
- `reeve doctor` finding a worker gone without a report, or a worker registry it can't read.

`REEVE_AUTOREPORT=dry` prints what would be filed and files nothing.

What goes in an issue is deny-by-default. Its body is built from five fields: reeve's version, the OS family, the error kind, a fingerprint, and a location and backtrace limited to frames inside reeve's own source. The message is a fixed sentence written in reeve's code, never the text of an error, a log, a prompt, a path or an environment variable. Everything then goes through `reeve leakcheck`'s scanner, and a hit means nothing is posted: the text is kept in `autoreport-blocked/` next to the worker registry.

The same problem is one issue: each has a fingerprint, an open issue with it gets a comment (at most one a day) instead of a new issue, and at most 5 new issues a day are filed.

## `reeve leakcheck`

`reeve leakcheck [--diff BASE] [FILE...]` (or text on stdin) blocks on the home directory or username, an email, the hostname, a worker name or directory in the registry, a project name under `~/work`, an IP address, a token or key (known prefixes, or a long high-entropy string) and any URL other than this repo's own. It names the pattern and the line, never the matched text, and exits 1 on a hit. `--diff` checks the lines `BASE...HEAD` adds, and its commit messages. Run it on every PR body, comment and branch before anything goes on a public repo.

## `reeve autofix`

`reeve autofix` picks one open `auto-reported` issue with no linked PR and starts a worker named `autofix-N` for it, in its own worktree of the repo checkout reeve was built in (`--repo DIR` picks another). The worker reproduces the problem with a failing test, fixes it, runs the repo's gate once on the final head, and opens a PR with `Fixes #N` and the gate's summary line. If the gate is green and your `gh` account has write access it squash-merges the PR; if the gate fails it leaves the PR open and comments with what failed. An account without write access opens the PR and stops. It never cuts a release or touches publishing or install scripts' release flow.

It skips a run while `reeve usage --max 50 --window all` exits 1 (`autofix_usage_max` in the config changes the 50), and while another `autofix-*` worker is still working. An issue that already had an `autofix-N` worker is not started again; `reeve rm autofix-N` lets it be.

`reeve autofix --dry-run` prints the issue it would pick and the exact brief it would give the worker.

### Who approves

By default only an issue that a maintainer has approved is worked on: someone with write access to the repo (`autofix_approvers`, a list of logins in the config, replaces that) adds the label `autofix-approved`, which `autofix` creates when it is missing. reeve checks who added it.

`REEVE_AUTOFIX_UNATTENDED=1` (or `"autofix_unattended": true`) skips the label: an auto-reported issue is picked up when a maintainer filed it. Leave it off unless you are away and trust the reports.

### The gate

The worker runs the repo's own gate: `scripts/ci-local.sh` through `reeve heavy --timeout 45m` if the repo has one, else `gh pr checks <PR> --watch`. `autofix_gate` in the config sets another command.

### Scheduling

reeve has no scheduler; run `reeve autofix` every 30 minutes or so. Each run exits at once when nothing is to be done.

launchd (`~/Library/LaunchAgents/local.reeve.autofix.plist`, then `launchctl load` it):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>local.reeve.autofix</string>
  <key>ProgramArguments</key>
  <array><string>/bin/sh</string><string>-c</string><string>reeve autofix</string></array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
    <key>REEVE_ACTOR</key><string>schedule</string>
  </dict>
  <key>StartInterval</key><integer>1800</integer>
</dict>
</plist>
```

systemd (`~/.config/systemd/user/reeve-autofix.service` and `.timer`, then `systemctl --user enable --now reeve-autofix.timer`):

```ini
[Service]
Type=oneshot
ExecStart=%h/.local/bin/reeve autofix
Environment=REEVE_ACTOR=schedule

[Timer]
OnUnitActiveSec=30min
OnBootSec=5min

[Install]
WantedBy=timers.target
```

cron: `*/30 * * * * PATH=$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin REEVE_ACTOR=schedule reeve autofix`.
