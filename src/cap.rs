//! `reeve run`'s worker cap: how many agent sessions are live and how much memory is free
//! decide whether a new worker starts now, waits as `queued`, or is refused.

use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::mem::{self, Limits, Mem};
use crate::workers::{self, Backend, Registry, START_SECS, State, Worker};

/// How often a queued worker looks again for room.
const POLL: Duration = Duration::from_secs(3);

/// Workers that hold a session: started by reeve (from this state dir) and neither finished nor
/// gone, and finished background ones whose session is still open. A queued worker holds nothing
/// yet. Sessions reeve didn't start count through free memory. The names are the finished
/// background workers, whose sessions nobody looks at and `reeve close` frees.
pub fn live(r: &Registry) -> (usize, Vec<String>) {
    let all = r.all().unwrap_or_default();
    let mut active = 0;
    let mut idle_done = vec![];
    for (w, (s, _, present)) in all.iter().zip(workers::probed(&all)) {
        match s {
            State::Starting | State::Running | State::Idle | State::Blocked => active += 1,
            State::Done if present && w.backend == Backend::Bg => idle_done.push(w.name.clone()),
            _ => {}
        }
    }
    (active + idle_done.len(), idle_done)
}

/// Workers started within START_SECS, whose agents have not yet grown into the memory they will
/// take; and queued workers that came first.
fn starting(all: &[Worker], now: u64) -> usize {
    all.iter()
        .filter(|w| !w.queued && w.starting && w.error.is_none())
        .filter(|w| now.saturating_sub(w.started) < START_SECS)
        .count()
}

/// Waiting workers that asked before `w`, whose process is still there.
fn ahead(all: &[Worker], name: &str, started: u64) -> usize {
    all.iter()
        .filter(|w| w.queued && w.name != name)
        .filter(|w| (w.started, w.name.as_str()) < (started, name))
        .filter(|w| w.handle.parse::<i32>().map_or(true, workers::pid_alive))
        .count()
}

/// Why no worker can start now, or None if one can.
fn no_room(all: &[Worker], mem: Option<&Mem>, limits: &Limits, live: usize) -> Option<String> {
    mem::room(mem, limits, live, starting(all, workers::now())).err()
}

/// What `reeve run` does with a named worker.
#[derive(Debug, PartialEq, Eq)]
pub enum Gate {
    Start,
    /// A process now waits for room, then starts it.
    Queued,
}

pub struct Opts {
    pub force: bool,
    pub no_wait: bool,
    /// This process is the one a queued run left behind to wait.
    pub queued: bool,
}

/// A lock held from the check until the worker is registered, so that runs starting together
/// each see the ones before them.
pub struct Held(#[allow(dead_code)] File);

fn lock(dir: &PathBuf) -> io::Result<Held> {
    std::fs::create_dir_all(dir)?;
    let f = File::create(dir.join("admit.lock"))?;
    f.lock()?;
    Ok(Held(f))
}

/// Decides for worker `name`. `argv` is this command line, rerun by the process that waits.
/// With `Start` the lock comes back: hold it until `workers::run` has registered the worker.
pub fn gate(
    r: &Registry,
    name: &str,
    opts: &Opts,
    argv: &[String],
) -> io::Result<(Gate, Option<Held>)> {
    if opts.force || Limits::from_env(None).off() {
        return Ok((Gate::Start, None));
    }
    let dir = Registry::default_dir()?;
    let mut announced = false;
    loop {
        let held = lock(&dir)?;
        let all = r.all()?;
        let (mem, started) = (
            mem::probe(),
            r.get(name).ok().filter(|w| w.queued).map(|w| w.started),
        );
        // The wait is dropped with its placeholder (`reeve rm`) or its directory.
        if opts.queued && (started.is_none() || std::env::current_dir().is_err()) {
            return Err(io::Error::other(format!(
                "queued worker {name} was dropped"
            )));
        }
        let limits = Limits::from_env(mem.as_ref());
        let (mut live, idle_done) = live(r);
        let mut why = no_room(&all, mem.as_ref(), &limits, live);
        if why.is_some() && !idle_done.is_empty() {
            // Their reports are written; ending their sessions makes room without losing any work.
            for n in &idle_done {
                if workers::close(r, n, true).is_ok() {
                    live -= 1;
                }
            }
            why = no_room(&all, mem.as_ref(), &limits, live);
        }
        let first = ahead(&all, name, started.unwrap_or(u64::MAX)) == 0;
        let blocked = why.or_else(|| (!first).then(|| "workers ahead of it are waiting".into()));
        match blocked {
            None => return Ok((Gate::Start, Some(held))),
            Some(why) if !opts.queued && opts.no_wait => {
                return Err(io::Error::other(format!(
                    "not starting worker {name}: {why}; close finished workers (`reeve ls` flags them), wait, or pass --force"
                )));
            }
            Some(why) if !opts.queued => {
                // Register the wait, so `reeve ls` shows it, and leave a process to carry it out.
                let w = workers::queued(name, std::env::current_dir()?, r.report_path(name));
                r.put(w)?;
                drop(held);
                spawn_waiter(argv)?;
                eprintln!(
                    "reeve: worker {name} is queued: {why}; it starts when there is room (`reeve ls` shows it as queued; --force starts it now)"
                );
                return Ok((Gate::Queued, None));
            }
            _ => {
                if !announced {
                    announced = true;
                    if let Some(w) = r.get(name).ok().filter(|w| w.queued) {
                        let pid = std::process::id().to_string();
                        r.set(name, w.started, |w| w.handle = pid)?;
                    }
                }
                drop(held);
                std::thread::sleep(POLL);
            }
        }
    }
}

/// `argv` without the `--cwd DIR` or `--cwd=DIR` given before the agent's own arguments.
fn without_cwd(argv: &[String]) -> Vec<String> {
    let mut out = vec![];
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--" => {
                out.push(a.clone());
                out.extend(it.by_ref().cloned());
            }
            "--cwd" => {
                it.next();
            }
            _ if a.starts_with("--cwd=") => {}
            _ => out.push(a.clone()),
        }
    }
    out
}

