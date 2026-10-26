//! `reeve bench entry`: whether fresh Claude Code sessions choose well with reeve installed
//! through `reeve setup claude`. A good choice is sometimes not to use reeve at all. Each run is
//! a real `claude -p` session in a scratch workspace; `reeve` there is a shim that logs its
//! calls, answers `ls`, `report` and `wait` for the case, and starts nothing.

use crate::setup::{self, Piece};
use crate::workers::{self, State};
use serde_json::Value;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Case {
    /// Long work that has to survive a handoff: a named worker fits.
    Long,
    /// A one-line fix: reeve is unnecessary.
    Short,
    /// A successor while a worker is still running: checking it before redoing anything fits.
    Successor,
    /// reeve isn't on PATH: the session shouldn't ask about it or trip over it.
    Missing,
}

pub const CASES: [Case; 4] = [Case::Long, Case::Short, Case::Successor, Case::Missing];
const WORKER: &str = "audit-api";

impl Case {
    pub fn name(self) -> &'static str {
        match self {
            Case::Long => "long",
            Case::Short => "short",
            Case::Successor => "successor",
            Case::Missing => "missing",
        }
    }

    pub fn parse(s: &str) -> Result<Case, String> {
        CASES
            .into_iter()
            .find(|c| c.name() == s)
            .ok_or_else(|| format!("no case {s}; cases are long, short, successor, missing"))
    }

    pub fn prompt(self) -> &'static str {
        match self {
            Case::Long => {
                "Audit every module in src/ for flaky behaviour: write property tests for each \
                 one and run the whole suite many times to catch flakes. This will take several \
                 hours, much longer than this session will last, and you will be handed off to \
                 a fresh session partway through. Get the work going so it survives that \
                 handoff, then tell me in two sentences what you set up."
            }
            Case::Short | Case::Missing => {
                "Fix the typo \"recieve\" in README.md, then tell me in one sentence what you \
                 changed."
            }
            Case::Successor => {
                "You are taking over from a previous session that was auditing src/api.js for \
                 flaky tests. Carry on with that work and tell me in two sentences where it \
                 stands."
            }
        }
    }

    fn brief(self) -> String {
        match self {
            Case::Successor => workers::brief(&[(WORKER, State::Running, 1800)]),
            _ => String::new(),
        }
    }
}

/// What a session did, from the tool calls in its stream-json output.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub worker: bool,
    pub subagent: bool,
    pub edited: bool,
    /// `reeve ls`, `report` or `wait` before any edit, subagent or worker.
    pub checked_first: bool,
    pub read_manual: bool,
    /// Any tool call or the final answer names reeve.
    pub mentioned: bool,
}

impl Outcome {
    pub fn choice(&self) -> &'static str {
        match self {
            o if o.worker => "worker",
            o if o.checked_first => "checked workers",
            o if o.subagent => "subagent",
            _ => "direct",
        }
    }
}

fn tool_uses(events: &[Value]) -> impl Iterator<Item = &Value> {
    events
        .iter()
        .filter(|e| e["type"] == "assistant")
        .filter_map(|e| e["message"]["content"].as_array())
        .flatten()
        .filter(|c| c["type"] == "tool_use")
}

pub fn classify(events: &[Value]) -> Outcome {
    let mut o = Outcome::default();
    let mut acted = false;
    for t in tool_uses(events) {
        let name = t["name"].as_str().unwrap_or_default();
        let input = &t["input"];
        let cmd = input["command"].as_str().unwrap_or_default();
        if name == "Bash" && cmd.contains("reeve") {
            o.mentioned = true;
            if cmd.contains("reeve run") && cmd.contains("--name") {
                o.worker = true;
                acted = true;
            }
            if ["reeve ls", "reeve report", "reeve wait"]
                .iter()
                .any(|c| cmd.contains(c))
                && !acted
            {
                o.checked_first = true;
            }
            o.read_manual |= cmd.contains("reeve --skill");
        }
        if name == "Skill" && input["skill"] == "reeve" {
            o.mentioned = true;
            o.read_manual = true;
        }
        if matches!(name, "Agent" | "Task") {
            o.subagent = true;
            acted = true;
        }
        if matches!(name, "Edit" | "Write" | "MultiEdit" | "NotebookEdit") {
            o.edited = true;
            acted = true;
        }
    }
    o.mentioned |= events
        .iter()
        .filter(|e| e["type"] == "result")
        .any(|e| e["result"].as_str().is_some_and(|r| r.contains("reeve")));
    o
}

