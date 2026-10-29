mod ask;
mod background;
mod bench;
mod bench_entry;
mod context;
mod doctor;
mod events;
mod feedback;
mod gate;
mod handoff;
mod heavy;
mod run;
mod sandbox;
mod setup;
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
#[command(
    name = "reeve",
    version,
    arg_required_else_help = true,
    args_conflicts_with_subcommands = true
)]
struct Cli {
    /// Print the manual for agents: what reeve offers and when each part helps
    #[arg(long, exclusive = true)]
    skill: bool,
    #[command(subcommand)]
    command: Option<Top>,
}

const SKILL: &str = include_str!("../docs/skill.md");
const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Subcommand)]
enum Top {
    /// Start an agent: in the foreground or in the background, on the host or in a sandbox
    Run {
        /// Start it as a Claude Code background session (`claude --bg`; see `claude agents`).
        /// Refused for a named worker inside Herdr, where it gets its own tab (see --force)
        #[arg(long)]
        bg: bool,
        /// Skip reeve's safety checks, such as refusing --bg for a named worker inside Herdr
        #[arg(long)]
        force: bool,
        /// Start a named worker that any session finds with `reeve ls`. It runs in a Herdr pane
        /// in its own tab labelled NAME (not focused) inside Herdr and as a background session
        /// otherwise, and is told to write its report
        /// to ~/.local/state/reeve/reports/NAME.md. The last claude argument is its prompt
        #[arg(long, value_name = "NAME", conflicts_with = "sandbox")]
        name: Option<String>,
        /// Run the named worker in a new Herdr tab
        #[arg(long, requires = "name", conflicts_with = "bg")]
        pane: bool,
        /// Run it in a sandbox with --dangerously-skip-permissions. Without NAME the sandbox is
        /// removed when the agent exits; with NAME it is started if needed and kept running
        #[arg(long, value_name = "NAME", num_args = 0..=1)]
        sandbox: Option<Option<String>>,
        /// Start the agent in DIR, usually the project's root, so it loads that project's
        /// CLAUDE.md, .claude/ settings, skills and hooks from the start [default: here]
        #[arg(long, value_name = "DIR")]
        cwd: Option<PathBuf>,
        #[command(flatten)]
        opts: sandbox::Opts,
        /// Arguments passed to claude
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// List named workers and whether each is running, done or gone
    Ls {
        /// One short line for a session-start hook, and nothing when there are no workers
        #[arg(long)]
        brief: bool,
    },
    /// Block until a named worker has written its report
    Wait {
        name: String,
        /// Give up after this many seconds
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Print a line on stdout each time a named worker becomes blocked, idle, gone or done, for
    /// Claude Code's Monitor tool or a background shell. Without names it follows every worker
    /// and never exits; with names it exits once each is done (0) or gone (1)
    Watch {
        #[arg(value_name = "NAME")]
        names: Vec<String>,
    },
    /// Print a named worker's report
    Report { name: String },
    /// Hand off to a successor: write a handoff file from your notes and the named workers, and
    /// start the successor as a named worker told to read it. Stop once it returns
    Handoff {
        /// The successor's worker name
        #[arg(long)]
        name: String,
        /// Your handoff notes: goal, what's done, what's next, open questions
        #[arg(long)]
        notes: PathBuf,
        /// Start the successor in a new Herdr tab
        #[arg(long, conflicts_with = "bg")]
        pane: bool,
        /// Start it as a Claude Code background session. Refused inside Herdr, where it gets its
        /// own tab (see --force)
        #[arg(long)]
        bg: bool,
        /// Skip reeve's safety checks, such as refusing --bg inside Herdr
        #[arg(long)]
        force: bool,
        /// Arguments passed to the successor's claude
        #[arg(last = true)]
        args: Vec<String>,
    },
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
    /// Install reeve's pieces into an agent's setup
    #[command(subcommand)]
    Setup(Setup),
    /// Check PATH, the Claude Code setup, the state dir, the agent CLIs and gone workers
    Doctor,
    /// Benchmark a communicator setup on a fixed scenario with a simulated user, and measure how
    /// agents choose with reeve installed
    #[command(subcommand)]
    Bench(Bench),
    /// Protect a branch so it only moves to commits whose CI passed (a git reference-transaction hook)
    #[command(subcommand)]
    Gate(Gate),
    /// Queue a question for the user and wait for the answer (run it in the background and keep working)
    Ask { question: String },
    /// List open questions
    Asks,
    /// Answer an open question
    Answer { id: String, text: String },
    /// Run as a Claude Code hook (installed by `reeve setup claude`)
    #[command(subcommand)]
    Hook(Hook),
    /// Drop a worker that is gone or done from `reeve ls`; its report stays
    Rm { name: String },
    /// Bring up a worker that `reeve run` registered; it runs this in the background
    #[command(hide = true)]
    Start {
        name: String,
        /// The Herdr tab `reeve run` opened for a pane worker
        #[arg(long)]
        tab: Option<String>,
        /// Claude's arguments in a pane, or the whole `claude --bg` command line
        #[arg(last = true)]
        argv: Vec<String>,
    },
}

#[derive(Subcommand)]
enum Hook {
    /// Read the hook's JSON on stdin and, past the handoff threshold, tell the session to hand
    /// off. The threshold is REEVE_HANDOFF_TOKENS, else `handoff_tokens` in
    /// ~/.config/reeve/config.json, else 350000
    Context,
    /// Read a PreToolUse hook's JSON on stdin and send a Bash call that starts with
    /// `reeve heavy` to the background
    Background,
}

#[derive(Subcommand)]
enum Gate {
    /// Install the gate in the current repository
    Install {
        /// The branch to protect
        #[arg(long)]
        protect: String,
        /// CI status command; `{rev}` is replaced by the commit. Exit 0 pass, 2 pending, else fail
        #[arg(long)]
        ci: String,
    },
    /// Run as the reference-transaction hook (installed by `reeve gate install`)
    Hook { state: String },
}

#[derive(Subcommand)]
enum Setup {
    /// Add hooks to ~/.claude/settings.json that run `reeve ls --brief` at session start and
    /// `reeve hook context` on prompts and tool calls, `reeve hook background` before Bash calls,
    /// and a skill stub at ~/.claude/skills/reeve/SKILL.md that points at `reeve --skill`
    Claude {
        /// Only report whether each piece is installed and current
        #[arg(long, conflicts_with = "remove")]
        check: bool,
        /// Take every piece out again, the CLAUDE.md block included
        #[arg(long)]
        remove: bool,
        /// Also add a short marked block to ~/.claude/CLAUDE.md that tells every session reeve is
        /// installed, when to use it, and to read `reeve --skill` for how
        #[arg(long, conflicts_with = "remove")]
        claude_md: bool,
    },
}

#[derive(Subcommand)]
enum Bench {
    /// Fresh `claude -p` sessions on four cases (long, short, successor, missing), with the
    /// choice each one made
    Entry {
        /// Runs per case
        #[arg(long, default_value_t = 1)]
        n: usize,
        /// Only these cases
        #[arg(long = "case", value_name = "CASE", value_parser = bench_entry::Case::parse)]
        cases: Vec<bench_entry::Case>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long, default_value_t = 8)]
        max_turns: u32,
        /// Seconds before a session is stopped
        #[arg(long, default_value_t = 240)]
        timeout: u64,
    },
    /// One run of an arm (bench/arms/<ARM>), named rv-<ARM>-<N>
    Run { arm: String, n: String },
    /// Aggregate results per arm
    Score { arms: Vec<String> },
    /// Done-check a branch of a repo and print it as JSON
    Check { repo: PathBuf, branch: String },
    /// One run of the handoff scenario (bench/handoff/arms/<ARM>), named rh-<ARM>-<N>
    Handoff {
        arm: String,
        n: String,
        /// Don't jump the clock; tasks last a sixtieth of their simulated time
        #[arg(long)]
        real: bool,
    },
    /// Aggregate handoff runs per arm
    HandoffScore { arms: Vec<String> },
    /// A handoff-scenario task: waits out its simulated duration, then prints its result
    Task { id: String },
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

/// A background session in Herdr is out of the user's sight, permission prompts included.
fn bg_in_herdr(who: &str) -> String {
    format!(
        "reeve: inside Herdr a {who} gets its own Herdr tab, where the user can see and answer it; drop --bg, or pass --force for a background session anyway"
    )
}

fn in_herdr() -> bool {
    std::env::var("HERDR_ENV").is_ok_and(|v| v == "1")
}

fn main() -> ExitCode {
    let matches = Cli::command().try_get_matches().unwrap_or_else(|e| {
        log_usage_error(&e);
        e.exit()
    });
    let path = command_path(&matches);
    let mut ev = Event::start(if path.is_empty() { "skill" } else { &path });
    let explicit = matches
        .subcommand_matches("run")
        .is_some_and(sandbox_opts_given);
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    // Runs on every prompt and tool call, so it stays out of the event log.
    if let Some(Top::Hook(Hook::Context)) = cli.command {
        if let Some(out) = context::hook(io::stdin()) {
            println!("{out}");
        }
        return ExitCode::SUCCESS;
    }
    if let Some(Top::Hook(Hook::Background)) = cli.command {
        if let Some(out) = background::hook(io::stdin()) {
            println!("{out}");
        }
        return ExitCode::SUCCESS;
    }
    let workers = || workers::Registry::default_dir().map(workers::Registry::at);
    let Some(command) = cli.command else {
        print!("{SKILL}");
        ev.finish(0, None);
        events::log(&ev);
        return ExitCode::SUCCESS;
    };
    // Every backend (pane, background session, foreground, sandbox) starts from here.
    let cd = match &command {
        Top::Run { cwd: Some(dir), .. } => std::env::set_current_dir(dir)
            .map_err(|e| io::Error::other(format!("--cwd {}: {e}", dir.display()))),
        _ => Ok(()),
    };
    let result: io::Result<i32> = match command {
        _ if cd.is_err() => cd.map(|_| 0),
        Top::Run { name: Some(_), .. } if explicit => {
            Err(io::Error::other("sandbox options need --sandbox"))
        }
        Top::Run {
            name: Some(_),
            bg: true,
            force: false,
            ..
        } if in_herdr() => {
            eprintln!("{}", bg_in_herdr("worker"));
            Ok(2)
        }
        Top::Run {
            name: Some(name),
            bg,
            pane,
            args,
            ..
        } => {
            ev.worker = Some(name.clone());
            let backend = workers::Backend::choose(pane, bg, in_herdr());
            if let Some(w) = std::env::current_dir()
                .ok()
                .and_then(|d| run::cwd_warning(&d))
            {
                eprintln!("reeve: {w}");
            }
            let started = workers().and_then(|r| {
                workers::run(&r, &name, backend, &args)?;
                r.get(&name)
            });
            if let Ok(w) = &started {
                ev.handoff_ms = Some(events::now_ms().saturating_sub(ev.ts));
                eprintln!("{}", workers::started(w));
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
        Top::Ls { brief: true } => {
            ev.label = Some("brief".into());
            workers()
                .and_then(|r| workers::ls_brief(&r))
                .map(|out| {
                    if !out.is_empty() {
                        println!("{out}");
                    }
                })
                .map(|_| 0)
        }
        Top::Handoff {
            bg: true,
            force: false,
            ..
        } if in_herdr() => {
            eprintln!("{}", bg_in_herdr("successor"));
            Ok(2)
        }
        Top::Handoff {
            name,
            notes,
            pane,
            bg,
            args,
            ..
        } => {
            let backend = workers::Backend::choose(pane, bg, in_herdr());
            workers()
                .and_then(|r| handoff::handoff(&r, &name, backend, &notes, &args))
                .map(|_| 0)
        }
        Top::Ls { brief: false } => workers()
            .and_then(|r| workers::ls(&r))
            .map(|out| println!("{out}"))
            .map(|_| 0),
        Top::Wait { name, timeout } => {
            ev.worker = Some(name.clone());
            workers()
                .and_then(|r| workers::wait(&r, &name, timeout))
                .map(|_| 0)
        }
        Top::Watch { names } => workers()
            .and_then(|r| workers::watch(&r, &names))
            .map(|all_done| i32::from(!all_done)),
        Top::Rm { name } => {
            ev.worker = Some(name.clone());
            workers().and_then(|r| workers::rm(&r, &name)).map(|_| 0)
        }
        Top::Start { name, tab, argv } => {
            ev.worker = Some(name.clone());
            workers()
                .and_then(|r| workers::start(&r, &name, tab.as_deref(), &argv))
                .map(|_| 0)
        }
        Top::Hook(Hook::Context | Hook::Background) => {
            unreachable!("answered before the event log")
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
        Top::Setup(Setup::Claude {
            check,
            remove,
            claude_md,
        }) => setup_claude(check, remove, claude_md),
        Top::Doctor => doctor::main(VERSION),
        Top::Bench(Bench::Entry {
            n,
            cases,
            model,
            max_turns,
            timeout,
        }) => {
            let cases = if cases.is_empty() {
                bench_entry::CASES.to_vec()
            } else {
                cases
            };
            let opts = bench_entry::Opts {
                n,
                cases,
                model,
                max_turns,
                timeout,
            };
            bench_entry::main(&opts, VERSION)
        }
        Top::Bench(Bench::Run { arm, n }) => bench::run::run(&arm, &n),
        Top::Bench(Bench::Handoff { arm, n, real }) => bench::handoff::run(&arm, &n, real),
        Top::Bench(Bench::HandoffScore { arms }) => bench::handoff::score(&arms)
            .map(|out| println!("{out}"))
            .map(|_| 0),
        Top::Bench(Bench::Task { id }) => bench::handoff::task(&id),
        Top::Bench(Bench::Score { arms }) => bench::score::score(&arms)
            .map(|t| println!("{t}"))
            .map(|_| 0),
        Top::Bench(Bench::Check { repo, branch }) => bench::check::check(&repo, &branch)
            .map(|c| println!("{c}"))
            .map(|_| 0),
        Top::Gate(Gate::Install { protect, ci }) => gate::install(&protect, &ci).map(|_| 0),
        Top::Ask { question } => ask::ask(&question),
        Top::Asks => ask::list(),
        Top::Answer { id, text } => ask::answer(&id, &text),
        Top::Gate(Gate::Hook { state }) => gate::hook(&state).map(|ok| if ok { 0 } else { 1 }),
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

fn setup_claude(check: bool, remove: bool, claude_md: bool) -> io::Result<i32> {
    let paths = setup::Paths::at(&setup::Paths::default_dir()?);
    if check {
        let pieces = setup::check(&paths, VERSION, claude_md)?;
        println!("{}", setup::render_check(&pieces, claude_md));
        return Ok(i32::from(
            pieces.iter().any(|(_, p)| *p != setup::Piece::Ok),
        ));
    }
    let now = events::now_ms() / 1000;
    let lines = if remove {
        setup::remove(&paths, now)?
    } else {
        setup::install(&paths, VERSION, claude_md, now)?
    };
    for l in lines {
        println!("{l}");
    }
    if !remove {
        println!("`reeve setup claude --remove` takes it all out again");
    }
    Ok(0)
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
    fn the_manual_for_agents_stays_under_1200_tokens() {
        assert!(SKILL.len() / 4 <= 1200, "about {} tokens", SKILL.len() / 4);
        for part in [
            "reeve run --name",
            "reeve ls",
            "reeve wait",
            "reeve report",
            "reeve rm NAME",
            "--cwd DIR",
            "-- --model sonnet \"",
            "`reeve heavy -- <command>`",
            "not a lock",
            "`reeve handoff --name NAME --notes FILE`",
            "/goal",
            "--permission-mode auto` after `--`",
            "blocked",
            "Context check",
            "reeve ask",
            "reeve asks",
            "reeve answer",
            "Inside Herdr each worker gets its own tab",
            "not Herdr's agent commands",
            "`--bg` is for that case only",
            "idle without a report",
            "reeve watch fix-flaky",
            "`reeve run --force`",
        ] {
            assert!(SKILL.contains(part), "{part}");
        }
        assert!(
            SKILL
                .lines()
                .all(|l| !l.starts_with("reeve run --name") || !l.contains("--bg"))
        );
        for part in [
            "Handoffs",
            "Sandbox",
            "reeve feedback",
            "When not to bother",
        ] {
            assert!(SKILL.contains(part), "{part}");
        }
        let lower = SKILL.to_lowercase();
        assert!(!lower.contains("must") && !lower.contains("instead of the agent tool"));
    }

    #[test]
    fn skill_stands_alone_and_bare_reeve_shows_help() {
        assert!(Cli::try_parse_from(["reeve", "--skill"]).unwrap().skill);
        assert!(Cli::try_parse_from(["reeve", "--skill", "ls"]).is_err());
        assert!(Cli::try_parse_from(["reeve"]).is_err());
        assert!(Cli::try_parse_from(["reeve", "setup", "claude", "--check", "--remove"]).is_err());
    }

    #[test]
    fn rm_the_hook_and_the_claude_md_block_parse() {
        let parse = |args: &[&str]| Cli::try_parse_from(["reeve"].iter().chain(args)).is_ok();
        assert!(parse(&["rm", "x"]));
        assert!(parse(&["hook", "context"]));
        assert!(parse(&["hook", "background"]));
        assert!(parse(&["setup", "claude", "--claude-md"]));
        assert!(parse(&["setup", "claude", "--check", "--claude-md"]));
        assert!(!parse(&["setup", "claude", "--remove", "--claude-md"]));
    }

    #[test]
    fn handoff_needs_a_successor_name_and_notes() {
        let parse =
            |args: &[&str]| Cli::try_parse_from(["reeve", "handoff"].iter().chain(args)).is_ok();
        assert!(parse(&[
            "--name", "next", "--notes", "n.md", "--", "--model", "sonnet"
        ]));
        assert!(parse(&["--name", "next", "--notes", "n.md", "--bg"]));
        assert!(parse(&[
            "--name", "next", "--notes", "n.md", "--bg", "--force"
        ]));
        assert!(!parse(&["--name", "next"]));
        assert!(!parse(&["--notes", "n.md"]));
        assert!(!parse(&[
            "--name", "next", "--notes", "n.md", "--pane", "--bg"
        ]));
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
