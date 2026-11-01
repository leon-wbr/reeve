//! `reeve-bench pressure`: parallel workers on a machine short of memory. `workers` sessions,
//! started with `reeve run`, each run `jobs` heavy jobs through `reeve heavy`; a job holds
//! `jobMb` of RAM for `jobSeconds`. While the jobs hold more than `budgetMb`, a stand-in for
//! Claude Code's low-memory reaper kills the background task of the newest `reeve heavy` caller,
//! as the real one did to queued and running callers. The budget is the bench's, far below the
//! machine's, so the real machine stays clear of trouble. See bench/pressure/README.md.

use super::day::{
    Usage, claude_wrapper, cleanup, copy_dir, git, run_settings, stop_bg, transcripts, write_exe,
};
use super::run::{append, clock};
use super::score::fmt;
use super::{js_str, round};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

macro_rules! git {
    ($($a:expr),* $(,)?) => { git(&[$(AsRef::<OsStr>::as_ref(&$a)),*]) };
}

/// An arm: `bench/pressure/arms/<ARM>/arm.json`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Arm {
    pub model: String,
    pub effort: Option<String>,
    pub setting_sources: Option<String>,
    /// A directory, relative to `bench/`, with the arm's `settings.json` and a `repo/` overlay.
    pub setup: Option<String>,
    pub workers: usize,
    pub jobs: usize,
    pub job_mb: u64,
    pub job_seconds: u64,
    /// The memory the machine has for the run's jobs, in MB: past it, the reaper kills.
    pub budget_mb: u64,
    /// The reeve to run, relative to the repo root; by default the one beside reeve-bench.
    pub reeve: Option<String>,
    /// Added to every session's environment: reeve's knobs, such as `REEVE_HEAVY_SLOTS`.
    pub env: Map<String, Value>,
    pub note: Option<String>,
}

impl Default for Arm {
    fn default() -> Arm {
        Arm {
            model: "sonnet".into(),
            effort: None,
            setting_sources: None,
            setup: None,
            workers: 6,
            jobs: 2,
            job_mb: 600,
            job_seconds: 60,
            budget_mb: 1500,
            reeve: None,
            env: Map::new(),
            note: None,
        }
    }
}

impl Arm {
    fn flags(&self, settings: &Path) -> Vec<String> {
        let mut f = vec!["--model".to_string(), self.model.clone()];
        for (flag, v) in [("--effort", &self.effort), ("--setting-sources", &self.setting_sources)] {
            if let Some(v) = v {
                f.extend([flag.to_string(), v.clone()]);
            }
        }
        f.extend(["--settings".into(), settings.display().to_string()]);
        f
    }
}

pub struct Opts {
    pub arm: String,
    pub n: String,
    pub root: PathBuf,
    pub trials: PathBuf,
    pub claude: PathBuf,
    pub reeve: PathBuf,
    pub home: PathBuf,
    pub tick_ms: u64,
    /// The run ends once no job has run or finished for this long.
    pub quiet_ms: i64,
    pub total_ms: i64,
    pub sample_ms: u64,
    pub extra_env: Vec<(OsString, OsString)>,
    pub arm_json: Option<String>,
}

/// What became of each job, from the ledger the fixture writes.
#[derive(Debug, Default, PartialEq)]
pub struct Jobs {
    pub completed: usize,
    pub started: usize,
    pub live: usize,
    /// When the last event happened, and when the last job finished.
    pub last_t: Option<i64>,
    pub last_done: Option<i64>,
}

/// The ledger's events against the jobs `ids`: a job is completed once one of its runs finished.
pub fn jobs(ledger: &str, ids: &[String]) -> Jobs {
    let mut j = Jobs::default();
    let mut done = HashSet::new();
    let mut running: HashSet<i64> = HashSet::new();
    for e in ledger.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        let pid = e["pid"].as_i64().unwrap_or(0);
        j.last_t = e["t"].as_i64().max(j.last_t);
        match e["event"].as_str() {
            Some("start") => {
                j.started += 1;
                running.insert(pid);
            }
            Some("done") => {
                running.remove(&pid);
                if ids.contains(&js_str(e.get("id"))) {
                    done.insert(js_str(e.get("id")));
                }
                j.last_done = e["t"].as_i64().max(j.last_done);
            }
            _ => {}
        }
    }
    j.completed = done.len();
    j.live = running.len();
    j
}

/// A process, from `ps -axo pid=,ppid=,rss=,args=`.
#[derive(Debug, Clone, PartialEq)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub rss_kb: u64,
    pub args: String,
}

