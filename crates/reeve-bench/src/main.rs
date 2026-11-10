//! `reeve-bench`: benchmarks agent setups on fixed scenarios with a simulated user, and measures
//! how agents choose with reeve installed. It builds on reeve's library and runs the real `reeve`
//! binary installed beside it.

mod bench;
mod bench_block;
mod bench_entry;
mod bench_wake;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "reeve-bench", version, arg_required_else_help = true)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
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
    /// Fresh `claude -p` sessions isolated from your setup, with only a CLAUDE.md block or the
    /// skill stub (arms a0-a6) as an orchestrator and as a worker under Herdr, on five cases
    /// (heavy, parallel, handoff, short, away), scored on whether they use reeve where it fits
    Block {
        /// Runs per arm, role and case
        #[arg(long, default_value_t = 1)]
        n: usize,
        #[arg(long = "arm", value_name = "ARM", value_parser = bench_block::Arm::parse)]
        arms: Vec<bench_block::Arm>,
        #[arg(long = "role", value_name = "ROLE", value_parser = bench_block::Role::parse)]
        roles: Vec<bench_block::Role>,
        #[arg(long = "case", value_name = "CASE", value_parser = bench_block::Case::parse)]
        cases: Vec<bench_block::Case>,
        #[arg(long, default_value = "sonnet")]
        model: String,
        #[arg(long, default_value_t = 15)]
        max_turns: u32,
        /// Seconds before a session is stopped
        #[arg(long, default_value_t = 300)]
        timeout: u64,
        /// Sessions at once
        #[arg(long, default_value_t = 4)]
        jobs: usize,
    },
    /// One run of an arm (bench/arms/<ARM>), named rv-<ARM>-<N>
    Run { arm: String, n: String },
    /// Aggregate results per arm
    Score { arms: Vec<String> },
    /// One run of the day scenario (bench/day/arms/<ARM>), named rd-<ARM>-<N>
    Day {
        arm: String,
        n: String,
        /// Model the scripted user falls back to for questions no rule covers
        #[arg(long, default_value = "haiku")]
        sim_model: String,
        /// Seconds the integration suite holds the test database
        #[arg(long)]
        hold: Option<u64>,
        /// Seconds the fixture rebuild holds it
        #[arg(long)]
        rebuild: Option<u64>,
    },
    /// Aggregate day runs per arm, without runs marked invalid unless asked
    DayScore {
        arms: Vec<String>,
        #[arg(long)]
        with_invalid: bool,
    },
    /// One run of the pressure scenario (bench/pressure/arms/<ARM>), named rp-<ARM>-<N>
    Pressure { arm: String, n: String },
    /// Per arm, median [min–max] of the pressure runs, tab-separated
    PressureScore { arms: Vec<String> },
    /// Done-check a branch of a repo and print it as JSON
    Check { repo: PathBuf, branch: String },
    /// Replay your orchestrator sessions from local Claude Code transcripts: wake-ups per hour
    /// and cost per day as they ran and with the quiet `reeve watch`. Prints
    /// totals only
    Wake {
        /// Where Claude Code keeps transcripts [default: ~/.claude/projects]
        #[arg(long)]
        projects: Option<PathBuf>,
        /// Only requests from this UTC date on, as YYYY-MM-DD
        #[arg(long)]
        since: String,
        /// The quiet watch's --stall, in minutes
        #[arg(long, default_value_t = 10)]
        stall: u64,
    },
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
    /// Exit 2 with reeve's own usage error if ARGS wouldn't run; the block bench's shim asks
    /// this before it fakes a call
    #[command(hide = true)]
    Parse {
        #[arg(last = true)]
        args: Vec<String>,
    },
}

fn day_opts(
    arm: String,
    n: String,
    sim_model: String,
    hold_s: Option<u64>,
    rebuild_s: Option<u64>,
) -> std::io::Result<bench::day::Opts> {
    let claude = std::env::var_os("REEVE_BENCH_CLAUDE")
        .map(PathBuf::from)
        .or_else(|| bench::day::which("claude", None))
        .ok_or_else(|| std::io::Error::other("no claude on PATH"))?;
    Ok(bench::day::Opts {
        arm,
        n,
        root: bench::root(),
        trials: bench::trials(),
        claude,
        reeve: Some(bench::reeve_bin()?),
        home: bench::home(),
        timing: bench::day::Timing::default(),
        hold_s,
        rebuild_s,
        sim_model,
        extra_env: vec![],
        arm_json: None,
    })
}

