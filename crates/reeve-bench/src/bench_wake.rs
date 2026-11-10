//! `reeve-bench wake`: replays the orchestrator sessions in your own Claude Code transcripts and
//! counts what woke them, as they ran (`reeve watch --verbose`) and as the quiet `reeve watch`
//! would have. It reads
//! `~/.claude/projects` locally and prints only totals: no names, paths or text.
//!
//! An orchestrator session is one whose first prompt names a handoff, report, brief or scratch
//! file of a worker called `morning…`, `orchestrat…`, `weekend…`, `evening…` or `overnight…`.
//! A wake-up is a turn started by a notification: a Monitor line, a Monitor's expiry, a
//! background command or agent ending. Each `reeve watch` line in a Monitor event is parsed
//! back into a worker and its state, each worker's states are replayed through the real quiet
//! [`Watch`], and a wake-up whose lines the quiet watch wouldn't print is dropped. Workers are
//! taken to have no `reeve heavy` job of their own, so the replay never drops more than reeve
//! would. Cost is in the units of Anthropic's prices relative to fresh input (cache read 0.1,
//! 5-minute write 1.25, 1-hour write 2, output 5; Sonnet 0.6×, Haiku 0.2×). A dropped wake-up
//! saves its turn's cost only if that turn did nothing but reply or re-arm a Monitor; any
//! other work is assumed to happen at a later wake-up anyway.

use reeve::workers::{State, Watch, Worker};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct Opts {
    pub projects: PathBuf,
    /// Only requests at or after this, in ms since the epoch.
    pub since_ms: u64,
    pub stall: Duration,
}

const W_INPUT: f64 = 1.0;
const W_WRITE_5M: f64 = 1.25;
const W_WRITE_1H: f64 = 2.0;
const W_READ: f64 = 0.1;
const W_OUTPUT: f64 = 5.0;
/// The quiet watch is stepped this often between the lines it replays.
const TICK_MS: u64 = 30_000;
/// The alternative to dropping progress lines: one summary line per window this long.
const BATCH_MS: u64 = 5 * 60_000;

/// Milliseconds since the epoch for an RFC 3339 UTC time such as `2026-11-03T19:55:14.560Z`.
pub fn parse_ts(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[10] != b'T' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (hh, mm, ss) = (n(11..13)?, n(14..16)?, n(17..19)?);
    let frac = s
        .get(19..)
        .and_then(|r| r.strip_prefix('.'))
        .map(|r| {
            r.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        })
        .map(|f| format!("{f:0<3}")[..3].parse::<i64>().unwrap_or(0))
        .unwrap_or(0);
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + hh * 3600 + mm * 60 + ss;
    u64::try_from(secs * 1000 + frac).ok()
}

/// The worker and state a `reeve watch` line reports, as `change` and the quiet idle line word
/// them; `None` for any other line.
pub fn watch_line(line: &str) -> Option<(&str, State)> {
    let (name, rest) = line.trim().split_once(' ')?;
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    {
        return None;
    }
    let s = [
        ("starting", State::Starting),
        ("running", State::Running),
        ("idle without a report", State::Idle),
        ("blocked", State::Blocked),
        ("done: report at", State::Done),
        ("gone", State::Gone),
        ("state unknown", State::Unknown),
    ]
    .into_iter()
    .find(|(p, _)| rest.starts_with(p))?
    .1;
    Some((name, s))
}