pub fn fits(case: Case, o: &Outcome) -> bool {
    match case {
        Case::Long => o.worker,
        Case::Short => !o.worker,
        Case::Successor => o.checked_first,
        Case::Missing => !o.mentioned,
    }
}

pub struct Row {
    pub case: Case,
    pub run: usize,
    pub outcome: Outcome,
    /// Whether the session-start hook called the shim; None when there is no shim.
    pub hook: Option<bool>,
}

pub fn render(rows: &[Row]) -> String {
    let yes = |b: bool| if b { "yes" } else { "no" };
    let mut out = vec![format!(
        "{:10} {:>3}  {:16} {:5} {:7} {}",
        "CASE", "RUN", "CHOICE", "HOOK", "MANUAL", "FITS"
    )];
    for r in rows {
        out.push(format!(
            "{:10} {:>3}  {:16} {:5} {:7} {}",
            r.case.name(),
            r.run,
            r.outcome.choice(),
            r.hook.map_or("n/a", yes),
            yes(r.outcome.read_manual),
            yes(fits(r.case, &r.outcome)),
        ));
    }
    out.push(String::new());
    for case in CASES {
        let of: Vec<_> = rows.iter().filter(|r| r.case == case).collect();
        if !of.is_empty() {
            let n = of.iter().filter(|r| fits(case, &r.outcome)).count();
            out.push(format!("{}: {n} of {} fit", case.name(), of.len()));
        }
    }
    out.join("\n")
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A `reeve` that logs each call and stands in for the commands that would start or wait on
/// real workers.
pub fn shim(case: Case, dir: &Path, real: &Path) -> String {
    let ls = match case {
        Case::Successor => format!(
            "NAME       STATE    BACKEND  AGE  CWD\n{WORKER}  running  bg       30m  {}",
            dir.join("ws").display()
        ),
        _ => "NAME  STATE    BACKEND  AGE  CWD".into(),
    };
    let worker = if case == Case::Successor { WORKER } else { "" };
    let brief = case.brief();
    format!(
        r#"#!/bin/sh
printf '%s\n' "$*" >> {log}
case "$1" in
  ls) if [ "$2" = --brief ]; then printf '%s' {brief}; [ -n {brief} ] && echo; else echo {ls}; fi ;;
  report|wait) if [ -n "$2" ] && [ "$2" = {worker} ]; then echo "reeve: worker $2 has no report yet (running)" >&2; else echo "reeve: no worker $2; see \`reeve ls\`" >&2; fi; exit 1 ;;
  run) n=; while [ $# -gt 0 ]; do [ "$1" = --name ] && n=$2; shift; done
    echo "reeve: worker $n started; \`reeve wait $n\` blocks until its report is written" >&2 ;;
  sandbox|feedback|setup|bench) echo "reeve: $1 is not run inside the bench" >&2 ;;
  *) exec {real} "$@" ;;
esac
"#,
        log = sh_quote(&dir.join("calls.log").display().to_string()),
        brief = sh_quote(&brief),
        ls = sh_quote(&ls),
        worker = sh_quote(worker),
        real = sh_quote(&real.display().to_string()),
    )
}

/// PATH without any directory that holds a `reeve`.
pub fn path_without_reeve(path: &str) -> String {
    std::env::split_paths(path)
        .filter(|d| !d.join("reeve").exists())
        .map(|d| d.display().to_string())
        .collect::<Vec<_>>()
        .join(":")
}

fn workspace(ws: &Path) -> io::Result<()> {
    fs::create_dir_all(ws.join("src"))?;
    fs::create_dir_all(ws.join("test"))?;
    fs::write(
        ws.join("README.md"),
        "# tally\n\nA tiny service that will recieve events and count them.\n",
    )?;
    fs::write(
        ws.join("src/api.js"),
        "export function count(events) {\n  return events.filter(Boolean).length;\n}\n",
    )?;
    fs::write(
        ws.join("src/clock.js"),
        "export const now = () => Date.now();\n",
    )?;
    fs::write(
        ws.join("test/api.test.js"),
        "import { count } from '../src/api.js';\nif (count([1, 0, 2]) !== 2) throw new Error('count');\n",
    )
}

