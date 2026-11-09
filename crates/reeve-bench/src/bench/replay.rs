//! `reeve-bench heavy-replay`: replays the heavy runs in reeve's event log through the queue's
//! own rule ([`reeve::sched`]), to compare first come, first served with express slots on the
//! same arrivals. Each run arrives when it did (`ts`, when `reeve heavy` started) and takes as
//! long as it did (`heavy_run_ms`); what a run's label is expected to take comes from the runs
//! that had really finished by then. The slot count is fixed and memory isn't modelled, so the
//! figures compare policies on one machine's arrivals rather than predict a wait.

use reeve::events::Event;
use reeve::sched::{self, History, Room, Turn};
use std::fmt::Write as _;

#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub label: String,
    pub submit: u64,
    pub run: u64,
    /// When it really finished, for the history later runs are judged by.
    pub finish: u64,
    /// How long it really waited.
    pub wait: u64,
}

/// Heavy runs that ran their command, in the order they arrived.
pub fn jobs(events: &[Event]) -> Vec<Job> {
    let mut v: Vec<Job> = events
        .iter()
        .filter(|e| e.command == "heavy")
        .filter_map(|e| {
            let run = e.heavy_run_ms?;
            let wait = e.heavy_wait_ms.unwrap_or(0);
            Some(Job {
                label: e.label.clone().unwrap_or_default(),
                submit: e.ts,
                run,
                finish: e.ts + wait + run,
                wait,
            })
        })
        .collect();
    v.sort_by_key(|j| j.submit);
    v
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Stats {
    pub jobs: usize,
    /// Runs expected to be short when they arrived.
    pub short: usize,
    pub wait_ms: u64,
    pub short_wait_ms: u64,
    pub p50_ms: u64,
    pub p90_ms: u64,
    /// The longest wait of a run not expected to be short: what express slots could starve.
    pub other_max_ms: u64,
}

/// Replays the runs that arrived at or after `from` through `room`. With `room.express` at 0
/// this is the queue before express slots: first come, first served.
pub fn replay(all: &[Job], from: u64, room: Room) -> Stats {
    let win: Vec<&Job> = all.iter().filter(|j| j.submit >= from).collect();
    let short = expectations(all, &win);
    let mut waits = vec![0u64; win.len()];
    let mut pending: Vec<usize> = vec![];
    // (start, end, expected short)
    let mut running: Vec<(u64, u64, bool)> = vec![];
    let (mut next, mut now) = (0, win.first().map_or(0, |j| j.submit));
    while next < win.len() || !pending.is_empty() {
        running.retain(|r| r.1 > now);
        while next < win.len() && win[next].submit <= now {
            pending.push(next);
            next += 1;
        }
        'place: loop {
            let flags: Vec<bool> = running
                .iter()
                .map(|r| r.2 && now - r.0 < sched::OVERRUN_MS)
                .collect();
            for p in 0..pending.len() {
                let ahead: Vec<bool> = pending[..p].iter().map(|&k| short[k]).collect();
                if room.turn(&ahead, short[pending[p]], &flags) == Turn::Go {
                    let k = pending.remove(p);
                    waits[k] = now - win[k].submit;
                    running.push((now, now + win[k].run, short[k]));
                    continue 'place;
                }
            }
            break;
        }
        let ends = running.iter().map(|r| r.1);
        let arrives = win.get(next).map(|j| j.submit);
        match ends.chain(arrives).filter(|&t| t > now).min() {
            Some(t) => now = t,
            None => break,
        }
    }
    let mut sorted = waits.clone();
    sorted.sort_unstable();
    let at = |q: f64| {
        sorted
            .get(((sorted.len() as f64 * q) as usize).min(sorted.len().saturating_sub(1)))
            .copied()
            .unwrap_or(0)
    };
    let pick = |want: bool| waits.iter().zip(&short).filter(move |(_, s)| **s == want);
    Stats {
        jobs: win.len(),
        short: short.iter().filter(|s| **s).count(),
        wait_ms: waits.iter().sum(),
        short_wait_ms: pick(true).map(|(w, _)| w).sum(),
        p50_ms: at(0.5),
        p90_ms: at(0.9),
        other_max_ms: pick(false).map(|(w, _)| *w).max().unwrap_or(0),
    }
}

/// For each run in `win`, whether its label was expected to be short when it arrived, judged by
/// the runs in `all` that had finished by then.
fn expectations(all: &[Job], win: &[&Job]) -> Vec<bool> {
    let mut done: Vec<&Job> = all.iter().collect();
    done.sort_by_key(|j| j.finish);
    let mut h = History::default();
    let mut d = 0;
    win.iter()
        .map(|j| {
            while d < done.len() && done[d].finish <= j.submit {
                h.push(&done[d].label, done[d].run);
                d += 1;
            }
            h.expect(&j.label).is_some_and(|e| e.short())
        })
        .collect()
}