/// Whether a worker named in a session's first prompt marks it as an orchestrator.
pub fn is_orchestrator(first: &str) -> bool {
    ["handoffs/", "reports/", "briefs/", "scratch/"]
        .iter()
        .any(|dir| {
            first.match_indices(dir).any(|(i, _)| {
                let name = &first[i + dir.len()..];
                ["morning", "orchestrat", "weekend", "evening", "overnight"]
                    .iter()
                    .any(|p| name.starts_with(p))
            })
        })
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Trigger {
    Human,
    Monitor,
    MonitorEnded,
    Background,
    Other,
}

#[derive(Debug, Clone, Default)]
pub struct Req {
    pub ts: u64,
    pub cost: f64,
}

#[derive(Debug, Clone)]
pub struct Wake {
    pub ts: u64,
    pub trigger: Trigger,
    /// A Monitor event's lines.
    pub lines: Vec<String>,
    /// Indexes into the session's requests, for the turn it started.
    pub reqs: Vec<usize>,
    /// The turn did nothing but reply or re-arm a Monitor.
    pub idle: bool,
}

#[derive(Debug, Default)]
pub struct Session {
    pub orchestrator: bool,
    pub reqs: Vec<Req>,
    pub wakes: Vec<Wake>,
}

fn user_text(d: &Value) -> (bool, String) {
    match &d["message"]["content"] {
        Value::String(s) => (false, s.clone()),
        Value::Array(parts) => {
            let tool = parts.iter().any(|p| p["type"] == "tool_result");
            let text = parts
                .iter()
                .filter(|p| p["type"] == "text")
                .filter_map(|p| p["text"].as_str())
                .collect::<Vec<_>>()
                .join(" ");
            (tool, text)
        }
        _ => (false, String::new()),
    }
}

fn trigger_of(text: &str) -> Trigger {
    let t = text.trim_start();
    if !t.starts_with("<task-notification>") {
        return Trigger::Human;
    }
    let head: String = t.chars().take(600).collect();
    if head.contains("Monitor expired")
        || head.contains("<summary>Monitor") && !head.contains("Monitor event")
    {
        Trigger::MonitorEnded
    } else if head.contains("Monitor event") {
        Trigger::Monitor
    } else if head.contains("Background command") {
        Trigger::Background
    } else {
        Trigger::Other
    }
}

fn event_lines(text: &str) -> Vec<String> {
    let Some(start) = text.find("<event>") else {
        return vec![];
    };
    let body = &text[start + 7..];
    let body = body.find("</event>").map_or(body, |e| &body[..e]);
    body.lines()
        .map(str::to_string)
        .filter(|l| !l.trim().is_empty())
        .collect()
}

fn model_mult(model: &str) -> f64 {
    if model.contains("sonnet") {
        0.6
    } else if model.contains("haiku") {
        0.2
    } else {
        1.0
    }
}

/// One transcript, read into its requests and the wake-ups that started turns.
pub fn read_session(text: &str, since_ms: u64) -> Session {
    let mut s = Session::default();
    let mut first: Option<String> = None;
    let mut seen = std::collections::HashSet::new();
    let mut open: Option<usize> = None;
    let mut turn_tools: Vec<String> = vec![];
    let close = |s: &mut Session, open: Option<usize>, tools: &[String]| {
        if let Some(i) = open {
            s.wakes[i].idle = tools.iter().all(|t| t == "Monitor");
        }
    };
    for line in text.lines() {
        let Ok(d) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match d["type"].as_str() {
            Some("user") if d["isMeta"] != true => {
                let (tool, text) = user_text(&d);
                if tool {
                    continue;
                }
                if first.is_none() && !text.trim().is_empty() && !text.trim_start().starts_with('<')
                {
                    first = Some(text.clone());
                }
                if text.trim_start().starts_with("<system-reminder>") {
                    continue;
                }
                close(&mut s, open.take(), &turn_tools);
                turn_tools.clear();
                let trigger = trigger_of(&text);
                let ts = d["timestamp"].as_str().and_then(parse_ts).unwrap_or(0);
                if trigger != Trigger::Human && ts >= since_ms {
                    s.wakes.push(Wake {
                        ts,
                        trigger,
                        lines: if trigger == Trigger::Monitor {
                            event_lines(&text)
                        } else {
                            vec![]
                        },
                        reqs: vec![],
                        idle: true,
                    });
                    open = Some(s.wakes.len() - 1);
                }
            }
            Some("assistant") => {
                let m = &d["message"];
                let ts = d["timestamp"].as_str().and_then(parse_ts).unwrap_or(0);
                for p in m["content"].as_array().into_iter().flatten() {
                    if p["type"] == "tool_use" {
                        turn_tools.push(p["name"].as_str().unwrap_or("").to_string());
                    }
                }
                let id = d["requestId"]
                    .as_str()
                    .or(m["id"].as_str())
                    .unwrap_or("")
                    .to_string();
                let u = &m["usage"];
                if ts < since_ms
                    || !u.is_object()
                    || m["model"] == "<synthetic>"
                    || !seen.insert(id)
                {
                    continue;
                }
                let n = |k: &str| u[k].as_u64().unwrap_or(0);
                let w1h = u["cache_creation"]["ephemeral_1h_input_tokens"]
                    .as_u64()
                    .unwrap_or(0);
                let wt = n("cache_creation_input_tokens");
                let w5 = wt.saturating_sub(w1h);
                let read = n("cache_read_input_tokens");
                let input = n("input_tokens");
                let mult = model_mult(m["model"].as_str().unwrap_or(""));
                let cost = (input as f64 * W_INPUT
                    + w5 as f64 * W_WRITE_5M
                    + w1h as f64 * W_WRITE_1H
                    + read as f64 * W_READ
                    + n("output_tokens") as f64 * W_OUTPUT)
                    * mult;
                s.reqs.push(Req { ts, cost });
                if let Some(i) = open {
                    s.wakes[i].reqs.push(s.reqs.len() - 1);
                }
            }
            _ => {}
        }
    }
    close(&mut s, open, &turn_tools);
    s.orchestrator = first.as_deref().is_some_and(is_orchestrator);
    s
}

fn worker(name: &str) -> Worker {
    serde_json::from_value(json!({
        "name": name, "backend": "pane", "cwd": "/", "started": 0,
        "report": format!("/r/{name}.md"), "handle": "p",
    }))
    .expect("a worker")
}

/// What the quiet watch makes of a session's `reeve watch` lines: for each wake-up, whether it
/// still happens, and the times of wake-ups it adds (an idle worker reported once it stalled).
pub fn quiet_replay(wakes: &[Wake], stall: Duration) -> (Vec<bool>, Vec<u64>) {
    // Each worker's state over time, from the lines the verbose watch printed.
    let mut events: Vec<(u64, String, State)> = vec![];
    for w in wakes {
        for l in &w.lines {
            if let Some((name, s)) = watch_line(l) {
                events.push((w.ts, name.to_string(), s));
            }
        }
    }
    let watchish = |w: &Wake| {
        w.trigger == Trigger::Monitor
            && !w.lines.is_empty()
            && w.lines.iter().all(|l| watch_line(l).is_some())
    };
    let mut keep: Vec<bool> = wakes.iter().map(|w| !watchish(w)).collect();
    if events.is_empty() {
        return (keep, vec![]);
    }
    let (start, end) = (
        events[0].0,
        events.last().expect("events").0 + stall.as_millis() as u64,
    );
    let mut quiet = Watch::default().quiet(stall);
    let mut state: BTreeMap<String, State> = BTreeMap::new();
    let mut emitted: Vec<u64> = vec![];
    let mut i = 0;
    let mut t = start;
    while t <= end {
        let mut changed = false;
        while i < events.len() && events[i].0 <= t {
            state.insert(events[i].1.clone(), events[i].2);
            i += 1;
            changed = true;
        }
        let polled: Vec<(Worker, State, bool)> =
            state.iter().map(|(n, s)| (worker(n), *s, false)).collect();
        if !quiet.step_at(t, &polled).is_empty() {
            emitted.push(t);
        }
        // Step to the next line, or one tick on while something may yet stall.
        let next = events.get(i).map_or(u64::MAX, |e| e.0);
        t = if changed || state.values().any(|s| *s == State::Idle) {
            (t + TICK_MS).min(next)
        } else {
            next
        };
        if t == u64::MAX {
            break;
        }
    }
    // A watch wake-up stays if the quiet watch printed something at its time.
    let mut added = vec![];
    for e in emitted {
        let near = wakes
            .iter()
            .enumerate()
            .find(|(_, w)| watchish(w) && w.ts <= e && e < w.ts + TICK_MS);
        match near {
            Some((k, _)) => keep[k] = true,
            None => added.push(e),
        }
    }
    added.dedup_by(|a, b| a.saturating_sub(*b) < TICK_MS);
    (keep, added)
}

/// Wake-ups if starts, progress and idles were batched into one line per BATCH_MS instead.
pub fn batched(wakes: &[Wake]) -> usize {
    let mut windows = std::collections::BTreeSet::new();
    let mut n = 0;
    for w in wakes {
        let parsed: Vec<_> = w.lines.iter().map(|l| watch_line(l)).collect();
        let watchish = w.trigger == Trigger::Monitor
            && !parsed.is_empty()
            && parsed.iter().all(Option::is_some);
        if !watchish {
            n += 1;
            continue;
        }
        let needs = parsed
            .iter()
            .flatten()
            .any(|(_, s)| matches!(s, State::Done | State::Blocked | State::Gone));
        if needs {
            n += 1;
        } else {
            windows.insert(w.ts / BATCH_MS);
        }
    }
    n + windows.len()
}

#[derive(Debug, Default, Clone)]
pub struct Day {
    pub sessions: usize,
    pub hours: f64,
    pub wakes: usize,
    pub wakes_quiet: usize,
    pub wakes_batched: usize,
    pub idle_wakes: usize,
    pub cost: f64,
    pub cost_quiet: f64,
    /// Wake-ups before and with the quiet watch, by what started them.
    pub by: BTreeMap<&'static str, (usize, usize)>,
}

const DAY_MS: u64 = 86_400_000;

/// Totals per UTC day for the orchestrator sessions.
pub fn tally(sessions: &[Session], o: &Opts) -> BTreeMap<u64, Day> {
    let mut days: BTreeMap<u64, Day> = BTreeMap::new();
    for s in sessions
        .iter()
        .filter(|s| s.orchestrator && !s.reqs.is_empty())
    {
        let first = s.reqs[0].ts;
        let d = days.entry(first / DAY_MS).or_default();
        d.sessions += 1;
        d.hours += (s.reqs.last().expect("reqs").ts - first) as f64 / 3_600_000.0;
        let (keep, added) = quiet_replay(&s.wakes, o.stall);
        d.wakes += s.wakes.len();
        d.wakes_quiet += keep.iter().filter(|k| **k).count() + added.len();
        d.wakes_batched += batched(&s.wakes);
        d.idle_wakes += s.wakes.iter().filter(|w| w.idle).count();
        for (w, k) in s.wakes.iter().zip(&keep) {
            let e = d.by.entry(kind(w)).or_default();
            e.0 += 1;
            e.1 += usize::from(*k);
        }
        d.by.entry("monitor: reeve watch").or_default().1 += added.len();
        // The requests left once dropped idle turns are gone; an added wake-up costs as much
        // as this session's average idle turn.
        let mut dropped = vec![false; s.reqs.len()];
        let (mut idle_cost, mut idle_n) = (0.0, 0);
        for (w, k) in s.wakes.iter().zip(&keep) {
            if w.idle {
                idle_cost += w.reqs.iter().map(|i| s.reqs[*i].cost).sum::<f64>();
                idle_n += 1;
                if !k {
                    for i in &w.reqs {
                        dropped[*i] = true;
                    }
                }
            }
        }
        let added_cost = added.len() as f64
            * if idle_n > 0 {
                idle_cost / idle_n as f64
            } else {
                0.0
            };
        let cost: f64 = s.reqs.iter().map(|r| r.cost).sum();
        let saved: f64 = s
            .reqs
            .iter()
            .zip(&dropped)
            .filter(|(_, d)| **d)
            .map(|(r, _)| r.cost)
            .sum();
        d.cost += cost;
        d.cost_quiet += cost - saved + added_cost;
    }
    days
}

fn transcripts(root: &Path, since_ms: u64) -> Vec<PathBuf> {
    let mut out = vec![];
    let Ok(dirs) = fs::read_dir(root) else {
        return out;
    };
    let since = std::time::UNIX_EPOCH + Duration::from_millis(since_ms);
    for d in dirs.flatten() {
        let Ok(files) = fs::read_dir(d.path()) else {
            continue;
        };
        for f in files.flatten() {
            let p = f.path();
            let fresh = f
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|m| m >= since);
            if fresh && p.extension().is_some_and(|e| e == "jsonl") {
                out.push(p);
            }
        }
    }
    out
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    match v.len() {
        0 => 0.0,
        n if n % 2 == 1 => v[n / 2],
        n => (v[n / 2 - 1] + v[n / 2]) / 2.0,
    }
}