impl Proc {
    /// The program's file name.
    fn name(&self) -> &str {
        let first = self.args.split_whitespace().next().unwrap_or("");
        first.rsplit('/').next().unwrap_or(first)
    }

    fn is_job(&self) -> bool {
        self.name() == "node" && self.args.contains("job.mjs")
    }

    fn is_heavy_caller(&self) -> bool {
        self.name() == "reeve" && self.args.split_whitespace().nth(1) == Some("heavy")
    }
}

pub fn parse_ps(ps: &str) -> Vec<Proc> {
    ps.lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            Some(Proc {
                pid: w.next()?.parse().ok()?,
                ppid: w.next()?.parse().ok()?,
                rss_kb: w.next()?.parse().ok()?,
                args: w.collect::<Vec<_>>().join(" "),
            })
        })
        .collect()
}

/// The run's processes: those whose working directory is in the run (`lsof -d cwd -Fpn`), and
/// their descendants, leaving out `skip` (the driver) and its children.
pub fn in_run(procs: &[Proc], lsof: &str, dir: &Path, skip: u32) -> Vec<Proc> {
    let mut roots = HashSet::new();
    let mut pid = 0;
    for l in lsof.lines() {
        if let Some(p) = l.strip_prefix('p') {
            pid = p.parse().unwrap_or(0);
        } else if let Some(n) = l.strip_prefix('n')
            && Path::new(n).starts_with(dir)
        {
            roots.insert(pid);
        }
    }
    let parent: HashMap<u32, u32> = procs.iter().map(|p| (p.pid, p.ppid)).collect();
    let inside = |mut p: u32| {
        for _ in 0..64 {
            if p == skip || p <= 1 {
                return false;
            }
            if roots.contains(&p) {
                return true;
            }
            match parent.get(&p) {
                Some(&q) => p = q,
                None => return false,
            }
        }
        false
    };
    procs.iter().filter(|p| inside(p.pid)).cloned().collect()
}

/// One sample of the run's processes.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Sample {
    pub rss_kb: u64,
    /// What the heavy jobs hold.
    pub jobs_rss_kb: u64,
    pub sessions: usize,
    pub jobs: usize,
    pub procs: usize,
}

pub fn sample(run: &[Proc]) -> Sample {
    let mut s = Sample::default();
    for p in run {
        s.rss_kb += p.rss_kb;
        s.procs += 1;
        s.sessions += usize::from(p.name() == "claude");
        if p.is_job() {
            s.jobs += 1;
            s.jobs_rss_kb += p.rss_kb;
        }
    }
    s
}

/// What Claude Code's low-memory reaper would kill: the background task of the newest
/// `reeve heavy` caller, that is the shell the caller runs in and everything under it. A job the
/// caller runs as its child dies with it; one that runs elsewhere doesn't.
pub fn reap_target(all: &[Proc], run: &[Proc]) -> Vec<u32> {
    let Some(caller) = run.iter().filter(|p| p.is_heavy_caller()).max_by_key(|p| p.pid) else {
        return vec![];
    };
    let shell = all
        .iter()
        .find(|p| p.pid == caller.ppid && matches!(p.name(), "bash" | "zsh" | "sh" | "-zsh" | "-bash"))
        .map_or(caller.pid, |p| p.pid);
    let mut tree = vec![shell];
    let mut i = 0;
    while i < tree.len() {
        let p = tree[i];
        tree.extend(all.iter().filter(|c| c.ppid == p).map(|c| c.pid));
        i += 1;
    }
    tree
}

/// Jobs that hold memory now: the fixture's live files whose process is alive. A job killed from
/// outside never writes its end, so the ledger alone can't tell.
fn live_jobs(live: &Path) -> usize {
    fs::read_dir(live)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.path().file_stem()?.to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        })
        .count()
}

