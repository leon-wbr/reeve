//! Pipelines: a named worker run as a short, fixed list of agent steps. Each step is one
//! headless agent call (`claude -p` or `codex exec`) with its preset prompt, model and
//! permissions; it gets the user's ask and the previous step's output, and the last step writes
//! the report. Nothing but plumbing runs between steps: no step is code.

use crate::workers::{self, Kind, Registry, Worker};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

/// The presets reeve ships; `~/.config/reeve/pipelines.json` adds more in the same shape and
/// replaces one of the same name.
pub const BUILT_IN: &str = include_str!("pipelines.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Permission {
    /// Reads, changes nothing: Claude's plan mode, Codex's read-only sandbox.
    ReadOnly,
    /// Works without asking: Claude's auto mode, Codex's workspace-write sandbox.
    Auto,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Models {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub name: String,
    /// The agent CLI; unset, the step runs the worker's `--agent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<Kind>,
    /// The model per agent; unset, the agent's own default.
    #[serde(default)]
    pub model: Models,
    pub permission: Permission,
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub about: String,
    pub steps: Vec<Step>,
}

/// What `reeve run --pipeline` hands the driver: the preset as it was when the worker started.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub preset: String,
    pub agent: Kind,
    pub ask: String,
    pub steps: Vec<Step>,
}

/// `$XDG_CONFIG_HOME/reeve/pipelines.json`, by default `~/.config/reeve/pipelines.json`.
pub fn config_path() -> Option<PathBuf> {
    crate::context::config_path().map(|p| p.with_file_name("pipelines.json"))
}

fn parse(text: &str, whence: &str) -> Result<BTreeMap<String, Preset>, String> {
    let all: BTreeMap<String, Preset> =
        serde_json::from_str(text).map_err(|e| format!("{whence}: {e}"))?;
    for (name, p) in &all {
        workers::check_name(name).map_err(|e| format!("{whence}: preset {e}"))?;
        if p.steps.is_empty() {
            return Err(format!("{whence}: preset {name} has no steps"));
        }
        for s in &p.steps {
            workers::check_name(&s.name).map_err(|e| format!("{whence}: {name}: step {e}"))?;
            if s.prompt.trim().is_empty() {
                return Err(format!("{whence}: {name}: step {} has no prompt", s.name));
            }
        }
    }
    Ok(all)
}

/// The built-in presets, then the user's, which replace any of the same name.
pub fn presets(user: Option<&str>) -> Result<BTreeMap<String, Preset>, String> {
    let mut all = parse(BUILT_IN, "built-in presets")?;
    if let Some(text) = user {
        all.extend(parse(text, "~/.config/reeve/pipelines.json")?);
    }
    Ok(all)
}

pub fn load() -> io::Result<BTreeMap<String, Preset>> {
    let user = config_path().and_then(|p| fs::read_to_string(p).ok());
    presets(user.as_deref()).map_err(io::Error::other)
}

