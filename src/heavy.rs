//! `reeve heavy`: queues heavy commands (builds, test suites, installs) so that many agents on
//! one machine don't run them all at once. There is no daemon: every waiter and runner is a JSON
//! file under the state dir, and a file lock held for a few milliseconds makes each claim atomic.
//!
//! Outside a terminal the job is handed to a runner process of its own, detached from the caller's
//! session and process tree, which waits, runs the command and records its output and outcome under
//! `jobs/`. The caller only follows it, so a caller that is killed (by an agent CLI reaping
//! background tasks under memory pressure, say) loses nothing: the job keeps its place, and the
//! outcome is in `reeve heavy ls`, `reeve heavy follow`, and the worker's inbox.

use crate::events::Event;
use crate::mem::{self, Mem};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Set in the command's environment; a nested `reeve heavy` sees it and runs straight away.
pub const ENV: &str = "REEVE_HEAVY";
const POLL: Duration = Duration::from_millis(250);
/// How long a command may take to stop after a forwarded signal before it is killed.
const GRACE: Duration = Duration::from_secs(5);
const BACKGROUND_HINT: &str = " (agents: start reeve heavy in the background and keep working; \
                               you are told when it ends)";
/// Finished jobs' logs and outcomes are kept this long.
const KEEP_MS: u64 = 24 * 3_600_000;
/// How long a runner may take to say it is up before its caller gives up on it.
const RUNNER_START: Duration = Duration::from_secs(10);

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
    /// The `--lock` key this run holds, or waits for in the lock list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock: Option<String>,
    /// The run's time limit once it runs, in ms; missing means none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_ms: Option<u64>,
    /// The detached job this entry belongs to, under `jobs/`; missing for a run in a terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
    /// The named worker that started it, so `reeve watch` knows an idle worker is waiting on
    /// its own job; missing outside a worker or for a run in a terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
}

/// A ticket or slot file, locked for as long as its owner lives. An unlocked file belongs to a
/// process that is gone, whatever its pid is now used for.
pub struct Held {
    pub entry: Entry,
    path: PathBuf,
    _lock: File,
}

/// Why the first waiter in line didn't get a slot, or how many are ahead of a later one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Wait {
    Ahead(usize),
    /// First in line, with every slot taken by this many runs.
    Slots(usize),
    /// First in line with a slot free, but less memory free than the floor, in GB.
    Memory {
        free: f64,
        floor: f64,
    },
}

impl Wait {
    /// Lines are printed when this changes, not every time the free memory figure moves.
    fn kind(&self) -> (u8, usize) {
        match *self {
            Wait::Ahead(n) => (0, n),
            Wait::Slots(n) => (1, n),
            Wait::Memory { .. } => (2, 0),
        }
    }
}

