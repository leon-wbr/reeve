//! Closing finished workers by default: a worker that is done (its report is written and its
//! agent is idle) is closed after a grace period, the way `reeve close` would, unless it opted
//! out, has unread messages or a heavy job of its own, or its report isn't pushed to its parent.

use crate::workers::{self, Agent, Registry, Worker};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

/// How long a report has been there before its worker is closed, in seconds.
pub const GRACE_SECS: u64 = 120;

/// `REEVE_AUTO_CLOSE=0` (or `off`, `false`, `no`) turns automatic closing off everywhere.
pub fn enabled() -> bool {
    enabled_in(std::env::var("REEVE_AUTO_CLOSE").ok().as_deref())
}

pub fn enabled_in(var: Option<&str>) -> bool {
    !matches!(
        var.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("0" | "off" | "false" | "no")
    )
}

/// `REEVE_CLOSE_GRACE`, in seconds.
pub fn grace() -> u64 {
    grace_in(std::env::var("REEVE_CLOSE_GRACE").ok().as_deref())
}

pub fn grace_in(var: Option<&str>) -> u64 {
    var.and_then(|v| v.trim().parse().ok())
        .unwrap_or(GRACE_SECS)
}

/// What decides whether a worker is closed.
#[derive(Debug, Clone)]
pub struct Facts {
    pub agent: Agent,
    /// Seconds since the report was written; `None` without a report.
    pub report_age: Option<u64>,
    pub keep_open: bool,
    pub unread: usize,
    pub heavy: bool,
    /// It has a parent its report isn't pushed to yet.
    pub unpushed: bool,
}

/// What a pass found for one worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Closed just now: what became of its worktree.
    Closed(String),
    /// Done, and will be closed once the grace period is over.
    Waiting,
    /// Done, and stays open for this reason.
    Stays(String),
    /// Nothing for auto-close to do (no report, no agent, or not finished).
    Not,
}

/// Whether a worker with these facts is closed now.
pub fn due(f: &Facts, grace: u64) -> Outcome {
    let stays = |why: &str| Outcome::Stays(why.to_string());
    let Some(age) = f.report_age else {
        return Outcome::Not;
    };
    match f.agent {
        Agent::Absent => return Outcome::Not,
        Agent::Working | Agent::Blocked | Agent::Unknown => return Outcome::Not,
        Agent::Idle => {}
    }
    if f.keep_open {
        return stays("started with --keep-open");
    }
    if f.unread > 0 {
        let s = if f.unread == 1 { "" } else { "s" };
        return Outcome::Stays(format!("it has {} unread message{s}", f.unread));
    }
    if f.heavy {
        return stays("a heavy job of its own is queued or running");
    }
    if age < grace || f.unpushed {
        return Outcome::Waiting;
    }
    Outcome::Closed(String::new())
}

/// One pass over `all`: closes the workers `due` allows, with `close` returning what became of
/// the worktree. Returns the outcome for each worker that has one.
pub fn pass(
    all: &[Worker],
    grace: u64,
    facts: impl Fn(&Worker) -> Facts,
    close: impl Fn(&Worker) -> io::Result<String>,
) -> HashMap<String, Outcome> {
    let mut out = HashMap::new();
    for w in all {
        let o = match due(&facts(w), grace) {
            Outcome::Closed(_) => match close(w) {
                Ok(wt) => Outcome::Closed(wt),
                Err(e) => Outcome::Stays(format!("closing failed: {e}")),
            },
            o => o,
        };
        if o != Outcome::Not {
            out.insert(w.name.clone(), o);
        }
    }
    out
}

/// The wording for a worktree: `worktree kept: why`, or nothing to add.
pub fn worktree_note(wt: &str) -> String {
    match wt.split_once(": ") {
        Some((head, why)) if head.starts_with("kept worktree") => {
            format!(" (worktree kept: {why})")
        }
        _ => String::new(),
    }
}

