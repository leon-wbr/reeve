mod bench;
mod run;
mod sandbox;

use clap::parser::ValueSource;
use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser, Subcommand};
use run::Target;
use std::io;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Parser)]
#[command(name = "reeve", version)]
struct Cli {
    #[command(subcommand)]
    command: Top,
}

#[derive(Subcommand)]
enum Top {
    /// Start an agent: in the foreground or in the background, on the host or in a sandbox
    Run {
        /// Start it as a Claude Code background session (`claude --bg`; see `claude agents`)
        #[arg(long)]
        bg: bool,
        /// Run it in a sandbox with --dangerously-skip-permissions. Without NAME the sandbox is
        /// removed when the agent exits; with NAME it is started if needed and kept running
        #[arg(long, value_name = "NAME", num_args = 0..=1)]
        sandbox: Option<Option<String>>,
        #[command(flatten)]
        opts: sandbox::Opts,
        /// Arguments passed to claude
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Manage sandboxes started by `reeve run --sandbox`
    #[command(subcommand)]
    Sandbox(Sandbox),
    /// Benchmark a communicator setup on a fixed scenario with a simulated user
    #[command(subcommand)]
    Bench(Bench),
}

#[derive(Subcommand)]
enum Bench {
    /// One run of an arm (bench/arms/<ARM>), named rv-<ARM>-<N>
    Run { arm: String, n: String },
    /// Aggregate results per arm
    Score { arms: Vec<String> },
    /// Done-check a branch of a repo and print it as JSON
    Check { repo: PathBuf, branch: String },
}

#[derive(Subcommand)]
enum Sandbox {
    /// Build the sandbox image
    Build {
        #[arg(long)]
        claude_version: Option<String>,
    },
    /// List running sandboxes
    Ls,
    /// Run a command in a running sandbox, e.g. `-- claude agents`
    Exec {
        #[arg(long)]
        name: String,
        /// Working dir inside the sandbox [default: the workspace it was started with]
        #[arg(long)]
        workspace: Option<PathBuf>,
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },
    /// Remove a sandbox
    Down {
        #[arg(long)]
        name: String,
    },
}

const SANDBOX_OPTS: [&str; 7] = [
    "workspace",
    "read_only",
    "state",
    "env_file",
    "net",
    "allow",
    "claude_version",
];

/// Whether any sandbox option was given on the command line (not from a default or the env).
fn sandbox_opts_given(run: &ArgMatches) -> bool {
    SANDBOX_OPTS
        .iter()
        .any(|id| run.value_source(id) == Some(ValueSource::CommandLine))
}

fn start(
    bg: bool,
    sb: Option<Option<String>>,
    opts: &sandbox::Opts,
    explicit: bool,
    args: &[String],
) -> io::Result<i32> {
    let target = run::target(sb, bg, std::process::id()).map_err(io::Error::other)?;
    if explicit && target == Target::Host {
        return Err(io::Error::other("sandbox options need --sandbox"));
    }
    let argv = run::claude_argv(bg, target != Target::Host, args);
    let code = |s: std::process::ExitStatus| s.code().unwrap_or(1);
    match target {
        Target::Host => Command::new(&argv[0]).args(&argv[1..]).status().map(code),
        Target::Named(name) => {
            if explicit && sandbox::running(&name) {
                eprintln!(
                    "reeve: sandbox {name} is already running; only --workspace is checked, other options stay as they were"
                );
            }
            let cwd = sandbox::ensure(&name, opts)?;
            sandbox::exec(&name, &cwd, &argv).map(code)
        }
        Target::Ephemeral(name) => {
            // A signal during startup is only recorded; the sandbox is removed once it exists.
            static STOP: AtomicBool = AtomicBool::new(false);
            static STARTED: AtomicBool = AtomicBool::new(false);
            let n = name.clone();
            let _ = ctrlc::set_handler(move || {
                STOP.store(true, Ordering::SeqCst);
                if STARTED.load(Ordering::SeqCst) {
                    let _ = sandbox::down(&n);
                    std::process::exit(130);
                }
            });
            let up = sandbox::up(&name, opts);
            STARTED.store(true, Ordering::SeqCst);
            if STOP.load(Ordering::SeqCst) {
                let _ = sandbox::down(&name);
                return Ok(130);
            }
            let status = sandbox::exec(&name, &up?, &argv);
            let _ = sandbox::down(&name);
            status.map(code)
        }
    }
}

fn main() -> ExitCode {
    let matches = Cli::command().get_matches();
    let explicit = matches
        .subcommand_matches("run")
        .is_some_and(sandbox_opts_given);
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    let result: io::Result<i32> = match cli.command {
        Top::Run {
            bg,
            sandbox,
            opts,
            args,
        } => start(bg, sandbox, &opts, explicit, &args),
        Top::Sandbox(Sandbox::Build { claude_version }) => {
            let v = claude_version.unwrap_or_else(|| sandbox::Opts::default_version().into());
            sandbox::build(&v).map(|tag| println!("{tag}")).map(|_| 0)
        }
        Top::Sandbox(Sandbox::Ls) => sandbox::ls().map(|out| println!("{out}")).map(|_| 0),
        Top::Sandbox(Sandbox::Exec {
            name,
            workspace,
            cmd,
        }) => workspace
            .map_or_else(|| sandbox::workspace_of(&name), std::fs::canonicalize)
            .and_then(|cwd| sandbox::exec(&name, &cwd, &cmd))
            .map(|s| s.code().unwrap_or(1)),
        Top::Sandbox(Sandbox::Down { name }) => sandbox::down(&name).map(|_| 0),
        Top::Bench(Bench::Run { arm, n }) => bench::run::run(&arm, &n),
        Top::Bench(Bench::Score { arms }) => bench::score::score(&arms)
            .map(|t| println!("{t}"))
            .map(|_| 0),
        Top::Bench(Bench::Check { repo, branch }) => bench::check::check(&repo, &branch)
            .map(|c| println!("{c}"))
            .map(|_| 0),
    };
    match result {
        Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
        Err(e) => {
            eprintln!("reeve: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn given(args: &[&str]) -> bool {
        let m = Cli::command()
            .try_get_matches_from(["reeve", "run"].iter().chain(args))
            .unwrap();
        sandbox_opts_given(m.subcommand_matches("run").unwrap())
    }

    #[test]
    fn every_sandbox_option_counts_when_given_even_at_its_default() {
        assert!(!given(&["--bg", "--", "-p", "hi"]));
        assert!(!given(&["--sandbox", "x"]));
        for opt in [
            &["--workspace", "."][..],
            &["--ro", "/a"],
            &["--state", "/s"],
            &["--env-file", "/e"],
            &["--net", "restricted"],
            &["--allow", "x.dev"],
            &["--claude-version", "2.0.0"],
        ] {
            assert!(given(opt), "{opt:?}");
        }
    }
}
