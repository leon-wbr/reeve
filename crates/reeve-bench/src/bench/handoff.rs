//! The handoff scenario: a session starts four long tasks, is told to hand off once all four run,
//! and its successor has to bring the four results home. The tasks are stand-ins
//! (`reeve-bench task`) that wait on a simulated clock, which the driver jumps ahead whenever
//! every live session is just waiting on it.

use super::run::{Comm, append, bench_env, clock, read_stderr, read_stdout, say, usage};
use super::score::fmt;
use super::{js_str, root, round, term, trials};
use reeve::workers::Registry;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

/// The message Leon's context hook sends at the threshold.
pub const HOOK: &str = "Context check: this session has used 350K tokens, past the 350K handoff \
    threshold. Finish the step in hand, write your handoff (your role file says where), reply with \
    its path and stop. Do not take on new work.";
/// No transcript has grown for this long, so every session is waiting.
const QUIET_MS: i64 = 20_000;
const REAL_CAP_MS: i64 = 45 * 60_000;
const SIM_CAP_MS: i64 = 6 * 3_600_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub minutes: f64,
    pub result: String,
}

/// How far to move the clock: to the earliest due task, if all of them are still ahead.
pub fn jump(dues: &[i64], now: i64) -> i64 {
    dues.iter()
        .min()
        .map_or(0, |d| if *d > now { d - now } else { 0 })
}

pub fn task_line(t: &Task) -> String {
    format!("task {}: {}", t.id, t.result)
}

/// Results of `tasks` that made it into the final report.
pub fn found(report: &str, tasks: &[Task]) -> usize {
    tasks.iter().filter(|t| report.contains(&t.result)).count()
}

/// Task starts beyond the first of each id.
pub fn redone(starts: &[String]) -> usize {
    starts.len() - starts.iter().collect::<HashSet<_>>().len()
}

/// Where Claude Code keeps the transcripts of sessions started in `cwd`.
pub fn project_dir(projects: &Path, cwd: &Path) -> PathBuf {
    let name: String = cwd
        .display()
        .to_string()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    projects.join(name)
}

pub fn mcp_servers(mcp_json: &str) -> Vec<String> {
    serde_json::from_str::<Value>(mcp_json)
        .ok()
        .and_then(|v| {
            v["mcpServers"]
                .as_object()
                .map(|m| m.keys().cloned().collect())
        })
        .unwrap_or_default()
}

/// The simulated clock is the real one plus the offset the driver keeps in `<sim>/clock`.
fn sim_now(sim: &Path) -> i64 {
    let offset = fs::read_to_string(sim.join("clock")).unwrap_or_default();
    clock() + offset.trim().parse::<i64>().unwrap_or(0)
}

fn tasks_in(file: &Path) -> io::Result<Vec<Task>> {
    serde_json::from_str(&fs::read_to_string(file)?).map_err(io::Error::other)
}

/// `reeve-bench task <id>`: waits out the task's simulated duration, then prints its result.
pub fn task(id: &str) -> io::Result<i32> {
    let sim = PathBuf::from(
        std::env::var_os("REEVE_BENCH_SIM")
            .ok_or_else(|| io::Error::other("reeve-bench task only runs inside a bench run"))?,
    );
    let tasks = tasks_in(&sim.join("tasks.json"))?;
    let t = tasks
        .iter()
        .find(|t| t.id == id)
        .ok_or_else(|| io::Error::other(format!("no task {id}")))?;
    let start = sim_now(&sim);
    let due = start + (t.minutes * 60_000.0) as i64;
    let pid = std::process::id();
    let start_line = json!({ "id": id, "start": start, "pid": pid });
    append(
        &sim.join("starts.jsonl"),
        format!("{start_line}\n").as_bytes(),
    )?;
    let mark = sim.join("waiting").join(format!("{id}-{pid}"));
    fs::create_dir_all(mark.parent().expect("waiting dir"))?;
    fs::write(&mark, due.to_string())?;
    while sim_now(&sim) < due {
        thread::sleep(Duration::from_millis(500));
    }
    let _ = fs::remove_file(&mark);
    println!("{}", task_line(t));
    Ok(0)
}

fn started(sim: &Path) -> Vec<String> {
    fs::read_to_string(sim.join("starts.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|v| js_str(v.get("id")))
        .collect()
}

/// Due times of tasks whose process is still alive; a killed one leaves its mark behind.
fn live_dues(sim: &Path) -> Vec<i64> {
    let Ok(entries) = fs::read_dir(sim.join("waiting")) else {
        return vec![];
    };
    entries
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let pid = name.rsplit('-').next().unwrap_or("");
            Command::new("kill")
                .args(["-0", pid])
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        })
        .filter_map(|e| fs::read_to_string(e.path()).ok()?.trim().parse().ok())
        .collect()
}

