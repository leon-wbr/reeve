use std::fs;
use std::io::{self, BufRead};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

const ZERO: &str = "0000000000000000000000000000000000000000";
const HOOK: &str = "reference-transaction";

fn git(args: &[&str]) -> io::Result<String> {
    let out = Command::new("git").args(args).output()?;
    if !out.status.success() {
        return Err(io::Error::other(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Installs the gate in the current repository: `branch` then only moves to commits whose CI passed.
pub fn install(branch: &str, ci: &str) -> io::Result<()> {
    let hooks = PathBuf::from(git(&[
        "rev-parse",
        "--path-format=absolute",
        "--git-common-dir",
    ])?)
    .join("hooks");
    fs::create_dir_all(&hooks)?;
    let hook = hooks.join(HOOK);
    let body = format!(
        "#!/bin/sh\nexec '{}' gate hook \"$@\"\n",
        std::env::current_exe()?.display()
    );
    match fs::read_to_string(&hook) {
        Ok(old) if !old.contains(" gate hook ") => {
            return Err(io::Error::other(format!(
                "{} exists and is not reeve's",
                hook.display()
            )));
        }
        _ => fs::write(&hook, body)?,
    }
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755))?;
    let protected = git(&["config", "--get-all", "reeve.protect"]).unwrap_or_default();
    if !protected.lines().any(|b| b == branch) {
        git(&["config", "--add", "reeve.protect", branch])?;
    }
    git(&["config", "reeve.ci", ci])?;
    Ok(())
}

/// The `reference-transaction` hook: refuses, while prepared, any update that moves a protected
/// branch to a commit without a CI pass. A merge made on the branch is judged by its merged tip.
pub fn hook(state: &str) -> io::Result<bool> {
    if state != "prepared" {
        return Ok(true);
    }
    let protected = git(&["config", "--get-all", "reeve.protect"]).unwrap_or_default();
    let ci = git(&["config", "reeve.ci"]).unwrap_or_default();
    for line in io::stdin().lock().lines() {
        let line = line?;
        let mut parts = line.split(' ');
        let (Some(old), Some(new), Some(name)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let Some(branch) = name
            .strip_prefix("refs/heads/")
            .filter(|b| protected.lines().any(|p| p == *b))
        else {
            continue;
        };
        if new == ZERO {
            eprintln!("reeve gate: {branch} is protected and can't be deleted.");
            return Ok(false);
        }
        let parents = git(&["rev-list", "--parents", "-n1", new])?;
        let parents: Vec<&str> = parents.split(' ').skip(1).collect();
        let tip = match parents.as_slice() {
            [first, merged, ..] if *first == old => *merged,
            _ => new,
        };
        let status = Command::new("sh")
            .arg("-c")
            .arg(ci.replace("{rev}", tip))
            .output()?
            .status
            .code();
        if status != Some(0) {
            let why = if status == Some(2) {
                "still pending"
            } else {
                "not passing"
            };
            let route = ci.replace("{rev}", "<branch>");
            eprintln!(
                "reeve gate: {branch} only takes work whose CI passed, and CI for {} is {why}. \
                 Commit on a branch, wait until `{route}` passes, then merge that branch into {branch}.",
                &tip[..tip.len().min(7)]
            );
            return Ok(false);
        }
    }
    Ok(true)
}
