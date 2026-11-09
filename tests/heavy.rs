use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

fn heavy(state: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_reeve"));
    c.arg("heavy")
        .args(args)
        .env("REEVE_HEAVY_DIR", state)
        .env("REEVE_EVENTS", state.join("events.jsonl"))
        .stdin(Stdio::null())
        .env_remove("REEVE_HEAVY")
        .env_remove("REEVE_HEAVY_SLOTS")
        .env_remove("REEVE_HEAVY_TIMEOUT")
        .env_remove("REEVE_HEAVY_MIN_FREE")
        .env_remove("REEVE_HEAVY_EXPRESS")
        // No worker registry and no inbox outside the test's dir.
        .env("XDG_STATE_HOME", state.join("xdg"))
        .env("REEVE_HOME", state.join("home"))
        .env_remove("HERDR_PANE_ID")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("CODEX_THREAD_ID");
    c
}

/// The pid `reeve heavy ls` shows for the run or waiter labelled `label`: its runner's.
fn pid_of(state: &Path, label: &str) -> i32 {
    let ls = stdout(&run(state, &["ls"]));
    ls.lines()
        .find(|l| l.split_whitespace().nth(1) == Some(label))
        .and_then(|l| {
            l.split("pid ")
                .nth(1)?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
        .unwrap_or_else(|| panic!("no {label} in:\n{ls}"))
}

fn run(state: &Path, args: &[&str]) -> Output {
    heavy(state, args).output().unwrap()
}

fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

fn wait_for(what: &str, ok: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ok() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Ticket or slot files in `state/<dir>`, counted without `reeve heavy ls`, which would clean up
/// after a dead owner anyway.
fn files(state: &Path, dir: &str) -> usize {
    std::fs::read_dir(state.join(dir)).map_or(0, |rd| {
        rd.filter(|e| {
            e.as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|x| x == "json")
        })
        .count()
    })
}

/// Waits for the child like `wait_with_output`, but kills it and fails after `secs`.
fn finish(mut c: Child, secs: u64) -> Output {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while c.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            c.kill().unwrap();
            panic!("still running after {secs}s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    c.wait_with_output().unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// SIGINT, which the caller passes on to the job's runner, so the command goes down with it.
/// SIGTERM or SIGKILL would only stop the following.
fn stop(c: Child) {
    unsafe { libc::kill(c.id() as i32, libc::SIGINT) };
    c.wait_with_output().unwrap();
}

/// Holds the only slot until the returned child is killed.
fn hold(state: &Path, label: &str) -> Child {
    let busy = state.join(format!("{label}.started"));
    let child = heavy(state, &["--slots", "1", "--label", label, "--", "sh", "-c"])
        .arg(format!("touch {}; sleep 30", busy.display()))
        .spawn()
        .unwrap();
    wait_for("the holder to start", || busy.exists());
    child
}

#[test]
fn runs_the_command_and_exits_with_its_code() {
    let d = tempfile::tempdir().unwrap();
    let o = run(d.path(), &["--", "sh", "-c", "echo hi; exit 3"]);
    assert_eq!(o.status.code(), Some(3));
    assert_eq!(stdout(&o), "hi\n");
    assert!(o.stderr.is_empty(), "quiet when nothing waits");
}

#[test]
fn the_command_sees_reeve_heavy_set() {
    let d = tempfile::tempdir().unwrap();
    let o = run(d.path(), &["--", "sh", "-c", "echo $REEVE_HEAVY"]);
    assert_eq!(stdout(&o), "1\n");
}

#[test]
fn the_command_runs_with_the_callers_env_path_and_cwd() {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let here = d.path().canonicalize().unwrap().join("project");
    let bin = d.path().join("bin");
    std::fs::create_dir_all(&here).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("tool"), "#!/bin/sh\necho pinned\n").unwrap();
    std::fs::set_permissions(bin.join("tool"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let o = heavy(
        d.path(),
        &["--", "sh", "-c", "tool; echo $PROJECT_PIN; pwd -P"],
    )
    .current_dir(&here)
    .env("PATH", &path)
    .env("PROJECT_PIN", "1.4.0")
    .output()
    .unwrap();
    assert_eq!(
        stdout(&o),
        format!("pinned\n1.4.0\n{}\n", here.display()),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
fn a_nested_call_skips_the_queue() {
    let d = tempfile::tempdir().unwrap();
    let holder = hold(d.path(), "outer");
    let c = heavy(d.path(), &["--slots", "1", "--", "echo", "inner"])
        .env("REEVE_HEAVY", "1")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    assert_eq!(stdout(&finish(c, 5)), "inner\n");
    stop(holder);
}

#[test]
fn one_slot_means_one_command_at_a_time() {
    let d = tempfile::tempdir().unwrap();
    let lock = d.path().join("busy");
    let script = format!("mkdir {0} || exit 99; sleep 0.1; rmdir {0}", lock.display());
    let children: Vec<Child> = (0..8)
        .map(|i| {
            heavy(d.path(), &["--slots", "1", "--label", &format!("job{i}")])
                .args(["--", "sh", "-c", &script])
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    for c in children {
        assert_eq!(c.wait_with_output().unwrap().status.code(), Some(0));
    }
}

#[test]
fn waiters_say_how_many_are_ahead_and_ls_shows_them_in_order() {
    let d = tempfile::tempdir().unwrap();
    let holder = hold(d.path(), "holder");
    let mut waiters = vec![];
    for label in ["first", "second"] {
        let c = heavy(d.path(), &["--slots", "1", "--label", label, "--", "true"])
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        waiters.push(c);
        wait_for("the waiter to queue", || {
            stdout(&run(d.path(), &["ls"])).contains(label)
        });
    }
    let ls = stdout(&run(d.path(), &["ls"]));
    let at = |s: &str| {
        ls.find(s)
            .unwrap_or_else(|| panic!("{s} missing from:\n{ls}"))
    };
    assert!(at("running") < at("holder"));
    assert!(at("holder") < at("waiting"));
    assert!(at("waiting") < at("first") && at("first") < at("second"));
    stop(holder);
    let second = waiters.pop().unwrap().wait_with_output().unwrap();
    let first = waiters.pop().unwrap().wait_with_output().unwrap();
    assert!(first.status.success() && second.status.success());
    assert!(String::from_utf8_lossy(&second.stderr).contains("1 ahead"));
}

#[test]
fn a_run_whose_label_was_quick_lately_takes_the_express_slot() {
    let d = tempfile::tempdir().unwrap();
    for _ in 0..3 {
        assert!(
            run(d.path(), &["--label", "quick", "--", "true"])
                .status
                .success()
        );
    }
    let holder = hold(d.path(), "holder");
    let quick = heavy(
        d.path(),
        &["--slots", "1", "--label", "quick", "--", "true"],
    )
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
    let o = finish(quick, 10);
    assert!(o.status.success());
    assert!(
        o.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    // A label with no history waits for the holder, and so does a quick one with express off.
    let new = heavy(d.path(), &["--slots", "1", "--label", "new", "--", "true"])
        .spawn()
        .unwrap();
    wait_for("the new label to queue", || files(d.path(), "queue") == 1);
    let off = heavy(
        d.path(),
        &[
            "--slots",
            "1",
            "--express",
            "0",
            "--label",
            "quick",
            "--",
            "true",
        ],
    )
    .spawn()
    .unwrap();
    wait_for("the quick one to queue", || files(d.path(), "queue") == 2);
    let ls = stdout(&run(d.path(), &["ls"]));
    assert!(ls.contains("short: its last 4 runs took at most"), "{ls}");
    stop(holder);
    assert!(finish(new, 10).status.success());
    assert!(finish(off, 10).status.success());
}

#[test]
fn a_waiter_outside_a_terminal_says_once_to_run_it_in_the_background() {
    let d = tempfile::tempdir().unwrap();
    let holder = hold(d.path(), "holder");
    let first = heavy(
        d.path(),
        &["--slots", "1", "--label", "first", "--", "sleep", "0.5"],
    )
    .stderr(Stdio::null())
    .spawn()
    .unwrap();
    wait_for("the first waiter to queue", || {
        files(d.path(), "queue") == 1
    });
    let second = heavy(
        d.path(),
        &["--slots", "1", "--label", "second", "--", "true"],
    )
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
    wait_for("the second waiter to queue", || {
        files(d.path(), "queue") == 2
    });
    stop(holder);
    finish(first, 5);
    let err = String::from_utf8_lossy(&finish(second, 5).stderr).into_owned();
    assert!(err.contains("1 ahead") && err.contains("0 ahead"), "{err}");
    assert_eq!(err.matches("in the background").count(), 1, "{err}");
}

/// A shell snippet that starts `sleep 30` in the background and records its pid in `bg.pid`.
fn background_sleep(state: &Path) -> String {
    let f = state.join("bg.pid");
    format!("sleep 30 & echo $! > {0}.tmp; mv {0}.tmp {0}", f.display())
}

fn background_pid(state: &Path) -> i32 {
    let f = state.join("bg.pid");
    wait_for("the command to start", || f.exists());
    std::fs::read_to_string(f).unwrap().trim().parse().unwrap()
}

#[test]
fn the_command_group_is_killed_when_the_command_ends() {
    let d = tempfile::tempdir().unwrap();
    let script = format!("{}; exit 0", background_sleep(d.path()));
    assert!(run(d.path(), &["--", "sh", "-c", &script]).status.success());
    let pid = background_pid(d.path());
    wait_for("the orphan to die", || !alive(pid));
}

#[test]
fn stop_kills_the_command_group_and_frees_the_slot() {
    let d = tempfile::tempdir().unwrap();
    let script = format!("{}; wait", background_sleep(d.path()));
    let c = heavy(d.path(), &["--label", "job", "--", "sh", "-c", &script])
        .spawn()
        .unwrap();
    let pid = background_pid(d.path());
    let runner = pid_of(d.path(), "job");
    assert!(
        run(d.path(), &["stop", &runner.to_string()])
            .status
            .success()
    );
    assert_eq!(finish(c, 5).status.code(), Some(143));
    wait_for("the command to die", || !alive(pid));
    assert_eq!(files(d.path(), "slots"), 0);
}

#[test]
fn sigterm_to_the_caller_only_stops_following_and_the_command_runs_on() {
    let d = tempfile::tempdir().unwrap();
    let script = format!("{}; wait", background_sleep(d.path()));
    let c = heavy(d.path(), &["--label", "job", "--", "sh", "-c", &script])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = background_pid(d.path());
    unsafe { libc::kill(c.id() as i32, libc::SIGTERM) };
    let o = finish(c, 5);
    assert_eq!(o.status.code(), Some(143));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("stopped following") && err.contains("reeve heavy follow"),
        "{err}"
    );
    std::thread::sleep(Duration::from_millis(500));
    assert!(alive(pid), "the command died with its caller");
    let runner = pid_of(d.path(), "job");
    run(d.path(), &["stop", &runner.to_string()]);
    wait_for("the command to die", || !alive(pid));
    wait_for("the slot to be free", || files(d.path(), "slots") == 0);
}

#[test]
fn the_runner_is_outside_the_callers_process_tree_and_group() {
    let d = tempfile::tempdir().unwrap();
    let holder = hold(d.path(), "holder");
    let runner = pid_of(d.path(), "holder");
    let ps = |field: &str| {
        let o = Command::new("ps")
            .args(["-o", &format!("{field}="), "-p", &runner.to_string()])
            .output()
            .unwrap();
        String::from_utf8_lossy(&o.stdout)
            .trim()
            .parse::<i32>()
            .unwrap()
    };
    assert_ne!(ps("ppid"), holder.id() as i32);
    // The group setsid made, led by the parent that forked it and exited at once.
    assert_ne!(ps("pgid"), unsafe { libc::getpgrp() });
    assert_ne!(ps("pgid"), runner);
    stop(holder);
}

/// What Claude Code's low-memory reaper did to queued waiters: SIGKILL, no cleanup.
#[test]
fn a_waiter_whose_caller_is_killed_keeps_its_place_runs_and_is_reported() {
    let d = tempfile::tempdir().unwrap();
    let state = d.path();
    // A named worker in a Herdr pane, so the runner can tell it in its inbox.
    let reg = state.join("xdg/reeve");
    std::fs::create_dir_all(&reg).unwrap();
    let worker = serde_json::json!([{"name": "w1", "backend": "pane", "cwd": "/", "started": 1,
        "report": reg.join("reports/w1.md"), "handle": "p-w1"}]);
    std::fs::write(reg.join("workers.json"), worker.to_string()).unwrap();
    let holder = hold(state, "holder");
    let ran = state.join("ran");
    let mut w = heavy(
        state,
        &["--slots", "1", "--label", "waiter", "--", "sh", "-c"],
    )
    .arg(format!("echo built; touch {}; exit 3", ran.display()))
    .env("HERDR_PANE_ID", "p-w1")
    .stderr(Stdio::null())
    .spawn()
    .unwrap();
    wait_for("the waiter to queue", || files(state, "queue") == 1);
    unsafe { libc::kill(w.id() as i32, libc::SIGKILL) };
    w.wait().unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let ls = stdout(&run(state, &["ls"]));
    assert!(
        ls.lines()
            .any(|l| l.starts_with("waiting") && l.contains("waiter")),
        "{ls}"
    );
    stop(holder);
    wait_for("the waiter's command to run", || ran.exists());
    wait_for("ls to show how it ended", || {
        stdout(&run(state, &["ls"])).contains("ended")
    });
    let ls = stdout(&run(state, &["ls"]));
    let line = ls.lines().find(|l| l.starts_with("ended")).unwrap();
    assert!(line.contains("waiter") && line.contains("exit 3"), "{ls}");
    let id = line
        .split("reeve heavy follow ")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let o = run(state, &["follow", id]);
    assert_eq!(o.status.code(), Some(3));
    assert_eq!(stdout(&o), "built\n");
    assert!(
        !stdout(&run(state, &["ls"])).contains("ended"),
        "shown until looked at"
    );
    let ev = events(state);
    let e = ev.iter().find(|e| e["label"] == "waiter").unwrap();
    assert_eq!(e["exit"], 3);
    assert_eq!(e["heavy_unfollowed"], true);
    assert_eq!(e["worker"], "w1");
    let inbox: Vec<String> = std::fs::read_dir(state.join("home/inbox"))
        .unwrap()
        .map(|f| std::fs::read_to_string(f.unwrap().path()).unwrap())
        .collect();
    assert!(
        inbox
            .iter()
            .any(|m| m.contains(r#""to":"w1""#) && m.contains("exit 3") && m.contains(id)),
        "{inbox:?}"
    );
}

#[test]
fn a_followed_job_is_logged_once_and_not_reported_as_unwatched() {
    let d = tempfile::tempdir().unwrap();
    let o = run(d.path(), &["--label", "quick", "--", "true"]);
    assert_eq!(o.status.code(), Some(0));
    let ev = events(d.path());
    let heavy: Vec<_> = ev.iter().filter(|e| e["command"] == "heavy").collect();
    assert_eq!(heavy.len(), 1, "{ev:?}");
    assert!(heavy[0].get("heavy_unfollowed").is_none());
    assert_eq!(stdout(&run(d.path(), &["ls"])), "no heavy commands\n");
}

#[test]
fn below_the_memory_floor_a_second_run_waits_and_ls_says_why() {
    let d = tempfile::tempdir().unwrap();
    let started = d.path().join("first");
    // No machine has this much free, so only the first run may start.
    let floor = ["--slots", "4", "--min-free", "100000"];
    let first = heavy(d.path(), &floor)
        .args(["--label", "first", "--", "sh", "-c"])
        .arg(format!("touch {}; sleep 30", started.display()))
        .spawn()
        .unwrap();
    wait_for("the first run to start despite the floor", || {
        started.exists()
    });
    let second = heavy(d.path(), &floor)
        .args(["--label", "second", "--", "true"])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for("ls to say it waits for memory", || {
        stdout(&run(d.path(), &["ls"]))
            .lines()
            .any(|l| l.contains("second") && l.contains("memory") && l.contains("GB free"))
    });
    stop(first);
    let o = finish(second, 10);
    assert!(o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("waiting for memory") && err.contains("REEVE_HEAVY_MIN_FREE"),
        "{err}"
    );
}

/// A command that ignores SIGTERM; `sleep` inherits the ignored signal.
fn stubborn(state: &Path) -> (Child, String) {
    let started = state.join("started");
    let script = format!("trap '' TERM; touch {}; sleep 30", started.display());
    let c = heavy(state, &["--label", "stubborn", "--", "sh", "-c", &script])
        .spawn()
        .unwrap();
    wait_for("the command to start", || started.exists());
    (c, pid_of(state, "stubborn").to_string())
}

#[test]
fn a_command_that_ignores_sigterm_is_killed_after_the_grace_period() {
    let d = tempfile::tempdir().unwrap();
    let (c, runner) = stubborn(d.path());
    let t = Instant::now();
    run(d.path(), &["stop", &runner]);
    assert_eq!(finish(c, 15).status.code(), Some(143));
    assert!(t.elapsed() > Duration::from_secs(4), "{:?}", t.elapsed());
    assert_eq!(files(d.path(), "slots"), 0);
}

#[test]
fn a_second_signal_kills_the_command_at_once() {
    let d = tempfile::tempdir().unwrap();
    let (c, runner) = stubborn(d.path());
    run(d.path(), &["stop", &runner]);
    std::thread::sleep(Duration::from_millis(200));
    run(d.path(), &["stop", &runner]);
    assert_eq!(finish(c, 3).status.code(), Some(143));
}

#[test]
fn sigint_while_waiting_leaves_the_queue() {
    let d = tempfile::tempdir().unwrap();
    let holder = hold(d.path(), "holder");
    let c = heavy(
        d.path(),
        &["--slots", "1", "--label", "impatient", "--", "true"],
    )
    .stderr(Stdio::null())
    .spawn()
    .unwrap();
    wait_for("the waiter to queue", || files(d.path(), "queue") == 1);
    unsafe { libc::kill(c.id() as i32, libc::SIGINT) };
    assert_eq!(finish(c, 5).status.code(), Some(130));
    assert_eq!(files(d.path(), "queue"), 0);
    stop(holder);
}

/// Under a terminal (`script` gives it one) a command that reads from it isn't stopped by SIGTTIN.
#[cfg(target_os = "macos")]
#[test]
fn in_a_terminal_the_command_can_read_from_it() {
    let d = tempfile::tempdir().unwrap();
    let c = Command::new("script")
        .args([
            "-q",
            "/dev/null",
            env!("CARGO_BIN_EXE_reeve"),
            "heavy",
            "--",
        ])
        .args(["sh", "-c", "read x; echo read it"])
        .env("REEVE_HEAVY_DIR", d.path())
        .env("REEVE_EVENTS", d.path().join("events.jsonl"))
        .env_remove("REEVE_HEAVY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    assert!(stdout(&finish(c, 10)).contains("read it"));
}

/// Runs `script` under `--lock key`; the script marks that it is inside with a directory named
/// after the key and fails (exit 99) if another run on the same key is already inside. Slots are
/// set explicitly: the default is a quarter of the cores, so a small CI runner has one, and then
/// slots alone would serialise the runs and hide whether the lock does.
fn locked(state: &Path, key: &str, script: &str) -> Child {
    heavy(
        state,
        &[
            "--slots", "8", "--lock", key, "--label", key, "--", "sh", "-c", script,
        ],
    )
    .stderr(Stdio::null())
    .spawn()
    .unwrap()
}

#[test]
fn runs_on_the_same_lock_key_never_overlap() {
    let d = tempfile::tempdir().unwrap();
    let inside = d.path().join("inside");
    let script = format!(
        "mkdir {0} || exit 99; sleep 0.2; rmdir {0}",
        inside.display()
    );
    let children: Vec<Child> = (0..5)
        .map(|_| locked(d.path(), "shared-db", &script))
        .collect();
    for c in children {
        assert_eq!(finish(c, 20).status.code(), Some(0));
    }
}

#[test]
fn runs_on_different_lock_keys_overlap() {
    let d = tempfile::tempdir().unwrap();
    // Each run waits for the other's marker, so they only finish if both are inside at once.
    let script = |mine: &str, theirs: &str| {
        format!(
            "touch {0}/{mine}; i=0; while [ ! -e {0}/{theirs} ]; do i=$((i+1)); [ $i -gt 200 ] && exit 99; sleep 0.05; done",
            d.path().display()
        )
    };
    let a = locked(d.path(), "db-a", &script("a", "b"));
    let b = locked(d.path(), "db-b", &script("b", "a"));
    assert_eq!(finish(a, 20).status.code(), Some(0));
    assert_eq!(finish(b, 20).status.code(), Some(0));
}

#[test]
fn a_lock_waiter_names_the_holder_and_ls_shows_holder_and_waiter() {
    let d = tempfile::tempdir().unwrap();
    let started = d.path().join("started");
    let holder = locked(
        d.path(),
        "shared-db",
        &format!("touch {}; sleep 30", started.display()),
    );
    wait_for("the holder to start", || started.exists());
    let waiter = heavy(
        d.path(),
        &[
            "--slots",
            "8",
            "--lock",
            "shared-db",
            "--label",
            "second",
            "--",
            "true",
        ],
    )
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
    wait_for("the waiter to show in ls", || {
        stdout(&run(d.path(), &["ls"])).contains("second")
    });
    let ls = stdout(&run(d.path(), &["ls"]));
    assert!(
        ls.lines()
            .any(|l| l.starts_with("running") && l.contains("holds lock shared-db")),
        "{ls}"
    );
    assert!(
        ls.lines().any(|l| l.starts_with("waiting")
            && l.contains("second")
            && l.contains("lock shared-db")),
        "{ls}"
    );
    stop(holder);
    let out = finish(waiter, 10);
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("waiting for lock shared-db") && err.contains("shared-db (pid"),
        "{err}"
    );
}

#[test]
fn a_killed_run_frees_its_lock_once_its_command_is_gone() {
    let d = tempfile::tempdir().unwrap();
    let pidfile = d.path().join("cmd.pid");
    let holder = locked(
        d.path(),
        "shared-db",
        &format!(
            "echo $$ > {0}.tmp; mv {0}.tmp {0}; sleep 30",
            pidfile.display()
        ),
    );
    wait_for("the holder to start", || pidfile.exists());
    let group: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SIGKILL gives the runner no chance to clean up.
    unsafe { libc::kill(pid_of(d.path(), "shared-db"), libc::SIGKILL) };
    finish(holder, 5);
    let next = heavy(
        d.path(),
        &["--slots", "8", "--lock", "shared-db", "--", "true"],
    )
    .stderr(Stdio::null())
    .spawn()
    .unwrap();
    std::thread::sleep(Duration::from_millis(800));
    let mut next = next;
    assert!(
        next.try_wait().unwrap().is_none(),
        "took the lock while the command still ran"
    );
    unsafe { libc::kill(-group, libc::SIGKILL) };
    assert!(finish(next, 10).status.success());
}

#[test]
fn a_run_that_ends_or_is_terminated_frees_its_lock() {
    let d = tempfile::tempdir().unwrap();
    let started = d.path().join("started");
    let holder = locked(
        d.path(),
        "shared-db",
        &format!("touch {}; sleep 30", started.display()),
    );
    wait_for("the holder to start", || started.exists());
    stop(holder);
    assert!(
        run(
            d.path(),
            &["--slots", "8", "--lock", "shared-db", "--", "true"]
        )
        .status
        .success()
    );
}

fn events(state: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(state.join("events.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn a_command_over_its_limit_is_killed_with_its_children_and_exits_124() {
    let d = tempfile::tempdir().unwrap();
    let script = format!("{}; wait", background_sleep(d.path()));
    let t = Instant::now();
    let o = run(d.path(), &["--timeout", "1s", "--", "sh", "-c", &script]);
    assert_eq!(o.status.code(), Some(124));
    assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
    let pid = background_pid(d.path());
    wait_for("the child to die", || !alive(pid));
    assert_eq!(files(d.path(), "slots"), 0);
    let err = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(
        err.contains("time limit") && err.contains("1s") && err.contains("--timeout"),
        "{err}"
    );
    assert_eq!(err.matches("time limit").count(), 1, "{err}");
}

#[test]
fn a_command_that_ignores_sigterm_is_killed_after_the_grace_period_on_timeout() {
    let d = tempfile::tempdir().unwrap();
    let script = "trap '' TERM; sleep 30";
    let o = run(d.path(), &["--timeout", "1s", "--", "sh", "-c", script]);
    assert_eq!(o.status.code(), Some(124));
    assert_eq!(files(d.path(), "slots"), 0);
}

#[test]
fn timeout_none_never_times_out_and_a_quick_command_keeps_its_code() {
    let d = tempfile::tempdir().unwrap();
    let o = run(
        d.path(),
        &["--timeout", "none", "--", "sh", "-c", "sleep 2; exit 3"],
    );
    assert_eq!(o.status.code(), Some(3));
    let o = run(d.path(), &["--timeout", "5s", "--", "sh", "-c", "exit 4"]);
    assert_eq!(o.status.code(), Some(4));
}

#[test]
fn the_default_comes_from_the_env_and_none_overrides_it() {
    let d = tempfile::tempdir().unwrap();
    let mut c = heavy(d.path(), &["--", "sleep", "30"]);
    c.env("REEVE_HEAVY_TIMEOUT", "1s");
    assert_eq!(c.output().unwrap().status.code(), Some(124));
    let mut c = heavy(
        d.path(),
        &["--timeout", "none", "--", "sh", "-c", "sleep 2"],
    );
    c.env("REEVE_HEAVY_TIMEOUT", "1s");
    assert_eq!(c.output().unwrap().status.code(), Some(0));
}

#[test]
fn waiting_for_a_slot_does_not_count_against_the_limit() {
    let d = tempfile::tempdir().unwrap();
    let holder = hold(d.path(), "holder");
    let second = heavy(
        d.path(),
        &[
            "--slots",
            "1",
            "--timeout",
            "3s",
            "--",
            "sh",
            "-c",
            "sleep 1",
        ],
    )
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
    // Waits longer than its whole limit before getting the slot.
    std::thread::sleep(Duration::from_secs(4));
    stop(holder);
    let o = finish(second, 10);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
fn a_timeout_is_recorded_in_the_event_log() {
    let d = tempfile::tempdir().unwrap();
    run(d.path(), &["--timeout", "1s", "--", "sleep", "30"]);
    let ev = events(d.path());
    let e = ev.iter().find(|e| e["command"] == "heavy").unwrap();
    assert_eq!(e["exit"], 124);
    assert_eq!(e["timed_out"], true);
    assert_eq!(e["heavy_limit_ms"], 1000);
}

#[test]
fn ls_shows_a_running_jobs_limit_and_elapsed_time() {
    let d = tempfile::tempdir().unwrap();
    let started = d.path().join("s");
    let script = format!("touch {}; sleep 30", started.display());
    let c = heavy(d.path(), &["--timeout", "45m", "--", "sh", "-c", &script])
        .spawn()
        .unwrap();
    wait_for("start", || started.exists());
    let o = stdout(&run(d.path(), &["ls"]));
    assert!(o.contains("limit 45m"), "{o}");
    stop(c);
    let c = heavy(d.path(), &["--timeout", "none", "--", "sh", "-c", &script])
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let o = stdout(&run(d.path(), &["ls"]));
    assert!(o.contains("no limit"), "{o}");
    stop(c);
}
