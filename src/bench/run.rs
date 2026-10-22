//! One bench run: sets up the scenario repo for an arm, drives a communicator session with the
//! scenario's asks, answers it through a simulated user that only knows the key, and records
//! interventions, done-check results, false done, time and tokens.

use super::check::{ITEMS, check};
use super::sim::{Sim, simulate};
use super::{home, js_str, num, root, round, term, trials};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TOTAL_MS: i64 = 40 * 60_000;
const REPLY_DELAY_MS: i64 = 4 * 60_000;
const POLL_AFTER_MS: i64 = 6 * 60_000;
const CLAIM_GRACE_MS: i64 = 5 * 60_000;
const MAX_INTERVENTIONS: usize = 10;

fn clock() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Matches `(^|[-/])<run>(-|/|$)`.
pub fn mine(run: &str, s: &str) -> bool {
    s.char_indices().any(|(i, _)| {
        s[i..].starts_with(run)
            && matches!(s[..i].chars().last(), None | Some('-' | '/'))
            && matches!(s[i + run.len()..].chars().next(), None | Some('-' | '/'))
    })
}

/// Commits that reached main without a passing CI run: a merge whose merged branch tip had no
/// pass, or a commit that landed directly or by fast-forward without a pass of its own.
/// `chain` is `git rev-list --first-parent --parents` output.
pub fn unchecked(chain: &str, passed: impl Fn(&str) -> bool) -> usize {
    chain
        .split('\n')
        .filter(|l| !l.is_empty())
        .filter(|l| {
            let f: Vec<&str> = l.split(' ').collect();
            !passed(f.get(2).unwrap_or(&f[0]))
        })
        .count()
}

struct Bench {
    run: String,
    base: String,
    dir: String,
    logs: PathBuf,
    env: Vec<(OsString, OsString)>,
    t0: i64,
}

impl Bench {
    fn git<S: AsRef<OsStr>>(&self, args: impl IntoIterator<Item = S>) -> io::Result<String> {
        let o = Command::new("git")
            .args(args)
            .stdin(Stdio::null())
            .output()?;
        if !o.status.success() {
            return Err(io::Error::other(
                String::from_utf8_lossy(&o.stderr).trim().to_string(),
            ));
        }
        Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
    }

    fn claude(&self) -> Command {
        let mut c = Command::new("claude");
        c.env_clear().envs(self.env.iter().map(|(k, v)| (k, v)));
        c
    }

    fn log(&self, event: &str, data: &Value) {
        let mut m = Map::new();
        m.insert("t".into(), json!(clock() - self.t0));
        m.insert("event".into(), event.into());
        if let Some(d) = data.as_object() {
            m.extend(d.clone());
        }
        let _ = append(
            &self.logs.join("events.jsonl"),
            format!("{}\n", Value::Object(m)).as_bytes(),
        );
    }

    fn agents(&self) -> Vec<Value> {
        let out = self
            .claude()
            .args(["agents", "--json", "--all"])
            .stdin(Stdio::null())
            .output();
        let all: Vec<Value> = match out {
            Ok(o) if o.status.success() => serde_json::from_slice(&o.stdout).unwrap_or_default(),
            _ => vec![],
        };
        let comm = format!("{}-comm", self.run);
        let is_mine = |v: &Value| mine(&self.run, v.as_str().unwrap_or(""));
        all.into_iter()
            .filter(|a| {
                a["name"] != comm.as_str()
                    && (is_mine(&a["name"]) || is_mine(&a["cwd"]) && a["kind"] == "background")
            })
            .collect()
    }

