#!/usr/bin/env python3
"""The close scenario: does closing finished workers lose work?

A coordinator session hands eight small tasks to parallel workers, each in its own git
worktree. The tasks leave the states closing can lose: uncommitted changes, an unpushed
commit, a git-ignored file under .claude/, a long job still running, a message that arrives
after the worker reported, and a report the coordinator has to receive. Every piece of work
carries a token; after the coordinator says ALL DONE the driver waits 11 minutes, then looks
for every token. See README.md.

    run.py ARM N --bin DIR --trials DIR      one run, rc-ARM-N
    run.py --score [--trials DIR] [ARM...]   per-arm table over finished runs
"""

import argparse
import json
import os
import random
import re
import shutil
import signal
import statistics
import subprocess
import sys
import threading
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
BENCH = HERE.parent
CLAIM = "ALL DONE"
SUBCOMMANDS = "agents|attach|logs|stop|kill|rm|respawn|auth|mcp|plugin|plugins|doctor|update|upgrade|install|setup-token|import|purge|gateway|auto-mode|ultrareview"
PRICE = {"opus": [5.0, 6.25, 0.5, 25.0], "haiku": [1.0, 1.25, 0.1, 5.0], "sonnet": [3.0, 3.75, 0.3, 15.0]}

TASKS = [
    # name, task text ({T} is its token), what kind of work it leaves
    ("w-clean-1", "Create notes/one.md with the single line `{T}`. Commit it and push your branch to origin.", "pushed"),
    ("w-clean-2", "Create notes/two.md with the single line `{T}`. Commit it and push your branch to origin.", "pushed"),
    ("w-clean-3", "Create notes/three.md with the single line `{T}`. Commit it and push your branch to origin.", "pushed"),
    ("w-dirty", "Create notes/draft.md with the single line `{T}`. Do NOT commit it: the user reviews the draft before it is committed.", "uncommitted"),
    ("w-local", "Create notes/local.md with the single line `{T}` and commit it. Do NOT push: the user pushes it after review.", "unpushed"),
    ("w-scratch", "Write your working notes to .claude/notes/scratch.md (git-ignored on purpose), with the single line `{T}`. Nothing to commit.", "ignored"),
    ("w-job", "Start the long job `node scripts/job.mjs {T}` in your worktree, in the background. It takes about 4 minutes and writes out/job.txt. Don't wait for it: write your report as soon as it has started, saying it is running.", "job"),
    ("w-late", "Create notes/late.md with the single line `{T}`. Commit it and push your branch to origin.", "pushed"),
]
LATE = "One more thing for the worker on task {name}: append the line `{T}` to notes/late.md, commit and push it."

HINT = {
    "vanilla": "Use Claude Code's own way: background subagents, each with worktree isolation, for instance.",
    "reeve": "Use reeve workers (`reeve run --worktree`); run `reeve --skill` first.",
}

FIRST = """Here are eight small tasks for this repo. Hand each one to its own parallel worker, in its own git worktree, named as below. {hint}

Every worker must do exactly what its task says about committing and pushing, and, as its last step, run `node scripts/stamp.mjs NAME` (NAME being its own name) and put the printed stamp code in its report.

{tasks}

Follow the workers until each has reported. Then reply with one line per worker, its name and its stamp code, and end with a line saying only ALL DONE."""


def tok(name):
    return f"TOK-{name}-{random.getrandbits(32):08x}"


def esc(p):
    return re.sub(r"[^A-Za-z0-9]", "-", str(p))


def now():
    return time.time()


def sh(*args, cwd=None, check=True, env=None):
    r = subprocess.run(args, cwd=cwd, env=env, capture_output=True, text=True)
    if check and r.returncode != 0:
        raise RuntimeError(f"{args}: {r.stderr.strip()}")
    return r.stdout


def git(*args, cwd, check=True):
    return sh("git", "-c", "user.name=bench", "-c", "user.email=bench@localhost", *args, cwd=cwd, check=check)


