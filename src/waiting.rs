//! `reeve waiting`: what waits on the user, worked out from reeve's live records each time it
//! runs: blocked workers and recent partial, failed or lost work. It stores nothing, so there
//! is no list to keep up or prune, and its output has a hard cap however many workers there
//! are.

use crate::workers::{self, Registry, State};
use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::time::{Duration, SystemTime};

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
    /// Closed with `reeve close`: whatever it left is settled.
    pub dismissed: bool,
    /// The session it took over from with `reeve handoff`.
    pub predecessor: Option<String>,
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

/// A session that handed off waits on nothing while its successor is listed: the successor is
/// the one to watch, and shows up here itself if it is blocked, failed or gone.
fn handed_off(w: &Seen, workers: &[Seen]) -> bool {
    workers.iter().any(|o| {
        o.predecessor.as_deref() == Some(&w.name)
            || w.status == "partial" && crate::handoff::handed_off_to(&w.summary) == Some(&o.name)
    })
}

/// The three lists every view shows, newest first: what waits on the user, each with what to
/// say after its name; what runs; and what finished in the last two days.
struct Lists<'a> {
    waiting: Vec<(&'a Seen, String)>,
    running: Vec<&'a Seen>,
    done: Vec<&'a Seen>,
}

fn lists(workers: &[Seen]) -> Lists<'_> {
    let recent = |w: &Seen| w.age < WINDOW_SECS;
    let mut l = Lists {
        waiting: Vec::new(),
        running: Vec::new(),
        done: Vec::new(),
    };
    for w in workers {
        let when = workers::age(w.age);
        let line = match w.state {
            State::Blocked => format!("blocked, {when}: answer it {}", w.answer),
            State::Starting | State::Running | State::Idle | State::Unknown => {
                l.running.push(w);
                continue;
            }
            State::Done | State::Gone if handed_off(w, workers) => continue,
            State::Done if recent(w) && w.dismissed => {
                l.done.push(w);
                continue;
            }
            State::Done if recent(w) && (w.status == "partial" || w.status == "failed") => {
                format!("{}, {when}: {}", w.status, w.summary)
            }
            State::Done if recent(w) => {
                l.done.push(w);
                continue;
            }
            State::Gone if recent(w) && !w.dismissed => format!("gone without a report, {when}"),
            _ => continue,
        };
        l.waiting.push((w, line));
    }
    l.waiting.sort_by_key(|(w, _)| w.age);
    l.running.sort_by_key(|w| w.age);
    l.done.sort_by_key(|w| w.age);
    l
}

/// What waits on the user, newest first, or nothing when nothing does.
pub fn waiting_section(workers: &[Seen]) -> String {
    let entries = lists(workers).waiting;
    if entries.is_empty() {
        return String::new();
    }
    let mut out = format!("Waiting on you ({}):", entries.len());
    for (w, line) in entries.iter().take(LINES) {
        out += &format!("\n- {}", clip(&format!("{} {line}", w.name)));
    }
    if let Some((oldest, _)) = entries.last().filter(|_| entries.len() > LINES) {
        out += &format!(
            "\n- {} more, the oldest {} old",
            entries.len() - LINES,
            workers::age(oldest.age)
        );
    }
    out
}

