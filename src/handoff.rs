//! `reeve handoff`: writes a handoff file that lists the session's named workers, and starts the
//! successor as a named worker whose launch prompt points at that file.

use crate::workers::{self, Agent, Backend, Kind, Registry, State, Worker};
use std::fs;
use std::io;
use std::path::Path;

pub fn doc(notes: &str, workers: &[(Worker, State)]) -> String {
    let mut d = notes.trim_end().to_string();
    d += "\n\n## Workers\n\n";
    if workers.is_empty() {
        d += "No named workers.\n";
        return d;
    }
    for (w, s) in workers {
        d += &format!("- {}: {s}, report at {}\n", w.name, w.report.display());
    }
    d += "\nThey keep running without the previous session. `reeve ls` shows them, `reeve wait NAME` \
          blocks until one has written its report and `reeve report NAME` prints it. Don't restart \
          a worker that is still running.\n";
    d
}

pub fn successor_prompt(file: &Path) -> String {
    format!(
        "You take over from a previous session that handed off. Read {} and continue the work \
         from there.",
        file.display()
    )
}

/// The named worker whose Herdr pane is `pane`: the session that is handing off, if reeve
/// started it. Sessions the user started by hand are not reeve's to close.
pub fn predecessor<'a>(all: &'a [Worker], pane: Option<&str>) -> Option<&'a Worker> {
    let pane = pane?;
    all.iter()
        .find(|w| w.backend == Backend::Pane && w.handle == pane)
}

/// What a predecessor's tab shows: `Err` says why it must stay open. `text` is the pane read;
/// the handoff shows in it as the `reeve handoff --name NEXT` command the predecessor ran.
pub fn tab_verdict(text: &str, next: &str) -> Result<(), String> {
    let lines: Vec<&str> = text.lines().collect();
    let is_prompt = |l: &&str| l.starts_with('❯') || l.starts_with('›');
    // The input box is the last prompt line, with the indented lines under it.
    let Some(at) = lines.iter().rposition(is_prompt) else {
        return Err("has a tab reeve cannot read: no input box in it".into());
    };
    let handoff = format!("handoff --name {next}");
    let ran = lines[..at].iter().rposition(|l| {
        l.split_once(&handoff).is_some_and(|(_, rest)| {
            !rest.starts_with(|c: char| c.is_alphanumeric() || "-_".contains(c))
        })
    });
    let Some(ran) = ran else {
        return Err(format!(
            "has a tab that no longer shows the handoff to {next}, so a new message can't be ruled out"
        ));
    };
    if let Some(msg) = lines[ran + 1..at].iter().copied().find(is_prompt) {
        return Err(format!("got a new message: {}", quoted(msg)));
    }
    let rest = lines[at + 1..]
        .iter()
        .take_while(|l| l.starts_with(' ') && !l.trim().is_empty() && !l.trim().starts_with('─'));
    let input = [&lines[at][lines[at].chars().next().map_or(0, char::len_utf8)..]]
        .into_iter()
        .chain(rest.copied().map(str::trim))
        .collect::<Vec<_>>()
        .join(" ");
    match input.trim() {
        "" => Ok(()),
        typed => Err(format!("has unsent input: {}", quoted(typed))),
    }
}

/// `"text"` for a line from a tab, without its prompt mark and cut short.
fn quoted(line: &str) -> String {
    let text = line.trim_start_matches(['❯', '›']).trim();
    let mut short: String = text.chars().take(80).collect();
    if text.chars().count() > 80 {
        short += "…";
    }
    format!("\"{short}\"")
}

#[derive(Debug, PartialEq)]
pub enum Verdict {
    Close,
    /// Why it stays, as a continuation of the predecessor's name.
    Keep(String),
}

/// Whether a predecessor can be closed: its agent is idle (or already gone), it wrote its report,
/// and for a pane, `tab` (the pane read) shows no new message and no unsent input.
pub fn verdict(agent: Agent, report: bool, tab: Option<Result<&str, &str>>, next: &str) -> Verdict {
    let keep = |why: &str| Verdict::Keep(why.to_string());
    match (agent, report, tab) {
        (Agent::Absent, ..) => Verdict::Close,
        (Agent::Working, ..) => keep("is still working"),
        (Agent::Blocked, ..) => keep("is waiting for an answer in its tab"),
        (Agent::Unknown, ..) => keep("could not be checked"),
        (Agent::Idle, false, _) => keep("has no report yet"),
        (Agent::Idle, true, None) => Verdict::Close,
        (Agent::Idle, true, Some(Err(e))) => {
            Verdict::Keep(format!("has a tab reeve can't read: {e}"))
        }
        (Agent::Idle, true, Some(Ok(text))) => match tab_verdict(text, next) {
            Ok(()) => Verdict::Close,
            Err(why) => Verdict::Keep(why),
        },
    }
}

