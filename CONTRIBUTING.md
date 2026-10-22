# Contributing

reeve is built slowly, in tiny steps. Each step is one small PR, reviewed before it merges. A step ships only when its acceptance test passes and no metric in [docs/metrics.md](docs/metrics.md) got worse.

## How we work

- **Build only what is missing.** Before adding anything, check whether Claude Code, Codex, Herdr, GitHub or `gh` already does it. If one does, use it and write down why it fits.
- **Name the metric.** Every feature PR says which number it moves and shows the before and after.
- **Small and plain.** Short commands, no daemons, no UI of our own. Delete code the moment a native feature replaces it.
- **Tests first** for any logic: write a failing test, then make it pass.
- **Conventional commits** (`feat:`, `fix:`, `docs:`, `chore:`), fewer and larger rather than many tiny ones.
- **Local only.** Nothing in reeve talks to anything but the local machine, `gh` and the agent CLIs.

## Build order

1. **Docs** (this PR): README, philosophy, metrics, this file.
2. **Baseline.** The benchmark task set, a script that scores the done-checks, and one run with plain Claude Code. *Ships when:* the plain Claude Code numbers are recorded.
3. **Event log and `reeve metrics`.** One SQLite file. Claude Code hooks (`SessionStart`, `UserPromptSubmit`, `Notification`, `Stop`, `SessionEnd`) append events, tagged with the actor. *Ships when:* on a fixture day, `metrics` prints every row, and the user's own replies are told apart from an agent's.
4. **`reeve add`.** It splits a message into requests and keeps each one's words. It starts `claude --bg --name item-<id> -w <slug>` with the worktree trusted first, and writes a brief that names a done-check a script can run. *Ships when:* a 10-request message gives 10 sessions, none fails to start, each replies within 2 minutes, and every brief has a runnable check.
5. **Sync with GitHub.** One batched `gh` query per repository, run at the start of every reeve command and from the `Stop` hook. *Ships when:* on a copy of real data, one run leaves 0 stale and 0 false done, in under a minute.
6. **Fixed commands:** `merge`, `rebase`, `close`, `drop`. A merge needs the user's recorded yes at the current head commit, then uses `gh pr merge --squash --auto` and confirms on GitHub. *Ships when:* 10 merges all show as merged, and one attempted after the head moved is refused.
7. **`reeve` with no arguments** prints what waits on you. The `Stop` hook raises a Herdr notification when something new starts waiting. *Ships when:* a new question shows a notification within 30 seconds and appears in the list with options and a recommendation.
8. **`reeve open`.** It puts an item's live session in a labelled Herdr tab. *Ships when:* one command gives the tab, and Herdr shows the session's state. (If Herdr can't read a background session's state, this step is dropped.)
9. **Rules.** `reeve rule "<sentence>"` has the model write the sentence down once as a fixed trigger and action; the sentence is kept with it. A short `reeve tick`, started by launchd, checks the triggers and exits. New rules start in shadow mode (log only); there is a pause switch, and each rule fires once per PR head. *Ships when:* three rules (merge when green, rebase when the base moves, a weekday-morning summary) run for 3 days with no wrong fire.
10. **Benchmark both ways.** *Ships when:* it clears the bar in the metrics doc. If it doesn't, stop and rethink before building more.
11. **Port open items** from the previous system: only those whose PRs are still open, plus non-PR items still open after one sync. Everything else is archived, not ported.
12. **Gates.** Deny rules and a `PreToolUse` hook on `gh` writes and pushes to protected branches, then a red-team run that tries every way around them.

Anything not on this list waits until the benchmark says it is worth it.
