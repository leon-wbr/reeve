//! `reeve doctor`: checks what reeve needs from the machine, with one fix line per problem.

use crate::setup::{self, Piece, tilde};
use crate::workers::Registry;
use std::fs;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    /// Worth knowing, but nothing reeve needs is broken.
    Note,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub status: Status,
    pub what: String,
    pub fix: Option<String>,
}

fn ok(what: impl Into<String>) -> Check {
    Check {
        status: Status::Ok,
        what: what.into(),
        fix: None,
    }
}

fn problem(status: Status, what: impl Into<String>, fix: impl Into<String>) -> Check {
    Check {
        status,
        what: what.into(),
        fix: Some(fix.into()),
    }
}

/// `found` is what `sh -c 'command -v reeve'` printed in this environment, which Claude Code's
/// hooks inherit; `clean` is the user's shell with only HOME, USER and SHELL set.
pub fn path_checks(
    found: Option<&str>,
    found_version: Option<&str>,
    clean: Option<&str>,
    version: &str,
) -> Vec<Check> {
    let want = format!("reeve {version}");
    let mut out = vec![match found {
        Some(p) if found_version.is_some_and(|v| v != want) => problem(
            Status::Fail,
            format!(
                "reeve on PATH ({p}) is {}, this is {want}",
                found_version.unwrap_or_default()
            ),
            "point PATH at one reeve build, or reinstall it",
        ),
        Some(p) => ok(format!("reeve on PATH for `sh -c`: {p}")),
        None => problem(
            Status::Fail,
            "reeve isn't on the PATH that `sh -c` sees, so the session-start hook stays silent",
            "put reeve in a directory on PATH, e.g. `ln -s \"$(command -v reeve)\" ~/.local/bin/`, and export that PATH from your shell's profile",
        ),
    }];
    if found.is_some() && clean.is_none() {
        out.push(problem(
            Status::Note,
            "reeve isn't on the PATH of a shell started without your profile (as some GUI apps start agents)",
            "add its directory to PATH in ~/.zshenv (zsh) or ~/.profile if you start agents from outside a terminal",
        ));
    }
    out
}

/// `stub` is the skill stub's text, `hook` the hook's state in settings.json.
pub fn setup_check(stub: Option<&str>, hook: Piece, version: &str) -> Check {
    let stub_version = stub.and_then(setup::stub_version);
    match (stub, hook) {
        (None, Piece::Missing) => problem(
            Status::Note,
            "Claude Code setup isn't installed (optional)",
            "`reeve setup claude` adds a session-start hook and a skill stub",
        ),
        _ if stub_version == Some(version) && hook == Piece::Ok => {
            ok(format!("Claude Code setup matches reeve {version}"))
        }
        _ => {
            let stub = match (stub, stub_version) {
                (None, _) => "skill stub missing".to_string(),
                (Some(_), None) => "skill stub not written by reeve".to_string(),
                (Some(_), Some(v)) => format!("skill stub for reeve {v}"),
            };
            let hook = match hook {
                Piece::Ok => "hook ok",
                Piece::Missing => "hook missing",
                Piece::Stale => "hook changed",
            };
            problem(
                Status::Fail,
                format!("Claude Code setup is out of date for reeve {version}: {stub}, {hook}"),
                "`reeve setup claude` rewrites both; `reeve setup claude --check` shows each",
            )
        }
    }
}

pub fn agent_check(name: &str, version: Option<&str>, required: bool) -> Option<Check> {
    match version {
        Some(v) if v.starts_with(name) => Some(ok(v)),
        Some(v) => Some(ok(format!("{name} {v}"))),
        None if required => Some(problem(
            Status::Fail,
            format!("{name} isn't on PATH"),
            format!("install {name}; `reeve run` starts it"),
        )),
        None => None,
    }
}

pub fn gone_checks(names: &[String]) -> Vec<Check> {
    names
        .iter()
        .map(|n| {
            problem(
                Status::Note,
                format!("worker {n} is gone without a report"),
                format!("start it again with `reeve run --name {n} …` if its work is still needed"),
            )
        })
        .collect()
}

/// The report, and whether every check passed (notes don't count against it).
pub fn render(checks: &[Check]) -> (String, bool) {
    let mut out = vec![];
    for c in checks {
        let tag = match c.status {
            Status::Ok => "ok  ",
            Status::Note => "note",
            Status::Fail => "FAIL",
        };
        out.push(format!("{tag}  {}", c.what));
        if let Some(fix) = &c.fix {
            out.push(format!("      fix: {fix}"));
        }
    }
    let passed = checks.iter().all(|c| c.status != Status::Fail);
    (out.join("\n"), passed)
}

fn first_line(cmd: &mut Command) -> Option<String> {
    let o = cmd
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&o.stdout);
    let line = s.lines().next()?.trim();
    (o.status.success() && !line.is_empty()).then(|| line.to_string())
}

