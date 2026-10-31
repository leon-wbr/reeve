//! `reeve update`: upgrades the agent CLIs with the tool that installed them, and reeve itself
//! from the checkout it was built in. The install is read from where the executable on PATH
//! really lives, so the same install always gets the same command, and each upgrade runs
//! through `reeve heavy`.

use crate::workers::{self, Agent, Kind, Registry, State};
use std::io;
use std::path::{Path, PathBuf};
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

/// Runs git in `dir` and returns its trimmed stdout, or its stderr as the error.
fn git(dir: &Path, args: &[&str]) -> io::Result<String> {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
    } else {
        Err(io::Error::other(
            String::from_utf8_lossy(&o.stderr).trim().to_string(),
        ))
    }
}

/// The remote branch reeve is updated from, e.g. `origin/main`: what `origin/HEAD` points at,
/// else `origin/main` or `origin/master`.
pub fn default_branch(main: &Path) -> Option<String> {
    if let Ok(head) = git(
        main,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    ) {
        return Some(head);
    }
    ["origin/main", "origin/master"].into_iter().find_map(|b| {
        git(
            main,
            &["rev-parse", "--verify", "-q", &format!("{b}^{{commit}}")],
        )
        .ok()
        .map(|_| b.to_string())
    })
}

/// How the installed build compares with the remote default branch.
#[derive(Debug, PartialEq)]
pub enum Standing {
    /// The build has everything on the branch.
    Current,
    /// The branch has commits the build lacks; `behind` counts them when the build's commit is
    /// still in the repository.
    Behind {
        branch: String,
        tip: String,
        behind: Option<usize>,
    },
}

/// Compares `installed` with the default branch as last fetched.
pub fn standing(main: &Path, installed: &str) -> io::Result<Standing> {
    let branch = default_branch(main)
        .ok_or_else(|| io::Error::other("no origin/main or origin/master in the checkout"))?;
    let tip = git(main, &["rev-parse", &format!("{branch}^{{commit}}")])?;
    let has_tip = git(main, &["merge-base", "--is-ancestor", &tip, installed]).is_ok();
    if has_tip {
        return Ok(Standing::Current);
    }
    let behind = git(
        main,
        &["rev-list", "--count", &format!("{installed}..{tip}")],
    )
    .ok()
    .and_then(|n| n.parse().ok());
    Ok(Standing::Behind {
        branch,
        tip,
        behind,
    })
}

/// A short line for a build that lacks commits of the branch.
pub fn behind_text(branch: &str, behind: Option<usize>) -> String {
    match behind {
        Some(1) => format!("1 commit behind {branch}"),
        Some(n) => format!("{n} commits behind {branch}"),
        None => format!("behind {branch}"),
    }
}

/// The dedicated worktree reeve builds updates in, detached at `tip`. Created on first use and
/// moved afterwards; the checkout's own working tree is never touched.
pub fn build_worktree(main: &Path, tip: &str) -> io::Result<PathBuf> {
    let dir = main.join(".claude/worktrees/install-main");
    if dir.join(".git").exists() {
        git(&dir, &["checkout", "-q", "-f", "--detach", tip])?;
    } else {
        let d = dir.to_string_lossy();
        // A leftover registration of a deleted directory would make `add` refuse.
        let _ = git(main, &["worktree", "prune"]);
        git(main, &["worktree", "add", "-q", "--detach", &d, tip])?;
    }
    Ok(dir)
}

/// The `--root` that makes `cargo install` replace `exe`, when it sits in a `bin` directory.
pub fn install_root(exe: &Path) -> Option<PathBuf> {
    let bin = exe.parent()?;
    (bin.file_name()? == "bin")
        .then(|| bin.parent().map(Path::to_path_buf))
        .flatten()
}

/// Where the command that builds and installs `tip` runs, and what it is.
pub fn install_command(worktree: &Path, root: Option<&Path>) -> Vec<String> {
    let mut cmd: Vec<String> = ["cargo", "install", "--locked", "--force", "--path"]
        .map(String::from)
        .into();
    cmd.push(worktree.to_string_lossy().into());
    cmd.push("--target-dir".into());
    cmd.push(worktree.join("target").to_string_lossy().into());
    if let Some(r) = root {
        cmd.push("--root".into());
        cmd.push(r.to_string_lossy().into());
    }
    cmd
}

