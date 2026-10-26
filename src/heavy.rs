//! `reeve heavy`: queues heavy commands (builds, test suites, installs) so that many agents on
//! one machine don't run them all at once. There is no daemon: every waiter and runner is a JSON
//! file under the state dir, and a file lock held for a few milliseconds makes each claim atomic.

use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Set in the command's environment; a nested `reeve heavy` sees it and runs straight away.
pub const ENV: &str = "REEVE_HEAVY";
const POLL: Duration = Duration::from_millis(250);
/// How long a command may take to stop after a forwarded signal before it is killed.
const GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub pid: u32,
    /// The command's process group, once started. The slot stays taken while the group lives,
    /// even if reeve heavy itself was killed.
    pub group: Option<u32>,
    pub label: String,
    pub cwd: String,
    /// When it joined the queue (waiting) or started (running), in ms since the epoch.
    pub since: u64,
}

/// A ticket or slot file, locked for as long as its owner lives. An unlocked file belongs to a
/// process that is gone, whatever its pid is now used for.
pub struct Held {
    pub entry: Entry,
    path: PathBuf,
    _lock: File,
}

#[derive(Debug, PartialEq)]
pub enum Claim {
    Slot(Held),
    /// Still waiting, with this many waiters ahead.
    Ahead(usize),
}

impl std::fmt::Debug for Held {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        self.path.fmt(f)
    }
}

impl PartialEq for Held {
    fn eq(&self, other: &Held) -> bool {
        self.path == other.path
    }
}

pub struct Queue {
    root: PathBuf,
}

impl Queue {
    pub fn at(root: impl Into<PathBuf>) -> Queue {
        Queue { root: root.into() }
    }

    /// `REEVE_HEAVY_DIR`, else `$XDG_STATE_HOME/reeve/heavy`, else `~/.local/state/reeve/heavy`.
    pub fn default_root() -> PathBuf {
        let var = |k| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        var("REEVE_HEAVY_DIR").unwrap_or_else(|| {
            var("XDG_STATE_HOME")
                .unwrap_or_else(|| var("HOME").unwrap_or_default().join(".local/state"))
                .join("reeve/heavy")
        })
    }

    fn queue_dir(&self) -> PathBuf {
        self.root.join("queue")
    }

    fn slots_dir(&self) -> PathBuf {
        self.root.join("slots")
    }

    pub fn enqueue(&self, e: &Entry) -> io::Result<Held> {
        let name = format!("{:013}-{:010}.json", e.since, e.pid);
        put(&self.queue_dir().join(name), e)
    }

    /// Waiters in FIFO order. The order comes from the file names, so it is recomputed on every
    /// read and tickets are never renamed.
    pub fn waiting(&self) -> Vec<Entry> {
        live(&self.queue_dir())
    }

    pub fn running(&self) -> Vec<Entry> {
        live(&self.slots_dir())
    }

    /// Takes a slot for the ticket's owner if it is first in line and one is free.
    pub fn try_claim(&self, ticket: &mut Held, slots: usize) -> io::Result<Claim> {
        fs::create_dir_all(&self.root)?;
        // Without the lock, two waiters could both count a free slot before either takes it.
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.root.join("lock"))?;
        lock.lock()?;
        let e = ticket.entry.clone();
        let mut waiting = self.waiting();
        if !waiting.iter().any(|w| w.pid == e.pid) {
            *ticket = self.enqueue(&e)?;
            waiting = self.waiting();
        }
        match waiting.iter().position(|w| w.pid == e.pid) {
            Some(0) if self.running().len() < slots => {
                let slot = put(
                    &self.slots_dir().join(format!("{}.json", e.pid)),
                    &Entry {
                        since: now_ms(),
                        ..e
                    },
                )?;
                let _ = fs::remove_file(&ticket.path);
                Ok(Claim::Slot(slot))
            }
            n => Ok(Claim::Ahead(n.unwrap_or(waiting.len()))),
        }
    }

    /// Rewrites a slot, e.g. to record the command's process group.
    pub fn update(&self, slot: &mut Held, e: Entry) -> io::Result<()> {
        *slot = put(&slot.path, &e)?;
        Ok(())
    }

    /// Removes a ticket or slot.
    pub fn drop(&self, held: Held) {
        let _ = fs::remove_file(&held.path);
    }
}