def which(name, skip_dirs_with=None):
    for d in os.environ.get("PATH", "").split(":"):
        p = Path(d) / name
        if p.is_file() and os.access(p, os.X_OK):
            return p
    return None


class Front:
    """The coordinator: claude -p with stream-json in and out, kept open."""

    def __init__(self, cmd, env, cwd, log):
        self.log = open(log, "a")
        self.p = subprocess.Popen(cmd, env=env, cwd=cwd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=open(str(log).replace(".jsonl", ".stderr"), "a"), text=True)
        self.texts = []  # (t, text) of assistant messages
        self.results = 0
        self.busy = True
        self.last = now()
        self.session = None
        threading.Thread(target=self.read, daemon=True).start()

    def read(self):
        for line in self.p.stdout:
            self.log.write(line)
            self.log.flush()
            self.last = now()
            try:
                m = json.loads(line)
            except ValueError:
                continue
            if m.get("session_id") and not self.session:
                self.session = m["session_id"]
            if m.get("type") == "assistant":
                self.busy = True
                for c in m.get("message", {}).get("content", []):
                    if c.get("type") == "text":
                        self.texts.append((now(), c["text"]))
            elif m.get("type") == "result":
                self.results += 1
                self.busy = False

    def send(self, text):
        msg = {"type": "user", "message": {"role": "user", "content": [{"type": "text", "text": text}]}}
        self.p.stdin.write(json.dumps(msg) + "\n")
        self.p.stdin.flush()
        self.busy = True

    def claimed(self, after=0):
        for t, text in self.texts:
            if t < after:
                continue
            for l in text.splitlines():
                if l.strip().strip("*_`#.!> ") == CLAIM:
                    return t
        return None


def procs_in(roots):
    """pid -> (args, cwd) for processes whose cwd is under root."""
    out = sh("lsof", "-a", "-d", "cwd", "-Fpcn", check=False)
    args = {}
    for l in sh("ps", "-Ao", "pid=,args=", check=False).splitlines():
        a = l.strip().split(None, 1)
        if len(a) == 2 and a[0].isdigit():
            args[int(a[0])] = a[1]
    res, pid, cmd = {}, None, None
    for l in out.splitlines():
        if l.startswith("p"):
            pid = int(l[1:])
        elif l.startswith("c"):
            cmd = l[1:]
        elif l.startswith("n") and pid:
            cwd = l[1:]
            if any(cwd == str(r) or cwd.startswith(str(r) + "/") for r in roots):
                res[pid] = (args.get(pid, cmd), cwd)
    return res


def stop_sessions(roots, keep=lambda cwd: True, stop=True):
    """Stops (or with stop=False lists) the Claude Code background sessions working under roots."""
    try:
        d = json.loads(sh("claude", "agents", "--json", check=False) or "[]")
    except ValueError:
        return []
    out = []
    for a in d if isinstance(d, list) else d.get("agents", []):
        cwd = a.get("cwd") or ""
        if any(cwd == str(r) or cwd.startswith(str(r) + "/") for r in roots) and keep(cwd):
            sid = a.get("id") or a.get("sessionId")
            if sid:
                if stop:
                    sh("claude", "stop", sid, check=False)
                out.append(f"{sid} {a.get('name')} {a.get('status')}")
    return out


def is_session(args):
    """A Claude Code session process, not a job or a reeve helper it started."""
    first = args.split()[0] if args.split() else ""
    return (os.path.basename(first) == "claude" or "/claude/versions/" in first) and " autoclose " not in args


def descendants(pid):
    out = sh("ps", "-Ao", "pid=,ppid=", check=False)
    kids = {}
    for l in out.splitlines():
        a = l.split()
        if len(a) == 2:
            kids.setdefault(int(a[1]), []).append(int(a[0]))
    seen, stack = set(), [pid]
    while stack:
        p = stack.pop()
        for k in kids.get(p, []):
            if k not in seen:
                seen.add(k)
                stack.append(k)
    return seen


