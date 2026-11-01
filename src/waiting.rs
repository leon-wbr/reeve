//! `reeve waiting`: what waits on the user, worked out from reeve's live records each time it
//! runs: blocked workers and recent partial, failed or lost work. It stores nothing, so there
//! is no list to keep up or prune, and its output has a hard cap however many workers there
//! are.

use crate::workers::{self, Registry, State};
use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::time::SystemTime;

/// Finished work older than this has stopped waiting: a report or a gone worker counts for two
/// days, then drops out, so old work never piles up here.
pub const WINDOW_SECS: u64 = 2 * 86400;
/// Lines in the "Waiting on you" list before the rest become one "N more" line.
const LINES: usize = 8;
/// Names in the "Running" and "Done" lists before the rest become "+N more".
const NAMES: usize = 12;
/// Characters per entry before it is cut with an ellipsis.
const LINE_MAX: usize = 160;
/// The whole output, far below the 10,000 characters Claude Code takes from a hook.
pub const MAX: usize = 2500;

/// A named worker as `reeve ls` sees it, with the start of its report if it wrote one.
pub struct Seen {
    pub name: String,
    pub state: State,
    /// Seconds since its report was written or, without one, since it started.
    pub age: u64,
    /// `done`, `partial` or `failed` from the report's first line; empty if it says none.
    pub status: String,
    /// The report's first line after the status.
    pub summary: String,
    /// Where to answer a blocked worker, e.g. "in its tab".
    pub answer: String,
}

/// The status line and the first line after it, from the start of a report.
pub fn parse_report(text: &str) -> (String, String) {
    let mut lines = text
        .lines()
        .map(|l| l.trim_matches(|c: char| c.is_whitespace() || "#*`>-_".contains(c)))
        .filter(|l| !l.is_empty());
    let first = lines.next().unwrap_or("").to_lowercase();
    let word = first
        .split(|c: char| !c.is_alphabetic())
        .next()
        .unwrap_or("");
    if !["done", "partial", "failed"].contains(&word) {
        return (String::new(), String::new());
    }
    (word.to_string(), lines.next().unwrap_or("").to_string())
}

fn clip(s: &str) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.len() <= LINE_MAX {
        return s;
    }
    format!("{}…", &s[..s.floor_char_boundary(LINE_MAX - 1)])
}

fn names(mut ws: Vec<&Seen>, label: impl Fn(&Seen) -> String) -> String {
    ws.sort_by_key(|w| w.age);
    let mut out: Vec<String> = ws.iter().take(NAMES).map(|w| label(w)).collect();
    if ws.len() > NAMES {
        out.push(format!("+{} more", ws.len() - NAMES));
    }
    out.join(", ")
}

/// What waits on the user, newest first, or nothing when nothing does.
pub fn waiting_section(workers: &[Seen]) -> String {
    let recent = |w: &Seen| w.age < WINDOW_SECS;
    let mut entries: Vec<(u64, String)> = Vec::new();
    for w in workers {
        let when = workers::age(w.age);
        let line = match w.state {
            State::Blocked => format!("{} blocked, {when}: answer it {}", w.name, w.answer),
            State::Done if recent(w) && (w.status == "partial" || w.status == "failed") => {
                format!("{} {}, {when}: {}", w.name, w.status, w.summary)
            }
            State::Gone if recent(w) => format!("{} gone without a report, {when}", w.name),
            _ => continue,
        };
        entries.push((w.age, line));
    }
    if entries.is_empty() {
        return String::new();
    }
    entries.sort_by_key(|(age, _)| *age);
    let mut out = format!("Waiting on you ({}):", entries.len());
    for (_, line) in entries.iter().take(LINES) {
        out += &format!("\n- {}", clip(line));
    }
    if let Some((oldest, _)) = entries.last().filter(|_| entries.len() > LINES) {
        out += &format!(
            "\n- {} more, the oldest {} old",
            entries.len() - LINES,
            workers::age(*oldest)
        );
    }
    out
}

/// The full view: what waits on the user, then what runs, then what finished lately.
pub fn render(workers: &[Seen]) -> String {
    let mut parts = vec![waiting_section(workers)];
    let running: Vec<&Seen> = workers
        .iter()
        .filter(|w| {
            matches!(
                w.state,
                State::Starting | State::Running | State::Idle | State::Unknown
            )
        })
        .collect();
    if !running.is_empty() {
        let n = running.len();
        parts.push(format!(
            "Running ({n}): {}",
            names(running, |w| match w.state {
                State::Running => clip(&w.name),
                s => format!("{} {s}", clip(&w.name)),
            })
        ));
    }
    let done: Vec<&Seen> = workers
        .iter()
        .filter(|w| w.state == State::Done && w.age < WINDOW_SECS)
        .filter(|w| w.status != "partial" && w.status != "failed")
        .collect();
    if !done.is_empty() {
        let n = done.len();
        parts.push(format!(
            "Done in the last 2 days ({n}): {}. `reeve report NAME` prints a report.",
            names(done, |w| clip(&w.name))
        ));
    }
    let out = parts
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if out.len() <= MAX {
        return out;
    }
    format!("{}…", &out[..out.floor_char_boundary(MAX - 3)])
}

