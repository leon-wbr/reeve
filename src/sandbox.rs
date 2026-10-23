//! Runs Claude Code with --dangerously-skip-permissions inside a Docker container that sees only
//! the workspace, gets credentials only through a docker env file, and (by default) can reach
//! only the Anthropic API.

use clap::ValueEnum;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

const PINNED_VERSION: &str = include_str!("../sandbox/claude-version");
const DOCKERFILE: &str = include_str!("../sandbox/Dockerfile");
const FIREWALL: &str = include_str!("../sandbox/init-firewall.sh");
const STATE_DIR: &str = "/home/node/.claude-state";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum Net {
    #[default]
    Restricted,
    Open,
}

#[derive(Debug, Clone, clap::Args)]
pub struct Opts {
    /// Directory mounted read-write at its host path
    #[arg(long, default_value = ".")]
    pub workspace: PathBuf,
    /// Extra read-only mount at its host path
    #[arg(long = "ro")]
    pub read_only: Vec<PathBuf>,
    /// Host dir kept as the container's CLAUDE_CONFIG_DIR (transcripts)
    #[arg(long)]
    pub state: Option<PathBuf>,
    /// Docker env file holding CLAUDE_CODE_OAUTH_TOKEN or ANTHROPIC_API_KEY [default: ~/.config/reeve/sandbox.env]
    #[arg(long, env = "REEVE_SANDBOX_ENV_FILE")]
    pub env_file: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t)]
    pub net: Net,
    /// Extra host the firewall lets through
    #[arg(long)]
    pub allow: Vec<String>,
    /// Claude Code version [default: the one pinned in sandbox/claude-version]
    #[arg(long)]
    pub claude_version: Option<String>,
}

impl Opts {
    pub fn default_version() -> &'static str {
        PINNED_VERSION.trim()
    }

    pub fn version(&self) -> &str {
        self.claude_version
            .as_deref()
            .unwrap_or(Self::default_version())
    }
}

/// A host path mounted at the same path inside the container; `true` means read-only.
pub type Mount = (PathBuf, bool);

const WORKSPACE_LABEL: &str = "reeve.workspace";
/// Written by init-firewall once egress is locked down.
const READY: &str = "/run/reeve-ready";

/// Tagged with a hash of the embedded build files, so editing them builds a new image.
pub fn image_tag(version: &str) -> String {
    let hash = [DOCKERFILE, FIREWALL]
        .iter()
        .flat_map(|s| s.bytes())
        .fold(0xcbf29ce484222325u64, |h, b| {
            (h ^ b as u64).wrapping_mul(0x100000001b3)
        });
    format!("reeve-sandbox:claude-{version}-{:08x}", hash as u32)
}

fn strings<const N: usize>(a: [&str; N]) -> impl Iterator<Item = String> {
    a.into_iter().map(String::from)
}

/// A `--mount` spec; fields are CSV-quoted so paths may contain ':' or ','.
fn bind(src: &Path, dst: &Path, ro: bool) -> String {
    let q = |k: &str, p: &Path| format!("\"{k}={}\"", p.display().to_string().replace('"', "\"\""));
    format!(
        "type=bind,{},{}{}",
        q("source", src),
        q("target", dst),
        if ro { ",readonly" } else { "" }
    )
}

pub fn up_args(
    name: &str,
    image: &str,
    mounts: &[Mount],
    env_file: &Path,
    o: &Opts,
) -> Vec<String> {
    let net = o.net.to_possible_value().expect("no skipped variants");
    let mut a: Vec<String> = strings([
        "run",
        "-d",
        "--rm",
        "--init",
        "--name",
        name,
        "--cap-add",
        "NET_ADMIN",
        "--cap-add",
        "NET_RAW",
    ])
    .collect();
    a.extend(strings([
        "-e",
        &format!("REEVE_SANDBOX_NET={}", net.get_name()),
    ]));
    if !o.allow.is_empty() {
        a.extend(strings([
            "-e",
            &format!("REEVE_SANDBOX_ALLOW={}", o.allow.join(" ")),
        ]));
    }
    a.extend(strings(["--env-file", &env_file.display().to_string()]));
    if let Some((ws, _)) = mounts.first() {
        a.extend(strings([
            "--label",
            &format!("{WORKSPACE_LABEL}={}", ws.display()),
        ]));
    }
    for (p, ro) in mounts {
        a.extend(strings(["--mount", &bind(p, p, *ro)]));
    }
    if let Some(s) = &o.state {
        a.extend(strings([
            "--mount",
            &bind(s, Path::new(STATE_DIR), false),
            "-e",
            &format!("CLAUDE_CONFIG_DIR={STATE_DIR}"),
        ]));
    }
    a.push(image.into());
    a
}

