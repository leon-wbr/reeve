//! A worker's report pushed to its parent: once its report is written, the session that started
//! it gets one inbox message, `NAME done: <the report's next line>`, through the inbox Monitor
//! or prompt and tool hook it already has. Nothing here waits on the worker or the parent: any
//! reeve process that sweeps (the hooks of any session, `reeve inbox --follow`, `reeve ls`,
//! `reeve watch`) delivers what is due, so a worker that exits right after its report, or a
//! parent that is busy, compacted or gone, loses nothing. `reeve ls` and `reeve watch` keep
//! working as before for whoever polls.

use crate::msg::{self, Store};
use crate::workers::{Caller, Registry, Worker};
use std::fs;
use std::io;
use std::time::{Duration, SystemTime};

/// A report this fresh may still be being written, so its push waits for the next sweep.
pub const SETTLE: Duration = Duration::from_secs(2);
/// The report's line shown in the push is cut to this many characters.
const LINE_MAX: usize = 200;

/// The parent address of a session that is no named worker: its Claude Code session id or
/// Codex thread, which a resumed or compacted session keeps.
pub fn session_key(id: &str) -> String {
    format!("session-{id}")
}

/// The address a worker started from session `c` pushes to: the named worker `c` is, else
/// `c`'s own session. None when reeve can't tell, which leaves the worker to polling.
pub fn parent(r: &Registry, c: &Caller) -> Option<String> {
    crate::workers::caller(r, c)
        .map(|w| w.name)
        .or_else(|| session_of(c))
}

/// `c`'s own session address, if it knows its session.
pub fn session_of(c: &Caller) -> Option<String> {
    c.claude_session
        .as_deref()
        .or(c.codex_thread.as_deref())
        .map(session_key)
}

/// The addresses session `c` reads pushes for: the named worker it is, then its own session.
pub fn addresses(r: &Registry, c: &Caller) -> Vec<String> {
    let mut a: Vec<String> = crate::workers::caller(r, c)
        .map(|w| w.name)
        .into_iter()
        .collect();
    a.extend(session_of(c));
    a
}

/// The push's id: fixed per worker run, so whoever pushes it, it is one message. Names have
/// only lowercase letters, digits, `-` and `_`; `_` becomes `U` to fit an id.
pub fn id(w: &Worker) -> String {
    let stamp: String = msg::fmt_utc(w.started)
        .chars()
        .filter(|c| *c != '-' && *c != ':')
        .collect();
    format!("{stamp}-{}", w.name.replace('_', "U"))
}

/// `done: the next line`, from a report's first two non-empty lines.
pub fn body(name: &str, report: &str) -> String {
    let mut lines = report
        .lines()
        .map(|l| l.trim().trim_start_matches('#').trim())
        .filter(|l| !l.is_empty());
    let status = lines.next().unwrap_or("(empty report)");
    let mut text = match lines.next() {
        Some(next) => format!("{status}: {next}"),
        None => status.to_string(),
    };
    if text.chars().count() > LINE_MAX {
        text = text.chars().take(LINE_MAX - 1).chain(['…']).collect();
    }
    format!("{text} (`reeve report {name}`)")
}

/// Whether `w` has a parent, isn't pushed yet, and its report has settled by `now`.
fn due(w: &Worker, now: SystemTime) -> bool {
    w.parent.is_some()
        && !w.pushed
        && fs::metadata(&w.report)
            .and_then(|m| m.modified())
            .is_ok_and(|t| now.duration_since(t).is_ok_and(|age| age >= SETTLE))
}

