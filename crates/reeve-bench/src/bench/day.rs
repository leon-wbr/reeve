//! `reeve-bench day`: a working day in one repo. A backlog of independent tasks, a test suite that
//! takes a minute and shares one test database with every checkout, a six-minute fixture rebuild
//! that holds that database, a decision only the user can make, and an urgent task the user adds
//! five minutes in. A scripted user answers from fixed rules and falls back to a cheap model only
//! for questions no rule covers. Every session of a run, workers and successors included, starts
//! through a `claude` wrapper that applies the arm's model, effort, setting sources and settings,
//! so the arm's parameters hold for the whole run. See bench/day/README.md.

use super::check::{passes, temp_dir};
use super::run::{Comm, append, clock, mine, never_claimed, read_stderr, read_stdout, say};
use super::score::fmt;
use super::sim::{Sim, simulate};
use super::{js_str, round, term};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

pub const ITEMS: [&str; 6] = ["price", "vat", "rename", "red", "fixtures", "discount"];
const MAX_INTERVENTIONS: usize = 10;
/// The line the asks tell the session to end with once everything is merged.
pub const CLAIM: &str = "ALL DONE";

/// An arm: `bench/day/arms/<ARM>/arm.json`. Every field is a parameter of the run.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Arm {
    pub model: String,
    pub effort: Option<String>,
    /// `--setting-sources`; `project,local` keeps the owner's user setup out.
    pub setting_sources: Option<String>,
    pub autocompact: Option<String>,
    /// Context, in tokens, past which a session is told to hand off: `REEVE_HANDOFF_TOKENS` for
    /// reeve's hook, or the harness's own check with `handoff: "notes"`.
    pub context_budget: Option<u64>,
    /// `notes` for arms without reeve: the harness tells the session at the budget to write
    /// notes and stop, then starts a fresh session on them.
    pub handoff: Option<String>,
    /// A file, relative to the arm, that `reeve --skill` prints instead of the built-in manual.
    pub manual: Option<String>,
    /// A directory, relative to `bench/`, with the arm's `settings.json` and a `repo/` overlay.
    pub setup: Option<String>,
    /// Appended to the first session's system prompt, relative to the arm.
    pub system: Option<String>,
    pub note: Option<String>,
}

impl Default for Arm {
    fn default() -> Arm {
        Arm {
            model: "sonnet".into(),
            effort: None,
            setting_sources: None,
            autocompact: None,
            context_budget: None,
            handoff: None,
            manual: None,
            setup: None,
            system: None,
            note: None,
        }
    }
}

impl Arm {
    /// The flags every session of the run starts with, before its own.
    pub fn flags(&self, settings: &Path) -> Vec<String> {
        let mut f = vec!["--model".to_string(), self.model.clone()];
        let pairs = [
            ("--effort", &self.effort),
            ("--setting-sources", &self.setting_sources),
            ("--autocompact", &self.autocompact),
        ];
        for (flag, v) in pairs {
            if let Some(v) = v {
                f.extend([flag.to_string(), v.clone()]);
            }
        }
        f.extend(["--settings".into(), settings.display().to_string()]);
        f
    }
}

/// How fast the driver runs; tests shrink it.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Timing {
    pub tick_ms: u64,
    pub check_ms: i64,
    /// The user answers this long after the session went quiet.
    pub reply_ms: i64,
    pub grace_ms: i64,
    pub total_ms: i64,
    /// Multiplies the follow-ups' and the poll's times.
    pub scale: f64,
}

impl Default for Timing {
    fn default() -> Timing {
        Timing {
            tick_ms: 2_000,
            check_ms: 30_000,
            reply_ms: 60_000,
            grace_ms: 5 * 60_000,
            total_ms: 50 * 60_000,
            scale: 1.0,
        }
    }
}

pub struct Opts {
    pub arm: String,
    pub n: String,
    /// The repo's `bench/`.
    pub root: PathBuf,
    pub trials: PathBuf,
    /// The real `claude`, by absolute path.
    pub claude: PathBuf,
    pub reeve: Option<PathBuf>,
    /// Where `.claude/projects` holds the transcripts.
    pub home: PathBuf,
    pub timing: Timing,
    pub hold_s: Option<u64>,
    pub rebuild_s: Option<u64>,
    pub sim_model: String,
    pub extra_env: Vec<(OsString, OsString)>,
    /// The arm's `arm.json` text, instead of reading it from `bench/day/arms`; for tests.
    pub arm_json: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Rule {
    pub id: String,
    pub any: Vec<String>,
    #[serde(default)]
    pub once: bool,
    pub kind: String,
    pub reply: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Poll {
    pub after_s: f64,
    pub kind: String,
    pub reply: String,
}

/// The scripted user: `bench/day/user.json`.
#[derive(Debug, Deserialize)]
pub struct User {
    pub rules: Vec<Rule>,
    pub poll: Option<Poll>,
}

/// The lines of `text` that ask something.
pub fn questions(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| l.contains('?'))
        .map(str::to_lowercase)
        .collect()
}

/// The first rule whose words appear in a question of `text`, skipping used `once` rules.
pub fn scripted<'a>(user: &'a User, text: &str, used: &[String]) -> Option<&'a Rule> {
    let qs = questions(text);
    user.rules.iter().find(|r| {
        !(r.once && used.contains(&r.id))
            && qs.iter().any(|q| r.any.iter().any(|w| q.contains(w.as_str())))
    })
}

