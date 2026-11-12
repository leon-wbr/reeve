# Queueing heavy commands

`reeve heavy` lets many agents on one machine share it. They wrap their heavy commands (builds, test suites, installs) in it, and it runs only a few at a time, first come first served, with an express slot so short runs don't wait behind long ones.

```sh
reeve heavy [--label L] [--slots N] [--express N] [--min-free GB] [--lock KEY] [--timeout DURATION|none] -- <command...>
reeve heavy ls
reeve heavy follow ID    # a job's output and exit code, also after it ended
reeve heavy stop PID     # stop a run, or take a waiter out of the queue
```

The command runs as soon as a slot is free, with the caller's environment, PATH and working dir unchanged (plus `REEVE_HEAVY=1`), so version managers such as proto resolve the same tools as without it. reeve heavy prints its output and exits with its code. While it waits it prints one line to stderr when its place or the reason changes (`waiting for a slot: 2 ahead`, `waiting for memory: …`) and is quiet otherwise. `reeve heavy ls` shows who runs and who waits, in order, with label, pid, age, working dir and why each waits, and then the jobs that ended while nobody followed them.

Agents start it in the background (Claude Code's `run_in_background`, a Codex background terminal) and keep working; their CLI tells them when it ends, with output and exit code. The skill, worker prompts and session-start line all say so, and in Claude Code the PreToolUse hook from `reeve setup claude` (`reeve hook background`) sets `run_in_background` on any Bash call that starts with `reeve heavy` (but `ls` and `stop`), so one that forgot doesn't block its session. Outside a terminal, the first waiting line also says it, so an agent that waited in the foreground learns it for its next call.

- **Slots:** a quarter of the cores, at least 1. `--slots` or `REEVE_HEAVY_SLOTS` overrides it; every caller should use the same number. Free memory doesn't add slots: on a 10-core, 16 GB Mac a third slot doubled each run's time through swapping while the memory floor still read 5 GB free ([bench/FINDINGS.md](../bench/FINDINGS.md#pressure-scenario)).
- **Express slot:** one more slot that only runs expected to be short may use (`--express N`, `REEVE_HEAVY_EXPRESS`, default 1; 0 turns it off). A run is expected to be short when each of its label's last 5 runs in the event log took at most a minute; a label with fewer than 3 runs is judged by the last 10 runs of labels ending in the same word (`gate` for `pr12-gate`), and one with no such history isn't short. Generic labels such as `e2e` or `install` are judged like any other, so one slow run among their last few keeps them out. A short run that has run for 2 minutes counts as long from then on. Waiters still go in the order they came: one passes another only while that one can't start, so a long run never loses a slot it could take and never starves. `ls` shows what each run is expected to take (`short: its last 5 runs took at most 9s`). `reeve-bench heavy-replay` replays the event log's heavy runs through this rule; on 7 days of one busy machine's log it put the total wait at 2 slots 44% lower with the express slot than without. In the pressure bench, with 1 GB short and long jobs, it halved the wait and swapped nothing ([bench/FINDINGS.md](../bench/FINDINGS.md#batch-s2-the-express-slot-under-memory-n3-resultspressure-express-n3tsv)).
- **Memory:** a run that would start while another is going also needs `--min-free` GB of memory free (`REEVE_HEAVY_MIN_FREE`, default 2; 0 turns it off). Free is what the OS could hand out without swapping: `kern.memorystatus_level` of `hw.memsize` on macOS (the figure behind Activity Monitor's pressure graph), `MemAvailable` on Linux. The first run always starts, so a machine short of memory still makes progress, one run at a time. The waiter whose turn it is waits, and `ls` says so (`waits for memory: 1.4 GB free, below the 2.0 GB floor`); where reeve can't read memory, only slots count.
- **State:** one JSON file per waiter or runner under `~/.local/state/reeve/heavy/` (or `$XDG_STATE_HOME/reeve/heavy`, or `REEVE_HEAVY_DIR`), and one dir per job under `jobs/`. There is no daemon. Each file stays locked while its owner lives; whoever reads an unlocked one next removes it, so a crashed runner never holds a slot, and a reused pid can't keep one. A short lock on the whole dir makes taking a slot atomic.
- **Signals:** SIGINT, SIGTERM and SIGHUP to the runner (or to reeve heavy in a terminal) are passed on to the command. If it hasn't ended 5 s later, or a second signal comes, it is killed.
- **Cleanup:** the command runs in its own process group, and when it ends the whole group is stopped (SIGTERM, then SIGKILL after 2 s), so no test workers are left behind. If the runner itself is killed, the slot stays taken until that group is gone. In a terminal's foreground job the command instead shares reeve heavy's group, so Ctrl-C, Ctrl-Z and prompts work as usual; leftovers aren't cleaned up then.
- **Locks:** `--lock KEY` also makes the run exclusive per key across every agent on the machine: it starts only when no other `reeve heavy` holds KEY, then takes a slot as usual. Two e2e runs sharing one database, both started with `--lock app-e2e-db`, run one after the other; runs with different keys, or none, are unaffected. While it waits it prints who holds the key (`waiting for lock KEY, held by LABEL (pid N, 2m)`) and `reeve heavy ls` shows the holder (`holds lock KEY`) and the waiters (`waits for lock KEY`). The lock is an OS file lock (`flock`) under `locks/` in the state dir, so there are no stale locks: it is released when the run ends, crashes or is killed. The command inherits it, so if reeve heavy is killed the key stays taken until the command's group is gone, like its slot. Waiters poll, so the order between them isn't guaranteed. A nested `reeve heavy` takes no lock.
- **Time limit:** a run that hangs holds its slot for everyone, so each has a limit, 30 minutes by default (longer than any run in the event history that exited 0, which took 22). It starts when the run gets its slot; waiting doesn't count. `--timeout 45m` (units `ms`, `s`, `m`, `h`) sets it for one run, `--timeout none` removes it (discouraged, and left out of the agent manual: give a large explicit value instead), and `REEVE_HEAVY_TIMEOUT` changes the default. At the limit the command's process group gets SIGTERM, and SIGKILL 5 s later; reeve heavy then frees the slot, prints one line saying the limit was hit and to narrow the run or pass `--timeout`, and exits 124. The event log records `timed_out` and `heavy_limit_ms`, and `reeve heavy ls` shows each running job's limit (the age column is its elapsed time). Start visual and e2e runs narrow, and raise the limit only for a known full run. A nested `reeve heavy` has no limit of its own; the outer one covers it.
- **Nesting:** the command gets `REEVE_HEAVY=1`. A `reeve heavy` that sees it runs its command straight away, so a wrapped script that wraps its own steps doesn't wait for itself.

## A job outlives its caller

An agent CLI may kill its background tasks: Claude Code's low-memory reaper did, on a 16 GB Mac with ~30 sessions, and every queued `reeve heavy` it killed lost its place and its run. So outside a terminal reeve heavy doesn't wait or run the command itself. It writes the job to `jobs/ID/` in the state dir and starts a runner for it, in a session of its own and reparented away from the caller (a double fork), so nothing that kills the caller's process group or walks its descendants reaches it. The runner waits for the lock and a slot, runs the command with its output in `jobs/ID/out.log` and `err.log`, and writes `result.json`. The caller follows: it copies the logs to its stdout and stderr as they grow and exits with the job's code, so for the agent nothing changes while it lives.

- **SIGINT** to the caller is passed on to the runner, which stops the job as before (out of the queue, or the command's group stopped).
- **SIGTERM, SIGHUP or SIGKILL** to the caller only end the following; those are what reaps a background task. The caller prints one line saying so, with the job's id, and exits 128+signal; the job keeps its place and runs.
- **How it ended** stays findable: when nobody followed a job to its end, `reeve heavy ls` lists it (`ended … exit N … unwatched: reeve heavy follow ID`) until someone follows it, the runner puts a line in the starting worker's inbox (looked up from the caller's environment, as `reeve inbox` does), and the event log marks it `heavy_unfollowed`. `reeve heavy follow ID` prints the output from the start and exits with the code, whether the job still runs or ended.
- **Stopping one** takes `reeve heavy stop PID` (the pid `ls` shows, the runner's), which sends SIGTERM to the runner: a waiter leaves the queue, a run is stopped as under Signals.
- Job dirs are removed a day after they end.

This is the agent CLIs' own background mechanism, kept: the caller is still the background task the CLI tells its agent about. Only the part that must not die with it moved out. In a terminal's foreground job none of this applies: reeve heavy runs the command itself, so Ctrl-C, Ctrl-Z and reads from the terminal work as usual.

## Prefer isolation to `--lock`

A lock serializes runs, so it costs the parallelism the queue exists to give. The better fix is to make the runs independent: e2e that gets its own database or port per run (a schema or database named after the run, port 0 or a free port) can run side by side. Use `--lock` only when the resource really is shared and can't be split, such as one external service, one fixed port, or a database that can't be duplicated.

## Later

Drawn from `orch heavy` and left out of this first version:

- swap, load and disk gates, and a gap between starts;
- priority waiters;
- stopping jobs that sit idle;
- a separate registry and cap for long-running servers;
- tool shims that queue `pnpm test`, `cargo build` and the like without being asked;
- concurrency hints for the command (`VITEST_MAX_WORKERS`, `RAYON_NUM_THREADS`, …);
- a config file;
- the legacy lock dirs and `orch brief` integration.