/// Pushes every due report and returns the names pushed. Cheap when nothing is due: one read of
/// the registry. The rest happens under the registry's lock, so two sweeps never both push one
/// report; the message goes in before the worker is marked, and a crash between the two is
/// caught by the message already being in the inbox (read or not), so it is never sent twice.
pub fn sweep(r: &Registry, store: &Store, now: SystemTime) -> io::Result<Vec<String>> {
    if !r.all()?.iter().any(|w| due(w, now)) {
        return Ok(vec![]);
    }
    let mut pushed = vec![];
    let mut failed = None;
    r.update(|all| {
        for w in all.iter_mut().filter(|w| due(w, now)) {
            match deliver(store, w) {
                Ok(()) => {
                    w.pushed = true;
                    pushed.push(w.name.clone());
                }
                Err(e) => failed = Some(e),
            }
        }
    })?;
    match failed {
        Some(e) if pushed.is_empty() => Err(e),
        _ => Ok(pushed),
    }
}

fn deliver(store: &Store, w: &Worker) -> io::Result<()> {
    let id = id(w);
    if store.has(&id) {
        return Ok(());
    }
    let report = fs::read_to_string(&w.report)?;
    let to = w.parent.as_deref().unwrap_or_default();
    let mut m = msg::compose(
        &w.name,
        to,
        &body(&w.name, &report),
        None,
        None,
        msg::now_secs(),
    )
    .map_err(io::Error::other)?;
    m["id"] = id.into();
    msg::write_atomic(&store.inbox(), &m)
}

/// After a handoff, `next` takes over the workers that pushed to any of `old` (the predecessor's
/// name and its session): their later pushes go to `next`, and so do the ones not yet read.
/// Each unread push is claimed before it is re-sent, so the predecessor reading it at the same
/// moment means it was delivered there, never to both.
pub fn adopt(r: &Registry, store: &Store, old: &[String], next: &str) -> io::Result<()> {
    let mut kids = vec![];
    r.update(|all| {
        for w in all.iter_mut().filter(|w| w.name != next) {
            if w.parent.as_ref().is_some_and(|p| old.contains(p)) {
                w.parent = Some(next.to_string());
                kids.push(w.name.clone());
            }
        }
    })?;
    for m in msg::list(&store.inbox()) {
        let (to, from) = (msg::field(&m, "to"), msg::field(&m, "from"));
        if !old.iter().any(|o| o == to) || !kids.iter().any(|k| k == from) {
            continue;
        }
        if store.mark_read(msg::field(&m, "id"))? {
            let mut m = m.clone();
            m["to"] = next.into();
            msg::write_atomic(&store.inbox(), &m)?;
        }
    }
    Ok(())
}