/// The full view: what waits on the user, then what runs, then what finished lately.
pub fn render(workers: &[Seen]) -> String {
    let Lists { running, done, .. } = lists(workers);
    let mut parts = vec![waiting_section(workers)];
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

/// What a running or finished worker's entry says after its name.
fn running_line(w: &Seen) -> String {
    match w.state {
        State::Running => String::new(),
        s => s.to_string(),
    }
}

fn done_line(w: &Seen) -> String {
    let when = workers::age(w.age);
    match (w.status.as_str(), w.summary.as_str()) {
        ("", _) => when,
        (s, "") => format!("{s}, {when}"),
        (s, summary) => format!("{s}, {when}: {summary}"),
    }
}

/// The three lists for a script: `waiting`, `running` and `done`, each entry with its `name`,
/// `state`, `secs` since its report or start, and the `line` the views show after the name.
pub fn json(workers: &[Seen]) -> serde_json::Value {
    let item = |w: &Seen, line: String| serde_json::json!({ "name": w.name, "state": w.state.to_string(), "secs": w.age, "line": line });
    let l = lists(workers);
    serde_json::json!({
        "waiting": l.waiting.into_iter().map(|(w, line)| item(w, line)).collect::<Vec<_>>(),
        "running": l.running.into_iter().map(|w| item(w, running_line(w))).collect::<Vec<_>>(),
        "done": l.done.into_iter().map(|w| item(w, done_line(w))).collect::<Vec<_>>(),
    })
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// How often `--follow` looks again, and the page reloads itself.
const PAGE_EVERY: Duration = Duration::from_secs(15);

/// The view as a page that reloads itself, for `--html`.
pub fn html(workers: &[Seen]) -> String {
    let l = lists(workers);
    let section = |title: &str, items: Vec<(&Seen, String)>| {
        if items.is_empty() {
            return String::new();
        }
        let lis: String = items
            .iter()
            .map(|(w, line)| format!("<li><b>{}</b> {}</li>\n", escape(&w.name), escape(line)))
            .collect();
        format!("<h2>{title} ({})</h2>\n<ul>\n{lis}</ul>\n", items.len())
    };
    let mut body = [
        section("Waiting on you", l.waiting),
        section(
            "Running",
            l.running
                .into_iter()
                .map(|w| (w, running_line(w)))
                .collect(),
        ),
        section(
            "Done in the last 2 days",
            l.done.into_iter().map(|w| (w, done_line(w))).collect(),
        ),
    ]
    .concat();
    if body.is_empty() {
        body = "<p>Nothing waits on you, and no worker runs.</p>\n".into();
    }
    format!(
        "<!doctype html>\n<meta charset=\"utf-8\">\n<meta http-equiv=\"refresh\" content=\"{}\">\n\
         <title>reeve waiting</title>\n\
         <style>body{{font:15px/1.5 system-ui,sans-serif;max-width:60rem;margin:2rem auto;padding:0 1rem}}</style>\n\
         {body}",
        PAGE_EVERY.as_secs()
    )
}

/// Writes the page to `path`; with `follow` it keeps looking and rewrites it when it changes.
pub fn page(r: &Registry, path: &Path, follow: bool, rounds: Option<u32>) -> io::Result<i32> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    let mut last = String::new();
    let mut done = 0;
    loop {
        let page = html(&collect(r)?);
        if page != last {
            let tmp = path.with_extension("tmp");
            fs::write(&tmp, &page)?;
            fs::rename(&tmp, path)?;
            last = page;
        }
        done += 1;
        if !follow || rounds.is_some_and(|n| done >= n) {
            return Ok(0);
        }
        std::thread::sleep(PAGE_EVERY);
    }
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
                dismissed: w.dismissed,
                predecessor: w.predecessor.clone(),
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
            dismissed: false,
            predecessor: None,
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

    const NOTE: &str = "partial\nHanded off to next-2, which carries on and reports when done.\n\nHandoff notes: /x.md\n";

    #[test]
    fn a_session_that_handed_off_does_not_wait_while_its_successor_is_listed() {
        let out = render(&[
            seen("old", State::Done, 60, NOTE),
            seen("next-2", State::Running, 30, ""),
        ]);
        assert_eq!(out, "Running (1): next-2");
    }

    #[test]
    fn a_failed_or_gone_successor_waits_as_its_own_entry() {
        let out = render(&[
            seen("old", State::Done, 60, NOTE),
            seen("next-2", State::Gone, 30, ""),
        ]);
        assert_eq!(
            out,
            "Waiting on you (1):\n- next-2 gone without a report, 30s"
        );
        let out = render(&[
            seen("old", State::Done, 60, NOTE),
            seen("next-2", State::Done, 30, "failed\nno disk"),
        ]);
        assert_eq!(out, "Waiting on you (1):\n- next-2 failed, 30s: no disk");
    }

    fn mixed() -> Vec<Seen> {
        vec![
            seen("fix-a", State::Blocked, 120, ""),
            seen("pr-<b>", State::Done, 600, "partial\nOpen it & <merge>."),
            seen("d", State::Running, 60, ""),
            seen("e", State::Idle, 30, ""),
            seen("f", State::Done, 60, "done\nMerged."),
        ]
    }

    #[test]
    fn the_json_holds_the_same_three_lists_newest_first() {
        let v = json(&mixed());
        assert_eq!(v["waiting"][0]["name"], "fix-a");
        assert_eq!(v["waiting"][0]["line"], "blocked, 2m: answer it in its tab");
        assert_eq!(v["waiting"][1]["name"], "pr-<b>");
        assert_eq!(v["waiting"][1]["secs"], 600);
        assert_eq!(v["running"][0]["name"], "e");
        assert_eq!(v["running"][0]["state"], "idle");
        assert_eq!(v["running"][1]["state"], "running");
        assert_eq!(v["done"][0]["name"], "f");
        assert_eq!(v["done"][0]["line"], "done, 1m: Merged.");
    }

    #[test]
    fn the_page_shows_the_lists_escaped_and_reloads_itself() {
        let page = html(&mixed());
        assert!(page.contains(r#"<meta http-equiv="refresh""#), "{page}");
        for part in [
            "<h2>Waiting on you (2)</h2>",
            "<h2>Running (2)</h2>",
            "<h2>Done in the last 2 days (1)</h2>",
            "<b>pr-&lt;b&gt;</b> partial, 10m: Open it &amp; &lt;merge&gt;.",
            "<b>e</b> idle",
        ] {
            assert!(page.contains(part), "{part}\n{page}");
        }
        assert!(!page.contains("<merge>"));
        assert!(html(&[]).contains("Nothing waits on you"));
    }

    #[test]
    fn a_session_whose_successor_names_it_does_not_wait_whatever_its_report_says() {
        let mut next = seen("next-2", State::Running, 30, "");
        next.predecessor = Some("old".into());
        let out = render(&[
            seen("old", State::Done, 60, "partial\nI ran past the threshold."),
            next,
        ]);
        assert_eq!(out, "Running (1): next-2");
    }

    #[test]
    fn dismissed_work_does_not_wait_and_finished_work_still_shows_as_done() {
        let ws: Vec<Seen> = [
            seen("a", State::Done, 60, "partial\nPR text in the report."),
            seen("b", State::Gone, 60, ""),
            seen("c", State::Done, 60, "failed\nno disk"),
        ]
        .into_iter()
        .map(|mut w| {
            w.dismissed = true;
            w
        })
        .collect();
        assert_eq!(
            render(&ws),
            "Done in the last 2 days (2): a, c. `reeve report NAME` prints a report."
        );
    }

    #[test]
    fn a_handoff_whose_successor_is_not_listed_still_waits() {
        let out = waiting_section(&[seen("old", State::Done, 60, NOTE)]);
        assert!(
            out.contains("old partial, 1m: Handed off to next-2"),
            "{out}"
        );
    }

    #[test]
    fn the_note_the_handoff_writes_is_the_one_waiting_recognises() {
        let note = crate::handoff::note("n-2", Path::new("/x.md"));
        let (status, summary) = parse_report(&note);
        assert_eq!(status, "partial");
        assert_eq!(crate::handoff::handed_off_to(&summary), Some("n-2"));
        assert_eq!(crate::handoff::handed_off_to("The PR needs opening."), None);
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
