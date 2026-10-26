# Handoff scenario

Reproduces the 23 Oct observation in [../OBSERVATIONS.md](../OBSERVATIONS.md): long tasks are still running when a session has to hand off.

A session gets [asks.md](asks.md): four tasks that take 20 minutes to 3 hours ([tasks.json](tasks.json)). Once all four have started, the driver sends the message the context hook sends at 35%. The old session hands off by its arm's rules, and the successor has to bring all four results home in `FINAL.md`.

```sh
reeve bench handoff <arm> <n> [--real]   # one run, rh-<arm>-<n>; logs in ~/work/reeve-trials/logs
reeve bench handoff-score [arms]         # median [min–max] per arm
```

- **B0** ([arms/b0](arms/b0/system.md)): vanilla. Background subagents, and the current handoff routine: write `HANDOFF.md`, wait for background tasks or name them as outstanding, stop. The driver plays the routine's successor start.
- **R1** ([arms/r1](arms/r1/system.md)): named workers (`reeve run --name`) and `reeve handoff`, which starts the successor itself.

**Time.** Each task is `reeve bench task <id>`, which waits on a simulated clock and then prints a fixed result. When no transcript has grown for 20 s and a task is waiting, the driver jumps the clock to the next due task, so a 3-hour task costs seconds. `--real` turns the jumps off and runs tasks at a sixtieth of their length (3 hours become 3 minutes), to check that the stand-ins hide nothing. Real-time runs are left out of the score.

**Measures:** simulated minutes from "handoff due" to the successor's first turn, results found in `FINAL.md` (of 4), task starts beyond the first per task (work redone), and cost over every session in the run. Sessions run with the user's own Claude Code setup, as in the main scenario.
