mod sandbox;

use clap::{Parser, Subcommand};
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "reeve", version)]
struct Cli {
    #[command(subcommand)]
    command: Top,
}

#[derive(Subcommand)]
enum Top {
    /// Run Claude Code with --dangerously-skip-permissions in an isolated container
    #[command(subcommand)]
    Sandbox(Sandbox),
}

#[derive(Subcommand)]
enum Sandbox {
    /// Build the sandbox image
    Build {
        #[arg(long)]
        claude_version: Option<String>,
    },
    /// Start a sandbox, run claude with the given args in it, and remove it
    Run {
        #[arg(long)]
        name: Option<String>,
        #[command(flatten)]
        opts: sandbox::Opts,
        #[arg(last = true)]
        claude_args: Vec<String>,
    },
    /// Start a sandbox and leave it running
    Up {
        #[arg(long)]
        name: String,
        #[command(flatten)]
        opts: sandbox::Opts,
    },
    /// Run a command (or claude, with --claude) in a running sandbox
    Exec {
        #[arg(long)]
        name: String,
        /// Working dir inside the sandbox [default: the workspace it was started with]
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Prefix the command with `claude --dangerously-skip-permissions`
        #[arg(long)]
        claude: bool,
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },
    /// Remove a sandbox
    Down {
        #[arg(long)]
        name: String,
    },
}

fn main() -> ExitCode {
    let Top::Sandbox(cmd) = Cli::parse().command;
    let result: io::Result<i32> = match cmd {
        Sandbox::Build { claude_version } => {
            let v = claude_version.unwrap_or_else(|| sandbox::Opts::default_version().into());
            sandbox::build(&v).map(|tag| println!("{tag}")).map(|_| 0)
        }
        Sandbox::Run {
            name,
            opts,
            claude_args,
        } => {
            let name = name.unwrap_or_else(|| format!("reeve-sandbox-{}", std::process::id()));
            sandbox::run(&name, &opts, &claude_args).map(|s| s.code().unwrap_or(1))
        }
        Sandbox::Up { name, opts } => sandbox::up(&name, &opts)
            .map(|_| println!("{name}"))
            .map(|_| 0),
        Sandbox::Exec {
            name,
            workspace,
            claude,
            cmd,
        } => workspace
            .map_or_else(|| sandbox::workspace_of(&name), std::fs::canonicalize)
            .and_then(|cwd| sandbox::exec(&name, &cwd, &cmd, claude))
            .map(|s| s.code().unwrap_or(1)),
        Sandbox::Down { name } => sandbox::down(&name).map(|_| 0),
    };
    match result {
        Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
        Err(e) => {
            eprintln!("reeve: {e}");
            ExitCode::FAILURE
        }
    }
}
