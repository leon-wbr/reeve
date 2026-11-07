use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

fn executable(path: &Path, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A `claude` that answers `-p --output-format json` for the step its `-n` names: it keeps the
/// prompt in `prompt-<label>`, writes the report when the prompt asks for one, and prints
/// "output of <label>" as its result; it exits 1 while `fail-<label>` exists. A `codex` that
/// does the same with its last message in the `--output-last-message` file.
fn fakes(dir: &Path) -> String {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let d = dir.display();
    let report = "f=$(printf '%s' \"$last\" | sed -n 's/.*Markdown to \\([^ ]*\\.md\\)\\..*/\\1/p'); \
                  if [ -n \"$f\" ]; then printf 'done\\nfrom %s\\n' \"$label\" > \"$f\"; fi";
    executable(
        &bin.join("claude"),
        &format!(
            "echo \"$*\" >> {d}/claude.log\nlabel=x\nprev=\nfor a; do [ \"$prev\" = -n ] && label=$a; prev=$a; last=$a; done\n\
             printf '%s' \"$last\" > {d}/prompt-$label\n\
             [ -f {d}/fail-$label ] && exit 1\n{report}\n\
             printf '{{\"type\":\"result\",\"is_error\":false,\"result\":\"output of %s\",\"session_id\":\"s-%s\"}}' \"$label\" \"$label\""
        ),
    );
    executable(
        &bin.join("codex"),
        &format!(
            "echo \"$*\" >> {d}/codex.log\nlabel=codex\nprev=\nfor a; do [ \"$prev\" = --output-last-message ] && out=$a; prev=$a; last=$a; done\n\
             printf '%s' \"$last\" > {d}/prompt-codex\n{report}\n\
             printf 'codex output' > \"$out\""
        ),
    );
    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap())
}