pub fn main(o: &Opts) -> io::Result<i32> {
    let sessions: Vec<Session> = transcripts(&o.projects, o.since_ms)
        .iter()
        .filter_map(|p| fs::read_to_string(p).ok())
        .map(|t| read_session(&t, o.since_ms))
        .collect();
    let days = tally(&sessions, o);
    println!(
        "day\tsessions\thours\twakes/h\tquiet wakes/h\tbatched wakes/h\tidle wakes\tcost Mu\tquiet"
    );
    let mut total = Day::default();
    let per_h = |n: usize, h: f64| if h > 0.0 { n as f64 / h } else { 0.0 };
    let mut ratios: HashMap<&str, Vec<f64>> = HashMap::new();
    for (day, d) in &days {
        println!(
            "{day}\t{}\t{:.1}\t{:.1}\t{:.1}\t{:.1}\t{}\t{:.1}\t{:.1}",
            d.sessions,
            d.hours,
            per_h(d.wakes, d.hours),
            per_h(d.wakes_quiet, d.hours),
            per_h(d.wakes_batched, d.hours),
            d.idle_wakes,
            d.cost / 1e6,
            d.cost_quiet / 1e6
        );
        if d.wakes > 0 && d.cost > 0.0 {
            ratios
                .entry("wakes")
                .or_default()
                .push(d.wakes_quiet as f64 / d.wakes as f64);
            ratios
                .entry("cost_quiet")
                .or_default()
                .push(d.cost_quiet / d.cost);
        }
        total.sessions += d.sessions;
        total.hours += d.hours;
        total.wakes += d.wakes;
        total.wakes_quiet += d.wakes_quiet;
        total.wakes_batched += d.wakes_batched;
        total.idle_wakes += d.idle_wakes;
        total.cost += d.cost;
        total.cost_quiet += d.cost_quiet;
        for (k, (b, a)) in &d.by {
            let e = total.by.entry(k).or_default();
            e.0 += b;
            e.1 += a;
        }
    }
    let n = days.len().max(1) as f64;
    println!(
        "all\t{}\t{:.1}\t{:.1}\t{:.1}\t{:.1}\t{}\t{:.1}/day\t{:.1}/day",
        total.sessions,
        total.hours,
        per_h(total.wakes, total.hours),
        per_h(total.wakes_quiet, total.hours),
        per_h(total.wakes_batched, total.hours),
        total.idle_wakes,
        total.cost / 1e6 / n,
        total.cost_quiet / 1e6 / n
    );
    for k in ["wakes", "cost_quiet"] {
        let v = ratios.remove(k).unwrap_or_default();
        let (lo, hi) = v
            .iter()
            .fold((f64::MAX, f64::MIN), |(a, b), x| (a.min(*x), b.max(*x)));
        println!(
            "after/before {k}: median {:.2}, range {:.2}-{:.2}, n={} days",
            median(v.clone()),
            lo,
            hi,
            v.len()
        );
    }
    for (k, (b, a)) in &total.by {
        println!(
            "wakes/h from {k}: {:.2} -> {:.2}",
            per_h(*b, total.hours),
            per_h(*a, total.hours)
        );
    }
    Ok(0)
}

