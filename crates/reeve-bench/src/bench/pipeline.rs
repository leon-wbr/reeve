//! A pipeline arm (`bench/arms/<ARM>/pipeline` names a preset): the scenario's asks go, as one
//! prompt, to a single `reeve run --pipeline PRESET` worker instead of a communicator. Its
//! steps run unattended, so the simulated user never hears from it: a decision it needs (the
//! rename) is its own call or left open in the report, and the scenario's timed follow-ups are
//! logged as undelivered. The run ends when the pipeline writes its report, and the report's
//! status line is its claim.

use super::check::check;
use super::run::{Bench, Scenario, TOTAL_MS, clock, finish, new_result, note_check};
use super::{home, reeve_bin};
use serde_json::{Value, json};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::Duration;

/// Whether a pipeline's report claims the work done: its status line says `done`.
pub fn claims_done(report: &str) -> bool {
    let first = report.trim_start().lines().next().unwrap_or("");
    first
        .trim()
        .trim_matches(|c: char| !c.is_ascii_alphabetic())
        == "done"
}

/// The step sessions from a pipeline's `steps.jsonl`, as the scorer takes them; a Codex step
/// has no session id, so its tokens aren't counted.
pub fn step_sessions(steps: &str, run: &str) -> Vec<Value> {
    steps
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|s| {
            let step = s["step"].as_str().unwrap_or("step");
            json!({ "name": format!("{run}-{step}"), "kind": "step", "sessionId": s["session"] })
        })
        .collect()
}

fn reeve(b: &Bench, args: &[&str]) -> io::Result<Output> {
    Command::new(reeve_bin()?)
        .env_clear()
        .envs(b.env.iter().map(|(k, v)| (k, v)))
        .args(args)
        .current_dir(&b.dir)
        .stdin(Stdio::null())
        .output()
}

/// The driver's pid, from reeve's registry.
fn driver(state: &Path, run: &str) -> Option<String> {
    let all: Vec<Value> =
        serde_json::from_str(&std::fs::read_to_string(state.join("workers.json")).ok()?).ok()?;
    let w = all.into_iter().find(|w| w["name"] == run)?;
    w["handle"]
        .as_str()
        .filter(|h| !h.is_empty())
        .map(String::from)
}

fn alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

pub(super) fn run(mut b: Bench, arm: &str, preset: &str, s: Scenario) -> io::Result<i32> {
    let run = b.run.clone();
    let dir = PathBuf::from(&b.dir);
    // Bench sessions run without XDG_STATE_HOME, so reeve keeps its records in the default place.
    let state = home().join(".local/state/reeve");
    let report = state.join("reports").join(format!("{run}.md"));
    let steps = state
        .join("logs")
        .join(format!("{run}.pipeline/steps.jsonl"));
    b.t0 = clock();
    let t0 = b.t0;
    let out = reeve(
        &b,
        &["run", "--name", &run, "--pipeline", preset, "--", &s.asks],
    )?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "reeve run --pipeline {preset}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    b.log(
        "start",
        &json!({ "arm": arm, "run": run, "pipeline": preset }),
    );
    let mut result = new_result(arm, &run);
    result["pipeline"] = preset.into();
    let mut followups = s.followups.clone();
    let mut undelivered = vec![];
    let mut last_check = 0;
    let error = loop {
        thread::sleep(Duration::from_secs(5));
        let now = clock();
        if now - last_check > 30_000 {
            last_check = now;
            match check(&dir, &run) {
                Ok(c) => note_check(&b, &mut result, &c, now - t0),
                Err(e) => break Some(e.to_string()),
            }
        }
        while followups
            .first()
            .is_some_and(|f| (now - t0) as f64 >= f["afterS"].as_f64().unwrap_or(0.0) * 1e3)
        {
            let f = followups.remove(0);
            b.log("followup-undelivered", &f);
            undelivered.push(f);
        }
        if report.is_file() {
            break None;
        }
        if driver(&state, &run).is_some_and(|pid| !alive(&pid)) {
            b.log("pipeline-exit", &json!({}));
            break None;
        }
        if now - t0 > TOTAL_MS {
            result["timeout"] = true.into();
            break None;
        }
    };
    if let Some(e) = error {
        result["error"] = e.into();
    }
    let now = clock();
    let c = check(&dir, &run)?;
    let text = std::fs::read_to_string(&report).unwrap_or_default();
    let claimed = claims_done(&text);
    if claimed {
        result["claims"] = json!([{ "t": now - t0, "items": "all" }]);
        result["falseDone"] = (c["done"] != true).into();
    } else {
        result["neverClaimed"] = (c["done"] == true).into();
    }
    if c["done"] == true && result["doneAt"].is_null() {
        result["doneAt"] = json!(now - t0);
    }
    result["final"] = c;
    result["report"] = text.into();
    result["undeliveredFollowups"] = undelivered.into();
    result["endedAt"] = json!(now - t0);
    if let Some(pid) = driver(&state, &run) {
        let _ = Command::new("kill")
            .args(["--", &format!("-{pid}")])
            .stderr(Stdio::null())
            .status();
        thread::sleep(Duration::from_secs(1));
    }
    let _ = reeve(&b, &["rm", &run]);
    let sessions = step_sessions(&std::fs::read_to_string(steps).unwrap_or_default(), &run);
    finish(&b, result, sessions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_done_status_line_is_a_claim() {
        assert!(claims_done("done\nmerged all six"));
        assert!(claims_done("**done**\n"));
        assert!(!claims_done("partial\ndone with five"));
        assert!(!claims_done(""));
    }

    #[test]
    fn step_sessions_name_each_step_of_the_run() {
        let steps =
            "{\"step\":\"brief\",\"session\":\"s1\"}\n{\"step\":\"work\",\"session\":null}\n";
        let s = step_sessions(steps, "rv-p-brief-1");
        assert_eq!(s.len(), 2);
        assert_eq!(s[0]["name"], "rv-p-brief-1-brief");
        assert_eq!(s[0]["sessionId"], "s1");
        assert_eq!(s[1]["kind"], "step");
    }
}
