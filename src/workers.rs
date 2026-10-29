//! Named workers: agents started by `reeve run --name` in a Herdr pane, as a Claude Code
//! background session or as a detached `codex exec`, recorded so that any later session can find
//! them and their reports.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Pane,
    Bg,
}

impl Backend {
    /// A pane when asked for, or by default under Herdr; otherwise a background session.
    pub fn choose(pane: bool, bg: bool, in_herdr: bool) -> Backend {
        if pane || (in_herdr && !bg) {
            Backend::Pane
        } else {
            Backend::Bg
        }
    }
}

/// Which agent CLI a worker runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Claude,
    Codex,
}

impl Kind {
    /// The CLI's executable, which is also Herdr's name for the agent.
    pub fn bin(self) -> &'static str {
        match self {
            Kind::Claude => "claude",
            Kind::Codex => "codex",
        }
    }

    fn is_claude(&self) -> bool {
        *self == Kind::Claude
    }

    /// The agent of the session reeve runs in: Codex sets `CODEX_THREAD_ID` in its shell
    /// commands, Claude Code sets `CLAUDECODE`. Claude when it can't tell.
    pub fn of_session(codex_thread: Option<&str>, claudecode: Option<&str>) -> Kind {
        match (codex_thread, claudecode) {
            (Some(t), None) if !t.is_empty() => Kind::Codex,
            _ => Kind::Claude,
        }
    }

    /// Variables by which the other agent would mistake this one for its own session.
    fn foreign_env(self) -> &'static [&'static str] {
        match self {
            Kind::Claude => &["CODEX_THREAD_ID", "CODEX_SESSION_ID"],
            Kind::Codex => &["CLAUDECODE"],
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self.bin())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worker {
    pub name: String,
    pub backend: Backend,
    #[serde(default, skip_serializing_if = "Kind::is_claude")]
    pub agent: Kind,
    pub cwd: PathBuf,
    /// Unix seconds.
    pub started: u64,
    pub report: PathBuf,
    /// The Herdr pane ID, the background session's short id, or the pid of a background
    /// `codex exec`; empty for a background worker that is still starting.
    pub handle: String,
    /// Set by `reeve run` until `reeve start`, in the background, has brought the agent up.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub starting: bool,
    /// Why the agent didn't come up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// What a worker's pane or session is doing, as far as Herdr or `claude agents` can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Working,
    /// Finished its turn without being asked anything.
    Idle,
    /// Waiting for the user: a permission prompt, a question or another dialog.
    Blocked,
    Absent,
    /// The probe itself failed.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Starting,
    Running,
    Idle,
    Blocked,
    Done,
    Gone,
    Unknown,
}

impl State {
    pub fn of(agent: Agent, report_exists: bool) -> State {
        match (agent, report_exists) {
            (_, true) => State::Done,
            (Agent::Working, false) => State::Running,
            (Agent::Idle, false) => State::Idle,
            (Agent::Blocked, false) => State::Blocked,
            (Agent::Absent, false) => State::Gone,
            (Agent::Unknown, false) => State::Unknown,
        }
    }
}

/// Long enough for a pane start's own waits: 5s for the shell, 10s for herdr, 10s for claude.
pub const START_SECS: u64 = 30;

/// A worker still starting is `starting`, and `gone` once its start failed or ran out of time,
/// unless it wrote its report anyway; only a started one has an agent worth asking about.
pub fn state_of(w: &Worker, now: u64, agent: impl FnOnce() -> Agent) -> State {
    match (&w.error, w.starting) {
        (Some(_), _) if w.report.is_file() => State::Done,
        (Some(_), _) => State::Gone,
        (None, true) if now.saturating_sub(w.started) < START_SECS => State::Starting,
        (None, true) => State::Gone,
        (None, false) => {
            let agent = agent();
            State::of(agent, w.report.is_file())
        }
    }
}

/// Why a worker without a report is gone, after `worker NAME`.
pub fn gone_why(w: &Worker) -> String {
    match &w.error {
        Some(e) => format!("failed to start: {e}"),
        None if w.starting => format!("did not start within {START_SECS}s"),
        None => format!("is gone without writing its report: {}", left_where(w)),
    }
}

/// Where a background `codex exec` worker's JSON events go: `logs/NAME.jsonl` beside `reports/`.
pub fn exec_log(w: &Worker) -> PathBuf {
    let state = w
        .report
        .parent()
        .and_then(Path::parent)
        .unwrap_or(Path::new("."));
    state.join("logs").join(format!("{}.jsonl", w.name))
}

/// The thread id from `codex exec --json`'s `thread.started` event.
pub fn exec_thread(log: &str) -> Option<String> {
    log.lines().find_map(|l| {
        let v: Value = serde_json::from_str(l).ok()?;
        (v["type"] == "thread.started").then(|| v["thread_id"].as_str().map(String::from))?
    })
}

/// How to send a background `codex exec` worker a message: resume its thread.
fn resume_hint(w: &Worker) -> String {
    let log = exec_log(w);
    let thread = fs::read_to_string(&log)
        .ok()
        .and_then(|l| exec_thread(&l))
        .unwrap_or_else(|| format!("THREAD (the thread_id in {})", crate::setup::tilde(&log)));
    format!("`codex exec resume {thread} \"MESSAGE\"`")
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match self {
            State::Starting => "starting",
            State::Running => "running",
            State::Idle => "idle",
            State::Blocked => "blocked",
            State::Done => "done",
            State::Gone => "gone",
            State::Unknown => "unknown",
        })
    }
}

/// `workers.json` and `reports/` under one state dir.
pub struct Registry {
    dir: PathBuf,
}

impl Registry {
    pub fn at(dir: PathBuf) -> Registry {
        Registry { dir }
    }

    /// `$XDG_STATE_HOME/reeve`, by default `~/.local/state/reeve`.
    pub fn default_dir() -> io::Result<PathBuf> {
        std::env::var_os("XDG_STATE_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| Some(std::env::home_dir()?.join(".local/state")))
            .map(|d| d.join("reeve"))
            .ok_or_else(|| io::Error::other("no home dir"))
    }

    pub fn report_path(&self, name: &str) -> PathBuf {
        self.dir.join("reports").join(format!("{name}.md"))
    }

    pub fn handoff_path(&self, name: &str) -> PathBuf {
        self.dir.join("handoffs").join(format!("{name}.md"))
    }

    /// Names the predecessor of a successor whose handoff reeve has not yet tidied up after.
    pub fn predecessor_path(&self, next: &str) -> PathBuf {
        self.dir.join("handoffs").join(format!("{next}.from"))
    }

    /// The (successor, predecessor) pairs still waiting for the predecessor to be closed.
    pub fn predecessors(&self) -> Vec<(String, String)> {
        let Ok(dir) = fs::read_dir(self.dir.join("handoffs")) else {
            return Vec::new();
        };
        let mut pairs: Vec<_> = dir
            .filter_map(|e| {
                let path = e.ok()?.path();
                let next = path
                    .file_name()?
                    .to_str()?
                    .strip_suffix(".from")?
                    .to_string();
                Some((next, fs::read_to_string(&path).ok()?.trim().to_string()))
            })
            .collect();
        pairs.sort();
        pairs
    }

    fn file(&self) -> PathBuf {
        self.dir.join("workers.json")
    }

    pub fn all(&self) -> io::Result<Vec<Worker>> {
        match fs::read_to_string(self.file()) {
            Ok(s) => serde_json::from_str(&s).map_err(io::Error::other),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(vec![]),
            Err(e) => Err(e),
        }
    }

    pub fn get(&self, name: &str) -> io::Result<Worker> {
        self.all()?
            .into_iter()
            .find(|w| w.name == name)
            .ok_or_else(|| io::Error::other(format!("no worker {name}; see `reeve ls`")))
    }

    /// Adds `w`, replacing any worker of the same name. Concurrent runs take turns on a lock
    /// file, and the registry is replaced by a rename so readers never see half of it.
    pub fn put(&self, w: Worker) -> io::Result<()> {
        self.update(|all| match all.iter_mut().find(|x| x.name == w.name) {
            Some(x) => *x = w,
            None => all.push(w),
        })
    }

    /// Drops the worker of that name; returns whether there was one.
    pub fn remove(&self, name: &str) -> io::Result<bool> {
        let mut found = false;
        self.update(|all| {
            let before = all.len();
            all.retain(|w| w.name != name);
            found = all.len() != before;
        })?;
        Ok(found)
    }

    /// Changes the worker of that name if it is still the one started at `started`.
    pub fn set(&self, name: &str, started: u64, f: impl FnOnce(&mut Worker)) -> io::Result<()> {
        self.update(|all| {
            if let Some(w) = all
                .iter_mut()
                .find(|w| w.name == name && w.started == started)
            {
                f(w);
            }
        })
    }

    fn update(&self, f: impl FnOnce(&mut Vec<Worker>)) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let lock = File::create(self.dir.join("workers.lock"))?;
        lock.lock()?;
        let mut all = self.all()?;
        f(&mut all);
        let tmp = self
            .dir
            .join(format!("workers.json.{}", std::process::id()));
        fs::write(
            &tmp,
            serde_json::to_string_pretty(&all).map_err(io::Error::other)?,
        )?;
        fs::rename(&tmp, self.file())
    }
}

/// Herdr's rule for agent names, which also keeps the report path inside `reports/`.
pub fn check_name(name: &str) -> Result<(), String> {
    let mut cs = name.chars();
    let ok = cs.next().is_some_and(|c| c.is_ascii_lowercase())
        && cs.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && name.len() <= 32;
    if ok {
        Ok(())
    } else {
        Err(format!(
            "worker name {name:?} must match [a-z][a-z0-9_-]{{0,31}}"
        ))
    }
}

pub fn split_prompt(args: &[String]) -> Result<(&[String], &str), String> {
    match args.split_last() {
        Some((p, rest)) if !p.starts_with('-') => Ok((rest, p)),
        _ => Err("a named worker needs its prompt as the last claude argument".into()),
    }
}

/// The caller's task, then where its report goes and what it says. One line, because a pane
/// types it; the heavy rule comes from the skill, not from here.
pub fn with_report(prompt: &str, report: &Path) -> String {
    format!(
        "{prompt} When the task is done, or you can't take it further without the user, write \
         your report as Markdown to {}. reeve treats that file as the end of your work, so \
         write it once, as your last step. Start it with one line, `done`, `partial` or \
         `failed`, then say what you did, how you checked it, and what is left.",
        report.display()
    )
}

/// Herdr refuses launch arguments it can't encode for the pane's shell, such as newlines, so
/// such a prompt is written to `file` and the pane is told to read it.
pub fn pane_prompt(prompt: &str, file: &Path) -> Option<String> {
    prompt.chars().any(char::is_control).then(|| pointer(file))
}

fn pointer(file: &Path) -> String {
    format!(
        "Read your task from {} and do it. Follow every instruction in it.",
        file.display()
    )
}

/// `herdr agent start` types the claude command into the new pane's shell. Typed before the
/// shell's line editor is up, it lands in the tty's line buffer, which macOS caps at 1024
/// bytes (MAX_CANON): the rest, Enter included, is dropped and claude never starts.
pub const TYPED_MAX: usize = 1000;

/// Length of the command herdr types, each argument single-quoted.
pub fn typed_len(kind: Kind, argv: &[String]) -> usize {
    argv.iter()
        .map(|a| 3 + a.len() + 3 * a.matches('\'').count())
        .sum::<usize>()
        + kind.bin().len()
}