fn pressure_opts(arm: String, n: String) -> std::io::Result<bench::pressure::Opts> {
    let claude = std::env::var_os("REEVE_BENCH_CLAUDE")
        .map(PathBuf::from)
        .or_else(|| bench::day::which("claude", None))
        .ok_or_else(|| std::io::Error::other("no claude on PATH"))?;
    Ok(bench::pressure::Opts {
        arm,
        n,
        root: bench::root(),
        trials: bench::trials(),
        claude,
        reeve: bench::reeve_bin()?,
        home: bench::home(),
        tick_ms: 2_000,
        quiet_ms: 3 * 60_000,
        total_ms: 30 * 60_000,
        sample_ms: 1_000,
        extra_env: vec![],
        arm_json: None,
    })
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Cmd::Entry {
            n,
            cases,
            model,
            max_turns,
            timeout,
        } => {
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
            bench_entry::main(&opts, reeve::VERSION)
        }
        Cmd::Block {
            n,
            arms,
            roles,
            cases,
            model,
            max_turns,
            timeout,
            jobs,
        } => {
            let opts = bench_block::Opts {
                n,
                arms: bench_block::or_all(arms, &bench_block::ARMS),
                roles: bench_block::or_all(roles, &bench_block::ROLES),
                cases: bench_block::or_all(cases, &bench_block::CASES),
                model,
                max_turns,
                timeout,
                jobs,
                manual: reeve::SKILL.into(),
            };
            bench_block::main(&opts)
        }
        Cmd::Run { arm, n } => bench::run::run(&arm, &n),
        Cmd::Day {
            arm,
            n,
            sim_model,
            hold,
            rebuild,
        } => day_opts(arm, n, sim_model, hold, rebuild).and_then(|o| bench::day::run(&o)),
        Cmd::DayScore { arms, with_invalid } => {
            bench::day::score(&bench::trials(), &arms, with_invalid)
                .map(|t| println!("{t}"))
                .map(|_| 0)
        }
        Cmd::Pressure { arm, n } => pressure_opts(arm, n).and_then(|o| bench::pressure::run(&o)),
        Cmd::PressureScore { arms } => bench::pressure::score(&bench::trials(), &arms)
            .map(|t| println!("{t}"))
            .map(|_| 0),
        Cmd::Handoff { arm, n, real } => bench::handoff::run(&arm, &n, real),
        Cmd::HandoffScore { arms } => bench::handoff::score(&arms)
            .map(|out| println!("{out}"))
            .map(|_| 0),
        Cmd::Task { id } => bench::handoff::task(&id),
        Cmd::Parse { args } => {
            return match reeve::usage_error(args) {
                Some(e) => {
                    eprintln!("{}", e.trim_end());
                    ExitCode::from(2)
                }
                None => ExitCode::SUCCESS,
            };
        }
        Cmd::Score { arms } => bench::score::score(&arms)
            .map(|t| println!("{t}"))
            .map(|_| 0),
        Cmd::Check { repo, branch } => bench::check::check(&repo, &branch)
            .map(|c| println!("{c}"))
            .map(|_| 0),
        Cmd::Wake {
            projects,
            since,
            stall,
        } => match bench_wake::parse_ts(&format!("{since}T00:00:00Z")) {
            None => Err(std::io::Error::other("--since takes YYYY-MM-DD")),
            Some(since_ms) => bench_wake::main(&bench_wake::Opts {
                projects: projects.unwrap_or_else(|| bench::home().join(".claude/projects")),
                since_ms,
                stall: std::time::Duration::from_secs(stall * 60),
            }),
        },
    };
    match result {
        Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
        Err(e) => {
            eprintln!("reeve-bench: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_parse_shim_takes_what_follows_the_double_dash() {
        let shim = Cli::try_parse_from(["reeve-bench", "parse", "--", "run", "--", "-x"]);
        assert!(shim.is_ok());
        Cli::command().debug_assert();
    }
}