/// Checks each predecessor of a handoff that is not yet closed, and closes the ones that
/// `verdict` allows. Returns a line for what the caller should know: a close, or why not.
/// Run by `reeve ls`, which the session-start hook runs too, so the successor sees the lines;
/// a predecessor that stays is checked again by the next `ls`.
pub fn tidy(r: &Registry) -> Vec<String> {
    r.predecessors()
        .into_iter()
        .filter_map(|(next, from)| {
            let forget = || {
                let _ = fs::remove_file(r.predecessor_path(&next));
            };
            let Ok(w) = r.get(&from) else {
                forget();
                return None;
            };
            let a = workers::agent(&w);
            let tab = (w.backend == Backend::Pane && a == Agent::Idle)
                .then(|| workers::read_tab(&w.handle));
            let tab = tab.as_ref().map(|t| t.as_deref().map_err(String::as_str));
            match verdict(a, w.report.exists(), tab, &next) {
                Verdict::Keep(why) => Some(format!("{from} {why}")),
                Verdict::Close => match workers::close(r, &from) {
                    Ok(()) => {
                        forget();
                        Some(format!("closed {from}, which handed off to {next}"))
                    }
                    Err(e) => Some(format!("{from} could not be closed: {e}")),
                },
            }
        })
        .collect()
}

