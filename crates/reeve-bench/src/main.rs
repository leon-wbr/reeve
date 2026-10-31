//! `reeve-bench`: benchmarks agent setups on fixed scenarios with a simulated user, and measures
//! how agents choose with reeve installed. It builds on reeve's library and runs the real `reeve`
//! binary installed beside it.

mod bench;
mod bench_block;
mod bench_entry;

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
    /// Exit 2 with reeve's own usage error if ARGS wouldn't run; the block bench's shim asks
    /// this before it fakes a call
    #[command(hide = true)]
    Parse {
        #[arg(last = true)]
        args: Vec<String>,
    },
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