fn newest(dir: &Path) -> Option<SystemTime> {
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            if m.is_dir() {
                newest(&e.path())
            } else {
                m.modified().ok()
            }
        })
        .max()
}

fn ms(t: SystemTime) -> i64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    state: Arc<Mutex<Comm>>,
}

impl Session {
    fn start(
        env: &[(OsString, OsString)],
        cwd: &Path,
        name: &str,
        system: &Path,
        logs: &Path,
    ) -> io::Result<Session> {
        let mut child = Command::new("claude")
            .args([
                "-p",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
            ])
            .args(["--verbose", "-n", name, "--model", "sonnet"])
            .arg("--append-system-prompt-file")
            .arg(system)
            .env_clear()
            .envs(env.iter().map(|(k, v)| (k, v)))
            .current_dir(cwd)
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
        let (s, p) = (state.clone(), logs.join(format!("{name}.jsonl")));
        thread::spawn(move || read_stdout(out, p, s));
        let p = logs.join(format!("{name}.stderr"));
        thread::spawn(move || read_stderr(err, p));
        let stdin = child.stdin.take();
        Ok(Session {
            child,
            stdin,
            state,
        })
    }

    fn say(&mut self, text: &str) {
        if let Some(stdin) = self.stdin.as_mut() {
            say(stdin, &self.state, &mut vec![], text);
        }
    }

    fn stop(&mut self) {
        self.stdin = None;
        term(&self.child);
    }

    fn busy(&self) -> bool {
        self.state.lock().unwrap().busy
    }

    fn last_event(&self) -> i64 {
        self.state.lock().unwrap().last_event
    }

    fn id(&self) -> Option<String> {
        self.state
            .lock()
            .unwrap()
            .session
            .as_ref()
            .and_then(|v| v.as_str().map(String::from))
    }
}

enum Successor {
    Stream(Session),
    /// A named worker started by `reeve handoff`.
    Worker(String),
}

/// Background sessions started in the run's directory.
fn bg_sessions(env: &[(OsString, OsString)], dir: &Path) -> Vec<Value> {
    let out = Command::new("claude")
        .args(["agents", "--json", "--all"])
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .output();
    let all: Vec<Value> = match out {
        Ok(o) if o.status.success() => serde_json::from_slice(&o.stdout).unwrap_or_default(),
        _ => vec![],
    };
    let dir = dir.display().to_string();
    all.into_iter()
        .filter(|a| a["kind"] == "background" && a["cwd"] == dir.as_str())
        .collect()
}

