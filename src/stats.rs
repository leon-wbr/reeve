//! `reeve stats`: a summary of the event log over the last few days.

use crate::events::Event;
use crate::workers::State;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Default, PartialEq, Serialize)]
pub struct Stats {
    pub days: u64,
    pub commands: usize,
    pub workers: Workers,
    pub heavy: Heavy,
    pub handoff_ms: Spread,
    pub errors: BTreeMap<String, usize>,
    pub actors: BTreeMap<String, usize>,
    pub agents: BTreeMap<String, usize>,
}

/// Started and failed come from the log; the rest is the state now of each worker started in
/// the window, counted once per name.
#[derive(Debug, Default, PartialEq, Serialize)]
pub struct Workers {
    pub started: usize,
    pub failed_starts: usize,
    pub done: usize,
    pub gone: usize,
    pub running: usize,
    pub unknown: usize,
}

#[derive(Debug, Default, PartialEq, Serialize)]
pub struct Heavy {
    pub runs: usize,
    pub wait_ms: Spread,
    pub run_ms: Spread,
}

#[derive(Debug, Default, PartialEq, Serialize)]
pub struct Spread {
    pub n: usize,
    pub p50: Option<u64>,
    pub p90: Option<u64>,
}

impl Spread {
    fn of(mut v: Vec<u64>) -> Spread {
        v.sort_unstable();
        Spread {
            n: v.len(),
            p50: percentile(&v, 50),
            p90: percentile(&v, 90),
        }
    }
}

/// Nearest rank on sorted values.
pub fn percentile(sorted: &[u64], p: usize) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (p * sorted.len()).div_ceil(100).max(1);
    Some(sorted[rank - 1])
}

pub fn summarize(
    events: &[Event],
    now_ms: u64,
    days: u64,
    state: impl Fn(&str) -> Option<State>,
) -> Stats {
    let since = now_ms.saturating_sub(days * 86_400_000);
    let events: Vec<&Event> = events.iter().filter(|e| e.ts >= since).collect();
    let mut s = Stats {
        days,
        commands: events.len(),
        ..Stats::default()
    };
    let runs = events.iter().filter(|e| e.command == "run");
    let mut started = BTreeSet::new();
    for e in runs {
        match (&e.error, &e.worker) {
            (Some(_), _) => s.workers.failed_starts += 1,
            (None, Some(w)) => {
                s.workers.started += 1;
                started.insert(w.as_str());
            }
            _ => {}
        }
    }
    for w in started {
        match state(w) {
            Some(State::Done) => s.workers.done += 1,
            Some(State::Gone) => s.workers.gone += 1,
            Some(State::Starting | State::Running | State::Idle | State::Blocked) => {
                s.workers.running += 1
            }
            Some(State::Unknown) | None => s.workers.unknown += 1,
        }
    }
    let heavy: Vec<&&Event> = events.iter().filter(|e| e.command == "heavy").collect();
    s.heavy = Heavy {
        runs: heavy.iter().filter(|e| e.heavy_run_ms.is_some()).count(),
        wait_ms: Spread::of(heavy.iter().filter_map(|e| e.heavy_wait_ms).collect()),
        run_ms: Spread::of(heavy.iter().filter_map(|e| e.heavy_run_ms).collect()),
    };
    s.handoff_ms = Spread::of(events.iter().filter_map(|e| e.handoff_ms).collect());
    for e in &events {
        if let Some(k) = &e.error {
            *s.errors.entry(k.clone()).or_default() += 1;
        }
        *s.actors.entry(e.actor.clone()).or_default() += 1;
        if let Some(a) = &e.agent {
            *s.agents.entry(a.clone()).or_default() += 1;
        }
    }
    s
}

pub fn duration(ms: u64) -> String {
    match ms {
        m if m < 1000 => format!("{m}ms"),
        m if m < 60_000 => format!("{:.1}s", m as f64 / 1000.0),
        m if m < 3_600_000 => format!("{}m{:02}s", m / 60_000, m / 1000 % 60),
        m => format!("{}h{:02}m", m / 3_600_000, m / 60_000 % 60),
    }
}

fn spread(s: &Spread) -> String {
    let d = |v: Option<u64>| v.map_or("-".into(), duration);
    format!("p50 {} p90 {}", d(s.p50), d(s.p90))
}

