# The pressure scenario

`reeve-bench pressure <ARM> <N>` runs parallel workers on a machine short of memory, as run `rp-<ARM>-<N>`. It exists because of 1 Nov: with about 30 Claude sessions on a 16 GB Mac, Claude Code's low-memory reaper killed the background shells that ran `reeve heavy`, queued or running, across about ten workers. See [FINDINGS.md](../FINDINGS.md#pressure-scenario) for results.

## What happens

- **Workers.** The driver starts `workers` sessions with `reeve run`. Each is asked to run its `jobs` jobs, one after another, through `reeve heavy`, and not to rerun a failed one.
- **Jobs.** `node scripts/job.mjs --id ID` ([repo/scripts/job.mjs](repo/scripts/job.mjs)) holds `jobMb` of RAM for `jobSeconds`. It logs its start and finish to a ledger.
- **A reaper.** Once a second the driver samples the run's processes: the processes whose working directory is in the run, and their descendants. While the jobs' memory is more than `budgetMb`, it kills the background task of the newest `reeve heavy` caller, that is the shell the caller runs in and everything under it. It kills at most once every 5 s. A job that runs as the caller's child dies with it; one that runs elsewhere doesn't.
- **Fixed work instead.** With `jobPasses` set, a job hashes its whole memory that many times instead of waiting `jobSeconds`, so it takes longer once the machine pages its memory out. With `budgetMb` at 0 there is no reaper and the real machine is the limit.
- **Scripted workers.** `REEVE_BENCH_CLAUDE=bench/pressure/scripted-claude` stands in for `claude`: each worker runs its jobs through `reeve heavy` with no model, so a run measures the machine and the queue only, costs nothing and needs no trusted trials dir.
- **A budget, not the real machine.** `budgetMb` is the bench's own limit, so the real machine stays clear of trouble: the baseline arm peaks at about 1.8 GB of jobs.

The run ends when every job has finished, or when no job has started or finished for three minutes, or after 30 minutes.

## Arms

`arms/<ARM>/arm.json` holds every parameter of a run:

- **Sessions:** `model`, `effort`, `settingSources`, `setup` (as in the [day scenario](../day/README.md#arms)).
- **Load:** `workers`, `jobs`, `jobMb`, `jobSeconds`, `jobPasses`, `budgetMb`. `shortJobs` short jobs (ids like `w1-s1`, `shortPasses` passes each) take turns with each worker's `jobs`; scripted workers label them `job-short` and the rest `job-long`.
- **`history`:** label → ms. Five heavy runs per label of that length are written to the run's event log before it starts, so `reeve heavy` knows which labels are short. The run's own counts leave them out.
- **`reeve`:** a reeve binary relative to the repo root, a branch's build for instance. By default it's the one beside `reeve-bench`.
- **`env`:** added to every session, background ones included, through the run's settings. Use it for reeve's knobs, such as `REEVE_HEAVY_SLOTS`.

The baseline arm, `reeve`, runs 6 workers × 2 jobs of 600 MB for 60 s each, with 3 heavy slots, on a 1500 MB budget. Two jobs fit and a third at once doesn't.

## Measures

`reeve-bench pressure-score [ARM...]` prints, per arm, median [min–max] of:

- jobs, completed and lost (never finished);
- reaper kills;
- peak memory of the run's processes, as `top` counts it (physical footprint, compressed pages included; RSS leaves those out and reads a fraction on a Mac that swaps), and sessions alive at that peak;
- minutes until the last job finished, when all finished;
- each job's time, start to done, median and longest;
- the whole machine's swap-ins and swap-outs during the run, in pages (`vm_stat`), and its lowest free memory (`kern.memorystatus_level`);
- cost.

Each run's `samples.tsv` holds every sample: time in ms, memory of the run's processes and of its jobs (both in KB), sessions, jobs, processes and the machine's free memory in percent.

## Running it

Use a trusted trials dir and a detached driver, as for the [day scenario](../day/README.md#running-it). `cargo test -p reeve-bench` runs the driver against a fake `claude` and `reeve`.
