//! `reeve doctor`: checks what reeve needs from the machine, with one fix line per problem.

use crate::setup::{self, Piece, tilde};
use crate::workers::{Kind, Registry};
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
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

/// Another build later on PATH: `cargo install` puts reeve in ~/.cargo/bin, while an older
/// symlink in ~/.local/bin keeps winning, and both report the same version.
pub fn shadow_check(path: &OsStr) -> Option<Check> {
    let found: Vec<PathBuf> = std::env::split_paths(path)
        .map(|d| d.join("reeve"))
        .filter(|p| p.is_file())
        .collect();
    let first = found.first()?;
    let used = fs::read(first).ok()?;
    let other = found[1..]
        .iter()
        .find(|p| fs::read(p).is_ok_and(|b| b != used))?;
    Some(problem(
        Status::Fail,
        format!(
            "{} shadows a different reeve build at {}",
            tilde(first),
            tilde(other)
        ),
        "remove the one you don't want, or `ln -sf` it to the other",
    ))
}

/// `stub` is the skill stub's text, `hook` the hook's state in settings.json.
pub fn setup_check(stub: Option<&str>, hook: Piece, perms: Piece, version: &str) -> Check {
    let stub_version = stub.and_then(setup::stub_version);
    match (stub, hook, perms) {
        (None, Piece::Missing, Piece::Missing) => problem(
            Status::Note,
            "Claude Code setup isn't installed (optional)",
            "`reeve setup claude` adds session hooks, a skill stub and allow rules for reeve's own commands",
        ),
        _ if stub_version == Some(version) && hook == Piece::Ok && perms == Piece::Ok => {
            ok(format!("Claude Code setup matches reeve {version}"))
        }
        _ => {
            let stub = match (stub, stub_version) {
                (None, _) => "skill stub missing".to_string(),
                (Some(_), None) => "skill stub not written by reeve".to_string(),
                (Some(_), Some(v)) => format!("skill stub for reeve {v}"),
            };
            let hook = match hook {
                Piece::Ok => "hooks ok",
                Piece::Missing => "hooks missing",
                Piece::Stale => "hooks changed",
            };
            let perms = match perms {
                Piece::Ok => "allow rules ok",
                Piece::Missing => "allow rules missing, so `reeve close` etc. can be declined",
                Piece::Stale => "allow rules incomplete",
            };
            problem(
                Status::Fail,
                format!(
                    "Claude Code setup is out of date for reeve {version}: {stub}, {hook}, {perms}"
                ),
                "`reeve setup claude` rewrites all three; `reeve setup claude --check` shows each",
            )
        }
    }
}