fn counts(m: &BTreeMap<String, usize>) -> String {
    let mut v: Vec<_> = m.iter().collect();
    v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    v.iter()
        .map(|(k, n)| format!("{k} {n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn render(s: &Stats) -> String {
    let w = &s.workers;
    let none = |m: &BTreeMap<String, usize>| {
        if m.is_empty() {
            "none".into()
        } else {
            counts(m)
        }
    };
    let agents = if s.agents.is_empty() {
        String::new()
    } else {
        format!(" ({})", counts(&s.agents))
    };
    format!(
        "last {} days: {} commands\n\
         workers: {} started ({} done, {} gone, {} running, {} unknown), {} failed starts\n\
         heavy: {} runs, wait {}, run {}\n\
         handoff: {} over {}\n\
         errors: {}\n\
         actors: {}{agents}\n",
        s.days,
        s.commands,
        w.started,
        w.done,
        w.gone,
        w.running,
        w.unknown,
        w.failed_starts,
        s.heavy.runs,
        spread(&s.heavy.wait_ms),
        spread(&s.heavy.run_ms),
        spread(&s.handoff_ms),
        s.handoff_ms.n,
        none(&s.errors),
        none(&s.actors),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400_000;

    fn ev(command: &str, ts: u64) -> Event {
        Event {
            ts,
            command: command.into(),
            actor: "person".into(),
            exit: Some(0),
            ..Event::default()
        }
    }

    fn run(worker: &str, error: Option<&str>) -> Event {
        Event {
            worker: Some(worker.into()),
            error: error.map(Into::into),
            handoff_ms: error.is_none().then_some(3000),
            ..ev("run", 10 * DAY)
        }
    }

    fn heavy(wait: u64, run: Option<u64>) -> Event {
        Event {
            heavy_wait_ms: Some(wait),
            heavy_run_ms: run,
            ..ev("heavy", 10 * DAY)
        }
    }

    #[test]
    fn percentiles_use_the_nearest_rank() {
        let v: Vec<u64> = (1..=10).collect();
        assert_eq!(percentile(&v, 50), Some(5));
        assert_eq!(percentile(&v, 90), Some(9));
        assert_eq!(percentile(&[7], 90), Some(7));
        assert_eq!(percentile(&[], 50), None);
    }

    #[test]
    fn only_the_window_counts() {
        let all = [ev("ls", DAY), ev("ls", 9 * DAY), ev("ls", 10 * DAY)];
        assert_eq!(summarize(&all, 10 * DAY, 7, |_| None).commands, 2);
        assert_eq!(summarize(&all, 10 * DAY, 30, |_| None).commands, 3);
    }

    #[test]
    fn workers_are_started_once_per_name_and_judged_by_their_state_now() {
        let all = [
            run("a", None),
            run("a", None),
            run("b", None),
            run("c", None),
            run("d", Some("herdr")),
        ];
        let s = summarize(&all, 10 * DAY, 7, |w| match w {
            "a" => Some(State::Done),
            "b" => Some(State::Gone),
            _ => None,
        });
        assert_eq!(
            s.workers,
            Workers {
                started: 4,
                failed_starts: 1,
                done: 1,
                gone: 1,
                running: 0,
                unknown: 1,
            }
        );
        assert_eq!(s.handoff_ms.n, 4);
        assert_eq!(s.handoff_ms.p50, Some(3000));
        assert_eq!(s.errors["herdr"], 1);
    }

    #[test]
    fn heavy_counts_runs_and_spreads_waits_including_interrupted_ones() {
        let all = [
            heavy(0, Some(100)),
            heavy(1000, Some(300)),
            heavy(9000, None),
        ];
        let s = summarize(&all, 10 * DAY, 7, |_| None);
        assert_eq!(s.heavy.runs, 2);
        assert_eq!(s.heavy.wait_ms.n, 3);
        assert_eq!(s.heavy.wait_ms.p90, Some(9000));
        assert_eq!(s.heavy.run_ms.p50, Some(100));
    }

    #[test]
    fn actors_and_agents_are_split() {
        let agent = Event {
            actor: "agent".into(),
            agent: Some("codex".into()),
            ..ev("ls", 10 * DAY)
        };
        let s = summarize(&[agent, ev("ls", 10 * DAY)], 10 * DAY, 7, |_| None);
        assert_eq!(s.actors["agent"], 1);
        assert_eq!(s.actors["person"], 1);
        assert_eq!(s.agents["codex"], 1);
        assert!(render(&s).contains("actors: agent 1, person 1 (codex 1)"));
    }

    #[test]
    fn durations_are_short() {
        assert_eq!(duration(250), "250ms");
        assert_eq!(duration(3200), "3.2s");
        assert_eq!(duration(125_000), "2m05s");
        assert_eq!(duration(3_720_000), "1h02m");
    }
}
