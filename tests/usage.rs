//! `reeve usage` against a fake `codex app-server` on PATH and a Claude statusline snapshot.

use serde_json::Value;
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn executable(path: &Path, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// A `codex` that answers `app-server` like Codex 0.166 does: it reads requests on stdin and
/// answers `account/rateLimits/read` (id 2) once `initialize` came first. It logs its arguments.
fn fake_codex(dir: &Path, five: u64, week: u64) {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let reset = now() + 3600;
    executable(
        &bin.join("codex"),
        &format!(
            "echo \"$*\" >> {log}\n\
             [ \"$1\" = app-server ] || exit 2\n\
             while read -r line; do\n\
               case \"$line\" in\n\
                 *'\"method\":\"initialize\"'*) echo '{{\"id\":1,\"result\":{{}}}}' ;;\n\
                 *'account/rateLimits/read'*) echo '{{\"id\":2,\"result\":{{\"rateLimits\":{{\"limitId\":\"codex\",\"primary\":{{\"usedPercent\":{five},\"windowDurationMins\":300,\"resetsAt\":{reset}}},\"secondary\":{{\"usedPercent\":{week},\"windowDurationMins\":10080,\"resetsAt\":{reset}}},\"planType\":\"plus\"}}}}}}' ;;\n\
               esac\n\
             done",
            log = dir.join("codex.log").display()
        ),
    );
}

fn reeve(dir: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_reeve"));
    c.args(args)
        .env_clear()
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", dir.join("bin").display()),
        )
        .env("HOME", dir.join("home"))
        .env("XDG_STATE_HOME", dir.join("state"))
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    if let Some(s) = stdin {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
    }
    child.wait_with_output().unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn record_then_usage_prints_a_line_per_agent_and_json_for_scripts() {
    let t = tempfile::tempdir().unwrap();
    fake_codex(t.path(), 12, 40);
    let reset = now() + 7200;
    let payload = format!(
        r#"{{"model":{{"display_name":"Opus"}},"rate_limits":{{"five_hour":{{"used_percentage":34,"resets_at":{reset}}},"seven_day":{{"used_percentage":89,"resets_at":{reset}}}}}}}"#
    );
    let rec = reeve(t.path(), &["usage", "record"], Some(&payload));
    assert!(rec.status.success(), "{rec:?}");
    assert_eq!(out(&rec), "", "the statusline prints its own line");

    let o = reeve(t.path(), &["usage"], None);
    assert!(o.status.success(), "{o:?}");
    let text = out(&o);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(
        lines[0].starts_with("claude") && lines[0].contains("5h 34%"),
        "{text}"
    );
    assert!(
        lines[0].contains("week 89%") && lines[0].contains("resets in"),
        "{text}"
    );
    assert!(
        lines[1].starts_with("codex") && lines[1].contains("5h 12%"),
        "{text}"
    );
    assert!(lines[1].contains("week 40%"), "{text}");
    let log = std::fs::read_to_string(t.path().join("codex.log")).unwrap();
    assert_eq!(log.trim(), "app-server");

    let j = reeve(t.path(), &["usage", "--json"], None);
    let v: Value = serde_json::from_slice(&j.stdout).unwrap();
    assert_eq!(v[0]["agent"], "claude");
    assert_eq!(v[0]["five_hour"]["percent"], 34.0);
    assert_eq!(v[1]["agent"], "codex");
    assert_eq!(v[1]["weekly"]["percent"], 40.0);
    assert!(v[1]["read_at"].as_str().unwrap().ends_with('Z'));
}

#[test]
fn max_exits_non_zero_only_when_a_window_is_over_it() {
    let t = tempfile::tempdir().unwrap();
    fake_codex(t.path(), 12, 40);
    let under = reeve(
        t.path(),
        &["usage", "--agent", "codex", "--max", "50"],
        None,
    );
    assert_eq!(under.status.code(), Some(0), "{under:?}");
    let over = reeve(
        t.path(),
        &["usage", "--agent", "codex", "--max", "30"],
        None,
    );
    assert_eq!(over.status.code(), Some(1), "{over:?}");
    assert!(out(&over).contains("week 40%"));
    assert!(String::from_utf8_lossy(&over.stderr).contains("codex weekly 40% is over 30%"));
}

#[test]
fn what_cannot_be_read_is_unknown_with_its_reason_and_still_exits_0() {
    let t = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(t.path().join("bin")).unwrap();
    let o = reeve(t.path(), &["usage", "--max", "10"], None);
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    let text = out(&o);
    assert!(
        text.contains("claude  unknown: no statusline snapshot"),
        "{text}"
    );
    assert!(text.contains("reeve usage record"), "{text}");
    assert!(
        text.contains("codex   unknown: codex is not on PATH"),
        "{text}"
    );
}