/// The Codex setup from `reeve setup codex --check`'s pieces. Whether Codex trusts the hooks
/// isn't something reeve can read back, so an installed setup says where that is done.
pub fn codex_check(pieces: &[(String, Piece)], version: &str) -> Check {
    if pieces.iter().all(|(_, p)| *p == Piece::Missing) {
        return problem(
            Status::Note,
            "Codex setup isn't installed (optional)",
            "`reeve setup codex` adds session hooks, a skill stub and rules",
        );
    }
    let off: Vec<String> = pieces
        .iter()
        .filter(|(_, p)| *p != Piece::Ok)
        .map(|(what, p)| match p {
            Piece::Missing => format!("{what} missing"),
            _ => format!("{what} changed"),
        })
        .collect();
    if off.is_empty() {
        return ok(format!(
            "Codex setup matches reeve {version} (its hooks run once trusted in Codex's /hooks)"
        ));
    }
    problem(
        Status::Fail,
        format!(
            "Codex setup is out of date for reeve {version}: {}",
            off.join(", ")
        ),
        "`reeve setup codex` rewrites it; `reeve setup codex --check` shows each piece",
    )
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

/// `queued` is each remote's name, how many messages wait for it and the age in seconds of the
/// oldest. Doctor never contacts a remote, so this says what is configured and what is stuck.
pub fn remote_checks(queued: &[(String, usize, Option<u64>)], ssh_found: bool) -> Vec<Check> {
    if queued.is_empty() {
        return vec![ok("no remotes configured (optional)")];
    }
    let names: Vec<&str> = queued.iter().map(|(n, ..)| n.as_str()).collect();
    let mut out = vec![if ssh_found {
        ok(format!("remotes: {}", names.join(", ")))
    } else {
        problem(
            Status::Fail,
            format!(
                "remotes are configured ({}) but ssh isn't on PATH",
                names.join(", ")
            ),
            "install an ssh client; reeve talks to remotes only through it",
        )
    }];
    for (name, n, oldest) in queued.iter().filter(|(_, n, _)| *n > 0) {
        out.push(problem(
            Status::Note,
            format!(
                "{n} message(s) queued for {name}, the oldest {} old",
                crate::msg::age(oldest.unwrap_or(0))
            ),
            format!("`reeve remote sync {name}` sends them once it is reachable"),
        ));
    }
    out
}

fn remote_state() -> Vec<Check> {
    let remotes = match crate::remote::load() {
        Ok(r) => r,
        Err(e) => {
            return vec![problem(
                Status::Fail,
                format!("can't read the remotes: {e}"),
                "fix or remove ~/.config/reeve/remotes.json",
            )];
        }
    };
    let store = crate::msg::Store::open().ok();
    let now = crate::msg::now_secs();
    let queued: Vec<_> = remotes
        .iter()
        .map(|r| {
            let waiting = store
                .as_ref()
                .map(|s| crate::msg::list(&s.spool(&r.name)))
                .unwrap_or_default();
            let oldest = waiting
                .iter()
                .filter_map(|m| crate::msg::parse_utc(crate::msg::field(m, "at")))
                .min()
                .map(|t| now.saturating_sub(t));
            (r.name.clone(), waiting.len(), oldest)
        })
        .collect();
    let ssh_found = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .any(|d| d.join("ssh").is_file());
    remote_checks(&queued, ssh_found)
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
    checks.extend(shadow_check(&std::env::var_os("PATH").unwrap_or_default()));

    let paths = setup::Paths::at(&setup::Paths::default_dir()?);
    match setup::check(&paths, version, false) {
        Ok(pieces) => {
            let (hook, perms) = (pieces[0].1, pieces[2].1);
            let stub = fs::read_to_string(&paths.skill).ok();
            checks.push(setup_check(stub.as_deref(), hook, perms, version));
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
        if name == "codex" && v.is_some() {
            let codex =
                setup::Paths::of(Kind::Codex).and_then(|p| setup::check(&p, version, false));
            checks.push(match codex {
                Ok(pieces) => codex_check(&pieces, version),
                Err(e) => problem(
                    Status::Fail,
                    format!("can't read the Codex setup: {e}"),
                    "fix or restore ~/.codex/hooks.json",
                ),
            });
        }
    }

    match crate::workers::gone(&Registry::at(state)) {
        Ok(names) => checks.extend(gone_checks(&names)),
        Err(e) => checks.push(problem(
            Status::Fail,
            format!("can't read the worker registry: {e}"),
            "move workers.json aside; `reeve ls` starts a new one",
        )),
    }

    checks.extend(remote_state());

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
    fn a_different_reeve_later_on_path_is_shadowed() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = (
            dir.path().join("a"),
            dir.path().join("b"),
            dir.path().join("c"),
        );
        for (d, body) in [(&a, "old"), (&b, "new"), (&c, "old")] {
            fs::create_dir(d).unwrap();
            fs::write(d.join("reeve"), body).unwrap();
        }
        let path = std::env::join_paths([&a, &c]).unwrap();
        assert_eq!(shadow_check(&path), None);
        let path = std::env::join_paths([&a, &b, &c]).unwrap();
        let shadowed = shadow_check(&path).unwrap();
        assert_eq!(shadowed.status, Status::Fail);
        assert!(
            shadowed.what.contains(&tilde(&b.join("reeve"))),
            "{}",
            shadowed.what
        );
    }

    #[test]
    fn setup_is_optional_but_a_stale_one_fails() {
        let v = "1.0.0";
        let stub = setup::stub(Kind::Claude, v);
        let (ok_, no) = (Piece::Ok, Piece::Missing);
        assert_eq!(setup_check(None, no, no, v).status, Status::Note);
        assert_eq!(setup_check(Some(&stub), ok_, ok_, v).status, Status::Ok);
        let old = setup_check(Some(&setup::stub(Kind::Claude, "0.9.0")), ok_, ok_, v);
        assert_eq!(old.status, Status::Fail);
        assert!(
            old.what.contains("skill stub for reeve 0.9.0"),
            "{}",
            old.what
        );
        let no_hook = setup_check(Some(&stub), no, ok_, v);
        assert!(no_hook.what.contains("hooks missing"), "{}", no_hook.what);
        assert_eq!(setup_check(None, ok_, no, v).status, Status::Fail);
        let no_perms = setup_check(Some(&stub), ok_, no, v);
        assert_eq!(no_perms.status, Status::Fail);
        assert!(
            no_perms.what.contains("allow rules missing"),
            "{}",
            no_perms.what
        );
        assert!(
            setup_check(Some(&stub), ok_, Piece::Stale, v)
                .what
                .contains("allow rules incomplete")
        );
    }

    #[test]
    fn the_codex_setup_is_optional_but_a_partial_one_fails() {
        let p = |a, b| vec![("hooks".to_string(), a), ("rules".to_string(), b)];
        let none = codex_check(&p(Piece::Missing, Piece::Missing), "1.0.0");
        assert_eq!(none.status, Status::Note);
        let all = codex_check(&p(Piece::Ok, Piece::Ok), "1.0.0");
        assert_eq!(all.status, Status::Ok);
        assert!(all.what.contains("/hooks"), "{}", all.what);
        let half = codex_check(&p(Piece::Ok, Piece::Stale), "1.0.0");
        assert_eq!(half.status, Status::Fail);
        assert!(half.what.contains("rules changed"), "{}", half.what);
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
    fn remotes_show_as_one_line_and_stuck_messages_as_notes() {
        assert_eq!(remote_checks(&[], true).len(), 1);
        let q = [("a".to_string(), 0, None), ("b".to_string(), 2, Some(7200))];
        let checks = remote_checks(&q, true);
        assert_eq!(checks[0].what, "remotes: a, b");
        assert_eq!(checks[1].status, Status::Note);
        assert!(
            checks[1]
                .what
                .contains("2 message(s) queued for b, the oldest 2h old")
        );
        assert_eq!(checks.len(), 2);
        assert_eq!(remote_checks(&q, false)[0].status, Status::Fail);
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