fn remove_bg(env: &[(OsString, OsString)], dir: &Path) {
    for a in bg_sessions(env, dir) {
        for cmd in ["stop", "rm"] {
            let _ = Command::new("claude")
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

fn wrote_assistant(transcripts: &Path, session: Option<&str>) -> bool {
    session.is_some_and(|id| {
        fs::read_to_string(transcripts.join(format!("{id}.jsonl")))
            .is_ok_and(|t| t.contains(r#""type":"assistant""#))
    })
}

fn minutes(from: Option<i64>, to: Option<i64>) -> Value {
    match (from, to) {
        (Some(a), Some(b)) => round((b - a) as f64 / 60e3, 1),
        _ => Value::Null,
    }
}

/// One run of an arm of the handoff scenario, named rh-<ARM>-<N>. With `real`, the clock isn't
/// jumped and tasks last a sixtieth of their simulated time (an hour becomes a minute).
pub fn run(arm: &str, n: &str, real: bool) -> io::Result<i32> {
    let here = root().join("handoff");
    let system = here.join("arms").join(arm).join("system.md");
    if !system.is_file() {
        return Err(io::Error::other(format!(
            "no arm {arm} in bench/handoff/arms"
        )));
    }
    let run = format!("rh-{arm}-{n}");
    let dir = trials().join(&run);
    let logs = trials().join("logs").join(&run);
    let (sim, state) = (logs.join("sim"), logs.join("state"));
    let mut env = bench_env()?;
    remove_bg(&env, &dir);
    for d in [&dir, &logs] {
        match fs::remove_dir_all(d) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    for d in [&dir, &sim, &state] {
        fs::create_dir_all(d)?;
    }
    // A background session stops at a dialog for MCP servers of any .mcp.json above its directory
    // that it hasn't seen yet, so the run's directory settles them as disabled.
    let servers: Vec<String> = dir
        .ancestors()
        .filter_map(|d| fs::read_to_string(d.join(".mcp.json")).ok())
        .flat_map(|j| mcp_servers(&j))
        .collect();
    fs::create_dir_all(dir.join(".claude"))?;
    fs::write(
        dir.join(".claude/settings.local.json"),
        json!({ "disabledMcpjsonServers": servers }).to_string(),
    )?;
    let mut tasks = tasks_in(&here.join("tasks.json"))?;
    if real {
        tasks.iter_mut().for_each(|t| t.minutes /= 60.0);
    }
    fs::write(
        sim.join("tasks.json"),
        serde_json::to_string_pretty(&tasks)?,
    )?;
    fs::write(sim.join("clock"), "0")?;
    env.push(("REEVE_BENCH_SIM".into(), sim.clone().into()));
    env.push(("XDG_STATE_HOME".into(), state.clone().into()));
    let transcripts = project_dir(&super::home().join(".claude/projects"), &dir);
    let reeve = Registry::at(state.join("reeve"));
    let log = |event: &str, data: Value| {
        let line = json!({ "t": clock(), "sim": sim_now(&sim), "event": event, "data": data });
        let _ = append(&logs.join("events.jsonl"), format!("{line}\n").as_bytes());
    };

    let mut comm = Session::start(&env, &dir, &format!("{run}-comm"), &system, &logs)?;
    comm.say(fs::read_to_string(here.join("asks.md"))?.trim());
    let (t0, s0) = (clock(), sim_now(&sim));
    log("start", json!({ "arm": arm, "real": real }));
    let (mut due, mut handed, mut working, mut ended) = (None, None, None, None);
    let mut successor: Option<Successor> = None;
    let mut comm_id = None;
    let error = loop {
        thread::sleep(Duration::from_secs(2));
        let (now, simt) = (clock(), sim_now(&sim));
        comm_id = comm_id.or_else(|| comm.id());
        if fs::metadata(dir.join("FINAL.md")).is_ok_and(|m| m.len() > 0) {
            ended = Some(simt);
            break None;
        }
        if now - t0 > REAL_CAP_MS || simt - s0 > SIM_CAP_MS {
            break Some("timed out".to_string());
        }
        if due.is_none() && started(&sim).iter().collect::<HashSet<_>>().len() >= tasks.len() {
            comm.say(HOOK);
            due = Some(simt);
            log("handoff-due", json!({}));
        }
        if !real {
            let last = [
                Some(comm.last_event()),
                match &successor {
                    Some(Successor::Stream(s)) => Some(s.last_event()),
                    _ => None,
                },
                newest(&transcripts).map(ms),
            ]
            .into_iter()
            .flatten()
            .max()
            .unwrap_or(now);
            let by = jump(&live_dues(&sim), simt);
            if now - last > QUIET_MS && by > 0 {
                fs::write(sim.join("clock"), (simt + by - now).to_string())?;
                log("jump", json!({ "byMin": round(by as f64 / 60e3, 1) }));
            }
        }
        if due.is_some() && handed.is_none() && !comm.busy() {
            let r1 = fs::read_dir(state.join("reeve/handoffs"))
                .ok()
                .and_then(|mut d| d.next())
                .and_then(|e| e.ok())
                .map(|e| {
                    e.path()
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                });
            if let Some(name) = r1 {
                successor = Some(Successor::Worker(name));
            } else if dir.join("HANDOFF.md").is_file() {
                let mut s = Session::start(&env, &dir, &format!("{run}-next"), &system, &logs)?;
                s.say("You take over from a previous session that handed off. Read HANDOFF.md in this directory and continue the work from there.");
                successor = Some(Successor::Stream(s));
            }
            if successor.is_some() {
                handed = Some(simt);
                comm.stop();
                log("handed-off", json!({}));
            }
        }
        if handed.is_none() && matches!(comm.child.try_wait(), Ok(Some(_))) {
            break Some("the session exited without handing off".into());
        }
        if working.is_none() {
            let id = match &successor {
                Some(Successor::Stream(s)) => s.id(),
                Some(Successor::Worker(name)) => {
                    let handle = reeve.get(name).map(|w| w.handle).unwrap_or_default();
                    bg_sessions(&env, &dir)
                        .into_iter()
                        .find(|a| a["id"] == handle.as_str())
                        .map(|a| js_str(a.get("sessionId")))
                }
                None => None,
            };
            if wrote_assistant(&transcripts, id.as_deref()) {
                working = Some(simt);
                log("successor-working", json!({}));
            }
        }
    };

    comm.stop();
    let mut ids: Vec<String> = comm_id.into_iter().collect();
    if let Some(Successor::Stream(mut s)) = successor {
        ids.extend(s.id());
        s.stop();
    }
    ids.extend(
        bg_sessions(&env, &dir)
            .iter()
            .map(|a| js_str(a.get("sessionId"))),
    );
    let cost: f64 = ids
        .iter()
        .filter_map(|id| usage(id)["costUsd"].as_f64())
        .sum();
    let report = fs::read_to_string(dir.join("FINAL.md")).unwrap_or_default();
    let starts = started(&sim);
    let result = json!({
        "arm": arm,
        "run": run,
        "real": real,
        "error": error,
        "handoffWaitMin": minutes(due, working),
        "handedOffMin": minutes(due, handed),
        "found": found(&report, &tasks),
        "of": tasks.len(),
        "starts": starts.len(),
        "redone": redone(&starts),
        "simMin": minutes(Some(s0), ended.or(Some(sim_now(&sim)))),
        "realMin": round((clock() - t0) as f64 / 60e3, 1),
        "sessions": ids.len(),
        "costUsd": round(cost, 3),
    });
    fs::write(
        logs.join("result.json"),
        serde_json::to_string_pretty(&result)?,
    )?;
    if dir.join("FINAL.md").is_file() {
        fs::copy(dir.join("FINAL.md"), logs.join("FINAL.md"))?;
    }
    let _ = fs::copy(dir.join("HANDOFF.md"), logs.join("HANDOFF.md"));
    println!("{result}");
    remove_bg(&env, &dir);
    let _ = fs::remove_dir_all(&dir);
    Ok(0)
}

/// Per arm: time from "handoff due" to the successor working, results brought home, work redone
/// and cost, as median [min–max] over its runs.
pub fn score(arms: &[String]) -> io::Result<String> {
    let logs = trials().join("logs");
    let mut runs: Vec<Value> = fs::read_dir(&logs)?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("rh-"))
        .filter_map(|e| {
            serde_json::from_str(&fs::read_to_string(e.path().join("result.json")).ok()?).ok()
        })
        .filter(|r: &Value| r["real"] != true)
        .collect();
    runs.sort_by_key(|r| js_str(r.get("run")));
    let mut names: Vec<String> = runs.iter().map(|r| js_str(r.get("arm"))).collect();
    names.dedup();
    let mut out = "arm  n  wait for successor (sim min)  results  redone  cost $".to_string();
    for arm in names.iter().filter(|a| arms.is_empty() || arms.contains(a)) {
        let mine: Vec<&Value> = runs.iter().filter(|r| r["arm"] == arm.as_str()).collect();
        let col = |k: &str| mine.iter().map(|r| r[k].as_f64()).collect::<Vec<_>>();
        out += &format!(
            "\n{arm}  {}  {}  {}  {}  {}",
            mine.len(),
            fmt(&col("handoffWaitMin"), 1),
            fmt(&col("found"), 0),
            fmt(&col("redone"), 0),
            fmt(&col("costUsd"), 2)
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(id: &str, result: &str) -> Task {
        Task {
            id: id.into(),
            minutes: 20.0,
            result: result.into(),
        }
    }

    #[test]
    fn the_clock_jumps_to_the_earliest_due_task() {
        assert_eq!(jump(&[5_000, 3_000, 9_000], 1_000), 2_000);
        assert_eq!(jump(&[], 1_000), 0);
        assert_eq!(jump(&[900, 3_000], 1_000), 0);
    }

    #[test]
    fn a_task_prints_its_fixed_result() {
        assert_eq!(task_line(&t("t3", "it races")), "task t3: it races");
    }

    #[test]
    fn found_counts_each_result_once() {
        let tasks = [t("t1", "drops a comma"), t("t2", "regex is slow")];
        assert_eq!(found("task t1: drops a comma\ndrops a comma", &tasks), 1);
        assert_eq!(found("- regex is slow\n- drops a comma", &tasks), 2);
        assert_eq!(found("", &tasks), 0);
    }

    #[test]
    fn redone_counts_repeat_starts() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(redone(&s(&["t1", "t2", "t1", "t1", "t3"])), 2);
        assert_eq!(redone(&[]), 0);
    }

    #[test]
    fn mcp_server_names_come_from_each_mcp_json() {
        let j = r#"{"mcpServers":{"exa":{"type":"http"},"context7":{"type":"http"}}}"#;
        assert_eq!(mcp_servers(j), ["exa", "context7"]);
        assert!(mcp_servers("not json").is_empty());
    }

    #[test]
    fn project_dirs_replace_slashes_and_dots() {
        assert_eq!(
            project_dir(
                Path::new("/h/.claude/projects"),
                Path::new("/w/reeve/.claude/x")
            ),
            Path::new("/h/.claude/projects/-w-reeve--claude-x")
        );
    }
}