/// Whether a line of `text` is the claim on its own, give or take markdown and a full stop.
/// "ALL DONE isn't true yet" is not a claim.
pub fn claimed(text: &str) -> bool {
    text.lines()
        .any(|l| l.trim().trim_matches(|c: char| "*_`#.!> ".contains(c)) == CLAIM)
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Claude Code's subcommands, which take no session flags.
const SUBCOMMANDS: &str = "agents|attach|logs|stop|kill|rm|respawn|auth|mcp|plugin|plugins|doctor|update|upgrade|install|setup-token|import|purge|gateway|auto-mode|ultrareview";

/// The `claude` every session of the run starts through: the arm's flags first, so a session's
/// own flags (a worker's `--model`) still win.
pub fn claude_wrapper(real: &Path, flags: &[String]) -> String {
    let flags: Vec<String> = flags.iter().map(|f| quote(f)).collect();
    format!(
        "#!/bin/sh\n# reeve-bench day: the arm's flags for every session of the run\ncase \"$1\" in\n  {SUBCOMMANDS}) exec {real} \"$@\" ;;\nesac\nexec {real} {flags} \"$@\"\n",
        real = quote(&real.display().to_string()),
        flags = flags.join(" "),
    )
}

/// The `reeve` the run sees when the arm pins a manual: `reeve` and `reeve --skill` print it.
pub fn reeve_wrapper(real: &Path, manual: &Path) -> String {
    format!(
        "#!/bin/sh\n# reeve-bench day: the arm's manual\nif [ $# -eq 0 ] || [ \"$*\" = --skill ]; then exec cat {manual}; fi\nexec {real} \"$@\"\n",
        manual = quote(&manual.display().to_string()),
        real = quote(&real.display().to_string()),
    )
}

/// The variables every session of a run needs, its own or its reeve's.
const RUN_ENV: [&str; 6] = [
    "HOME",
    "PATH",
    "XDG_STATE_HOME",
    "REEVE_HOME",
    "REEVE_HANDOFF_TOKENS",
    "ORCH_HOME",
];

/// The arm's settings plus the run's environment under `env`. `claude --bg` sessions, reeve's
/// workers and successors, are started by Claude Code's daemon with the daemon's environment,
/// not the caller's, so without this their reeve keeps its state wherever the daemon's does:
/// another run's, or the owner's.
pub fn run_settings(arm: &str, env: &[(OsString, OsString)]) -> io::Result<String> {
    let mut s: Value = serde_json::from_str(arm).map_err(io::Error::other)?;
    let vars: Map<String, Value> = env
        .iter()
        .filter(|(k, _)| RUN_ENV.iter().any(|v| k == v) || k.to_string_lossy().starts_with("REEVE_BENCH_"))
        .map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned().into()))
        .collect();
    let obj = s.as_object_mut().ok_or_else(|| io::Error::other("settings aren't an object"))?;
    let e = obj.entry("env").or_insert_with(|| json!({}));
    if let Some(e) = e.as_object_mut() {
        e.extend(vars);
    }
    Ok(serde_json::to_string_pretty(&s)? + "\n")
}

pub(super) fn write_exe(path: &Path, text: &str) -> io::Result<()> {
    fs::write(path, text)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

/// `name` on PATH, skipping `skip`.
pub fn which(name: &str, skip: Option<&Path>) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|d| Some(d.as_path()) != skip)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// Token use and cost of a set of transcripts, priced per model at list prices.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Usage {
    pub input: f64,
    pub cache_write: f64,
    pub cache_read: f64,
    pub output: f64,
    pub cost: f64,
    /// The largest context one turn read.
    pub peak: f64,
    pub turns: usize,
}

/// $ per million input, cache write, cache read and output tokens.
pub fn price(model: &str) -> [f64; 4] {
    if model.contains("opus") {
        [5.0, 6.25, 0.5, 25.0]
    } else if model.contains("haiku") {
        [1.0, 1.25, 0.1, 5.0]
    } else {
        [3.0, 3.75, 0.3, 15.0]
    }
}

impl Usage {
    /// Adds a transcript's assistant turns, each message id once.
    pub fn add(&mut self, text: &str, seen: &mut HashSet<String>) {
        for line in text.lines() {
            let Ok(m) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let u = &m["message"]["usage"];
            if m["type"] != "assistant" || !u.is_object() {
                continue;
            }
            if let Some(id) = m["message"]["id"].as_str()
                && !seen.insert(id.to_string())
            {
                continue;
            }
            let n = |k: &str| u[k].as_f64().unwrap_or(0.0);
            let (i, w, r, o) = (
                n("input_tokens"),
                n("cache_creation_input_tokens"),
                n("cache_read_input_tokens"),
                n("output_tokens"),
            );
            let p = price(m["message"]["model"].as_str().unwrap_or(""));
            self.input += i;
            self.cache_write += w;
            self.cache_read += r;
            self.output += o;
            self.cost += (i * p[0] + w * p[1] + r * p[2] + o * p[3]) / 1e6;
            self.peak = self.peak.max(i + w + r);
            self.turns += 1;
        }
    }

    pub fn json(&self) -> Value {
        json!({
            "input": self.input, "cacheWrite": self.cache_write, "cacheRead": self.cache_read,
            "output": self.output, "costUsd": round(self.cost, 4), "peakContext": self.peak,
            "turns": self.turns,
        })
    }
}

/// What reeve's event log says the run's sessions did.
pub fn reeve_metrics(events: &str) -> Value {
    let evs: Vec<Value> = events
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    fn of<'a>(evs: &'a [Value], c: &'a str) -> impl Iterator<Item = &'a Value> + 'a {
        evs.iter().filter(move |e| e["command"] == c)
    }
    let of = |c: &'static str| of(&evs, c);
    let ok = |e: &&Value| e["exit"] == 0;
    let waits: Vec<f64> = of("heavy")
        .filter_map(|e| e["heavy_wait_ms"].as_f64())
        .collect();
    json!({
        "skillReads": of("skill").count(),
        "workersStarted": of("run").filter(ok).count(),
        "runFailures": of("run").filter(|e| e["exit"] != 0).count(),
        "heavyRuns": of("heavy").count(),
        "heavyWaitS": round(waits.iter().sum::<f64>() / 1e3, 1),
        "heavyMaxWaitS": round(waits.iter().copied().fold(0.0, f64::max) / 1e3, 1),
        "handoffs": of("handoff").filter(ok).count(),
        "handoffFailures": of("handoff").filter(|e| e["exit"] != 0).count(),
        "sends": of("send").count(),
        "watches": of("watch").count(),
        "reports": of("report").count(),
        "commands": evs.len(),
    })
}

/// What the shared test database saw: suite runs, how many went through `reeve heavy`, runs
/// turned away because it was busy, and whether a rebuild finished.
pub fn db_metrics(runs: &str) -> Value {
    let rs: Vec<Value> = runs
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let count = |f: &dyn Fn(&Value) -> bool| rs.iter().filter(|r| f(r)).count();
    json!({
        "suiteRuns": count(&|r| r["what"] == "suite" && r["busy"] == false),
        "suiteRunsHeavy": count(&|r| r["what"] == "suite" && r["busy"] == false && r["heavy"] == true),
        "busy": count(&|r| r["busy"] == true),
        "rebuilds": count(&|r| r["what"] == "rebuild" && r["ok"] == true),
        "rebuildsHeavy": count(&|r| r["what"] == "rebuild" && r["ok"] == true && r["heavy"] == true),
    })
}