/// The part of a `done` line that says what happened to the pane.
pub fn done_line(name: &str, report: &Path, o: Option<&Outcome>) -> String {
    let at = crate::setup::tilde(report);
    match o {
        Some(Outcome::Closed(wt)) => {
            format!(
                "{name} done: report at {at}; pane closed automatically{}",
                worktree_note(wt)
            )
        }
        Some(Outcome::Stays(why)) => format!(
            "{name} done: report at {at}; not closed automatically ({why}); read it, then \
             `reeve close {name}`"
        ),
        _ => format!("{name} done: report at {at}; read it, then `reeve close {name}`"),
    }
}

pub fn keep_open_path(r: &Registry, name: &str) -> std::path::PathBuf {
    r.report_path(name).with_extension("keep-open")
}

fn report_age(w: &Worker, now: u64) -> Option<u64> {
    let t = fs::metadata(&w.report).ok()?.modified().ok()?;
    let secs = t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    Some(now.saturating_sub(secs))
}

/// Whether a heavy job runs or waits in the worker's checkout.
fn heavy_for(w: &Worker, jobs: &[crate::heavy::Entry]) -> bool {
    // A worker's own worktree is the only checkout its jobs can be told from others'.
    let own = w.cwd.ends_with(format!(".claude/worktrees/{}", w.name));
    let tree = w.cwd.join(".claude/worktrees").join(&w.name);
    jobs.iter().any(|j| {
        let at = Path::new(&j.cwd);
        at.starts_with(&tree) || (own && at.starts_with(&w.cwd))
    })
}

/// `pass` with the real probes, for `ls`, `watch` and `reeve autoclose`. Does nothing when
/// automatic closing is off. Agents `ls` or `watch` have just probed aren't probed again; the
/// one about to be closed is, under a lock per worker that keeps two passes from closing it twice.
pub fn sweep(r: &Registry, all: &[Worker]) -> HashMap<String, Outcome> {
    if !enabled() {
        return HashMap::new();
    }
    let now = workers::now();
    let unread = crate::msg::Store::open()
        .map(|s| crate::inbox::unread_counts(&s, now))
        .unwrap_or_default();
    let queue = crate::heavy::Queue::at(crate::heavy::Queue::default_root());
    let jobs: Vec<_> = queue.running().into_iter().chain(queue.waiting()).collect();
    // Only a worker with a report can be closed, and that costs no probe to tell.
    let reported: Vec<Worker> = all.iter().filter(|w| w.report.is_file()).cloned().collect();
    pass(
        &reported,
        grace(),
        |w| Facts {
            agent: workers::seen_agent(w),
            report_age: report_age(w, now),
            keep_open: keep_open_path(r, &w.name).exists(),
            unread: unread.get(&w.name).copied().unwrap_or(0),
            heavy: heavy_for(w, &jobs),
            unpushed: w.parent.is_some() && !w.pushed,
        },
        |w| {
            let lock = fs::File::create(r.report_path(&w.name).with_extension("close.lock"))?;
            lock.try_lock()
                .map_err(|_| io::Error::other("another pass is closing it"))?;
            if workers::agent(w) == Agent::Absent {
                return Ok(String::new());
            }
            workers::close_quiet(r, &w.name, false)
        },
    )
}

/// Runs `reeve autoclose NAME` in its own process group, so it outlives `reeve run`.
pub fn spawn_follow(name: &str) -> io::Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    Command::new(std::env::current_exe()?)
        .args(["autoclose", name])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map(|_| ())
}

