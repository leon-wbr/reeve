#!/usr/bin/env python3
"""Redact a session transcript before it goes into this (public) repository.

Replaces the home directory with ~, emails with <email>, API keys and tokens with <secret>, the
user's names with <user>, and cuts any string longer than LIMIT characters. JSONL lines are walked
value by value, so the result stays valid JSONL.

The names come from this machine, never from this file: the login name, `git config user.name`
(whole and word by word) and any extra comma-separated terms in REDACT_TERMS.

Usage: redact.py < transcript.jsonl > redacted.jsonl
"""

import getpass
import json
import os
import re
import subprocess
import sys

LIMIT = 4000

SECRETS = [
    r"sk-[A-Za-z0-9_-]{16,}",
    r"gh[pousr]_[A-Za-z0-9]{20,}",
    r"github_pat_\w{20,}",
    r"cf[a-z]{2,4}_[A-Za-z0-9_-]{16,}",
    r"xox[abprs]-[A-Za-z0-9-]{10,}",
    r"AKIA[0-9A-Z]{16}",
    r"eyJ[\w-]+\.[\w-]+\.[\w-]+",
]
EMAIL = r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}"


def machine_terms():
    terms = {getpass.getuser()}
    try:
        name = subprocess.run(["git", "config", "user.name"], capture_output=True, text=True).stdout.strip()
    except OSError:
        name = ""
    if name:
        terms.add(name)
        terms.update(name.split())
    terms.update(t.strip() for t in os.environ.get("REDACT_TERMS", "").split(","))
    return sorted((t for t in terms if len(t) >= 3), key=len, reverse=True)


class Redactor:
    def __init__(self, home=None, terms=None, limit=LIMIT):
        home = (home or os.path.expanduser("~")).rstrip("/")
        self.limit = limit
        self.subs = [(re.compile(r"(Bearer\s+)[A-Za-z0-9._~+/=-]+"), r"\1<secret>")]
        self.subs += [(re.compile(p), "<secret>") for p in SECRETS]
        self.subs.append((re.compile(EMAIL), "<email>"))
        # The home dir, and the same path the way Claude Code names its project folders.
        for h in (home, home.replace("/", "-")):
            self.subs.append((re.compile(re.escape(h) + r"(?![A-Za-z0-9])"), "~"))
        terms = machine_terms() if terms is None else sorted(terms, key=len, reverse=True)
        if terms:
            words = "|".join(re.escape(t) for t in terms)
            self.subs.append((re.compile(rf"(?<![A-Za-z0-9])(?:{words})(?![A-Za-z0-9])", re.I), "<user>"))

    def text(self, s):
        for pat, rep in self.subs:
            s = pat.sub(rep, s)
        if len(s) > self.limit:
            s = s[: self.limit] + f"… [truncated {len(s) - self.limit} chars]"
        return s

    def value(self, v):
        if isinstance(v, str):
            return self.text(v)
        if isinstance(v, list):
            return [self.value(x) for x in v]
        if isinstance(v, dict):
            return {self.text(k): self.value(x) for k, x in v.items()}
        return v

    def jsonl_line(self, line):
        try:
            return json.dumps(self.value(json.loads(line)), ensure_ascii=False)
        except json.JSONDecodeError:
            return self.text(line)


def main():
    r = Redactor()
    for line in sys.stdin:
        line = line.rstrip("\n")
        if line:
            print(r.jsonl_line(line))


if __name__ == "__main__":
    main()