pub fn exec_args(name: &str, cwd: &Path, argv: &[String], tty: bool) -> Vec<String> {
    let mut a: Vec<String> = strings(["exec", "-i"]).collect();
    if tty {
        a.push("-t".into());
    }
    a.extend(strings([
        "-u",
        "node",
        "-w",
        &cwd.display().to_string(),
        name,
    ]));
    a.extend(argv.iter().cloned());
    a
}

/// The workspace first, then its git dir when that lies outside (a worktree's common dir).
/// Hooks and config of that git dir and of every submodule's are read-only, so the agent
/// can't plant code that git on the host would run.
pub fn mounts(workspace: &Path) -> io::Result<Vec<Mount>> {
    let ws = fs::canonicalize(workspace)?;
    let mut out = vec![(ws.clone(), false)];
    if !ws.join(".git").exists() {
        return Ok(out);
    }
    let o = Command::new("git")
        .arg("-C")
        .arg(&ws)
        .args(["rev-parse", "--git-common-dir"])
        .output()?;
    if !o.status.success() {
        return Err(io::Error::other("git rev-parse --git-common-dir failed"));
    }
    let git = fs::canonicalize(ws.join(String::from_utf8_lossy(&o.stdout).trim()))?;
    if !git.starts_with(&ws) {
        out.push((git.clone(), false));
    }
    for d in git_dirs(&git)? {
        fs::create_dir_all(d.join("hooks"))?;
        out.extend([(d.join("hooks"), true), (d.join("config"), true)]);
    }
    Ok(out)
}

/// A git dir and the git dirs of its submodules, which live (possibly nested) under `modules/`.
fn git_dirs(git: &Path) -> io::Result<Vec<PathBuf>> {
    fn walk(d: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
        for e in fs::read_dir(d)? {
            let p = e?.path();
            if !p.is_dir() {
                continue;
            }
            if p.join("HEAD").is_file() {
                out.extend(git_dirs(&p)?);
            } else {
                walk(&p, out)?;
            }
        }
        Ok(())
    }
    let mut out = vec![git.to_path_buf()];
    if git.join("modules").is_dir() {
        walk(&git.join("modules"), &mut out)?;
    }
    Ok(out)
}

fn default_env_file() -> Option<PathBuf> {
    Some(std::env::home_dir()?.join(".config/reeve/sandbox.env")).filter(|p| p.is_file())
}

