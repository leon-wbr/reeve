use serde_json::Value;
use std::path::Path;
use std::process::{Command, Output, Stdio};

/// reeve with its state, event log and actor confined to `dir`, run as a person.
fn reeve(dir: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_reeve"));
    c.args(args)
        .env("XDG_STATE_HOME", dir)
        .env("REEVE_HEAVY_DIR", dir.join("heavy"))
        .env_remove("REEVE_EVENTS")
        .env_remove("REEVE_HEAVY")
        .env_remove("REEVE_HEAVY_SLOTS")
        .env_remove("CLAUDECODE")
        .env_remove("AI_AGENT")
        .env_remove("CODEX_SANDBOX")
        .env_remove("CODEX_SANDBOX_NETWORK_DISABLED")
        .env_remove("BROWSER")
        .stdin(Stdio::null());
    c
}

fn run(dir: &Path, args: &[&str]) -> Output {
    reeve(dir, args).output().unwrap()
}

fn log(dir: &Path) -> Vec<Value> {
    std::fs::read_to_string(dir.join("reeve/events.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn last(dir: &Path) -> Value {
    log(dir).pop().expect("an event")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn a_heavy_command_logs_its_label_outcome_and_durations() {
    let d = tempfile::tempdir().unwrap();
    let o = run(
        d.path(),
        &["heavy", "--label", "tests", "--", "sh", "-c", "exit 3"],
    );
    assert_eq!(o.status.code(), Some(3));
    let e = last(d.path());
    assert_eq!(e["command"], "heavy");
    assert_eq!(e["label"], "tests");
    assert_eq!(e["exit"], 3);
    assert_eq!(e["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(e["actor"], "person");
    assert!(e["ts"].as_u64().unwrap() > 1_700_000_000_000);
    assert!(
        e["heavy_wait_ms"].is_u64() && e["heavy_run_ms"].is_u64(),
        "{e}"
    );
    assert!(e.get("error").is_none());
}

#[test]
fn without_a_label_heavy_logs_only_the_program_name() {
    let d = tempfile::tempdir().unwrap();
    run(
        d.path(),
        &["heavy", "--", "/bin/sh", "-c", "echo secret-arg"],
    );
    let e = last(d.path());
    assert_eq!(e["label"], "sh");
    assert!(!e.to_string().contains("secret-arg"), "{e}");
}

#[test]
fn a_failed_command_logs_its_error_kind_and_nothing_else_of_the_message() {
    let d = tempfile::tempdir().unwrap();
    let o = run(d.path(), &["wait", "nobody"]);
    assert!(!o.status.success());
    let e = last(d.path());
    assert_eq!(e["command"], "wait");
    assert_eq!(e["worker"], "nobody");
    assert_eq!(e["exit"], 1);
    assert_eq!(e["error"], "no_worker");
}

#[test]
fn a_usage_error_is_logged_by_its_subcommand_only() {
    let d = tempfile::tempdir().unwrap();
    let o = run(
        d.path(),
        &["heavy", "--slots", "0", "--", "rm", "-rf", "/x"],
    );
    assert_eq!(o.status.code(), Some(2));
    let e = last(d.path());
    assert_eq!(e["command"], "heavy");
    assert_eq!(e["error"], "usage");
    assert!(!e.to_string().contains("rm"), "{e}");
    run(d.path(), &["--help"]);
    run(d.path(), &["--version"]);
    assert_eq!(log(d.path()).len(), 1, "help and version aren't logged");
}

#[test]
fn an_agent_is_told_apart_from_a_person() {
    let d = tempfile::tempdir().unwrap();
    reeve(d.path(), &["ls"])
        .env("CLAUDECODE", "1")
        .output()
        .unwrap();
    let e = last(d.path());
    assert_eq!(e["command"], "ls");
    assert_eq!(e["actor"], "agent");
    assert_eq!(e["agent"], "claude-code");
}

#[test]
fn reeve_events_moves_the_log() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("elsewhere.jsonl");
    reeve(d.path(), &["heavy", "ls"])
        .env("REEVE_EVENTS", &f)
        .output()
        .unwrap();
    let raw = std::fs::read_to_string(&f).unwrap();
    assert!(raw.contains(r#""command":"heavy ls""#), "{raw}");
    assert!(log(d.path()).is_empty());
}

#[test]
fn a_log_that_cant_be_written_warns_and_keeps_the_commands_outcome() {
    let d = tempfile::tempdir().unwrap();
    let o = reeve(d.path(), &["heavy", "--", "sh", "-c", "exit 4"])
        .env("REEVE_EVENTS", d.path())
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(4));
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(err.matches("reeve: could not log").count(), 1, "{err}");
}

#[test]
fn concurrent_commands_each_append_one_whole_line() {
    let d = tempfile::tempdir().unwrap();
    let children: Vec<_> = (0..16)
        .map(|i| {
            reeve(d.path(), &["heavy", "--slots", "16", "--label"])
                .args([format!("job{i}"), "--".into(), "true".into()])
                .spawn()
                .unwrap()
        })
        .collect();
    for c in children {
        assert!(c.wait_with_output().unwrap().status.success());
    }
    let mut labels: Vec<String> = log(d.path())
        .iter()
        .map(|e| e["label"].as_str().unwrap().to_string())
        .collect();
    labels.sort();
    let mut want: Vec<String> = (0..16).map(|i| format!("job{i}")).collect();
    want.sort();
    assert_eq!(labels, want);
}

#[test]
fn stats_sums_up_the_log() {
    let d = tempfile::tempdir().unwrap();
    run(d.path(), &["heavy", "--", "true"]);
    run(d.path(), &["heavy", "--", "true"]);
    run(d.path(), &["wait", "nobody"]);
    reeve(d.path(), &["ls"])
        .env("CLAUDECODE", "1")
        .output()
        .unwrap();
    let o = run(d.path(), &["stats", "--json"]);
    assert!(o.status.success(), "{o:?}");
    let s: Value = serde_json::from_str(&stdout(&o)).unwrap();
    assert_eq!(s["days"], 7);
    assert_eq!(s["commands"], 4);
    assert_eq!(s["heavy"]["runs"], 2);
    assert!(s["heavy"]["wait_ms"]["p50"].is_u64(), "{s}");
    assert_eq!(s["errors"]["no_worker"], 1);
    assert_eq!(s["actors"]["person"], 3);
    assert_eq!(s["actors"]["agent"], 1);
    assert_eq!(s["agents"]["claude-code"], 1);
    let text = stdout(&run(d.path(), &["stats"]));
    assert!(text.contains("heavy: 2 runs"), "{text}");
    assert!(text.contains("no_worker 1"), "{text}");
    assert_eq!(last(d.path())["command"], "stats");
}

#[test]
fn feedback_is_saved_locally_with_context_and_redacted_paths() {
    let d = tempfile::tempdir().unwrap();
    run(
        d.path(),
        &["heavy", "--label", "/Users/x/secret", "--", "true"],
    );
    let o = run(d.path(), &["feedback", "wait hangs on idle panes"]);
    assert!(o.status.success(), "{o:?}");
    let dir = d.path().join("reeve/feedback");
    let files: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
    assert_eq!(files.len(), 1);
    let note = std::fs::read_to_string(files[0].as_ref().unwrap().path()).unwrap();
    assert!(note.contains("wait hangs on idle panes"), "{note}");
    assert!(note.contains(env!("CARGO_PKG_VERSION")), "{note}");
    assert!(note.contains(std::env::consts::OS), "{note}");
    assert!(note.contains(r#""command":"heavy""#), "{note}");
    assert!(!note.contains("/Users/x/secret"), "{note}");
    assert!(stdout(&o).contains("feedback/"), "prints where it went");
    let e = last(d.path());
    assert_eq!(e["command"], "feedback");
    assert!(!e.to_string().contains("hangs"), "{e}");
}

#[test]
fn feedback_keeps_paths_when_asked() {
    let d = tempfile::tempdir().unwrap();
    run(
        d.path(),
        &["heavy", "--label", "/Users/x/secret", "--", "true"],
    );
    run(d.path(), &["feedback", "--with-paths", "note"]);
    let f = std::fs::read_dir(d.path().join("reeve/feedback"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(
        std::fs::read_to_string(f)
            .unwrap()
            .contains("/Users/x/secret")
    );
}

fn executable(path: &Path, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A `gh` on PATH that logs its arguments to `gh.log`; `auth status` succeeds when `authed`.
fn fake_gh(dir: &Path, authed: bool) -> String {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let log = dir.join("gh.log");
    executable(
        &bin.join("gh"),
        &format!(
            "echo \"$*\" >> {}\n[ \"$1 $2\" = 'auth status' ] && exit {}\n\
             echo https://github.com/leon-wbr/reeve/issues/99",
            log.display(),
            if authed { 0 } else { 1 }
        ),
    );
    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap())
}

fn gh_log(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("gh.log")).unwrap_or_default()
}

#[test]
fn run_by_an_agent_feedback_prints_the_ready_issue_and_posts_nothing() {
    let d = tempfile::tempdir().unwrap();
    let path = fake_gh(d.path(), true);
    let o = reeve(d.path(), &["feedback", "ls is slow & odd"])
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(o.status.success(), "{o:?}");
    let out = stdout(&o);
    assert!(out.contains("repo: leon-wbr/reeve"), "{out}");
    assert!(out.contains("title: ls is slow & odd"), "{out}");
    assert!(out.contains("# reeve feedback"), "the body: {out}");
    assert!(
        out.contains("gh issue create --repo leon-wbr/reeve"),
        "{out}"
    );
    assert!(
        out.contains(
            "https://github.com/leon-wbr/reeve/issues/new?title=ls%20is%20slow%20%26%20odd&body="
        ),
        "{out}"
    );
    assert!(!gh_log(d.path()).contains("issue"), "never posts");
}

#[test]
fn feedback_open_opens_the_prefilled_new_issue_page() {
    let d = tempfile::tempdir().unwrap();
    let opened = d.path().join("opened");
    let browser = d.path().join("browser");
    executable(
        &browser,
        &format!("printf '%s' \"$1\" > {}", opened.display()),
    );
    let o = reeve(d.path(), &["feedback", "--open", "ls is slow"])
        .env("BROWSER", &browser)
        .output()
        .unwrap();
    assert!(o.status.success(), "{o:?}");
    let url = std::fs::read_to_string(&opened).unwrap();
    assert!(
        url.starts_with("https://github.com/leon-wbr/reeve/issues/new?title=ls%20is%20slow&body="),
        "{url}"
    );
    assert!(url.len() <= 8000);
}

/// `reeve feedback` in a terminal (`script` gives it one), answering the prompt with `answer`.
#[cfg(target_os = "macos")]
fn in_terminal(dir: &Path, path: &str, answer: &str) -> String {
    let mut c = Command::new("script");
    c.args([
        "-q",
        "/dev/null",
        env!("CARGO_BIN_EXE_reeve"),
        "feedback",
        "it hangs",
    ])
    .env("XDG_STATE_HOME", dir)
    .env("PATH", path)
    .env_remove("REEVE_EVENTS")
    .env_remove("CLAUDECODE")
    .env_remove("BROWSER")
    .stdin(Stdio::piped())
    .stdout(Stdio::piped());
    let mut child = c.spawn().unwrap();
    use std::io::{Read, Write};
    let mut out = child.stdout.take().unwrap();
    let mut seen = Vec::new();
    let mut buf = [0; 4096];
    // Input typed before the prompt can be lost, so the answer waits for it.
    while !String::from_utf8_lossy(&seen).contains("[y/N]") {
        match out.read(&mut buf).unwrap() {
            0 => break,
            n => seen.extend_from_slice(&buf[..n]),
        }
    }
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(answer.as_bytes()).unwrap();
    out.read_to_end(&mut seen).unwrap();
    drop(stdin);
    child.wait().unwrap();
    String::from_utf8_lossy(&seen).into_owned()
}

#[cfg(target_os = "macos")]
#[test]
fn in_a_terminal_with_gh_feedback_posts_only_after_a_yes() {
    let d = tempfile::tempdir().unwrap();
    let path = fake_gh(d.path(), true);
    let out = in_terminal(d.path(), &path, "n\n");
    assert!(
        out.contains("title: it hangs") && out.contains("[y/N]"),
        "{out}"
    );
    assert!(!gh_log(d.path()).contains("issue create"), "{out}");
    assert!(
        out.contains("issues/new?title="),
        "falls back to the URL: {out}"
    );
    let out = in_terminal(d.path(), &path, "y\n");
    let log = gh_log(d.path());
    assert!(
        log.contains("issue create --repo leon-wbr/reeve --title it hangs --body-file"),
        "{log}\n{out}"
    );
    assert!(out.contains("issues/99"), "{out}");
}

#[cfg(target_os = "macos")]
#[test]
fn in_a_terminal_without_gh_feedback_prints_the_url() {
    let d = tempfile::tempdir().unwrap();
    let path = fake_gh(d.path(), false);
    let out = in_terminal(d.path(), &path, "");
    assert!(!out.contains("[y/N]"), "{out}");
    assert!(out.contains("issues/new?title=it%20hangs&body="), "{out}");
}

#[test]
fn the_context_hook_answers_from_the_transcript_and_stays_out_of_the_log() {
    use std::io::Write;
    let d = tempfile::tempdir().unwrap();
    let transcript = d.path().join("t.jsonl");
    let usage = r#"{"message":{"usage":{"input_tokens":5,"cache_read_input_tokens":2000}}}"#;
    std::fs::write(&transcript, usage).unwrap();
    std::fs::create_dir_all(d.path().join("cfg/reeve")).unwrap();
    std::fs::write(
        d.path().join("cfg/reeve/config.json"),
        r#"{"handoff_tokens": 1000}"#,
    )
    .unwrap();
    let hook = |payload: &str, env: Option<&str>| {
        let mut c = reeve(d.path(), &["hook", "context"]);
        c.env("XDG_CONFIG_HOME", d.path().join("cfg"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        match env {
            Some(v) => c.env("REEVE_HANDOFF_TOKENS", v),
            None => c.env_remove("REEVE_HANDOFF_TOKENS"),
        };
        let mut child = c.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    };
    let prompt = serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "transcript_path": transcript,
    })
    .to_string();
    let o = hook(&prompt, None);
    assert!(o.status.success());
    let out: Value = serde_json::from_str(&stdout(&o)).unwrap();
    let notice = out["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(
        notice.contains("2K tokens, past the 1K handoff threshold"),
        "{notice}"
    );
    assert_eq!(stdout(&hook(&prompt, Some("5000"))), "");
    let bad = hook("not json", None);
    assert!(bad.status.success());
    assert_eq!(stdout(&bad), "");
    assert!(log(d.path()).is_empty());
}