/// The agent's argv for a pane. A prompt that can't be typed whole goes through `file`, which
/// the caller writes; flags too long to type are an error.
pub fn pane_argv(
    kind: Kind,
    flags: &[String],
    prompt: &str,
    file: &Path,
) -> Result<Vec<String>, String> {
    let with = |p: String| -> Vec<String> { flags.iter().cloned().chain([p]).collect() };
    let argv = with(prompt.into());
    if pane_prompt(prompt, file).is_none() && typed_len(kind, &argv) <= TYPED_MAX {
        return Ok(argv);
    }
    let argv = with(pointer(file));
    match typed_len(kind, &argv) {
        n if n <= TYPED_MAX => Ok(argv),
        n => Err(format!(
            "{kind}'s flags are too long to type into a pane ({n} bytes, at most {TYPED_MAX})"
        )),
    }
}

/// Claude args for a worker: it may write its report, and is shown under its name. reeve's
/// flags come last, after the user's, so a flag taking several values can't swallow the
/// prompt that follows.
pub fn claude_args(name: &str, report: &Path, flags: &[String]) -> Vec<String> {
    let reports = report.parent().unwrap_or(report).display().to_string();
    let own = [
        "--add-dir".into(),
        reports,
        "--allowedTools".into(),
        format!("Edit(/{})", report.display()),
        "-n".into(),
        name.into(),
    ];
    flags.iter().cloned().chain(own).collect()
}

/// Codex args for a worker: its sandbox may write reeve's state dir, for the report and
/// `reeve heavy`'s queue. That takes a writable sandbox, which `codex exec` and an untrusted
/// folder don't give by default (codex then refuses `--add-dir` and exits), so it gets
/// workspace-write unless its arguments choose a sandbox. Codex has no flag to name a session.
pub fn codex_args(report: &Path, flags: &[String]) -> Vec<String> {
    let state = report.parent().and_then(Path::parent).unwrap_or(report);
    let chosen = flags.iter().any(|a| {
        CODEX_SANDBOX_FLAGS.contains(&a.as_str())
            || a.starts_with("--sandbox=")
            || a.starts_with("-s=")
    });
    let sandbox = (!chosen).then_some(["-s", "workspace-write"]);
    flags
        .iter()
        .cloned()
        .chain(sandbox.into_iter().flatten().map(String::from))
        .chain(["--add-dir".into(), state.display().to_string()])
        .collect()
}

pub fn agent_args(kind: Kind, name: &str, report: &Path, flags: &[String]) -> Vec<String> {
    match kind {
        Kind::Claude => claude_args(name, report, flags),
        Kind::Codex => codex_args(report, flags),
    }
}

/// Flags by which codex's own arguments already choose a sandbox.
const CODEX_SANDBOX_FLAGS: [&str; 3] = [
    "-s",
    "--sandbox",
    "--dangerously-bypass-approvals-and-sandbox",
];

/// The whole `codex exec` command line for a background worker, with its events as JSON.
pub fn exec_argv(codex: &[String], prompt: &str) -> Vec<String> {
    ["codex", "exec", "--json", "--skip-git-repo-check"]
        .into_iter()
        .map(String::from)
        .chain(codex.iter().cloned())
        .chain([prompt.into()])
        .collect()
}

pub fn tab_create_args(name: &str, cwd: &str, workspace: Option<&str>) -> Vec<String> {
    ["tab", "create", "--label", name, "--cwd", cwd, "--no-focus"]
        .into_iter()
        .chain(workspace.into_iter().flat_map(|w| ["--workspace", w]))
        .map(String::from)
        .collect()
}

/// The tab ID and root pane ID from `herdr tab create`.
pub fn created_tab(out: &Value) -> Option<(String, String)> {
    let r = &out["result"];
    Some((
        r["tab"]["tab_id"].as_str()?.to_string(),
        r["root_pane"]["pane_id"].as_str()?.to_string(),
    ))
}

pub fn pane_start_args(name: &str, pane: &str, kind: Kind, claude: &[String]) -> Vec<String> {
    [
        "agent",
        "start",
        name,
        "--kind",
        kind.bin(),
        "--pane",
        pane,
        "--timeout",
        "10000",
        "--",
    ]
    .into_iter()
    .map(String::from)
    .chain(claude.iter().cloned())
    .collect()
}

/// `herdr agent start` gave up, but the agent may well be running: a launch prompt keeps it
/// busy, so herdr times out waiting for it to become idle, and Codex's own session hook can
/// claim the pane for its session before herdr is done, so herdr's name no longer owns it.
pub fn busy_at_start(err: &str) -> bool {
    herdr_error_code(err).is_some_and(|c| c == "timeout" || c == "agent_name_not_found")
}

/// The agent started but stopped at a question for the user, such as Codex asking whether to
/// trust the folder: it is up, and shows as blocked until the user answers it in its tab.
pub fn blocked_at_start(err: &str) -> bool {
    herdr_error_code(err).is_some_and(|c| c == "agent_not_ready")
}

/// A new pane's shell isn't up yet; herdr refuses before typing anything, so retrying is safe.
pub fn pane_not_ready(err: &str) -> bool {
    herdr_error_code(err).is_some_and(|c| c == "agent_pane_busy")
}

fn herdr_error_code(err: &str) -> Option<String> {
    let v = serde_json::from_str::<Value>(err.strip_prefix("herdr: ")?).ok()?;
    v["error"]["code"].as_str().map(String::from)
}

pub fn bg_argv(claude: &[String], prompt: &str) -> Vec<String> {
    let args: Vec<String> = claude.iter().cloned().chain([prompt.into()]).collect();
    crate::run::claude_argv(true, false, &args)
}

/// From `claude --bg`'s "backgrounded · <id> · <name>" line.
pub fn bg_id(out: &str) -> Option<String> {
    out.lines()
        .find_map(|l| l.trim().strip_prefix("backgrounded · "))
        .and_then(|rest| rest.split(" · ").next())
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
}

/// From `herdr pane get`: its JSON, or the error naming a closed pane. A pane whose agent
/// isn't `kind` (any more) has none of ours.
pub fn pane_agent(probe: Result<&str, &str>, kind: Kind) -> Agent {
    let out = match probe {
        Err(e) if e.contains(r#""code":"pane_not_found""#) => return Agent::Absent,
        Err(_) => return Agent::Unknown,
        Ok(o) => o,
    };
    let Ok(v) = serde_json::from_str::<Value>(out) else {
        return Agent::Unknown;
    };
    let pane = &v["result"]["pane"];
    if pane["agent"] != kind.bin() {
        return Agent::Absent;
    }
    match pane["agent_status"].as_str() {
        Some("blocked") => Agent::Blocked,
        Some("idle" | "done") => Agent::Idle,
        _ => Agent::Working,
    }
}

/// From `claude agents --json`, which keeps listing a background session after its turn.
pub fn bg_agent(probe: Result<&str, &str>, id: &str) -> Agent {
    let Ok(all) = probe.map(serde_json::from_str::<Vec<Value>>) else {
        return Agent::Unknown;
    };
    let Ok(all) = all else {
        return Agent::Unknown;
    };
    match all.iter().find(|a| a["id"] == id) {
        None => Agent::Absent,
        Some(a) if a["status"] == "busy" => Agent::Working,
        Some(a) if a["state"] == "stopped" || a["state"] == "failed" => Agent::Absent,
        // Also without a process: one idle for an hour is stopped and resumes on attach.
        Some(a) if a["state"] == "blocked" => Agent::Blocked,
        Some(_) => Agent::Idle,
    }
}

/// From `ps -o command= -p PID`: a background `codex exec` works until its process is gone. It
/// never stops to ask, so it is never blocked, and it exits when its turn ends. A reused pid
/// runs something else.
pub fn exec_agent(probe: Result<&str, &str>) -> Agent {
    match probe {
        Ok(cmd) if cmd.contains("codex") => Agent::Working,
        _ => Agent::Absent,
    }
}

pub fn age(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86400),
    }
}

