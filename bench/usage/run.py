#!/usr/bin/env python3
"""How much a coordinator spends finding out its remaining plan budget, and how right it is.

Each trial reads the truth from the CLIs' own displays (Claude Code's /usage, Codex's /status,
driven in tmux), then asks a fresh `claude -p` session in each arm the same question:

- vanilla: Claude Code in --safe-mode, without reeve (or any of ~/.local/bin) on PATH;
- reeve: the same, with reeve on PATH and its manual (`reeve --skill`) appended to the prompt.

It prints one TSV row per run. Usage: run.py REEVE_BIN TRIALS [MODEL] > results.tsv
"""

import json
import os
import re
import subprocess
import sys
import time

PROMPT = (
    "Before starting more workers, find out how much of the 5-hour and the weekly plan limits "
    "Claude Code and Codex have each used on this machine. Don't start any agent sessions. "
    "Reply with one JSON object and nothing else: "
    '{"claude": {"five_hour": P, "weekly": P}, "codex": {"five_hour": P, "weekly": P}}, '
    "where P is the percent used as a number, or null when you can't find it out "
    "or the plan shows no such window."
)
TOLERANCE = 2  # percentage points between the answer and the CLI's own display


def tmux_screen(name, cmd, keys, cwd):
    subprocess.run(["tmux", "kill-session", "-t", name], capture_output=True)
    subprocess.run(["tmux", "new-session", "-d", "-s", name, "-x", "160", "-y", "60", "-c", cwd, cmd], check=True)
    time.sleep(6)
    for k in keys:
        subprocess.run(["tmux", "send-keys", "-t", name, *k], check=True)
        time.sleep(3)
    time.sleep(4)
    out = subprocess.run(["tmux", "capture-pane", "-p", "-t", name, "-S", "-80"], capture_output=True, text=True).stdout
    subprocess.run(["tmux", "kill-session", "-t", name], capture_output=True)
    return out


def truth(cwd):
    c = tmux_screen("bench-claude-usage", "claude --safe-mode", [["/usage", "Enter"]], cwd)

    def used_after(heading):
        m = re.search(re.escape(heading) + r"[^\n]*\n[^\n]*?(\d+)% used", c)
        return float(m.group(1)) if m else None

    x = tmux_screen("bench-codex-status", "codex", [["Escape"], ["/status", "Enter"], ["Enter"]], cwd)

    def left(label):
        m = re.search(label + r":[^\n]*?(\d+)% left", x)
        return 100.0 - float(m.group(1)) if m else None

    if "Current week (all models)" not in c or "Weekly limit" not in x:
        sys.exit(f"could not read the CLIs' own displays:\n{c}\n---\n{x}")
    return {
        "claude": {"five_hour": used_after("Current session"), "weekly": used_after("Current week (all models)")},
        "codex": {"five_hour": left("5h limit"), "weekly": left("Weekly limit")},
    }


def answer(text):
    m = re.search(r"\{.*\}", text or "", re.S)
    try:
        return json.loads(m.group(0)) if m else None
    except json.JSONDecodeError:
        return None


def score(ans, want):
    right = 0
    for agent in ("claude", "codex"):
        for w in ("five_hour", "weekly"):
            got = ((ans or {}).get(agent) or {}).get(w)
            exp = want[agent][w]
            if exp is None:
                right += got is None
            elif isinstance(got, (int, float)):
                right += abs(got - exp) <= TOLERANCE
    return right


def run_arm(arm, reeve_bin, model, cwd):
    env = {k: v for k, v in os.environ.items() if not k.startswith(("CLAUDE", "REEVE_", "HERDR_", "CODEX_THREAD"))}
    home = os.path.expanduser("~")
    path = [p for p in env["PATH"].split(":") if p not in (f"{home}/.local/bin", f"{home}/.cargo/bin")]
    args = ["claude", "-p", PROMPT, "--safe-mode", "--model", model, "--output-format", "json",
            "--permission-mode", "auto", "--permission-prompts", "none", "--no-session-persistence"]
    if arm == "reeve":
        path.insert(0, os.path.dirname(reeve_bin))
        # Leon's statusline already writes Claude's rate_limits in the shape `reeve usage record` keeps.
        env["REEVE_CLAUDE_USAGE"] = f"{home}/.claude/orchestration/usage-now.json"
        manual = subprocess.run([reeve_bin, "--skill"], capture_output=True, text=True, check=True).stdout
        args += ["--append-system-prompt", manual]
    env["PATH"] = ":".join(path)
    t0 = time.time()
    try:
        p = subprocess.run(args, capture_output=True, text=True, env=env, cwd=cwd, timeout=600)
        out = json.loads(p.stdout)
    except (subprocess.TimeoutExpired, json.JSONDecodeError):
        out = {"result": None, "num_turns": None, "usage": {}}
    u = out.get("usage") or {}
    tokens = sum(u.get(k) or 0 for k in ("input_tokens", "output_tokens", "cache_read_input_tokens", "cache_creation_input_tokens"))
    return {
        "secs": round(time.time() - t0, 1),
        "turns": out.get("num_turns"),
        "tokens": tokens,
        "output_tokens": u.get("output_tokens"),
        "cost_usd": out.get("total_cost_usd"),
        "result": out.get("result"),
    }


def main():
    reeve_bin, trials = os.path.abspath(sys.argv[1]), int(sys.argv[2])
    model = sys.argv[3] if len(sys.argv) > 3 else "sonnet"
    cwd = os.path.dirname(os.path.abspath(__file__))
    cols = ["arm", "trial", "secs", "turns", "tokens", "output_tokens", "cost_usd", "right_of_4", "truth", "answer"]
    print("\t".join(cols), flush=True)
    for t in range(1, trials + 1):
        want = truth(cwd)
        # Alternate the order so neither arm always runs on the fresher numbers.
        for arm in (("vanilla", "reeve") if t % 2 else ("reeve", "vanilla")):
            r = run_arm(arm, reeve_bin, model, cwd)
            ans = answer(r["result"])
            row = [arm, t, r["secs"], r["turns"], r["tokens"], r["output_tokens"], r["cost_usd"],
                   score(ans, want), json.dumps(want, separators=(",", ":")), json.dumps(ans, separators=(",", ":"))]
            print("\t".join(str(c) for c in row), flush=True)


if __name__ == "__main__":
    main()