fn which_reeve(clean: bool) -> Option<String> {
    // Without a login or a terminal, the user's shell reads only its env file (~/.zshenv).
    let shell = clean
        .then(|| std::env::var_os("SHELL").filter(|s| !s.is_empty()))
        .flatten()
        .unwrap_or_else(|| "/bin/sh".into());
    let mut c = Command::new(shell);
    c.args(["-c", "command -v reeve"]);
    if clean {
        c.env_clear();
        for k in ["HOME", "USER", "SHELL"] {
            if let Some(v) = std::env::var_os(k) {
                c.env(k, v);
            }
        }
    }
    first_line(&mut c)
}

fn writable(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let probe = dir.join(format!(".doctor-{}", std::process::id()));
    fs::write(&probe, "")?;
    fs::remove_file(&probe)
}

pub fn main(version: &str) -> io::Result<i32> {
    let found = which_reeve(false);
    let found_version = found
        .as_ref()
        .and_then(|p| first_line(Command::new(p).arg("--version")));
    let mut checks = path_checks(
        found.as_deref(),
        found_version.as_deref(),
        which_reeve(true).as_deref(),
        version,
    );

    let paths = setup::Paths::at(&setup::Paths::default_dir()?);
    match setup::check(&paths, version) {
        Ok([(_, hook), _]) => {
            let stub = fs::read_to_string(&paths.skill).ok();
            checks.push(setup_check(stub.as_deref(), hook, version));
        }
        Err(e) => checks.push(problem(
            Status::Fail,
            format!("can't read the Claude Code setup: {e}"),
            format!("fix or restore {}", tilde(&paths.settings)),
        )),
    }

    let state = Registry::default_dir()?;
    checks.push(match writable(&state) {
        Ok(()) => ok(format!("state dir writable: {}", tilde(&state))),
        Err(e) => problem(
            Status::Fail,
            format!("state dir {} isn't writable: {e}", tilde(&state)),
            "fix its permissions, or point XDG_STATE_HOME somewhere writable",
        ),
    });

    for (name, required) in [("claude", true), ("codex", false)] {
        let v = first_line(Command::new(name).arg("--version"));
        checks.extend(agent_check(name, v.as_deref(), required));
    }

    match crate::workers::gone(&Registry::at(state)) {
        Ok(names) => checks.extend(gone_checks(&names)),
        Err(e) => checks.push(problem(
            Status::Fail,
            format!("can't read the worker registry: {e}"),
            "move workers.json aside; `reeve ls` starts a new one",
        )),
    }

    let (out, passed) = render(&checks);
    println!("{out}");
    Ok(if passed { 0 } else { 1 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_from_path_fails_and_a_profile_only_path_is_a_note() {
        let r = Some("/bin/reeve");
        let fine = path_checks(r, Some("reeve 1.0.0"), r, "1.0.0");
        assert_eq!(fine.len(), 1);
        assert_eq!(fine[0].status, Status::Ok);
        let other = path_checks(r, Some("reeve 0.9.0"), r, "1.0.0");
        assert_eq!(other[0].status, Status::Fail);
        assert!(
            other[0].what.contains("is reeve 0.9.0"),
            "{}",
            other[0].what
        );
        let profile_only = path_checks(r, None, None, "1.0.0");
        assert_eq!(profile_only[1].status, Status::Note);
        let missing = path_checks(None, None, None, "1.0.0");
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].status, Status::Fail);
        assert!(missing[0].fix.is_some());
    }

    #[test]
    fn setup_is_optional_but_a_stale_one_fails() {
        let v = "1.0.0";
        let stub = setup::stub(v);
        assert_eq!(setup_check(None, Piece::Missing, v).status, Status::Note);
        assert_eq!(setup_check(Some(&stub), Piece::Ok, v).status, Status::Ok);
        let old = setup_check(Some(&setup::stub("0.9.0")), Piece::Ok, v);
        assert_eq!(old.status, Status::Fail);
        assert!(
            old.what.contains("skill stub for reeve 0.9.0"),
            "{}",
            old.what
        );
        let no_hook = setup_check(Some(&stub), Piece::Missing, v);
        assert!(no_hook.what.contains("hook missing"), "{}", no_hook.what);
        assert_eq!(setup_check(None, Piece::Ok, v).status, Status::Fail);
    }

    #[test]
    fn claude_is_required_and_codex_only_shown_when_present() {
        assert_eq!(
            agent_check("claude", None, true).unwrap().status,
            Status::Fail
        );
        assert_eq!(agent_check("codex", None, false), None);
        assert_eq!(
            agent_check("codex", Some("codex-cli 0.9"), false)
                .unwrap()
                .what,
            "codex-cli 0.9"
        );
    }

    #[test]
    fn every_problem_has_one_fix_line_and_only_failures_fail() {
        let checks = [
            ok("a"),
            problem(Status::Note, "b", "do b"),
            problem(Status::Fail, "c", "do c"),
        ];
        let (out, passed) = render(&checks);
        assert!(!passed);
        assert_eq!(out.matches("fix:").count(), 2);
        assert_eq!(out.lines().count(), 5);
        assert!(render(&checks[..2]).1);
        assert_eq!(
            gone_checks(&["old".into()])[0].what,
            "worker old is gone without a report"
        );
    }
}