fn reeve(dir: &Path, args: &[&str]) -> Output {
    let cwd = dir.join("proj");
    std::fs::create_dir_all(&cwd).unwrap();
    Command::new(env!("CARGO_BIN_EXE_reeve"))
        .args(args)
        .current_dir(&cwd)
        .env("PATH", fakes(dir))
        .env("XDG_STATE_HOME", dir.join("state"))
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("REEVE_HOME", dir.join("reeve-home"))
        .env("REEVE_EVENTS", dir.join("events.jsonl"))
        .env_remove("HERDR_ENV")
        .env_remove("HERDR_PANE_ID")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("CLAUDECODE")
        .env_remove("CODEX_THREAD_ID")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn read(path: PathBuf) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Waits for the detached `reeve pipeline-run` to finish: its report exists and its process
/// has gone, which `reeve ls` sees as done.
fn finished(dir: &Path, name: &str) -> String {
    let report = dir.join(format!("state/reeve/reports/{name}.md"));
    for _ in 0..2400 {
        if report.is_file() {
            let ls = String::from_utf8_lossy(&reeve(dir, &["ls"]).stdout).into_owned();
            if ls
                .lines()
                .any(|l| l.starts_with(name) && l.contains("done"))
            {
                return read(report);
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("pipeline {name} never finished");
}

fn job(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("state/reeve/logs/{name}.pipeline"))
}

#[test]
fn each_step_gets_the_ask_and_the_previous_output_and_the_last_writes_the_report() {
    let t = tempfile::tempdir().unwrap();
    let out = reeve(
        t.path(),
        &[
            "run",
            "--name",
            "w",
            "--pipeline",
            "brief",
            "--",
            "fix the bug",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(finished(t.path(), "w"), "done\nfrom w-work\n");

    let brief = read(t.path().join("prompt-w-brief"));
    assert!(brief.contains("<ask>\nfix the bug\n</ask>"), "{brief}");
    assert!(!brief.contains("<previous"));
    assert!(!brief.contains("Markdown to"));
    let work = read(t.path().join("prompt-w-work"));
    assert!(
        work.contains("<previous step=\"brief\">\noutput of w-brief\n</previous>"),
        "{work}"
    );
    assert!(work.contains("Markdown to"));

    let log = read(t.path().join("claude.log"));
    assert!(log.contains("--permission-mode plan"), "{log}");
    assert!(log.contains("--permission-mode auto"), "{log}");

    let steps: Vec<Value> = read(job(t.path(), "w").join("steps.jsonl"))
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0]["step"], "brief");
    assert_eq!(steps[0]["session"], "s-w-brief");
    assert_eq!(steps[1]["exit"], 0);
    assert_eq!(
        read(job(t.path(), "w").join("1-brief.md")),
        "output of w-brief"
    );

    let all: Value =
        serde_json::from_str(&read(t.path().join("state/reeve/workers.json"))).unwrap();
    assert_eq!(all[0]["backend"], "pipeline");
    assert_eq!(all[0]["pipeline"], "brief");
}

#[test]
fn a_failing_step_ends_the_pipeline_with_a_failed_report_naming_it() {
    let t = tempfile::tempdir().unwrap();
    std::fs::write(t.path().join("fail-w-brief"), "").unwrap();
    let out = reeve(
        t.path(),
        &["run", "--name", "w", "--pipeline", "brief", "--", "go"],
    );
    assert!(out.status.success());
    let report = finished(t.path(), "w");
    assert!(report.starts_with("failed\n"), "{report}");
    assert!(report.contains("step 1 of 2, brief"), "{report}");
    assert!(!t.path().join("prompt-w-work").exists());
}

#[test]
fn codex_steps_pass_their_last_message_on() {
    let t = tempfile::tempdir().unwrap();
    let cfg = t.path().join("config/reeve");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(
        cfg.join("pipelines.json"),
        r#"{"two": {"about": "x", "steps": [
            {"name": "look", "agent": "codex", "permission": "read-only", "prompt": "Look."},
            {"name": "work", "permission": "auto", "prompt": "Work."}]}}"#,
    )
    .unwrap();
    let out = reeve(
        t.path(),
        &["run", "--name", "c", "--pipeline", "two", "--", "go"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(finished(t.path(), "c"), "done\nfrom c-work\n");
    assert!(read(t.path().join("codex.log")).contains("-s read-only"));
    let work = read(t.path().join("prompt-c-work"));
    assert!(
        work.contains("<previous step=\"look\">\ncodex output\n</previous>"),
        "{work}"
    );
}

#[test]
fn agent_codex_runs_steps_that_name_no_agent_in_codex() {
    let t = tempfile::tempdir().unwrap();
    let out = reeve(
        t.path(),
        &[
            "run",
            "--name",
            "c",
            "--agent",
            "codex",
            "--pipeline",
            "plain",
            "--",
            "go",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(finished(t.path(), "c"), "done\nfrom codex\n");
    assert!(read(t.path().join("codex.log")).contains("-s workspace-write"));
    assert!(!t.path().join("claude.log").exists());
}

#[test]
fn an_unknown_preset_or_extra_flags_are_refused_before_anything_starts() {
    let t = tempfile::tempdir().unwrap();
    let out = reeve(
        t.path(),
        &["run", "--name", "w", "--pipeline", "nope", "--", "go"],
    );
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("no pipeline preset \"nope\"") && err.contains("brief"),
        "{err}"
    );
    let out = reeve(
        t.path(),
        &[
            "run",
            "--name",
            "w",
            "--pipeline",
            "plain",
            "--",
            "--model",
            "opus",
            "go",
        ],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("only its prompt"));
    assert!(
        !t.path().join("state/reeve/workers.json").exists() || {
            let s = read(t.path().join("state/reeve/workers.json"));
            !s.contains("\"w\"")
        }
    );
}

#[test]
fn pipelines_lists_built_in_and_user_presets() {
    let t = tempfile::tempdir().unwrap();
    let cfg = t.path().join("config/reeve");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(
        cfg.join("pipelines.json"),
        r#"{"mine": {"about": "my own", "steps": [{"name": "a", "permission": "auto", "prompt": "p"}]}}"#,
    )
    .unwrap();
    let out = reeve(t.path(), &["pipelines"]);
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    for name in ["plain:", "brief:", "plan:", "review:", "mine: my own"] {
        assert!(s.contains(name), "{s}");
    }
}
