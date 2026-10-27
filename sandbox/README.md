# Running agents

`reeve run` is how reeve starts an agent. It runs Claude Code in the foreground or as a background session, on the host or in a sandbox: a Docker container (OrbStack on macOS) where it runs with `--dangerously-skip-permissions` and can't touch the rest of the machine.

```sh
cargo install --path .
reeve run [--bg] [--sandbox [NAME]] [sandbox options] -- <claude args>
```

| Command | What it does |
|---|---|
| `reeve run -- <args>` | `claude <args>` on the host, in the foreground |
| `reeve run --bg -- <args>` | `claude --bg <args>` on the host: a Claude Code background session, listed by `claude agents` |
| `reeve run --sandbox -- <args>` | starts a sandbox, runs `claude --dangerously-skip-permissions <args>` in it, and removes the sandbox when claude exits |
| `reeve run --sandbox NAME -- <args>` | the same in sandbox NAME, which is started if it isn't running and kept afterwards |
| `reeve run --bg --sandbox NAME -- <args>` | a background session inside sandbox NAME; it keeps running after the command returns |

`--bg` with an unnamed `--sandbox` is refused, since nothing would remove the sandbox. Claude Code also refuses `--bg` with skipped permissions until its disclaimer has been accepted once in that sandbox's config; reeve doesn't pre-accept it (open question in the PR). Claude args pass through untouched, so a communicator starts a sandboxed worker exactly like a host one: `reeve run --bg --sandbox work -- -w fix-x -n fix-x "<prompt>"`. Sandbox options apply when a sandbox is started and are refused without `--sandbox`. Reusing a running sandbox from a different `--workspace` is refused, as is a container reeve didn't start; several `reeve run --sandbox NAME` at once share one sandbox, and none runs before its firewall is up.

Sandboxes are managed with `reeve sandbox`: `ls`, `exec --name NAME -- <cmd>` (for example `-- claude agents --json`; it runs in the sandbox's workspace), `down --name NAME`, and `build`.

## Named workers

A subagent belongs to the session that started it, so a session that hands off has to wait for it or orphan it. A named worker runs in its own session instead, and any later session can find it:

```sh
reeve run --name fix-x [--pane | --bg] -- [claude args] "<prompt>"
reeve ls                  # each worker: running, idle, done, gone or unknown
reeve wait fix-x          # blocks until its report exists
reeve report fix-x        # prints the report
```

The worker runs in a new Herdr tab labelled NAME, started without taking focus (the default inside Herdr, `HERDR_ENV=1`) or as a Claude Code background session (`--bg`, the default elsewhere). The last claude argument is the prompt. reeve adds an instruction to write the final report to `~/.local/state/reeve/reports/NAME.md`, and claude flags that let the worker write that file (`--add-dir` and an `Edit` allow rule for it) and name the session. These come after your own flags so none of them swallows the prompt. The prompt is passed when claude starts, never typed into a running session. Herdr can't pass newlines as launch arguments, so a multi-line prompt is saved next to the report as `NAME.prompt.md` and the worker is told to read it.

Workers are recorded in `~/.local/state/reeve/workers.json` (`$XDG_STATE_HOME` is honoured). A worker is done once its report exists. Otherwise it is running while Herdr or `claude agents` shows it working, idle while it waits for input, gone once its pane or session has ended, and unknown when that check fails. `reeve wait` fails if the worker is gone, or stays idle without a report, which usually means it is asking a question. A name can be reused once its old pane or session is closed. Named workers don't run in sandboxes yet.

## What a sandbox gets

- **Files:** the workspace (`--workspace`, default the current dir), mounted read-write at its host path, and nothing else from the host. The workspace should be a repo or worktree root: for a worktree, its common `.git` dir is mounted too so git works. The `hooks` and `config` of the git dir and its submodules are read-only, so the agent can't plant code that git on the host would run. `--ro` adds read-only mounts; `--state` keeps the container's Claude config dir (transcripts) on the host.
- **Network:** outbound traffic is dropped except to `api.anthropic.com` (enough for both API keys and OAuth tokens). Allowed hosts are resolved when the sandbox starts and pinned in `/etc/hosts`, and DNS is then blocked too, so nothing can tunnel out through the resolver. IPv6 is dropped. `--allow registry.npmjs.org` adds a host, though CDN-hosted ones share IPs with other sites; `--net open` turns the firewall off.
- **Auth:** only through a docker env file holding `CLAUDE_CODE_OAUTH_TOKEN=…` from `claude setup-token` or `ANTHROPIC_API_KEY=…`, kept at mode 600. It defaults to `~/.config/reeve/sandbox.env` (override with `--env-file` or `REEVE_SANDBOX_ENV_FILE`), and a sandbox won't start without one. Values never go into the image, the command line or logs. Anything inside the sandbox, including the token, can be sent to the allowed hosts, so only run trusted scenarios.

The image is `reeve-sandbox:claude-<version>-<hash of the build files>` and is built on first use from the Dockerfile and firewall script embedded in the binary. Auto-updates and nonessential traffic are disabled in it.

## Updating versions

Everything is pinned: the base image by tag and digest in [Dockerfile](Dockerfile) (Debian packages come with it), Claude Code in [claude-version](claude-version), Rust in `rust-toolchain.toml`, crates in `Cargo.lock`. To check against the latest:

```sh
curl -s https://nodejs.org/dist/index.json | jq -r '[.[] | select(.lts)][0].version'     # Node LTS
docker buildx imagetools inspect node:<version>-trixie-slim | grep -m1 Digest           # its digest
npm view @anthropic-ai/claude-code version                                                # Claude Code
curl -s https://static.rust-lang.org/dist/channel-rust-stable.toml | grep -m1 -A1 '^\[pkg.rust\]'  # Rust
cargo update --dry-run                                                                     # crates
```

Then edit the pins, run `cargo update`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, rebuild reeve and run `reeve sandbox build`. `--claude-version` tries another Claude Code version without changing the pin.
