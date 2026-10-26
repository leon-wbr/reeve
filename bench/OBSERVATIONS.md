# Field observations: where reeve might beat vanilla

Moments from real use where a vanilla session got in the way. Each one is a candidate scenario for the backlog bench ([PLAN.md](PLAN.md)). Newest last.

## 2026-10-23: a running subagent blocks a handoff

- **What happened:** a main session reached 45% context and wanted to hand off to a successor, but one subagent was still running. A subagent belongs to the session that started it: its result only comes back to that session. Handing off would orphan it, so the handoff had to wait.
- **Vanilla workarounds:** wait for the subagent; `/compact` instead of handing off, which keeps the same session so the subagent survives but loses detail; or hand off anyway and redo the subagent's work.
- **Where reeve could win:** work running in its own session (a pane or `claude --bg` worker) is addressed by name, not owned by a session. A successor reads its report file and inherits it, so a handoff never waits on outstanding work.
- **Bench measure:** time from "handoff wanted" to "successor working", and work lost or redone across a handoff. Scenario: force a handoff at a fixed context size while long tasks are in flight.