/// Writes the handoff for `next` and starts it; the calling session should stop afterwards.
/// `from` is the calling session when reeve started it: `tidy` closes it once it is done.
pub fn handoff(
    r: &Registry,
    next: &str,
    kind: Kind,
    backend: Backend,
    notes: &Path,
    from: Option<&str>,
    args: &[String],
) -> io::Result<()> {
    workers::check_name(next).map_err(io::Error::other)?;
    let notes = fs::read_to_string(notes)?;
    let all: Vec<(Worker, State)> = r
        .all()?
        .into_iter()
        .filter(|w| w.name != next)
        .map(|w| {
            let s = workers::state(&w);
            (w, s)
        })
        .collect();
    let file = r.handoff_path(next);
    fs::create_dir_all(file.parent().expect("handoffs dir"))?;
    fs::write(&file, doc(&notes, &all))?;
    let args: Vec<String> = args
        .iter()
        .cloned()
        .chain([successor_prompt(&file)])
        .collect();
    // Before the successor starts: its first `reeve ls` may come at once.
    if let Some(from) = from {
        fs::write(r.predecessor_path(next), from)?;
    }
    if let Err(e) = workers::run(r, next, kind, backend, &args) {
        let _ = fs::remove_file(r.predecessor_path(next));
        return Err(e);
    }
    println!("{}", file.display());
    eprintln!("reeve: handed off to {next}; stop now and leave the workers running");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(name: &str) -> Worker {
        Worker {
            name: name.into(),
            backend: Backend::Bg,
            agent: Kind::Claude,
            cwd: "/work/x".into(),
            started: 1,
            report: format!("/st/reports/{name}.md").into(),
            handle: "ab12".into(),
            starting: false,
            error: None,
        }
    }

    #[test]
    fn the_handoff_keeps_the_notes_and_lists_every_worker_with_its_report() {
        let d = doc(
            "Goal: ship it.\n",
            &[(w("fix-a"), State::Running), (w("fix-b"), State::Done)],
        );
        assert!(d.starts_with("Goal: ship it.\n"));
        assert!(d.contains("- fix-a: running, report at /st/reports/fix-a.md"));
        assert!(d.contains("- fix-b: done, report at /st/reports/fix-b.md"));
        assert!(d.contains("reeve wait"));
        assert!(d.contains("reeve report"));
    }

    #[test]
    fn a_handoff_without_workers_says_so() {
        assert!(doc("Goal.", &[]).contains("No named workers"));
    }

    /// A Claude tab after `reeve handoff`, with `input` in the box and `after` between.
    fn tab(after: &str, input: &str) -> String {
        format!(
            "⏺ Bash(reeve handoff --name next --notes n.md)\n  ⎿  /st/handoffs/next.md\n\n\
             ⏺ Handed off.\n{after}\n\
             ────────────────────────── old ─\n❯ {input}\n\
             ───────────────────────────────\n  old [Opus 5.5] ctx:9%\n"
        )
    }

    #[test]
    fn a_tab_with_nothing_new_and_an_empty_box_can_be_closed() {
        assert_eq!(tab_verdict(&tab("", ""), "next"), Ok(()));
    }

    #[test]
    fn unsent_input_keeps_the_tab_and_is_quoted() {
        let why = tab_verdict(&tab("", "start a worker on the reeve close fix"), "next");
        assert_eq!(
            why,
            Err("has unsent input: \"start a worker on the reeve close fix\"".into())
        );
        let long = "x".repeat(100);
        let why = tab_verdict(&tab("", &long), "next").unwrap_err();
        assert!(why.ends_with(&format!("\"{}…\"", "x".repeat(80))), "{why}");
        let multi = tab("", "first line\n  second line");
        assert_eq!(
            tab_verdict(&multi, "next"),
            Err("has unsent input: \"first line second line\"".into())
        );
    }

    #[test]
    fn a_message_after_the_handoff_keeps_the_tab_and_is_quoted() {
        let after = "\n❯ also check the docs\n\n⏺ Sure.\n";
        assert_eq!(
            tab_verdict(&tab(after, ""), "next"),
            Err("got a new message: \"also check the docs\"".into())
        );
        // A message before the handoff is old news.
        let before = format!("❯ hello\n⏺ hi\n{}", tab("", ""));
        assert_eq!(tab_verdict(&before, "next"), Ok(()));
    }

    #[test]
    fn a_tab_that_shows_no_handoff_to_this_successor_or_no_box_is_not_closed() {
        let other = tab("", "").replace("--name next", "--name next-2");
        assert!(
            tab_verdict(&other, "next")
                .unwrap_err()
                .contains("handoff to next")
        );
        let scrolled = "⏺ Handed off.\n❯ \n";
        assert!(
            tab_verdict(scrolled, "next")
                .unwrap_err()
                .contains("no longer shows")
        );
        let no_box = "⏺ Bash(reeve handoff --name next)\n";
        assert!(
            tab_verdict(no_box, "next")
                .unwrap_err()
                .contains("no input box")
        );
    }

    #[test]
    fn a_predecessor_closes_only_when_idle_with_a_report_and_a_quiet_tab() {
        let quiet = tab("", "");
        let typed = tab("", "hi");
        let go = |a, report, t: Option<Result<&str, &str>>| verdict(a, report, t, "next");
        let keep = |why: &str| Verdict::Keep(why.into());
        assert_eq!(go(Agent::Idle, true, Some(Ok(&quiet))), Verdict::Close);
        assert_eq!(go(Agent::Idle, true, None), Verdict::Close);
        assert_eq!(go(Agent::Absent, false, None), Verdict::Close);
        assert_eq!(go(Agent::Working, true, None), keep("is still working"));
        assert!(matches!(go(Agent::Blocked, true, None), Verdict::Keep(_)));
        assert!(matches!(go(Agent::Unknown, true, None), Verdict::Keep(_)));
        assert_eq!(
            go(Agent::Idle, false, Some(Ok(&quiet))),
            keep("has no report yet")
        );
        assert_eq!(
            go(Agent::Idle, true, Some(Ok(&typed))),
            keep("has unsent input: \"hi\"")
        );
        let Verdict::Keep(why) = go(Agent::Idle, true, Some(Err("boom"))) else {
            panic!("closed a tab it could not read");
        };
        assert!(why.contains("boom"), "{why}");
    }

    #[test]
    fn the_predecessor_is_the_worker_in_the_callers_pane() {
        let mut old = w("old");
        old.backend = Backend::Pane;
        old.handle = "w1:p2".into();
        let bg = w("bg");
        let all = [bg, old];
        assert_eq!(
            predecessor(&all, Some("w1:p2")).map(|w| &w.name[..]),
            Some("old")
        );
        assert_eq!(predecessor(&all, Some("w1:p9")), None);
        assert_eq!(predecessor(&all, None), None);
    }

    #[test]
    fn the_successor_is_told_to_read_the_file_in_one_line() {
        let p = successor_prompt(Path::new("/st/handoffs/next.md"));
        assert!(p.contains("/st/handoffs/next.md"));
        assert!(!p.chars().any(char::is_control));
    }
}
