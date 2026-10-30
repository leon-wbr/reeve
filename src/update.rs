//! `reeve update`: upgrades the agent CLIs with the tool that installed them. The install is
//! read from where the executable on PATH really lives, so the same install always gets the
//! same command, and each upgrade runs through `reeve heavy`.

use crate::workers::{self, Agent, Kind, Registry, State};
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How an agent CLI was installed, and so how it is upgraded.
#[derive(Debug, PartialEq)]
pub enum Install {
    BrewCask(String),
    BrewFormula(String),
    Npm(&'static str),
    /// Claude Code's own installer, which `claude update` upgrades.
    Native,
}

impl Install {
    pub fn command(&self) -> Vec<String> {
        let words: Vec<&str> = match self {
            Install::BrewCask(c) => vec!["brew", "upgrade", "--cask", c],
            Install::BrewFormula(f) => vec!["brew", "upgrade", f],
            Install::Npm(p) => {
                return ["npm", "install", "-g", &format!("{p}@latest")]
                    .map(String::from)
                    .to_vec();
            }
            Install::Native => vec!["claude", "update"],
        };
        words.into_iter().map(String::from).collect()
    }
}

/// The npm package of each CLI.
fn package(kind: Kind) -> &'static str {
    match kind {
        Kind::Claude => "@anthropic-ai/claude-code",
        Kind::Codex => "@openai/codex",
    }
}

/// The directory right after `marker` in `path`, e.g. the cask in `.../Caskroom/codex/0.1/...`.
fn after<'a>(path: &'a str, marker: &str) -> Option<&'a str> {
    let rest = &path[path.find(marker)? + marker.len()..];
    rest.split('/').next().filter(|s| !s.is_empty())
}

/// The install behind an executable's resolved path, or None when reeve doesn't know it.
pub fn detect(kind: Kind, resolved: &Path) -> Option<Install> {
    let p = resolved.to_string_lossy();
    if let Some(c) = after(&p, "/Caskroom/") {
        return Some(Install::BrewCask(c.into()));
    }
    if let Some(f) = after(&p, "/Cellar/") {
        return Some(Install::BrewFormula(f.into()));
    }
    if p.contains(&format!("/node_modules/{}/", package(kind))) {
        // Claude Code's old local install lives in npm's layout under ~/.claude/local.
        return Some(if kind == Kind::Claude && p.contains("/.claude/local/") {
            Install::Native
        } else {
            Install::Npm(package(kind))
        });
    }
    (kind == Kind::Claude && p.contains("/share/claude/versions/")).then_some(Install::Native)
}

/// Names of the workers of this agent that are still starting; an upgrade would pull the
/// executable from under them.
pub fn starting(all: &[workers::Worker], kind: Kind, now: u64) -> Vec<String> {
    all.iter()
        .filter(|w| {
            w.agent == kind && workers::state_of(w, now, || Agent::Absent) == State::Starting
        })
        .map(|w| w.name.clone())
        .collect()
}

