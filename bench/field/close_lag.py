"""Field measure for automatic closing: how a done worker got closed, and how long it stayed open.

Reads reeve's local event log and report files, read-only, and prints aggregates only (no names,
no paths). For each `reeve close NAME` in the last N days, the lag is the time from the worker's
report file's last write to the close. Usage: python3 -I close_lag.py [DAYS]
"""

import json
import os
import statistics
import sys
import time

days = float(sys.argv[1]) if len(sys.argv) > 1 else 7
state = os.environ.get("XDG_STATE_HOME", os.path.expanduser("~/.local/state")) + "/reeve"
since = (time.time() - days * 86400) * 1000

closes, by_actor, cmds = [], {}, {}
with open(state + "/events.jsonl") as f:
    for line in f:
        try:
            e = json.loads(line)
        except ValueError:
            continue
        if e.get("ts", 0) < since:
            continue
        cmds[e.get("command")] = cmds.get(e.get("command"), 0) + 1
        if e.get("command") == "close" and e.get("worker"):
            closes.append(e)
            a = e.get("actor", "?")
            by_actor[a] = by_actor.get(a, 0) + 1

lags = []
for e in closes:
    p = f"{state}/reports/{e['worker']}.md"
    try:
        lag = e["ts"] / 1000 - os.path.getmtime(p)
    except OSError:
        continue
    if lag >= 0:
        lags.append(lag / 60)

workers = json.load(open(state + "/workers.json"))
ws = workers if isinstance(workers, list) else workers.get("workers", list(workers.values()))
started = sum(1 for w in ws if w.get("started", 0) * 1000 >= since)

def q(xs, p):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(p * len(xs)))]

print(f"days\t{days:g}")
print(f"workers_started\t{started}")
print(f"close_calls\t{len(closes)}")
print(f"close_calls_by_actor\t{json.dumps(by_actor, sort_keys=True)}")
print(f"close_calls_per_day\t{len(closes) / days:.1f}")
print(f"report_calls\t{cmds.get('report', 0)}")
print(f"ls_calls\t{cmds.get('ls', 0)}")
print(f"closes_with_report\t{len(lags)}")
if lags:
    print(f"lag_min_p50\t{statistics.median(lags):.1f}")
    print(f"lag_min_p90\t{q(lags, 0.9):.1f}")
    print(f"lag_min_max\t{max(lags):.1f}")
    print(f"lag_over_10min\t{sum(1 for x in lags if x > 10)}")
    print(f"lag_over_60min\t{sum(1 for x in lags if x > 60)}")