fn docker<S: AsRef<OsStr>>(args: impl IntoIterator<Item = S>) -> io::Result<String> {
    let o = Command::new("docker")
        .args(args)
        .stdin(Stdio::null())
        .output()?;
    if !o.status.success() {
        return Err(io::Error::other(
            String::from_utf8_lossy(&o.stderr).trim().to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

pub fn build(version: &str) -> io::Result<String> {
    let tag = image_tag(version);
    let ctx = std::env::temp_dir().join(format!("reeve-sandbox-build-{}", std::process::id()));
    fs::create_dir_all(&ctx)?;
    let result = fs::write(ctx.join("Dockerfile"), DOCKERFILE)
        .and_then(|_| fs::write(ctx.join("init-firewall.sh"), FIREWALL))
        .and_then(|_| {
            let arg = format!("CLAUDE_CODE_VERSION={version}");
            Command::new("docker")
                .args(["build", "-t", &tag, "--build-arg", &arg])
                .arg(&ctx)
                .status()
        });
    let _ = fs::remove_dir_all(&ctx);
    match result? {
        s if s.success() => Ok(tag),
        s => Err(io::Error::other(format!("docker build failed ({s})"))),
    }
}

/// Starts the sandbox and returns the workspace path to run in.
pub fn up(name: &str, o: &Opts) -> io::Result<PathBuf> {
    let env_file = o.env_file.clone().or_else(default_env_file).ok_or_else(|| {
        io::Error::other("no sandbox env file: put CLAUDE_CODE_OAUTH_TOKEN=… in ~/.config/reeve/sandbox.env or pass --env-file")
    })?;
    let mut ms = mounts(&o.workspace)?;
    for p in &o.read_only {
        ms.push((fs::canonicalize(p)?, true));
    }
    let state = o
        .state
        .as_ref()
        .map(|s| fs::create_dir_all(s).and_then(|_| fs::canonicalize(s)));
    let o = Opts {
        state: state.transpose()?,
        ..o.clone()
    };
    let image = image_tag(o.version());
    if docker(["image", "inspect", &image]).is_err() {
        build(o.version())?;
    }
    docker(up_args(name, &image, &ms, &env_file, &o))?;
    if let Err(e) = docker(["exec", "-u", "root", name, "init-firewall"]) {
        let _ = down(name);
        return Err(io::Error::other(format!("sandbox firewall failed: {e}")));
    }
    Ok(ms.swap_remove(0).0)
}

pub fn down(name: &str) -> io::Result<()> {
    docker(["rm", "-f", name]).map(|_| ())
}

/// The workspace a running sandbox was started with.
pub fn workspace_of(name: &str) -> io::Result<PathBuf> {
    let label = format!("{{{{index .Config.Labels \"{WORKSPACE_LABEL}\"}}}}");
    match docker(["inspect", "-f", &label, name])?.as_str() {
        "" | "<no value>" => Err(io::Error::other(format!("{name} is not a reeve sandbox"))),
        w => Ok(PathBuf::from(w)),
    }
}

/// Sandbox `name` for workspace `o.workspace`: reused if it exists (once its firewall is up),
/// otherwise started. Losing a race to start it counts as finding it.
pub fn ensure(name: &str, o: &Opts) -> io::Result<PathBuf> {
    if !running(name) {
        match up(name, o) {
            Ok(ws) => return Ok(ws),
            Err(e) if !e.to_string().contains("is already in use") => return Err(e),
            Err(_) => wait_for(
                || docker(["inspect", name]).is_ok(),
                &format!("sandbox {name} to appear"),
            )?,
        }
    }
    let ws = workspace_of(name)?;
    let want = fs::canonicalize(&o.workspace)?;
    if ws != want {
        return Err(io::Error::other(format!(
            "sandbox {name} holds {}, not {}; use another name",
            ws.display(),
            want.display()
        )));
    }
    wait_for(
        || docker(["exec", name, "test", "-f", READY]).is_ok(),
        &format!("sandbox {name}'s firewall"),
    )?;
    Ok(ws)
}

fn wait_for(ok: impl Fn() -> bool, what: &str) -> io::Result<()> {
    for _ in 0..60 {
        if ok() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    Err(io::Error::other(format!("timed out waiting for {what}")))
}

pub fn exec(name: &str, cwd: &Path, argv: &[String]) -> io::Result<ExitStatus> {
    let tty = io::stdin().is_terminal() && io::stdout().is_terminal();
    Command::new("docker")
        .args(exec_args(name, cwd, argv, tty))
        .status()
}

pub fn running(name: &str) -> bool {
    docker(["inspect", "-f", "{{.State.Running}}", name]).is_ok_and(|s| s == "true")
}

/// Running sandboxes, one per line: name, workspace, status.
pub fn ls() -> io::Result<String> {
    let label = format!("{{{{.Label \"{WORKSPACE_LABEL}\"}}}}");
    let format = format!("table {{{{.Names}}}}\t{label}\t{{{{.Status}}}}");
    docker([
        "ps",
        "--filter",
        &format!("label={WORKSPACE_LABEL}"),
        "--format",
        &format,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct T {
        #[command(flatten)]
        o: Opts,
    }

    fn opts(args: &[&str]) -> Opts {
        T::parse_from(std::iter::once("t").chain(args.iter().copied())).o
    }

    fn up(o: &Opts, mounts: &[Mount]) -> Vec<String> {
        up_args("sb", "img", mounts, Path::new("/cfg/sandbox.env"), o)
    }

    fn after<'a>(args: &'a [String], flag: &str) -> Vec<&'a str> {
        args.windows(2)
            .filter(|w| w[0] == flag)
            .map(|w| w[1].as_str())
            .collect()
    }

    #[test]
    fn image_tag_carries_the_pinned_version_and_build_files_hash() {
        assert!(image_tag("2.1.308").starts_with("reeve-sandbox:claude-2.1.308-"));
        assert_eq!(opts(&[]).version(), PINNED_VERSION.trim());
        assert!(
            opts(&[])
                .version()
                .split('.')
                .all(|p| p.parse::<u32>().is_ok())
        );
        assert_eq!(opts(&["--claude-version", "2.0.1"]).version(), "2.0.1");
    }

    #[test]
    fn mounts_only_the_given_paths_at_their_host_path() {
        let a = up(
            &opts(&[]),
            &[("/work/s".into(), false), ("/a:b,c".into(), true)],
        );
        assert_eq!(
            after(&a, "--mount"),
            [
                r#"type=bind,"source=/work/s","target=/work/s""#,
                r#"type=bind,"source=/a:b,c","target=/a:b,c",readonly"#
            ]
        );
        assert_eq!(after(&a, "--label"), ["reeve.workspace=/work/s"]);
        assert_eq!(a.last().unwrap(), "img");
    }

    #[test]
    fn grants_firewall_caps_and_restricts_network_unless_opened() {
        let a = up(&opts(&[]), &[]);
        assert_eq!(after(&a, "--cap-add"), ["NET_ADMIN", "NET_RAW"]);
        assert_eq!(after(&a, "-e"), ["REEVE_SANDBOX_NET=restricted"]);
        let open = up(
            &opts(&[
                "--net",
                "open",
                "--allow",
                "registry.npmjs.org",
                "--allow",
                "x.dev",
            ]),
            &[],
        );
        assert_eq!(
            after(&open, "-e"),
            [
                "REEVE_SANDBOX_NET=open",
                "REEVE_SANDBOX_ALLOW=registry.npmjs.org x.dev"
            ]
        );
    }

    #[test]
    fn credentials_only_through_the_env_file() {
        let a = up(&opts(&[]), &[]);
        assert_eq!(after(&a, "--env-file"), ["/cfg/sandbox.env"]);
        assert!(
            !a.iter()
                .any(|x| x.contains("ANTHROPIC_API_KEY") || x.contains("OAUTH"))
        );
    }

    #[test]
    fn state_dir_becomes_claude_config_dir() {
        let a = up(&opts(&["--state", "/logs/claude"]), &[]);
        assert_eq!(
            after(&a, "--mount"),
            [r#"type=bind,"source=/logs/claude","target=/home/node/.claude-state""#]
        );
        assert!(a.contains(&"CLAUDE_CONFIG_DIR=/home/node/.claude-state".to_string()));
    }

    #[test]
    fn commands_run_as_node_in_the_workspace() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let a = exec_args("sb", Path::new("/ws"), &s(&["claude", "-p", "hi"]), false);
        assert_eq!(
            a,
            s(&[
                "exec", "-i", "-u", "node", "-w", "/ws", "sb", "claude", "-p", "hi"
            ])
        );
        let tty = exec_args("sb", Path::new("/ws"), &s(&["ls"]), true);
        assert_eq!(
            tty,
            s(&["exec", "-i", "-t", "-u", "node", "-w", "/ws", "sb", "ls"])
        );
    }

    #[test]
    fn sandbox_runs_an_init_to_reap_background_sessions() {
        assert!(up(&opts(&[]), &[]).contains(&"--init".to_string()));
    }

    #[test]
    fn git_dirs_are_mounted_with_read_only_hooks_and_config() {
        let root = std::env::temp_dir().join(format!("reeve-sb-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("plain")).unwrap();
        let root = fs::canonicalize(&root).unwrap();
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .current_dir(&root)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .status()
                .unwrap()
                .success();
            assert!(ok);
        };
        git(&["init", "-q", "-b", "main", "base"]);
        git(&["-C", "base", "commit", "-q", "--allow-empty", "-m", "x"]);
        git(&[
            "-C",
            "base",
            "worktree",
            "add",
            "-q",
            root.join("wt").to_str().unwrap(),
        ]);
        let g = root.join("base/.git");
        let sub = g.join("modules/libs/sub");
        fs::create_dir_all(sub.join("modules/inner")).unwrap();
        fs::write(sub.join("HEAD"), "").unwrap();
        fs::write(sub.join("modules/inner/HEAD"), "").unwrap();
        let guarded = |d: &Path| [(d.join("hooks"), true), (d.join("config"), true)];
        let rw = |p: PathBuf| (p, false);
        let expected_git: Vec<Mount> = [
            g.as_path(),
            sub.as_path(),
            sub.join("modules/inner").as_path(),
        ]
        .iter()
        .flat_map(|d| guarded(d))
        .collect();
        let wt = mounts(&root.join("wt")).unwrap();
        assert_eq!(wt[..2], [rw(root.join("wt")), rw(g.clone())]);
        assert_eq!(wt[2..], expected_git[..]);
        let base = mounts(&root.join("base")).unwrap();
        assert_eq!(base[..1], [rw(root.join("base"))]);
        assert_eq!(base[1..], expected_git[..]);
        assert_eq!(
            mounts(&root.join("plain")).unwrap(),
            [rw(root.join("plain"))]
        );
        fs::remove_dir_all(&root).unwrap();
    }
}
