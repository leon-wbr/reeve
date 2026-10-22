# Sandbox

`reeve sandbox` runs Claude Code with `--dangerously-skip-permissions` in a Docker container (OrbStack on macOS), so benchmarks and other unattended runs can't touch the rest of the machine.

```sh
cargo install --path .
reeve sandbox run --workspace path/to/repo -- -p "fix the failing test"
```

`run` starts a container, runs `claude --dangerously-skip-permissions <args>` in the workspace as a non-root user, and removes the container afterwards. For longer jobs, such as a session that starts background agents, use `up`, `exec` (`--claude` prefixes the command with claude; it runs in the sandbox's workspace) and `down` with a `--name`. `reeve sandbox --help` lists all options.

What the container gets:

- **Files:** the workspace, mounted read-write at its host path, and nothing else from the host. The workspace should be a repo or worktree root: for a worktree, its common `.git` dir is mounted too so git works. The `hooks` and `config` of the git dir and its submodules are read-only, so the agent can't plant code that git on the host would run. `--ro` adds read-only mounts; `--state` keeps the container's Claude config dir (transcripts) on the host.
- **Network:** outbound traffic is dropped except to `api.anthropic.com` (enough for both API keys and OAuth tokens). Allowed hosts are resolved when the container starts and pinned in `/etc/hosts`, and DNS is then blocked too, so nothing can tunnel out through the resolver. IPv6 is dropped. `--allow registry.npmjs.org` adds a host, though CDN-hosted ones share IPs with other sites; `--net open` turns the firewall off.
- **Auth:** only through a docker env file holding `CLAUDE_CODE_OAUTH_TOKEN=…` from `claude setup-token` or `ANTHROPIC_API_KEY=…`, kept at mode 600. It defaults to `~/.config/reeve/sandbox.env` (override with `--env-file` or `REEVE_SANDBOX_ENV_FILE`), and `up` and `run` refuse to start without one. Values never go into the image, the command line or logs. Anything inside the container, including the token, can be sent to the allowed hosts, so only run trusted scenarios.

The image is `reeve-sandbox:claude-<version>-<hash of the build files>` and is built on first use from the Dockerfile and firewall script embedded in the binary. The Claude Code version is pinned in [claude-version](claude-version); bump it there and rebuild reeve, or try another one with `--claude-version`. Auto-updates and nonessential traffic are disabled in the image.