impl std::fmt::Display for Wait {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match *self {
            Wait::Ahead(n) => write!(f, "waiting for a slot: {n} ahead"),
            Wait::Slots(n) => write!(f, "waiting for a slot: 0 ahead, {n} running"),
            Wait::Memory { free, floor } => write!(
                f,
                "waiting for memory: 0 ahead, {free:.1} GB free, below the {floor:.1} GB floor \
                 (REEVE_HEAVY_MIN_FREE)"
            ),
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum Claim {
    Slot(Held),
    Wait(Wait),
}

/// What a waiter at the head of the line must find before it gets a slot.
pub struct Gate<'a> {
    pub slots: usize,
    /// GB that must stay free once one heavy run is going; 0 turns the check off.
    pub min_free_gb: f64,
    pub probe: &'a dyn Fn() -> Option<Mem>,
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

/// An exclusive `--lock` key. Dropping it, or the process ending, releases it.
pub struct KeyLock {
    file: File,
}

impl KeyLock {
    /// Lets the command (and what it starts) inherit the lock, so that it stays taken while
    /// any of them lives, even if reeve heavy is killed.
    fn inherit(&self) {
        use std::os::fd::AsRawFd;
        let fd = self.file.as_raw_fd();
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
        }
    }
}

/// A file name for any key: letters, digits, `.`, `_` and `-` as they are, the rest as `%XX`.
fn encode_key(key: &str) -> String {
    key.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'_' | b'-' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
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

    /// One flock'd file per key, never removed, so there is no inode to race on.
    fn locks_dir(&self) -> PathBuf {
        self.root.join("locks")
    }

    /// Tickets of runs waiting for a lock key, for `ls`.
    fn lockwait_dir(&self) -> PathBuf {
        self.root.join("lockwait")
    }

    /// Why each waiter waits, one small file per pid, for `ls`. Kept apart from the tickets so
    /// that rewriting it never touches a locked file.
    fn why_dir(&self) -> PathBuf {
        self.root.join("why")
    }

    /// One directory per detached job: what to run, its output, and how it ended.
    fn jobs_dir(&self) -> PathBuf {
        self.root.join("jobs")
    }

    /// Joins the list of runs waiting for `e.lock`.
    pub fn wait_for_lock(&self, e: &Entry) -> io::Result<Held> {
        let name = format!("{:013}-{:010}.json", e.since, e.pid);
        put(&self.lockwait_dir().join(name), e)
    }

    pub fn lock_waiting(&self) -> Vec<Entry> {
        live(&self.lockwait_dir())
    }

    /// Takes the exclusive lock on `key` for `me`, or says who holds it. The lock is the OS's
    /// flock on a file, so it ends with the last process holding the file open, however that
    /// process ends.
    pub fn try_lock(&self, key: &str, me: &Entry) -> io::Result<Result<KeyLock, Option<Entry>>> {
        let dir = self.locks_dir();
        fs::create_dir_all(&dir)?;
        let mut f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join(format!("{}.lock", encode_key(key))))?;
        match f.try_lock() {
            Ok(()) => {
                f.set_len(0)?;
                f.write_all(&serde_json::to_vec(me)?)?;
                Ok(Ok(KeyLock { file: f }))
            }
            Err(TryLockError::WouldBlock) => {
                // The holder writes its entry just after locking; until then it is unknown.
                Ok(Err(serde_json::from_reader::<_, Entry>(&f).ok()))
            }
            Err(TryLockError::Error(e)) => Err(e),
        }
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

    /// The named workers with a job running, queued or waiting for a lock: an idle one of
    /// these waits on its own job, which isn't news for whoever follows it.
    pub fn busy_workers(&self) -> std::collections::HashSet<String> {
        [self.running(), self.waiting(), self.lock_waiting()]
            .into_iter()
            .flatten()
            .filter_map(|e| e.worker)
            .collect()
    }

    /// Takes a slot for the ticket's owner if it is first in line, a slot is free, and, when
    /// another heavy run is already going, enough memory is free. The first run always gets
    /// its slot, so a machine that is short of memory still makes progress, one run at a time.
    pub fn try_claim(&self, ticket: &mut Held, gate: &Gate) -> io::Result<Claim> {
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
        let Some(0) = waiting.iter().position(|w| w.pid == e.pid) else {
            let n = waiting.iter().position(|w| w.pid == e.pid);
            return Ok(Claim::Wait(Wait::Ahead(n.unwrap_or(waiting.len()))));
        };
        let running = self.running().len();
        if running >= gate.slots {
            return Ok(Claim::Wait(Wait::Slots(running)));
        }
        if running > 0
            && gate.min_free_gb > 0.0
            && let Some(m) = (gate.probe)()
            && m.free_gb() < gate.min_free_gb
        {
            return Ok(Claim::Wait(Wait::Memory {
                free: m.free_gb(),
                floor: gate.min_free_gb,
            }));
        }
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

    /// Rewrites a slot, e.g. to record the command's process group.
    pub fn update(&self, slot: &mut Held, e: Entry) -> io::Result<()> {
        *slot = put(&slot.path, &e)?;
        Ok(())
    }

    /// Removes a ticket or slot.
    pub fn drop(&self, held: Held) {
        let _ = fs::remove_file(self.why_dir().join(held.entry.pid.to_string()));
        let _ = fs::remove_file(&held.path);
    }

    fn say_why(&self, pid: u32, why: &str) {
        let dir = self.why_dir();
        let _ = fs::create_dir_all(&dir);
        let tmp = dir.join(format!(".{pid}.tmp"));
        if fs::write(&tmp, why).is_ok() {
            let _ = fs::rename(&tmp, dir.join(pid.to_string()));
        }
    }

    fn why(&self, pid: u32) -> Option<String> {
        fs::read_to_string(self.why_dir().join(pid.to_string())).ok()
    }

    /// Finished detached jobs nobody followed to the end and nobody has looked at since.
    fn unseen(&self, now: u64) -> Vec<(String, Job, Outcome)> {
        let Ok(rd) = fs::read_dir(self.jobs_dir()) else {
            return vec![];
        };
        let mut v: Vec<(String, Job, Outcome)> = rd
            .filter_map(|d| {
                let d = d.ok()?.path();
                let o: Outcome = read_json(&d.join("result.json"))?;
                if o.followed || d.join("seen").exists() || now.saturating_sub(o.finished) > KEEP_MS
                {
                    return None;
                }
                let j: Job = read_json(&d.join("job.json"))?;
                Some((d.file_name()?.to_string_lossy().into_owned(), j, o))
            })
            .collect();
        v.sort_by_key(|(_, _, o)| o.finished);
        v
    }

    /// Removes jobs that ended, or whose runner is gone, more than a day ago.
    fn prune(&self, now: u64) {
        let Ok(rd) = fs::read_dir(self.jobs_dir()) else {
            return;
        };
        for d in rd.filter_map(|d| d.ok().map(|d| d.path())) {
            let old = |p: &Path| {
                fs::metadata(p)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .is_some_and(|t| now.saturating_sub(t.as_millis() as u64) > KEEP_MS)
            };
            let done = d.join("result.json");
            if (done.exists() && old(&done)) || (!done.exists() && !runner_alive(&d) && old(&d)) {
                let _ = fs::remove_dir_all(&d);
            }
        }
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

/// Free memory, in GB, below which a second heavy run waits.
pub const DEFAULT_MIN_FREE_GB: f64 = 2.0;

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
    /// GB of memory that must be free for a run to start while another is going; 0 turns the
    /// check off. The first run always starts
    #[arg(
        long,
        env = "REEVE_HEAVY_MIN_FREE",
        value_name = "GB",
        default_value_t = DEFAULT_MIN_FREE_GB,
        value_parser = parse_gb
    )]
    min_free: f64,
    /// Run only while no other `reeve heavy` holds KEY, on any agent on this machine. For a
    /// resource that can't be shared; prefer making runs independent (own database or port)
    #[arg(long, value_name = "KEY", value_parser = non_empty)]
    lock: Option<String>,
    /// Longest the command may run once it has a slot (e.g. 45m, 90s). At the limit its process
    /// group is stopped and reeve heavy exits 124. Start visual/e2e runs narrow rather than
    /// raising this; prefer a large value to `none`, which removes the limit
    #[arg(
        long,
        env = "REEVE_HEAVY_TIMEOUT",
        value_name = "DURATION",
        default_value = DEFAULT_TIMEOUT,
        value_parser = parse_limit
    )]
    timeout: Limit,
    #[arg(last = true)]
    cmd: Vec<String>,
}