def transcripts(home, roots, since):
    base = home / ".claude/projects"
    pres = [esc(r) for r in roots]
    out = []
    for d in base.iterdir() if base.exists() else []:
        if any(d.name == pre or d.name.startswith(pre + "-") for pre in pres):
            for p in d.rglob("*.jsonl"):
                if p.stat().st_mtime >= since:
                    out.append(p)
    return sorted(out)


def usage(paths):
    """Tokens and cost over transcripts, each message id once."""
    seen, tot = set(), {"input": 0, "cacheWrite": 0, "cacheRead": 0, "output": 0, "costUsd": 0.0}
    for p in paths:
        for line in p.read_text(errors="replace").splitlines():
            try:
                m = json.loads(line)
            except ValueError:
                continue
            msg = m.get("message") or {}
            u = msg.get("usage")
            if m.get("type") != "assistant" or not u:
                continue
            mid = msg.get("id") or m.get("uuid")
            if mid in seen:
                continue
            seen.add(mid)
            model = msg.get("model", "")
            pr = next((v for k, v in PRICE.items() if k in model), PRICE["sonnet"])
            n = [u.get("input_tokens", 0), u.get("cache_creation_input_tokens", 0),
                 u.get("cache_read_input_tokens", 0), u.get("output_tokens", 0)]
            for k, v in zip(["input", "cacheWrite", "cacheRead", "output"], n):
                tot[k] += v
            tot["costUsd"] += sum(a * b for a, b in zip(n, pr)) / 1e6
    tot["costUsd"] = round(tot["costUsd"], 4)
    tot["tokens"] = tot["input"] + tot["cacheWrite"] + tot["cacheRead"] + tot["output"]
    return tot


CLOSE_RE = re.compile(r"(^|[;&|(]\s*)(reeve close|git worktree remove|git worktree prune|claude (stop|kill|rm))\b")


def closes(paths):
    """Tool calls in the coordinator's transcripts that close or tidy up a worker."""
    n, seen = [], set()
    for p in paths:
        for line in p.read_text(errors="replace").splitlines():
            try:
                m = json.loads(line)
            except ValueError:
                continue
            if m.get("type") != "assistant":
                continue
            for c in (m.get("message") or {}).get("content", []):
                if c.get("type") != "tool_use" or c.get("id") in seen:
                    continue
                seen.add(c.get("id"))
                name, inp = c.get("name"), c.get("input") or {}
                if name in ("TaskStop", "KillShell", "ExitWorktree"):
                    n.append(name)
                elif name == "Bash":
                    for seg in re.split(r"&&|\|\||;|\n", inp.get("command", "")):
                        if CLOSE_RE.search(seg.strip()):
                            n.append(seg.strip()[:80])
    return n


FILES = {"w-clean-1": "one.md", "w-clean-2": "two.md", "w-clean-3": "three.md", "w-dirty": "draft.md",
         "w-local": "local.md", "w-scratch": "scratch.md", "w-job": "job.txt", "w-late": "late.md",
         "w-late-more": "late.md"}


def roots_of(run_dir, wt_base, before):
    """The run's repo plus the worktrees made since the run started under the trials repo's
    .claude/worktrees, where `reeve run --worktree` and `claude -w` put them."""
    new = [wt_base / d for d in (os.listdir(wt_base) if wt_base.exists() else []) if d not in before]
    return [run_dir] + sorted(new)


