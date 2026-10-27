use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn executable(path: &Path, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A `claude` and a `herdr` on PATH that log the directory they were started in and their
/// arguments to `<name>.log`, and answer the way reeve expects.
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
    executable(
        &bin.join("herdr"),
        &format!(
            "echo \"$*\" >> {}\ncase \"$1 $2\" in\n\
             'tab create') echo '{{\"result\":{{\"tab\":{{\"tab_id\":\"t1\"}},\"root_pane\":{{\"pane_id\":\"p1\"}}}}}}' ;;\n\
             *) echo '{{\"result\":{{}}}}' ;;\nesac",
            log("herdr")
        ),
    );
    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap())
}

fn reeve(dir: &Path, from: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_reeve"))
        .args(args)
        .current_dir(from)
        .env("PATH", fakes(dir))
        .env("XDG_STATE_HOME", dir.join("state"))
        .env("REEVE_EVENTS", dir.join("events.jsonl"))
        .env_remove("HERDR_ENV")
        .env_remove("HERDR_WORKSPACE_ID")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn read(path: PathBuf) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
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