/// Longest successful heavy run in the event history was 22 min, so nothing that worked is cut.
pub const DEFAULT_TIMEOUT: &str = "30m";

/// A time limit; None is `none`.
type Limit = Option<Duration>;

fn parse_limit(s: &str) -> Result<Limit, String> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (n, unit) = s.split_at(split);
    let n: u64 = n
        .parse()
        .map_err(|_| format!("`{s}` is not a duration like 45m, 90s or 2h, or `none`"))?;
    let ms = match unit {
        "ms" => n,
        "s" => n.saturating_mul(1000),
        "m" => n.saturating_mul(60_000),
        "h" => n.saturating_mul(3_600_000),
        _ => {
            return Err(format!(
                "`{s}` needs a unit: ms, s, m or h (e.g. 45m), or `none`"
            ));
        }
    };
    if ms == 0 {
        return Err("the limit can't be zero; use `none` for no limit".into());
    }
    Ok(Some(Duration::from_millis(ms)))
}

fn parse_gb(s: &str) -> Result<f64, String> {
    let s = s.trim();
    let n = s.strip_suffix(['G', 'g']).unwrap_or(s);
    match n.parse::<f64>() {
        Ok(gb) if gb.is_finite() && gb >= 0.0 => Ok(gb),
        _ => Err(format!("`{s}` is not a number of GB like 2 or 1.5")),
    }
}

fn show_limit(ms: u64) -> String {
    match ms {
        m if m % 3_600_000 == 0 => format!("{}h", m / 3_600_000),
        m if m % 60_000 == 0 => format!("{}m", m / 60_000),
        m if m % 1000 == 0 => format!("{}s", m / 1000),
        m => format!("{m}ms"),
    }
}

fn non_empty(s: &str) -> Result<String, String> {
    if s.is_empty() {
        Err("the key can't be empty".into())
    } else {
        Ok(s.into())
    }
}

#[derive(clap::Subcommand)]
enum Sub {
    /// Show who runs and who waits, in order, why each waits, and jobs that ended unwatched
    Ls,
    /// Print a job's output as it comes and exit with its code, also once it has ended
    Follow {
        /// The job's id, from `reeve heavy ls` or the line its caller printed
        id: String,
    },
    /// Stop a run or take a waiter out of the queue, by the pid `reeve heavy ls` shows
    Stop { pid: u32 },
    /// The detached runner a caller starts; not for direct use
    #[command(name = "__run", hide = true)]
    Runner { dir: PathBuf },
}

/// What a caller hands its runner, in `jobs/ID/job.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Job {
    cmd: Vec<String>,
    label: String,
    /// The event log's label: the `--label`, else only the program's name.
    ev_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lock: Option<String>,
    slots: usize,
    min_free_gb: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit_ms: Option<u64>,
    cwd: String,
}

/// How a detached job ended, in `jobs/ID/result.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Outcome {
    code: i32,
    finished: u64,
    /// Whether a caller was following it when it ended.
    followed: bool,
}

fn read_json<T: serde::de::DeserializeOwned>(p: &Path) -> Option<T> {
    serde_json::from_slice(&fs::read(p).ok()?).ok()
}

fn write_json<T: Serialize>(p: &Path, v: &T) -> io::Result<()> {
    let tmp = p.with_extension("tmp");
    fs::write(&tmp, serde_json::to_vec(v)?)?;
    fs::rename(tmp, p)
}

/// Whether a job's runner still holds `runner.lock`.
fn runner_alive(dir: &Path) -> bool {
    File::open(dir.join("runner.lock"))
        .is_ok_and(|f| matches!(f.try_lock(), Err(TryLockError::WouldBlock)))
}