def find_tokens(roots, origin, want, only=None):
    """Which of want (key -> token) survive: in a file of the expected name under the run's repo
    (worktrees included), or in that file in a commit reachable from a ref of the repo or origin."""
    found = set()
    names = {}
    for k, t in want.items():
        names.setdefault(FILES[k], []).append(k)
    walks = [w for r in roots for w in os.walk(r)] if only is None else []
    for root, dirs, files in walks:
        dirs[:] = [d for d in dirs if d not in (".git", "node_modules")]
        for f in files:
            for k in names.get(f, []):
                if k in found:
                    continue
                try:
                    if want[k] in (Path(root) / f).read_text(errors="replace"):
                        found.add(k)
                except OSError:
                    pass
    for repo in (roots[0], origin) if only is None else (only,):
        revs = git("rev-list", "--all", cwd=repo, check=False).split()
        for k, t in want.items():
            if k in found or not revs:
                continue
            out = git("grep", "-l", "-F", t, *revs, "--", f":(glob)**/{FILES[k]}", cwd=repo, check=False)
            if out.strip():
                found.add(k)
    return found


def run(arm_name, n, bin_dir, trials):
    arm = json.loads((HERE / "arms" / f"{arm_name}.json").read_text())
    trials = Path(trials).resolve()
    run = f"rc-{arm_name}-{n}"
    run_dir = trials / run
    logs = trials / "close" / run
    if (logs / "result.json").exists():
        print(f"{run} has a result; skipping")
        return
    shutil.rmtree(logs, ignore_errors=True)
    for d in ("state", "reeve-home", "bin"):
        (logs / d).mkdir(parents=True, exist_ok=True)
    git("worktree", "remove", "--force", str(run_dir), cwd=trials, check=False)
    git("branch", "-D", run, cwd=trials, check=False)
    shutil.rmtree(run_dir, ignore_errors=True)
    git("worktree", "prune", cwd=trials, check=False)
    git("worktree", "add", "-q", "-b", run, str(run_dir), "main", cwd=trials)

    tag = arm.get("tag", arm_name[0]) + str(n)
    ledger = logs / "ledger.txt"
    ledger.touch()
    shutil.copytree(HERE / "repo", run_dir, dirs_exist_ok=True)
    setup = BENCH / arm["setup"]
    if (setup / "repo").exists():
        shutil.copytree(setup / "repo", run_dir, dirs_exist_ok=True)
    for f in ("scripts/stamp.mjs", "scripts/job.mjs"):
        p = run_dir / f
        p.write_text(p.read_text().replace("{{LEDGER}}", str(ledger)).replace("{{JOB_SECONDS}}", str(arm.get("jobSeconds", 240))))
    git("add", "-A", cwd=run_dir)
    git("commit", "-qm", "scenario", cwd=run_dir)
    # Every run is a worktree of the trials repo, so they share its config and one origin. Worker
    # names carry the run's tag, so their branches don't meet there.
    origin = trials / "origin.git"
    if not origin.exists():
        git("init", "-q", "--bare", str(origin), cwd=trials)
    # A rerun starts without the last attempt's worker sessions, worktrees and branches.
    stop_sessions([trials / ".claude/worktrees"], lambda c: c.rstrip("/").endswith(f"-{tag}"))
    for wt in (trials / ".claude/worktrees").glob(f"*-{tag}"):
        git("worktree", "remove", "--force", str(wt), cwd=trials, check=False)
        shutil.rmtree(wt, ignore_errors=True)
    git("worktree", "prune", cwd=trials, check=False)
    for repo in (origin, trials):
        for ref in git("for-each-ref", "--format=%(refname)", "refs/heads/", "refs/remotes/", cwd=repo, check=False).split():
            if ref.endswith(f"-{tag}") or f"-{tag}/" in ref:
                git("update-ref", "-d", ref, cwd=repo, check=False)
    git("remote", "remove", "origin", cwd=run_dir, check=False)
    git("remote", "add", "origin", str(origin), cwd=run_dir)
    git("push", "-q", "-f", "origin", f"HEAD:refs/heads/{run}", cwd=run_dir)
    git("fetch", "-q", "origin", cwd=run_dir)
    git("branch", "-q", "--set-upstream-to", f"origin/{run}", cwd=run_dir, check=False)

    home = Path.home()
    bin_dir = Path(bin_dir).resolve()
    wrap = logs / "bin"
    real_claude = Path(os.environ.get("REEVE_BENCH_CLAUDE") or which("claude"))
    # PATH without any reeve of the owner's; the reeve arms get the branch's build first.
    path = [d for d in os.environ["PATH"].split(":") if d and not (Path(d) / "reeve").exists() and Path(d) != wrap]
    path = [str(wrap)] + ([str(bin_dir)] if arm["setup"].endswith("reeve") else []) + path
    run_env = {
        "HOME": str(home),
        "PATH": ":".join(path),
        "XDG_STATE_HOME": str(logs / "state"),
        "REEVE_HOME": str(logs / "reeve-home"),
        "ORCH_HOME": str(trials / ".orch-sink"),
        "REEVE_HANDOFF_TOKENS": "350000",
    }
    run_env.update(arm.get("env", {}))
    settings = json.loads((setup / "settings.json").read_text())
    settings.setdefault("env", {}).update(run_env)
    (logs / "settings.json").write_text(json.dumps(settings, indent=2) + "\n")
    flags = ["--model", arm.get("workerModel", "haiku"), "--setting-sources", "project,local",
             "--settings", str(logs / "settings.json")]
    q = lambda s: "'" + s.replace("'", "'\\''") + "'"
    (wrap / "claude").write_text(
        f'#!/bin/sh\n# bench close: the arm\'s flags for every session of the run\ncase "$1" in\n  {SUBCOMMANDS}) exec {q(str(real_claude))} "$@" ;;\nesac\nexec {q(str(real_claude))} {" ".join(q(f) for f in flags)} "$@"\n')
    (wrap / "claude").chmod(0o755)

    env = {k: v for k, v in os.environ.items()
           if not any(k.startswith(p) for p in ("HERDR_", "ORCH_", "CLAUDECODE", "CLAUDE_CODE_", "REEVE_", "XDG_STATE_HOME"))}
    env.update(run_env)

    shown = {name: f"{name}-{tag}" for name, _, _ in TASKS}
    tokens = {name: tok(name) for name, _, _ in TASKS}
    late_tok = tok("w-late-more")
    tasks = "\n".join(f"- **{shown[name]}**: {text.format(T=tokens[name])}" for name, text, _ in TASKS)
    first = FIRST.format(hint=HINT[arm["kind"]], tasks=tasks)
    (logs / "first.md").write_text(first)

    wt_base = trials / ".claude/worktrees"
    before = set(os.listdir(wt_base)) if wt_base.exists() else set()
    roots = lambda: roots_of(run_dir, wt_base, before)
    t0 = now()
    since = t0 - 5
    log = lambda ev, **kw: open(logs / "driver.jsonl", "a").write(json.dumps({"t": round(now() - t0, 1), "ev": ev, **kw}) + "\n")
    log("start", arm=arm_name, run=run, params=arm, tokens=tokens, late=late_tok, names=shown)
    cmd = [str(wrap / "claude"), "-p", "--input-format", "stream-json", "--output-format", "stream-json",
           "--verbose", "-n", f"{run}-front", "--model", arm.get("model", "sonnet")]
    front = Front(cmd, env, run_dir, logs / "front.jsonl")
    front.send(first)
    result = {"arm": arm_name, "run": run, "params": arm, "claimedAt": None, "lateAt": None,
              "interventions": [], "timeout": False}
    late_sent, late_t, claimed_at, end_at = False, 0, None, None
    cap, observe = arm.get("capS", 25 * 60), arm.get("observeS", 11 * 60)
    samples = open(logs / "samples.tsv", "w")
    samples.write("t_s\tworker_procs\tclaude_procs\n")
    last_sample, last_scan = 0, 0
    want = {name: tokens[name] for name, _, _ in TASKS}
    want["w-late-more"] = late_tok
    ever = {}
    while True:
        time.sleep(2)
        t = now() - t0
        if front.p.poll() is not None and end_at is None:
            log("front-exited", code=front.p.returncode)
            end_at = now()
        if not late_sent:
            if "w-late" in find_tokens([run_dir], origin, {"w-late": tokens["w-late"]}, only=origin):
                front.send(LATE.format(name=shown["w-late"], T=late_tok))
                late_sent, late_t = True, now()
                result["lateAt"] = round(t, 1)
                result["interventions"].append({"t": round(t, 1), "text": "late message"})
                log("late")
        if t - last_scan >= 10:
            last_scan = t
            for k in find_tokens(roots(), origin, {k: v for k, v in want.items() if k not in ever}):
                ever[k] = round(t, 1)
                log("made", item=k)
        if t - last_sample >= 15:
            last_sample = t
            ps = procs_in(roots())
            mine = descendants(front.p.pid) | {front.p.pid}
            workers = {p for p, (c, _) in ps.items() if p not in mine}
            cl = {p for p in workers if is_session(ps[p][0])}
            samples.write(f"{round(t)}\t{len(workers)}\t{len(cl)}\n")
            samples.flush()
        if claimed_at is None:
            c = front.claimed(late_t) if late_sent else None
            if c is not None:
                claimed_at = c
                result["claimedAt"] = round(c - t0, 1)
                end_at = now() + observe
                log("claimed")
            elif not front.busy and now() - front.last > 90 and front.texts and front.texts[-1][1].rstrip().endswith("?"):
                front.send("Your call; carry on without me.")
                result["interventions"].append({"t": round(t, 1), "text": "your call"})
                log("your-call")
        if claimed_at is None and t > cap and end_at is None:
            result["timeout"] = True
            end_at = now()
            log("timeout")
        if end_at is not None and now() >= end_at:
            break

    # What is open now, 11 minutes after the claim: worker sessions that nobody closed.
    ps = procs_in(roots())
    mine = descendants(front.p.pid) | {front.p.pid}
    open_procs = {p: v for p, v in ps.items() if p not in mine}
    sessions = sorted({v[1] for v in open_procs.values() if is_session(v[0])})
    result["openAtEnd"] = {"procs": [f"{v[0]} {v[1]}" for v in open_procs.values()], "claudeCwds": sessions}
    bg = stop_sessions(roots()[1:], stop=False)
    result["bgSessionsAtEnd"] = bg
    # A tab is a worker session still alive: a process, or a background session the daemon lists.
    result["tabsOpen10m"] = max(len(sessions), len(bg))
    # Lost work: every token that should survive.
    found = find_tokens(roots(), origin, want)
    result["worktrees"] = [str(r) for r in roots()[1:]]
    for k in found:
        ever.setdefault(k, round(now() - t0, 1))
    led = ledger.read_text()
    stamps = dict(re.findall(r"^stamp (\S+) (STAMP-[0-9a-f]+)", led, re.M))
    front_paths = [p for p in transcripts(home, roots(), since)
                   if p.parent.name == esc(run_dir) and front.session and p.stem == front.session]
    # What reached the coordinator: its transcript, and its stream as the driver logged it.
    front_text = "".join(p.read_text(errors="replace") for p in front_paths + [logs / "front.jsonl"])
    # Lost: work that existed and is gone. Not done: work no one made, which is the task's
    # failure, not the close's.
    lost, not_done = [], []
    for k in want:
        if k == "w-job":
            continue
        if k not in ever:
            not_done.append(f"work:{k}")
        elif k not in found:
            lost.append(f"work:{k}")
    jt = tokens["w-job"]
    if f"job-start {jt}" not in led:
        not_done.append("job:never-started")
    elif f"job-finish {jt}" not in led:
        lost.append("job:killed")
    elif "w-job" not in found:
        lost.append("job:output-gone")
    for name, _, _ in TASKS:
        code = stamps.get(shown[name])
        if not code:
            not_done.append(f"report:{name}:no-stamp")
        elif code not in front_text:
            lost.append(f"report:{name}:not-received")
    result["notDone"] = not_done
    result["notDoneCount"] = len(not_done)
    result["madeAt"] = ever
    result["lost"] = lost
    result["lostCount"] = len(lost)
    result["items"] = len(want) + len(TASKS)
    result["closes"] = closes(front_paths)
    result["closesCount"] = len(result["closes"])
    result["coordinator"] = usage(front_paths)
    allp = transcripts(home, roots(), since)
    result["total"] = usage(allp)
    result["sessions"] = len([p for p in allp if "subagents" not in p.parts])
    result["subagents"] = len([p for p in allp if "subagents" in p.parts])
    events = (logs / "state/reeve/events.jsonl")
    ev = events.read_text() if events.exists() else ""
    result["reeve"] = {k: ev.count(f'"{k}"') for k in ("run", "close", "autoclose")}
    result["minutes"] = round((now() - t0) / 60, 1)
    # Stop everything the run left.
    front.p.terminate()
    for p in open_procs:
        try:
            os.kill(p, signal.SIGTERM)
        except OSError:
            pass
    sh("pkill", "-f", str(logs / "reeve-home"), check=False)
    result["stoppedAtEnd"] = stop_sessions(roots()[1:])
    (logs / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({k: result[k] for k in ("run", "lostCount", "lost", "notDone", "closesCount", "tabsOpen10m", "claimedAt", "timeout")}))