    fn claude_quiet(&self, cmd: &str, id: &Value) {
        let _ = self
            .claude()
            .arg(cmd)
            .arg(js_str(Some(id)))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    fn cleanup(&self) -> io::Result<()> {
        for a in self.agents() {
            self.claude_quiet("rm", &a["id"]);
        }
        let list = self.git(["-C", &self.base, "worktree", "list", "--porcelain"])?;
        for w in list.split('\n').filter_map(|l| l.strip_prefix("worktree ")) {
            if w == self.dir || w.contains("/.claude/worktrees/") && mine(&self.run, w) {
                let _ = self.git([
                    "-C", &self.base, "worktree", "remove", "--force", "--force", w,
                ]);
            }
        }
        self.git(["-C", &self.base, "worktree", "prune"])?;
        let _ = self.git([
            "-C", &self.base, "config", "--unset-all", "reeve.protect", &format!("^{}$", self.run),
        ]);
        let branches = self.git(["-C", &self.base, "branch", "--format=%(refname:short)"])?;
        for b in branches.split('\n').filter(|b| mine(&self.run, b)) {
            let _ = self.git(["-C", &self.base, "branch", "-D", b]);
        }
        match fs::remove_dir_all(&self.dir) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    fn unchecked_landings(&self) -> io::Result<usize> {
        let common = self.git([
            "-C",
            &self.dir,
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
        ])?;
        let store = Path::new(&common).join("ci");
        let passed = |sha: &str| {
            fs::read_to_string(store.join(format!("{sha}.json")))
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .is_some_and(|r| r["status"] == "pass" || r["everPassed"] == true)
        };
        let start = self.git([
            "-C",
            &self.dir,
            "rev-list",
            "-n1",
            "--grep=^scenario$",
            &self.run,
        ])?;
        let chain = self.git([
            "-C",
            &self.dir,
            "rev-list",
            "--first-parent",
            "--parents",
            &self.run,
            "--not",
            &start,
        ])?;
        Ok(unchecked(&chain, passed))
    }
}

fn append(path: &Path, bytes: &[u8]) -> io::Result<()> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(bytes)
}

fn copy_dir(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for e in fs::read_dir(src)? {
        let e = e?;
        let (from, to) = (e.path(), dst.join(e.file_name()));
        let t = e.file_type()?;
        if t.is_dir() {
            copy_dir(&from, &to)?;
        } else if t.is_symlink() {
            let _ = fs::remove_file(&to);
            std::os::unix::fs::symlink(fs::read_link(&from)?, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Sums token usage over a session's transcript and its subagents; cost uses Sonnet list prices.
fn usage(session_id: &str) -> Map<String, Value> {
    let projects = home().join(".claude/projects");
    let sorted = |d: &Path| -> Vec<OsString> {
        let mut v: Vec<OsString> = fs::read_dir(d)
            .map(|r| r.flatten().map(|e| e.file_name()).collect())
            .unwrap_or_default();
        v.sort();
        v
    };
    let mut files = vec![];
    for p in sorted(&projects) {
        let f = projects.join(&p).join(format!("{session_id}.jsonl"));
        if f.exists() {
            files.push(f);
            let sub = projects.join(&p).join(session_id).join("subagents");
            for s in sorted(&sub) {
                if s.to_string_lossy().ends_with(".jsonl") {
                    files.push(sub.join(s));
                }
            }
        }
    }
    let mut seen: HashMap<String, Value> = HashMap::new();
    let mut assistant_turns = 0;
    for f in &files {
        let text = fs::read_to_string(f).unwrap_or_default();
        for line in text.split('\n') {
            let Ok(m) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let u = &m["message"]["usage"];
            if m["type"] != "assistant" || falsy(u) {
                continue;
            }
            if !f.to_string_lossy().contains("/subagents/") {
                assistant_turns += 1;
            }
            let id = match &m["message"]["id"] {
                Value::Null => format!("#{}", seen.len()),
                id => format!("={id}"),
            };
            seen.insert(id, u.clone());
        }
    }
    let sum = |k: &str| {
        seen.values()
            .map(|u| u[k].as_f64().unwrap_or(0.0))
            .sum::<f64>()
    };
    let (input, cache_write, cache_read, output) = (
        sum("input_tokens"),
        sum("cache_creation_input_tokens"),
        sum("cache_read_input_tokens"),
        sum("output_tokens"),
    );
    let cost = (input * 3.0 + cache_write * 3.75 + cache_read * 0.3 + output * 15.0) / 1e6;
    let v = json!({
        "input": num(input),
        "cacheWrite": num(cache_write),
        "cacheRead": num(cache_read),
        "output": num(output),
        "assistantTurns": assistant_turns,
        "costUsd": round(cost, 3),
        "transcripts": files.len(),
    });
    v.as_object().cloned().unwrap_or_default()
}

#[derive(Default)]
struct Comm {
    busy: bool,
    last_event: i64,
    session: Option<Value>,
    unread: Vec<String>,
}

fn say(
    stdin: &mut ChildStdin,
    comm: &Mutex<Comm>,
    history: &mut Vec<(&'static str, String)>,
    text: &str,
) {
    history.push(("user", text.to_string()));
    let msg = json!({ "type": "user", "message": { "role": "user", "content": text } });
    let _ = writeln!(stdin, "{msg}");
    let mut c = comm.lock().unwrap();
    c.busy = true;
    c.last_event = clock();
}

fn read_stdout(out: impl Read, path: PathBuf, comm: Arc<Mutex<Comm>>) {
    let mut r = BufReader::new(out);
    let mut buf = vec![];
    while r.read_until(b'\n', &mut buf).is_ok_and(|n| n > 0) {
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim_end_matches('\n').trim_end_matches('\r');
        let _ = append(&path, format!("{line}\n").as_bytes());
        if let Ok(m) = serde_json::from_str::<Value>(line) {
            let mut c = comm.lock().unwrap();
            c.last_event = clock();
            if m["type"] == "system" && m["subtype"] == "init" {
                c.session = Some(m["session_id"].clone());
            }
            if m["type"] == "result" {
                c.busy = false;
            } else {
                c.busy = true;
                if m["type"] == "assistant" {
                    let content = m["message"]["content"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                    for x in content {
                        let text = x["text"].as_str().unwrap_or("").trim();
                        if x["type"] == "text" && !text.is_empty() {
                            c.unread.push(text.to_string());
                        }
                    }
                }
            }
        }
        buf.clear();
    }
}

fn read_stderr(mut err: impl Read, path: PathBuf) {
    let mut buf = [0u8; 8192];
    while let Ok(n) = err.read(&mut buf) {
        if n == 0 {
            break;
        }
        let _ = append(&path, &buf[..n]);
    }
}

fn open_asks(dir: &Path) -> Vec<Value> {
    let Ok(entries) = fs::read_dir(dir) else { return vec![] };
    entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json") && !p.with_extension("answer").exists())
        .filter_map(|p| serde_json::from_str(&fs::read_to_string(p).ok()?).ok())
        .collect()
}

fn falsy(v: &Value) -> bool {
    v.is_null() || *v == false || *v == 0 || *v == ""
}

pub fn run(arm: &str, n: &str) -> io::Result<i32> {
    let here = root();
    let base = std::env::var_os("REEVE_BENCH_BASE")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join("work/reeve-vanilla-trial"));
    let trials = trials();
    let run = format!("rv-{arm}-{n}");
    let dir = trials.join(&run);
    let arm_dir = here.join("arms").join(arm);
    let mut env: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(k, _)| {
            let k = k.to_string_lossy();
            !["HERDR_", "ORCH_", "CLAUDECODE", "CLAUDE_CODE_", "PATH", "REEVE_HOME"]
                .iter()
                .any(|p| k.starts_with(p))
        })
        .collect();
    env.push(("ORCH_HOME".into(), trials.join(".orch-sink").into()));
    let exe_dir = std::env::current_exe()?.parent().map(PathBuf::from).unwrap_or_default();
    let path = std::env::var_os("PATH").unwrap_or_default();
    env.push(("PATH".into(), std::env::join_paths(std::iter::once(exe_dir).chain(std::env::split_paths(&path))).map_err(io::Error::other)?));
    let queue = trials.join("logs").join(&run).join("reeve");
    env.push(("REEVE_HOME".into(), queue.clone().into()));
    let mut b = Bench {
        logs: trials.join("logs").join(&run),
        base: base.to_string_lossy().into_owned(),
        dir: dir.to_string_lossy().into_owned(),
        run: run.clone(),
        env,
        t0: clock(),
    };

    b.cleanup()?;
    let _ = fs::remove_dir_all(&b.logs);
    fs::create_dir_all(&b.logs)?;
    b.git([
        "-C", &b.base, "worktree", "add", "-q", "-b", &run, &b.dir, "main",
    ])?;
    copy_dir(&here.join("scenario/repo"), &dir)?;
    if arm_dir.join("repo").exists() {
        copy_dir(&arm_dir.join("repo"), &dir)?;
    }
    let claude_md = dir.join("CLAUDE.md");
    fs::write(
        &claude_md,
        fs::read_to_string(&claude_md)?.replace("{{RUN}}", &run),
    )?;
    b.git(["-C", &b.dir, "add", "-A"])?;
    b.git([
        "-C",
        &b.dir,
        "-c",
        "user.name=bench",
        "-c",
        "user.email=bench@localhost",
        "commit",
        "-qm",
        "scenario",
    ])?;
    if arm_dir.join("setup").exists() {
        let ok = Command::new(arm_dir.join("setup"))
            .current_dir(&dir)
            .env("RUN", &run)
            .env("REEVE", std::env::current_exe()?)
            .status()?
            .success();
        if !ok {
            return Err(io::Error::other("arm setup failed"));
        }
    }

    let comm_name = format!("{run}-comm");
    let mut args: Vec<OsString> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "-n",
        &comm_name,
        "--model",
        "sonnet",
    ]
    .map(OsString::from)
    .to_vec();
    for (file, flag) in [
        ("system.md", "--append-system-prompt-file"),
        ("settings.json", "--settings"),
    ] {
        if arm_dir.join(file).exists() {
            args.extend([flag.into(), arm_dir.join(file).into()]);
        }
    }
    let mut comm = b
        .claude()
        .args(&args)
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = comm.stdin.take().expect("piped");
    let state = Arc::new(Mutex::new(Comm {
        busy: true,
        last_event: clock(),
        ..Default::default()
    }));
    let (out, err) = (
        comm.stdout.take().expect("piped"),
        comm.stderr.take().expect("piped"),
    );
    let (s, p) = (state.clone(), b.logs.join("comm.jsonl"));
    thread::spawn(move || read_stdout(out, p, s));
    let p = b.logs.join("comm.stderr");
    thread::spawn(move || read_stderr(err, p));

    let asks = fs::read_to_string(here.join("scenario/asks.md"))?
        .trim()
        .to_string();
    let key = fs::read_to_string(here.join("scenario/key.md"))?;
    let followups_file = here.join("scenario/followups.json");
    let mut followups: Vec<Value> = if followups_file.exists() {
        serde_json::from_str(&fs::read_to_string(followups_file)?).map_err(io::Error::other)?
    } else {
        vec![]
    };
    let mut history: Vec<(&'static str, String)> = vec![];
    b.t0 = clock();
    let t0 = b.t0;
    say(&mut stdin, &state, &mut history, &asks);
    b.log("start", &json!({ "arm": arm, "run": run }));

    let mut result = json!({
        "arm": arm, "run": run, "interventions": [], "claims": [], "falseDone": null,
        "neverClaimed": false, "doneAt": null, "itemsAt": {}, "blockedMs": {}, "timeout": false,
    });
    let mut seen: Vec<(String, Value)> = vec![];
    let remember = |seen: &mut Vec<(String, Value)>, agents: Vec<Value>| {
        for a in agents {
            let id = js_str(a.get("id"));
            match seen.iter_mut().find(|(k, _)| *k == id) {
                Some((_, v)) => *v = a,
                None => seen.push((id, a)),
            }
        }
    };
    let (mut last_check, mut last_poll, mut last_blocked, mut ended) = (0, clock(), clock(), false);
    let error = 'run: loop {
        if ended {
            break None;
        }
        thread::sleep(Duration::from_secs(5));
        let now = clock();
        if now - last_check > 30_000 {
            last_check = now;
            let c = match check(&dir, &run) {
                Ok(c) => c,
                Err(e) => break Some(e.to_string()),
            };
            for k in ITEMS {
                if c["items"][k] == true && falsy(&result["itemsAt"][k]) {
                    result["itemsAt"][k] = json!(now - t0);
                }
            }
            if c["done"] == true && falsy(&result["doneAt"]) {
                result["doneAt"] = json!(now - t0);
                b.log("done-check", &c);
            }
            if now - last_blocked > 30_000 {
                let agents = b.agents();
                for a in agents.iter().filter(|a| a["state"] == "blocked") {
                    let ms = &mut result["blockedMs"][js_str(a.get("name"))];
                    *ms = json!(ms.as_i64().unwrap_or(0) + now - last_blocked);
                }
                remember(&mut seen, agents);
                last_blocked = now;
            }
        }
        while followups
            .first()
            .is_some_and(|f| (now - t0) as f64 >= f["afterS"].as_f64().unwrap_or(0.0) * 1e3)
        {
            let f = followups.remove(0);
            b.log("followup", &f);
            say(
                &mut stdin,
                &state,
                &mut history,
                f["text"].as_str().unwrap_or(""),
            );
        }
        if let Ok(Some(status)) = comm.try_wait() {
            b.log("comm-exit", &json!({ "code": status.code() }));
            break None;
        }
        if now - t0 > TOTAL_MS {
            result["timeout"] = true.into();
            break None;
        }
        // Questions in the decision queue are read even while the session is busy.
        for q in open_asks(&queue.join("asks")) {
            if now - q["askedAt"].as_i64().unwrap_or(now) < REPLY_DELAY_MS {
                continue;
            }
            let question = js_str(q.get("question"));
            history.push(("communicator (decision queue)", question.clone()));
            let sim = Sim {
                asks: &asks,
                key: &key,
                items: &ITEMS,
                history: &history,
                silent_minutes: 0,
                env: &b.env,
                cwd: &trials,
            };
            let s = match simulate(&sim).or_else(|_| simulate(&sim)) {
                Ok(s) => s,
                Err(e) => break 'run Some(e.to_string()),
            };
            let reply = s["reply"].as_str().filter(|r| !r.is_empty()).unwrap_or("your call").to_string();
            b.log("queue-answer", &json!({ "question": question, "text": reply }));
            push(&mut result["interventions"], json!({ "t": now - t0, "kind": "answer", "via": "queue", "text": reply }));
            history.push(("user", reply.clone()));
            let id = js_str(q.get("id"));
            if let Err(e) = fs::write(queue.join("asks").join(format!("{id}.answer")), reply) {
                break 'run Some(e.to_string());
            }
        }
        let (busy, last_event) = {
            let c = state.lock().unwrap();
            (c.busy, c.last_event)
        };
        if busy || now - last_event < REPLY_DELAY_MS {
            continue;
        }
        let done_at = result["doneAt"].as_i64().unwrap_or(0);
        if done_at != 0 && now - t0 - done_at > CLAIM_GRACE_MS {
            result["neverClaimed"] = true.into();
            break None;
        }
        let silent = now - last_event.max(last_poll) > POLL_AFTER_MS;
        let said = {
            let mut c = state.lock().unwrap();
            if c.unread.is_empty() && !silent {
                continue;
            }
            std::mem::take(&mut c.unread).join("\n\n")
        };
        if !said.is_empty() {
            history.push(("communicator", said));
        }
        last_poll = now;
        let sim = Sim {
            asks: &asks,
            key: &key,
            items: &ITEMS,
            history: &history,
            silent_minutes: if silent {
                ((now - last_event) as f64 / 60e3).round() as i64
            } else {
                0
            },
            env: &b.env,
            cwd: &trials,
        };
        let s = match simulate(&sim).or_else(|_| simulate(&sim)) {
            Ok(s) => s,
            Err(e) => break Some(e.to_string()),
        };
        b.log("sim", &s);
        {
            let mut c = state.lock().unwrap();
            if c.busy || !c.unread.is_empty() {
                c.last_event = clock();
                continue;
            }
        }
        let claimed: Vec<Value> = s["claimedDone"].as_array().cloned().unwrap_or_default();
        if !claimed.is_empty() {
            push(
                &mut result["claims"],
                json!({ "t": now - t0, "items": claimed }),
            );
        }
        if s["allDone"] == true {
            let c = match check(&dir, &run) {
                Ok(c) => c,
                Err(e) => break Some(e.to_string()),
            };
            result["falseDone"] = (c["done"] != true).into();
            let false_items: Vec<&Value> = claimed
                .iter()
                .filter(|k| c["items"][k.as_str().unwrap_or("")] != true)
                .collect();
            result["falseItems"] = json!(false_items);
            if c["done"] == true && falsy(&result["doneAt"]) {
                result["doneAt"] = json!(now - t0);
            }
            result["final"] = c;
            ended = true;
        } else if let Some(reply) = s["reply"].as_str().filter(|r| !r.is_empty()) {
            push(
                &mut result["interventions"],
                json!({ "t": now - t0, "kind": s["kind"], "text": reply }),
            );
            b.log("reply", &json!({ "kind": s["kind"], "text": reply }));
            say(&mut stdin, &state, &mut history, reply);
            if result["interventions"].as_array().map_or(0, Vec::len) >= MAX_INTERVENTIONS {
                ended = true;
            }
        }
    };

    if let Some(e) = error {
        result["error"] = e.into();
    }
    if result["final"].is_null() {
        result["final"] = check(&dir, &run)?;
    }
    result["endedAt"] = json!(clock() - t0);
    drop(stdin);
    term(&comm);
    for a in b.agents() {
        b.claude_quiet("stop", &a["id"]);
    }
    remember(&mut seen, b.agents());
    let comm_session = state.lock().unwrap().session.clone();
    let mut sessions: Vec<Value> = seen.into_iter().map(|(_, a)| a).collect();
    if let Some(id) = comm_session {
        sessions.push(json!({ "name": comm_name, "sessionId": id, "kind": "comm" }));
    }
    let sessions: Vec<Value> = sessions
        .iter()
        .map(|s| {
            let mut m = Map::new();
            for k in ["name", "state", "kind"] {
                if let Some(v) = s.get(k) {
                    m.insert(k.into(), v.clone());
                }
            }
            m.extend(usage(&js_str(s.get("sessionId"))));
            Value::Object(m)
        })
        .collect();
    let workers = sessions.iter().filter(|s| s["kind"] != "comm").count();
    let failed = sessions
        .iter()
        .filter(|s| s["kind"] != "comm" && s["assistantTurns"] == 0)
        .count();
    let cost: f64 = sessions.iter().filter_map(|s| s["costUsd"].as_f64()).sum();
    result["sessions"] = sessions.into();
    result["workers"] = workers.into();
    result["failedStarts"] = failed.into();
    result["costUsd"] = round(cost, 3);
    result["uncheckedLandings"] = b.unchecked_landings()?.into();
    let bundle = b.logs.join("repo.bundle").to_string_lossy().into_owned();
    let _ = b.git(["-C", &b.dir, "bundle", "create", &bundle, &run]);
    let graph = b.git([
        "-C",
        &b.dir,
        "log",
        "--oneline",
        "--graph",
        "--all",
        "-n",
        "40",
    ])?;
    fs::write(b.logs.join("git-log.txt"), graph)?;
    fs::write(
        b.logs.join("result.json"),
        serde_json::to_string_pretty(&result)?,
    )?;
    let done_min = match result["doneAt"].as_f64() {
        Some(t) if t != 0.0 => round(t / 60e3, 1),
        _ => result["doneAt"].clone(),
    };
    println!(
        "{}",
        json!({
            "run": run,
            "interventions": result["interventions"].as_array().map_or(0, Vec::len),
            "falseDone": result["falseDone"],
            "done": result["final"]["done"],
            "doneMin": done_min,
            "cost": result["costUsd"],
        })
    );
    b.cleanup()?;
    Ok(0)
}

fn push(arr: &mut Value, v: Value) {
    if let Some(a) = arr.as_array_mut() {
        a.push(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mine_matches_the_run_only_as_a_whole_segment() {
        let r = "rv-b1-1";
        for s in [
            "rv-b1-1",
            "rv-b1-1-comm",
            "/x/.claude/worktrees/rv-b1-1-fix",
            "a/rv-b1-1/b",
        ] {
            assert!(mine(r, s), "{s}");
        }
        for s in ["rv-b1-10", "rv-b1-12-x", "xrv-b1-1", "rv-b1-1x", ""] {
            assert!(!mine(r, s), "{s}");
        }
    }

    #[test]
    fn unchecked_landings_count_commits_and_merged_tips_without_a_pass() {
        let chain = "m1 p1 tip1\nm2 p2 tip2\nff1 p3\nff2 p4\n";
        let passed = |sha: &str| ["tip1", "ff1", "m2"].contains(&sha);
        assert_eq!(unchecked(chain, passed), 2);
        assert_eq!(unchecked("", passed), 0);
    }
}