/// What started a wake-up, for the breakdown.
fn kind(w: &Wake) -> &'static str {
    match w.trigger {
        Trigger::Monitor if w.lines.iter().all(|l| watch_line(l).is_some()) => {
            "monitor: reeve watch"
        }
        Trigger::Monitor if w.lines.iter().any(|l| l.starts_with("reeve: message")) => {
            "monitor: inbox"
        }
        Trigger::Monitor => "monitor: other",
        Trigger::MonitorEnded => "monitor expired or ended",
        Trigger::Background => "background command",
        Trigger::Human | Trigger::Other => "other notification",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_parse_to_epoch_ms() {
        assert_eq!(parse_ts("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_ts("2026-11-03T19:55:14.560Z"),
            Some(1_793_735_714_560)
        );
        assert_eq!(parse_ts("nope"), None);
    }

    #[test]
    fn watch_lines_parse_back_to_worker_and_state() {
        assert_eq!(
            watch_line("a-1 running again"),
            Some(("a-1", State::Running))
        );
        assert_eq!(
            watch_line("b idle without a report: read its tab"),
            Some(("b", State::Idle))
        );
        assert_eq!(
            watch_line("c done: report at ~/r/c.md; read it"),
            Some(("c", State::Done))
        );
        assert_eq!(watch_line("reeve: message 1 from local: x done: y"), None);
        assert_eq!(watch_line("[Monitor expired after 30m]"), None);
    }

    #[test]
    fn orchestrators_are_known_by_the_worker_their_first_prompt_names() {
        assert!(is_orchestrator(
            "Read /s/reeve/handoffs/morning-1109-3.md and go on"
        ));
        assert!(is_orchestrator("Read ~/x/briefs/weekend-2.md"));
        assert!(!is_orchestrator("Read /s/reeve/handoffs/fix-flaky.md"));
    }

    fn wake(ts: u64, lines: &[&str]) -> Wake {
        Wake {
            ts,
            trigger: Trigger::Monitor,
            lines: lines.iter().map(|l| l.to_string()).collect(),
            reqs: vec![],
            idle: true,
        }
    }

    #[test]
    fn the_quiet_replay_drops_progress_and_short_idles_and_keeps_what_needs_acting_on() {
        let m = 60_000;
        let wakes = [
            wake(0, &["a running"]),
            wake(m, &["a idle without a report: x"]),
            wake(2 * m, &["a running again"]),
            wake(3 * m, &["a blocked (waiting for the user): x"]),
            wake(4 * m, &["a running again"]),
            wake(5 * m, &["reeve: message 1 from local: hi"]),
            wake(6 * m, &["a idle without a report: x"]),
            wake(30 * m, &["a done: report at x"]),
        ];
        let (keep, added) = quiet_replay(&wakes, Duration::from_secs(600));
        assert_eq!(keep, [false, false, false, true, false, true, false, true]);
        // The last idle lasted 24 minutes, so it surfaces once, 10 minutes in.
        assert_eq!(added, [16 * m]);
        // Batching keeps blocked, the message and done, plus one line per 5-minute window.
        assert_eq!(batched(&wakes), 3 + 2);
    }
}