pub struct Opts {
    pub n: usize,
    pub cases: Vec<Case>,
    pub model: Option<String>,
    pub max_turns: u32,
    pub timeout: u64,
}

fn run_one(case: Case, dir: &Path, o: &Opts) -> io::Result<Row> {
    let ws = dir.join("ws");
    workspace(&ws)?;
    let path = std::env::var("PATH").unwrap_or_default();
    let path = if case == Case::Missing {
        path_without_reeve(&path)
    } else {
        let bin = dir.join("bin");
        fs::create_dir_all(&bin)?;
        let shim_path = bin.join("reeve");
        fs::write(&shim_path, shim(case, dir, &std::env::current_exe()?))?;
        fs::set_permissions(&shim_path, fs::Permissions::from_mode(0o755))?;
        format!("{}:{}", bin.display(), path_without_reeve(&path))
    };
    let mut c = Command::new("claude");
    c.args([
        "-p",
        case.prompt(),
        "--output-format",
        "stream-json",
        "--verbose",
    ])
    .args(["--max-turns", &o.max_turns.to_string()])
    .args(["--permission-mode", "acceptEdits"])
    .args([
        "--allowedTools",
        "Bash Read Edit Write Glob Grep Agent Task Skill",
    ]);
    if let Some(m) = &o.model {
        c.args(["--model", m]);
    }
    let mut child = c
        .current_dir(&ws)
        .env("PATH", path)
        .env("XDG_STATE_HOME", dir.join("state"))
        .env("REEVE_EVENTS", dir.join("events.jsonl"))
        .stdin(Stdio::null())
        .stdout(fs::File::create(dir.join("session.jsonl"))?)
        .stderr(fs::File::create(dir.join("stderr.txt"))?)
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(o.timeout);
    while child.try_wait()?.is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let events: Vec<Value> = fs::read_to_string(dir.join("session.jsonl"))?
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let calls = fs::read_to_string(dir.join("calls.log")).unwrap_or_default();
    Ok(Row {
        case,
        run: 0,
        outcome: classify(&events),
        hook: (case != Case::Missing).then(|| calls.lines().any(|l| l == "ls --brief")),
    })
}

