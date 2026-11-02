# Scheduled workers

reeve has no scheduler of its own. launchd (macOS) or cron (Linux) starts `reeve run` at a set time, and the worker is an ordinary named worker: it shows in `reeve ls`, writes its report to `~/.local/state/reeve/reports/NAME.md`, and the session-start line names it once it is done.

Claude Code's own `/loop`, `/schedule` and cron tools either live inside one open session or run in the cloud without your files, so they can't start a fresh local worker on a timer. launchd and cron can.

## A report-only worker

A worker nobody watches shouldn't push, comment, merge or post. It writes what it would do into its report as numbered proposals with the exact commands, and you say yes or no to each in any session.

For Claude Code, pass deny rules as one argument, `"--disallowedTools=RULE,RULE"`. Deny rules keep permission checks on, so `reeve run` accepts them (it refuses `--settings`, since a settings file can turn the checks off). Keep the `=`: written as two arguments, `--disallowedTools` takes every argument after it as a tool name, the prompt included. Keep the rules in `~/.config/reeve/report-only.txt`, one per line, and pass them as `"--disallowedTools=$(paste -sd, ~/.config/reeve/report-only.txt)"`:

```text
Bash(git push:*)
Bash(gh pr create:*)
Bash(gh pr merge:*)
Bash(gh pr comment:*)
Bash(gh pr review:*)
Bash(gh pr close:*)
Bash(gh issue comment:*)
Bash(gh release create:*)
```

Add the write tools of your MCP servers (Slack sends, Jira transitions), e.g. `mcp__slack__send_message`. `gh api` can write too; deny it unless the brief needs it to read.

Codex has no per-run deny list (its rules in `~/.codex/rules/` apply to every session). A Codex worker started outside Herdr runs `codex exec` in reeve's default `workspace-write` sandbox, where the network is off, so it can't push or post, but it can't read GitHub either. Use Codex for scheduled briefs that need only local files.

## launchd

`~/Library/LaunchAgents/local.reeve.intake.plist` starts an intake worker at 08:30 on weekdays. A run missed while the Mac sleeps starts when it wakes.

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>local.reeve.intake</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/sh</string><string>-c</string>
    <string>C="$HOME/.config/reeve"; reeve run --name intake-$(date +%F) --cwd "$HOME/work" -- "--disallowedTools=$(paste -sd, "$C/report-only.txt")" "$(cat "$C/briefs/intake.md")"</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string>/Users/YOU/.local/bin:/opt/homebrew/bin:/usr/bin:/bin</string>
    <key>REEVE_ACTOR</key><string>schedule</string>
  </dict>
  <key>StartCalendarInterval</key>
  <array>
    <dict><key>Weekday</key><integer>1</integer><key>Hour</key><integer>8</integer><key>Minute</key><integer>30</integer></dict>
    <!-- the same for Weekday 2 to 5 -->
  </array>
  <key>StandardErrorPath</key><string>/Users/YOU/.local/state/reeve/logs/intake.launchd.log</string>
</dict>
</plist>
```

Load it with `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/local.reeve.intake.plist`, try it now with `launchctl kickstart gui/$(id -u)/local.reeve.intake`, and stop it with `launchctl bootout gui/$(id -u)/local.reeve.intake`.

- **PATH** must find reeve, the agent CLI and whatever the brief runs (`gh`, `git`); launchd's default PATH has none of them.
- **`REEVE_ACTOR=schedule`** marks the start in the [event log](events.md) as `schedule`, not `person`, so scheduled starts don't count as your interventions in [metrics.md](metrics.md).
- **`--cwd`** must be a folder Claude Code trusts; run `claude` there once first.
- Don't add `--bare`: it skips the OAuth sign-in a subscription needs.

On Linux, the crontab line is `30 8 * * 1-5 PATH=… REEVE_ACTOR=schedule reeve run --name intake-$(date +\%F) …`, with the same arguments.

## Measuring it

A scheduled worker pays off if you act on its report instead of doing the work yourself. For each report, note which proposals you said yes to. Keep it if half or more are accepted and you skip your own manual pass on most days. `reeve stats` counts the scheduled starts on its actors line.