/// Reruns this `reeve run`, detached, as the process that waits for room.
fn spawn_waiter(argv: &[String]) -> io::Result<()> {
    use std::os::unix::process::CommandExt;
    // Already in the --cwd directory, which a relative path would enter a second time.
    Command::new(std::env::current_exe()?)
        .args(without_cwd(argv))
        .env("REEVE_QUEUED_RUN", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map(|_| ())
}

/// The line `reeve ls` ends with: free memory, live sessions and the limits, plus how many
/// finished workers still hold a session that `reeve close` would free.
pub fn ls_line(mem: Option<&Mem>, live: Option<usize>, limits: &Limits, closable: usize) -> String {
    let mut out = String::from("memory:");
    match mem {
        Some(m) => {
            out += &format!(
                " {}% free ({:.1} of {:.0} GB)",
                m.free_pct,
                m.free_gb(),
                m.total_gb()
            )
        }
        None => out += " unknown",
    }
    match live {
        Some(n) if limits.max_workers == usize::MAX => out += &format!(", {n} workers live"),
        Some(n) => out += &format!(", {n} workers live (limit {})", limits.max_workers),
        None => {}
    }
    if closable > 0 {
        out += &format!(
            "; {closable} done worker{} still hold{} a session: `reeve close NAME` frees it",
            if closable == 1 { "" } else { "s" },
            if closable == 1 { "s" } else { "" }
        );
    }
    out
}

/// Whether a finished worker could be closed to free its session.
pub fn closable(state: State, agent_present: bool) -> bool {
    state == State::Done && agent_present
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_waiting_process_is_not_told_the_cwd_again() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            without_cwd(&a(&[
                "run", "--name", "w", "--cwd", "rel", "--", "--cwd", "x"
            ])),
            a(&["run", "--name", "w", "--", "--cwd", "x"])
        );
        assert_eq!(
            without_cwd(&a(&["run", "--cwd=rel", "--name", "w"])),
            a(&["run", "--name", "w"])
        );
    }

    #[test]
    fn ls_line_gives_memory_sessions_and_what_could_be_closed() {
        let m = Mem {
            total: 16 << 30,
            free_pct: 50,
        };
        let l = Limits::from(Some(&m), Some("12"), None);
        assert_eq!(
            ls_line(Some(&m), Some(9), &l, 0),
            "memory: 50% free (8.0 of 16 GB), 9 workers live (limit 12)"
        );
        assert!(
            ls_line(Some(&m), Some(9), &l, 2)
                .ends_with("; 2 done workers still hold a session: `reeve close NAME` frees it")
        );
        assert!(
            ls_line(None, None, &l, 1).starts_with("memory: unknown; 1 done worker still holds")
        );
    }

    fn w(name: &str, started: u64, queued: bool, handle: &str) -> Worker {
        workers::queued(name, "/x".into(), "/r.md".into()).tap(|w| {
            w.started = started;
            w.queued = queued;
            w.starting = !queued;
            w.handle = handle.into();
        })
    }

    trait Tap: Sized {
        fn tap(mut self, f: impl FnOnce(&mut Self)) -> Self {
            f(&mut self);
            self
        }
    }
    impl Tap for Worker {}

    #[test]
    fn starting_workers_count_for_thirty_seconds() {
        let all = [
            w("a", 100, false, ""),
            w("b", 90, false, ""),
            w("c", 100, true, ""),
        ];
        assert_eq!(starting(&all, 110), 2);
        assert_eq!(starting(&all, 100 + START_SECS), 0);
    }

    #[test]
    fn earlier_waiters_go_first() {
        let me = std::process::id().to_string();
        let all = [
            w("a", 10, true, &me),
            w("b", 20, true, &me),
            w("c", 30, true, ""),
        ];
        assert_eq!(ahead(&all, "a", 10), 0);
        assert_eq!(ahead(&all, "b", 20), 1);
        assert_eq!(ahead(&all, "c", 30), 2);
        // A waiter whose process died no longer holds anyone up.
        let dead = [w("a", 10, true, "999999999"), w("b", 20, true, &me)];
        assert_eq!(ahead(&dead, "b", 20), 0);
    }
}
