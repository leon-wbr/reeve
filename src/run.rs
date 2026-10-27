//! `reeve run`: the one way reeve starts an agent, in the foreground or as a Claude Code
//! background session (`--bg`), on the host or in a sandbox.

/// Where the agent runs.
#[derive(Debug, PartialEq, Eq)]
pub enum Target {
    Host,
    /// A sandbox started for this run and removed when the agent exits.
    Ephemeral(String),
    /// A named sandbox, started if it isn't running and left running afterwards.
    Named(String),
}

pub fn target(sandbox: Option<Option<String>>, bg: bool, pid: u32) -> Result<Target, String> {
    match sandbox {
        None => Ok(Target::Host),
        Some(Some(name)) => Ok(Target::Named(name)),
        Some(None) if bg => Err(
            "--bg in a sandbox needs --sandbox <NAME>, so the sandbox outlives this command".into(),
        ),
        Some(None) => Ok(Target::Ephemeral(format!("reeve-run-{pid}"))),
    }
}

/// A worker started outside any repository loads no project's .claude/ settings, skills or hooks.
pub fn cwd_warning(dir: &std::path::Path) -> Option<String> {
    let in_repo = std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    (!in_repo).then(|| {
        format!(
            "{} is not in a git repository, so the worker loads no project's .claude/ settings, skills or hooks; pass --cwd <project dir> to start it in the project",
            crate::setup::tilde(dir)
        )
    })
}

/// The claude command line; in a sandbox it skips permission prompts.
pub fn claude_argv(bg: bool, sandboxed: bool, args: &[String]) -> Vec<String> {
    let flags = [(sandboxed, "--dangerously-skip-permissions"), (bg, "--bg")];
    let flags = flags
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, f)| f.to_string());
    std::iter::once("claude".to_string())
        .chain(flags)
        .chain(args.iter().cloned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn host_unless_a_sandbox_is_asked_for() {
        assert_eq!(target(None, false, 7), Ok(Target::Host));
        assert_eq!(target(None, true, 7), Ok(Target::Host));
    }

    #[test]
    fn unnamed_sandbox_is_ephemeral_and_named_one_is_kept() {
        assert_eq!(
            target(Some(None), false, 7),
            Ok(Target::Ephemeral("reeve-run-7".into()))
        );
        assert_eq!(
            target(Some(Some("bench".into())), false, 7),
            Ok(Target::Named("bench".into()))
        );
        assert_eq!(
            target(Some(Some("bench".into())), true, 7),
            Ok(Target::Named("bench".into()))
        );
    }

    #[test]
    fn background_sandbox_needs_a_name_to_outlive_the_command() {
        assert!(
            target(Some(None), true, 7)
                .unwrap_err()
                .contains("--sandbox <NAME>")
        );
    }

    #[test]
    fn claude_argv_combines_bg_and_sandbox() {
        let a = s(&["-n", "w1", "fix it"]);
        assert_eq!(
            claude_argv(false, false, &a),
            s(&["claude", "-n", "w1", "fix it"])
        );
        assert_eq!(
            claude_argv(true, false, &a),
            s(&["claude", "--bg", "-n", "w1", "fix it"])
        );
        assert_eq!(
            claude_argv(false, true, &a),
            s(&[
                "claude",
                "--dangerously-skip-permissions",
                "-n",
                "w1",
                "fix it"
            ])
        );
        assert_eq!(
            claude_argv(true, true, &a),
            s(&[
                "claude",
                "--dangerously-skip-permissions",
                "--bg",
                "-n",
                "w1",
                "fix it"
            ])
        );
    }
}