/// The report: the runs as they were, then each slot count first come, first served and with
/// `express` express slots.
pub fn report(all: &[Job], from: u64, days: u64, slots: &[usize], express: usize) -> String {
    let win: Vec<&Job> = all.iter().filter(|j| j.submit >= from).collect();
    let min = |ms: u64| ms as f64 / 60_000.0;
    let mut s = format!(
        "{} heavy runs in the last {days} days: {:.0} min waited, {:.0} min run, as measured\n\n",
        win.len(),
        min(win.iter().map(|j| j.wait).sum()),
        min(win.iter().map(|j| j.run).sum()),
    );
    s += "| Slots | Express | Waited, min | vs FIFO | p50 wait, s | p90 wait, min | Short runs | Their wait, min | Longest wait of the rest, min |\n";
    s += "|---|---|---|---|---|---|---|---|---|\n";
    for &n in slots {
        let fifo = replay(
            all,
            from,
            Room {
                slots: n,
                express: 0,
            },
        );
        let mut rows = vec![(0, fifo.clone(), "FIFO".to_string())];
        if express > 0 {
            let r = replay(all, from, Room { slots: n, express });
            let vs = (r.wait_ms as f64 / fifo.wait_ms.max(1) as f64 - 1.0) * 100.0;
            rows.push((express, r, format!("{vs:+.0}%")));
        }
        for (e, r, vs) in rows {
            let _ = writeln!(
                s,
                "| {n} | {e} | {:.0} | {vs} | {:.0} | {:.1} | {} | {:.0} | {:.1} |",
                min(r.wait_ms),
                r.p50_ms as f64 / 1000.0,
                min(r.p90_ms),
                r.short,
                min(r.short_wait_ms),
                min(r.other_max_ms),
            );
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(label: &str, submit: u64, run: u64) -> Job {
        Job {
            label: label.into(),
            submit,
            run,
            finish: submit + run,
            wait: 0,
        }
    }

    /// Three quick `fmt` runs long ago, then two half-hour e2e runs, then three `fmt` runs.
    fn day() -> Vec<Job> {
        let mut v = vec![
            job("fmt", 0, 5_000),
            job("fmt", 10_000, 5_000),
            job("fmt", 20_000, 5_000),
        ];
        v.push(job("e2e", 1_000_000, 1_800_000));
        v.push(job("e2e", 1_000_001, 1_800_000));
        for k in 0..3 {
            v.push(job("fmt", 1_000_100 + k, 5_000));
        }
        v
    }

    #[test]
    fn short_runs_wait_behind_long_ones_in_fifo_and_not_with_an_express_slot() {
        let fifo = replay(
            &day(),
            1_000_000,
            Room {
                slots: 2,
                express: 0,
            },
        );
        assert_eq!((fifo.jobs, fifo.short), (5, 3));
        assert!(fifo.short_wait_ms > 3 * 1_700_000, "{fifo:?}");
        let express = replay(
            &day(),
            1_000_000,
            Room {
                slots: 2,
                express: 1,
            },
        );
        assert_eq!(express.short, 3);
        // The three fmt runs take the express slot one after another, 5 s each.
        assert!(express.short_wait_ms < 20_000, "{express:?}");
        assert_eq!(express.other_max_ms, 0);
    }

    #[test]
    fn only_runs_finished_before_one_arrives_say_what_it_is_expected_to_take() {
        // The history runs end after the later ones arrive, so nothing is known yet.
        let v = vec![
            job("fmt", 0, 50_000),
            job("fmt", 1, 50_000),
            job("fmt", 2, 50_000),
            job("fmt", 3, 1_000),
        ];
        let r = replay(
            &v,
            3,
            Room {
                slots: 1,
                express: 1,
            },
        );
        assert_eq!(r.short, 0);
    }

    #[test]
    fn jobs_come_from_heavy_events_that_ran() {
        let ev = |command: &str, ts, run: Option<u64>| Event {
            ts,
            command: command.into(),
            label: Some("x".into()),
            heavy_wait_ms: Some(10),
            heavy_run_ms: run,
            ..Event::default()
        };
        let v = jobs(&[
            ev("heavy", 5, Some(100)),
            ev("ls", 1, Some(1)),
            ev("heavy", 2, None),
        ]);
        assert_eq!(
            v,
            vec![Job {
                label: "x".into(),
                submit: 5,
                run: 100,
                finish: 115,
                wait: 10
            }]
        );
    }

    #[test]
    fn the_report_compares_each_slot_count_with_fifo() {
        let out = report(&day(), 1_000_000, 7, &[2], 1);
        assert!(out.starts_with("5 heavy runs in the last 7 days"), "{out}");
        assert!(out.contains("| 2 | 0 |") && out.contains("FIFO"), "{out}");
        assert!(out.contains("| 2 | 1 |"), "{out}");
    }
}