/// The first line of `BIN --version`, or "unknown" if it fails or takes over 10 s.
fn version(bin: &str) -> String {
    let read = || -> Option<String> {
        let mut child = Command::new(bin)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while child.try_wait().ok()?.is_none() {
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let o = child.wait_with_output().ok()?;
        let s = String::from_utf8_lossy(&o.stdout);
        let line = s.lines().next()?.trim();
        (o.status.success() && !line.is_empty()).then(|| line.to_string())
    };
    read().unwrap_or_else(|| "unknown".into())
}

pub fn main(dry_run: bool) -> io::Result<i32> {
    let reeve = std::env::current_exe()?;
    let all = Registry::default_dir()
        .map(Registry::at)
        .and_then(|r| r.all())
        .unwrap_or_default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut code = 0;
    for kind in [Kind::Claude, Kind::Codex] {
        let bin = kind.bin();
        let Some(exe) = crate::run::on_path(bin) else {
            println!("{bin}: not on PATH, skipped");
            continue;
        };
        let resolved = exe.canonicalize().unwrap_or(exe);
        let Some(install) = detect(kind, &resolved) else {
            println!(
                "{bin}: installed at {}, which reeve doesn't know how to upgrade; skipped",
                resolved.display()
            );
            continue;
        };
        let cmd = install.command();
        let before = version(bin);
        if dry_run {
            println!("{bin} {before}: would run reeve heavy -- {}", cmd.join(" "));
            continue;
        }
        let busy = starting(&all, kind, now);
        if !busy.is_empty() {
            println!(
                "{bin}: skipped, workers still starting: {}",
                busy.join(", ")
            );
            continue;
        }
        println!("{bin} {before}: running {}", cmd.join(" "));
        let status = Command::new(&reeve)
            .args(["heavy", "--label", &format!("update {bin}"), "--"])
            .args(&cmd)
            .status()?;
        if !status.success() {
            println!("{bin}: {} failed", cmd.join(" "));
            code = 1;
            continue;
        }
        let after = version(bin);
        if after == before {
            println!("{bin}: already current, {after}");
        } else {
            println!("{bin}: {before} -> {after}");
        }
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(kind: Kind, p: &str) -> Option<Install> {
        detect(kind, Path::new(p))
    }

    #[test]
    fn homebrew_casks_and_formulae_are_upgraded_by_name() {
        let claude = d(
            Kind::Claude,
            "/opt/homebrew/Caskroom/claude-code@latest/2.1.312/claude",
        );
        assert_eq!(claude, Some(Install::BrewCask("claude-code@latest".into())));
        assert_eq!(
            claude.unwrap().command(),
            ["brew", "upgrade", "--cask", "claude-code@latest"]
        );
        let codex = d(
            Kind::Codex,
            "/opt/homebrew/Caskroom/codex/0.165.0/bin/codex",
        );
        assert_eq!(
            codex.unwrap().command(),
            ["brew", "upgrade", "--cask", "codex"]
        );
        let formula = d(Kind::Codex, "/usr/local/Cellar/codex/0.9.0/bin/codex");
        assert_eq!(formula.unwrap().command(), ["brew", "upgrade", "codex"]);
    }

    #[test]
    fn global_npm_installs_get_the_latest_package() {
        let claude = d(
            Kind::Claude,
            "/usr/local/lib/node_modules/@anthropic-ai/claude-code/cli.js",
        );
        assert_eq!(
            claude.unwrap().command(),
            ["npm", "install", "-g", "@anthropic-ai/claude-code@latest"]
        );
        let codex = d(
            Kind::Codex,
            "/home/u/.nvm/versions/node/v22/lib/node_modules/@openai/codex/bin/codex.js",
        );
        assert_eq!(codex, Some(Install::Npm("@openai/codex")));
    }

    #[test]
    fn claude_s_own_installs_use_claude_update() {
        for p in [
            "/Users/u/.local/share/claude/versions/2.1.312",
            "/Users/u/.claude/local/node_modules/@anthropic-ai/claude-code/cli.js",
        ] {
            assert_eq!(
                d(Kind::Claude, p).unwrap().command(),
                ["claude", "update"],
                "{p}"
            );
        }
    }

    #[test]
    fn only_workers_of_that_agent_still_starting_hold_its_upgrade() {
        let w = |name: &str, agent, starting, started| workers::Worker {
            name: name.into(),
            backend: workers::Backend::Pane,
            agent,
            cwd: "/w".into(),
            started,
            report: "/nonexistent/report.md".into(),
            handle: String::new(),
            starting,
            error: None,
        };
        let all = [
            w("new", Kind::Claude, true, 990),
            w("stale", Kind::Claude, true, 900),
            w("up", Kind::Claude, false, 990),
            w("cx", Kind::Codex, true, 995),
        ];
        assert_eq!(starting(&all, Kind::Claude, 1000), ["new"]);
        assert_eq!(starting(&all, Kind::Codex, 1000), ["cx"]);
    }

    #[test]
    fn an_unknown_install_is_left_alone() {
        assert_eq!(d(Kind::Claude, "/usr/bin/claude"), None);
        assert_eq!(
            d(Kind::Codex, "/Users/u/.local/share/claude/versions/1/codex"),
            None
        );
        assert_eq!(d(Kind::Codex, "/Users/u/.cargo/bin/codex"), None);
    }
}
