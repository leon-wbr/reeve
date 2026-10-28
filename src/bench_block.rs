//! `reeve bench block`: whether the CLAUDE.md block `reeve setup claude --claude-md` writes gets
//! fresh sessions to use reeve, as an orchestrator and as a worker under Herdr. It reuses the
//! entry bench's shim, workspace and transcript reading. Each session is isolated from the
//! owner's setup: only project settings load, so the owner's plugins, hooks and skills stay out,
//! as do MCP servers and slash commands; the workspaces live outside the home dir, so no
//! CLAUDE.md above them is read. The arm's text is the workspace's `.claude/CLAUDE.md`; there
//! is no skill stub and no session-start hook in any arm.

use crate::bench_entry::{self, Case as EntryCase};
use crate::{context, setup};
use serde_json::{Value, json};
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arm {
    /// No block.
    A0,
    /// The block setup writes now: when to use reeve, no syntax.
    A1,
    /// The block from before, with the commands' syntax.
    A2,
    /// A1 with the manual pasted after it.
    A3,
}

pub const ARMS: [Arm; 4] = [Arm::A0, Arm::A1, Arm::A2, Arm::A3];

/// `block()` as of a3575f4, the block in CLAUDE.md files written before the slim one.
pub const OLD_BLOCK: &str = "<!-- reeve setup claude: start -->\n## reeve\n\nreeve is installed on this machine.\n\n\
- Run test suites, e2e, builds and installs through `reeve heavy -- <command>`, e.g. `reeve heavy -- cargo test`, \
started in the background (Claude Code: `run_in_background`); keep working and you're told when it ends.\n\
- When a \"Context check\" tells you that you passed the handoff threshold (350K tokens of context unless set \
otherwise), write your notes (goal, what's done, what's next, open questions) to a file, run \
`reeve handoff --name NAME --notes FILE` with a new name for your successor, and stop.\n\
- Long or parallel work may go to named workers (`reeve run --name NAME --bg --cwd DIR -- \"PROMPT\"`).\n\
- Read `reeve --skill` before heavy, long or parallel work, or a handoff.\n\
<!-- reeve setup claude: end -->";

