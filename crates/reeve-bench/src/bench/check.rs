//! Done-check: exports a snapshot of a branch and tests each item against hidden assertions
//! (bench/scenario/hidden.mjs, which imports the fixture's modules).

use super::{root, term};
use serde_json::{Map, Value, json};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const ITEMS: [&str; 6] = ["mean", "median", "rename", "red", "summary", "flaky"];

/// `{ tests, items: { item: bool }, done }`
pub fn check(repo: &Path, branch: &str) -> io::Result<Value> {
    let dir = temp_dir("reeve-check-")?;
    let result = check_in(repo, branch, &dir);
    let _ = fs::remove_dir_all(&dir);
    result
}

fn check_in(repo: &Path, branch: &str, dir: &Path) -> io::Result<Value> {
    let tar = dir.join("snap.tar");
    let mut archive = Command::new("git");
    archive
        .arg("-C")
        .arg(repo)
        .args(["archive", "-o"])
        .arg(&tar)
        .arg(branch);
    must(&mut archive)?;
    must(
        Command::new("tar")
            .args(["-xf", "snap.tar"])
            .current_dir(dir),
    )?;
    fs::remove_file(&tar)?;
    let tests = passes(
        Command::new("npm")
            .args(["run", "-s", "ci"])
            .current_dir(dir),
    );
    let mut items = hidden(dir);
    if items["flaky"] == true {
        let stable = (0..10).all(|_| passes(Command::new("node").arg("--test").current_dir(dir)));
        items["flaky"] = stable.into();
    }
    if !tests {
        items.values_mut().for_each(|v| *v = false.into());
    }
    let done = tests && items.values().all(|v| *v == true);
    Ok(json!({ "tests": tests, "items": items, "done": done }))
}

fn hidden(dir: &Path) -> Map<String, Value> {
    let out = Command::new("node")
        .arg(root().join("scenario/hidden.mjs"))
        .arg(dir)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let got: Value = out
        .ok()
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).into_owned();
            let last = s.lines().rfind(|l| !l.trim().is_empty())?.to_string();
            serde_json::from_str(&last).ok()
        })
        .unwrap_or_default();
    ITEMS
        .iter()
        .map(|k| (k.to_string(), (got[k] == true).into()))
        .collect()
}

fn must(cmd: &mut Command) -> io::Result<()> {
    let status = cmd.stdin(Stdio::null()).stdout(Stdio::null()).status()?;
    if !status.success() {
        return Err(io::Error::other(format!("command failed: {cmd:?}")));
    }
    Ok(())
}

/// Whether the command succeeds within a minute; it gets SIGTERM after that.
fn passes(cmd: &mut Command) -> bool {
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return s.success(),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            _ => {
                term(&child);
                let _ = child.wait();
                return false;
            }
        }
    }
}

fn temp_dir(prefix: &str) -> io::Result<PathBuf> {
    static N: AtomicUsize = AtomicUsize::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("{prefix}{}-{nanos}-{n}", std::process::id()));
    fs::create_dir(&dir)?;
    Ok(dir)
}