/// The db.json a run's repo gets.
pub fn db_json(db: &Path, hold: u64, rebuild: u64) -> String {
    format!(
        "{{ \"path\": {}, \"holdSeconds\": {hold}, \"rebuildSeconds\": {rebuild} }}\n",
        json!(db.display().to_string())
    )
}

/// Done-check of `branch`: the suite on a snapshot with a private, instant database, then the
/// hidden checks. Every item fails if the suite does or if `test/db.json` was changed; the
/// fixture counts only if a rebuild really finished on the shared database.
pub fn check(root: &Path, repo: &Path, branch: &str, db: &Path, db_text: &str) -> io::Result<Value> {
    let dir = temp_dir("reeve-day-check-")?;
    let r = check_in(root, repo, branch, db, db_text, &dir);
    let _ = fs::remove_dir_all(&dir);
    r
}

fn check_in(
    root: &Path,
    repo: &Path,
    branch: &str,
    db: &Path,
    db_text: &str,
    dir: &Path,
) -> io::Result<Value> {
    let tar = dir.join("snap.tar");
    super::check::must(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["archive", "-o"])
            .arg(&tar)
            .arg(branch),
    )?;
    super::check::must(Command::new("tar").args(["-xf", "snap.tar"]).current_dir(dir))?;
    fs::remove_file(&tar)?;
    let db_file = dir.join("test/db.json");
    let tampered = fs::read_to_string(&db_file).unwrap_or_default().trim() != db_text.trim();
    fs::write(&db_file, db_json(&dir.join(".db"), 0, 0))?;
    let tests = passes(Command::new("node").arg("--test").current_dir(dir));
    let out = Command::new("node")
        .arg(root.join("day/hidden.mjs"))
        .arg(dir)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()?;
    let got: Value = String::from_utf8_lossy(&out.stdout)
        .lines()
        .rfind(|l| !l.trim().is_empty())
        .and_then(|l| serde_json::from_str(l).ok())
        .unwrap_or_default();
    let rebuilt = db_metrics(&fs::read_to_string(db.join("runs.jsonl")).unwrap_or_default())
        ["rebuilds"]
        .as_u64()
        .unwrap_or(0)
        > 0;
    let items: Map<String, Value> = ITEMS
        .iter()
        .map(|k| {
            let ok = tests && !tampered && got[k] == true && (*k != "fixtures" || rebuilt);
            (k.to_string(), ok.into())
        })
        .collect();
    let done = items.values().all(|v| *v == true);
    Ok(json!({ "tests": tests, "tampered": tampered, "rebuilt": rebuilt, "items": items, "done": done }))
}

