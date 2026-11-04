#!/usr/bin/env python3
"""Copy a pressure batch's session transcripts into the repo, redacted, with a tool-call count.

    python3 -I bench/pressure/transcripts.py SINCE OUT RUN...

SINCE is an ISO time (UTC); transcripts last written before it are left out, so an older run of
the same name doesn't count. Each RUN (rp-ARM-N) gets OUT/RUN/*.jsonl, one per session file
(subagents' too), and OUT/tools.tsv gets a row per run and tool. Redaction: the home path becomes
~, emails, tokens and the owner's name are replaced, and any string over 1000 characters is cut.
"""

import json
import os
import re
import sys
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path

HOME = str(Path.home())
CUT = 1000
SUBS = [
    (re.compile(re.escape(HOME)), "~"),
    (re.compile(re.escape(HOME.replace("/", "-"))), "~"),
    (re.compile(r"[\w.+-]+@[\w-]+\.[\w.-]+"), "<email>"),
    (re.compile(r"\b(sk-ant-[\w-]+|gh[pousr]_\w+|github_pat_\w+|xox[abprs]-[\w-]+|AKIA[0-9A-Z]{16})"), "<token>"),
    (re.compile(r"leon[\s_-]?weber|leon-wbr|\bleon\b", re.I), "<owner>"),
]


def clean(v):
    if isinstance(v, str):
        for rx, to in SUBS:
            v = rx.sub(to, v)
        return v if len(v) <= CUT else v[:CUT] + f"…[cut {len(v) - CUT} chars]"
    if isinstance(v, list):
        return [clean(x) for x in v]
    if isinstance(v, dict):
        return {clean(k): clean(x) for k, x in v.items()}
    return v


def tools(lines):
    c = Counter()
    for l in lines:
        try:
            e = json.loads(l)
        except ValueError:
            continue
        msg = e.get("message") or {}
        content = msg.get("content") if isinstance(msg, dict) else None
        if e.get("type") == "assistant" and isinstance(content, list):
            for part in content:
                if isinstance(part, dict) and part.get("type") == "tool_use":
                    name = part.get("name", "?")
                    cmd = (part.get("input") or {}).get("command", "")
                    if name == "Bash" and isinstance(cmd, str):
                        if "reeve heavy" in cmd:
                            name = "Bash:reeve heavy"
                        elif "job.mjs" in cmd:
                            name = "Bash:job"
                        elif re.search(r"\breeve\b", cmd):
                            name = "Bash:reeve " + (cmd.split("reeve", 1)[1].split() or ["?"])[0]
                    c[name] += 1
    return c


def main():
    since = datetime.fromisoformat(sys.argv[1].replace("Z", "+00:00")).timestamp()
    out = Path(sys.argv[2])
    projects = Path.home() / ".claude/projects"
    rows = []
    for run in sys.argv[3:]:
        dest = out / run
        dest.mkdir(parents=True, exist_ok=True)
        total = Counter()
        sessions = 0
        for d in projects.iterdir():
            if not (d.name.endswith(run) or (run + "-") in d.name):
                continue
            for f in sorted(d.rglob("*.jsonl")):
                if f.stat().st_mtime < since:
                    continue
                lines = f.read_text(errors="replace").splitlines()
                total += tools(lines)
                sessions += "/subagents/" not in str(f)
                rel = f.relative_to(d)
                name = "__".join(rel.parts)
                with open(dest / name, "w") as w:
                    for l in lines:
                        try:
                            w.write(json.dumps(clean(json.loads(l)), ensure_ascii=False) + "\n")
                        except ValueError:
                            pass
        rows.append((run, sessions, total))
    with open(out / "tools.tsv", "a") as t:
        for run, sessions, total in rows:
            for name, n in sorted(total.items()):
                t.write(f"{run}\t{sessions}\t{name}\t{n}\n")


if __name__ == "__main__":
    main()