/// Locked first, then written aside and renamed into place, so a reader never sees half a file
/// or an unlocked one.
fn put(path: &Path, e: &Entry) -> io::Result<Held> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dir.join(format!(".{name}.tmp"));
    let mut f = File::create(&tmp)?;
    f.lock()?;
    f.write_all(&serde_json::to_vec(e)?)?;
    fs::rename(&tmp, path)?;
    Ok(Held {
        entry: e.clone(),
        path: path.into(),
        _lock: f,
    })
}

/// Entries sorted by file name. Files whose owner is gone are removed.
fn live(dir: &Path) -> Vec<Entry> {
    let Ok(rd) = fs::read_dir(dir) else {
        return vec![];
    };
    let mut paths: Vec<PathBuf> = rd
        .filter_map(|d| d.ok().map(|d| d.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|p| {
            let f = File::open(&p).ok()?;
            let owned = matches!(f.try_lock(), Err(TryLockError::WouldBlock));
            let e = serde_json::from_reader::<_, Entry>(&f)
                .ok()
                .filter(|e| owned || e.group.is_some_and(group_alive));
            if e.is_none() {
                let _ = fs::remove_file(&p);
            }
            e
        })
        .collect()
}

fn group_alive(pgid: u32) -> bool {
    let Ok(pgid) = i32::try_from(pgid) else {
        return false;
    };
    pgid > 0
        && (unsafe { libc::kill(-pgid, 0) } == 0
            || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

pub fn default_slots(cores: usize) -> usize {
    (cores / 4).max(1)
}

#[derive(clap::Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct Args {
    #[command(subcommand)]
    sub: Option<Sub>,
    /// Shown by `reeve heavy ls` [default: the command]
    #[arg(long)]
    label: Option<String>,
    /// How many heavy commands may run at once [default: a quarter of the cores, at least 1]
    #[arg(long, env = "REEVE_HEAVY_SLOTS", value_parser = clap::value_parser!(u32).range(1..))]
    slots: Option<u32>,
    #[arg(last = true)]
    cmd: Vec<String>,
}

#[derive(clap::Subcommand)]
enum Sub {
    /// Show who runs and who waits, in order
    Ls,
}

pub fn main(args: Args) -> io::Result<i32> {
    let q = Queue::at(Queue::default_root());
    if let Some(Sub::Ls) = args.sub {
        print!("{}", ls(&q.running(), &q.waiting(), now_ms()));
        return Ok(0);
    }
    if args.cmd.is_empty() {
        return Err(io::Error::other(
            "nothing to run: reeve heavy [--label L] -- <command...>",
        ));
    }
    let slots = args.slots.map_or_else(
        || default_slots(std::thread::available_parallelism().map_or(1, |n| n.get())),
        |n| n as usize,
    );
    let label = args
        .label
        .unwrap_or_else(|| args.cmd.join(" ").chars().take(40).collect());
    run(&q, label, &args.cmd, slots)
}

static STOP: AtomicI32 = AtomicI32::new(0);
static HITS: AtomicI32 = AtomicI32::new(0);

extern "C" fn on_signal(sig: libc::c_int) {
    STOP.store(sig, Ordering::SeqCst);
    HITS.fetch_add(1, Ordering::SeqCst);
}

fn stopped() -> Option<i32> {
    Some(STOP.load(Ordering::SeqCst)).filter(|&s| s != 0)
}

fn run(q: &Queue, label: String, cmd: &[String], slots: usize) -> io::Result<i32> {
    let command = || {
        let mut c = Command::new(&cmd[0]);
        c.args(&cmd[1..]).env(ENV, "1");
        c
    };
    if std::env::var_os(ENV).is_some_and(|v| !v.is_empty()) {
        let err = command().exec();
        eprintln!("reeve heavy: could not start {}: {err}", cmd[0]);
        return Ok(127);
    }
    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        unsafe { libc::signal(sig, on_signal as *const () as libc::sighandler_t) };
    }
    let me = Entry {
        pid: std::process::id(),
        group: None,
        label,
        cwd: std::env::current_dir().map_or_else(|_| String::new(), |d| d.display().to_string()),
        since: now_ms(),
    };
    let mut ticket = q.enqueue(&me)?;
    let mut said = None;
    let mut slot = loop {
        if let Some(sig) = stopped() {
            q.drop(ticket);
            return Ok(128 + sig);
        }
        match q.try_claim(&mut ticket, slots)? {
            Claim::Slot(slot) => break slot,
            Claim::Ahead(n) if said != Some(n) => {
                eprintln!("reeve heavy: waiting for a slot: {n} ahead");
                said = Some(n);
            }
            Claim::Ahead(_) => {}
        }
        let until = Instant::now() + POLL;
        while Instant::now() < until && stopped().is_none() {
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    if let Some(sig) = stopped() {
        q.drop(slot);
        return Ok(128 + sig);
    }
    // In a terminal's foreground job the command shares our process group, so Ctrl-C, Ctrl-Z and
    // reads from the terminal work as usual. Elsewhere (agents) it gets its own group, which is
    // killed when it ends so no test workers outlive it.
    let interactive = unsafe { libc::isatty(0) == 1 && libc::tcgetpgrp(0) == libc::getpgrp() };
    let mut c = command();
    if !interactive {
        c.process_group(0);
    }
    let mut child = match c.spawn() {
        Ok(child) => child,
        Err(err) => {
            q.drop(slot);
            eprintln!("reeve heavy: could not start {}: {err}", cmd[0]);
            return Ok(127);
        }
    };
    let entry = Entry {
        group: (!interactive).then(|| child.id()),
        since: now_ms(),
        ..slot.entry.clone()
    };
    if let Err(err) = q.update(&mut slot, entry) {
        eprintln!("reeve heavy: could not record the command's group: {err}");
    }
    let target = if interactive {
        child.id() as i32
    } else {
        -(child.id() as i32)
    };
    let status = wait(&mut child, target);
    if !interactive {
        kill_group(target);
    }
    q.drop(slot);
    Ok(stopped().map_or_else(|| code(status), |sig| 128 + sig))
}

/// Waits for the command, passing on any signal reeve heavy gets. It is killed if it hasn't
/// stopped after `GRACE`, or at once on a second signal.
fn wait(child: &mut Child, target: i32) -> ExitStatus {
    let mut forwarded = None;
    let mut deadline = None;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return status;
        }
        if let Some(sig) = stopped().filter(|&s| forwarded != Some(s)) {
            forwarded = Some(sig);
            unsafe { libc::kill(target, sig) };
            deadline.get_or_insert(Instant::now() + GRACE);
        }
        if deadline.is_some_and(|d| Instant::now() > d) || HITS.load(Ordering::SeqCst) > 1 {
            unsafe { libc::kill(target, libc::SIGKILL) };
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Whatever the command left running in its group (test workers, watchers) goes with it.
fn kill_group(target: i32) {
    if unsafe { libc::kill(target, libc::SIGTERM) } != 0 {
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while unsafe { libc::kill(target, 0) } == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    unsafe { libc::kill(target, libc::SIGKILL) };
}

fn code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}

fn ls(running: &[Entry], waiting: &[Entry], now: u64) -> String {
    let rows: Vec<(&str, &Entry)> = running
        .iter()
        .map(|e| ("running", e))
        .chain(waiting.iter().map(|e| ("waiting", e)))
        .collect();
    if rows.is_empty() {
        return "no heavy commands\n".into();
    }
    let width = rows
        .iter()
        .map(|(_, e)| e.label.chars().count())
        .max()
        .unwrap_or(0);
    rows.iter()
        .map(|(state, e)| {
            format!(
                "{state}  {:<width$}  pid {:<6}  {:>5}  {}\n",
                e.label,
                e.pid,
                age(now.saturating_sub(e.since)),
                e.cwd
            )
        })
        .collect()
}

fn age(ms: u64) -> String {
    match ms / 1000 {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s => format!("{}h{:02}m", s / 3600, s / 60 % 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(pid: u32, label: &str, since: u64) -> Entry {
        Entry {
            pid,
            group: None,
            label: label.into(),
            cwd: "/w".into(),
            since,
        }
    }

    fn labels(v: Vec<Entry>) -> Vec<String> {
        v.into_iter().map(|e| e.label).collect()
    }

    fn queue() -> (tempfile::TempDir, Queue) {
        let d = tempfile::tempdir().unwrap();
        let q = Queue::at(d.path());
        (d, q)
    }

    fn slot(c: Claim) -> Held {
        match c {
            Claim::Slot(s) => s,
            other => panic!("expected a slot, got {other:?}"),
        }
    }

    /// What a crashed owner leaves behind. Unlocking rather than dropping keeps a test thread
    /// that forks meanwhile (and so shares the file until it execs) from keeping the lock.
    fn crash(h: Held) {
        h._lock.unlock().unwrap();
    }

    fn count(dir: PathBuf) -> usize {
        fs::read_dir(dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|x| x == "json")
            })
            .count()
    }

    #[test]
    fn a_free_slot_is_claimed_and_released() {
        let (_d, q) = queue();
        let mut t = q.enqueue(&entry(1, "build", 1)).unwrap();
        let s = slot(q.try_claim(&mut t, 1).unwrap());
        assert!(q.waiting().is_empty());
        assert_eq!(labels(q.running()), ["build"]);
        q.drop(s);
        assert!(q.running().is_empty());
    }

    #[test]
    fn with_every_slot_taken_the_first_waiter_waits() {
        let (_d, q) = queue();
        let mut holder = q.enqueue(&entry(1, "holder", 1)).unwrap();
        let _held = slot(q.try_claim(&mut holder, 1).unwrap());
        let mut t = q.enqueue(&entry(2, "next", 2)).unwrap();
        assert_eq!(q.try_claim(&mut t, 1).unwrap(), Claim::Ahead(0));
        slot(q.try_claim(&mut t, 2).unwrap());
    }

    #[test]
    fn slots_and_tickets_whose_owner_is_gone_are_reclaimed() {
        let (_d, q) = queue();
        let mut t = q.enqueue(&entry(1, "gone", 1)).unwrap();
        crash(slot(q.try_claim(&mut t, 1).unwrap()));
        crash(q.enqueue(&entry(2, "gone-too", 2)).unwrap());
        let mut t = q.enqueue(&entry(3, "next", 3)).unwrap();
        let _s = slot(q.try_claim(&mut t, 1).unwrap());
        assert_eq!(labels(q.running()), ["next"]);
        assert_eq!(count(q.slots_dir()), 1);
        assert_eq!(count(q.queue_dir()), 0);
    }

    #[test]
    fn a_live_pid_does_not_keep_an_unlocked_file() {
        let (_d, q) = queue();
        crash(
            q.enqueue(&entry(std::process::id(), "reused pid", 1))
                .unwrap(),
        );
        assert!(q.waiting().is_empty());
    }

    #[test]
    fn a_slot_whose_runner_died_stays_taken_while_its_command_group_runs() {
        let (_d, q) = queue();
        let mut t = q.enqueue(&entry(1, "orphan", 1)).unwrap();
        let mut s = slot(q.try_claim(&mut t, 1).unwrap());
        let group = Some(unsafe { libc::getpgrp() } as u32);
        let e = Entry {
            group,
            ..s.entry.clone()
        };
        q.update(&mut s, e).unwrap();
        crash(s);
        assert_eq!(labels(q.running()), ["orphan"]);
    }

    #[test]
    fn waiters_go_in_fifo_order_even_when_a_slot_is_free() {
        let (_d, q) = queue();
        let mut second = q.enqueue(&entry(2, "second", 2000)).unwrap();
        let mut first = q.enqueue(&entry(1, "first", 1000)).unwrap();
        assert_eq!(labels(q.waiting()), ["first", "second"]);
        assert_eq!(q.try_claim(&mut second, 2).unwrap(), Claim::Ahead(1));
        let _a = slot(q.try_claim(&mut first, 2).unwrap());
        let _b = slot(q.try_claim(&mut second, 2).unwrap());
    }

    #[test]
    fn a_waiter_whose_ticket_vanished_rejoins_the_queue() {
        let (_d, q) = queue();
        let mut t = q.enqueue(&entry(2, "lost", 1)).unwrap();
        fs::remove_file(&t.path).unwrap();
        let _first = q.enqueue(&entry(1, "first", 0)).unwrap();
        assert_eq!(q.try_claim(&mut t, 1).unwrap(), Claim::Ahead(1));
        assert_eq!(labels(q.waiting()), ["first", "lost"]);
    }

    #[test]
    fn default_slots_is_a_quarter_of_the_cores_and_at_least_one() {
        assert_eq!(default_slots(16), 4);
        assert_eq!(default_slots(10), 2);
        assert_eq!(default_slots(2), 1);
        assert_eq!(default_slots(0), 1);
    }
}
