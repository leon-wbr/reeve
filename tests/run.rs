use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn executable(path: &Path, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A `claude` and a `herdr` on PATH that log the directory they were started in and their
/// arguments to `<name>.log`, and answer the way reeve expects. `herdr pane get` answers with
/// `pane.json` when it exists, and otherwise with a pane that has no agent. `herdr agent start`
/// waits while `hold` exists, fails if `fail` does, and fails with the error code in
/// `start-error` if that does.
fn fakes(dir: &Path) -> String {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let log = |n: &str| dir.join(format!("{n}.log")).display().to_string();
    executable(
        &bin.join("claude"),
        &format!(
            "pwd -P >> {}\necho \"$*\" >> {0}\necho 'backgrounded · abc123 · w'",
            log("claude")
        ),
    );
    // Prints exec's first event, then writes the report when its prompt asks for `report`.
    executable(
        &bin.join("codex"),
        &format!(
            "pwd -P >> {}\necho \"$*\" >> {0}\nenv | grep -c '^CLAUDECODE=' >> {0}\n\
             echo '{{\"type\":\"thread.started\",\"thread_id\":\"th-1\"}}'\n\
             for a; do last=$a; done\n\
             case \"$last\" in report*) f=$(echo \"$last\" | sed 's/.*Markdown to \\([^ ]*\\.md\\)\\..*/\\1/'); echo done > \"$f\" ;; esac",
            log("codex")
        ),
    );
    executable(
        &bin.join("herdr"),
        &format!(
            "echo \"$*\" >> {}\ncase \"$1 $2\" in\n\
             'tab create') echo '{{\"result\":{{\"tab\":{{\"tab_id\":\"t1\"}},\"root_pane\":{{\"pane_id\":\"p1\"}}}}}}' ;;\n\
             'pane get') if [ -f {pane} ]; then cat {pane}; else echo '{{\"result\":{{}}}}'; fi ;;\n\
             'agent start') while [ -f {hold} ]; do sleep 0.05; done\n\
               if [ -f {fail} ]; then echo '{{\"error\":{{\"code\":\"boom\",\"message\":\"no shell\"}}}}' >&2; exit 1; fi\n\
               if [ -f {err} ]; then printf '{{\"error\":{{\"code\":\"%s\",\"message\":\"m\"}}}}\\n' \"$(cat {err})\" >&2; exit 1; fi\n\
               echo '{{\"result\":{{}}}}' ;;\n\
             *) echo '{{\"result\":{{}}}}' ;;\nesac",
            log("herdr"),
            pane = dir.join("pane.json").display(),
            hold = dir.join("hold").display(),
            fail = dir.join("fail").display(),
            err = dir.join("start-error").display()
        ),
    );
    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap())
}

fn reeve(dir: &Path, from: &Path, args: &[&str]) -> Output {
    reeve_with(dir, from, args, &[])
}

fn reeve_with(dir: &Path, from: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_reeve"))
        .args(args)
        .current_dir(from)
        .env("PATH", fakes(dir))
        .env("XDG_STATE_HOME", dir.join("state"))
        .env("REEVE_EVENTS", dir.join("events.jsonl"))
        .env_remove("HERDR_ENV")
        .env_remove("HERDR_WORKSPACE_ID")
        .env_remove("CLAUDECODE")
        .env_remove("CODEX_THREAD_ID")
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn read(path: PathBuf) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Waits for the `reeve start` that `reeve run` left in the background to bring the worker up
/// or fail, and returns its record.
fn started(dir: &Path, name: &str) -> Value {
    for _ in 0..400 {
        let all: Value =
            serde_json::from_str(&read(dir.join("state/reeve/workers.json"))).unwrap_or_default();
        let w = all
            .as_array()
            .and_then(|a| a.iter().find(|w| w["name"] == name))
            .cloned();
        if let Some(w) = w.filter(|w| w.get("starting").is_none()) {
            return w;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    panic!("worker {name} never finished starting");
}

fn worker_cwd(dir: &Path, name: &str) -> String {
    let v: Value = serde_json::from_str(&read(dir.join("state/reeve/workers.json"))).unwrap();
    v.as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == name)
        .map(|w| w["cwd"].as_str().unwrap().to_string())
        .unwrap()
}

/// A project dir that is a git repository, and somewhere else to start from.
fn dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().canonicalize().unwrap();
    let project = root.join("project");
    let elsewhere = root.join("elsewhere");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    let git = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&project)
        .status()
        .unwrap();
    assert!(git.success());
    (d, project, elsewhere)
}

