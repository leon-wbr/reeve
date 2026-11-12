//! Which `reeve heavy` waiter may start, and which runs are expected to be short. Nothing here
//! reads or writes a file: the queue calls it under its lock, and `reeve-bench heavy-replay`
//! calls it to replay the event log.
//!
//! A run is expected to be short when every one of its label's last few runs took at most
//! [`SHORT_MS`]. Short runs may use the express slots, on top of the ordinary ones, so a
//! one-minute check doesn't wait behind two half-hour e2e runs. The line itself stays first come,
//! first served: a waiter passes one ahead of it only while that one can't start anyway, so a
//! long run is never kept from a slot it could take, and no run starves.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Runs whose label's recent runs all took at most this long are short.
pub const SHORT_MS: u64 = 60_000;
/// A run counted as short that has run this long counts as long from then on, so a wrong guess
/// holds an express slot only briefly.
pub const OVERRUN_MS: u64 = 2 * SHORT_MS;
/// How many of a label's last runs are looked at.
const LABEL_RUNS: usize = 5;
/// How many of the last runs sharing the label's last word are looked at, when the label itself
/// has too few.
const WORD_RUNS: usize = 10;
/// Fewer runs than this say nothing.
const MIN_RUNS: usize = 3;

/// What a label's history says about how long its next run takes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Expect {
    /// The longest of the runs looked at, in ms.
    pub ms: u64,
    /// How many runs were looked at.
    pub runs: usize,
    /// The label's last word, when it was the runs sharing that word that were looked at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word: Option<String>,
}

impl Expect {
    pub fn short(&self) -> bool {
        self.ms <= SHORT_MS
    }
}

impl std::fmt::Display for Expect {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let whose = match &self.word {
            Some(w) => format!("the last {} `{w}` runs", self.runs),
            None => format!("its last {} runs", self.runs),
        };
        let took = secs(self.ms);
        if self.short() {
            write!(f, "short: {whose} took at most {took}")
        } else {
            write!(f, "not short: one of {whose} took {took}")
        }
    }
}

fn secs(ms: u64) -> String {
    match ms / 1000 {
        s if s < 120 => format!("{s}s"),
        s => format!("{}m", s / 60),
    }
}

/// The label's last word, with trailing digits dropped: `gate` for `pr12-gate2`, `e2e` for
/// `web:e2e`. Generic labels such as `e2e` or `install` are their own word.
pub fn word(label: &str) -> Option<String> {
    label
        .split(|c: char| !c.is_ascii_alphanumeric())
        .map(|w| w.trim_end_matches(|c: char| c.is_ascii_digit()))
        .rfind(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
}

/// Run times per label and per last word, oldest first.
#[derive(Debug, Default)]
pub struct History {
    by_label: HashMap<String, Vec<u64>>,
    by_word: HashMap<String, Vec<u64>>,
}

impl History {
    /// Adds a finished run; runs must be added in the order they finished.
    pub fn push(&mut self, label: &str, run_ms: u64) {
        self.by_label
            .entry(label.to_string())
            .or_default()
            .push(run_ms);
        if let Some(w) = word(label) {
            self.by_word.entry(w).or_default().push(run_ms);
        }
    }

    /// The label's own last runs if it has enough, else those sharing its last word, else none.
    pub fn expect(&self, label: &str) -> Option<Expect> {
        let last = |runs: Option<&Vec<u64>>, n: usize| {
            let runs = runs?;
            let tail = &runs[runs.len().saturating_sub(n)..];
            (tail.len() >= MIN_RUNS).then(|| (tail.iter().copied().max().unwrap_or(0), tail.len()))
        };
        if let Some((ms, runs)) = last(self.by_label.get(label), LABEL_RUNS) {
            return Some(Expect {
                ms,
                runs,
                word: None,
            });
        }
        let w = word(label)?;
        let (ms, runs) = last(self.by_word.get(&w), WORD_RUNS)?;
        Some(Expect {
            ms,
            runs,
            word: Some(w),
        })
    }
}

/// Whether a run counts as short now: expected short, and not yet past [`OVERRUN_MS`].
pub fn counts_short(expect: Option<&Expect>, ran_ms: u64) -> bool {
    expect.is_some_and(Expect::short) && ran_ms < OVERRUN_MS
}

/// How many runs may go at once: `slots` of any kind, and `express` more that only short runs
/// may use.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Room {
    pub slots: usize,
    pub express: usize,
}

impl Room {
    /// Whether a run that is short or not could start next to `running` (true for each running
    /// run that counts as short).
    pub fn fits(&self, short: bool, running: &[bool]) -> bool {
        let all = running.len();
        let long = running.iter().filter(|s| !**s).count();
        if short {
            all < self.slots + self.express
        } else {
            long < self.slots && all < self.slots + self.express
        }
    }