pub fn main(
    args: Args,
    ev: &mut Event,
    worker: impl FnOnce() -> Option<String>,
) -> io::Result<i32> {
    let q = Queue::at(Queue::default_root());
    match args.sub {
        Some(Sub::Ls) => {
            let now = now_ms();
            let (running, waiting, lock_waiting) = (q.running(), q.waiting(), q.lock_waiting());
            let why = |pid| q.why(pid);
            print!(
                "{}",
                ls(
                    &running,
                    &waiting,
                    &lock_waiting,
                    &why,
                    &q.unseen(now),
                    &q.jobs_dir(),
                    now
                )
            );
            return Ok(0);
        }
        Some(Sub::Follow { id }) => {
            if !msg_safe(&id) || !q.jobs_dir().join(&id).join("job.json").exists() {
                return Err(io::Error::other(format!(
                    "no heavy job {id}; see `reeve heavy ls`"
                )));
            }
            return follow(&q.jobs_dir().join(&id), None, None);
        }
        Some(Sub::Stop { pid }) => {
            let all = [q.running(), q.waiting(), q.lock_waiting()].concat();
            let Some(e) = all.iter().find(|e| e.pid == pid) else {
                return Err(io::Error::other(format!(
                    "no heavy run or waiter with pid {pid}; see `reeve heavy ls`"
                )));
            };
            unsafe { libc::kill(pid as i32, libc::SIGTERM) };
            println!("stopping {} (pid {pid})", e.label);
            return Ok(0);
        }
        Some(Sub::Runner { dir }) => {
            ev.skip = true;
            return runner(&q, &dir, worker);
        }
        None => {}
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
    // Without a label only the program's name is logged, not its arguments.
    let ev_label = args.label.clone().unwrap_or_else(|| {
        Path::new(&args.cmd[0])
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    });
    ev.label = Some(ev_label.clone());
    let label = args
        .label
        .unwrap_or_else(|| args.cmd.join(" ").chars().take(40).collect());
    if std::env::var_os(ENV).is_some_and(|v| !v.is_empty()) {
        nested(&args.cmd, ev);
    }
    // In a terminal's foreground job the command runs right here and shares our process group,
    // so Ctrl-C, Ctrl-Z and reads from the terminal work as usual. Elsewhere (agents) a detached
    // runner owns it, and the command gets its own group, killed when it ends so no test workers
    // outlive it.
    let interactive = unsafe { libc::isatty(0) == 1 && libc::tcgetpgrp(0) == libc::getpgrp() };
    let gate = Gate {
        slots,
        min_free_gb: args.min_free,
        probe: &mem::probe,
    };
    if interactive {
        let me = entry(std::process::id(), label, args.lock.clone(), None);
        return run(&q, me, &args.cmd, &gate, args.timeout, true, ev);
    }
    let job = Job {
        cmd: args.cmd,
        label,
        ev_label,
        lock: args.lock,
        slots,
        min_free_gb: args.min_free,
        limit_ms: args.timeout.map(|l| l.as_millis() as u64),
        cwd: cwd(),
    };
    // The runner logs the run's event, whether or not anyone still follows it.
    ev.skip = true;
    let (dir, watch) = submit(&q, &job)?;
    follow(&dir, Some(&job), Some(watch))
}

fn msg_safe(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
}

fn cwd() -> String {
    std::env::current_dir().map_or_else(|_| String::new(), |d| d.display().to_string())
}

fn entry(pid: u32, label: String, lock: Option<String>, job: Option<String>) -> Entry {
    Entry {
        pid,
        group: None,
        label,
        cwd: cwd(),
        since: now_ms(),
        lock,
        limit_ms: None,
        job,
        worker: None,
    }
}

/// Inside another `reeve heavy`'s command: replaces itself with the command, which doesn't come
/// back, so this run is logged now, without an exit code.
fn nested(cmd: &[String], ev: &mut Event) -> ! {
    ev.ms = crate::events::now_ms().saturating_sub(ev.ts);
    crate::events::log(ev);
    let err = Command::new(&cmd[0]).args(&cmd[1..]).env(ENV, "1").exec();
    eprintln!("reeve heavy: could not start {}: {err}", cmd[0]);
    std::process::exit(127);
}

/// Writes the job's dir and starts its runner outside the caller's session and process tree:
/// what kills the caller's process group or descendants doesn't reach it.
fn submit(q: &Queue, job: &Job) -> io::Result<(PathBuf, File)> {
    let now = now_ms();
    q.prune(now);
    let id = format!("{now}-{}", std::process::id());
    let dir = q.jobs_dir().join(&id);
    fs::create_dir_all(&dir)?;
    write_json(&dir.join("job.json"), job)?;
    // Held from before the runner starts, so a job that ends at once still counts as followed.
    let watch = watch_lock(&dir)?;
    let log = |name| {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(name))
    };
    let (out, err) = (log("out.log")?, log("err.log")?);
    let mut c = Command::new(std::env::current_exe()?);
    c.args(["heavy", "__run"])
        .arg(&dir)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    unsafe {
        c.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            // The runner is the grandchild, so it isn't a descendant anything walks down to.
            match libc::fork() {
                -1 => Err(io::Error::last_os_error()),
                0 => Ok(()),
                _ => libc::_exit(0),
            }
        });
    }
    c.spawn()?.wait()?;
    Ok((dir, watch))
}

/// The detached runner: waits for the job's turn, runs it, and records how it ended. It logs the
/// run's event, and if nobody was following when it ended, tells the worker that started it.
/// The worker is looked up here, not by the caller, as that can mean asking the agent CLI; the
/// runner has the caller's environment to look it up from.
fn runner(q: &Queue, dir: &Path, worker: impl FnOnce() -> Option<String>) -> io::Result<i32> {
    let job: Job = read_json(&dir.join("job.json"))
        .ok_or_else(|| io::Error::other(format!("no job in {}", dir.display())))?;
    let id = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut alive = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("runner.lock"))?;
    alive.lock()?;
    alive.set_len(0)?;
    alive.write_all(std::process::id().to_string().as_bytes())?;
    let mut ev = Event::start("heavy");
    ev.label = Some(job.ev_label.clone());
    let gate = Gate {
        slots: job.slots,
        min_free_gb: job.min_free_gb,
        probe: &mem::probe,
    };
    // Looked up before the run, in this detached runner, so the caller never waits on it and
    // `reeve watch` can tell the worker waits on its own job.
    let worker = worker();
    let me = Entry {
        worker: worker.clone(),
        ..entry(
            std::process::id(),
            job.label.clone(),
            job.lock.clone(),
            Some(id.clone()),
        )
    };
    let limit = job.limit_ms.map(Duration::from_millis);
    let code = run(q, me, &job.cmd, &gate, limit, false, &mut ev).unwrap_or_else(|err| {
        eprintln!("reeve heavy: {err}");
        1
    });
    let followed = File::open(dir.join("follow.lock"))
        .is_ok_and(|f| matches!(f.try_lock(), Err(TryLockError::WouldBlock)));
    let worker = if followed { None } else { worker };
    ev.heavy_unfollowed = (!followed).then_some(true);
    ev.worker = worker.clone();
    ev.finish(code, None);
    crate::events::log(&ev);
    let finished = now_ms();
    write_json(
        &dir.join("result.json"),
        &Outcome {
            code,
            finished,
            followed,
        },
    )?;
    if let Some(w) = &worker {
        tell(w, &id, &job, code);
    }
    Ok(code)
}