impl Arm {
    pub fn name(self) -> &'static str {
        match self {
            Arm::A0 => "a0",
            Arm::A1 => "a1",
            Arm::A2 => "a2",
            Arm::A3 => "a3",
        }
    }

    pub fn parse(s: &str) -> Result<Arm, String> {
        ARMS.into_iter()
            .find(|a| a.name() == s.to_lowercase())
            .ok_or_else(|| format!("no arm {s}; arms are a0, a1, a2, a3"))
    }

    /// The CLAUDE.md the session gets, if any.
    pub fn text(self, manual: &str) -> Option<String> {
        match self {
            Arm::A0 => None,
            Arm::A1 => Some(setup::block()),
            Arm::A2 => Some(OLD_BLOCK.into()),
            Arm::A3 => Some(format!(
                "{}\n\nThe output of `reeve --skill`:\n\n{manual}",
                setup::block()
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Orchestrator,
    Worker,
}

pub const ROLES: [Role; 2] = [Role::Orchestrator, Role::Worker];
const WORKER_NAME: &str = "tally-task";

impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Role::Orchestrator => "orch",
            Role::Worker => "worker",
        }
    }

    pub fn parse(s: &str) -> Result<Role, String> {
        ROLES
            .into_iter()
            .find(|r| r.name() == s)
            .ok_or_else(|| format!("no role {s}; roles are orch, worker"))
    }

    /// The user turn: the case's ask, framed for the role. A worker's brief ends the way
    /// `workers::with_report` ends it, without its sentence on `reeve heavy`, so that only the
    /// arm's text speaks of reeve.
    pub fn prompt(self, case: Case, report: &Path) -> String {
        match self {
            Role::Orchestrator => format!(
                "You're my orchestrator today: I'll hand you several tasks in this project and \
                 you coordinate them for me. First: {}",
                case.ask()
            ),
            Role::Worker => format!(
                "You are the named worker {WORKER_NAME}. An orchestrating session handed you this \
                 one task: {} When you are finished, write your final report as Markdown to {}. \
                 Write that file last and only once: its existence tells reeve that you are done.",
                case.ask(),
                report.display()
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Case {
    /// Run the suite: `reeve heavy` fits.
    Heavy,
    /// Two independent multi-step jobs: named workers fit.
    Parallel,
    /// A Context check arrives with the prompt: `reeve handoff` fits.
    Handoff,
    /// A one-line fix: no workers, and `reeve heavy` only if it runs the suite.
    Short,
}

pub const CASES: [Case; 4] = [Case::Heavy, Case::Parallel, Case::Handoff, Case::Short];

impl Case {
    pub fn name(self) -> &'static str {
        match self {
            Case::Heavy => "heavy",
            Case::Parallel => "parallel",
            Case::Handoff => "handoff",
            Case::Short => "short",
        }
    }

    pub fn parse(s: &str) -> Result<Case, String> {
        CASES
            .into_iter()
            .find(|c| c.name() == s)
            .ok_or_else(|| format!("no case {s}; cases are heavy, parallel, handoff, short"))
    }

    pub fn ask(self) -> &'static str {
        match self {
            Case::Heavy => "Run the test suite and tell me in one sentence whether it passes.",
            Case::Parallel => {
                "Two independent jobs, each several hours of work: (1) harden src/api.js \
                 against bad input, with property tests, running the suite many times to catch \
                 flakes; (2) make src/clock.js injectable and test every caller with a fake \
                 clock, again running the suite many times. Get both going in parallel, then \
                 tell me in two sentences what you set up."
            }
            Case::Handoff => {
                "Carry on with the audit of src/: next, add property tests for src/clock.js and \
                 run them."
            }
            Case::Short => {
                "Fix the typo \"recieve\" in README.md, then tell me in one sentence what you \
                 changed."
            }
        }
    }
}

/// What a session did, from the shim's log, the suite's log and the transcript.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Score {
    /// `reeve --skill` before any edit, subagent, reeve command that acts, or suite run.
    pub skill_first: bool,
    pub skill: bool,
    pub suite_heavy: usize,
    pub suite_direct: usize,
    /// `reeve run --name` calls that started a worker (not refused).
    pub workers: usize,
    /// `reeve run --name … --bg` calls without --force, which reeve refuses under Herdr.
    pub bg: usize,
    pub handoff: bool,
    pub subagent: bool,
    pub turns: Option<u64>,
    pub cost: Option<f64>,
}

fn acts(name: &str, cmd: &str) -> bool {
    match name {
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" | "Agent" | "Task" => true,
        "Bash" => {
            [
                "reeve run",
                "reeve heavy --",
                "reeve heavy -",
                "reeve handoff",
            ]
            .iter()
            .any(|c| cmd.contains(c))
                || ["npm test", "npm run test", "scripts/test.sh"]
                    .iter()
                    .any(|c| cmd.contains(c))
        }
        _ => false,
    }
}

pub fn score(events: &[Value], calls: &str, suite: &str) -> Score {
    let mut s = Score::default();
    let mut acted = false;
    for t in bench_entry::tool_uses(events) {
        let name = t["name"].as_str().unwrap_or_default();
        let cmd = t["input"]["command"].as_str().unwrap_or_default();
        if name == "Bash" && cmd.contains("reeve --skill") {
            s.skill = true;
            s.skill_first |= !acted;
        }
        acted |= acts(name, cmd);
        s.subagent |= matches!(name, "Agent" | "Task");
    }
    for l in calls.lines() {
        let args: Vec<&str> = l.split_whitespace().collect();
        let before_prompt = &args[..args.iter().position(|a| *a == "--").unwrap_or(args.len())];
        match args.first() {
            Some(&"--skill") => s.skill = true,
            Some(&"run") if before_prompt.contains(&"--name") => {
                if before_prompt.contains(&"--bg") && !before_prompt.contains(&"--force") {
                    s.bg += 1;
                } else {
                    s.workers += 1;
                }
            }
            Some(&"handoff") => s.handoff = true,
            _ => {}
        }
    }
    s.suite_heavy = suite.lines().filter(|l| *l == "heavy").count();
    s.suite_direct = suite.lines().filter(|l| *l == "direct").count();
    if let Some(r) = events.iter().find(|e| e["type"] == "result") {
        s.turns = r["num_turns"].as_u64();
        s.cost = r["total_cost_usd"].as_f64();
    }
    s
}

/// Whether the session did what the case calls for, with no false positive.
pub fn fits(case: Case, s: &Score) -> bool {
    let clean = s.bg == 0 && s.suite_direct == 0;
    clean
        && match case {
            Case::Heavy => s.suite_heavy > 0,
            Case::Parallel => s.workers > 0,
            Case::Handoff => s.handoff,
            Case::Short => s.workers == 0 && !s.handoff,
        }
}

/// Reeve used where the case doesn't call for it: workers or a handoff on the short case, or
/// any refused `--bg`.
pub fn false_positive(case: Case, s: &Score) -> bool {
    s.bg > 0 || (case == Case::Short && (s.workers > 0 || s.handoff))
}

pub struct Row {
    pub arm: Arm,
    pub role: Role,
    pub case: Case,
    pub run: usize,
    pub score: Score,
}

fn yes(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

/// One line per run, tab-separated.
pub fn tsv(rows: &[Row]) -> String {
    let mut out = vec![
        "arm\trole\tcase\trun\tfits\tskill_first\tskill\tsuite_heavy\tsuite_direct\tworkers\tbg\thandoff\tsubagent\tturns\tcost_usd"
            .to_string(),
    ];
    for r in rows {
        let s = &r.score;
        out.push(format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            r.arm.name(),
            r.role.name(),
            r.case.name(),
            r.run,
            yes(fits(r.case, s)),
            yes(s.skill_first),
            yes(s.skill),
            s.suite_heavy,
            s.suite_direct,
            s.workers,
            s.bg,
            yes(s.handoff),
            yes(s.subagent),
            s.turns.map_or("-".into(), |t| t.to_string()),
            s.cost.map_or("-".into(), |c| format!("{c:.3}")),
        ));
    }
    out.join("\n") + "\n"
}

/// One line per arm and role: fits per case as k/n, then the shared columns.
pub fn render(rows: &[Row]) -> String {
    let mut out = vec!["| Arm | Role | heavy | parallel | handoff | short | skill first | --bg refused | false pos. | turns | cost $ |".to_string()];
    out.push("|---|---|---|---|---|---|---|---|---|---|---|".into());
    let mut total = 0.0;
    for arm in ARMS {
        for role in ROLES {
            let of: Vec<&Row> = rows
                .iter()
                .filter(|r| r.arm == arm && r.role == role)
                .collect();
            if of.is_empty() {
                continue;
            }
            let per_case = CASES.map(|c| {
                let rs: Vec<_> = of.iter().filter(|r| r.case == c).collect();
                if rs.is_empty() {
                    "-".to_string()
                } else {
                    let k = rs.iter().filter(|r| fits(c, &r.score)).count();
                    format!("{k}/{}", rs.len())
                }
            });
            let count = |f: &dyn Fn(&Row) -> bool| of.iter().filter(|r| f(r)).count();
            let n = of.len();
            let turns: Vec<u64> = of.iter().filter_map(|r| r.score.turns).collect();
            let cost: f64 = of.iter().filter_map(|r| r.score.cost).sum();
            total += cost;
            out.push(format!(
                "| {} | {} | {} | {}/{n} | {} | {}/{n} | {:.1} | {:.2} |",
                arm.name().to_uppercase(),
                role.name(),
                per_case.join(" | "),
                count(&|r| r.score.skill_first),
                of.iter().map(|r| r.score.bg).sum::<usize>(),
                count(&|r| false_positive(r.case, &r.score)),
                turns.iter().sum::<u64>() as f64 / turns.len().max(1) as f64,
                cost / n as f64,
            ));
        }
    }
    out.push(String::new());
    out.push(format!(
        "Cells are runs that fit out of runs; turns and cost are means per run. Total cost: ${total:.2}."
    ));
    out.join("\n")
}

const SUITE: &str = "#!/bin/sh\n\
# Runs every test file; slow on purpose, like a real suite.\n\
if [ -n \"$REEVE_HEAVY_JOB\" ]; then w=heavy; else w=direct; fi\n\
echo $w >> \"$(git rev-parse --git-dir)/suite-runs\"\n\
sleep 5\n\
for f in test/*.test.js; do node \"$f\" || exit 1; done\n\
echo \"all tests passed\"\n";

/// The entry bench's workspace as a git repo with a test script.
fn workspace(ws: &Path, claude_md: Option<&str>) -> io::Result<()> {
    bench_entry::workspace(ws)?;
    fs::create_dir_all(ws.join("scripts"))?;
    fs::write(ws.join("scripts/test.sh"), SUITE)?;
    fs::write(
        ws.join("package.json"),
        "{\n  \"name\": \"tally\",\n  \"type\": \"module\",\n  \"scripts\": { \"test\": \"sh scripts/test.sh\" }\n}\n",
    )?;
    let git = |args: &[&str]| -> io::Result<()> {
        let ok = Command::new("git")
            .args(args)
            .current_dir(ws)
            .stdout(Stdio::null())
            .status()?
            .success();
        if ok {
            Ok(())
        } else {
            Err(io::Error::other(format!("git {args:?} failed")))
        }
    };
    git(&["init", "-q", "-b", "main"])?;
    git(&["add", "."])?;
    git(&[
        "-c",
        "user.name=bench",
        "-c",
        "user.email=bench@localhost",
        "commit",
        "-qm",
        "tally",
    ])?;
    if let Some(text) = claude_md {
        fs::create_dir_all(ws.join(".claude"))?;
        fs::write(ws.join(".claude/CLAUDE.md"), text)?;
    }
    Ok(())
}

/// The flags that keep the owner's setup out of a session.
pub const ISOLATION: [&str; 5] = [
    "--setting-sources=project",
    "--strict-mcp-config",
    "--disable-slash-commands",
    "--no-session-persistence",
    "--no-chrome",
];

/// Variables of the caller's own session, Herdr pane or reeve that must not reach a bench
/// session.
fn inherited(key: &str) -> bool {
    [
        "HERDR_",
        "REEVE_",
        "CLAUDE_CODE_",
        "CLAUDE_PID",
        "CLAUDE_EFFORT",
        "CLAUDECODE",
    ]
    .iter()
    .any(|p| key.starts_with(p))
}

/// The ones given, or all of them.
pub fn or_all<T: Copy>(given: Vec<T>, all: &[T]) -> Vec<T> {
    if given.is_empty() {
        all.to_vec()
    } else {
        given
    }
}

pub struct Opts {
    pub n: usize,
    pub arms: Vec<Arm>,
    pub roles: Vec<Role>,
    pub cases: Vec<Case>,
    pub model: String,
    pub max_turns: u32,
    pub timeout: u64,
    pub jobs: usize,
    pub manual: String,
}

fn run_one(arm: Arm, role: Role, case: Case, dir: &Path, o: &Opts) -> io::Result<Score> {
    let ws = dir.join("ws");
    workspace(&ws, arm.text(&o.manual).as_deref())?;
    let bin = dir.join("bin");
    fs::create_dir_all(&bin)?;
    let entry_case = if case == Case::Short {
        EntryCase::Short
    } else {
        EntryCase::Long
    };
    for (name, text) in [
        (
            "reeve",
            bench_entry::shim(entry_case, dir, &std::env::current_exe()?),
        ),
        ("herdr", bench_entry::herdr_shim(dir)),
    ] {
        fs::write(bin.join(name), text)?;
        fs::set_permissions(bin.join(name), fs::Permissions::from_mode(0o755))?;
    }
    // First on PATH, the shims shadow the real reeve and herdr without hiding the rest of
    // their directories: claude often sits beside herdr.
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let report = dir
        .join("state/reeve/reports")
        .join(format!("{WORKER_NAME}.md"));
    let mut c = Command::new("claude");
    c.args(["-p", &role.prompt(case, &report)])
        .args(["--output-format", "stream-json", "--verbose"])
        .args(ISOLATION)
        .args(["--model", &o.model])
        .args(["--max-turns", &o.max_turns.to_string()])
        .args(["--permission-mode", "acceptEdits"])
        .args([
            "--allowedTools",
            "Bash Read Edit Write Glob Grep Agent Task",
        ]);
    if case == Case::Handoff {
        let notice = context::output("UserPromptSubmit", &context::text(361_000, 350_000));
        fs::write(dir.join("notice.json"), notice.to_string())?;
        let hook = format!(
            "cat {}",
            bench_entry::sh_quote(&dir.join("notice.json").display().to_string())
        );
        let settings = json!({"hooks": {"UserPromptSubmit": [{"hooks": [{"type": "command", "command": hook}]}]}});
        c.args(["--settings", &settings.to_string()]);
    }
    for (k, _) in std::env::vars() {
        if inherited(&k) {
            c.env_remove(k);
        }
    }
    let mut child = c
        .current_dir(&ws)
        .env("PATH", path)
        .env("HERDR_ENV", "1")
        .env("HERDR_BIN_PATH", bin.join("herdr"))
        .env("HERDR_SOCKET_PATH", dir.join("herdr.sock"))
        .env("XDG_STATE_HOME", dir.join("state"))
        .env("REEVE_EVENTS", dir.join("events.jsonl"))
        .env("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1")
        .stdin(Stdio::null())
        .stdout(fs::File::create(dir.join("session.jsonl"))?)
        .stderr(fs::File::create(dir.join("stderr.txt"))?)
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(o.timeout);
    while child.try_wait()?.is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let events: Vec<Value> = fs::read_to_string(dir.join("session.jsonl"))?
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let read = |p: PathBuf| fs::read_to_string(p).unwrap_or_default();
    Ok(score(
        &events,
        &read(dir.join("calls.log")),
        &read(ws.join(".git/suite-runs")),
    ))
}

/// Instruction files Claude Code would load from above a workspace in `dir`: it reads
/// `CLAUDE.md` and `.claude/CLAUDE.md` in every ancestor as project instructions, whatever
/// `--setting-sources` says, so a bench under the home dir reads the owner's own CLAUDE.md.
pub fn ancestor_instructions(dir: &Path) -> Vec<PathBuf> {
    dir.ancestors()
        .flat_map(|a| [a.join("CLAUDE.md"), a.join(".claude/CLAUDE.md")])
        .filter(|p| p.exists())
        .collect()
}

pub fn main(o: &Opts) -> io::Result<i32> {
    let root = std::env::temp_dir().join("reeve-bench-block").join(format!(
        "{}-{}",
        crate::events::now_ms(),
        std::process::id()
    ));
    let mut queue = vec![];
    for &arm in &o.arms {
        for &role in &o.roles {
            for &case in &o.cases {
                for i in 1..=o.n {
                    queue.push((arm, role, case, i));
                }
            }
        }
    }
    fs::create_dir_all(&root)?;
    let leaks = ancestor_instructions(&root);
    if !leaks.is_empty() {
        return Err(io::Error::other(format!(
            "{} would reach every session; move the bench root ({}) elsewhere",
            leaks
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
            root.display()
        )));
    }
    queue.reverse();
    let total = queue.len();
    let queue = Mutex::new(queue);
    let rows = Mutex::new(vec![]);
    std::thread::scope(|s| {
        for _ in 0..o.jobs.max(1) {
            s.spawn(|| {
                loop {
                    let Some((arm, role, case, i)) = queue.lock().unwrap().pop() else {
                        break;
                    };
                    let tag = format!("{}-{}-{}-{i}", arm.name(), role.name(), case.name());
                    let dir = root.join(&tag);
                    match run_one(arm, role, case, &dir, o) {
                        Ok(score) => {
                            let mut rows = rows.lock().unwrap();
                            rows.push(Row {
                                arm,
                                role,
                                case,
                                run: i,
                                score,
                            });
                            eprintln!("reeve bench: {}/{total} {tag}", rows.len());
                        }
                        Err(e) => eprintln!("reeve bench: {tag} failed: {e}"),
                    }
                }
            });
        }
    });
    let mut rows = rows.into_inner().unwrap();
    let key = |r: &Row| {
        (
            ARMS.iter().position(|a| *a == r.arm),
            ROLES.iter().position(|x| *x == r.role),
            CASES.iter().position(|c| *c == r.case),
            r.run,
        )
    };
    rows.sort_by_key(key);
    fs::write(root.join("runs.tsv"), tsv(&rows))?;
    println!("{}", render(&rows));
    println!("\nsessions and runs.tsv: {}", root.display());
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bash(cmd: &str) -> Value {
        json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "name": "Bash", "input": {"command": cmd}}
        ]}})
    }

    fn edit() -> Value {
        json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "name": "Edit", "input": {}}
        ]}})
    }

    #[test]
    fn the_manual_counts_as_first_only_before_any_action() {
        let first = score(
            &[bash("ls"), bash("reeve --skill"), edit()],
            "--skill\n",
            "",
        );
        assert!(first.skill && first.skill_first);
        let late = score(
            &[bash("npm test"), bash("reeve --skill")],
            "--skill\n",
            "direct\n",
        );
        assert!(late.skill && !late.skill_first);
        let none = score(&[edit()], "", "");
        assert!(!none.skill && !none.skill_first);
    }

    #[test]
    fn workers_count_from_the_shim_log_and_a_refused_bg_is_not_one() {
        let calls = "run --name a --bg -- \"go\"\nrun --name a -- \"go --bg\"\nrun --name b --bg --force -- go\nls\n";
        let s = score(&[], calls, "");
        assert_eq!((s.workers, s.bg), (2, 1));
        assert!(!fits(Case::Parallel, &s));
        assert!(false_positive(Case::Heavy, &s));
        let clean = score(&[], "run --name a -- go\n", "");
        assert!(fits(Case::Parallel, &clean) && !false_positive(Case::Parallel, &clean));
        assert!(!fits(Case::Short, &clean) && false_positive(Case::Short, &clean));
    }

    #[test]
    fn the_suite_fits_only_through_heavy() {
        let heavy = score(&[], "heavy -- npm test\n", "heavy\n");
        assert!(fits(Case::Heavy, &heavy));
        let both = score(&[], "heavy -- npm test\n", "heavy\ndirect\n");
        assert!(!fits(Case::Heavy, &both));
        assert!(fits(Case::Short, &score(&[], "", "")));
        assert!(!fits(Case::Short, &score(&[], "", "direct\n")));
        assert!(fits(Case::Short, &heavy));
    }

    #[test]
    fn a_handoff_and_the_result_line_are_read() {
        let events = [json!({"type": "result", "num_turns": 4, "total_cost_usd": 0.25})];
        let s = score(&events, "--skill\nhandoff --name next --notes n.md\n", "");
        assert!(s.handoff && fits(Case::Handoff, &s));
        assert_eq!((s.turns, s.cost), (Some(4), Some(0.25)));
    }

    #[test]
    fn each_arm_carries_its_own_text() {
        assert_eq!(Arm::A0.text("M"), None);
        let a1 = Arm::A1.text("M").unwrap();
        assert_eq!(a1, setup::block());
        let a2 = Arm::A2.text("M").unwrap();
        assert!(a2.contains("reeve heavy -- <command>") && a2.contains("--bg"));
        assert!(a2.starts_with(setup::BLOCK_START) && a2.ends_with(setup::BLOCK_END));
        let a3 = Arm::A3.text("MANUAL").unwrap();
        assert!(a3.starts_with(&a1) && a3.ends_with("MANUAL"));
        assert_eq!(Arm::parse("A2"), Ok(Arm::A2));
    }

    #[test]
    fn the_worker_brief_names_the_report_and_not_heavy() {
        let p = Role::Worker.prompt(Case::Short, Path::new("/r/tally-task.md"));
        assert!(p.contains("/r/tally-task.md") && p.contains("recieve"));
        assert!(!p.contains("reeve heavy"));
        let o = Role::Orchestrator.prompt(Case::Heavy, Path::new("/r"));
        assert!(o.contains("orchestrator") && !o.contains("/r"));
    }

    #[test]
    fn instruction_files_above_a_workspace_are_found() {
        let d = std::env::temp_dir().join(format!("reeve-bench-anc-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("a/.claude")).unwrap();
        fs::create_dir_all(d.join("a/b/c")).unwrap();
        assert!(
            ancestor_instructions(&d.join("a/b/c"))
                .iter()
                .all(|p| !p.starts_with(&d))
        );
        fs::write(d.join("a/.claude/CLAUDE.md"), "x").unwrap();
        fs::write(d.join("a/b/CLAUDE.md"), "x").unwrap();
        let found: Vec<_> = ancestor_instructions(&d.join("a/b/c"))
            .into_iter()
            .filter(|p| p.starts_with(&d))
            .collect();
        assert_eq!(
            found,
            [d.join("a/b/CLAUDE.md"), d.join("a/.claude/CLAUDE.md")]
        );
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn the_callers_session_and_herdr_variables_stay_out() {
        for k in [
            "HERDR_SOCKET_PATH",
            "REEVE_HANDOFF_TOKENS",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDECODE",
        ] {
            assert!(inherited(k), "{k}");
        }
        for k in ["PATH", "HOME", "ANTHROPIC_API_KEY"] {
            assert!(!inherited(k), "{k}");
        }
    }

    #[test]
    fn the_table_tallies_fits_per_case_and_cost() {
        let s = |calls: &str, suite: &str, cost: f64| {
            let mut s = score(&[], calls, suite);
            s.cost = Some(cost);
            s.turns = Some(3);
            s
        };
        let rows = [
            Row {
                arm: Arm::A1,
                role: Role::Worker,
                case: Case::Heavy,
                run: 1,
                score: s("", "heavy\n", 0.1),
            },
            Row {
                arm: Arm::A1,
                role: Role::Worker,
                case: Case::Short,
                run: 1,
                score: s("run --name x -- go\n", "", 0.3),
            },
        ];
        let out = render(&rows);
        assert!(
            out.contains("| A1 | worker | 1/1 | - | - | 0/1 | 0/2 | 0 | 1/2 | 3.0 | 0.20 |"),
            "{out}"
        );
        assert!(out.contains("Total cost: $0.40"), "{out}");
        assert!(
            tsv(&rows)
                .lines()
                .nth(2)
                .unwrap()
                .starts_with("a1\tworker\tshort\t1\tno")
        );
    }
}