/// Updates reeve itself from the checkout it was built in. Returns whether it went well.
fn update_self(reeve: &Path, dry_run: bool) -> io::Result<bool> {
    let current = crate::build_info::LONG_VERSION;
    let (Some(source), Some(commit)) = (crate::build_info::source(), crate::build_info::commit())
    else {
        println!(
            "reeve {current}: not built from a git checkout, skipped; update it the way you installed it"
        );
        return Ok(true);
    };
    if !source.join(".git").exists() {
        println!(
            "reeve {current}: built from {}, which is gone or not a git checkout now, skipped",
            source.display()
        );
        return Ok(true);
    }
    if let Err(e) = git(&source, &["fetch", "-q", "origin"]) {
        println!("reeve: git fetch failed ({e}); comparing with the last fetched branch");
    }
    let (branch, tip, behind) = match standing(&source, commit) {
        Ok(Standing::Current) => {
            println!("reeve {current}: already current");
            return Ok(true);
        }
        Ok(Standing::Behind {
            branch,
            tip,
            behind,
        }) => (branch, tip, behind),
        Err(e) => {
            println!("reeve {current}: can't compare with the remote ({e}), skipped");
            return Ok(false);
        }
    };
    let why = behind_text(&branch, behind);
    let exe = reeve.canonicalize().unwrap_or_else(|_| reeve.to_path_buf());
    let root = install_root(&exe);
    let dir = source.join(".claude/worktrees/install-main");
    let cmd = install_command(&dir, root.as_deref());
    if dry_run {
        println!(
            "reeve {current}: {why}; would build {branch} ({}) in {} and run reeve heavy -- {}",
            &tip[..tip.len().min(7)],
            dir.display(),
            cmd.join(" ")
        );
        return Ok(true);
    }
    println!(
        "reeve {current}: {why}; building {branch} in {}",
        dir.display()
    );
    let dir = match build_worktree(&source, &tip) {
        Ok(d) => d,
        Err(e) => {
            println!("reeve: can't prepare {}: {e}", dir.display());
            return Ok(false);
        }
    };
    let status = Command::new(reeve)
        .args(["heavy", "--label", "update reeve", "--"])
        .args(&cmd)
        .current_dir(&dir)
        .status()?;
    if !status.success() {
        println!("reeve: {} failed", cmd.join(" "));
        return Ok(false);
    }
    let installed = root.map_or_else(|| exe.clone(), |r| r.join("bin/reeve"));
    let after = version(&installed.to_string_lossy());
    let after = after.strip_prefix("reeve ").unwrap_or(&after);
    println!("reeve: {current} -> {after}");
    Ok(true)
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
    if !update_self(&reeve, dry_run)? {
        code = 1;
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

    fn g(dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-C"])
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success(), "git {args:?}: {o:?}");
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    /// A checkout with two commits, both pushed to its origin; returns it and the two shas.
    fn checkout() -> (tempfile::TempDir, PathBuf, String, String) {
        let root = tempfile::tempdir().unwrap();
        let (origin, repo) = (root.path().join("origin.git"), root.path().join("repo"));
        std::fs::create_dir(&repo).unwrap();
        g(
            root.path(),
            &["init", "-q", "--bare", "-b", "main", "origin.git"],
        );
        g(&repo, &["init", "-q", "-b", "main"]);
        g(
            &repo,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        g(&repo, &["commit", "-q", "--allow-empty", "-m", "a"]);
        let a = g(&repo, &["rev-parse", "HEAD"]);
        g(&repo, &["commit", "-q", "--allow-empty", "-m", "b"]);
        let b = g(&repo, &["rev-parse", "HEAD"]);
        g(&repo, &["push", "-q", "origin", "main"]);
        (root, repo, a, b)
    }

    #[test]
    fn a_build_is_behind_when_the_default_branch_has_commits_it_lacks() {
        let (_root, repo, a, b) = checkout();
        assert_eq!(default_branch(&repo).as_deref(), Some("origin/main"));
        assert_eq!(standing(&repo, &b).unwrap(), Standing::Current);
        assert_eq!(
            standing(&repo, &a).unwrap(),
            Standing::Behind {
                branch: "origin/main".into(),
                tip: b.clone(),
                behind: Some(1)
            }
        );
        // A build with commits of its own on top of the branch is not behind it.
        g(&repo, &["commit", "-q", "--allow-empty", "-m", "c"]);
        assert_eq!(
            standing(&repo, &g(&repo, &["rev-parse", "HEAD"])).unwrap(),
            Standing::Current
        );
        // A commit this checkout has never seen can only be called behind.
        let gone = "1".repeat(40);
        assert!(matches!(
            standing(&repo, &gone).unwrap(),
            Standing::Behind { behind: None, .. }
        ));
    }

    #[test]
    fn a_checkout_without_a_remote_branch_cannot_be_compared() {
        let dir = tempfile::tempdir().unwrap();
        g(dir.path(), &["init", "-q", "-b", "main"]);
        g(dir.path(), &["commit", "-q", "--allow-empty", "-m", "a"]);
        assert_eq!(default_branch(dir.path()), None);
        assert!(standing(dir.path(), "HEAD").is_err());
    }

    #[test]
    fn the_build_worktree_follows_the_tip_and_leaves_the_main_tree_alone() {
        let (_root, repo, a, b) = checkout();
        // Leon's working tree: a feature branch with an edit in progress.
        g(&repo, &["switch", "-q", "-c", "feature"]);
        std::fs::write(repo.join("wip.txt"), "mine").unwrap();
        let head = g(&repo, &["rev-parse", "HEAD"]);
        let dir = build_worktree(&repo, &a).unwrap();
        assert_eq!(dir, repo.join(".claude/worktrees/install-main"));
        assert_eq!(g(&dir, &["rev-parse", "HEAD"]), a);
        assert_eq!(g(&dir, &["branch", "--show-current"]), "", "detached");
        // The second update reuses it.
        assert_eq!(build_worktree(&repo, &b).unwrap(), dir);
        assert_eq!(g(&dir, &["rev-parse", "HEAD"]), b);
        // Even when it was left with changes.
        std::fs::write(dir.join("junk"), "x").unwrap();
        g(&dir, &["add", "junk"]);
        build_worktree(&repo, &a).unwrap();
        assert_eq!(g(&dir, &["rev-parse", "HEAD"]), a);
        assert_eq!(g(&repo, &["branch", "--show-current"]), "feature");
        assert_eq!(g(&repo, &["rev-parse", "HEAD"]), head);
        assert_eq!(
            std::fs::read_to_string(repo.join("wip.txt")).unwrap(),
            "mine"
        );
    }

    #[test]
    fn a_deleted_build_worktree_is_made_again() {
        let (_root, repo, a, b) = checkout();
        let dir = build_worktree(&repo, &a).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        build_worktree(&repo, &b).unwrap();
        assert_eq!(g(&dir, &["rev-parse", "HEAD"]), b);
    }

    #[test]
    fn the_install_replaces_the_reeve_that_is_running() {
        assert_eq!(
            install_root(Path::new("/Users/u/.cargo/bin/reeve")),
            Some("/Users/u/.cargo".into())
        );
        assert_eq!(install_root(Path::new("/w/reeve/target/debug/reeve")), None);
        let wt = Path::new("/r/.claude/worktrees/install-main");
        assert_eq!(
            install_command(wt, Some(Path::new("/Users/u/.cargo"))).join(" "),
            "cargo install --locked --force --path /r/.claude/worktrees/install-main \
             --target-dir /r/.claude/worktrees/install-main/target --root /Users/u/.cargo"
        );
        assert!(!install_command(wt, None).contains(&"--root".to_string()));
    }

    #[test]
    fn behind_is_said_in_commits_when_they_are_known() {
        assert_eq!(
            behind_text("origin/main", Some(1)),
            "1 commit behind origin/main"
        );
        assert_eq!(
            behind_text("origin/main", Some(3)),
            "3 commits behind origin/main"
        );
        assert_eq!(behind_text("origin/main", None), "behind origin/main");
    }
}
