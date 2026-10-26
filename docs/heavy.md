# Queueing heavy commands

`reeve heavy` lets many agents on one machine share it. They wrap their heavy commands (builds, test suites, installs) in it, and it runs only a few at a time, first come first served.

```sh
reeve heavy [--label L] [--slots N] -- <command...>
reeve heavy ls
```

The command runs as soon as a slot is free and reeve heavy exits with its code. While it waits it prints one line to stderr when its place changes (`waiting for a slot: 2 ahead`) and is quiet otherwise. `reeve heavy ls` shows who runs and who waits, in order, with label, pid, age and working dir.

- **Slots:** a quarter of the cores, at least 1. `--slots` or `REEVE_HEAVY_SLOTS` overrides it; every caller should use the same number.
- **State:** one JSON file per waiter or runner under `~/.local/state/reeve/heavy/` (or `$XDG_STATE_HOME/reeve/heavy`, or `REEVE_HEAVY_DIR`). There is no daemon. Each file stays locked while its owner lives; whoever reads an unlocked one next removes it, so a crashed agent never holds a slot, and a reused pid can't keep one. A short lock on the whole dir makes taking a slot atomic.
- **Signals:** SIGINT, SIGTERM and SIGHUP are passed on to the command. If it hasn't ended 5 s later, or a second signal comes, it is killed.
- **Cleanup:** the command runs in its own process group, and when it ends the whole group is stopped (SIGTERM, then SIGKILL after 2 s), so no test workers are left behind. If reeve heavy itself is killed, the slot stays taken until that group is gone. In a terminal's foreground job the command instead shares reeve heavy's group, so Ctrl-C, Ctrl-Z and prompts work as usual; leftovers aren't cleaned up then.
- **Nesting:** the command gets `REEVE_HEAVY=1`. A `reeve heavy` that sees it runs its command straight away, so a wrapped script that wraps its own steps doesn't wait for itself.

## Later

Drawn from `orch heavy` and left out of this first version:

- memory, swap, load and disk gates, and a gap between starts;
- priority waiters;
- stopping jobs that sit idle;
- a separate registry and cap for long-running servers;
- tool shims that queue `pnpm test`, `cargo build` and the like without being asked;
- concurrency hints for the command (`VITEST_MAX_WORKERS`, `RAYON_NUM_THREADS`, …);
- a config file;
- the legacy lock dirs and `orch brief` integration.