/// A sweep for a hook or a loop: never fails, never prints.
pub fn quietly() {
    let (Ok(dir), Ok(store)) = (Registry::default_dir(), Store::open()) else {
        return;
    };
    let _ = sweep(&Registry::at(dir), &store, SystemTime::now());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbox;
    use crate::workers::{Backend, Kind};
    use std::path::Path;

    struct Fixture {
        _d: tempfile::TempDir,
        r: Registry,
        s: Store,
        state: std::path::PathBuf,
    }

    fn fixture() -> Fixture {
        let d = tempfile::tempdir().unwrap();
        let state = d.path().join("state");
        Fixture {
            r: Registry::at(state.clone()),
            s: Store::at(d.path().join("home")),
            state,
            _d: d,
        }
    }

    fn worker(f: &Fixture, name: &str, parent: Option<&str>) -> Worker {
        let w = Worker {
            name: name.into(),
            backend: Backend::Bg,
            agent: Kind::Claude,
            cwd: "/".into(),
            started: 1_793_016_045,
            report: f.r.report_path(name),
            handle: "h".into(),
            starting: false,
            error: None,
            parent: parent.map(String::from),
            pushed: false,
            pipeline: None,
        };
        f.r.put(w.clone()).unwrap();
        w
    }

    fn write_report(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn later() -> SystemTime {
        SystemTime::now() + SETTLE
    }

    fn bodies(ms: &[serde_json::Value]) -> Vec<String> {
        ms.iter()
            .map(|m| msg::field(m, "body").to_string())
            .collect()
    }

    #[test]
    fn the_push_is_the_status_and_the_next_line_and_says_where_the_rest_is() {
        assert_eq!(
            body("fix", "done\n\nFixed the flaky test; 20 runs green.\nMore."),
            "done: Fixed the flaky test; 20 runs green. (`reeve report fix`)"
        );
        assert_eq!(body("a", "# partial\n"), "partial (`reeve report a`)");
        assert_eq!(body("a", ""), "(empty report) (`reeve report a`)");
        let long = body("a", &format!("done\n{}", "x".repeat(500)));
        assert!(
            long.contains('…') && long.chars().count() < LINE_MAX + 30,
            "{long}"
        );
    }

    #[test]
    fn ids_are_fixed_per_run_and_valid() {
        let f = fixture();
        let mut w = worker(&f, "a_b", Some("boss"));
        let first = id(&w);
        assert!(msg::valid_id(&first), "{first}");
        assert_eq!(first, id(&w));
        assert_ne!(first, id(&worker(&f, "a-b", Some("boss"))));
        w.started += 1;
        assert_ne!(first, id(&w), "a new run of the same name is a new push");
    }

    #[test]
    fn a_report_is_pushed_to_the_parent_once_it_has_settled() {
        let f = fixture();
        let w = worker(&f, "a", Some("boss"));
        assert!(
            sweep(&f.r, &f.s, later()).unwrap().is_empty(),
            "no report yet"
        );
        write_report(&w.report, "done\nAll green.");
        assert!(
            sweep(&f.r, &f.s, SystemTime::now()).unwrap().is_empty(),
            "a report still being written waits"
        );
        assert_eq!(sweep(&f.r, &f.s, later()).unwrap(), ["a"]);
        let got = inbox::claim(&f.s, "boss", msg::now_secs());
        assert_eq!(bodies(&got), ["done: All green. (`reeve report a`)"]);
        assert_eq!(msg::field(&got[0], "from"), "a");
        assert!(f.r.get("a").unwrap().pushed);
    }

    #[test]
    fn a_worker_without_a_parent_is_left_to_polling() {
        let f = fixture();
        let w = worker(&f, "a", None);
        write_report(&w.report, "done\nx");
        assert!(sweep(&f.r, &f.s, later()).unwrap().is_empty());
        assert!(msg::list(&f.s.inbox()).is_empty());
    }

    #[test]
    fn a_busy_parent_gets_it_once_whenever_it_looks() {
        let f = fixture();
        let w = worker(&f, "a", Some("boss"));
        write_report(&w.report, "done\nx");
        // The parent doesn't look while many sweeps run.
        for _ in 0..5 {
            sweep(&f.r, &f.s, later()).unwrap();
        }
        assert_eq!(inbox::for_worker(&f.s, "boss", msg::now_secs()).len(), 1);
        assert_eq!(inbox::claim(&f.s, "boss", msg::now_secs()).len(), 1);
        sweep(&f.r, &f.s, later()).unwrap();
        assert!(inbox::claim(&f.s, "boss", msg::now_secs()).is_empty());
    }

    #[test]
    fn a_parent_that_is_gone_leaves_the_push_in_the_inbox_and_the_worker_done() {
        let f = fixture();
        let w = worker(&f, "a", Some("session-gone"));
        write_report(&w.report, "failed\nno disk");
        assert_eq!(sweep(&f.r, &f.s, later()).unwrap(), ["a"]);
        // Nobody claims it: it stays unread, for `reeve inbox`, and polling sees done as before.
        let left = msg::list(&f.s.inbox());
        assert_eq!(left.len(), 1);
        assert_eq!(msg::field(&left[0], "to"), "session-gone");
        assert!(w.report.is_file());
    }

    #[test]
    fn a_crash_after_the_message_but_before_the_mark_sends_nothing_twice() {
        let f = fixture();
        let w = worker(&f, "a", Some("boss"));
        write_report(&w.report, "done\nx");
        sweep(&f.r, &f.s, later()).unwrap();
        // The parent already read it; the mark was lost.
        assert_eq!(inbox::claim(&f.s, "boss", msg::now_secs()).len(), 1);
        f.r.set("a", w.started, |w| w.pushed = false).unwrap();
        sweep(&f.r, &f.s, later()).unwrap();
        assert!(inbox::claim(&f.s, "boss", msg::now_secs()).is_empty());
        assert!(f.r.get("a").unwrap().pushed);
    }

    #[test]
    fn concurrent_sweeps_push_one_message() {
        let f = fixture();
        for i in 0..4 {
            let w = worker(&f, &format!("w{i}"), Some("boss"));
            write_report(&w.report, "done\nx");
        }
        std::thread::scope(|sc| {
            for _ in 0..8 {
                sc.spawn(|| {
                    let r = Registry::at(f.state.clone());
                    for _ in 0..5 {
                        sweep(&r, &f.s, later()).unwrap();
                    }
                });
            }
        });
        let got = inbox::claim(&f.s, "boss", msg::now_secs());
        let mut froms: Vec<_> = got.iter().map(|m| msg::field(m, "from")).collect();
        froms.sort();
        assert_eq!(froms, ["w0", "w1", "w2", "w3"]);
    }

    #[test]
    fn two_readers_racing_for_the_same_push_show_it_once() {
        let f = fixture();
        let w = worker(&f, "a", Some("boss"));
        write_report(&w.report, "done\nx");
        sweep(&f.r, &f.s, later()).unwrap();
        let shown: usize = std::thread::scope(|sc| {
            let hs: Vec<_> = (0..8)
                .map(|_| sc.spawn(|| inbox::claim(&f.s, "boss", msg::now_secs()).len()))
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).sum()
        });
        assert_eq!(shown, 1);
    }

    #[test]
    fn a_successor_gets_its_predecessors_workers_and_their_unread_pushes_once() {
        let f = fixture();
        let read = worker(&f, "read", Some("session-old"));
        let unread = worker(&f, "unread", Some("old"));
        let late = worker(&f, "later", Some("session-old"));
        let other = worker(&f, "other", Some("someone"));
        worker(&f, "next", Some("boss"));
        for w in [&read, &unread, &other] {
            write_report(&w.report, "done\nx");
        }
        sweep(&f.r, &f.s, later()).unwrap();
        // The predecessor saw one push before it handed off, and a message from a person waits.
        assert_eq!(inbox::claim(&f.s, "session-old", msg::now_secs()).len(), 1);
        let note = msg::compose("local", "old", "hi", None, None, msg::now_secs()).unwrap();
        msg::write_atomic(&f.s.inbox(), &note).unwrap();

        adopt(&f.r, &f.s, &["old".into(), "session-old".into()], "next").unwrap();
        for n in ["read", "unread", "later"] {
            assert_eq!(f.r.get(n).unwrap().parent.as_deref(), Some("next"), "{n}");
        }
        assert_eq!(f.r.get("other").unwrap().parent.as_deref(), Some("someone"));
        assert_eq!(f.r.get("next").unwrap().parent.as_deref(), Some("boss"));
        let got = inbox::claim(&f.s, "next", msg::now_secs());
        let froms: Vec<_> = got.iter().map(|m| msg::field(m, "from")).collect();
        assert_eq!(froms, ["unread"], "only the unread push moves");
        assert_eq!(inbox::for_worker(&f.s, "old", msg::now_secs()).len(), 1);
        write_report(&late.report, "done\ny");
        sweep(&f.r, &f.s, later()).unwrap();
        let got = inbox::claim(&f.s, "next", msg::now_secs());
        assert_eq!(msg::field(&got[0], "from"), "later");
    }

    #[test]
    fn a_worker_run_again_under_its_name_pushes_its_new_report() {
        let f = fixture();
        let w = worker(&f, "a", Some("boss"));
        write_report(&w.report, "failed\nfirst try");
        sweep(&f.r, &f.s, later()).unwrap();
        let mut again = w.clone();
        again.started += 60;
        f.r.put(again).unwrap();
        write_report(&w.report, "done\nsecond try");
        sweep(&f.r, &f.s, later()).unwrap();
        let got = bodies(&inbox::claim(&f.s, "boss", msg::now_secs()));
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[1].contains("second try"), "{got:?}");
    }
}