#[test]
fn a_background_worker_starts_in_the_cwd_it_is_given() {
    let (d, project, elsewhere) = dirs();
    let root = d.path().canonicalize().unwrap();
    let o = reeve(
        &root,
        &elsewhere,
        &[
            "run",
            "--name",
            "w",
            "--bg",
            "--cwd",
            "../project",
            "--",
            "go",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(started(&root, "w")["handle"], "abc123");
    let log = read(root.join("claude.log"));
    assert_eq!(log.lines().next(), Some(project.to_str().unwrap()));
    assert_eq!(worker_cwd(&root, "w"), project.to_str().unwrap());
    assert!(!String::from_utf8_lossy(&o.stderr).contains("git repository"));
}

#[test]
fn a_pane_worker_opens_its_tab_in_the_cwd_it_is_given() {
    let (d, project, elsewhere) = dirs();
    let root = d.path().canonicalize().unwrap();
    let o = reeve(
        &root,
        &elsewhere,
        &[
            "run",
            "--name",
            "w",
            "--pane",
            "--cwd",
            project.to_str().unwrap(),
            "--",
            "go",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(started(&root, "w")["handle"], "p1");
    let create = read(root.join("herdr.log"));
    let create = create.lines().next().unwrap();
    assert!(
        create.contains(&format!("--cwd {} ", project.display())),
        "{create}"
    );
    assert_eq!(worker_cwd(&root, "w"), project.to_str().unwrap());
}

#[test]
fn a_worker_outside_any_git_repository_is_started_with_a_warning() {
    let (d, _project, elsewhere) = dirs();
    let root = d.path().canonicalize().unwrap();
    let o = reeve(
        &root,
        &elsewhere,
        &["run", "--name", "w", "--bg", "--", "go"],
    );
    assert!(o.status.success());
    started(&root, "w");
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("not in a git repository"), "{err}");
    assert!(err.contains("--cwd"), "{err}");
    assert_eq!(worker_cwd(&root, "w"), elsewhere.to_str().unwrap());
}

#[test]
fn a_cwd_that_does_not_exist_starts_nothing() {
    let (d, _project, elsewhere) = dirs();
    let root = d.path().canonicalize().unwrap();
    let o = reeve(
        &root,
        &elsewhere,
        &["run", "--name", "w", "--bg", "--cwd", "nowhere", "--", "go"],
    );
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("nowhere"));
    assert_eq!(read(root.join("claude.log")), "");
}

#[test]
fn inside_herdr_a_background_worker_is_refused_and_nothing_starts() {
    let (d, project, _elsewhere) = dirs();
    let root = d.path().canonicalize().unwrap();
    let o = reeve_with(
        &root,
        &project,
        &["run", "--name", "w", "--bg", "--", "go"],
        &[("HERDR_ENV", "1")],
    );
    assert_eq!(o.status.code(), Some(2));
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(err.lines().count(), 1, "{err}");
    assert!(
        err.contains("own Herdr tab") && err.contains("drop --bg"),
        "{err}"
    );
    assert!(err.contains("--force"), "{err}");
    assert_eq!(read(root.join("claude.log")), "");
    assert_eq!(read(root.join("herdr.log")), "");
    assert_eq!(read(root.join("state/reeve/workers.json")), "");
}

#[test]
fn inside_herdr_a_worker_without_bg_gets_its_own_tab() {
    let (d, project, _elsewhere) = dirs();
    let root = d.path().canonicalize().unwrap();
    let o = reeve_with(
        &root,
        &project,
        &["run", "--name", "w", "--", "go"],
        &[("HERDR_ENV", "1")],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    started(&root, "w");
    assert!(read(root.join("herdr.log")).starts_with("tab create"));
}

#[test]
fn inside_herdr_a_background_successor_is_refused_and_nothing_starts() {
    let (d, project, _elsewhere) = dirs();
    let root = d.path().canonicalize().unwrap();
    std::fs::write(project.join("n.md"), "Goal: ship it.").unwrap();
    let args = ["handoff", "--name", "next", "--notes", "n.md", "--bg"];
    let o = reeve_with(&root, &project, &args, &[("HERDR_ENV", "1")]);
    assert_eq!(o.status.code(), Some(2));
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(err.lines().count(), 1, "{err}");
    assert!(err.contains("a successor gets its own Herdr tab"), "{err}");
    assert!(
        err.contains("drop --bg") && err.contains("--force"),
        "{err}"
    );
    assert_eq!(read(root.join("claude.log")), "");
    assert_eq!(read(root.join("herdr.log")), "");
    assert!(!root.join("state/reeve/handoffs/next.md").exists());

    let forced = [&args[..], &["--force"]].concat();
    let o = reeve_with(&root, &project, &forced, &[("HERDR_ENV", "1")]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    started(&root, "next");
    assert!(read(root.join("claude.log")).contains("--bg"));
    assert_eq!(read(root.join("herdr.log")), "");
}

#[test]
fn a_started_worker_says_how_to_follow_it_in_two_lines() {
    let (d, project, _elsewhere) = dirs();
    let root = d.path().canonicalize().unwrap();
    let o = reeve(&root, &project, &["run", "--name", "w", "--bg", "--", "go"]);
    assert!(o.status.success());
    started(&root, "w");
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(err.lines().count(), 2, "{err}");
    assert!(err.contains("follow it with `reeve watch w`"), "{err}");
    assert!(err.contains("`reeve close w`"), "{err}");
}

/// Writes a registry of pane workers, each with handle `p-NAME`.
fn registry(root: &Path, names: &[&str]) {
    let state = root.join("state/reeve");
    std::fs::create_dir_all(state.join("reports")).unwrap();
    let all: Vec<Value> = names
        .iter()
        .map(|n| {
            serde_json::json!({
                "name": n,
                "backend": "pane",
                "cwd": root,
                "started": 1,
                "report": state.join(format!("reports/{n}.md")),
                "handle": format!("p-{n}"),
            })
        })
        .collect();
    std::fs::write(state.join("workers.json"), Value::Array(all).to_string()).unwrap();
}

fn report(root: &Path, name: &str) {
    std::fs::write(root.join(format!("state/reeve/reports/{name}.md")), "done").unwrap();
}

#[test]
fn a_named_watch_reports_final_states_once_and_exits() {
    let (d, project, _elsewhere) = dirs();
    let root = d.path().canonicalize().unwrap();
    // The fake herdr knows no pane, so a worker without a report is gone.
    registry(&root, &["a", "b"]);
    report(&root, "a");
    report(&root, "b");
    let o = reeve(&root, &project, &["watch", "a", "b"]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let out = String::from_utf8_lossy(&o.stdout);
    assert_eq!(out.lines().count(), 2, "{out}");
    assert!(out.contains("a done: report at "), "{out}");
    assert!(out.contains("b done: report at "), "{out}");

    std::fs::remove_file(root.join("state/reeve/reports/b.md")).unwrap();
    let o = reeve(&root, &project, &["watch", "a", "b"]);
    assert_eq!(o.status.code(), Some(1));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("a done: report at "), "{out}");
    assert!(out.contains("b gone without a report"), "{out}");

    let o = reeve(&root, &project, &["watch", "a", "nope"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("no worker nope"));
}

#[test]
fn watch_prints_the_report_being_written() {
    let (d, project, _elsewhere) = dirs();
    let root = d.path().canonicalize().unwrap();
    registry(&root, &["a"]);
    std::fs::write(
        root.join("pane.json"),
        r#"{"result":{"pane":{"agent":"claude","agent_status":"working"}}}"#,
    )
    .unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_reeve"))
        .args(["watch", "a"])
        .current_dir(&project)
        .env("PATH", fakes(&root))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("REEVE_EVENTS", root.join("events.jsonl"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A poll asks herdr first and looks for the report after, so the report goes in once the
    // second poll has begun: only then has the first one surely seen the worker running.
    let polls = || read(root.join("herdr.log")).matches("pane get p-a").count();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while polls() < 2 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(polls() >= 2, "watch never polled herdr twice");
    report(&root, "a");
    let o = child.wait_with_output().unwrap();
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let out = String::from_utf8_lossy(&o.stdout);
    assert_eq!(out.lines().count(), 1, "{out}");
    assert!(out.starts_with("a done: report at "), "{out}");
}

#[test]
fn inside_herdr_force_starts_a_background_worker_anyway() {
    let (d, project, _elsewhere) = dirs();
    let root = d.path().canonicalize().unwrap();
    let o = reeve_with(
        &root,
        &project,
        &["run", "--name", "w", "--bg", "--force", "--", "go"],
        &[("HERDR_ENV", "1")],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    started(&root, "w");
    assert!(read(root.join("claude.log")).contains("--bg"));
    assert_eq!(read(root.join("herdr.log")), "");
}

fn ls_state(root: &Path, project: &Path, name: &str) -> String {
    let o = reeve(root, project, &["ls"]);
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    out.lines()
        .find_map(|l| l.strip_prefix(&format!("{name} ")))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("no {name} in {out}"))
        .to_string()
}

#[test]
fn a_run_returns_while_its_worker_starts_and_ls_shows_it_starting_then_running() {
    let (d, project, _elsewhere) = dirs();
    let root = d.path().canonicalize().unwrap();
    std::fs::write(root.join("hold"), "").unwrap();
    let o = reeve(
        &root,
        &project,
        &["run", "--name", "w", "--pane", "--", "go"],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stderr).contains("worker w is starting"));
    // The tab is open and labelled while claude is still on its way.
    assert!(read(root.join("herdr.log")).starts_with("tab create --label w "));
    assert_eq!(ls_state(&root, &project, "w"), "starting");
    let again = reeve(
        &root,
        &project,
        &["run", "--name", "w", "--pane", "--", "go"],
    );
    assert!(!again.status.success());
    assert!(String::from_utf8_lossy(&again.stderr).contains("still starting"));
    let rm = reeve(&root, &project, &["rm", "w"]);
    assert!(!rm.status.success());

    std::fs::write(
        root.join("pane.json"),
        r#"{"result":{"pane":{"agent":"claude","agent_status":"working"}}}"#,
    )
    .unwrap();
    std::fs::remove_file(root.join("hold")).unwrap();
    assert_eq!(started(&root, "w")["handle"], "p1");
    assert_eq!(ls_state(&root, &project, "w"), "running");
}

#[test]
fn a_start_that_fails_leaves_the_worker_gone_with_the_reason() {
    let (d, project, _elsewhere) = dirs();
    let root = d.path().canonicalize().unwrap();
    std::fs::write(root.join("fail"), "").unwrap();
    let o = reeve(
        &root,
        &project,
        &["run", "--name", "w", "--pane", "--", "go"],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let w = started(&root, "w");
    assert!(w["error"].as_str().unwrap().contains("no shell"), "{w}");
    assert!(read(root.join("herdr.log")).contains("tab close t1"));
    assert_eq!(ls_state(&root, &project, "w"), "gone");

    let o = reeve(&root, &project, &["wait", "w"]);
    assert_eq!(o.status.code(), Some(1));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("worker w failed to start: "), "{err}");
    assert!(err.contains("no shell"), "{err}");

    let o = reeve(&root, &project, &["rm", "w"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

fn ls_line(root: &Path, project: &Path, name: &str) -> String {
    let o = reeve(root, project, &["ls"]);
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    out.lines()
        .find(|l| l.starts_with(&format!("{name} ")))
        .unwrap_or_else(|| panic!("no {name} in {out}"))
        .to_string()
}

/// The fake codex's log once it holds its first three lines: a background codex is recorded as
/// started as soon as it is spawned.
fn codex_log(root: &Path) -> String {
    // A deadline, not a sleep: it returns as soon as the log is there, and a busy machine
    // (parallel test binaries, `reeve heavy` neighbours) can take well over 5 s to spawn.
    for _ in 0..2400 {
        let log = read(root.join("codex.log"));
        if log.lines().count() >= 3 {
            return log;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    panic!("codex never logged its start");
}

fn wait_for(path: &Path) {
    for _ in 0..2400 {
        if path.exists() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    panic!("{} never appeared", path.display());
}

#[test]
fn a_background_codex_worker_runs_exec_and_is_done_once_it_reports() {
    let (d, project, _elsewhere) = dirs();
    let root = d.path().canonicalize().unwrap();
    let args = [
        "run",
        "--name",
        "cx",
        "--bg",
        "--agent",
        "codex",
        "--",
        "-m",
        "o9",
        "report it",
    ];
    let o = reeve_with(&root, &project, &args, &[("CLAUDECODE", "1")]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let w = started(&root, "cx");
    assert_eq!(w["agent"], "codex");
    assert!(w["handle"].as_str().unwrap().parse::<u32>().is_ok(), "{w}");
    wait_for(&root.join("state/reeve/reports/cx.md"));
    let log = codex_log(&root);
    let mut lines = log.lines();
    assert_eq!(lines.next(), Some(project.to_str().unwrap()));
    let argv = lines.next().unwrap();
    assert!(
        argv.starts_with("exec --json --skip-git-repo-check -m o9 -s workspace-write --add-dir "),
        "{argv}"
    );
    let state = root.join("state/reeve");
    assert!(
        argv.contains(&format!("--add-dir {} report it", state.display())),
        "{argv}"
    );
    assert_eq!(lines.next(), Some("0"), "CLAUDECODE reached codex");
    assert!(read(state.join("logs/cx.jsonl")).contains("th-1"));
    let line = ls_line(&root, &project, "cx");
    assert!(
        line.contains(" done ") && line.contains(" codex ") && line.contains(" bg "),
        "{line}"
    );
    assert_eq!(read(root.join("claude.log")), "");
}

#[test]
fn a_codex_worker_that_exits_without_a_report_is_gone_with_how_to_resume_it() {
    let (d, project, _elsewhere) = dirs();
    let root = d.path().canonicalize().unwrap();
    let args = [
        "run", "--name", "cx", "--bg", "--agent", "codex", "--", "go",
    ];
    let o = reeve(&root, &project, &args);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    started(&root, "cx");
    let o = reeve(&root, &project, &["wait", "cx", "--timeout", "20"]);
    assert_eq!(o.status.code(), Some(1));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("worker cx is gone without writing its report"),
        "{err}"
    );
    assert!(
        err.contains("`codex exec resume th-1 \"MESSAGE\"`"),
        "{err}"
    );
    assert!(ls_line(&root, &project, "cx").contains(" gone "));
}

#[test]
fn a_codex_pane_worker_is_started_as_codex_and_followed_as_codex() {
    let (d, project, _elsewhere) = dirs();
    let root = d.path().canonicalize().unwrap();
    std::fs::write(
        root.join("pane.json"),
        r#"{"result":{"pane":{"agent":"codex","agent_status":"blocked"}}}"#,
    )
    .unwrap();
    let args = [
        "run", "--name", "cx", "--pane", "--agent", "codex", "--", "go",
    ];
    let o = reeve(&root, &project, &args);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(started(&root, "cx")["handle"], "p1");
    let herdr = read(root.join("herdr.log"));
    let start = herdr
        .lines()
        .find(|l| l.starts_with("agent start"))
        .unwrap();
    assert!(
        start.starts_with("agent start cx --kind codex --pane p1 "),
        "{start}"
    );
    assert!(
        start.contains(" -- -s workspace-write --add-dir "),
        "{start}"
    );
    assert!(ls_line(&root, &project, "cx").contains(" blocked "));
}

#[test]
fn a_worker_that_stops_at_a_question_on_start_keeps_its_tab_and_shows_blocked() {
    let (d, project, _elsewhere) = dirs();
    let root = d.path().canonicalize().unwrap();
    std::fs::write(root.join("start-error"), "agent_not_ready").unwrap();
    std::fs::write(
        root.join("pane.json"),
        r#"{"result":{"pane":{"agent":"codex","agent_status":"blocked"}}}"#,
    )
    .unwrap();
    let args = [
        "run", "--name", "cx", "--pane", "--agent", "codex", "--", "go",
    ];
    let o = reeve(&root, &project, &args);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let w = started(&root, "cx");
    assert!(w.get("error").is_none(), "{w}");
    assert!(!read(root.join("herdr.log")).contains("tab close"));
    assert!(ls_line(&root, &project, "cx").contains(" blocked "));
}

#[test]
fn a_codex_that_claims_its_pane_before_herdr_is_done_is_found_there_and_named() {
    let (d, project, _elsewhere) = dirs();
    let root = d.path().canonicalize().unwrap();
    std::fs::write(root.join("start-error"), "agent_name_not_found").unwrap();
    std::fs::write(
        root.join("pane.json"),
        r#"{"result":{"pane":{"agent":"codex","agent_status":"working"}}}"#,
    )
    .unwrap();
    let args = [
        "run", "--name", "cx", "--pane", "--agent", "codex", "--", "go",
    ];
    let o = reeve(&root, &project, &args);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let w = started(&root, "cx");
    assert!(w.get("error").is_none(), "{w}");
    let herdr = read(root.join("herdr.log"));
    assert!(herdr.contains("agent rename p1 cx"), "{herdr}");
    assert!(!herdr.contains("tab close"), "{herdr}");
    assert!(ls_line(&root, &project, "cx").contains(" running "));
}

#[test]
fn codex_without_a_name_runs_in_the_foreground_but_never_in_the_background_or_a_sandbox() {
    let (d, project, _elsewhere) = dirs();
    let root = d.path().canonicalize().unwrap();
    let o = reeve(&root, &project, &["run", "--agent", "codex", "--", "hi"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(read(root.join("codex.log")).lines().nth(1), Some("hi"));
    let bg = reeve(
        &root,
        &project,
        &["run", "--agent", "codex", "--bg", "--", "hi"],
    );
    assert!(!bg.status.success());
    assert!(String::from_utf8_lossy(&bg.stderr).contains("pass --name"));
    let sb = ["run", "--agent", "codex", "--sandbox", "--", "hi"];
    assert!(!reeve(&root, &project, &sb).status.success());
}

fn commit(project: &Path) {
    let args = [
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "init",
    ];
    let ok = Command::new("git")
        .args(args)
        .current_dir(project)
        .status()
        .unwrap();
    assert!(ok.success());
}

#[test]
fn a_worktree_worker_starts_in_its_own_worktree_under_the_main_checkout() {
    let (d, project, _elsewhere) = dirs();
    let root = d.path().canonicalize().unwrap();
    commit(&project);
    let args = [
        "run",
        "--name",
        "wt",
        "--bg",
        "--worktree",
        "--agent",
        "codex",
        "--",
        "go",
    ];
    let o = reeve(&root, &project, &args);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    started(&root, "wt");
    let tree = project.join(".claude/worktrees/wt");
    assert_eq!(worker_cwd(&root, "wt"), tree.to_str().unwrap());
    assert_eq!(codex_log(&root).lines().next(), tree.to_str());
    let branch = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(&tree)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&branch.stdout).trim(),
        "worktree-wt"
    );

    // Once that worker is gone, a restart under the same name, even from inside the worktree,
    // uses it again.
    let o = reeve(&root, &project, &["wait", "wt", "--timeout", "20"]);
    assert_eq!(o.status.code(), Some(1));
    let o = reeve(&root, &tree, &args);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    started(&root, "wt");
    assert_eq!(worker_cwd(&root, "wt"), tree.to_str().unwrap());
}

#[test]
fn a_handoff_from_a_codex_session_starts_a_codex_successor() {
    let (d, project, _elsewhere) = dirs();
    let root = d.path().canonicalize().unwrap();
    std::fs::write(project.join("n.md"), "Goal: ship it.").unwrap();
    let args = ["handoff", "--name", "next", "--notes", "n.md", "--bg"];
    let o = reeve_with(&root, &project, &args, &[("CODEX_THREAD_ID", "th-0")]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(started(&root, "next")["agent"], "codex");
    assert!(codex_log(&root).contains("exec --json"));
    assert_eq!(read(root.join("claude.log")), "");

    let to_claude = [&args[..], &["--agent", "claude"]].concat();
    let to_claude: Vec<&str> = to_claude
        .iter()
        .map(|a| if *a == "next" { "next2" } else { a })
        .collect();
    let o = reeve_with(&root, &project, &to_claude, &[("CODEX_THREAD_ID", "th-0")]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(started(&root, "next2").get("agent").is_none());
    assert!(read(root.join("claude.log")).contains("--bg"));
}