    /// Whether the waiter `me` may start, given the waiters ahead of it in line and the runs
    /// going.
    pub fn turn(&self, ahead: &[bool], me: bool, running: &[bool]) -> Turn {
        if ahead.iter().any(|s| self.fits(*s, running)) {
            Turn::Ahead
        } else if self.fits(me, running) {
            Turn::Go
        } else {
            Turn::Full
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Turn {
    /// It may start.
    Go,
    /// One ahead of it in line could start now, so that one goes first.
    Ahead,
    /// Nobody ahead could start, and neither can it.
    Full,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(runs: &[(&str, u64)]) -> History {
        let mut h = History::default();
        for (label, ms) in runs {
            h.push(label, *ms);
        }
        h
    }

    #[test]
    fn a_label_whose_last_runs_were_all_quick_is_short() {
        let h = history(&[
            ("fmt", 200_000),
            ("fmt", 5_000),
            ("fmt", 9_000),
            ("fmt", 7_000),
            ("fmt", 8_000),
            ("fmt", 6_000),
        ]);
        let e = h.expect("fmt").unwrap();
        // Only the last five count, so the slow run long ago doesn't.
        assert_eq!((e.ms, e.runs, e.word.as_deref()), (9_000, 5, None));
        assert!(e.short());
        assert_eq!(e.to_string(), "short: its last 5 runs took at most 9s");
    }

    #[test]
    fn one_slow_run_among_the_last_makes_a_label_not_short() {
        let h = history(&[("e2e", 30_000), ("e2e", 900_000), ("e2e", 40_000)]);
        let e = h.expect("e2e").unwrap();
        assert!(!e.short());
        assert_eq!(e.to_string(), "not short: one of its last 3 runs took 15m");
    }

    #[test]
    fn a_label_with_too_few_runs_falls_back_to_its_last_word_then_to_nothing() {
        let h = history(&[
            ("a-gate", 20_000),
            ("b-gate2", 30_000),
            ("c-gate", 10_000),
            ("new-gate", 1_000),
        ]);
        let e = h.expect("new-gate").unwrap();
        assert_eq!((e.ms, e.runs, e.word.as_deref()), (30_000, 4, Some("gate")));
        assert_eq!(
            e.to_string(),
            "short: the last 4 `gate` runs took at most 30s"
        );
        assert_eq!(h.expect("lint"), None);
        assert_eq!(history(&[("x", 1), ("x", 1)]).expect("x"), None);
    }

    #[test]
    fn the_last_word_drops_separators_trailing_digits_and_case() {
        assert_eq!(word("pr12-gate2").as_deref(), Some("gate"));
        assert_eq!(word("web:e2e").as_deref(), Some("e2e"));
        assert_eq!(word("e2e").as_deref(), Some("e2e"));
        assert_eq!(word("Build").as_deref(), Some("build"));
        assert_eq!(word("monitor-42").as_deref(), Some("monitor"));
        assert_eq!(word("--"), None);
    }

    #[test]
    fn a_short_run_counts_as_long_once_it_overruns() {
        let short = Expect {
            ms: 10_000,
            runs: 5,
            word: None,
        };
        let long = Expect {
            ms: 600_000,
            ..short.clone()
        };
        assert!(counts_short(Some(&short), 1_000));
        assert!(!counts_short(Some(&short), OVERRUN_MS));
        assert!(!counts_short(Some(&long), 0));
        assert!(!counts_short(None, 0));
    }

    const ROOM: Room = Room {
        slots: 2,
        express: 1,
    };

    #[test]
    fn short_runs_may_use_the_express_slot_and_long_ones_may_not() {
        let two_long = [false, false];
        assert!(!ROOM.fits(false, &two_long));
        assert!(ROOM.fits(true, &two_long));
        assert!(!ROOM.fits(true, &[false, false, true]));
        // Short runs in the ordinary slots leave them to a long one, up to the total.
        assert!(ROOM.fits(false, &[true, true]));
        assert!(!ROOM.fits(false, &[true, true, true]));
        let off = Room {
            slots: 2,
            express: 0,
        };
        assert!(!off.fits(true, &two_long));
    }

    #[test]
    fn a_short_waiter_passes_a_long_one_only_while_that_one_cannot_start() {
        let two_long = [false, false];
        assert_eq!(ROOM.turn(&[false], true, &two_long), Turn::Go);
        assert_eq!(ROOM.turn(&[false], false, &two_long), Turn::Full);
        // A slot free for the long one: it goes first, and the short one waits its turn.
        assert_eq!(ROOM.turn(&[false], true, &[false]), Turn::Ahead);
        assert_eq!(ROOM.turn(&[], false, &[false]), Turn::Go);
    }

    #[test]
    fn a_stream_of_short_runs_never_keeps_a_long_one_from_a_free_slot() {
        // A long run waits at the head while short ones keep arriving behind it.
        let mut running: Vec<bool> = vec![false, false];
        let mut started_long = false;
        for _ in 0..10 {
            if ROOM.turn(&[], false, &running) == Turn::Go {
                started_long = true;
                break;
            }
            if ROOM.turn(&[false], true, &running) == Turn::Go {
                running.push(true);
            }
            // The oldest run ends.
            running.remove(0);
        }
        assert!(started_long, "the long run never started: {running:?}");
    }

    #[test]
    fn with_express_off_the_line_is_plain_fifo() {
        let off = Room {
            slots: 1,
            express: 0,
        };
        assert_eq!(off.turn(&[], true, &[false]), Turn::Full);
        assert_eq!(off.turn(&[false], true, &[false]), Turn::Full);
        assert_eq!(off.turn(&[false], true, &[]), Turn::Ahead);
        assert_eq!(off.turn(&[], false, &[]), Turn::Go);
    }
}