/// A line in the worker's inbox: the caller that would have reported this is gone.
fn tell(worker: &str, id: &str, job: &Job, code: i32) {
    let body = format!(
        "reeve heavy: {} ended with exit {code} after its caller stopped following it; \
         `reeve heavy follow {id}` prints its output",
        job.label
    );
    let now = crate::msg::now_secs();
    let sent = crate::msg::Store::open().and_then(|s| {
        let m = crate::msg::compose("local", worker, &body, None, Some(KEEP_MS / 1000), now)
            .map_err(io::Error::other)?;
        crate::msg::write_atomic(&s.inbox(), &m)
    });
    if let Err(err) = sent {
        eprintln!("reeve heavy: could not tell {worker}: {err}");
    }
}

/// Copies a job's output to this process's stdout and stderr as it comes, and exits with the
/// job's code. SIGINT is passed on to the runner (which stops the job); SIGTERM and SIGHUP only
/// stop the following, and the job runs on: those are what reaps a background task.
fn follow(dir: &Path, job: Option<&Job>, watch: Option<File>) -> io::Result<i32> {
    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        unsafe { libc::signal(sig, on_signal as *const () as libc::sighandler_t) };
    }
    let id = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let _watch = match watch {
        Some(w) => w,
        None => watch_lock(dir)?,
    };
    if dir.join("result.json").exists() {
        let _ = File::create(dir.join("seen"));
    }
    let mut out = Tail::open(&dir.join("out.log"));
    let mut err = Tail::open(&dir.join("err.log"));
    let started = Instant::now();
    let mut runner_pid: Option<i32> = None;
    let mut forwarded = false;
    loop {
        out.pump(&mut io::stdout());
        err.pump(&mut io::stderr());
        if let Some(o) = read_json::<Outcome>(&dir.join("result.json")) {
            out.pump(&mut io::stdout());
            err.pump(&mut io::stderr());
            return Ok(o.code);
        }
        runner_pid = runner_pid.or_else(|| {
            fs::read_to_string(dir.join("runner.lock"))
                .ok()
                .and_then(|s| s.trim().parse().ok())
        });
        match stopped() {
            Some(libc::SIGINT) => {
                if let (false, Some(pid)) = (forwarded, runner_pid) {
                    unsafe { libc::kill(pid, libc::SIGINT) };
                    forwarded = true;
                }
            }
            Some(sig) => {
                let label = job.map_or_else(String::new, |j| format!("{} ", j.label));
                eprintln!(
                    "reeve heavy: stopped following; job {label}{id} keeps its place and runs. \
                     `reeve heavy ls` shows it and how it ends, `reeve heavy follow {id}` prints \
                     its output and exit code, `reeve heavy stop PID` stops it"
                );
                return Ok(128 + sig);
            }
            None => {}
        }
        let up = runner_pid.is_some() || started.elapsed() < RUNNER_START;
        if !up || (runner_pid.is_some() && !runner_alive(dir)) {
            if let Some(o) = read_json::<Outcome>(&dir.join("result.json")) {
                out.pump(&mut io::stdout());
                err.pump(&mut io::stderr());
                return Ok(o.code);
            }
            eprintln!(
                "reeve heavy: the runner of job {id} is gone without a result; its output is in {}",
                dir.display()
            );
            return Ok(1);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Taken by whoever follows a job, so its runner can tell at the end whether anyone still does.
/// A second follower of the same job reads along without it.
fn watch_lock(dir: &Path) -> io::Result<File> {
    let f = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("follow.lock"))?;
    let _ = f.try_lock();
    Ok(f)
}

/// Reads a growing file from where it last stopped.
struct Tail {
    file: Option<File>,
    path: PathBuf,
    at: u64,
}

impl Tail {
    fn open(path: &Path) -> Tail {
        Tail {
            file: None,
            path: path.into(),
            at: 0,
        }
    }

    fn pump(&mut self, to: &mut impl Write) {
        if self.file.is_none() {
            self.file = File::open(&self.path).ok();
        }
        let Some(f) = &mut self.file else { return };
        let mut buf = Vec::new();
        if f.seek(SeekFrom::Start(self.at)).is_ok() && f.read_to_end(&mut buf).is_ok() {
            self.at += buf.len() as u64;
            // A reader that went away (a closed pipe) doesn't stop the job.
            let _ = to.write_all(&buf).and_then(|_| to.flush());
        }
    }
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

/// Waits for the lock and a slot, then runs the command: in a terminal (`interactive`) in our
/// own process group, else in one of its own.
fn run(
    q: &Queue,
    me: Entry,
    cmd: &[String],
    gate: &Gate,
    limit: Limit,
    interactive: bool,
    ev: &mut Event,
) -> io::Result<i32> {
    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        unsafe { libc::signal(sig, on_signal as *const () as libc::sighandler_t) };
    }
    let queued = Instant::now();
    // The key comes first, so that runs waiting for it don't hold slots others could use. It is
    // released when `_key` drops, on every way out of here.
    let key = match &me.lock {
        Some(k) => match take_lock(q, k, &me, interactive)? {
            Some(held) => Some(held),
            None => {
                ev.heavy_wait_ms = Some(queued.elapsed().as_millis() as u64);
                return Ok(128 + stopped().unwrap_or(0));
            }
        },
        None => None,
    };
    let mut ticket = q.enqueue(&me)?;
    let mut said: Option<(u8, usize)> = None;
    let mut shown = String::new();
    let mut slot = loop {
        if let Some(sig) = stopped() {
            ev.heavy_wait_ms = Some(queued.elapsed().as_millis() as u64);
            q.drop(ticket);
            return Ok(128 + sig);
        }
        match q.try_claim(&mut ticket, gate)? {
            Claim::Slot(slot) => {
                ev.heavy_wait_ms = Some(queued.elapsed().as_millis() as u64);
                let _ = fs::remove_file(q.why_dir().join(me.pid.to_string()));
                break slot;
            }
            Claim::Wait(w) => {
                let why = w.to_string();
                if why != shown {
                    q.say_why(me.pid, &why);
                    shown = why;
                }
                if said != Some(w.kind()) {
                    // An agent sees this line only once the command is done; it is told once
                    // so that the next call goes better.
                    let hint = if interactive || said.is_some() {
                        ""
                    } else {
                        BACKGROUND_HINT
                    };
                    eprintln!("reeve heavy: {w}{hint}");
                    said = Some(w.kind());
                }
            }
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
    let mut c = Command::new(&cmd[0]);
    c.args(&cmd[1..]).env(ENV, "1");
    if !interactive {
        c.process_group(0);
        if let Some(key) = &key {
            key.inherit();
        }
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
        limit_ms: limit.map(|l| l.as_millis() as u64),
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
    let ran = Instant::now();
    let (status, timed_out) = wait(&mut child, target, limit.map(|l| ran + l));
    ev.heavy_run_ms = Some(ran.elapsed().as_millis() as u64);
    if let Some(l) = limit {
        ev.heavy_limit_ms = Some(l.as_millis() as u64);
        ev.timed_out = timed_out.then_some(true);
    }
    if !interactive {
        kill_group(target);
    }
    q.drop(slot);
    if let (true, Some(l)) = (timed_out, limit) {
        eprintln!(
            "reeve heavy: hit the time limit of {} and stopped the command; narrow the run \
             (only the affected suite or spec) or pass --timeout DURATION|none",
            show_limit(l.as_millis() as u64)
        );
        if stopped().is_none() {
            return Ok(124);
        }
    }
    Ok(stopped().map_or_else(|| code(status), |sig| 128 + sig))
}

/// Waits for the lock on `key`, saying who holds it. None if a signal came first.
fn take_lock(q: &Queue, key: &str, me: &Entry, interactive: bool) -> io::Result<Option<KeyLock>> {
    let mut ticket = None;
    let mut said: Option<Option<u32>> = None;
    loop {
        if stopped().is_some() {
            ticket.into_iter().for_each(|t| q.drop(t));
            return Ok(None);
        }
        match q.try_lock(key, me)? {
            Ok(held) => {
                ticket.into_iter().for_each(|t| q.drop(t));
                return Ok(Some(held));
            }
            Err(holder) => {
                if ticket.is_none() {
                    ticket = Some(q.wait_for_lock(me)?);
                }
                let who = holder.as_ref().map(|h| h.pid);
                if said != Some(who) {
                    let hint = if interactive || said.is_some() {
                        ""
                    } else {
                        BACKGROUND_HINT
                    };
                    let by = holder.map_or_else(String::new, |h| {
                        format!(
                            ", held by {} (pid {}, {})",
                            h.label,
                            h.pid,
                            age(now_ms().saturating_sub(h.since))
                        )
                    });
                    eprintln!("reeve heavy: waiting for lock {key}{by}{hint}");
                    said = Some(who);
                }
            }
        }
        let until = Instant::now() + POLL;
        while Instant::now() < until && stopped().is_none() {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// Waits for the command, passing on any signal reeve heavy gets. It is killed if it hasn't
/// stopped after `GRACE`, or at once on a second signal. At `limit` it is stopped the same way
/// as if signalled; the flag says that happened.
fn wait(child: &mut Child, target: i32, limit: Option<Instant>) -> (ExitStatus, bool) {
    let mut forwarded = None;
    let mut deadline = None;
    let mut timed_out = false;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return (status, timed_out);
        }
        if !timed_out && limit.is_some_and(|l| Instant::now() >= l) {
            timed_out = true;
            unsafe { libc::kill(target, libc::SIGTERM) };
            deadline.get_or_insert(Instant::now() + GRACE);
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

fn ls(
    running: &[Entry],
    waiting: &[Entry],
    lock_waiting: &[Entry],
    why: &dyn Fn(u32) -> Option<String>,
    unseen: &[(String, Job, Outcome)],
    jobs: &Path,
    now: u64,
) -> String {
    let rows: Vec<(&str, &Entry)> = running
        .iter()
        .map(|e| ("running", e))
        .chain(waiting.iter().map(|e| ("waiting", e)))
        .chain(lock_waiting.iter().map(|e| ("waiting", e)))
        .collect();
    if rows.is_empty() && unseen.is_empty() {
        return "no heavy commands\n".into();
    }
    let width = rows
        .iter()
        .map(|(_, e)| e.label.chars().count())
        .chain(unseen.iter().map(|(_, j, _)| j.label.chars().count()))
        .max()
        .unwrap_or(0);
    let mut s: String = rows
        .iter()
        .map(|(state, e)| {
            // A run that is in the lock list waits for its key; any other with a key holds it.
            let in_lock_list = lock_waiting.contains(e);
            let lock = match &e.lock {
                Some(k) if in_lock_list => format!("  waits for lock {k}"),
                Some(k) => format!("  holds lock {k}"),
                None => String::new(),
            };
            let limit = match (*state, e.limit_ms) {
                ("running", Some(ms)) => format!("  limit {}", show_limit(ms)),
                ("running", None) => "  no limit".into(),
                _ => String::new(),
            };
            let reason = match *state {
                "waiting" if !in_lock_list => why(e.pid)
                    .map(|w| format!("  waits for {}", w.trim_start_matches("waiting for ")))
                    .unwrap_or_default(),
                _ => String::new(),
            };
            format!(
                "{state}  {:<width$}  pid {:<6}  {:>5}  {}{limit}{lock}{reason}\n",
                e.label,
                e.pid,
                age(now.saturating_sub(e.since)),
                e.cwd
            )
        })
        .collect();
    for (id, j, o) in unseen {
        s += &format!(
            "ended    {:<width$}  exit {:<5}  {:>5} ago  {}  unwatched: reeve heavy follow {id}  ({})\n",
            j.label,
            o.code,
            age(now.saturating_sub(o.finished)),
            j.cwd,
            jobs.join(id).display()
        );
    }
    s
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
            lock: None,
            limit_ms: None,
            job: None,
            worker: None,
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

    fn gate(slots: usize) -> Gate<'static> {
        Gate {
            slots,
            min_free_gb: 0.0,
            probe: &|| None,
        }
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
        let s = slot(q.try_claim(&mut t, &gate(1)).unwrap());
        assert!(q.waiting().is_empty());
        assert_eq!(labels(q.running()), ["build"]);
        q.drop(s);
        assert!(q.running().is_empty());
    }

    #[test]
    fn with_every_slot_taken_the_first_waiter_waits() {
        let (_d, q) = queue();
        let mut holder = q.enqueue(&entry(1, "holder", 1)).unwrap();
        let _held = slot(q.try_claim(&mut holder, &gate(1)).unwrap());
        let mut t = q.enqueue(&entry(2, "next", 2)).unwrap();
        assert_eq!(
            q.try_claim(&mut t, &gate(1)).unwrap(),
            Claim::Wait(Wait::Slots(1))
        );
        slot(q.try_claim(&mut t, &gate(2)).unwrap());
    }

    #[test]
    fn slots_and_tickets_whose_owner_is_gone_are_reclaimed() {
        let (_d, q) = queue();
        let mut t = q.enqueue(&entry(1, "gone", 1)).unwrap();
        crash(slot(q.try_claim(&mut t, &gate(1)).unwrap()));
        crash(q.enqueue(&entry(2, "gone-too", 2)).unwrap());
        let mut t = q.enqueue(&entry(3, "next", 3)).unwrap();
        let _s = slot(q.try_claim(&mut t, &gate(1)).unwrap());
        assert_eq!(labels(q.running()), ["next"]);
        assert_eq!(count(q.slots_dir()), 1);
        assert_eq!(count(q.queue_dir()), 0);
    }

    #[test]
    fn a_worker_is_busy_while_a_job_of_its_own_runs_or_waits() {
        let (_d, q) = queue();
        let mine = |pid, w: &str| Entry {
            worker: Some(w.into()),
            ..entry(pid, "t", pid.into())
        };
        let mut t = q.enqueue(&mine(1, "runs")).unwrap();
        let _s = slot(q.try_claim(&mut t, &gate(1)).unwrap());
        let _w = q.enqueue(&mine(2, "waits")).unwrap();
        let _l = q.wait_for_lock(&mine(3, "locked")).unwrap();
        let _n = q.enqueue(&entry(4, "nobody's", 4)).unwrap();
        crash(q.enqueue(&mine(5, "crashed")).unwrap());
        let mut busy: Vec<String> = q.busy_workers().into_iter().collect();
        busy.sort();
        assert_eq!(busy, ["locked", "runs", "waits"]);
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
        let mut s = slot(q.try_claim(&mut t, &gate(1)).unwrap());
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
        assert_eq!(
            q.try_claim(&mut second, &gate(2)).unwrap(),
            Claim::Wait(Wait::Ahead(1))
        );
        let _a = slot(q.try_claim(&mut first, &gate(2)).unwrap());
        let _b = slot(q.try_claim(&mut second, &gate(2)).unwrap());
    }

    #[test]
    fn a_waiter_whose_ticket_vanished_rejoins_the_queue() {
        let (_d, q) = queue();
        let mut t = q.enqueue(&entry(2, "lost", 1)).unwrap();
        fs::remove_file(&t.path).unwrap();
        let _first = q.enqueue(&entry(1, "first", 0)).unwrap();
        assert_eq!(
            q.try_claim(&mut t, &gate(1)).unwrap(),
            Claim::Wait(Wait::Ahead(1))
        );
        assert_eq!(labels(q.waiting()), ["first", "lost"]);
    }

    #[test]
    fn a_lock_is_exclusive_per_key_and_free_once_dropped_or_its_owner_is_gone() {
        let (_d, q) = queue();
        let me = entry(1, "a", 1);
        let held = q.try_lock("db", &me).unwrap().ok().unwrap();
        let other = q.try_lock("db", &entry(2, "b", 2)).unwrap();
        assert_eq!(other.err().unwrap().map(|h| h.pid), Some(1));
        assert!(q.try_lock("other", &me).unwrap().is_ok());
        drop(held);
        assert!(q.try_lock("db", &entry(2, "b", 2)).unwrap().is_ok());
    }

    #[test]
    fn keys_become_safe_distinct_file_names() {
        assert_eq!(encode_key("app-e2e_db.1"), "app-e2e_db.1");
        assert_eq!(encode_key("a/b"), "a%2Fb");
        assert_ne!(encode_key("a/b"), encode_key("a%2Fb"));
    }

    #[test]
    fn limits_parse_with_a_unit_or_none() {
        let ms = |s| parse_limit(s).unwrap().map(|d| d.as_millis());
        assert_eq!(ms("90s"), Some(90_000));
        assert_eq!(ms("45m"), Some(2_700_000));
        assert_eq!(ms("2h"), Some(7_200_000));
        assert_eq!(ms("500ms"), Some(500));
        assert_eq!(ms("none"), None);
        assert!(parse_limit("45").is_err());
        assert!(parse_limit("0s").is_err());
        assert!(parse_limit("fast").is_err());
        assert_eq!(ms(DEFAULT_TIMEOUT), Some(1_800_000));
        assert_eq!(show_limit(2_700_000), "45m");
    }

    #[test]
    fn below_the_memory_floor_only_the_first_run_starts() {
        let (_d, q) = queue();
        let tight = || {
            Some(Mem {
                total: 16 << 30,
                free_pct: 10,
            })
        };
        let g = Gate {
            slots: 4,
            min_free_gb: 2.0,
            probe: &tight,
        };
        let mut a = q.enqueue(&entry(1, "a", 1)).unwrap();
        let _a = slot(q.try_claim(&mut a, &g).unwrap());
        let mut b = q.enqueue(&entry(2, "b", 2)).unwrap();
        match q.try_claim(&mut b, &g).unwrap() {
            Claim::Wait(w @ Wait::Memory { free, floor }) => {
                assert!((free - 1.6).abs() < 1e-9 && floor == 2.0);
                assert!(w.to_string().contains("1.6 GB free"), "{w}");
            }
            other => panic!("expected a wait for memory, got {other:?}"),
        }
        // A floor of 0 turns the check off, and so does a machine reeve can't read.
        let off = Gate {
            min_free_gb: 0.0,
            ..g
        };
        let _b = slot(q.try_claim(&mut b, &off).unwrap());
        let mut c = q.enqueue(&entry(3, "c", 3)).unwrap();
        let unknown = Gate {
            slots: 4,
            min_free_gb: 2.0,
            probe: &|| None,
        };
        slot(q.try_claim(&mut c, &unknown).unwrap());
    }

    #[test]
    fn min_free_parses_gb_with_or_without_the_unit() {
        assert_eq!(parse_gb("2"), Ok(2.0));
        assert_eq!(parse_gb("1.5G"), Ok(1.5));
        assert_eq!(parse_gb("0"), Ok(0.0));
        assert!(parse_gb("-1").is_err() && parse_gb("lots").is_err());
    }

    #[test]
    fn ls_says_why_each_waiter_waits_and_shows_unwatched_ends() {
        let job = Job {
            cmd: vec!["cargo".into()],
            label: "build".into(),
            ev_label: "cargo".into(),
            lock: None,
            slots: 1,
            min_free_gb: 2.0,
            limit_ms: None,
            cwd: "/w".into(),
        };
        let ended = Outcome {
            code: 101,
            finished: 0,
            followed: false,
        };
        let why = |pid| (pid == 2).then(|| Wait::Slots(1).to_string());
        let out = ls(
            &[entry(1, "test", 0)],
            &[entry(2, "next", 0)],
            &[],
            &why,
            &[("17-9".into(), job, ended)],
            Path::new("/s/jobs"),
            120_000,
        );
        assert!(
            out.contains("next") && out.contains("waits for a slot: 0 ahead, 1 running"),
            "{out}"
        );
        let last = out.lines().last().unwrap();
        assert!(
            last.starts_with("ended") && last.contains("exit 101") && last.contains("2m ago"),
            "{out}"
        );
        assert!(last.contains("reeve heavy follow 17-9"), "{out}");
    }

    #[test]
    fn default_slots_is_a_quarter_of_the_cores_and_at_least_one() {
        assert_eq!(default_slots(16), 4);
        assert_eq!(default_slots(10), 2);
        assert_eq!(default_slots(2), 1);
        assert_eq!(default_slots(0), 1);
    }
}