fn output(cmd: &mut Command) -> String {
    cmd.stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

struct Sampler {
    dir: PathBuf,
    every: Duration,
    t0: i64,
    logs: PathBuf,
    /// The jobs' memory past which the reaper kills, in KB; 0 for none.
    budget_kb: u64,
    stop: Arc<AtomicBool>,
    peak: Arc<Mutex<(i64, Sample)>>,
    kills: Arc<Mutex<usize>>,
}

/// Samples the run's processes every `every` until `stop`, keeping the peak and every sample,
/// and plays the reaper: while the jobs hold more than the budget, it kills the newest heavy
/// caller's task, then gives the memory a few seconds to come back.
fn sampler(s: Sampler) {
    let me = std::process::id();
    let mut last_kill = i64::MIN / 2;
    // lsof reports real paths: /private/var, not /var.
    let dir = fs::canonicalize(&s.dir).unwrap_or_else(|_| s.dir.clone());
    while !s.stop.load(Ordering::Relaxed) {
        let all = parse_ps(&output(Command::new("ps").args(["-axo", "pid=,ppid=,rss=,args="])));
        let lsof = output(Command::new("lsof").args(["-n", "-P", "-d", "cwd", "-Fpn"]));
        let run = in_run(&all, &lsof, &dir, me);
        let x = sample(&run);
        let t = clock() - s.t0;
        let _ = append(
            &s.logs.join("samples.tsv"),
            format!("{t}\t{}\t{}\t{}\t{}\t{}\n", x.rss_kb, x.jobs_rss_kb, x.sessions, x.jobs, x.procs).as_bytes(),
        );
        if s.budget_kb > 0 && x.jobs_rss_kb > s.budget_kb && t - last_kill >= 5_000 {
            let tree = reap_target(&all, &run);
            if !tree.is_empty() {
                let pids: Vec<String> = tree.iter().map(u32::to_string).collect();
                let _ = Command::new("kill").arg("-9").args(&pids).stderr(Stdio::null()).status();
                last_kill = t;
                *s.kills.lock().unwrap() += 1;
                let line = json!({ "t": t, "event": "reap", "jobsRssKb": x.jobs_rss_kb, "pids": tree });
                let _ = append(&s.logs.join("events.jsonl"), format!("{line}\n").as_bytes());
            }
        }
        let mut p = s.peak.lock().unwrap();
        if x.rss_kb > p.1.rss_kb {
            *p = (t, x);
        }
        drop(p);
        thread::sleep(s.every);
    }
}

/// What each worker is asked: its jobs, one after another, through `reeve heavy`.
pub fn prompt(ids: &[String]) -> String {
    let cmds: Vec<String> = ids.iter().map(|i| format!("- `node scripts/job.mjs --id {i}`")).collect();
    format!(
        "Run these heavy jobs, one after another, each through `reeve heavy`:\n{}\nWait for each to finish before the next. Don't rerun a job that fails. When all have run, say for each job whether it finished or failed, with its exit code, and stop.",
        cmds.join("\n")
    )
}

/// One run of an arm, named rp-<ARM>-<N>.
pub fn run(o: &Opts) -> io::Result<i32> {
    let leaks = crate::bench_block::ancestor_instructions(&o.trials);
    if !leaks.is_empty() {
        return Err(io::Error::other(format!(
            "{} would read {leaks:?}; set REEVE_BENCH_DIR to a dir with no CLAUDE.md above it",
            o.trials.display()
        )));
    }
    let arm_text = match &o.arm_json {
        Some(t) => t.clone(),
        None => fs::read_to_string(o.root.join("pressure/arms").join(&o.arm).join("arm.json"))
            .map_err(|e| io::Error::other(format!("arm {}: {e}", o.arm)))?,
    };
    let arm: Arm = serde_json::from_str(&arm_text).map_err(io::Error::other)?;
    let run = format!("rp-{}-{}", o.arm, o.n);
    let dir = o.trials.join(&run);
    let logs = o.trials.join("logs").join(&run);
    let (ledger, state, bin) = (logs.join("ledger"), logs.join("state"), logs.join("bin"));
    let base = o.trials.clone();

    cleanup(&base, &run, &dir)?;
    let _ = fs::remove_dir_all(&logs);
    for d in [&logs, &ledger, &state, &bin, &o.trials.join(".orch-sink")] {
        fs::create_dir_all(d)?;
    }
    if !base.join(".git").exists() {
        git!("-C", base, "init", "-q", "-b", "main")?;
        git!(
            "-C", base, "-c", "user.name=bench", "-c", "user.email=bench@localhost", "commit",
            "-q", "--allow-empty", "-m", "base"
        )?;
    }
    git!("-C", base, "worktree", "add", "-q", "-b", run, dir, "main")?;
    copy_dir(&o.root.join("pressure/repo"), &dir)?;
    let setup = arm.setup.as_ref().map(|s| o.root.join(s));
    if let Some(repo) = setup.as_ref().map(|s| s.join("repo")).filter(|r| r.exists()) {
        copy_dir(&repo, &dir)?;
    }
    let claude_md = dir.join("CLAUDE.md");
    fs::write(&claude_md, fs::read_to_string(&claude_md)?.replace("{{RUN}}", &run))?;
    let cfg = json!({
        "ledger": ledger.display().to_string(), "mb": arm.job_mb, "seconds": arm.job_seconds,
    });
    fs::write(dir.join("job.json"), format!("{cfg:#}\n"))?;
    git!("-C", dir, "add", "-A")?;
    git!(
        "-C", dir, "-c", "user.name=bench", "-c", "user.email=bench@localhost", "commit", "-qm",
        "scenario"
    )?;

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
    // The arm's reeve, a branch's build say, or the one beside this binary.
    let reeve = match &arm.reeve {
        Some(r) => o.root.join("..").join(r),
        None => o.reeve.clone(),
    };
    if !reeve.is_file() {
        return Err(io::Error::other(format!("no reeve at {}", reeve.display())));
    }
    path.extend(reeve.parent().map(PathBuf::from));
    path.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
    env.extend([
        ("HOME".into(), o.home.clone().into()),
        ("PATH".into(), std::env::join_paths(path).map_err(io::Error::other)?),
        ("XDG_STATE_HOME".into(), state.clone().into()),
        ("REEVE_HOME".into(), logs.join("reeve-home").into()),
        ("ORCH_HOME".into(), o.trials.join(".orch-sink").into()),
    ]);
    for (k, v) in &arm.env {
        let v = v.as_str().map_or_else(|| v.to_string(), String::from);
        env.push((k.into(), v.into()));
    }
    env.extend(o.extra_env.iter().cloned());
    let settings = logs.join("settings.json");
    let base_settings = match setup.as_ref().map(|s| s.join("settings.json")) {
        Some(s) if s.is_file() => fs::read_to_string(s)?,
        _ => r#"{ "permissions": { "defaultMode": "auto" } }"#.into(),
    };
    // The arm's own variables reach background sessions through the settings too.
    let mut s: Value = serde_json::from_str(&run_settings(&base_settings, &env)?)?;
    for (k, v) in &arm.env {
        s["env"][k] = json!(v.as_str().map_or_else(|| v.to_string(), String::from));
    }
    fs::write(&settings, format!("{s:#}\n"))?;
    write_exe(&bin.join("claude"), &claude_wrapper(&o.claude, &arm.flags(&settings)))?;

    let since = SystemTime::now();
    let t0 = clock();
    let events = logs.join("events.jsonl");
    let log = |event: &str, data: Value| {
        let mut m = Map::new();
        m.insert("t".into(), json!(clock() - t0));
        m.insert("event".into(), event.into());
        if let Value::Object(d) = data {
            m.extend(d);
        }
        let _ = append(&events, format!("{}\n", Value::Object(m)).as_bytes());
    };
    let stop = Arc::new(AtomicBool::new(false));
    let peak = Arc::new(Mutex::new((0i64, Sample::default())));
    let kills = Arc::new(Mutex::new(0usize));
    let sampling = {
        let s = Sampler {
            dir: dir.clone(),
            every: Duration::from_millis(o.sample_ms),
            t0,
            logs: logs.clone(),
            budget_kb: arm.budget_mb * 1024,
            stop: stop.clone(),
            peak: peak.clone(),
            kills: kills.clone(),
        };
        thread::spawn(move || sampler(s))
    };
    log("start", json!({ "arm": o.arm, "run": run, "params": arm_json(&arm) }));

    let mut ids = vec![];
    let mut run_failures = 0;
    for w in 1..=arm.workers {
        let mine: Vec<String> = (1..=arm.jobs).map(|j| format!("w{w}-{j}")).collect();
        let name = format!("{run}-w{w}");
        let out = Command::new("reeve")
            .args(["run", "--name", &name, "--cwd"])
            .arg(&dir)
            .args(["--", &prompt(&mine)])
            .env_clear()
            .envs(env.iter().map(|(k, v)| (k, v)))
            .current_dir(&dir)
            .stdin(Stdio::null())
            .output()?;
        let ok = out.status.success();
        run_failures += usize::from(!ok);
        log(
            "worker",
            json!({ "name": name, "ok": ok, "out": String::from_utf8_lossy(&out.stdout).trim(), "err": String::from_utf8_lossy(&out.stderr).trim() }),
        );
        ids.extend(mine);
    }

    let ledger_file = ledger.join("events.jsonl");
    let (end, timeout) = loop {
        thread::sleep(Duration::from_millis(o.tick_ms));
        let now = clock();
        let j = jobs(&fs::read_to_string(&ledger_file).unwrap_or_default(), &ids);
        if j.completed == ids.len() {
            break ("all done", false);
        }
        let last = j.last_t.unwrap_or(t0);
        if live_jobs(&ledger.join("live")) == 0 && now - last >= o.quiet_ms && now - t0 >= o.quiet_ms {
            break ("quiet", false);
        }
        if now - t0 > o.total_ms {
            break ("timeout", true);
        }
    };
    let ended = clock() - t0;
    stop.store(true, Ordering::Relaxed);
    let _ = sampling.join();
    stop_bg(&o.claude, &env, &run, &dir);

    let j = jobs(&fs::read_to_string(&ledger_file).unwrap_or_default(), &ids);
    let killed = *kills.lock().unwrap();
    let (peak_t, p) = peak.lock().unwrap().clone();
    let mut usage = Usage::default();
    let mut seen = HashSet::new();
    let mut sessions = 0;
    for f in transcripts(&o.home, &run, since) {
        u_add(&mut usage, &f, &mut seen);
        sessions += usize::from(!f.to_string_lossy().contains("/subagents/"));
    }
    let reeve = super::day::reeve_metrics(&fs::read_to_string(state.join("reeve/events.jsonl")).unwrap_or_default());
    let total = ids.len();
    let result = json!({
        "arm": o.arm, "run": run, "params": arm_json(&arm), "end": end, "timeout": timeout,
        "endedAt": ended, "jobs": total, "completed": j.completed, "killed": killed,
        "lost": total - j.completed, "started": j.started,
        "doneAt": if j.completed == total { j.last_done.map(|t| t - t0) } else { None },
        "peakRssMb": round(p.rss_kb as f64 / 1024.0, 0), "peakAt": peak_t,
        "peakJobsRssMb": round(p.jobs_rss_kb as f64 / 1024.0, 0),
        "sessionsAtPeak": p.sessions, "jobsAtPeak": p.jobs, "procsAtPeak": p.procs,
        "runFailures": run_failures, "usage": usage.json(), "costUsd": round(usage.cost, 3),
        "sessionCount": sessions, "reeve": reeve,
    });
    fs::write(logs.join("result.json"), serde_json::to_string_pretty(&result)?)?;
    println!("{}", json!({
        "run": run, "completed": j.completed, "killed": killed, "lost": total - j.completed,
        "peakRssMb": result["peakRssMb"], "sessionsAtPeak": p.sessions, "doneAt": result["doneAt"],
        "cost": result["costUsd"],
    }));
    cleanup(&base, &run, &dir)?;
    Ok(0)
}

fn u_add(u: &mut Usage, f: &Path, seen: &mut HashSet<String>) {
    u.add(&fs::read_to_string(f).unwrap_or_default(), seen);
}

fn arm_json(arm: &Arm) -> Value {
    json!({
        "model": arm.model, "effort": arm.effort, "settingSources": arm.setting_sources,
        "setup": arm.setup, "workers": arm.workers, "jobs": arm.jobs, "jobMb": arm.job_mb,
        "jobSeconds": arm.job_seconds, "budgetMb": arm.budget_mb, "env": arm.env,
        "reeve": arm.reeve.clone().unwrap_or_else(|| "built-in".into()),
    })
}

/// Per arm: median [min–max] of each measure.
pub fn score(trials: &Path, only: &[String]) -> io::Result<String> {
    let logs = trials.join("logs");
    let mut runs: Vec<Value> = fs::read_dir(&logs)?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("rp-"))
        .filter_map(|e| serde_json::from_str(&fs::read_to_string(e.path().join("result.json")).ok()?).ok())
        .collect();
    runs.sort_by_key(|r| js_str(r.get("run")));
    let mut arms: Vec<String> = runs.iter().map(|r| js_str(r.get("arm"))).collect();
    arms.dedup();
    let header = [
        "arm", "n", "jobs", "completed", "lost", "killed", "peak RSS MB", "sessions at peak",
        "min to done", "cost $",
    ];
    let mut rows: Vec<Vec<String>> = vec![header.map(String::from).to_vec()];
    for arm in arms.iter().filter(|a| only.is_empty() || only.contains(a)) {
        let rs: Vec<&Value> = runs.iter().filter(|r| r["arm"] == arm.as_str()).collect();
        let col = |k: &str, d| fmt(&rs.iter().map(|r| r[k].as_f64()).collect::<Vec<_>>(), d);
        rows.push(vec![
            arm.clone(),
            rs.len().to_string(),
            col("jobs", 0),
            col("completed", 0),
            col("lost", 0),
            col("killed", 0),
            col("peakRssMb", 0),
            col("sessionsAtPeak", 0),
            fmt(&rs.iter().map(|r| r["doneAt"].as_f64().map(|t| t / 60e3)).collect::<Vec<_>>(), 1),
            col("costUsd", 2),
        ]);
    }
    Ok(rows.iter().map(|r| r.join("\t")).collect::<Vec<_>>().join("\n"))
}

#[cfg(test)]
mod tests;