def med(xs):
    xs = [x for x in xs if x is not None]
    if not xs:
        return "–"
    m = statistics.median(xs)
    f = (lambda v: f"{v:.2f}") if any(isinstance(x, float) and x != int(x) for x in xs) else (lambda v: f"{v:g}")
    return f"{f(m)} [{f(min(xs))}–{f(max(xs))}]"


def score(trials, arms):
    base = Path(trials) / "close"
    rows = {}
    for d in sorted(base.glob("rc-*")):
        r = d / "result.json"
        if not r.exists():
            continue
        j = json.loads(r.read_text())
        if arms and j["arm"] not in arms:
            continue
        rows.setdefault(j["arm"], []).append(j)
    print("arm\truns\tclaimed\tlost work\truns losing work\tnot done\tcloses by coordinator\ttabs open 10 min after done\tcoordinator tokens K\tcoordinator $\ttotal $")
    for arm, rs in rows.items():
        print("\t".join([
            arm, str(len(rs)), f"{sum(1 for r in rs if r['claimedAt'] is not None)}/{len(rs)}",
            med([r["lostCount"] for r in rs]), f"{sum(1 for r in rs if r['lostCount'])}/{len(rs)}",
            med([r["notDoneCount"] for r in rs]),
            med([r["closesCount"] for r in rs]), med([r["tabsOpen10m"] for r in rs]),
            med([round(r["coordinator"]["tokens"] / 1000) for r in rs]),
            med([r["coordinator"]["costUsd"] for r in rs]), med([r["total"]["costUsd"] for r in rs]),
        ]))
    print()
    print("run\tlost\tnot_done\tcloses\ttabs\tclaimed_min\tcoord_tok_K\ttotal_usd")
    for arm, rs in rows.items():
        for r in rs:
            print(f"{r['run']}\t{','.join(r['lost']) or '-'}\t{','.join(r['notDone']) or '-'}\t{r['closesCount']}\t{r['tabsOpen10m']}\t"
                  f"{'' if r['claimedAt'] is None else round(r['claimedAt'] / 60, 1)}\t{round(r['coordinator']['tokens'] / 1000)}\t{r['total']['costUsd']}")


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("arm", nargs="?")
    ap.add_argument("n", nargs="?")
    ap.add_argument("--bin")
    ap.add_argument("--trials", default=os.environ.get("REEVE_BENCH_DIR"))
    ap.add_argument("--score", nargs="*")
    a = ap.parse_args()
    if a.score is not None:
        score(a.trials, a.score + ([a.arm] if a.arm else []) + ([a.n] if a.n else []))
    else:
        run(a.arm, a.n, a.bin, a.trials)
