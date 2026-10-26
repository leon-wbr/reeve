mod events;
mod feedback;
mod heavy;
mod run;
mod sandbox;
mod stats;
mod workers;

use clap::parser::ValueSource;
use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser, Subcommand};
use events::Event;
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
        /// Start a named worker that any session finds with `reeve ls`. It runs in a Herdr pane
        /// inside Herdr and as a background session otherwise, and is told to write its report
        /// to ~/.local/state/reeve/reports/NAME.md. The last claude argument is its prompt
        #[arg(long, value_name = "NAME", conflicts_with = "sandbox")]
        name: Option<String>,
        /// Run the named worker in a new Herdr pane
        #[arg(long, requires = "name", conflicts_with = "bg")]
        pane: bool,
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
    /// List named workers and whether each is running, done or gone
    Ls,
    /// Block until a named worker has written its report
    Wait {
        name: String,
        /// Give up after this many seconds
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Print a named worker's report
    Report { name: String },
    /// Manage sandboxes started by `reeve run --sandbox`
    #[command(subcommand)]
    Sandbox(Sandbox),
    /// Run a heavy command (build, test suite, install) once a slot is free, so agents queue
    Heavy(heavy::Args),
    /// Sum up the event log: workers, heavy waits and runs, handoffs, errors and who ran reeve
    Stats {
        /// How many days back to look
        #[arg(long, default_value_t = 7)]
        days: u64,
        #[arg(long)]
        json: bool,
    },
    /// Save a note about reeve itself, with its version, OS and last 20 events, and offer to post
    /// it as a GitHub issue (with gh after a yes, or from a prefilled page)
    Feedback {
        text: String,
        /// Keep paths in the attached events
        #[arg(long)]
        with_paths: bool,
        /// Open the prefilled new-issue page in the browser
        #[arg(long)]
        open: bool,
    },
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

/// The subcommand path, e.g. `sandbox build`, without any arguments.
fn command_path(mut m: &ArgMatches) -> String {
    let mut parts = vec![];
    while let Some((name, sub)) = m.subcommand() {
        parts.push(name);
        m = sub;
    }
    parts.join(" ")
}

/// A command line that doesn't parse is logged by its first word, if that names a command.
fn log_usage_error(e: &clap::Error) {
    if !e.use_stderr() {
        return;
    }
    let first = std::env::args().nth(1).unwrap_or_default();
    let known = Cli::command()
        .get_subcommands()
        .any(|s| s.get_name() == first);
    let mut ev = Event::start(if known { &first } else { "unknown" });
    ev.finish(e.exit_code(), None);
    ev.error = Some("usage".into());
    events::log(&ev);
}

fn main() -> ExitCode {
    let matches = Cli::command().try_get_matches().unwrap_or_else(|e| {
        log_usage_error(&e);
        e.exit()
    });
    let mut ev = Event::start(&command_path(&matches));
    let explicit = matches
        .subcommand_matches("run")
        .is_some_and(sandbox_opts_given);
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    let workers = || workers::Registry::default_dir().map(workers::Registry::at);
    let result: io::Result<i32> = match cli.command {
        Top::Run { name: Some(_), .. } if explicit => {
            Err(io::Error::other("sandbox options need --sandbox"))
        }
        Top::Run {
            name: Some(name),
            bg,
            pane,
            args,
            ..
        } => {
            ev.worker = Some(name.clone());
            let backend = workers::Backend::choose(
                pane,
                bg,
                std::env::var("HERDR_ENV").is_ok_and(|v| v == "1"),
            );
            let started = workers().and_then(|r| workers::run(&r, &name, backend, &args));
            if started.is_ok() {
                ev.handoff_ms = Some(events::now_ms().saturating_sub(ev.ts));
            }
            started.map(|_| 0)
        }
        Top::Run {
            bg,
            sandbox,
            opts,
            args,
            ..
        } => {
            ev.label = sandbox.clone().flatten();
            start(bg, sandbox, &opts, explicit, &args)
        }
        Top::Ls => workers()
            .and_then(|r| workers::ls(&r))
            .map(|out| println!("{out}"))
            .map(|_| 0),
        Top::Wait { name, timeout } => {
            ev.worker = Some(name.clone());
            workers()
                .and_then(|r| workers::wait(&r, &name, timeout))
                .map(|_| 0)
        }
        Top::Report { name } => {
            ev.worker = Some(name.clone());
            workers()
                .and_then(|r| workers::report(&r, &name))
                .map(|out| print!("{out}"))
                .map(|_| 0)
        }
        Top::Sandbox(Sandbox::Build { claude_version }) => {
            let v = claude_version.unwrap_or_else(|| sandbox::Opts::default_version().into());
            sandbox::build(&v).map(|tag| println!("{tag}")).map(|_| 0)
        }
        Top::Sandbox(Sandbox::Ls) => sandbox::ls().map(|out| println!("{out}")).map(|_| 0),
        Top::Sandbox(Sandbox::Exec {
            name,
            workspace,
            cmd,
        }) => {
            ev.label = Some(name.clone());
            workspace
                .map_or_else(|| sandbox::workspace_of(&name), std::fs::canonicalize)
                .and_then(|cwd| sandbox::exec(&name, &cwd, &cmd))
                .map(|s| s.code().unwrap_or(1))
        }
        Top::Sandbox(Sandbox::Down { name }) => {
            ev.label = Some(name.clone());
            sandbox::down(&name).map(|_| 0)
        }
        Top::Heavy(args) => heavy::main(args, &mut ev),
        Top::Stats { days, json } => stats(days, json),
        Top::Feedback {
            text,
            with_paths,
            open,
        } => feedback::main(&text, with_paths, open),
    };
    ev.finish(*result.as_ref().unwrap_or(&1), result.as_ref().err());
    events::log(&ev);
    match result {
        Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
        Err(e) => {
            eprintln!("reeve: {e}");
            ExitCode::FAILURE
        }
    }
}

fn stats(days: u64, json: bool) -> io::Result<i32> {
    let r = workers::Registry::at(workers::Registry::default_dir()?);
    let state = |name: &str| r.get(name).ok().map(|w| workers::state(&w));
    let s = stats::summarize(&events::read()?, events::now_ms(), days, state);
    if json {
        println!("{}", serde_json::to_string_pretty(&s)?);
    } else {
        print!("{}", stats::render(&s));
    }
    Ok(0)
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
    fn pane_needs_a_name_and_named_workers_stay_out_of_sandboxes() {
        let parse = |args: &[&str]| {
            Cli::try_parse_from(["reeve", "run"].iter().chain(args).chain(&["--", "go"])).is_ok()
        };
        assert!(parse(&["--name", "x", "--pane"]));
        assert!(parse(&["--name", "x", "--bg"]));
        assert!(!parse(&["--pane"]));
        assert!(!parse(&["--name", "x", "--pane", "--bg"]));
        assert!(!parse(&["--name", "x", "--sandbox", "sb"]));
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