/// The first few KB of a report, enough for its status and summary.
fn report_head(path: &Path) -> Option<(String, u64)> {
    let f = fs::File::open(path).ok()?;
    let modified = f.metadata().ok()?.modified().ok()?;
    let mut head = String::new();
    f.take(4096).read_to_string(&mut head).ok()?;
    let age = SystemTime::now()
        .duration_since(modified)
        .map_or(0, |d| d.as_secs());
    Some((head, age))
}

/// The named workers, read now.
pub fn collect(r: &Registry) -> io::Result<Vec<Seen>> {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let all = r.all()?;
    Ok(all
        .iter()
        .zip(workers::states(&all))
        .map(|(w, (state, _))| {
            let head = report_head(&w.report);
            let (status, summary) = head
                .as_ref()
                .map(|(t, _)| parse_report(t))
                .unwrap_or_default();
            Seen {
                name: w.name.clone(),
                state,
                age: head.map_or(now.saturating_sub(w.started), |(_, age)| age),
                status,
                summary,
                answer: workers::answer_where(w),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(name: &str, state: State, age: u64, report: &str) -> Seen {
        let (status, summary) = parse_report(report);
        Seen {
            name: name.into(),
            state,
            age,
            status,
            summary,
            answer: "in its tab".into(),
        }
    }

    #[test]
    fn a_report_says_its_status_on_its_first_line() {
        assert_eq!(
            parse_report("partial\n\nThe PR needs opening.\nMore."),
            ("partial".into(), "The PR needs opening.".into())
        );
        assert_eq!(
            parse_report("# Done — fixed it\n\n## What I did\n"),
            ("done".into(), "What I did".into())
        );
        assert_eq!(parse_report("**failed**\n"), ("failed".into(), "".into()));
        assert_eq!(parse_report("Here is my report\n"), ("".into(), "".into()));
        assert_eq!(parse_report(""), ("".into(), "".into()));
    }

    #[test]
    fn nothing_waiting_prints_nothing() {
        assert_eq!(render(&[]), "");
        assert_eq!(waiting_section(&[seen("a", State::Running, 60, "")]), "");
    }

    #[test]
    fn blocked_workers_and_unfinished_or_lost_work_wait_on_the_user() {
        let out = render(&[
            seen("fix-a", State::Blocked, 120, ""),
            seen("pr-b", State::Done, 600, "partial\nThe PR needs opening."),
            seen("c", State::Gone, 60, ""),
            seen("d", State::Running, 60, ""),
            seen("e", State::Idle, 60, ""),
            seen("f", State::Done, 60, "done\nMerged."),
        ]);
        assert_eq!(
            out,
            "Waiting on you (3):\n\
             - c gone without a report, 1m\n\
             - fix-a blocked, 2m: answer it in its tab\n\
             - pr-b partial, 10m: The PR needs opening.\n\
             Running (2): d, e idle\n\
             Done in the last 2 days (1): f. `reeve report NAME` prints a report."
        );
    }

    #[test]
    fn finished_work_drops_out_after_two_days_but_live_waits_stay() {
        let old = WINDOW_SECS + 1;
        let out = render(&[
            seen("a", State::Done, old, "partial\nx"),
            seen("b", State::Gone, old, ""),
            seen("c", State::Done, old, "done\ny"),
            seen("d", State::Blocked, 30 * 86400, ""),
        ]);
        assert_eq!(
            out,
            "Waiting on you (1):\n- d blocked, 30d: answer it in its tab"
        );
    }

    /// The view can't grow without limit: whatever piles up in reeve's records, the output
    /// keeps the same small bound in characters and in lines.
    #[test]
    fn the_view_is_bounded_however_much_piles_up() {
        let long = "x".repeat(5000);
        for n in [10, 1000, 20_000] {
            let states = [
                State::Blocked,
                State::Done,
                State::Gone,
                State::Running,
                State::Idle,
                State::Starting,
                State::Unknown,
            ];
            let ws: Vec<Seen> = (0..n)
                .map(|i| {
                    let s = states[i as usize % states.len()];
                    let report = if i % 2 == 0 { "partial\n" } else { "done\n" };
                    seen(&format!("{i}{long}"), s, i, &format!("{report}{long}"))
                })
                .collect();
            let out = render(&ws);
            assert!(out.len() <= MAX, "{n}: {} chars", out.len());
            assert!(out.lines().count() <= LINES + 5, "{n}: {out}");
            assert!(out.starts_with("Waiting on you ("), "{out}");
        }
    }

    #[test]
    fn more_than_fit_are_counted_with_the_oldest_age() {
        let ws: Vec<Seen> = (0..10)
            .map(|i| seen(&format!("w{i}"), State::Blocked, i * 60, ""))
            .collect();
        let out = waiting_section(&ws);
        assert!(
            out.contains("- w7 blocked, 7m: answer it in its tab\n- 2 more, the oldest 9m old"),
            "{out}"
        );
    }
}
