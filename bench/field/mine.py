"""Field metrics from Claude Code transcripts (read-only, local). Usage: python3 -I field.py <days> <out.json>"""
import json, os, re, sys, time, glob
from collections import Counter, defaultdict
from datetime import datetime

DAYS = int(sys.argv[1]); OUT = sys.argv[2]
root = os.path.expanduser('~/.claude/projects')
cutoff = time.time() - DAYS * 86400
SKIP = ('reeve-trials', 'reeve-vanilla-trial')

def ts(s):
    try: return datetime.fromisoformat(s.replace('Z', '+00:00')).timestamp()
    except Exception: return None

def text_of(content):
    if isinstance(content, str): return content
    if isinstance(content, list):
        return '\n'.join(c.get('text', '') for c in content if isinstance(c, dict) and c.get('type') == 'text')
    return ''

BLOCK = [
    ('classifier', re.compile(r'denied by the Claude Code auto mode classifier')),
    ('permission', re.compile(r"Permission (for this action )?(was |has been )?denied|user doesn't want to proceed|requires approval", re.I)),
    ('sandbox', re.compile(r'Operation not permitted')),
]
POLL = re.compile(r"^\s*(status|progress|how('s| is) it going|any news|update\??|eta\??|is it (still )?running|still (going|running)|are you (still )?(there|working)|you sure it's going|check if anything is stuck|progress\?)", re.I)
TROUBLE = re.compile(r'stuck|hang|hung|frozen|not (start|running|working)|failed to start|crash|retry|again\?|what happened|why did', re.I)

sessions = []
for f in glob.glob(f'{root}/*/*.jsonl'):
    if any(s in f for s in SKIP): continue
    try:
        if os.path.getmtime(f) < cutoff: continue
    except OSError: continue
    s = dict(file=f, project=f.split('/')[-2], human=0, peer=0, notifications=0, polls=0, trouble=0,
             blocks=Counter(), ask_tool=0, questions=[], start=None, end=None, assistant_turns=0,
             pr_create=0, pr_merge=0, bg_starts=0, entry=None, opening=None)
    last_assistant_text, last_assistant_t = None, None
    try:
        fh = open(f, errors='replace')
    except OSError: continue
    with fh:
        for line in fh:
            try: m = json.loads(line)
            except Exception: continue
            t = ts(m.get('timestamp', '')) if m.get('timestamp') else None
            if t:
                s['start'] = s['start'] or t; s['end'] = t
            s['entry'] = s['entry'] or m.get('entrypoint')
            typ = m.get('type')
            msg = m.get('message') or {}
            if typ == 'assistant':
                s['assistant_turns'] += 1
                for c in msg.get('content') or []:
                    if not isinstance(c, dict): continue
                    if c.get('type') == 'text' and c.get('text', '').strip():
                        last_assistant_text, last_assistant_t = c['text'].strip(), t
                    if c.get('type') == 'tool_use':
                        if c.get('name') == 'AskUserQuestion': s['ask_tool'] += 1
                        cmd = str((c.get('input') or {}).get('command', ''))
                        if 'gh pr create' in cmd: s['pr_create'] += 1
                        if 'gh pr merge' in cmd: s['pr_merge'] += 1
                        if re.search(r'claude\b.*--bg|herdr agent start|start-worker', cmd): s['bg_starts'] += 1
            elif typ == 'user':
                content = msg.get('content')
                if isinstance(content, list) and any(isinstance(c, dict) and c.get('type') == 'tool_result' for c in content):
                    for c in content:
                        if isinstance(c, dict) and c.get('type') == 'tool_result':
                            body = c.get('content'); body = body if isinstance(body, str) else json.dumps(body)[:4000]
                            for k, rx in BLOCK:
                                if rx.search(body[:4000]): s['blocks'][k] += 1; break
                    continue
                txt = text_of(content).strip()
                if not txt or m.get('isMeta') or txt.startswith('<command-') or txt.startswith('<local-command'): continue
                if s['opening'] is None:
                    s['opening'] = txt[:80]; continue
                if txt.startswith('Another Claude session') or '<cross-session-message' in txt[:300] or '<agent-message' in txt[:300] or 'teammate-message' in txt[:40]:
                    s['peer'] += 1; continue
                if txt.startswith('<task-notification') or txt.startswith('<system-reminder') or txt.startswith('Check-in:') or txt.startswith('[SYSTEM'):
                    s['notifications'] += 1; continue
                s['human'] += 1
                if POLL.search(txt) or len(txt) < 40 and txt.endswith('?') and re.search(r'status|progress|going|eta|done|stuck|running', txt, re.I): s['polls'] += 1
                if len(txt) < 400 and TROUBLE.search(txt): s['trouble'] += 1
                if last_assistant_text and last_assistant_t and t and last_assistant_text.rstrip().endswith('?'):
                    s['questions'].append(round(t - last_assistant_t))
                last_assistant_text = None
    if s['start'] and s['assistant_turns']:
        sessions.append(s)

def pct(xs, p):
    xs = sorted(xs); return xs[min(len(xs) - 1, int(p * len(xs)))] if xs else None

days = defaultdict(lambda: Counter())
for s in sessions:
    d = datetime.fromtimestamp(s['start']).strftime('%Y-%m-%d')
    c = days[d]
    c['sessions'] += 1; c['human'] += s['human']; c['peer'] += s['peer']; c['polls'] += s['polls']; c['trouble'] += s['trouble']
    c['questions'] += len(s['questions']); c['ask_tool'] += s['ask_tool']; c['pr_create'] += s['pr_create']; c['pr_merge'] += s['pr_merge']
    c['blocks'] += sum(s['blocks'].values()); c['classifier'] += s['blocks']['classifier']; c['bg_starts'] += s['bg_starts']
    if s['human'] == 0: c['no_human'] += 1

waits = [w for s in sessions for w in s['questions']]
blocks = Counter(); [blocks.update(s['blocks']) for s in sessions]
projects = Counter(s['project'] for s in sessions)
summary = dict(
    days=DAYS, sessions=len(sessions),
    human_msgs=sum(s['human'] for s in sessions), peer_msgs=sum(s['peer'] for s in sessions),
    polls=sum(s['polls'] for s in sessions), trouble_msgs=sum(s['trouble'] for s in sessions),
    questions_answered=len(waits), question_wait_s=dict(p50=pct(waits, .5), p90=pct(waits, .9)),
    ask_tool=sum(s['ask_tool'] for s in sessions), blocks=dict(blocks),
    pr_create=sum(s['pr_create'] for s in sessions), pr_merge=sum(s['pr_merge'] for s in sessions),
    sessions_without_human=sum(1 for s in sessions if s['human'] == 0),
    conversational_sessions=sum(1 for s in sessions if s['human'] > 0),
    worker_sessions=sum(1 for s in sessions if (s['opening'] or '').startswith('Your brief is')),
    human_msgs_per_conversational=round(sum(s['human'] for s in sessions) / max(1, sum(1 for s in sessions if s['human'] > 0)), 1),
    session_minutes=dict(p50=pct([(s['end'] - s['start']) / 60 for s in sessions], .5), p90=pct([(s['end'] - s['start']) / 60 for s in sessions], .9)),
    per_day={d: dict(c) for d, c in sorted(days.items())},
    top_projects=projects.most_common(12),
)
json.dump(summary, open(OUT, 'w'), indent=1)
print(json.dumps({k: v for k, v in summary.items() if k not in ('per_day',)}, indent=1))