pub(super) fn git(args: &[&OsStr]) -> io::Result<String> {
    let o = Command::new("git")
        .args(args)
        .stdin(Stdio::null())
        .output()?;
    if !o.status.success() {
        return Err(io::Error::other(format!(
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&o.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

macro_rules! git {
    ($($a:expr),* $(,)?) => { git(&[$(AsRef::<OsStr>::as_ref(&$a)),*]) };
}

pub(super) fn copy_dir(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for e in fs::read_dir(src)? {
        let e = e?;
        let (from, to) = (e.path(), dst.join(e.file_name()));
        if e.file_type()?.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// The run's sessions that Claude Code runs in the background.
fn bg_sessions(claude: &Path, env: &[(OsString, OsString)], run: &str, dir: &Path) -> Vec<Value> {
    let out = Command::new(claude)
        .args(["agents", "--json", "--all"])
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let all: Vec<Value> = match out {
        Ok(o) if o.status.success() => serde_json::from_slice(&o.stdout).unwrap_or_default(),
        _ => vec![],
    };
    let d = dir.display().to_string();
    all.into_iter()
        .filter(|a| {
            let cwd = a["cwd"].as_str().unwrap_or("");
            a["kind"] == "background" && (cwd.starts_with(&d) || mine(run, cwd))
        })
        .collect()
}

pub(super) fn stop_bg(claude: &Path, env: &[(OsString, OsString)], run: &str, dir: &Path) {
    for a in bg_sessions(claude, env, run, dir) {
        for cmd in ["stop", "rm"] {
            let _ = Command::new(claude)
                .arg(cmd)
                .arg(js_str(a.get("id")))
                .env_clear()
                .envs(env.iter().map(|(k, v)| (k, v)))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

pub(super) fn cleanup(base: &Path, run: &str, dir: &Path) -> io::Result<()> {
    if base.join(".git").exists() {
        let list = git!("-C", base, "worktree", "list", "--porcelain")?;
        for w in list.lines().filter_map(|l| l.strip_prefix("worktree ")) {
            if Path::new(w) == dir || Path::new(w).starts_with(dir) || mine(run, w) {
                let _ = git!("-C", base, "worktree", "remove", "--force", "--force", w);
            }
        }
        git!("-C", base, "worktree", "prune")?;
        let branches = git!("-C", base, "branch", "--format=%(refname:short)")?;
        for b in branches.lines().filter(|b| mine(run, b)) {
            let _ = git!("-C", base, "branch", "-D", b);
        }
    }
    match fs::remove_dir_all(dir) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// The run's transcripts: every `.jsonl` written since `since` in a project dir of the run.
pub(super) fn transcripts(home: &Path, run: &str, since: SystemTime) -> Vec<PathBuf> {
    let projects = home.join(".claude/projects");
    let mut out = vec![];
    let Ok(dirs) = fs::read_dir(&projects) else {
        return out;
    };
    let fresh = |p: &Path| {
        fs::metadata(p)
            .and_then(|m| m.modified())
            .is_ok_and(|t| t >= since)
    };
    for d in dirs.flatten() {
        if !mine(run, &d.file_name().to_string_lossy()) {
            continue;
        }
        let mut stack = vec![d.path()];
        while let Some(p) = stack.pop() {
            for e in fs::read_dir(&p).into_iter().flatten().flatten() {
                let path = e.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|x| x == "jsonl") && fresh(&path) {
                    out.push(path);
                }
            }
        }
    }
    out.sort();
    out
}

/// The name `reeve handoff` gave the successor, from reeve's state.
fn successor_name(state: &Path) -> Option<String> {
    fs::read_dir(state.join("reeve/handoffs"))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "from"))
        .map(|p| p.file_stem().unwrap_or_default().to_string_lossy().into_owned())
}

/// The line `reeve handoff` starts a successor on; see reeve's `handoff::successor_prompt`.
const TAKE_OVER: &str = "You take over from a previous session that handed off.";

/// The text of a transcript's first user prompt.
fn first_prompt(text: &str) -> Option<String> {
    let m = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|m| m["type"] == "user" && m["isMeta"] != true)?;
    let c = &m["message"]["content"];
    Some(match c.as_array() {
        Some(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        None => c.as_str()?.to_string(),
    })
}

/// The successor's transcript among `paths`: the first one created since the handoff whose
/// first prompt is the handoff's. The predecessor writes on after the handoff, names the
/// successor and says "handoff", so neither its mtime nor its text tells them apart.
pub fn successor_transcript(paths: &[PathBuf], since: SystemTime) -> Option<PathBuf> {
    let born = |p: &Path| fs::metadata(p).and_then(|m| m.created()).ok();
    paths
        .iter()
        .filter(|p| !p.to_string_lossy().contains("/subagents/"))
        .filter(|p| born(p).is_none_or(|t| t >= since))
        .filter(|p| {
            fs::read_to_string(p)
                .ok()
                .and_then(|t| first_prompt(&t))
                .is_some_and(|f| f.contains(TAKE_OVER))
        })
        .min_by_key(|p| born(p))
        .cloned()
}

/// Whether a shell command runs `reeve handoff`, not just mentions it.
fn runs_handoff(cmd: &str) -> bool {
    cmd.split(['\n', ';', '|', '&'])
        .any(|seg| seg.trim_start().starts_with("reeve handoff"))
}

/// The `reeve handoff` calls in a transcript that succeeded, each tool call once across `seen`.
/// Counted from the sessions' own transcripts, so every session's handoff counts, wherever its
/// reeve kept its state.
pub fn handoff_calls(text: &str, seen: &mut HashSet<String>) -> usize {
    let msgs: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let parts = |kind: &'static str| {
        msgs.iter()
            .flat_map(|m| m["message"]["content"].as_array().cloned().unwrap_or_default())
            .filter(move |c| c["type"] == kind)
    };
    let ok: HashSet<String> = parts("tool_result")
        .filter(|r| r["is_error"] != true)
        .map(|r| js_str(r.get("tool_use_id")))
        .collect();
    parts("tool_use")
        .filter(|c| c["name"] == "Bash" && runs_handoff(c["input"]["command"].as_str().unwrap_or("")))
        .map(|c| js_str(c.get("id")))
        .filter(|id| ok.contains(id) && seen.insert(id.clone()))
        .count()
}

/// At most `max` bytes from the end of `text`, starting on a char boundary and, past the start,
/// after the first line break, so the window holds whole lines.
pub fn tail_window(text: &str, max: usize) -> &str {
    let mut start = text.len().saturating_sub(max);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let tail = &text[start..];
    match tail.find('\n') {
        Some(i) if start > 0 => &tail[i + 1..],
        _ => tail,
    }
}

/// Check errors in a row: fewer than `k` skip a tick each, `k` in a row end the run.
pub struct Strikes {
    k: usize,
    row: usize,
    pub errors: usize,
}

impl Strikes {
    pub fn new(k: usize) -> Strikes {
        Strikes { k, row: 0, errors: 0 }
    }

    /// The check's result; `None` to skip this tick; `Err` once `k` failed in a row.
    pub fn note(&mut self, r: io::Result<Value>) -> Result<Option<Value>, String> {
        match r {
            Ok(v) => {
                self.row = 0;
                Ok(Some(v))
            }
            Err(e) => {
                self.row += 1;
                self.errors += 1;
                if self.row >= self.k {
                    Err(format!("{} checks in a row failed, the last: {e}", self.k))
                } else {
                    Ok(None)
                }
            }
        }
    }
}

/// Check errors in a row that end a run.
const STRIKES: usize = 3;

/// New assistant text in a transcript since `offset`.
fn tail_text(path: &Path, offset: &mut u64) -> Vec<String> {
    let Ok(mut f) = fs::File::open(path) else {
        return vec![];
    };
    if f.seek(SeekFrom::Start(*offset)).is_err() {
        return vec![];
    }
    let mut buf = String::new();
    let _ = f.read_to_string(&mut buf);
    let Some(end) = buf.rfind('\n') else {
        return vec![];
    };
    *offset += end as u64 + 1;
    buf[..end]
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|m| m["type"] == "assistant")
        .flat_map(|m| m["message"]["content"].as_array().cloned().unwrap_or_default())
        .filter(|c| c["type"] == "text")
        .filter_map(|c| c["text"].as_str().map(|t| t.trim().to_string()))
        .filter(|t| !t.is_empty())
        .collect()
}

fn mtime_ms(p: &Path) -> Option<i64> {
    let t = fs::metadata(p).ok()?.modified().ok()?;
    Some(t.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_millis() as i64)
}

struct Successor {
    name: String,
    transcript: Option<PathBuf>,
    offset: u64,
    since: SystemTime,
}

struct Run<'a> {
    o: &'a Opts,
    run: String,
    logs: PathBuf,
    env: Vec<(OsString, OsString)>,
    t0: i64,
}

impl Run<'_> {
    fn log(&self, event: &str, data: Value) {
        let mut m = Map::new();
        m.insert("t".into(), json!(clock() - self.t0));
        m.insert("event".into(), event.into());
        if let Value::Object(d) = data {
            m.extend(d);
        }
        let _ = append(
            &self.logs.join("events.jsonl"),
            format!("{}\n", Value::Object(m)).as_bytes(),
        );
    }
}

/// One run of an arm, named rd-<ARM>-<N>.
pub fn run(o: &Opts) -> io::Result<i32> {
    let leaks = crate::bench_block::ancestor_instructions(&o.trials);
    if !leaks.is_empty() {
        return Err(io::Error::other(format!(
            "{} would read {leaks:?}; set REEVE_BENCH_DIR to a dir with no CLAUDE.md above it",
            o.trials.display()
        )));
    }
    let arm_dir = o.root.join("day/arms").join(&o.arm);
    let arm_text = match &o.arm_json {
        Some(t) => t.clone(),
        None => fs::read_to_string(arm_dir.join("arm.json"))
            .map_err(|e| io::Error::other(format!("arm {}: {e}", o.arm)))?,
    };
    let arm: Arm = serde_json::from_str(&arm_text).map_err(io::Error::other)?;
    let user: User = serde_json::from_str(&fs::read_to_string(o.root.join("day/user.json"))?)
        .map_err(io::Error::other)?;
    let run = format!("rd-{}-{}", o.arm, o.n);
    let dir = o.trials.join(&run);
    let logs = o.trials.join("logs").join(&run);
    let (db, state, bin) = (logs.join("db"), logs.join("state"), logs.join("bin"));
    // The trials dir is the base repo and every run a worktree inside it. `claude --bg`, which
    // starts reeve's workers and successors, refuses a workspace that isn't trusted, and Claude
    // Code trusts a worktree by its main repo: trusting the trials dir once covers every run.
    let base = o.trials.clone();

    cleanup(&base, &run, &dir)?;
    let _ = fs::remove_dir_all(&logs);
    for d in [&logs, &db, &state, &bin, &o.trials.join(".orch-sink")] {
        fs::create_dir_all(d)?;
    }
    if !base.join(".git").exists() {
        fs::create_dir_all(&base)?;
        git!("-C", base, "init", "-q", "-b", "main")?;
        git!(
            "-C", base, "-c", "user.name=bench", "-c", "user.email=bench@localhost", "commit",
            "-q", "--allow-empty", "-m", "base"
        )?;
    }
    git!("-C", base, "worktree", "add", "-q", "-b", run, dir, "main")?;
    copy_dir(&o.root.join("day/repo"), &dir)?;
    let setup = arm.setup.as_ref().map(|s| o.root.join(s));
    if let Some(repo) = setup.as_ref().map(|s| s.join("repo")).filter(|r| r.exists()) {
        copy_dir(&repo, &dir)?;
    }
    let claude_md = dir.join("CLAUDE.md");
    fs::write(
        &claude_md,
        fs::read_to_string(&claude_md)?.replace("{{RUN}}", &run),
    )?;
    let db_text = db_json(&db, o.hold_s.unwrap_or(60), o.rebuild_s.unwrap_or(360));
    fs::write(dir.join("test/db.json"), &db_text)?;
    git!("-C", dir, "add", "-A")?;
    git!(
        "-C", dir, "-c", "user.name=bench", "-c", "user.email=bench@localhost", "commit", "-qm",
        "scenario"
    )?;

    let settings = logs.join("settings.json");
    let base_settings = match setup.as_ref().map(|s| s.join("settings.json")) {
        Some(s) if s.is_file() => fs::read_to_string(s)?,
        _ => r#"{ "permissions": { "defaultMode": "auto" } }"#.into(),
    };
    if let (Some(m), Some(reeve)) = (&arm.manual, &o.reeve) {
        write_exe(&bin.join("reeve"), &reeve_wrapper(reeve, &arm_dir.join(m)))?;
    }

    let mut env: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(k, _)| {
            let k = k.to_string_lossy();
            ![
                "HERDR_", "ORCH_", "CLAUDECODE", "CLAUDE_CODE_", "PATH", "REEVE_", "XDG_STATE_HOME",
                "HOME",
            ]
            .iter()
            .any(|p| k.starts_with(p))
        })
        .collect();
    let mut path = vec![bin.clone()];
    path.extend(o.reeve.as_ref().and_then(|r| r.parent().map(PathBuf::from)));
    path.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
    env.extend([
        ("HOME".into(), o.home.clone().into()),
        ("PATH".into(), std::env::join_paths(path).map_err(io::Error::other)?),
        ("XDG_STATE_HOME".into(), state.clone().into()),
        ("REEVE_HOME".into(), logs.join("reeve-home").into()),
        (
            "REEVE_HANDOFF_TOKENS".into(),
            arm.context_budget.unwrap_or(350_000).to_string().into(),
        ),
        ("ORCH_HOME".into(), o.trials.join(".orch-sink").into()),
    ]);
    env.extend(o.extra_env.iter().cloned());
    fs::write(&settings, run_settings(&base_settings, &env)?)?;
    write_exe(&bin.join("claude"), &claude_wrapper(&o.claude, &arm.flags(&settings)))?;
    let since = SystemTime::now();
    let r = Run {
        o,
        run: run.clone(),
        logs: logs.clone(),
        env,
        t0: clock(),
    };


    let asks = fs::read_to_string(o.root.join("day/asks.md"))?.trim().to_string();
    let key = fs::read_to_string(o.root.join("day/key.md"))?;
    let mut followups: Vec<Value> =
        serde_json::from_str(&fs::read_to_string(o.root.join("day/followups.json"))?)
            .map_err(io::Error::other)?;
    let mut history: Vec<(&'static str, String)> = vec![];
    let t0 = r.t0;
    let system = arm.system.as_ref().map(|s| arm_dir.join(s));
    let mut front = Front::start(&r, &bin, "front", system.as_deref(), &dir, &asks, &mut history)?;
    r.log("start", json!({ "arm": o.arm, "run": run, "params": arm_json(&arm) }));

    let mut result = json!({
        "arm": o.arm, "run": run, "params": arm_json(&arm), "interventions": [], "followups": [],
        "claimedAt": null, "falseDone": null, "neverClaimed": false, "doneAt": null, "itemsAt": {},
        "contextCheckAt": null, "handoffAt": null, "successor": null, "timeout": false,
        "simCostUsd": 0.0,
    });
    let t = &o.timing;
    let notes_mode = arm.handoff.as_deref() == Some("notes");
    let mut notes = logs.join("HANDOFF.md");
    // The vanilla handoffs so far, and whether the current session was told to hand off.
    let (mut notes_handoffs, mut notes_told) = (0usize, false);
    let mut strikes = Strikes::new(STRIKES);
    let mut used: Vec<String> = vec![];
    let (mut last_check, mut last_poll) = (i64::MIN / 2, clock());
    let mut bg_successor: Option<Successor> = None;
    let mut sim_cost = 0.0;
    let error: Option<String> = loop {
        thread::sleep(Duration::from_millis(t.tick_ms));
        let now = clock();
        if now - last_check >= t.check_ms {
            last_check = now;
            let c = match strikes.note(check(&o.root, &dir, &run, &db, &db_text)) {
                Ok(Some(c)) => c,
                Ok(None) => {
                    r.log("check-error", json!({ "errors": strikes.errors }));
                    continue;
                }
                Err(e) => break Some(e),
            };
            for k in ITEMS {
                if c["items"][k] == true && result["itemsAt"][k].is_null() {
                    result["itemsAt"][k] = json!(now - t0);
                }
            }
            if c["done"] == true && result["doneAt"].is_null() {
                result["doneAt"] = json!(now - t0);
                r.log("done-check", c.clone());
            }
        }
        while followups
            .first()
            .is_some_and(|f| (now - t0) as f64 >= f["afterS"].as_f64().unwrap_or(0.0) * 1e3 * t.scale)
        {
            let f = followups.remove(0);
            r.log("followup", f.clone());
            push(&mut result["followups"], json!({ "t": now - t0, "text": f["text"] }));
            send(&r, &mut front, &mut history, &bg_successor, f["text"].as_str().unwrap_or(""));
        }
        // The vanilla handoff: the harness tells the session what reeve's hook would, then
        // starts a fresh session on the notes once the old one has stopped.
        // Every session past the budget hands off, successors too, as with reeve's hook.
        if notes_mode {
            if !notes_told {
                let used_now = arm.context_budget.zip(front.context(&o.home, &run));
                if let Some((budget, used_now)) = used_now.filter(|(b, u)| u >= b) {
                    let text = notes_check(used_now, budget, &notes);
                    r.log("context-check", json!({ "used": used_now }));
                    if result["contextCheckAt"].is_null() {
                        result["contextCheckAt"] = json!(now - t0);
                    }
                    notes_told = true;
                    front.say(&mut history, &text);
                }
            } else if !front.busy() && fs::metadata(&notes).is_ok_and(|m| m.len() > 0) {
                notes_handoffs += 1;
                let role = match notes_handoffs {
                    1 => "next".to_string(),
                    n => format!("next-{n}"),
                };
                let first = format!(
                    "{TAKE_OVER} Read {} and continue the work from there.",
                    notes.display()
                );
                let mut next = Front::start(&r, &bin, &role, system.as_deref(), &dir, &first, &mut history)?;
                std::mem::swap(&mut front, &mut next);
                let leftover = std::mem::take(&mut next.state.lock().unwrap().unread);
                front.state.lock().unwrap().unread.extend(leftover);
                next.stop();
                r.log("handoff", json!({ "successor": role }));
                if result["handoffAt"].is_null() {
                    result["handoffAt"] = json!(now - t0);
                    result["successor"] = json!(role);
                }
                notes_told = false;
                notes = logs.join(format!("HANDOFF-{}.md", notes_handoffs + 1));
            }
        }
        if !notes_mode
            && bg_successor.is_none()
            && let Some(name) = successor_name(&state)
        {
            r.log("handoff", json!({ "successor": name }));
            result["handoffAt"] = json!(now - t0);
            result["successor"] = json!(name);
            bg_successor = Some(Successor {
                name,
                transcript: None,
                offset: 0,
                since: SystemTime::now(),
            });
        }
        let mut succ_last = None;
        if let Some(s) = bg_successor.as_mut() {
            if s.transcript.is_none() {
                // The successor runs where its predecessor did.
                s.transcript = successor_transcript(&transcripts(&o.home, &run, s.since), s.since);
            }
            if let Some(tp) = s.transcript.clone() {
                let text = tail_text(&tp, &mut s.offset);
                front.state.lock().unwrap().unread.extend(text);
                succ_last = mtime_ms(&tp);
            }
        }
        if now - t0 > t.total_ms {
            result["timeout"] = true.into();
            break None;
        }
        let exited = matches!(front.child.try_wait(), Ok(Some(_)));
        if exited {
            front.stdin = None;
            if bg_successor.is_none() {
                r.log("front-exit", json!({}));
                break None;
            }
        }
        let (busy, last_event) = {
            let c = front.state.lock().unwrap();
            let busy = c.busy && !exited && bg_successor.is_none();
            (busy, c.last_event.max(succ_last.unwrap_or(0)))
        };
        let succ_busy = succ_last.is_some_and(|l| now - l < t.reply_ms);
        if busy || succ_busy || now - last_event < t.reply_ms {
            continue;
        }
        let done_abs = result["doneAt"].as_i64().map(|d| t0 + d);
        if never_claimed(done_abs, last_event, now, t.grace_ms) {
            result["neverClaimed"] = true.into();
            break None;
        }
        let said = std::mem::take(&mut front.state.lock().unwrap().unread).join("\n\n");
        let mut reply: Option<(String, String)> = None;
        if !said.is_empty() {
            history.push(("communicator", said.clone()));
            if claimed(&said) {
                let c = match strikes.note(check(&o.root, &dir, &run, &db, &db_text)) {
                    Ok(Some(c)) => c,
                    Ok(None) => {
                        // Try the claim again next tick.
                        r.log("check-error", json!({ "errors": strikes.errors }));
                        history.pop();
                        front.state.lock().unwrap().unread.insert(0, said);
                        continue;
                    }
                    Err(e) => break Some(e),
                };
                result["claimedAt"] = json!(now - t0);
                result["falseDone"] = (c["done"] != true).into();
                if c["done"] == true && result["doneAt"].is_null() {
                    result["doneAt"] = json!(now - t0);
                }
                result["final"] = c;
                break None;
            }
            if let Some(rule) = scripted(&user, &said, &used) {
                used.push(rule.id.clone());
                reply = Some((rule.kind.clone(), rule.reply.clone()));
            } else if !questions(&said).is_empty() {
                let sim = Sim {
                    asks: &asks,
                    key: &key,
                    items: &ITEMS,
                    history: &history,
                    silent_minutes: 0,
                    env: &r.env,
                    cwd: &o.trials,
                    bin: o.claude.as_os_str(),
                    model: &o.sim_model,
                };
                match simulate(&sim) {
                    Ok(s) => {
                        sim_cost += s["costUsd"].as_f64().unwrap_or(0.0);
                        r.log("sim", s.clone());
                        if let Some(text) = s["reply"].as_str().filter(|x| !x.is_empty()) {
                            reply = Some((js_str(s.get("kind")), text.to_string()));
                        }
                    }
                    Err(e) => r.log("sim-error", json!({ "error": e.to_string() })),
                }
            }
        } else if let Some(p) = user.poll.as_ref().filter(|_| result["doneAt"].is_null()) {
            let after = (p.after_s * 1e3 * t.scale) as i64;
            if now - last_event >= after && now - last_poll >= after {
                reply = Some((p.kind.clone(), p.reply.clone()));
            }
        }
        if let Some((kind, text)) = reply {
            last_poll = now;
            push(
                &mut result["interventions"],
                json!({ "t": now - t0, "kind": kind, "text": text }),
            );
            r.log("reply", json!({ "kind": kind, "text": text }));
            send(&r, &mut front, &mut history, &bg_successor, &text);
            if result["interventions"].as_array().map_or(0, Vec::len) >= MAX_INTERVENTIONS {
                break None;
            }
        }
    };
    // From here on an error is recorded, not returned: the sessions are stopped, the result is
    // written and the run is cleaned up whatever happens.
    let mut errors: Vec<String> = error.into_iter().collect();
    if result["final"].is_null() {
        let mut s = Strikes::new(STRIKES);
        result["final"] = loop {
            match s.note(check(&o.root, &dir, &run, &db, &db_text)) {
                Ok(Some(c)) => break c,
                Ok(None) => thread::sleep(Duration::from_millis(t.tick_ms)),
                Err(e) => {
                    errors.push(format!("final check: {e}"));
                    break json!({ "done": false, "error": e });
                }
            }
        };
        strikes.errors += s.errors;
    }
    result["checkErrors"] = json!(strikes.errors);
    result["endedAt"] = json!(clock() - t0);
    front.stop();
    stop_bg(&o.claude, &r.env, &run, &dir);
    if let Err(e) = finish(&r, &mut result, since, sim_cost, &db, &state, notes_handoffs) {
        errors.push(format!("finish: {e}"));
    }
    let bundle = logs.join("repo.bundle");
    let _ = git!("-C", dir, "bundle", "create", bundle, run);
    if let Ok(g) = git!("-C", dir, "log", "--oneline", "--graph", "--all", "-n", "60") {
        let _ = fs::write(logs.join("git-log.txt"), g);
    }
    if !errors.is_empty() {
        result["error"] = errors.join("; ").into();
        // The harness failed, not the agent: such a run says nothing about the arm.
        result["invalid"] = true.into();
    }
    let written = fs::write(logs.join("result.json"), serde_json::to_string_pretty(&result)?);
    println!("{}", summary(&result));
    cleanup(&base, &run, &dir)?;
    written?;
    Ok(if errors.is_empty() { 0 } else { 1 })
}

/// What the harness tells a session without reeve once it passes the budget: reeve's own
/// notice, with the notes file instead of `reeve handoff`.
pub fn notes_check(used: u64, budget: u64, notes: &Path) -> String {
    let k = |n: u64| format!("{}K", (n + 500) / 1000);
    format!(
        "Context check: this session has used {} tokens, past the {} handoff threshold. Finish \
         the step in hand, write your handoff notes (goal, what's done, what's next, open \
         questions) to {}, and stop; a new session picks them up. Don't take on new work.",
        k(used),
        k(budget),
        notes.display()
    )
}

/// A `claude -p` session the user types into.
struct Front {
    child: Child,
    stdin: Option<ChildStdin>,
    state: Arc<Mutex<Comm>>,
}

impl Front {
    fn start(
        r: &Run,
        bin: &Path,
        role: &str,
        system: Option<&Path>,
        dir: &Path,
        first: &str,
        history: &mut Vec<(&'static str, String)>,
    ) -> io::Result<Front> {
        let name = format!("{}-{role}", r.run);
        let mut cmd = Command::new(bin.join("claude"));
        cmd.args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "-n",
            &name,
        ]);
        if let Some(s) = system {
            cmd.arg("--append-system-prompt-file").arg(s);
        }
        let mut child = cmd
            .env_clear()
            .envs(r.env.iter().map(|(k, v)| (k, v)))
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let state = Arc::new(Mutex::new(Comm {
            busy: true,
            last_event: clock(),
            ..Default::default()
        }));
        let (out, err) = (
            child.stdout.take().expect("piped"),
            child.stderr.take().expect("piped"),
        );
        let (s, p) = (state.clone(), r.logs.join(format!("{role}.jsonl")));
        thread::spawn(move || read_stdout(out, p, s));
        let p = r.logs.join(format!("{role}.stderr"));
        thread::spawn(move || read_stderr(err, p));
        let mut f = Front {
            stdin: child.stdin.take(),
            child,
            state,
        };
        f.say(history, first);
        Ok(f)
    }

    fn say(&mut self, history: &mut Vec<(&'static str, String)>, text: &str) {
        if let Some(si) = self.stdin.as_mut() {
            say(si, &self.state, history, text);
        }
    }

    fn busy(&self) -> bool {
        self.state.lock().unwrap().busy
    }

    /// The context its last turn read, from its transcript.
    fn context(&self, home: &Path, run: &str) -> Option<u64> {
        let sid = self.state.lock().unwrap().session.clone()?;
        let name = format!("{}.jsonl", sid.as_str()?);
        let file = transcripts(home, run, SystemTime::UNIX_EPOCH)
            .into_iter()
            .find(|p| p.file_name().is_some_and(|f| f == name.as_str()))?;
        let text = fs::read_to_string(file).ok()?;
        reeve::context::tokens(tail_window(&text, 256 * 1024), 0)
    }

    fn stop(&mut self) {
        self.stdin = None;
        stop(&mut self.child);
    }
}

/// Types into the front session; once it has exited after a reeve handoff, sends to the
/// successor's inbox.
fn send(
    r: &Run,
    front: &mut Front,
    history: &mut Vec<(&'static str, String)>,
    successor: &Option<Successor>,
    text: &str,
) {
    if front.stdin.is_some() {
        front.say(history, text);
    } else if let Some(s) = successor {
        history.push(("user", text.to_string()));
        let _ = Command::new("reeve")
            .args(["send", &s.name, text])
            .env_clear()
            .envs(r.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn stop(child: &mut Child) {
    term(child);
    let _ = child.wait();
}

fn arm_json(arm: &Arm) -> Value {
    json!({
        "model": arm.model, "effort": arm.effort, "settingSources": arm.setting_sources,
        "autocompact": arm.autocompact, "contextBudget": arm.context_budget.unwrap_or(350_000),
        "handoff": arm.handoff.clone().unwrap_or_else(|| "reeve".into()),
        "manual": arm.manual.clone().unwrap_or_else(|| "built-in".into()), "setup": arm.setup,
        "system": arm.system,
    })
}

/// Adds tokens, cost, sessions and reeve's and the database's records to `result`.
fn finish(
    r: &Run,
    result: &mut Value,
    since: SystemTime,
    sim_cost: f64,
    db: &Path,
    state: &Path,
    notes_handoffs: usize,
) -> io::Result<()> {
    let files = transcripts(&r.o.home, &r.run, since);
    let mut total = Usage::default();
    let mut seen = HashSet::new();
    let (mut calls, mut handoffs) = (HashSet::new(), notes_handoffs);
    let mut sessions = vec![];
    let mut api_errors = 0;
    for f in &files {
        let text = fs::read_to_string(f).unwrap_or_default();
        api_errors += network_errors(&text);
        handoffs += handoff_calls(&text, &mut calls);
        let mut u = Usage::default();
        u.add(&text, &mut seen);
        total.cost += u.cost;
        total.input += u.input;
        total.cache_write += u.cache_write;
        total.cache_read += u.cache_read;
        total.output += u.output;
        total.turns += u.turns;
        total.peak = total.peak.max(u.peak);
        if !f.to_string_lossy().contains("/subagents/") {
            let mut s = u.json();
            s["file"] = json!(f.file_name().map(|n| n.to_string_lossy().into_owned()));
            sessions.push(s);
        }
    }
    for e in fs::read_dir(&r.logs)?.flatten() {
        let f = e.file_name().to_string_lossy().into_owned();
        let session = (f.starts_with("front") || f.starts_with("next"))
            && (f.ends_with(".jsonl") || f.ends_with(".stderr"));
        if session || f == "events.jsonl" {
            api_errors += network_errors(&fs::read_to_string(e.path()).unwrap_or_default());
        }
    }
    let events = fs::read_to_string(state.join("reeve/events.jsonl")).unwrap_or_default();
    let dbm = db_metrics(&fs::read_to_string(db.join("runs.jsonl")).unwrap_or_default());
    result["usage"] = total.json();
    result["costUsd"] = round(total.cost, 3);
    result["simCostUsd"] = round(sim_cost, 4);
    result["sessions"] = json!(sessions);
    result["sessionCount"] = json!(sessions.len());
    result["reeve"] = reeve_metrics(&events);
    result["handoffs"] = json!(handoffs);
    result["db"] = dbm;
    result["apiErrors"] = json!(api_errors);
    result["invalid"] = json!(api_errors > 0);
    Ok(())
}

/// Signs that a run met an API or network error: in a transcript, a session's stderr, or the
/// driver's log (a failed call to the user model).
pub fn network_errors(text: &str) -> usize {
    [
        "API Error",
        "\"api_error_status\":4",
        "\"api_error_status\":5",
        "Connection error",
        "ECONNRESET",
        "ECONNREFUSED",
        "ENOTFOUND",
        "ETIMEDOUT",
        "fetch failed",
        "socket hang up",
        "\"event\":\"sim-error\"",
    ]
    .iter()
    .map(|p| text.matches(p).count())
    .sum()
}

fn push(arr: &mut Value, v: Value) {
    if let Some(a) = arr.as_array_mut() {
        a.push(v);
    }
}

fn summary(r: &Value) -> Value {
    let min = |v: &Value| v.as_f64().map(|t| round(t / 60e3, 1));
    json!({
        "run": r["run"], "done": r["final"]["done"], "items": items_done(r),
        "doneMin": min(&r["doneAt"]), "interventions": r["interventions"].as_array().map_or(0, Vec::len),
        "falseDone": r["falseDone"], "neverClaimed": r["neverClaimed"], "cost": r["costUsd"],
        "workers": r["reeve"]["workersStarted"], "heavy": r["reeve"]["heavyRuns"],
        "handoffs": handoffs(r), "busy": r["db"]["busy"], "invalid": r["invalid"],
    })
}

/// Handoffs across all of a run's sessions. Results from before they were counted so have only
/// reeve's own record, which holds one reeve state's.
fn handoffs(r: &Value) -> &Value {
    if r["handoffs"].is_null() {
        &r["reeve"]["handoffs"]
    } else {
        &r["handoffs"]
    }
}

fn items_done(r: &Value) -> usize {
    r["final"]["items"]
        .as_object()
        .map_or(0, |m| m.values().filter(|v| **v == true).count())
}

/// Per arm over its valid runs: median [min–max] of each measure.
pub fn score(trials: &Path, only: &[String], with_invalid: bool) -> io::Result<String> {
    let logs = trials.join("logs");
    let mut runs: Vec<Value> = fs::read_dir(&logs)?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("rd-"))
        .filter_map(|e| serde_json::from_str(&fs::read_to_string(e.path().join("result.json")).ok()?).ok())
        .filter(|r: &Value| with_invalid || r["invalid"] != true)
        .collect();
    runs.sort_by_key(|r| js_str(r.get("run")));
    let mut arms: Vec<String> = runs.iter().map(|r| js_str(r.get("arm"))).collect();
    arms.sort();
    arms.dedup();
    let header = [
        "arm", "n", "done", "items", "false done", "never claimed", "min to done", "interv.",
        "cost $", "sim $", "cache read M", "output k", "peak ctx k", "sessions", "workers",
        "heavy", "heavy wait s", "db busy", "handoffs",
    ];
    let mut rows: Vec<Vec<String>> = vec![header.map(String::from).to_vec()];
    for arm in arms.iter().filter(|a| only.is_empty() || only.contains(a)) {
        let rs: Vec<&Value> = runs.iter().filter(|r| r["arm"] == arm.as_str()).collect();
        let col = |f: &dyn Fn(&Value) -> Option<f64>, d| fmt(&rs.iter().map(|r| f(r)).collect::<Vec<_>>(), d);
        let count = |f: &dyn Fn(&Value) -> bool| rs.iter().filter(|r| f(r)).count();
        rows.push(vec![
            arm.clone(),
            rs.len().to_string(),
            format!("{}/{}", count(&|r| r["final"]["done"] == true), rs.len()),
            col(&|r| Some(items_done(r) as f64), 0),
            count(&|r| r["falseDone"] == true).to_string(),
            count(&|r| r["neverClaimed"] == true).to_string(),
            col(&|r| r["doneAt"].as_f64().map(|t| t / 60e3), 1),
            col(&|r| r["interventions"].as_array().map(|a| a.len() as f64), 0),
            col(&|r| r["costUsd"].as_f64(), 2),
            col(&|r| r["simCostUsd"].as_f64(), 3),
            col(&|r| r["usage"]["cacheRead"].as_f64().map(|x| x / 1e6), 1),
            col(&|r| r["usage"]["output"].as_f64().map(|x| x / 1e3), 0),
            col(&|r| r["usage"]["peakContext"].as_f64().map(|x| x / 1e3), 0),
            col(&|r| r["sessionCount"].as_f64(), 0),
            col(&|r| r["reeve"]["workersStarted"].as_f64(), 0),
            col(&|r| r["reeve"]["heavyRuns"].as_f64(), 0),
            col(&|r| r["reeve"]["heavyWaitS"].as_f64(), 0),
            col(&|r| r["db"]["busy"].as_f64(), 0),
            col(&|r| handoffs(r).as_f64(), 0),
        ]);
    }
    let widths: Vec<usize> = (0..header.len())
        .map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0))
        .collect();
    Ok(rows
        .iter()
        .map(|r| {
            r.iter()
                .zip(&widths)
                .map(|(c, w)| format!("{c:w$}"))
                .collect::<Vec<_>>()
                .join("  ")
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

#[cfg(test)]
mod tests;
