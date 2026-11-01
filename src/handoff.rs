//! `reeve handoff`: writes a handoff file that lists the session's named workers, and starts the
//! successor as a named worker whose launch prompt points at that file. A message the user sends
//! the predecessor after that goes to the successor's inbox, like any `reeve send`, and the
//! predecessor is closed.

use crate::setup;
use crate::workers::{self, Agent, Backend, Kind, Registry, State, Worker};
use serde_json::{Value, json};
use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

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
    workers::pane_worker(all, pane?)
}

/// What a predecessor's tab shows. `text` is the pane read; the handoff shows in it as the
/// `reeve handoff --name NEXT` command the predecessor ran, unless Claude Code folded that into
/// "Ran 1 shell command". Once the hook `forwards` the predecessor's messages to the successor,
/// it catches each one, so only the input box counts.
pub fn tab_verdict(text: &str, next: &str, forwards: bool) -> Verdict {
    let lines: Vec<&str> = text.lines().collect();
    let is_prompt = |l: &&str| l.starts_with('❯') || l.starts_with('›');
    // The input box is the last prompt line, with the indented lines under it.
    let Some(at) = lines.iter().rposition(is_prompt) else {
        return Verdict::Check("has a tab reeve cannot read: no input box in it".into());
    };
    let handoff = format!("handoff --name {next}");
    let ran = lines[..at].iter().rposition(|l| {
        l.split_once(&handoff).is_some_and(|(_, rest)| {
            !rest.starts_with(|c: char| c.is_alphanumeric() || "-_".contains(c))
        })
    });
    match ran {
        _ if forwards => {}
        None => {
            return Verdict::Check(format!(
                "has a tab that no longer shows the handoff to {next}, so a new message can't be ruled out"
            ));
        }
        Some(ran) => {
            if let Some(msg) = lines[ran + 1..at].iter().copied().find(is_prompt) {
                return Verdict::Check(format!("got a new message: {}", quoted(msg)));
            }
        }
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
        "" => Verdict::Close,
        typed => Verdict::Keep(format!("has unsent input: {}", quoted(typed))),
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
    /// Why it stays, as a continuation of the predecessor's name; reeve checks again later.
    Keep(String),
    /// Why it stays, when only reading its tab can tell whether it is done, which is for the
    /// successor to do.
    Check(String),
}

/// The line for a predecessor that stays: why, and what the successor should do about it.
pub fn stays(from: &str, v: &Verdict) -> Option<String> {
    match v {
        Verdict::Close => None,
        Verdict::Keep(why) => Some(format!("{from} {why}")),
        Verdict::Check(why) => Some(format!(
            "{from} {why}; read its tab and `reeve close {from}` if nothing new is there"
        )),
    }
}

/// Whether a predecessor can be closed: its agent is idle (or already gone), it wrote its report,
/// and for a pane, `tab` (the pane read) shows no new message and no unsent input. `forwards`
/// is as for `tab_verdict`; then no report is needed either, as the hook keeps every prompt
/// from the predecessor, so it could never write one.
pub fn verdict(
    agent: Agent,
    report: bool,
    tab: Option<Result<&str, &str>>,
    next: &str,
    forwards: bool,
) -> Verdict {
    let keep = |why: &str| Verdict::Keep(why.to_string());
    let check = |why: &str| Verdict::Check(why.to_string());
    match (agent, report || forwards, tab) {
        (Agent::Absent, ..) => Verdict::Close,
        (Agent::Working, ..) => keep("is still working"),
        (Agent::Blocked, ..) => keep("is waiting for an answer in its tab"),
        (Agent::Unknown, ..) => check("could not be checked"),
        (Agent::Idle, false, _) => check("has no report yet"),
        (Agent::Idle, true, None) => Verdict::Close,
        (Agent::Idle, true, Some(Err(e))) => {
            Verdict::Check(format!("has a tab reeve can't read: {e}"))
        }
        (Agent::Idle, true, Some(Ok(text))) => tab_verdict(text, next, forwards),
    }
}

/// Checks each predecessor of a handoff that is not yet closed, and closes the ones that
/// `verdict` allows. Returns a line for what the caller should know: a close, or why not.
/// Run by `reeve ls`, which the session-start hook runs too, so the successor sees the lines;
/// `follow`, which the handoff starts, closes a predecessor as soon as it is done.
pub fn tidy(r: &Registry) -> Vec<String> {
    r.predecessors()
        .into_iter()
        .filter_map(|(next, from)| tidy_one(r, &next, &from))
        .collect()
}

/// `tidy` for one handoff: closes the predecessor if `verdict` allows. Holds the handoff's
/// lock, so two tidies don't close it at once.
fn tidy_one(r: &Registry, next: &str, from: &str) -> Option<String> {
    let lock = File::create(r.forward_lock(next)).ok()?;
    lock.lock().ok()?;
    // Another tidy may have closed it while this one waited for the lock.
    if !r.predecessor_path(next).exists() {
        return None;
    }
    let forwards = r.forwards_path(next).exists();
    // The `.from` file is the handoff's marker, so it goes after the rest: once it is gone,
    // nothing of the handoff is left. The lock goes last, after another `tidy` that takes a
    // fresh one would find no `.from` and stop.
    let forget = || {
        let _ = fs::remove_file(r.forwards_path(next).with_extension("last"));
        let _ = fs::remove_file(r.forwards_path(next).with_extension("lastlock"));
        let _ = fs::remove_file(r.forwards_path(next));
        let _ = fs::remove_file(r.predecessor_path(next));
        let _ = fs::remove_file(r.forward_lock(next));
    };
    let Ok(w) = r.get(from) else {
        forget();
        return None;
    };
    let a = workers::agent(&w);
    let tab =
        (w.backend == Backend::Pane && a == Agent::Idle).then(|| workers::read_tab(&w.handle));
    let tab = tab.as_ref().map(|t| t.as_deref().map_err(String::as_str));
    match verdict(a, w.report.exists(), tab, next, forwards) {
        v @ (Verdict::Keep(_) | Verdict::Check(_)) => stays(from, &v),
        Verdict::Close => match workers::close(r, from, false) {
            Ok(()) => {
                forget();
                Some(format!("closed {from}, which handed off to {next}"))
            }
            Err(e) => Some(format!("{from} could not be closed: {e}")),
        },
    }
}

/// What the successor gets for a message the user sent its predecessor.
pub fn forwarded(from: &str, text: &str) -> String {
    format!(
        "reeve: the user sent this to {from} after it handed off to you, so it comes to you \
         instead:\n\n{text}"
    )
}

/// The handoff whose predecessor runs in `pane`, as (successor, predecessor).
pub fn handed_off_from(r: &Registry, pane: &str) -> Option<(String, String)> {
    let all = r.all().ok()?;
    let me = predecessor(&all, Some(pane))?;
    r.predecessors()
        .into_iter()
        .find(|(next, from)| *from == me.name && all.iter().any(|w| w.name == *next))
}

/// Puts `text` in `next`'s inbox as a message from `from`, the way `reeve send` would, and
/// marks the handoff as forwarding.
pub fn queue(r: &Registry, next: &str, from: &str, text: &str) -> io::Result<()> {
    use crate::msg;
    if !claim_forward(r, next, text, msg::now_secs())? {
        return Ok(());
    }
    let m = msg::compose(
        from,
        next,
        &forwarded(from, text),
        None,
        None,
        msg::now_secs(),
    )
    .map_err(io::Error::other)?;
    msg::write_atomic(&msg::Store::open()?.inbox(), &m)?;
    fs::write(r.forwards_path(next), "")
}

/// How long the same text from the predecessor counts as one message: a prompt can reach the
/// hook several times at once.
const DUPLICATE_SECS: u64 = 30;

/// Whether `text` should be forwarded to `next`: false when the same text was forwarded in the
/// last `DUPLICATE_SECS`. Records `text` when it is forwarded. Holds a lock, so hooks that run
/// at the same moment forward it once.
fn claim_forward(r: &Registry, next: &str, text: &str, now: u64) -> io::Result<bool> {
    let dir = r.forwards_path(next);
    let last = dir.with_extension("last");
    let lock = File::create(dir.with_extension("lastlock"))?;
    lock.lock()?;
    let seen = fs::read_to_string(&last).ok().and_then(|s| {
        let (at, prev) = s.split_once('\n')?;
        Some((at.parse::<u64>().ok()?, prev.to_string()))
    });
    if seen.is_some_and(|(at, prev)| prev == text && now.saturating_sub(at) < DUPLICATE_SECS) {
        return Ok(false);
    }
    fs::write(&last, format!("{now}\n{text}"))?;
    Ok(true)
}

/// A prompt that is a machine event, not something the user typed: a finished background task
/// or a message from another session. reeve delivers those to the successor itself.
pub fn is_machine_event(text: &str) -> bool {
    let t = text.trim_start();
    ["<task-notification>", "<system-reminder", "<cross-session-message"]
        .iter()
        .any(|p| t.starts_with(p))
}

/// What the predecessor's prompt hook answers: the prompt is kept from the predecessor, and the
/// user reads where it goes instead.
pub fn blocked(next: &str) -> Value {
    json!({
        "decision": "block",
        "reason": format!(
            "This session handed off to {next}, so reeve passes your message on to {next}'s \
             inbox, where {next} sees it, and closes this one."
        ),
    })
}

/// For the prompt hook: a prompt the user sends a predecessor after its handoff goes to the
/// successor's inbox and is kept from the predecessor, and `reeve tidy` starts in the
/// background to close the predecessor. `None` lets the prompt through, as when anything here
/// fails, so a message is never dropped.
pub fn on_prompt(payload: &Value) -> Option<Value> {
    if payload["hook_event_name"] != "UserPromptSubmit" {
        return None;
    }
    let text = payload["prompt"].as_str()?;
    // The predecessor's own monitors and background tasks keep reporting to it; blocking those
    // would loop, and the successor has its own.
    if is_machine_event(text) {
        return None;
    }
    let pane = std::env::var("HERDR_PANE_ID").ok()?;
    let r = Registry::at(Registry::default_dir().ok()?);
    let (next, from) = handed_off_from(&r, &pane)?;
    queue(&r, &next, &from, text).ok()?;
    let _ = spawn_follow(&next);
    Some(blocked(&next))
}

/// Runs `reeve tidy NEXT` in its own process group, so it outlives the hook.
fn spawn_follow(next: &str) -> io::Result<()> {
    use std::os::unix::process::CommandExt;
    Command::new(std::env::current_exe()?)
        .args(["tidy", next])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map(|_| ())
}

/// How often and how long `follow` tries.
const FOLLOW_EVERY: Duration = Duration::from_secs(2);
const FOLLOW_TRIES: u32 = 300;

/// `reeve tidy NEXT`, started by the handoff and by the prompt hook: tidies the handoff to
/// `next` until its predecessor is closed, giving up after ten minutes; a later `reeve ls`
/// carries on from there.
pub fn follow(r: &Registry, next: &str) {
    for _ in 0..FOLLOW_TRIES {
        std::thread::sleep(FOLLOW_EVERY);
        let Ok(from) = fs::read_to_string(r.predecessor_path(next)) else {
            return;
        };
        tidy_one(r, next, from.trim());
    }
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
        // With reeve's prompt hook, every later prompt to the predecessor goes to the
        // successor's inbox, so the handoff forwards from the start, whatever its tab still shows.
        if r.get(from)
            .is_ok_and(|w| setup::prompt_hook_installed(w.agent))
        {
            fs::write(r.forwards_path(next), "")?;
        }
    }
    if let Err(e) = workers::run(r, next, kind, backend, &args) {
        let _ = fs::remove_file(r.forwards_path(next));
        let _ = fs::remove_file(r.predecessor_path(next));
        return Err(e);
    }
    // Closes the predecessor as soon as it is done, not at some later `reeve ls`.
    if from.is_some() {
        let _ = spawn_follow(next);
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

    fn why(v: Verdict) -> String {
        match v {
            Verdict::Keep(why) | Verdict::Check(why) => why,
            Verdict::Close => panic!("closed"),
        }
    }

    #[test]
    fn a_tab_with_nothing_new_and_an_empty_box_can_be_closed() {
        assert_eq!(tab_verdict(&tab("", ""), "next", false), Verdict::Close);
    }

    #[test]
    fn unsent_input_keeps_the_tab_and_is_quoted() {
        let v = tab_verdict(
            &tab("", "start a worker on the reeve close fix"),
            "next",
            false,
        );
        assert_eq!(
            v,
            Verdict::Keep("has unsent input: \"start a worker on the reeve close fix\"".into())
        );
        let long = "x".repeat(100);
        let w = why(tab_verdict(&tab("", &long), "next", false));
        assert!(w.ends_with(&format!("\"{}…\"", "x".repeat(80))), "{w}");
        let multi = tab("", "first line\n  second line");
        assert_eq!(
            tab_verdict(&multi, "next", false),
            Verdict::Keep("has unsent input: \"first line second line\"".into())
        );
    }

    #[test]
    fn a_message_after_the_handoff_keeps_the_tab_and_is_quoted() {
        let after = "\n❯ also check the docs\n\n⏺ Sure.\n";
        assert_eq!(
            tab_verdict(&tab(after, ""), "next", false),
            Verdict::Check("got a new message: \"also check the docs\"".into())
        );
        // A message before the handoff is old news.
        let before = format!("❯ hello\n⏺ hi\n{}", tab("", ""));
        assert_eq!(tab_verdict(&before, "next", false), Verdict::Close);
    }

    #[test]
    fn a_tab_that_shows_no_handoff_to_this_successor_or_no_box_is_left_to_the_successor() {
        let other = tab("", "").replace("--name next", "--name next-2");
        let v = tab_verdict(&other, "next", false);
        assert!(
            matches!(&v, Verdict::Check(w) if w.contains("handoff to next")),
            "{v:?}"
        );
        // Claude Code folds the Bash call into "Ran 1 shell command".
        let folded = "⏺ Ran 1 shell command\n⏺ Handed off.\n❯ \n";
        let v = tab_verdict(folded, "next", false);
        assert!(
            matches!(&v, Verdict::Check(w) if w.contains("no longer shows")),
            "{v:?}"
        );
        let no_box = "⏺ Bash(reeve handoff --name next)\n";
        let v = tab_verdict(no_box, "next", false);
        assert!(
            matches!(&v, Verdict::Check(w) if w.contains("no input box")),
            "{v:?}"
        );
    }

    #[test]
    fn a_predecessor_reeve_cant_judge_says_what_the_successor_should_do() {
        let check = Verdict::Check("has no report yet".into());
        assert_eq!(
            stays("old", &check).as_deref(),
            Some(
                "old has no report yet; read its tab and `reeve close old` if nothing new is there"
            )
        );
        let keep = Verdict::Keep("is still working".into());
        assert_eq!(stays("old", &keep).as_deref(), Some("old is still working"));
        assert_eq!(stays("old", &Verdict::Close), None);
        let go = |a, report, t: Option<Result<&str, &str>>| verdict(a, report, t, "next", false);
        assert!(matches!(go(Agent::Idle, false, None), Verdict::Check(_)));
        assert!(matches!(go(Agent::Unknown, true, None), Verdict::Check(_)));
        assert!(matches!(
            go(Agent::Idle, true, Some(Err("boom"))),
            Verdict::Check(_)
        ));
        assert!(matches!(go(Agent::Working, true, None), Verdict::Keep(_)));
        assert!(matches!(go(Agent::Blocked, true, None), Verdict::Keep(_)));
    }

    #[test]
    fn once_reeve_forwards_the_messages_only_the_input_box_keeps_the_tab() {
        let after = "\n❯ also check the docs\n\n⏺ Sure.\n";
        assert_eq!(tab_verdict(&tab(after, ""), "next", true), Verdict::Close);
        assert_eq!(
            tab_verdict("⏺ Ran 1 shell command\n❯ \n", "next", true),
            Verdict::Close
        );
        assert_eq!(
            tab_verdict(&tab(after, "hi"), "next", true),
            Verdict::Keep("has unsent input: \"hi\"".into())
        );
        assert_ne!(tab_verdict("⏺ Compacted.\n", "next", true), Verdict::Close);
        // The hook keeps every prompt from it, so it can't write a report any more.
        let quiet = tab("", "");
        let v = verdict(Agent::Idle, false, Some(Ok(&quiet)), "next", true);
        assert_eq!(v, Verdict::Close);
        let v = verdict(Agent::Working, false, None, "next", true);
        assert_eq!(v, Verdict::Keep("is still working".into()));
    }

    #[test]
    fn the_successor_reads_who_the_message_was_for_and_all_of_it() {
        let f = forwarded("old", "line one\nline two");
        assert!(f.contains("old"), "{f}");
        assert!(f.ends_with("\n\nline one\nline two"), "{f}");
        let b = blocked("next");
        assert_eq!(b["decision"], "block");
        assert!(b["reason"].as_str().unwrap().contains("next's inbox"));
    }

    #[test]
    fn a_predecessor_closes_only_when_idle_with_a_report_and_a_quiet_tab() {
        let quiet = tab("", "");
        let typed = tab("", "hi");
        let go = |a, report, t: Option<Result<&str, &str>>| verdict(a, report, t, "next", false);
        let keep = |why: &str| Verdict::Keep(why.into());
        assert_eq!(go(Agent::Idle, true, Some(Ok(&quiet))), Verdict::Close);
        assert_eq!(go(Agent::Idle, true, None), Verdict::Close);
        assert_eq!(go(Agent::Absent, false, None), Verdict::Close);
        assert_eq!(go(Agent::Working, true, None), keep("is still working"));
        assert!(matches!(go(Agent::Blocked, true, None), Verdict::Keep(_)));
        assert_eq!(
            go(Agent::Idle, false, Some(Ok(&quiet))),
            Verdict::Check("has no report yet".into())
        );
        assert_eq!(
            go(Agent::Idle, true, Some(Ok(&typed))),
            keep("has unsent input: \"hi\"")
        );
        let w = why(go(Agent::Idle, true, Some(Err("boom"))));
        assert!(w.contains("boom"), "{w}");
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
    fn the_predecessor_in_a_reused_pane_is_the_newest_worker_there() {
        let mut stale = w("stale");
        stale.backend = Backend::Pane;
        stale.handle = "w1:p2".into();
        stale.started = 100;
        let mut current = stale.clone();
        current.name = "current".into();
        current.started = 200;
        let all = [stale, current];
        assert_eq!(
            predecessor(&all, Some("w1:p2")).map(|w| &w.name[..]),
            Some("current")
        );
    }

    #[test]
    fn three_workers_sharing_a_pane_across_two_handoffs_resolve_to_the_newest() {
        let dir = std::env::temp_dir().join(format!("reeve-ho-three-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let r = Registry::at(dir);
        let at = |name: &str, started| {
            let mut x = w(name);
            x.backend = Backend::Pane;
            x.handle = "wH4:p3".into();
            x.started = started;
            x
        };
        for x in [at("knip", 100), at("report", 200), at("ws6", 300)] {
            r.put(x).unwrap();
        }
        fs::create_dir_all(r.predecessor_path("x").parent().unwrap()).unwrap();
        fs::write(r.predecessor_path("report-2"), "report").unwrap();
        fs::write(r.predecessor_path("ws7"), "ws6").unwrap();
        r.put(w("report-2")).unwrap();
        r.put(w("ws7")).unwrap();
        // ws6 hands off from the shared pane: the handoff is ws6's, not knip's or report's.
        assert_eq!(
            handed_off_from(&r, "wH4:p3"),
            Some(("ws7".into(), "ws6".into()))
        );
    }

    #[test]
    fn a_repeated_forward_is_dropped_within_the_window_only() {
        let dir = std::env::temp_dir().join(format!("reeve-ho-dup-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("handoffs")).unwrap();
        let r = Registry::at(dir);
        assert!(claim_forward(&r, "next", "hello", 1000).unwrap());
        assert!(!claim_forward(&r, "next", "hello", 1002).unwrap());
        assert!(claim_forward(&r, "next", "something else", 1003).unwrap());
        assert!(claim_forward(&r, "next", "hello", 1004).unwrap());
        assert!(!claim_forward(&r, "next", "hello", 1005).unwrap());
        assert!(claim_forward(&r, "next", "hello", 1005 + DUPLICATE_SECS).unwrap());
        assert!(claim_forward(&r, "other", "hello", 1005).unwrap());
    }

    #[test]
    fn machine_events_are_not_forwarded() {
        assert!(is_machine_event("<task-notification>\n<task-id>x</task-id>"));
        assert!(is_machine_event("  <system-reminder>\nfoo"));
        assert!(is_machine_event("<cross-session-message from=\"a\">hi"));
        assert!(!is_machine_event("please look at <task-notification> handling"));
        assert!(!is_machine_event("hello"));
    }

    #[test]
    fn the_successor_is_told_to_read_the_file_in_one_line() {
        let p = successor_prompt(Path::new("/st/handoffs/next.md"));
        assert!(p.contains("/st/handoffs/next.md"));
        assert!(!p.chars().any(char::is_control));
    }
}