fn stdout<S: AsRef<OsStr>>(cmd: &str, args: impl IntoIterator<Item = S>) -> io::Result<String> {
    let o = Command::new(cmd).args(args).stdin(Stdio::null()).output()?;
    if !o.status.success() {
        return Err(io::Error::other(format!(
            "{cmd}: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

fn probe<S: AsRef<OsStr>>(cmd: &str, args: impl IntoIterator<Item = S>) -> Result<String, String> {
    stdout(cmd, args).map_err(|e| e.to_string())
}

fn herdr<S: AsRef<OsStr>>(args: impl IntoIterator<Item = S>) -> io::Result<Value> {
    serde_json::from_str(&stdout("herdr", args)?).map_err(io::Error::other)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Opens a tab labelled NAME in the caller's workspace without focusing it. Returns its tab
/// and pane IDs.
fn open_tab(name: &str, cwd: &Path) -> io::Result<(String, String)> {
    let workspace = std::env::var("HERDR_WORKSPACE_ID").ok();
    let cwd = cwd.display().to_string();
    let created = herdr(tab_create_args(name, &cwd, workspace.as_deref()))?;
    created_tab(&created).ok_or_else(|| io::Error::other("herdr tab create returned no pane id"))
}

/// Starts the agent in the tab's pane with the prompt as its argument; the tab is closed again
/// if it didn't start.
fn start_pane(
    name: &str,
    tab: &str,
    pane: &str,
    kind: Kind,
    args: &[String],
) -> io::Result<String> {
    let start = || herdr(pane_start_args(name, pane, kind, args));
    let mut started = start();
    for _ in 0..20 {
        match &started {
            Err(e) if pane_not_ready(&e.to_string()) => {
                std::thread::sleep(Duration::from_millis(250));
                started = start();
            }
            _ => break,
        }
    }
    let started = match started {
        Err(e) if busy_at_start(&e.to_string()) => wait_for_agent(pane, kind)
            .and_then(|_| herdr(["agent", "rename", pane, name]))
            .map_err(|_| e),
        Err(e) if blocked_at_start(&e.to_string()) => Ok(Value::Null),
        r => r,
    };
    if let Err(e) = started {
        let _ = herdr(["tab", "close", tab]);
        return Err(e);
    }
    Ok(pane.into())
}

fn wait_for_agent(pane: &str, kind: Kind) -> io::Result<()> {
    for _ in 0..20 {
        let a = pane_agent(
            probe("herdr", ["pane", "get", pane])
                .as_deref()
                .map_err(|e| e.as_str()),
            kind,
        );
        if matches!(a, Agent::Working | Agent::Idle | Agent::Blocked) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(io::Error::other(format!("no {kind} in pane {pane}")))
}

/// Drops the variables by which `kind` would take itself for its starter's session.
fn without_foreign_env(mut cmd: Command, kind: Kind) -> Command {
    for k in kind.foreign_env() {
        cmd.env_remove(k);
    }
    cmd
}

/// Returns the short id that `claude --bg` prints and `claude agents` lists.
fn start_bg(argv: &[String]) -> io::Result<String> {
    let mut cmd = without_foreign_env(Command::new(&argv[0]), Kind::Claude);
    let o = cmd.args(&argv[1..]).stdin(Stdio::null()).output()?;
    if !o.status.success() {
        return Err(io::Error::other(format!(
            "claude --bg failed ({}): {}",
            o.status,
            String::from_utf8_lossy(&o.stderr).trim()
        )));
    }
    bg_id(&String::from_utf8_lossy(&[o.stdout, o.stderr].concat()))
        .ok_or_else(|| io::Error::other("claude --bg printed no session id"))
}

/// Starts `codex exec` in its own process group with its events in `log`, and returns its pid.
/// Nothing waits for it: once `reeve start` exits, init reaps it.
fn start_exec(argv: &[String], log: &Path) -> io::Result<String> {
    use std::os::unix::process::CommandExt;
    fs::create_dir_all(log.parent().expect("logs dir"))?;
    let out = File::create(log)?;
    let mut cmd = without_foreign_env(Command::new(&argv[0]), Kind::Codex);
    let child = cmd
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(out.try_clone()?)
        .stderr(out)
        .process_group(0)
        .spawn()
        .map_err(|e| io::Error::other(format!("codex exec failed to start: {e}")))?;
    Ok(child.id().to_string())
}

/// A pane's recent terminal text, as Herdr reads it.
pub fn read_tab(pane: &str) -> Result<String, String> {
    probe(
        "herdr",
        [
            "pane", "read", pane, "--source", "recent", "--lines", "2000",
        ],
    )
}

pub fn agent(w: &Worker) -> Agent {
    match (w.backend, w.agent) {
        (Backend::Pane, kind) => {
            let p = probe("herdr", ["pane", "get", &w.handle]);
            pane_agent(p.as_deref().map_err(|e| e.as_str()), kind)
        }
        (Backend::Bg, Kind::Claude) => {
            let p = probe("claude", ["agents", "--json"]);
            bg_agent(p.as_deref().map_err(|e| e.as_str()), &w.handle)
        }
        (Backend::Bg, Kind::Codex) => {
            let p = probe("ps", ["-o", "command=", "-p", &w.handle]);
            exec_agent(p.as_deref().map_err(|e| e.as_str()))
        }
    }
}

/// How to stop a worker's agent by hand.
fn stop_hint(w: &Worker) -> String {
    match (w.backend, w.agent) {
        (Backend::Pane, _) => format!("close pane {}", w.handle),
        (Backend::Bg, Kind::Claude) => format!("`claude rm {}`", w.handle),
        (Backend::Bg, Kind::Codex) => format!("`kill {}`", w.handle),
    }
}

/// The agent is checked before the report, so one that writes its report and exits in
/// between still counts as done.
pub fn state(w: &Worker) -> State {
    state_of(w, now(), || agent(w))
}

pub fn run(
    r: &Registry,
    name: &str,
    kind: Kind,
    backend: Backend,
    args: &[String],
) -> io::Result<()> {
    check_name(name).map_err(io::Error::other)?;
    let (flags, prompt) = split_prompt(args).map_err(io::Error::other)?;
    if let Ok(old) = r.get(name) {
        let a = agent(&old);
        if state_of(&old, now(), || a) == State::Starting {
            return Err(io::Error::other(format!(
                "worker {name} is still starting; wait for it or use another name"
            )));
        }
        if a != Agent::Absent {
            return Err(io::Error::other(format!(
                "worker {name} still has an agent; {} or use another name",
                stop_hint(&old)
            )));
        }
    }
    let report = r.report_path(name);
    let reports = report.parent().expect("reports dir");
    fs::create_dir_all(reports)?;
    let old = report.with_extension("md.old");
    match fs::rename(&report, &old) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let own = agent_args(kind, name, &report, flags);
    let prompt = with_report(prompt, &report);
    let cwd = std::env::current_dir()?;
    // The tab opens now, so the user sees the worker starting; the agent starts in the background.
    let opened = match backend {
        Backend::Pane => {
            let file = reports.join(format!("{name}.prompt.md"));
            pane_argv(kind, &own, &prompt, &file)
                .map_err(io::Error::other)
                .and_then(|argv| {
                    if argv.last() != Some(&prompt) {
                        fs::write(&file, &prompt)?;
                    }
                    let (tab, pane) = open_tab(name, &cwd)?;
                    Ok((pane, Some(tab), argv))
                })
        }
        Backend::Bg if kind == Kind::Codex => Ok((String::new(), None, exec_argv(&own, &prompt))),
        Backend::Bg => Ok((String::new(), None, bg_argv(&own, &prompt))),
    };
    let (handle, tab, argv) = match opened {
        Ok(o) => o,
        Err(e) => {
            let _ = fs::rename(&old, &report);
            return Err(e);
        }
    };
    let _ = fs::remove_file(&old);
    let started = now();
    r.put(Worker {
        name: name.into(),
        backend,
        agent: kind,
        cwd,
        started,
        report,
        handle,
        starting: true,
        error: None,
    })?;
    if let Err(e) = spawn_start(name, tab.as_deref(), &argv) {
        if let Some(tab) = &tab {
            let _ = herdr(["tab", "close", tab]);
        }
        let why = format!("could not run `reeve start`: {e}");
        r.set(name, started, |w| {
            w.starting = false;
            w.error = Some(why.clone());
        })?;
        return Err(io::Error::other(why));
    }
    Ok(())
}

/// Runs `reeve start` in its own process group, so it outlives `reeve run` and the shell that
/// called it.
fn spawn_start(name: &str, tab: Option<&str>, argv: &[String]) -> io::Result<()> {
    use std::os::unix::process::CommandExt;
    let tab = tab.into_iter().flat_map(|t| ["--tab", t]);
    Command::new(std::env::current_exe()?)
        .args(["start", name])
        .args(tab)
        .arg("--")
        .args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map(|_| ())
}

/// The slow half of `reeve run`: brings the registered worker's agent up, with `argv` as the
/// agent's arguments in a pane or its whole background command line, and records its handle
/// or why it failed.
pub fn start(r: &Registry, name: &str, tab: Option<&str>, argv: &[String]) -> io::Result<()> {
    let w = r.get(name)?;
    let up = match (w.backend, w.agent, tab) {
        (Backend::Pane, kind, Some(tab)) => start_pane(name, tab, &w.handle, kind, argv),
        (Backend::Pane, _, None) => Err(io::Error::other("a pane worker needs --tab")),
        (Backend::Bg, Kind::Claude, _) => start_bg(argv),
        (Backend::Bg, Kind::Codex, _) => start_exec(argv, &exec_log(&w)),
    };
    r.set(name, w.started, |w| {
        w.starting = false;
        match &up {
            Ok(handle) => w.handle = handle.clone(),
            Err(e) => w.error = Some(e.to_string()),
        }
    })?;
    up.map(|_| ())
}

/// What `reeve run` prints once a worker is registered: where its report goes and how the
/// starting agent hears about it.
pub fn started(w: &Worker) -> String {
    let name = &w.name;
    format!(
        "reeve: worker {name} is starting; its report goes to {}\n\
         reeve: follow it with `reeve watch {name}` (Claude Code: the Monitor tool, timeout_ms \
         1800000, re-armed when it expires; elsewhere a background shell). A line per change \
         says what to do: done, read the report, then `reeve close {name}`; idle, read {}: \
         nothing if it waits on its own job, else nudge it, let it carry on, or ask the user; \
         blocked, answer it {} if you can, else ask the user.",
        crate::setup::tilde(&w.report),
        look_where(w),
        answer_where(w)
    )
}

/// Workers older than this show in the brief listing only while they still have an agent.
const BRIEF_DAYS: u64 = 7;
const BRIEF_NAMES: usize = 8;
/// Far below the 10,000 characters Claude Code takes from a hook.
pub const BRIEF_MAX: usize = 1000;

/// The brief shows only when other workers share the machine, which is when the queue matters.
const HEAVY_REMINDER: &str = " Heavy commands go through `reeve heavy`, in the background.";

/// One line for a session-start hook, e.g. `reeve: 2 workers: a running, b done (report
/// ready).`, or nothing when there is nothing worth telling.
pub fn brief(workers: &[(&str, State, u64)]) -> String {
    let shown: Vec<_> = workers
        .iter()
        .filter(|(_, s, age)| {
            matches!(
                s,
                State::Starting | State::Running | State::Idle | State::Blocked | State::Unknown
            ) || *age < BRIEF_DAYS * 86400
        })
        .collect();
    if shown.is_empty() {
        return String::new();
    }
    let mut parts: Vec<String> = shown
        .iter()
        .take(BRIEF_NAMES)
        .map(|(name, s, _)| {
            let what = match s {
                State::Starting => "starting",
                State::Running => "running",
                State::Idle => "idle without a report",
                State::Blocked => "blocked (waiting for the user)",
                State::Done => "done (report ready)",
                State::Gone => "gone without a report",
                State::Unknown => "state unknown",
            };
            format!("{name} {what}")
        })
        .collect();
    if shown.len() > BRIEF_NAMES {
        parts.push(format!("{} more", shown.len() - BRIEF_NAMES));
    }
    let n = shown.len();
    let mut out = format!(
        "reeve: {n} worker{}: {}. `reeve ls` shows details",
        if n == 1 { "" } else { "s" },
        parts.join(", ")
    );
    if shown.iter().any(|(_, s, _)| *s == State::Done) {
        out += "; `reeve report NAME` prints a report, then `reeve close NAME` tidies up";
    }
    if shown.iter().any(|(_, s, _)| *s == State::Idle) {
        out += "; look at an idle one and nudge it";
    }
    out.push('.');
    let max = BRIEF_MAX - HEAVY_REMINDER.len();
    if out.len() > max {
        let cut = out.floor_char_boundary(max - 1);
        out.truncate(cut);
        out.push('…');
    }
    out + HEAVY_REMINDER
}

/// States for many workers, asking `claude agents` only once.
fn states(all: &[Worker]) -> Vec<State> {
    let mut bg: Option<Result<String, String>> = None;
    let now = now();
    all.iter()
        .map(|w| {
            state_of(w, now, || match (w.backend, w.agent) {
                (Backend::Pane, _) | (Backend::Bg, Kind::Codex) => agent(w),
                (Backend::Bg, Kind::Claude) => {
                    let p = bg.get_or_insert_with(|| probe("claude", ["agents", "--json"]));
                    bg_agent(p.as_deref().map_err(|e| e.as_str()), &w.handle)
                }
            })
        })
        .collect()
}

pub fn ls_brief(r: &Registry) -> io::Result<String> {
    let all = r.all()?;
    let states = states(&all);
    let now = now();
    let rows: Vec<_> = all
        .iter()
        .zip(states)
        .map(|(w, s)| (w.name.as_str(), s, now.saturating_sub(w.started)))
        .collect();
    Ok(brief(&rows))
}

/// Named workers that are gone without having written a report.
pub fn gone(r: &Registry) -> io::Result<Vec<String>> {
    let all = r.all()?;
    Ok(all
        .iter()
        .zip(states(&all))
        .filter(|(_, s)| *s == State::Gone)
        .map(|(w, _)| w.name.clone())
        .collect())
}

pub fn ls(r: &Registry) -> io::Result<String> {
    let all = r.all()?;
    let width = all.iter().map(|w| w.name.len()).max().unwrap_or(0).max(4);
    let mut out = format!("{:width$}  STATE     AGENT   BACKEND  AGE  CWD", "NAME");
    for (w, state) in all.iter().zip(states(&all)) {
        let backend = match w.backend {
            Backend::Pane => "pane",
            Backend::Bg => "bg",
        };
        out += &format!(
            "\n{:width$}  {:8}  {:6}  {backend:7}  {:>3}  {}",
            w.name,
            state.to_string(),
            w.agent.bin(),
            age(now().saturating_sub(w.started)),
            crate::setup::tilde(&w.cwd)
        );
    }
    Ok(out)
}

/// Fails once the worker is gone or waits for the user, or has sat idle without a report for a
/// few polls.
pub fn wait(r: &Registry, name: &str, timeout: Option<u64>) -> io::Result<()> {
    let deadline = timeout.map(|t| Instant::now() + Duration::from_secs(t));
    let mut idle = 0;
    loop {
        // Read again each time: `reeve start` fills in the handle once the agent is up.
        let w = r.get(name)?;
        let s = state(&w);
        idle = if s == State::Idle { idle + 1 } else { 0 };
        let fail = |why: &str| Err(io::Error::other(format!("worker {name} {why}")));
        match s {
            State::Done => return Ok(()),
            State::Gone => return fail(&gone_why(&w)),
            State::Blocked => return fail(&blocked_hint(&w)),
            _ if idle >= IDLE_POLLS => {
                return fail("is idle without a report; send it a message, then wait again");
            }
            _ if deadline.is_some_and(|d| Instant::now() >= d) => return fail("timed out"),
            _ => std::thread::sleep(POLL),
        }
    }
}

/// Idle between turns is normal; idle for this many polls in a row means it stopped early.
const IDLE_POLLS: u32 = 3;
const POLL: Duration = Duration::from_secs(2);

fn answer_where(w: &Worker) -> String {
    match (w.backend, w.agent) {
        (Backend::Pane, _) => "in its tab".to_string(),
        (Backend::Bg, Kind::Claude) => format!("with `claude attach {}`", w.handle),
        (Backend::Bg, Kind::Codex) => format!("with {}", resume_hint(w)),
    }
}

/// Where to see what a gone worker left.
fn left_where(w: &Worker) -> String {
    match (w.backend, w.agent) {
        (_, Kind::Claude) => format!(
            "see `claude logs {}`, or its transcript under ~/.claude/projects/",
            w.name
        ),
        (Backend::Pane, Kind::Codex) => "`codex resume --all` finds its session".into(),
        (Backend::Bg, Kind::Codex) => format!(
            "see {}; {} continues it",
            crate::setup::tilde(&exec_log(w)),
            resume_hint(w)
        ),
    }
}

fn look_where(w: &Worker) -> String {
    match (w.backend, w.agent) {
        (Backend::Pane, _) => "its tab".to_string(),
        (Backend::Bg, Kind::Claude) => format!("`claude logs {}`", w.name),
        (Backend::Bg, Kind::Codex) => crate::setup::tilde(&exec_log(w)),
    }
}

fn blocked_hint(w: &Worker) -> String {
    format!(
        "is waiting for the user, such as a permission prompt; answer it {}",
        answer_where(w)
    )
}

/// The last state `reeve watch` reported for each worker, and how long each has been idle.
#[derive(Default)]
pub struct Watch {
    /// Following workers by name: one already done or gone when first seen is news too, so a
    /// watch re-armed after Monitor's timeout still reports what ended meanwhile.
    named: bool,
    last: std::collections::HashMap<String, State>,
    idle: std::collections::HashMap<String, u32>,
}

impl Watch {
    pub fn named() -> Watch {
        Watch {
            named: true,
            ..Watch::default()
        }
    }

    /// One line per worker whose state changed since the last poll. A worker seen for the first
    /// time is reported only if it needs someone, so a restart doesn't replay old news.
    pub fn step(&mut self, polled: &[(Worker, State)]) -> Vec<String> {
        let mut lines = vec![];
        for (w, s) in polled {
            let idle = self.idle.entry(w.name.clone()).or_default();
            *idle = if *s == State::Idle { *idle + 1 } else { 0 };
            if *s == State::Unknown || (*s == State::Idle && *idle < IDLE_POLLS) {
                continue;
            }
            let before = self.last.insert(w.name.clone(), *s);
            let news = match before {
                None => {
                    matches!(s, State::Blocked | State::Idle)
                        || (self.named && matches!(s, State::Done | State::Gone))
                }
                Some(b) => b != *s,
            };
            if news {
                lines.push(change(w, before, *s));
            }
        }
        lines
    }

    /// With names: `Some(true)` once all are done, `Some(false)` once all are done or gone with
    /// at least one gone, else `None`. Without names it never ends.
    pub fn end(&self, names: &[String]) -> Option<bool> {
        let states: Vec<_> = names.iter().map(|n| self.last.get(n)).collect();
        let fin = |s: &Option<&State>| matches!(s, Some(State::Done | State::Gone));
        (!names.is_empty() && states.iter().all(fin))
            .then(|| states.iter().all(|s| *s == Some(&State::Done)))
    }
}

fn change(w: &Worker, before: Option<State>, s: State) -> String {
    let name = &w.name;
    match s {
        State::Starting => format!("{name} starting"),
        State::Running if before == Some(State::Starting) => format!("{name} running"),
        State::Running => format!("{name} running again"),
        State::Idle => format!(
            "{name} idle without a report: read {}; if it waits on its own job do nothing, else \
             nudge it, let it carry on, or ask the user",
            look_where(w)
        ),
        State::Blocked => format!(
            "{name} blocked (waiting for the user): answer it {} if you can, else ask the user",
            answer_where(w)
        ),
        State::Done => format!(
            "{name} done: report at {}; read it, then `reeve close {name}`",
            crate::setup::tilde(&w.report)
        ),
        State::Gone if w.error.is_some() || w.starting => format!("{name} gone: {}", gone_why(w)),
        State::Gone => format!("{name} gone without a report: {}", left_where(w)),
        State::Unknown => format!("{name} state unknown"),
    }
}

/// Prints a line on stdout for each change in the named workers, or in all of them. With names
/// it returns once each is done or gone: true if all are done.
pub fn watch(r: &Registry, names: &[String]) -> io::Result<bool> {
    for n in names {
        r.get(n)?;
    }
    let mut w = if names.is_empty() {
        Watch::default()
    } else {
        Watch::named()
    };
    loop {
        let all: Vec<Worker> = r
            .all()?
            .into_iter()
            .filter(|w| names.is_empty() || names.contains(&w.name))
            .collect();
        let polled: Vec<_> = all.iter().cloned().zip(states(&all)).collect();
        for line in w.step(&polled) {
            println!("{line}");
        }
        if let Some(all_done) = w.end(names) {
            return Ok(all_done);
        }
        std::thread::sleep(POLL);
    }
}

/// Drops a worker whose agent has gone from the registry; its report and prompt files stay.
pub fn rm(r: &Registry, name: &str) -> io::Result<()> {
    let w = r.get(name)?;
    let a = agent(&w);
    let s = state_of(&w, now(), || a);
    if a != Agent::Absent || s == State::Starting {
        return Err(io::Error::other(format!(
            "worker {name} is {s}; reeve drops only workers whose agent has gone"
        )));
    }
    r.remove(name)?;
    eprintln!("reeve: dropped worker {name}");
    Ok(())
}

/// The command that ends a worker's agent: closes its Herdr pane, which takes the tab with it,
/// stops its Claude background session, or kills the pid of its `codex exec`.
pub fn close_cmd(w: &Worker) -> (&'static str, Vec<String>) {
    let (cmd, args): (_, &[&str]) = match (w.backend, w.agent) {
        (Backend::Pane, _) => ("herdr", &["pane", "close"]),
        (Backend::Bg, Kind::Claude) => ("claude", &["stop"]),
        (Backend::Bg, Kind::Codex) => ("kill", &[]),
    };
    let args = args.iter().map(|a| a.to_string());
    (cmd, args.chain([w.handle.clone()]).collect())
}

/// From `git worktree list --porcelain`: the worktree of the worker, made by `claude -w NAME`
/// or `reeve run --worktree`, which lives under the main repo's `.claude/worktrees/` whatever
/// checkout the worker ran in, and its lock reason if it is locked.
pub fn worktree_named(list: &str, name: &str) -> Option<(PathBuf, Option<String>)> {
    let tail = format!("/.claude/worktrees/{name}");
    list.split("\n\n").find_map(|b| {
        let dir = b.lines().next()?.strip_prefix("worktree ")?;
        let lock = b
            .lines()
            .find_map(|l| l.strip_prefix("locked").map(|r| r.trim().to_string()));
        dir.ends_with(&tail).then(|| (PathBuf::from(dir), lock))
    })
}

/// The pid in Claude Code's lock reason, "claude agent … (pid 123)".
pub fn lock_pid(reason: &str) -> Option<u32> {
    let rest = reason.split("(pid ").nth(1)?;
    let digits = rest.split(|c: char| !c.is_ascii_digit()).next()?;
    digits.parse().ok()
}

/// From `git status --porcelain` and `git rev-list HEAD --not --remotes`: why a worktree must
/// stay, if it must.
pub fn removable(status: &str, unpushed: &str) -> Result<(), String> {
    let n = unpushed.lines().filter(|l| !l.is_empty()).count();
    if !status.trim().is_empty() {
        Err("it has uncommitted changes".into())
    } else if n > 0 {
        let s = if n == 1 { "" } else { "s" };
        Err(format!("it has {n} commit{s} not pushed"))
    } else {
        Ok(())
    }
}

fn git(dir: &Path, args: &[&OsStr]) -> io::Result<String> {
    let all = [OsStr::new("-C"), dir.as_os_str()]
        .into_iter()
        .chain(args.iter().copied());
    stdout("git", all)
}

/// Whether a git-ignored file is one a worker made on purpose and `reeve close` should save:
/// under `.claude/`, but not other worktrees, local settings or build output.
pub fn savable(rel: &str) -> bool {
    let Some(rest) = rel.strip_prefix(".claude/") else {
        return false;
    };
    !rest.starts_with("worktrees/")
        && rest != "settings.local.json"
        && !rest
            .split('/')
            .any(|c| matches!(c, "node_modules" | "target"))
}

/// The worktrees git lists strictly inside `dir`, with their lock reasons.
pub fn nested_in(list: &str, dir: &Path) -> Vec<(PathBuf, Option<String>)> {
    list.split("\n\n")
        .filter_map(|b| {
            let path = PathBuf::from(b.lines().next()?.strip_prefix("worktree ")?);
            let lock = b
                .lines()
                .find_map(|l| l.strip_prefix("locked").map(|r| r.trim().to_string()));
            (path.starts_with(dir) && path != dir).then_some((path, lock))
        })
        .collect()
}

/// From `git branch -r --points-at HEAD`: whether some remote branch is at HEAD.
pub fn pushed_at_head(remote_branches: &str) -> bool {
    remote_branches
        .lines()
        .any(|l| !l.trim().is_empty() && !l.contains(" -> "))
}

/// Why a worktree nested in the worker's must stay: it has changes, or no remote branch is at
/// its HEAD.
fn nested_stays(dir: &Path) -> io::Result<Option<String>> {
    let o = |s: &'static str| OsStr::new(s);
    let status = git(dir, &[o("status"), o("--porcelain")])?;
    let unpushed = git(dir, &[o("rev-list"), o("HEAD"), o("--not"), o("--remotes")])?;
    if let Err(why) = removable(&status, &unpushed) {
        return Ok(Some(why));
    }
    let at = git(dir, &[o("branch"), o("-r"), o("--points-at"), o("HEAD")])?;
    Ok((!pushed_at_head(&at)).then(|| "no remote branch at its commit".to_string()))
}

/// Unlocks a worktree whose lock names a process that has died, such as the "claude session
/// NAME (pid N …)" lock of `claude -w`; says why it stays if the process lives.
fn unlock_dead(main: &Path, dir: &Path, lock: Option<&str>) -> io::Result<Option<String>> {
    let Some(reason) = lock else {
        return Ok(None);
    };
    // The agent was only just closed, so give its process a moment to exit.
    let alive = lock_pid(reason).is_none_or(|p| {
        (0..10).all(|_| {
            let up = Command::new("kill")
                .args(["-0", &p.to_string()])
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if up {
                std::thread::sleep(Duration::from_millis(300));
            }
            up
        })
    });
    if alive {
        return Ok(Some(format!("locked ({reason})")));
    }
    let o = |s: &'static str| OsStr::new(s);
    git(main, &[o("worktree"), o("unlock"), dir.as_os_str()])?;
    Ok(None)
}

/// Copies the worktree's ignored files under `.claude/` to `to`, keeping their relative paths,
/// and returns those paths. A worker's PR body or notes are often ignored, so they are in no
/// commit and would go with the worktree.
fn save_ignored(dir: &Path, to: &Path) -> io::Result<Vec<String>> {
    let o = |s: &'static str| OsStr::new(s);
    let args = [
        o("ls-files"),
        o("-z"),
        o("--others"),
        o("--ignored"),
        o("--exclude-standard"),
        o("--"),
        o(".claude"),
    ];
    let files: Vec<String> = git(dir, &args)?
        .split('\0')
        .filter(|f| savable(f))
        .map(String::from)
        .collect();
    for f in &files {
        let dest = to.join(f);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(dir.join(f), dest)?;
    }
    Ok(files)
}

/// Removes the worker's own worktree when it is clean and pushed: first the worktrees nested
/// in it that are pushed at their HEAD, and after saving its ignored files under `.claude/`
/// next to the report. Unlocks a lock whose process has died; says why it stays otherwise.
/// Never forces.
fn remove_worktree(w: &Worker) -> io::Result<String> {
    let o = |s: &'static str| OsStr::new(s);
    let none = || Ok("no worktree of its own".to_string());
    if git(&w.cwd, &[o("rev-parse"), o("--is-inside-work-tree")]).is_err() {
        return none();
    }
    let list = git(&w.cwd, &[o("worktree"), o("list"), o("--porcelain")])?;
    let Some((dir, lock)) = worktree_named(&list, &w.name) else {
        return none();
    };
    // Not from the worktree itself, which `git worktree remove` is about to delete.
    let main = list
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("worktree "))
        .map_or_else(|| w.cwd.clone(), PathBuf::from);
    let shown = crate::setup::tilde(&dir);
    let keep = |why: String| Ok(format!("kept worktree {shown}: {why}"));
    let nested = nested_in(&list, &dir);
    for (n, _) in &nested {
        if let Some(why) = nested_stays(n)? {
            return keep(format!("its worktree {} has {why}", crate::setup::tilde(n)));
        }
    }
    let status = git(&dir, &[o("status"), o("--porcelain")])?;
    let unpushed = git(
        &dir,
        &[o("rev-list"), o("HEAD"), o("--not"), o("--remotes")],
    )?;
    // A nested worktree shows up as untracked, and is about to go.
    let status = status
        .lines()
        .filter(|l| {
            let Some(path) = l.strip_prefix("?? ") else {
                return true;
            };
            let path = dir.join(path.trim_end_matches('/'));
            !nested.iter().any(|(n, _)| n.starts_with(&path))
        })
        .collect::<Vec<_>>()
        .join("\n");
    if let Err(why) = removable(&status, &unpushed) {
        return keep(why);
    }
    let saved_to = w.report.with_file_name(&w.name);
    let saved = match save_ignored(&dir, &saved_to) {
        Ok(files) => files,
        Err(e) => return keep(format!("could not save its ignored files: {e}")),
    };
    if let Some(why) = unlock_dead(&main, &dir, lock.as_deref())? {
        return keep(why);
    }
    for (n, lock) in &nested {
        if let Some(why) = unlock_dead(&main, n, lock.as_deref())? {
            return keep(format!("its worktree {} is {why}", crate::setup::tilde(n)));
        }
        git(&main, &[o("worktree"), o("remove"), n.as_os_str()])?;
    }
    match git(&main, &[o("worktree"), o("remove"), dir.as_os_str()]) {
        Ok(_) if saved.is_empty() => Ok(format!("removed worktree {shown}")),
        Ok(_) => Ok(format!(
            "removed worktree {shown}; saved {} to {}",
            saved.join(", "),
            crate::setup::tilde(&saved_to)
        )),
        Err(e) => keep(e.to_string()),
    }
}

/// Ends a finished worker's agent (its tab, or its background session) and removes its
/// worktree when that loses nothing. The registry entry and the report stay. A worker that is
/// still running or waiting for the user is refused.
pub fn close(r: &Registry, name: &str) -> io::Result<()> {
    let w = r.get(name)?;
    let a = agent(&w);
    let s = state_of(&w, now(), || a);
    if matches!(
        s,
        State::Starting | State::Running | State::Blocked | State::Unknown
    ) {
        return Err(io::Error::other(format!(
            "worker {name} is {s}; close it once it is done, idle or gone"
        )));
    }
    if a != Agent::Absent {
        let (cmd, args) = close_cmd(&w);
        stdout(cmd, &args)?;
        eprintln!("reeve: closed {name}'s agent");
    }
    eprintln!("reeve: {}", remove_worktree(&w)?);
    Ok(())
}

pub fn report(r: &Registry, name: &str) -> io::Result<String> {
    let w = r.get(name)?;
    match fs::read_to_string(&w.report) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err(io::Error::other(format!(
            "worker {name} has no report yet ({})",
            state(&w)
        ))),
        r => r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("reeve-wk-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn worker(r: &Registry, name: &str, handle: &str) -> Worker {
        Worker {
            name: name.into(),
            backend: Backend::Pane,
            agent: Kind::Claude,
            cwd: "/work/x".into(),
            started: 100,
            report: r.report_path(name),
            handle: handle.into(),
            starting: false,
            error: None,
        }
    }

    #[test]
    fn registry_keeps_one_record_per_name_across_loads() {
        let dir = temp("reg");
        let r = Registry::at(dir.clone());
        assert_eq!(r.all().unwrap(), []);
        r.put(worker(&r, "a", "w1:p1")).unwrap();
        r.put(worker(&r, "b", "w1:p2")).unwrap();
        r.put(worker(&r, "a", "w1:p3")).unwrap();
        let again = Registry::at(dir.clone());
        let names: Vec<_> = again.all().unwrap().into_iter().map(|w| w.name).collect();
        assert_eq!(names, ["a", "b"]);
        assert_eq!(again.get("a").unwrap().handle, "w1:p3");
        assert!(
            again
                .get("c")
                .unwrap_err()
                .to_string()
                .contains("no worker c")
        );
        assert_eq!(r.report_path("a"), dir.join("reports/a.md"));
        assert!(again.remove("a").unwrap());
        assert!(!again.remove("a").unwrap());
        let names: Vec<_> = r.all().unwrap().into_iter().map(|w| w.name).collect();
        assert_eq!(names, ["b"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn registry_round_trips_as_json() {
        let dir = temp("json");
        let r = Registry::at(dir.clone());
        let mut w = worker(&r, "a", "abc");
        w.backend = Backend::Bg;
        r.put(w.clone()).unwrap();
        let raw = fs::read_to_string(dir.join("workers.json")).unwrap();
        assert!(raw.contains(r#""backend": "bg""#), "{raw}");
        assert!(
            !raw.contains("agent"),
            "a claude worker reads as before: {raw}"
        );
        assert_eq!(r.all().unwrap(), [w.clone()]);
        w.agent = Kind::Codex;
        r.put(w.clone()).unwrap();
        let raw = fs::read_to_string(dir.join("workers.json")).unwrap();
        assert!(raw.contains(r#""agent": "codex""#), "{raw}");
        assert_eq!(r.all().unwrap(), [w]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_report_means_done_whatever_the_agent_does() {
        for a in [
            Agent::Working,
            Agent::Idle,
            Agent::Blocked,
            Agent::Absent,
            Agent::Unknown,
        ] {
            assert_eq!(State::of(a, true), State::Done, "{a:?}");
        }
        assert_eq!(State::of(Agent::Working, false), State::Running);
        assert_eq!(State::of(Agent::Idle, false), State::Idle);
        assert_eq!(State::of(Agent::Blocked, false), State::Blocked);
        assert_eq!(State::of(Agent::Absent, false), State::Gone);
        assert_eq!(State::of(Agent::Unknown, false), State::Unknown);
    }

    #[test]
    fn a_starting_worker_is_starting_until_up_and_gone_if_it_never_comes_up() {
        let r = Registry::at(temp("starting"));
        let mut w = worker(&r, "a", "");
        w.starting = true;
        let unasked = || -> Agent { panic!("a starting worker has no agent to ask") };
        assert_eq!(state_of(&w, 100, unasked), State::Starting);
        assert_eq!(state_of(&w, 100 + START_SECS - 1, unasked), State::Starting);
        assert_eq!(state_of(&w, 100 + START_SECS, unasked), State::Gone);
        assert_eq!(gone_why(&w), format!("did not start within {START_SECS}s"));

        w.starting = false;
        assert_eq!(state_of(&w, 100, || Agent::Working), State::Running);

        w.error = Some("herdr: boom".into());
        assert_eq!(state_of(&w, 100, unasked), State::Gone);
        assert_eq!(gone_why(&w), "failed to start: herdr: boom");
        // An agent reeve took for failed may still have done its work.
        fs::create_dir_all(w.report.parent().unwrap()).unwrap();
        fs::write(&w.report, "done").unwrap();
        assert_eq!(state_of(&w, 100, unasked), State::Done);
        fs::remove_file(&w.report).unwrap();
        w.error = None;
        assert_eq!(
            gone_why(&w),
            "is gone without writing its report: see `claude logs a`, or its transcript under \
             ~/.claude/projects/"
        );
    }

    #[test]
    fn a_registry_from_before_starting_reads_as_started() {
        let w: Worker = serde_json::from_str(
            r#"{"name":"a","backend":"pane","cwd":"/w","started":1,"report":"/r/a.md","handle":"p1"}"#,
        )
        .unwrap();
        assert!(!w.starting);
        assert_eq!(w.error, None);
        let raw = serde_json::to_string(&w).unwrap();
        assert!(!raw.contains("starting") && !raw.contains("error"), "{raw}");
    }

    #[test]
    fn set_changes_only_the_worker_it_was_started_for() {
        let dir = temp("set");
        let r = Registry::at(dir.clone());
        r.put(worker(&r, "a", "")).unwrap();
        r.set("a", 100, |w| w.handle = "p1".into()).unwrap();
        assert_eq!(r.get("a").unwrap().handle, "p1");
        r.set("a", 99, |w| w.handle = "old".into()).unwrap();
        assert_eq!(r.get("a").unwrap().handle, "p1");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn herdr_panes_are_the_default_inside_herdr() {
        assert_eq!(Backend::choose(false, false, true), Backend::Pane);
        assert_eq!(Backend::choose(false, false, false), Backend::Bg);
        assert_eq!(Backend::choose(false, true, true), Backend::Bg);
        assert_eq!(Backend::choose(true, false, false), Backend::Pane);
    }

    #[test]
    fn names_follow_herdrs_agent_name_rule() {
        for ok in ["a", "fix-x", "w_2", &"a".repeat(32)] {
            assert_eq!(check_name(ok), Ok(()), "{ok}");
        }
        for bad in ["", "Fix", "1a", "a/b", "../x", "a.md", &"a".repeat(33)] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_last_argument_is_the_prompt() {
        let a = s(&["-w", "fix-x", "fix it"]);
        assert_eq!(split_prompt(&a), Ok((&a[..2], "fix it")));
        assert!(split_prompt(&[]).is_err());
        assert!(split_prompt(&s(&["fix it", "--model"])).is_err());
    }

    #[test]
    fn the_prompt_asks_for_the_report_file() {
        let p = with_report("fix it", Path::new("/st/reports/x.md"));
        assert!(p.starts_with("fix it "));
        assert!(!p.contains('\n'));
        assert!(p.contains("/st/reports/x.md"));
        assert!(p.contains("`done`, `partial` or `failed`"));
        assert!(p.contains("can't take it further"));
        assert!(!p.contains("reeve heavy"));
    }

    #[test]
    fn claudes_own_flags_come_last_so_none_swallows_the_prompt() {
        assert_eq!(
            claude_args(
                "x",
                Path::new("/st/reports/x.md"),
                &s(&["--add-dir", "../other"])
            ),
            s(&[
                "--add-dir",
                "../other",
                "--add-dir",
                "/st/reports",
                "--allowedTools",
                "Edit(//st/reports/x.md)",
                "-n",
                "x"
            ])
        );
    }

    #[test]
    fn a_worker_gets_a_labelled_tab_without_focus() {
        assert_eq!(
            tab_create_args("x", "/w", Some("w1")),
            s(&[
                "tab",
                "create",
                "--label",
                "x",
                "--cwd",
                "/w",
                "--no-focus",
                "--workspace",
                "w1"
            ])
        );
        assert_eq!(
            tab_create_args("x", "/w", None),
            s(&["tab", "create", "--label", "x", "--cwd", "/w", "--no-focus"])
        );
    }

    #[test]
    fn a_created_tab_names_its_root_pane() {
        let out: Value = serde_json::from_str(
            r#"{"result":{"root_pane":{"pane_id":"w1:p2"},"tab":{"tab_id":"w1:t3"}}}"#,
        )
        .unwrap();
        assert_eq!(
            created_tab(&out),
            Some(("w1:t3".to_string(), "w1:p2".to_string()))
        );
        assert_eq!(created_tab(&serde_json::json!({"result":{}})), None);
    }

    #[test]
    fn pane_worker_gets_its_prompt_as_a_launch_argument() {
        assert_eq!(
            pane_start_args("x", "p", Kind::Codex, &s(&["go"]))[4],
            "codex"
        );
        assert_eq!(
            pane_start_args("x", "w1:p2", Kind::Claude, &s(&["-n", "x", "fix it"])),
            s(&[
                "agent",
                "start",
                "x",
                "--kind",
                "claude",
                "--pane",
                "w1:p2",
                "--timeout",
                "10000",
                "--",
                "-n",
                "x",
                "fix it"
            ])
        );
    }

    #[test]
    fn a_busy_start_is_herdrs_timeout_or_a_pane_codex_claimed_and_nothing_else() {
        let timeout = r#"herdr: {"error":{"code":"timeout","message":"timed out waiting for agent startup"},"id":"cli:agent:start"}"#;
        let claimed = r#"herdr: {"error":{"code":"agent_name_not_found","message":"named agent x no longer owns the target terminal"},"id":"cli:agent:start"}"#;
        let bad = r#"herdr: {"error":{"code":"invalid_agent_argument","message":"timeout"},"id":"cli:agent:start"}"#;
        assert!(busy_at_start(timeout));
        assert!(busy_at_start(claimed));
        assert!(!busy_at_start(bad));
        assert!(!busy_at_start("herdr: not json"));
    }

    #[test]
    fn an_agent_that_waits_for_the_user_at_start_is_up() {
        let blocked = r#"herdr: {"error":{"code":"agent_not_ready","message":"agent x is blocked during startup and is not ready for prompts"},"id":"cli:agent:start"}"#;
        let timeout = r#"herdr: {"error":{"code":"timeout","message":"timed out waiting for agent startup"},"id":"cli:agent:start"}"#;
        assert!(blocked_at_start(blocked));
        assert!(!blocked_at_start(timeout));
    }

    #[test]
    fn a_pane_is_not_ready_while_herdr_says_its_shell_is_busy() {
        let busy = r#"herdr: {"error":{"code":"agent_pane_busy","message":"pane w1-2 is not an available shell"},"id":"cli:agent:start"}"#;
        let timeout = r#"herdr: {"error":{"code":"timeout","message":"timed out waiting for agent startup"},"id":"cli:agent:start"}"#;
        assert!(pane_not_ready(busy));
        assert!(!pane_not_ready(timeout));
        assert!(!pane_not_ready("herdr: agent_pane_busy"));
    }

    #[test]
    fn a_multi_line_prompt_reaches_a_pane_through_a_file() {
        let f = Path::new("/st/reports/x.prompt.md");
        assert_eq!(pane_prompt("fix it", f), None);
        let p = pane_prompt("fix it\nthen test", f).unwrap();
        assert!(p.contains("/st/reports/x.prompt.md"));
        assert!(!p.chars().any(char::is_control));
        assert!(pane_prompt("a\tb", f).is_some());
    }

    #[test]
    fn a_long_prompt_reaches_a_pane_through_a_file_so_the_typed_command_fits_the_tty() {
        let f = Path::new("/st/reports/x.prompt.md");
        let flags = claude_args("x", Path::new("/st/reports/x.md"), &s(&["--model", "opus"]));
        let long = "it's \"quoted\" (and) https://example.com/?a=1&b=2 ".repeat(40);
        let argv = pane_argv(Kind::Claude, &flags, &long, f).unwrap();
        assert!(typed_len(Kind::Claude, &argv) <= TYPED_MAX);
        assert!(argv.last().unwrap().contains("/st/reports/x.prompt.md"));
        let short = pane_argv(Kind::Claude, &flags, "fix it", f).unwrap();
        assert_eq!(short.last().unwrap(), "fix it");
    }

    #[test]
    fn typed_len_counts_the_quoting_herdr_types() {
        let n = |kind, a: &[&str]| typed_len(kind, &s(a));
        assert_eq!(n(Kind::Claude, &["-n", "x"]), "claude '-n' 'x'".len());
        assert_eq!(n(Kind::Claude, &["it's"]), r"claude 'it'\''s'".len());
        assert_eq!(n(Kind::Codex, &["go"]), "codex 'go'".len());
    }

    #[test]
    fn flags_too_long_to_type_fail_before_any_pane_opens() {
        let f = Path::new("/st/reports/x.prompt.md");
        let flags = s(&["--append-system-prompt", &"x".repeat(TYPED_MAX)]);
        let e = pane_argv(Kind::Codex, &flags, "fix it", f).unwrap_err();
        assert!(e.starts_with("codex's flags"), "{e}");
    }

    #[test]
    fn a_codex_worker_may_write_reeves_state_dir_in_a_writable_sandbox_unless_one_is_chosen() {
        let report = Path::new("/st/reports/x.md");
        assert_eq!(
            agent_args(Kind::Codex, "x", report, &s(&["-m", "o9"])),
            s(&["-m", "o9", "-s", "workspace-write", "--add-dir", "/st"])
        );
        for chosen in [
            &["-s", "read-only"][..],
            &["--sandbox", "danger-full-access"],
            &["--sandbox=danger-full-access"],
            &["--dangerously-bypass-approvals-and-sandbox"],
        ] {
            let args = codex_args(report, &s(chosen));
            assert!(!args.contains(&"workspace-write".into()), "{args:?}");
            assert_eq!(args[args.len() - 2..], s(&["--add-dir", "/st"]));
        }
        assert_eq!(
            agent_args(Kind::Claude, "x", report, &[]),
            claude_args("x", report, &[])
        );
    }

    #[test]
    fn a_background_codex_runs_exec_with_json() {
        assert_eq!(
            exec_argv(&s(&["--add-dir", "/st"]), "go"),
            s(&[
                "codex",
                "exec",
                "--json",
                "--skip-git-repo-check",
                "--add-dir",
                "/st",
                "go"
            ])
        );
    }

    #[test]
    fn a_background_codex_is_working_while_its_process_runs_codex() {
        assert_eq!(
            exec_agent(Ok("/opt/homebrew/bin/codex exec --json go\n")),
            Agent::Working
        );
        assert_eq!(exec_agent(Ok("")), Agent::Absent);
        assert_eq!(exec_agent(Ok("/usr/bin/vim notes\n")), Agent::Absent);
        assert_eq!(exec_agent(Err("ps: ")), Agent::Absent);
    }

    #[test]
    fn the_thread_comes_from_execs_first_event() {
        let log = "Reading prompt\n{\"type\":\"thread.started\",\"thread_id\":\"th-1\"}\n{\"type\":\"turn.started\"}";
        assert_eq!(exec_thread(log).as_deref(), Some("th-1"));
        assert_eq!(exec_thread("{\"type\":\"turn.started\"}"), None);
    }

    #[test]
    fn the_session_is_codex_only_when_codex_says_so_and_claude_doesnt() {
        assert_eq!(Kind::of_session(Some("th"), None), Kind::Codex);
        assert_eq!(Kind::of_session(Some("th"), Some("1")), Kind::Claude);
        assert_eq!(Kind::of_session(None, None), Kind::Claude);
        assert_eq!(Kind::of_session(Some(""), None), Kind::Claude);
    }

    #[test]
    fn a_codex_worker_says_where_to_answer_and_what_it_left() {
        let dir = temp("codex-hints");
        let r = Registry::at(dir.clone());
        let w = Worker {
            agent: Kind::Codex,
            backend: Backend::Bg,
            handle: "4242".into(),
            ..worker(&r, "cx", "")
        };
        assert_eq!(exec_log(&w), dir.join("logs/cx.jsonl"));
        assert!(
            gone_why(&w).contains("THREAD (the thread_id in "),
            "{}",
            gone_why(&w)
        );
        fs::create_dir_all(dir.join("logs")).unwrap();
        fs::write(
            exec_log(&w),
            "{\"type\":\"thread.started\",\"thread_id\":\"th-9\"}\n",
        )
        .unwrap();
        assert!(gone_why(&w).contains("`codex exec resume th-9 \"MESSAGE\"`"));
        assert!(started(&w).contains("codex exec resume th-9"));
        assert_eq!(stop_hint(&w), "`kill 4242`");
        let pane = Worker {
            backend: Backend::Pane,
            ..w.clone()
        };
        assert!(blocked_hint(&pane).contains("in its tab"));
        assert!(change(&pane, Some(State::Running), State::Gone).contains("codex resume --all"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn bg_worker_is_known_by_the_id_claude_prints() {
        assert_eq!(
            bg_argv(&s(&["-n", "x"]), "go"),
            s(&["claude", "--bg", "-n", "x", "go"])
        );
        let out = "Starting background service…\nbackgrounded · 0b7cb42f · e2e-bg\n  claude agents             list sessions\n";
        assert_eq!(bg_id(out).as_deref(), Some("0b7cb42f"));
        assert_eq!(bg_id("something else"), None);
    }

    #[test]
    fn pane_agent_follows_herdrs_status() {
        let pane = |agent: &str| format!(r#"{{"result":{{"pane":{{{agent}"pane_id":"w1:p2"}}}}}}"#);
        let with = |status: &str| pane(&format!(r#""agent":"claude","agent_status":"{status}","#));
        let claude = |p: Result<&str, &str>| pane_agent(p, Kind::Claude);
        assert_eq!(claude(Ok(&with("working"))), Agent::Working);
        assert_eq!(claude(Ok(&with("unknown"))), Agent::Working);
        for idle in ["idle", "done"] {
            assert_eq!(claude(Ok(&with(idle))), Agent::Idle, "{idle}");
        }
        assert_eq!(claude(Ok(&with("blocked"))), Agent::Blocked);
        assert_eq!(claude(Ok(&pane(""))), Agent::Absent);
        let closed =
            r#"herdr: {"error":{"code":"pane_not_found","message":"pane w1:p2 not found"}}"#;
        assert_eq!(claude(Err(closed)), Agent::Absent);
        assert_eq!(claude(Err("herdr: connection refused")), Agent::Unknown);
        assert_eq!(claude(Ok("garbage")), Agent::Unknown);
        let codex = pane(r#""agent":"codex","agent_status":"blocked","#);
        assert_eq!(pane_agent(Ok(&codex), Kind::Codex), Agent::Blocked);
        assert_eq!(claude(Ok(&codex)), Agent::Absent);
        assert_eq!(pane_agent(Ok(&with("working")), Kind::Codex), Agent::Absent);
    }

    #[test]
    fn bg_agent_follows_claude_agents() {
        let json = r#"[{"id":"a1","kind":"background","status":"busy","state":"working"},
            {"id":"a2","kind":"background","status":"idle","state":"done"},
            {"id":"a3","kind":"background","state":"blocked"},
            {"id":"a4","kind":"background","state":"stopped"},
            {"id":"a5","kind":"background","pid":7,"status":"waiting","state":"blocked","waitingFor":"permission prompt"},
            {"pid":1,"kind":"interactive","status":"busy"}]"#;
        assert_eq!(bg_agent(Ok(json), "a1"), Agent::Working);
        assert_eq!(bg_agent(Ok(json), "a2"), Agent::Idle);
        assert_eq!(bg_agent(Ok(json), "a3"), Agent::Blocked);
        assert_eq!(bg_agent(Ok(json), "a5"), Agent::Blocked);
        assert_eq!(bg_agent(Ok(json), "a4"), Agent::Absent);
        assert_eq!(bg_agent(Ok(json), "nope"), Agent::Absent);
        assert_eq!(bg_agent(Ok("not json"), "a1"), Agent::Unknown);
        assert_eq!(bg_agent(Err("claude: boom"), "a1"), Agent::Unknown);
    }

    #[test]
    fn brief_names_each_worker_with_what_it_is_doing() {
        assert_eq!(brief(&[]), "");
        assert_eq!(
            brief(&[("fix-x", State::Running, 60)]),
            "reeve: 1 worker: fix-x running. `reeve ls` shows details. Heavy commands go \
             through `reeve heavy`, in the background."
        );
        assert_eq!(
            brief(&[("a", State::Running, 60), ("b", State::Done, 60)]),
            "reeve: 2 workers: a running, b done (report ready). `reeve ls` shows details; \
             `reeve report NAME` prints a report, then `reeve close NAME` tidies up. Heavy commands go through `reeve heavy`, in the background."
        );
        let idle = brief(&[("c", State::Idle, 60)]);
        assert!(
            idle.contains("; look at an idle one and nudge it."),
            "{idle}"
        );
    }

    #[test]
    fn brief_leaves_out_old_finished_workers_but_never_live_ones() {
        let old = 8 * 86400;
        assert_eq!(
            brief(&[("a", State::Done, old), ("b", State::Gone, old)]),
            ""
        );
        let live = brief(&[("a", State::Idle, old), ("b", State::Gone, 3600)]);
        assert!(live.starts_with("reeve: 2 workers: a idle"), "{live}");
        assert!(live.contains("b gone without a report"), "{live}");
    }

    #[test]
    fn a_blocked_worker_says_where_to_answer_it() {
        let r = Registry::at(temp("blocked"));
        let pane = worker(&r, "fix-a", "w1:p2");
        assert!(blocked_hint(&pane).contains("in its tab"));
        let bg = Worker {
            backend: Backend::Bg,
            handle: "ab12".into(),
            ..pane
        };
        assert!(blocked_hint(&bg).contains("`claude attach ab12`"));
        let b = brief(&[("fix-a", State::Blocked, 1)]);
        assert!(b.contains("fix-a blocked (waiting for the user)"), "{b}");
    }

    #[test]
    fn a_started_worker_comes_with_how_to_follow_it() {
        let r = Registry::at(temp("started"));
        let pane = worker(&r, "fix-a", "w1:p2");
        let out = started(&pane);
        assert_eq!(out.lines().count(), 2, "{out}");
        assert!(out.contains("worker fix-a is starting"), "{out}");
        assert!(out.contains("fix-a.md"), "{out}");
        assert!(out.contains("follow it with `reeve watch fix-a`"), "{out}");
        assert!(out.contains("Monitor tool"), "{out}");
        assert!(out.contains("re-armed when it expires"), "{out}");
        assert!(out.contains("`reeve close fix-a`"), "{out}");
        assert!(out.contains("own job"), "{out}");
        assert!(out.contains("in its tab"), "{out}");
        let bg = Worker {
            backend: Backend::Bg,
            handle: "ab12".into(),
            ..pane
        };
        assert!(started(&bg).contains("`claude attach ab12`"));
    }

    fn polled(ws: &[(&Worker, State)]) -> Vec<(Worker, State)> {
        ws.iter().map(|(w, s)| ((*w).clone(), *s)).collect()
    }

    #[test]
    fn watch_prints_a_line_per_change_but_no_old_news() {
        let r = Registry::at(temp("watch"));
        let a = worker(&r, "a", "w1:p1");
        let b = worker(&r, "b", "w1:p2");
        let c = worker(&r, "c", "w1:p3");
        let mut w = Watch::default();
        let first = w.step(&polled(&[
            (&a, State::Running),
            (&b, State::Done),
            (&c, State::Gone),
        ]));
        assert_eq!(first, Vec::<String>::new());
        let next = w.step(&polled(&[
            (&a, State::Blocked),
            (&b, State::Done),
            (&c, State::Gone),
        ]));
        assert_eq!(next.len(), 1, "{next:?}");
        assert!(next[0].starts_with("a blocked"), "{next:?}");
        assert!(next[0].contains("in its tab"), "{next:?}");
        assert_eq!(
            w.step(&polled(&[(&a, State::Blocked)])),
            Vec::<String>::new()
        );
        assert_eq!(
            w.step(&polled(&[(&a, State::Unknown)])),
            Vec::<String>::new()
        );
        assert_eq!(
            w.step(&polled(&[(&a, State::Running)])),
            ["a running again"]
        );
        let done = w.step(&polled(&[(&a, State::Done)]));
        assert_eq!(done.len(), 1);
        assert!(done[0].starts_with("a done: report at "), "{done:?}");
        assert!(done[0].contains("a.md; read it, then"), "{done:?}");
    }

    #[test]
    fn watch_reports_idle_only_once_it_lasts() {
        let r = Registry::at(temp("watch-idle"));
        let a = worker(&r, "a", "w1:p1");
        let b = worker(&r, "b", "w1:p2");
        let mut w = Watch::default();
        assert!(
            w.step(&polled(&[(&a, State::Running), (&b, State::Blocked)]))[0]
                .starts_with("b blocked")
        );
        for _ in 1..IDLE_POLLS {
            assert_eq!(w.step(&polled(&[(&a, State::Idle)])), Vec::<String>::new());
        }
        let idle = w.step(&polled(&[(&a, State::Idle)]));
        assert_eq!(idle.len(), 1);
        assert!(idle[0].starts_with("a idle without a report"), "{idle:?}");
        let gone = w.step(&polled(&[(&a, State::Gone)]));
        assert!(gone[0].starts_with("a gone without a report"), "{gone:?}");
    }

    #[test]
    fn watch_follows_a_start_through_to_running_or_its_failure() {
        let r = Registry::at(temp("watch-start"));
        let mut a = worker(&r, "a", "w1:p1");
        a.starting = true;
        let mut w = Watch::default();
        assert_eq!(
            w.step(&polled(&[(&a, State::Starting)])),
            Vec::<String>::new()
        );
        assert_eq!(w.end(&s(&["a"])), None);
        a.starting = false;
        assert_eq!(w.step(&polled(&[(&a, State::Running)])), ["a running"]);

        let mut b = worker(&r, "b", "w1:p2");
        b.starting = true;
        w.step(&polled(&[(&b, State::Starting)]));
        b.starting = false;
        b.error = Some("herdr: boom".into());
        assert_eq!(
            w.step(&polled(&[(&b, State::Gone)])),
            ["b gone: failed to start: herdr: boom"]
        );
        assert_eq!(w.end(&s(&["b"])), Some(false));
    }

    #[test]
    fn watch_ends_once_every_named_worker_is_done_or_gone() {
        let r = Registry::at(temp("watch-end"));
        let a = worker(&r, "a", "w1:p1");
        let b = worker(&r, "b", "w1:p2");
        let names = s(&["a", "b"]);
        let mut w = Watch::default();
        w.step(&polled(&[(&a, State::Done), (&b, State::Running)]));
        assert_eq!(w.end(&names), None);
        assert_eq!(w.end(&[]), None);
        w.step(&polled(&[(&a, State::Done), (&b, State::Done)]));
        assert_eq!(w.end(&names), Some(true));
        w.step(&polled(&[(&a, State::Done), (&b, State::Gone)]));
        assert_eq!(w.end(&names), Some(false));
    }

    #[test]
    fn each_watch_line_carries_the_next_step_on_one_line() {
        let r = Registry::at(temp("watch-next"));
        let pane = worker(&r, "a", "w1:p1");
        let bg = Worker {
            backend: Backend::Bg,
            handle: "ab12".into(),
            ..worker(&r, "b", "")
        };
        let done = change(&pane, Some(State::Running), State::Done);
        assert!(done.ends_with("; read it, then `reeve close a`"), "{done}");
        let idle = change(&pane, Some(State::Running), State::Idle);
        assert!(idle.contains("read its tab"), "{idle}");
        assert!(idle.contains("own job"), "{idle}");
        assert!(idle.contains("ask the user"), "{idle}");
        assert!(change(&bg, None, State::Idle).contains("`claude logs b`"));
        let blocked = change(&bg, None, State::Blocked);
        assert!(blocked.contains("`claude attach ab12`"), "{blocked}");
        assert!(blocked.contains("ask the user"), "{blocked}");
        for l in [done, idle, blocked] {
            assert!(!l.contains('\n'), "{l}");
        }
    }

    #[test]
    fn a_named_watch_reports_a_worker_already_done_when_it_starts() {
        let r = Registry::at(temp("watch-named"));
        let a = worker(&r, "a", "w1:p1");
        let first = Watch::named().step(&polled(&[(&a, State::Done)]));
        assert_eq!(first.len(), 1, "{first:?}");
        assert!(first[0].starts_with("a done: report at "), "{first:?}");
        assert!(
            Watch::default()
                .step(&polled(&[(&a, State::Done)]))
                .is_empty()
        );
    }

    #[test]
    fn a_worker_is_closed_in_its_tab_or_as_a_background_session() {
        let r = Registry::at(temp("close"));
        let pane = worker(&r, "a", "w1:p1");
        assert_eq!(close_cmd(&pane), ("herdr", s(&["pane", "close", "w1:p1"])));
        let bg = Worker {
            backend: Backend::Bg,
            handle: "ab12".into(),
            ..pane
        };
        assert_eq!(close_cmd(&bg), ("claude", s(&["stop", "ab12"])));
        let exec = Worker {
            agent: Kind::Codex,
            handle: "4242".into(),
            ..bg
        };
        assert_eq!(close_cmd(&exec), ("kill", s(&["4242"])));
        let tab = Worker {
            backend: Backend::Pane,
            handle: "w1:p2".into(),
            ..exec
        };
        assert_eq!(close_cmd(&tab), ("herdr", s(&["pane", "close", "w1:p2"])));
    }

    #[test]
    fn next_steps_point_a_codex_worker_to_its_tab_or_its_log() {
        let r = Registry::at(temp("codex-next"));
        let tab = Worker {
            agent: Kind::Codex,
            ..worker(&r, "c", "w1:p1")
        };
        let exec = Worker {
            backend: Backend::Bg,
            handle: "4242".into(),
            ..tab.clone()
        };
        assert!(change(&tab, None, State::Idle).contains("read its tab"));
        let idle = change(&exec, None, State::Idle);
        assert!(idle.contains("c.jsonl"), "{idle}");
        assert!(!idle.contains("claude logs"), "{idle}");
        let started = started(&exec);
        assert!(
            started.contains("idle, read ") && started.contains("c.jsonl"),
            "{started}"
        );
        assert!(!started.contains("claude attach"), "{started}");
        assert!(started.contains("codex exec resume"), "{started}");
    }

    fn git_ok(dir: &Path, args: &[&str]) {
        let o = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }

    /// A repo with a pushed main, and a worker whose worktree `reeve run --worktree` made.
    fn repo_with_worktree(tag: &str, name: &str) -> (PathBuf, Worker) {
        let root = temp(tag);
        let (origin, repo) = (root.join("origin.git"), root.join("repo"));
        fs::create_dir_all(&repo).unwrap();
        git_ok(&root, &["init", "--bare", "-q", "-b", "main", "origin.git"]);
        git_ok(&repo, &["init", "-q", "-b", "main"]);
        git_ok(
            &repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "x",
            ],
        );
        git_ok(
            &repo,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git_ok(&repo, &["push", "-q", "origin", "main"]);
        let dir = crate::run::worktree(&repo, name).unwrap();
        let r = Registry::at(root.join("state/workers"));
        let w = Worker {
            agent: Kind::Codex,
            cwd: dir,
            ..worker(&r, name, "w1:p1")
        };
        (repo, w)
    }

    #[test]
    fn a_worktree_made_by_run_goes_when_clean_and_pushed() {
        let (repo, w) = repo_with_worktree("wt-clean", "w");
        let msg = remove_worktree(&w).unwrap();
        assert!(msg.starts_with("removed worktree "), "{msg}");
        assert!(!repo.join(".claude/worktrees/w").exists());
    }

    #[test]
    fn a_worktree_made_by_run_stays_with_unpushed_work() {
        let (repo, w) = repo_with_worktree("wt-dirty", "w");
        git_ok(
            &w.cwd,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "y",
            ],
        );
        let msg = remove_worktree(&w).unwrap();
        assert!(msg.contains("1 commit not pushed"), "{msg}");
        assert!(repo.join(".claude/worktrees/w").exists());
    }

    fn commit(dir: &Path) {
        git_ok(
            dir,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "y",
            ],
        );
    }

    /// A worktree of the worker's worktree, the way Claude makes one for screenshots.
    fn nested_worktree(w: &Worker, branch: &str) -> PathBuf {
        let nested = w.cwd.join(".claude/worktrees").join(branch);
        let path = nested.to_str().unwrap();
        git_ok(&w.cwd, &["worktree", "add", "-q", "-b", branch, path]);
        nested
    }

    #[test]
    fn ignored_files_under_claude_are_saved_before_the_worktree_goes() {
        let (repo, w) = repo_with_worktree("wt-ignored", "w");
        let ignore = ".claude/pr-body.md\n.claude/notes\nsettings.local.json\nnode_modules\n";
        fs::write(repo.join(".git/info/exclude"), ignore).unwrap();
        let notes = w.cwd.join(".claude/notes");
        fs::create_dir_all(&notes).unwrap();
        fs::write(notes.join("guide.md"), "the guide").unwrap();
        fs::write(w.cwd.join(".claude/pr-body.md"), "body").unwrap();
        fs::write(w.cwd.join(".claude/settings.local.json"), "{}").unwrap();
        fs::create_dir_all(w.cwd.join(".claude/node_modules/x")).unwrap();
        fs::write(w.cwd.join(".claude/node_modules/x/a.js"), "").unwrap();
        // Ignored, but not under .claude/.
        fs::write(w.cwd.join("node_modules"), "").unwrap();
        let saved = w.report.parent().unwrap().join("w");
        let msg = remove_worktree(&w).unwrap();
        assert!(msg.starts_with("removed worktree "), "{msg}");
        assert!(msg.contains(".claude/pr-body.md"), "{msg}");
        assert!(msg.contains(&crate::setup::tilde(&saved)), "{msg}");
        assert_eq!(
            fs::read_to_string(saved.join(".claude/pr-body.md")).unwrap(),
            "body"
        );
        assert_eq!(
            fs::read_to_string(saved.join(".claude/notes/guide.md")).unwrap(),
            "the guide"
        );
        assert!(!saved.join(".claude/settings.local.json").exists());
        assert!(!saved.join(".claude/node_modules").exists());
        assert!(!saved.join("node_modules").exists());
        assert!(!w.cwd.exists());
    }

    #[test]
    fn a_worktree_stays_when_its_ignored_files_cannot_be_saved() {
        let (repo, w) = repo_with_worktree("wt-nosave", "w");
        fs::write(repo.join(".git/info/exclude"), ".claude/pr-body.md\n").unwrap();
        fs::create_dir_all(w.cwd.join(".claude")).unwrap();
        fs::write(w.cwd.join(".claude/pr-body.md"), "body").unwrap();
        let reports = w.report.parent().unwrap();
        fs::create_dir_all(reports).unwrap();
        fs::write(reports.join("w"), "in the way").unwrap();
        let msg = remove_worktree(&w).unwrap();
        assert!(msg.starts_with("kept worktree "), "{msg}");
        assert!(msg.contains("could not save"), "{msg}");
        assert!(w.cwd.join(".claude/pr-body.md").exists());
    }

    #[test]
    fn a_worker_outside_any_git_repo_has_no_worktree_of_its_own() {
        let dir = temp("wt-plain");
        fs::create_dir_all(&dir).unwrap();
        let r = Registry::at(dir.join("state"));
        let w = Worker {
            cwd: dir.clone(),
            ..worker(&r, "w", "w1:p1")
        };
        assert_eq!(remove_worktree(&w).unwrap(), "no worktree of its own");
    }

    #[test]
    fn a_nested_worktree_pushed_at_its_head_goes_before_the_outer_one() {
        let (repo, w) = repo_with_worktree("wt-nested", "w");
        let nested = nested_worktree(&w, "pr-assets-x");
        commit(&nested);
        git_ok(&nested, &["push", "-q", "origin", "pr-assets-x"]);
        let msg = remove_worktree(&w).unwrap();
        assert!(msg.starts_with("removed worktree "), "{msg}");
        assert!(!nested.exists());
        assert!(!repo.join(".claude/worktrees/w").exists());
    }

    #[test]
    fn a_nested_worktree_with_unpushed_work_keeps_both_and_is_named() {
        let (repo, w) = repo_with_worktree("wt-nested-keep", "w");
        let nested = nested_worktree(&w, "pr-assets-x");
        commit(&nested);
        let msg = remove_worktree(&w).unwrap();
        assert!(msg.starts_with("kept worktree "), "{msg}");
        assert!(
            msg.contains("pr-assets-x") && msg.contains("not pushed"),
            "{msg}"
        );
        assert!(nested.exists());
        assert!(repo.join(".claude/worktrees/w").exists());
    }

    #[test]
    fn a_nested_worktree_with_changes_keeps_both_and_is_named() {
        let (_, w) = repo_with_worktree("wt-nested-dirty", "w");
        let nested = nested_worktree(&w, "pr-assets-x");
        fs::write(nested.join("shot.png"), "").unwrap();
        let msg = remove_worktree(&w).unwrap();
        assert!(
            msg.contains("pr-assets-x") && msg.contains("uncommitted"),
            "{msg}"
        );
        assert!(nested.exists() && w.cwd.exists());
    }

    /// A pid that was alive a moment ago and is gone now.
    fn dead_pid() -> u32 {
        let mut c = Command::new("true").spawn().unwrap();
        let pid = c.id();
        c.wait().unwrap();
        pid
    }

    #[test]
    fn a_lock_left_by_claudes_w_for_a_dead_pid_is_undone() {
        let (repo, w) = repo_with_worktree("wt-lock", "w");
        let reason = format!(
            "claude session w (pid {} start Thu Oct 29 07:00:56 2026)",
            dead_pid()
        );
        git_ok(&w.cwd, &["worktree", "lock", "--reason", &reason, "."]);
        let msg = remove_worktree(&w).unwrap();
        assert!(msg.starts_with("removed worktree "), "{msg}");
        assert!(!repo.join(".claude/worktrees/w").exists());
    }

    #[test]
    fn only_a_workers_own_ignored_files_under_claude_are_saved() {
        assert!(savable(".claude/pr-body.md"));
        assert!(savable(".claude/notes/guide.html"));
        assert!(!savable("pr-body.md"));
        assert!(!savable(".claude/settings.local.json"));
        assert!(!savable(".claude/worktrees/x/a.md"));
        assert!(!savable(".claude/tools/node_modules/a/b.js"));
        assert!(!savable(".claude/target/debug/x"));
        assert!(!savable(""));
    }

    #[test]
    fn nested_worktrees_are_those_git_lists_inside_the_workers() {
        let list = "worktree /repo\nHEAD a\n\n\
                    worktree /repo/.claude/worktrees/w\nHEAD b\n\n\
                    worktree /repo/.claude/worktrees/w/.claude/worktrees/s\nHEAD c\n\
                    locked claude session s (pid 7)\n\n\
                    worktree /repo/.claude/worktrees/w2\nHEAD d\n";
        let at = nested_in(list, Path::new("/repo/.claude/worktrees/w"));
        let dir = "/repo/.claude/worktrees/w/.claude/worktrees/s";
        assert_eq!(at, [(dir.into(), Some("claude session s (pid 7)".into()))]);
    }

    #[test]
    fn a_remote_branch_at_head_ignores_the_remote_s_own_pointer() {
        assert!(pushed_at_head("  origin/pr-assets-x\n"));
        assert!(!pushed_at_head(""));
        assert!(!pushed_at_head("  origin/HEAD -> origin/main\n"));
    }

    #[test]
    fn a_worktree_goes_only_when_clean_and_pushed() {
        assert_eq!(removable("", ""), Ok(()));
        let dirty = removable(" M src/a.rs\n?? b\n", "").unwrap_err();
        assert!(dirty.contains("uncommitted changes"), "{dirty}");
        let unpushed = removable("", "abc123\ndef456\n").unwrap_err();
        assert!(unpushed.contains("2 commits not pushed"), "{unpushed}");
    }

    #[test]
    fn a_workers_worktree_is_found_in_git_s_list_with_its_lock() {
        let list = "worktree /repo\nHEAD aaa\nbranch refs/heads/main\n\n\
                    worktree /repo/.claude/worktrees/a\nHEAD bbb\nbranch refs/heads/worktree-a\n\
                    locked claude agent agent-1 (pid 4242)\n\n\
                    worktree /repo/.claude/worktrees/b\nHEAD ccc\nlocked\n\n\
                    worktree /repo/.claude/worktrees/c\nHEAD ddd\n";
        let at = |n: &str| worktree_named(list, n);
        assert_eq!(at("zz"), None);
        assert_eq!(at("c"), Some(("/repo/.claude/worktrees/c".into(), None)));
        assert_eq!(at("b").unwrap().1, Some(String::new()));
        let (dir, reason) = at("a").unwrap();
        assert_eq!(dir, PathBuf::from("/repo/.claude/worktrees/a"));
        assert_eq!(lock_pid(&reason.unwrap()), Some(4242));
        assert_eq!(
            lock_pid("claude session x (pid 69786 start Thu Oct 29 07:00:56 2026)"),
            Some(69786)
        );
        assert_eq!(lock_pid(""), None);
    }

    #[test]
    fn brief_is_capped_however_many_workers_there_are() {
        let names: Vec<String> = (0..500).map(|i| format!("{i:0>32}")).collect();
        let rows: Vec<_> = names
            .iter()
            .map(|n| (n.as_str(), State::Running, 1))
            .collect();
        let b = brief(&rows);
        assert!(b.len() <= BRIEF_MAX + 3, "{}", b.len());
        assert!(b.contains("492 more"), "{b}");
        assert!(b.ends_with(HEAVY_REMINDER), "{b}");
    }

    #[test]
    fn age_is_short() {
        assert_eq!(age(5), "5s");
        assert_eq!(age(125), "2m");
        assert_eq!(age(3 * 3600 + 5), "3h");
        assert_eq!(age(2 * 86400), "2d");
    }
}
