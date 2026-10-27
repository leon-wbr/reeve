//! Named workers: agents started by `reeve run --name` in a Herdr pane or as a Claude Code
//! background session, recorded so that any later session can find them and their reports.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worker {
    pub name: String,
    pub backend: Backend,
    pub cwd: PathBuf,
    /// Unix seconds.
    pub started: u64,
    pub report: PathBuf,
    /// The Herdr pane ID or the background session's short id.
    pub handle: String,
}

/// What a worker's pane or session is doing, as far as Herdr or `claude agents` can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Working,
    /// Waiting for input: finished its turn, or asking a question.
    Idle,
    Absent,
    /// The probe itself failed.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Running,
    Idle,
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
            (Agent::Absent, false) => State::Gone,
            (Agent::Unknown, false) => State::Unknown,
        }
    }
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match self {
            State::Running => "running",
            State::Idle => "idle",
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
        fs::create_dir_all(&self.dir)?;
        let lock = File::create(self.dir.join("workers.lock"))?;
        lock.lock()?;
        let mut all = self.all()?;
        match all.iter_mut().find(|x| x.name == w.name) {
            Some(x) => *x = w,
            None => all.push(w),
        }
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

pub fn with_report(prompt: &str, report: &Path) -> String {
    format!(
        "{prompt} Run test suites, e2e, builds and installs through `reeve heavy -- <command>`. \
         When you are finished, write your final report as Markdown to {}. Write that file last \
         and only once: its existence tells reeve that you are done.",
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
pub fn typed_len(argv: &[String]) -> usize {
    argv.iter()
        .map(|a| 3 + a.len() + 3 * a.matches('\'').count())
        .sum::<usize>()
        + "claude".len()
}

/// Claude's argv for a pane. A prompt that can't be typed whole goes through `file`, which
/// the caller writes; flags too long to type are an error.
pub fn pane_argv(flags: &[String], prompt: &str, file: &Path) -> Result<Vec<String>, String> {
    let with = |p: String| -> Vec<String> { flags.iter().cloned().chain([p]).collect() };
    let argv = with(prompt.into());
    if pane_prompt(prompt, file).is_none() && typed_len(&argv) <= TYPED_MAX {
        return Ok(argv);
    }
    let argv = with(pointer(file));
    match typed_len(&argv) {
        n if n <= TYPED_MAX => Ok(argv),
        n => Err(format!(
            "claude's flags are too long to type into a pane ({n} bytes, at most {TYPED_MAX})"
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

pub fn pane_start_args(name: &str, pane: &str, claude: &[String]) -> Vec<String> {
    [
        "agent",
        "start",
        name,
        "--kind",
        "claude",
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

/// A launch prompt keeps claude busy, and `herdr agent start` times out waiting for it to
/// become idle.
pub fn busy_at_start(err: &str) -> bool {
    herdr_error_code(err).is_some_and(|c| c == "timeout")
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

/// From `herdr pane get`: its JSON, or the error naming a closed pane.
pub fn pane_agent(probe: Result<&str, &str>) -> Agent {
    let out = match probe {
        Err(e) if e.contains(r#""code":"pane_not_found""#) => return Agent::Absent,
        Err(_) => return Agent::Unknown,
        Ok(o) => o,
    };
    let Ok(v) = serde_json::from_str::<Value>(out) else {
        return Agent::Unknown;
    };
    let pane = &v["result"]["pane"];
    if pane["agent"] != "claude" {
        return Agent::Absent;
    }
    match pane["agent_status"].as_str() {
        Some("idle" | "done" | "blocked") => Agent::Idle,
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
        Some(_) => Agent::Idle,
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

/// Opens a tab labelled NAME in the caller's workspace without focusing it and starts claude
/// in its pane with the prompt as its argument. Returns the pane ID; the tab is closed again
/// if claude didn't start.
fn start_pane(name: &str, cwd: &Path, claude: &[String]) -> io::Result<String> {
    let workspace = std::env::var("HERDR_WORKSPACE_ID").ok();
    let cwd = cwd.display().to_string();
    let created = herdr(tab_create_args(name, &cwd, workspace.as_deref()))?;
    let (tab, pane) = created_tab(&created)
        .ok_or_else(|| io::Error::other("herdr tab create returned no pane id"))?;
    let start = || herdr(pane_start_args(name, &pane, claude));
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
        Err(e) if busy_at_start(&e.to_string()) => wait_for_claude(&pane)
            .and_then(|_| herdr(["agent", "rename", &pane, name]))
            .map_err(|_| e),
        r => r,
    };
    if let Err(e) = started {
        let _ = herdr(["tab", "close", &tab]);
        return Err(e);
    }
    Ok(pane)
}

fn wait_for_claude(pane: &str) -> io::Result<()> {
    for _ in 0..20 {
        let a = pane_agent(
            probe("herdr", ["pane", "get", pane])
                .as_deref()
                .map_err(|e| e.as_str()),
        );
        if matches!(a, Agent::Working | Agent::Idle) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(io::Error::other(format!("no claude in pane {pane}")))
}

/// Returns the short id that `claude --bg` prints and `claude agents` lists.
fn start_bg(claude: &[String], prompt: &str) -> io::Result<String> {
    let argv = bg_argv(claude, prompt);
    let o = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .output()?;
    io::stderr().write_all(&o.stderr)?;
    io::stdout().write_all(&o.stdout)?;
    if !o.status.success() {
        return Err(io::Error::other(format!(
            "claude --bg failed ({})",
            o.status
        )));
    }
    bg_id(&String::from_utf8_lossy(&[o.stdout, o.stderr].concat()))
        .ok_or_else(|| io::Error::other("claude --bg printed no session id"))
}

fn agent(w: &Worker) -> Agent {
    match w.backend {
        Backend::Pane => {
            let p = probe("herdr", ["pane", "get", &w.handle]);
            pane_agent(p.as_deref().map_err(|e| e.as_str()))
        }
        Backend::Bg => {
            let p = probe("claude", ["agents", "--json"]);
            bg_agent(p.as_deref().map_err(|e| e.as_str()), &w.handle)
        }
    }
}

/// The agent is checked before the report, so one that writes its report and exits in
/// between still counts as done.
pub fn state(w: &Worker) -> State {
    let agent = agent(w);
    State::of(agent, w.report.is_file())
}

pub fn run(r: &Registry, name: &str, backend: Backend, args: &[String]) -> io::Result<()> {
    check_name(name).map_err(io::Error::other)?;
    let (flags, prompt) = split_prompt(args).map_err(io::Error::other)?;
    if let Ok(old) = r.get(name)
        && agent(&old) != Agent::Absent
    {
        let close = match old.backend {
            Backend::Pane => format!("close pane {}", old.handle),
            Backend::Bg => format!("`claude rm {}`", old.handle),
        };
        return Err(io::Error::other(format!(
            "worker {name} still has an agent; {close} or use another name"
        )));
    }
    let report = r.report_path(name);
    let reports = report.parent().expect("reports dir");
    fs::create_dir_all(reports)?;
    let old = report.with_extension("md.old");
    match fs::rename(&report, &old) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let claude = claude_args(name, &report, flags);
    let prompt = with_report(prompt, &report);
    let cwd = std::env::current_dir()?;
    let started = match backend {
        Backend::Pane => {
            let file = reports.join(format!("{name}.prompt.md"));
            pane_argv(&claude, &prompt, &file)
                .map_err(io::Error::other)
                .and_then(|argv| {
                    if argv.last() != Some(&prompt) {
                        fs::write(&file, &prompt)?;
                    }
                    start_pane(name, &cwd, &argv)
                })
        }
        Backend::Bg => start_bg(&claude, &prompt),
    };
    let handle = match started {
        Ok(h) => h,
        Err(e) => {
            let _ = fs::rename(&old, &report);
            return Err(e);
        }
    };
    let _ = fs::remove_file(&old);
    r.put(Worker {
        name: name.into(),
        backend,
        cwd,
        started: now(),
        report,
        handle,
    })?;
    eprintln!(
        "reeve: worker {name} started; `reeve wait {name}` blocks until its report is written"
    );
    Ok(())
}

/// Workers older than this show in the brief listing only while they still have an agent.
const BRIEF_DAYS: u64 = 7;
const BRIEF_NAMES: usize = 8;
/// Far below the 10,000 characters Claude Code takes from a hook.
pub const BRIEF_MAX: usize = 1000;

/// The brief shows only when other workers share the machine, which is when the queue matters.
const HEAVY_REMINDER: &str = " Heavy commands go through `reeve heavy`.";

/// One line for a session-start hook, e.g. `reeve: 2 workers: a running, b done (report
/// ready).`, or nothing when there is nothing worth telling.
pub fn brief(workers: &[(&str, State, u64)]) -> String {
    let shown: Vec<_> = workers
        .iter()
        .filter(|(_, s, age)| {
            matches!(s, State::Running | State::Idle | State::Unknown) || *age < BRIEF_DAYS * 86400
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
                State::Running => "running",
                State::Idle => "idle (may be waiting for input)",
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
        out += "; `reeve report NAME` prints a report";
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
    all.iter()
        .map(|w| {
            let agent = match w.backend {
                Backend::Pane => agent(w),
                Backend::Bg => {
                    let p = bg.get_or_insert_with(|| probe("claude", ["agents", "--json"]));
                    bg_agent(p.as_deref().map_err(|e| e.as_str()), &w.handle)
                }
            };
            State::of(agent, w.report.is_file())
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
    let mut out = format!("{:width$}  STATE    BACKEND  AGE  CWD", "NAME");
    for (w, state) in all.iter().zip(states(&all)) {
        let backend = match w.backend {
            Backend::Pane => "pane",
            Backend::Bg => "bg",
        };
        out += &format!(
            "\n{:width$}  {:7}  {backend:7}  {:>3}  {}",
            w.name,
            state.to_string(),
            age(now().saturating_sub(w.started)),
            crate::setup::tilde(&w.cwd)
        );
    }
    Ok(out)
}

/// Fails once the worker is gone, or has sat idle without a report for a few polls, which
/// usually means it is asking a question.
pub fn wait(r: &Registry, name: &str, timeout: Option<u64>) -> io::Result<()> {
    let w = r.get(name)?;
    let deadline = timeout.map(|t| Instant::now() + Duration::from_secs(t));
    let mut idle = 0;
    loop {
        let s = state(&w);
        idle = if s == State::Idle { idle + 1 } else { 0 };
        let fail = |why: &str| Err(io::Error::other(format!("worker {name} {why}")));
        match s {
            State::Done => return Ok(()),
            State::Gone => return fail("is gone without writing its report"),
            _ if idle >= 3 => {
                return fail("is idle without a report; it may be asking a question");
            }
            _ if deadline.is_some_and(|d| Instant::now() >= d) => return fail("timed out"),
            _ => std::thread::sleep(Duration::from_secs(2)),
        }
    }
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
            cwd: "/work/x".into(),
            started: 100,
            report: r.report_path(name),
            handle: handle.into(),
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
        assert_eq!(r.all().unwrap(), [w]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_report_means_done_whatever_the_agent_does() {
        for a in [Agent::Working, Agent::Idle, Agent::Absent, Agent::Unknown] {
            assert_eq!(State::of(a, true), State::Done, "{a:?}");
        }
        assert_eq!(State::of(Agent::Working, false), State::Running);
        assert_eq!(State::of(Agent::Idle, false), State::Idle);
        assert_eq!(State::of(Agent::Absent, false), State::Gone);
        assert_eq!(State::of(Agent::Unknown, false), State::Unknown);
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
        assert!(p.contains("reeve heavy"));
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
            pane_start_args("x", "w1:p2", &s(&["-n", "x", "fix it"])),
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
    fn a_busy_start_is_herdrs_timeout_and_nothing_else() {
        let timeout = r#"herdr: {"error":{"code":"timeout","message":"timed out waiting for agent startup"},"id":"cli:agent:start"}"#;
        let bad = r#"herdr: {"error":{"code":"invalid_agent_argument","message":"timeout"},"id":"cli:agent:start"}"#;
        assert!(busy_at_start(timeout));
        assert!(!busy_at_start(bad));
        assert!(!busy_at_start("herdr: not json"));
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
        let argv = pane_argv(&flags, &long, f).unwrap();
        assert!(typed_len(&argv) <= TYPED_MAX);
        assert!(argv.last().unwrap().contains("/st/reports/x.prompt.md"));
        let short = pane_argv(&flags, "fix it", f).unwrap();
        assert_eq!(short.last().unwrap(), "fix it");
    }

    #[test]
    fn typed_len_counts_the_quoting_herdr_types() {
        assert_eq!(typed_len(&s(&["-n", "x"])), "claude '-n' 'x'".len());
        assert_eq!(typed_len(&s(&["it's"])), r"claude 'it'\''s'".len());
    }

    #[test]
    fn flags_too_long_to_type_fail_before_any_pane_opens() {
        let f = Path::new("/st/reports/x.prompt.md");
        let flags = s(&["--append-system-prompt", &"x".repeat(TYPED_MAX)]);
        assert!(pane_argv(&flags, "fix it", f).is_err());
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
        assert_eq!(pane_agent(Ok(&with("working"))), Agent::Working);
        assert_eq!(pane_agent(Ok(&with("unknown"))), Agent::Working);
        for idle in ["idle", "done", "blocked"] {
            assert_eq!(pane_agent(Ok(&with(idle))), Agent::Idle, "{idle}");
        }
        assert_eq!(pane_agent(Ok(&pane(""))), Agent::Absent);
        let closed =
            r#"herdr: {"error":{"code":"pane_not_found","message":"pane w1:p2 not found"}}"#;
        assert_eq!(pane_agent(Err(closed)), Agent::Absent);
        assert_eq!(pane_agent(Err("herdr: connection refused")), Agent::Unknown);
        assert_eq!(pane_agent(Ok("garbage")), Agent::Unknown);
    }

    #[test]
    fn bg_agent_follows_claude_agents() {
        let json = r#"[{"id":"a1","kind":"background","status":"busy","state":"working"},
            {"id":"a2","kind":"background","status":"idle","state":"done"},
            {"id":"a3","kind":"background","state":"blocked"},
            {"id":"a4","kind":"background","state":"stopped"},
            {"pid":1,"kind":"interactive","status":"busy"}]"#;
        assert_eq!(bg_agent(Ok(json), "a1"), Agent::Working);
        assert_eq!(bg_agent(Ok(json), "a2"), Agent::Idle);
        assert_eq!(bg_agent(Ok(json), "a3"), Agent::Idle);
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
             through `reeve heavy`."
        );
        assert_eq!(
            brief(&[("a", State::Running, 60), ("b", State::Done, 60)]),
            "reeve: 2 workers: a running, b done (report ready). `reeve ls` shows details; \
             `reeve report NAME` prints a report. Heavy commands go through `reeve heavy`."
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
