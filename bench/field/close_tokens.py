"""Field measure: tool calls and tokens Claude Code sessions spent on `reeve close`.

Reads ~/.claude/projects/*/*.jsonl changed in the last N days, read-only, and prints aggregates
only. A call is a Bash tool use whose command runs `reeve close`. A bookkeeping turn is an
assistant message whose every tool use is such a call; its tokens are what that turn cost
(input, cache read and write, output), which automatic closing saves.
Usage: python3 -I close_tokens.py [DAYS]
"""

import glob
import json
import os
import re
import sys
import time

days = float(sys.argv[1]) if len(sys.argv) > 1 else 7
since = time.time() - days * 86400
close = re.compile(r"(^|[;&|\s])reeve close\b")

calls = turns = sessions = 0
tok = {"input": 0, "cache_read": 0, "cache_write": 0, "output": 0}
seen = set()
for path in glob.glob(os.path.expanduser("~/.claude/projects/*/*.jsonl")):
    if os.path.getmtime(path) < since:
        continue
    hit = False
    with open(path, errors="replace") as f:
        for line in f:
            if "reeve close" not in line:
                continue
            try:
                e = json.loads(line)
            except ValueError:
                continue
            m = e.get("message") or {}
            if e.get("type") != "assistant" or not isinstance(m.get("content"), list):
                continue
            uses = [c for c in m["content"] if c.get("type") == "tool_use"]
            ours = [
                u for u in uses
                if u.get("name") == "Bash" and close.search(str(u.get("input", {}).get("command", "")))
            ]
            if not ours:
                continue
            # A message is split over several lines, one per content block; count each id once.
            key = (m.get("id"), ours[0].get("id"))
            if key in seen:
                continue
            seen.add(key)
            hit = True
            calls += len(ours)
            if len(ours) == len(uses):
                turns += 1
                u = m.get("usage") or {}
                tok["input"] += u.get("input_tokens", 0)
                tok["cache_read"] += u.get("cache_read_input_tokens", 0)
                tok["cache_write"] += u.get("cache_creation_input_tokens", 0)
                tok["output"] += u.get("output_tokens", 0)
    sessions += hit

print(f"days\t{days:g}")
print(f"sessions_with_reeve_close\t{sessions}")
print(f"reeve_close_tool_calls\t{calls}")
print(f"bookkeeping_turns\t{turns}")
for k, v in tok.items():
    print(f"tokens_{k}\t{v}")
if turns:
    print(f"cache_read_per_turn\t{tok['cache_read'] // turns}")
    print(f"output_per_turn\t{tok['output'] // turns}")
