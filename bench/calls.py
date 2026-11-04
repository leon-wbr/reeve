#!/usr/bin/env python3
"""One line per tool call in a stream-json transcript: the tool and what it was asked to do.

Usage: calls.py TRANSCRIPT.jsonl...
"""

import json
import sys

WIDTH = 110


def describe(name, inp):
    for key in ("command", "file_path", "pattern", "url", "query", "prompt", "skill"):
        if key in inp:
            return " ".join(str(inp[key]).split())
    return json.dumps(inp, ensure_ascii=False)


def calls(path):
    with open(path) as f:
        for line in f:
            try:
                ev = json.loads(line)
            except json.JSONDecodeError:
                continue
            if ev.get("type") != "assistant":
                continue
            for block in (ev.get("message") or {}).get("content") or []:
                if block.get("type") == "tool_use":
                    text = f"{block['name']}: {describe(block['name'], block.get('input') or {})}"
                    yield text if len(text) <= WIDTH else text[: WIDTH - 1] + "…"


def main():
    for path in sys.argv[1:]:
        print(f"{path}:")
        for c in calls(path):
            print(f"  {c}")


if __name__ == "__main__":
    main()