/// `reeve pipelines`: each preset and its steps.
pub fn list(all: &BTreeMap<String, Preset>) -> String {
    all.iter()
        .map(|(name, p)| {
            let steps: Vec<String> = p
                .steps
                .iter()
                .map(|s| {
                    let model = [("claude", &s.model.claude), ("codex", &s.model.codex)]
                        .iter()
                        .filter_map(|(k, m)| m.as_ref().map(|m| format!("{k} {m}")))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let agent = s.agent.map(|a| format!("{a}, ")).unwrap_or_default();
                    let perm = match s.permission {
                        Permission::ReadOnly => "read-only",
                        Permission::Auto => "auto",
                    };
                    let model = if model.is_empty() {
                        String::new()
                    } else {
                        format!(", {model}")
                    };
                    format!("{} ({agent}{perm}{model})", s.name)
                })
                .collect();
            format!("{name}: {}\n  {}", p.about, steps.join(" → "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The step's preset prompt, then the user's ask and the previous step's output, each in its
/// own tag. Every step runs unattended, and is told so.
pub fn step_prompt(step: &Step, ask: &str, previous: Option<(&str, &str)>) -> String {
    let mut p = format!(
        "{}\n\nYou are one step of an unattended pipeline: nobody reads your messages or can \
         answer a question. Decide what you safely can yourself; leave anything only the user \
         can decide undone, and name it in your output.\n\n<ask>\n{}\n</ask>",
        step.prompt.trim(),
        ask.trim()
    );
    if let Some((name, out)) = previous {
        p += &format!(
            "\n\n<previous step=\"{name}\">\n{}\n</previous>",
            out.trim()
        );
    }
    p
}

/// The whole command line of one step. `out` is where Codex writes its last message; Claude
/// prints its result as JSON. A step that may write gets reeve's report and state dirs.
pub fn step_argv(
    step: &Step,
    kind: Kind,
    name: &str,
    report: &Path,
    out: &Path,
    prompt: &str,
) -> Vec<String> {
    let s = |v: &[&str]| v.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    let mut argv = match kind {
        Kind::Claude => {
            let mode = match step.permission {
                Permission::ReadOnly => "plan",
                Permission::Auto => "auto",
            };
            let mut a = s(&["claude", "-p", "--output-format", "json"]);
            if let Some(m) = &step.model.claude {
                a.extend(s(&["--model", m]));
            }
            a.extend(s(&["--permission-mode", mode]));
            let label = format!("{name}-{}", step.name);
            a.extend(workers::claude_args(&label, report, &[]));
            a
        }
        Kind::Codex => {
            let mut a = s(&["codex", "exec", "--skip-git-repo-check"]);
            a.extend(["--output-last-message".into(), out.display().to_string()]);
            if let Some(m) = &step.model.codex {
                a.extend(s(&["-m", m]));
            }
            match step.permission {
                // Codex refuses --add-dir in a read-only sandbox.
                Permission::ReadOnly => a.extend(s(&["-s", "read-only"])),
                Permission::Auto => {
                    a.extend(workers::codex_args(report, &s(&["-s", "workspace-write"])))
                }
            }
            a
        }
    };
    argv.push(prompt.into());
    argv
}

/// A step's output and session id: Claude's from its JSON result on stdout, Codex's from the
/// last-message file (its session id isn't printed without `--json`). An error result or empty
/// text is no output.
pub fn output(kind: Kind, stdout: &str, last_message: Option<&str>) -> (Option<String>, Value) {
    let text = |t: Option<&str>| t.map(str::trim).filter(|t| !t.is_empty()).map(String::from);
    match kind {
        Kind::Claude => {
            let v: Value = serde_json::from_str(stdout.trim()).unwrap_or_default();
            let ok = v["is_error"] != true;
            let out = text(v["result"].as_str()).filter(|_| ok);
            (out, v["session_id"].clone())
        }
        Kind::Codex => (text(last_message), Value::Null),
    }
}

/// Whether a report starts with the status line the worker contract asks for.
pub fn has_status(report: &str) -> bool {
    let first = report.trim_start().lines().next().unwrap_or("");
    let word = first
        .trim()
        .trim_matches(|c: char| !c.is_ascii_alphabetic());
    matches!(word, "done" | "partial" | "failed")
}

/// A report the last step didn't write: its output, as `partial` unless it says otherwise, so
/// the plumbing never claims `done` for it.
pub fn fallback_report(out: &str) -> String {
    if has_status(out) {
        format!("{}\n", out.trim())
    } else {
        format!("partial\n\n{}\n", out.trim())
    }
}

/// Variables by which a step would take itself for the session that started the worker.
const CALLER_ENV: [&str; 6] = [
    "CLAUDECODE",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_ENTRYPOINT",
    "CODEX_THREAD_ID",
    "CODEX_SESSION_ID",
    "HERDR_PANE_ID",
];

/// Where a pipeline worker's job, step outputs and `steps.jsonl` live: `logs/NAME.pipeline/`
/// beside `reports/`.
pub fn job_dir(w: &Worker) -> PathBuf {
    workers::exec_log(w).with_extension("pipeline")
}

/// `reeve pipeline-run`: runs the worker's steps in order in its cwd, each step's output
/// passed to the next, and records each step in `steps.jsonl`. A step that fails or prints
/// nothing ends the run with a `failed` report naming it.
pub fn drive(r: &Registry, name: &str) -> io::Result<()> {
    let w = r.get(name)?;
    let dir = job_dir(&w);
    let job: Job = serde_json::from_str(&fs::read_to_string(dir.join("job.json"))?)
        .map_err(io::Error::other)?;
    let n = job.steps.len();
    let mut previous: Option<(String, String)> = None;
    for (i, step) in job.steps.iter().enumerate() {
        let prev = previous.as_ref().map(|(a, b)| (a.as_str(), b.as_str()));
        let mut prompt = step_prompt(step, &job.ask, prev);
        if i + 1 == n {
            prompt = workers::with_report(&prompt, &w.report);
        }
        let kind = step.agent.unwrap_or(job.agent);
        let base = dir.join(format!("{}-{}", i + 1, step.name));
        let out = base.with_extension("md");
        let argv = step_argv(step, kind, name, &w.report, &out, &prompt);
        let t0 = Instant::now();
        let mut cmd = Command::new(&argv[0]);
        for k in CALLER_ENV {
            cmd.env_remove(k);
        }
        let status = cmd
            .args(&argv[1..])
            .current_dir(&w.cwd)
            .stdin(Stdio::null())
            .stdout(File::create(base.with_extension("stdout"))?)
            .stderr(File::create(base.with_extension("stderr"))?)
            .status();
        let stdout = fs::read_to_string(base.with_extension("stdout")).unwrap_or_default();
        let last = fs::read_to_string(&out).ok();
        let (text, session) = output(kind, &stdout, last.as_deref());
        let exit = status.as_ref().ok().and_then(|s| s.code());
        let mut log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("steps.jsonl"))?;
        writeln!(
            log,
            "{}",
            json!({
                "step": step.name, "agent": kind, "exit": exit, "session": session,
                "ms": t0.elapsed().as_millis() as u64, "output": text.is_some(),
            })
        )?;
        let text = match (status, text) {
            (Ok(s), Some(t)) if s.success() => t,
            (status, _) => {
                let why = match status {
                    Err(e) => format!("could not start {kind}: {e}"),
                    Ok(s) if !s.success() => format!("{kind} exited with {s}"),
                    Ok(_) => format!("{kind} printed no output"),
                };
                if !w.report.is_file() {
                    fs::write(
                        &w.report,
                        format!(
                            "failed\n\nPipeline {} stopped at step {} of {n}, {}: {why}. Its \
                             output and errors are in {}.\n",
                            job.preset,
                            i + 1,
                            step.name,
                            crate::setup::tilde(&dir)
                        ),
                    )?;
                }
                return Ok(());
            }
        };
        if kind == Kind::Claude {
            fs::write(&out, &text)?;
        }
        previous = Some((step.name.clone(), text));
    }
    if !w.report.is_file()
        && let Some((_, out)) = previous
    {
        fs::write(&w.report, fallback_report(&out))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(perm: Permission) -> Step {
        Step {
            name: "work".into(),
            agent: None,
            model: Models {
                claude: Some("opus".into()),
                codex: Some("gpt-5.5".into()),
            },
            permission: perm,
            prompt: "Do it.".into(),
        }
    }

    #[test]
    fn built_in_presets_parse_and_include_a_plain_control() {
        let all = presets(None).unwrap();
        for name in ["plain", "brief", "plan", "review"] {
            assert!(all.contains_key(name), "{name}");
        }
        assert_eq!(all["plain"].steps.len(), 1);
        assert!(all.values().all(|p| p.steps.len() <= 3));
    }

    #[test]
    fn a_user_preset_replaces_one_of_the_same_name_and_adds_new_ones() {
        let user = r#"{"plain": {"about": "mine", "steps": [
            {"name": "w", "permission": "auto", "prompt": "go"}]},
            "x": {"about": "x", "steps": [
            {"name": "a", "agent": "codex", "permission": "read-only", "prompt": "look"}]}}"#;
        let all = presets(Some(user)).unwrap();
        assert_eq!(all["plain"].about, "mine");
        assert_eq!(all["x"].steps[0].agent, Some(Kind::Codex));
        assert!(all.contains_key("review"));
    }

    #[test]
    fn invalid_presets_are_refused_with_where() {
        let bad = [
            r#"{"x": {"about": "", "steps": []}}"#,
            r#"{"x": {"about": "", "steps": [{"name": "a", "permission": "auto", "prompt": " "}]}}"#,
            r#"{"x": {"about": "", "steps": [{"name": "a", "permission": "sudo", "prompt": "p"}]}}"#,
            r#"{"x": {"about": "", "steps": [{"name": "a", "permission": "auto", "prompt": "p", "extra": 1}]}}"#,
        ];
        for b in bad {
            let e = presets(Some(b)).unwrap_err();
            assert!(e.contains("pipelines.json"), "{e}");
        }
    }

    #[test]
    fn a_step_gets_its_prompt_the_ask_and_the_previous_output_in_tags() {
        let s = step(Permission::Auto);
        let first = step_prompt(&s, "fix it\n", None);
        assert!(first.starts_with("Do it."));
        assert!(first.contains("<ask>\nfix it\n</ask>"));
        assert!(first.contains("unattended"));
        assert!(!first.contains("<previous"));
        let next = step_prompt(&s, "fix it", Some(("brief", "the brief")));
        assert!(next.ends_with("<previous step=\"brief\">\nthe brief\n</previous>"));
    }

    #[test]
    fn claude_steps_run_headless_with_their_model_mode_and_report_access() {
        let r = Path::new("/s/reports/w.md");
        let o = Path::new("/s/logs/w.pipeline/1-work.md");
        let a = step_argv(&step(Permission::ReadOnly), Kind::Claude, "w", r, o, "P");
        assert_eq!(&a[..4], ["claude", "-p", "--output-format", "json"]);
        let has = |a: &[String], pair: [&str; 2]| a.windows(2).any(|w| w == pair);
        assert!(has(&a, ["--model", "opus"]));
        assert!(has(&a, ["--permission-mode", "plan"]));
        assert!(has(&a, ["-n", "w-work"]));
        assert_eq!(a.last().unwrap(), "P");
        let a = step_argv(&step(Permission::Auto), Kind::Claude, "w", r, o, "P");
        assert!(has(&a, ["--permission-mode", "auto"]));
    }

    #[test]
    fn codex_steps_write_their_last_message_and_get_a_sandbox() {
        let r = Path::new("/s/reports/w.md");
        let o = Path::new("/s/logs/w.pipeline/1-work.md");
        let has = |a: &[String], pair: [&str; 2]| a.windows(2).any(|w| w == pair);
        let a = step_argv(&step(Permission::ReadOnly), Kind::Codex, "w", r, o, "P");
        assert_eq!(&a[..2], ["codex", "exec"]);
        assert!(has(
            &a,
            ["--output-last-message", "/s/logs/w.pipeline/1-work.md"]
        ));
        assert!(has(&a, ["-m", "gpt-5.5"]));
        assert!(has(&a, ["-s", "read-only"]));
        assert!(!a.contains(&"--add-dir".to_string()));
        let a = step_argv(&step(Permission::Auto), Kind::Codex, "w", r, o, "P");
        assert!(has(&a, ["-s", "workspace-write"]));
        assert!(has(&a, ["--add-dir", "/s"]));
        assert_eq!(a.last().unwrap(), "P");
    }

    #[test]
    fn output_comes_from_claude_json_or_codex_last_message() {
        let ok = r#"{"type":"result","is_error":false,"result":" the brief ","session_id":"s1"}"#;
        assert_eq!(
            output(Kind::Claude, ok, None),
            (Some("the brief".into()), json!("s1"))
        );
        let err = r#"{"is_error":true,"result":"boom","session_id":"s2"}"#;
        assert_eq!(output(Kind::Claude, err, None).0, None);
        assert_eq!(output(Kind::Claude, "not json", None).0, None);
        assert_eq!(
            output(Kind::Codex, "", Some("plan\n")).0,
            Some("plan".into())
        );
        assert_eq!(output(Kind::Codex, "", Some("  ")).0, None);
    }

    #[test]
    fn a_fallback_report_never_claims_done_without_saying_so() {
        assert_eq!(fallback_report("did things"), "partial\n\ndid things\n");
        assert_eq!(fallback_report("done\nall good"), "done\nall good\n");
        assert!(has_status("**partial**\nx"));
        assert!(!has_status("I am done"));
    }

    #[test]
    fn list_shows_each_preset_and_its_steps() {
        let out = list(&presets(None).unwrap());
        assert!(out.contains("brief: "));
        assert!(out.contains("brief (read-only, claude sonnet) → work (auto, claude sonnet)"));
    }
}