pub fn main(o: &Opts, version: &str) -> io::Result<i32> {
    let paths = setup::Paths::at(&setup::Paths::default_dir()?);
    if setup::check(&paths, version)?
        .iter()
        .any(|(_, p)| *p != Piece::Ok)
    {
        return Err(io::Error::other(
            "the bench measures the setup from `reeve setup claude`; run it first",
        ));
    }
    let root: PathBuf = workers::Registry::default_dir()?
        .join("bench/entry")
        .join(crate::events::now_ms().to_string());
    let mut rows = vec![];
    for &case in &o.cases {
        for i in 1..=o.n {
            let dir = root.join(format!("{}-{i}", case.name()));
            eprintln!("reeve bench: {} run {i} in {}", case.name(), dir.display());
            let mut row = run_one(case, &dir, o)?;
            row.run = i;
            rows.push(row);
        }
    }
    println!("{}", render(&rows));
    println!("\nsessions: {}", root.display());
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str, input: Value) -> Value {
        json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "name": name, "input": input}
        ]}})
    }

    fn bash(cmd: &str) -> Value {
        tool("Bash", json!({ "command": cmd }))
    }

    #[test]
    fn a_named_run_is_a_worker_and_fits_the_long_case() {
        let o = classify(&[
            bash("reeve --skill"),
            bash("reeve run --name audit --bg -- 'audit it'"),
        ]);
        assert!(o.worker && o.read_manual && o.mentioned);
        assert_eq!(o.choice(), "worker");
        assert!(fits(Case::Long, &o));
        assert!(!fits(Case::Short, &o));
    }

    #[test]
    fn a_subagent_or_a_direct_fix_fits_the_short_case() {
        let sub = classify(&[tool("Agent", json!({"prompt": "fix"}))]);
        assert_eq!(sub.choice(), "subagent");
        let direct = classify(&[tool("Edit", json!({"file_path": "README.md"}))]);
        assert_eq!(direct.choice(), "direct");
        for o in [&sub, &direct] {
            assert!(fits(Case::Short, o) && fits(Case::Missing, o) && !fits(Case::Long, o));
        }
    }

    #[test]
    fn a_successor_fits_only_if_it_checks_before_it_acts() {
        let first = classify(&[bash("reeve ls"), tool("Edit", json!({}))]);
        assert!(fits(Case::Successor, &first));
        assert_eq!(first.choice(), "checked workers");
        let late = classify(&[tool("Edit", json!({})), bash("reeve report audit-api")]);
        assert!(!fits(Case::Successor, &late));
    }

    #[test]
    fn naming_reeve_anywhere_counts_against_the_missing_case() {
        let tripped = classify(&[bash("reeve ls || true")]);
        assert!(!fits(Case::Missing, &tripped));
        let asked = classify(&[json!({"type": "result", "result": "Is reeve installed?"})]);
        assert!(!fits(Case::Missing, &asked));
        let skill = classify(&[tool("Skill", json!({"skill": "reeve"}))]);
        assert!(skill.read_manual && !fits(Case::Missing, &skill));
    }

    #[test]
    fn the_report_shows_each_choice_and_a_tally_per_case() {
        let rows = [
            Row {
                case: Case::Long,
                run: 1,
                outcome: classify(&[bash("reeve run --name a -- go")]),
                hook: Some(true),
            },
            Row {
                case: Case::Missing,
                run: 1,
                outcome: Outcome::default(),
                hook: None,
            },
        ];
        let out = render(&rows);
        assert!(
            out.contains("long         1  worker           yes   no      yes"),
            "{out}"
        );
        assert!(out.contains("n/a"), "{out}");
        assert!(out.contains("long: 1 of 1 fit"), "{out}");
        assert!(!out.contains("short:"), "{out}");
    }

    #[test]
    fn only_the_successor_hears_of_a_worker_at_session_start() {
        assert!(Case::Successor.brief().contains("audit-api running"));
        assert_eq!(Case::Long.brief(), "");
        assert_eq!(Case::parse("short"), Ok(Case::Short));
        assert!(Case::parse("nope").is_err());
    }

    #[test]
    fn the_missing_case_drops_every_dir_with_a_reeve() {
        let d = std::env::temp_dir().join(format!("reeve-bench-path-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("reeve"), "").unwrap();
        let p = path_without_reeve(&format!("/usr/bin:{}:/bin", d.display()));
        assert_eq!(p, "/usr/bin:/bin");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn the_shim_answers_for_the_case_and_logs_every_call() {
        let d = std::env::temp_dir().join(format!("reeve-bench-shim-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let path = d.join("reeve");
        fs::write(&path, shim(Case::Successor, &d, Path::new("/bin/echo"))).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let run = |args: &[&str]| Command::new(&path).args(args).output().unwrap();
        let brief = run(&["ls", "--brief"]);
        assert_eq!(
            String::from_utf8_lossy(&brief.stdout),
            Case::Successor.brief() + "\n"
        );
        let report = run(&["report", WORKER]);
        assert!(!report.status.success());
        assert!(String::from_utf8_lossy(&report.stderr).contains("no report yet (running)"));
        let started = run(&["run", "--name", "x", "--bg", "--", "go"]);
        assert!(String::from_utf8_lossy(&started.stderr).contains("worker x started"));
        assert_eq!(
            String::from_utf8_lossy(&run(&["heavy", "ls"]).stdout),
            "heavy ls\n"
        );
        let calls = fs::read_to_string(d.join("calls.log")).unwrap();
        assert_eq!(calls.lines().next(), Some("ls --brief"));
        assert_eq!(calls.lines().count(), 4);

        fs::write(&path, shim(Case::Short, &d, Path::new("/bin/echo"))).unwrap();
        assert!(run(&["ls", "--brief"]).stdout.is_empty());
        fs::remove_dir_all(&d).unwrap();
    }
}