/// `reeve autoclose NAME`, started by `reeve run`: sweeps until the worker is closed, stays open
/// for good, or an hour has passed without a report.
pub fn follow(r: &Registry, name: &str) {
    const EVERY: std::time::Duration = std::time::Duration::from_secs(10);
    for _ in 0..(6 * 60 * 24) {
        std::thread::sleep(EVERY);
        // The report reaches the parent first, with nobody running `ls` or `watch`.
        crate::push::quietly();
        let Ok(w) = r.get(name) else { return };
        let o = sweep(r, std::slice::from_ref(&w));
        match o.get(name) {
            Some(Outcome::Closed(_)) => return,
            Some(Outcome::Stays(why)) if why.starts_with("started with") => return,
            _ if workers::agent(&w) == Agent::Absent => return,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workers::{Backend, Kind};
    use std::cell::RefCell;

    fn facts(agent: Agent, age: Option<u64>) -> Facts {
        Facts {
            agent,
            report_age: age,
            keep_open: false,
            unread: 0,
            heavy: false,
            unpushed: false,
        }
    }

    fn worker(name: &str) -> Worker {
        Worker {
            name: name.into(),
            backend: Backend::Pane,
            agent: Kind::Claude,
            cwd: "/work/x".into(),
            started: 1,
            report: format!("/st/reports/{name}.md").into(),
            handle: "w1:p1".into(),
            starting: false,
            error: None,
            parent: None,
            pushed: false,
        }
    }

    #[test]
    fn a_done_worker_with_a_report_is_closed_after_the_grace_period() {
        let f = facts(Agent::Idle, Some(121));
        assert_eq!(due(&f, 120), Outcome::Closed(String::new()));
        let f = facts(Agent::Idle, Some(30));
        assert_eq!(due(&f, 120), Outcome::Waiting);
    }

    #[test]
    fn the_pass_closes_a_due_worker_and_reports_a_kept_worktree() {
        let closed = RefCell::new(vec![]);
        let out = pass(
            &[worker("a"), worker("b")],
            120,
            |w| facts(Agent::Idle, Some(if w.name == "a" { 500 } else { 5 })),
            |w| {
                closed.borrow_mut().push(w.name.clone());
                Ok("kept worktree /wt/a: it has uncommitted changes".into())
            },
        );
        assert_eq!(*closed.borrow(), ["a"]);
        assert_eq!(out["b"], Outcome::Waiting);
        let line = done_line("a", Path::new("/st/reports/a.md"), out.get("a"));
        assert!(line.contains("pane closed automatically"), "{line}");
        assert!(
            line.ends_with("(worktree kept: it has uncommitted changes)"),
            "{line}"
        );
    }

    #[test]
    fn keep_open_is_respected() {
        let mut f = facts(Agent::Idle, Some(9999));
        f.keep_open = true;
        assert!(matches!(due(&f, 120), Outcome::Stays(_)));
    }

    #[test]
    fn an_idle_worker_without_a_report_is_not_closed() {
        assert_eq!(due(&facts(Agent::Idle, None), 120), Outcome::Not);
    }

    #[test]
    fn working_blocked_or_gone_agents_are_left_alone() {
        for a in [
            Agent::Working,
            Agent::Blocked,
            Agent::Unknown,
            Agent::Absent,
        ] {
            assert_eq!(due(&facts(a, Some(9999)), 120), Outcome::Not, "{a:?}");
        }
    }

    #[test]
    fn unread_messages_and_heavy_jobs_keep_it_open() {
        let mut f = facts(Agent::Idle, Some(9999));
        f.unread = 2;
        assert_eq!(
            due(&f, 120),
            Outcome::Stays("it has 2 unread messages".into())
        );
        f.unread = 0;
        f.heavy = true;
        assert!(matches!(due(&f, 120), Outcome::Stays(_)));
    }

    #[test]
    fn a_report_not_yet_pushed_to_its_parent_waits() {
        let mut f = facts(Agent::Idle, Some(9999));
        f.unpushed = true;
        assert_eq!(due(&f, 120), Outcome::Waiting);
    }

    #[test]
    fn a_failed_close_is_reported_not_swallowed() {
        let out = pass(
            &[worker("a")],
            0,
            |_| facts(Agent::Idle, Some(10)),
            |_| Err(io::Error::other("herdr is gone")),
        );
        assert_eq!(
            out["a"],
            Outcome::Stays("closing failed: herdr is gone".into())
        );
    }

    #[test]
    fn the_switches_read_the_environment() {
        assert!(enabled_in(None));
        assert!(enabled_in(Some("1")));
        assert!(!enabled_in(Some("0")));
        assert!(!enabled_in(Some("Off")));
        assert_eq!(grace_in(None), 120);
        assert_eq!(grace_in(Some("30")), 30);
        assert_eq!(grace_in(Some("soon")), 120);
    }

    #[test]
    fn the_done_line_without_an_outcome_keeps_the_old_wording() {
        let l = done_line("a", Path::new("/st/reports/a.md"), None);
        assert!(l.ends_with("; read it, then `reeve close a`"), "{l}");
    }
}
