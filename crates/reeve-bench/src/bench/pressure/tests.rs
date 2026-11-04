use super::*;

#[test]
fn the_ledger_counts_a_job_done_once() {
    let ids: Vec<String> = ["w1-1", "w1-2", "w2-1"].map(String::from).to_vec();
    let ledger = [
        r#"{"t":10,"id":"w1-1","pid":1,"event":"start"}"#,
        r#"{"t":20,"id":"w1-1","pid":1,"event":"done"}"#,
        r#"{"t":21,"id":"w1-1","pid":3,"event":"start"}"#,
        r#"{"t":30,"id":"w1-1","pid":3,"event":"done"}"#,
        r#"{"t":31,"id":"w1-2","pid":4,"event":"start"}"#,
        "not json",
    ]
    .join("\n");
    let j = jobs(&ledger, &ids);
    assert_eq!(
        j,
        Jobs {
            completed: 1,
            started: 3,
            live: 1,
            last_t: Some(31),
            last_done: Some(30)
        }
    );
}

const PS: &str = "100 1 1000 /opt/claude/claude --bg\n\
101 100 50 /bin/zsh -c reeve heavy -- node scripts/job.mjs --id w1-1\n\
102 101 20 /x/reeve heavy -- node scripts/job.mjs --id w1-1\n\
103 102 600000 node scripts/job.mjs --id w1-1\n\
110 100 50 /bin/zsh -c reeve heavy -- node scripts/job.mjs --id w1-2\n\
111 110 20 reeve heavy -- node scripts/job.mjs --id w1-2\n\
200 1 9999 /opt/claude/claude\n\
300 1 2000 claude\n\
400 1 7777 node scripts/job.mjs\n\
500 999 10 reeve-bench pressure\n\
501 500 10 lsof";

const LSOF: &str = "p100\nfcwd\nn/trials/rp-x-1\np200\nfcwd\nn/elsewhere\np300\nfcwd\nn/trials/rp-x-1/sub\np400\nfcwd\nn/trials/rp-x-10\np500\nfcwd\nn/trials/rp-x-1";

#[test]
fn a_sample_sums_the_processes_in_the_run_and_their_children() {
    let all = parse_ps(PS);
    let run = in_run(&all, LSOF, Path::new("/trials/rp-x-1"), 500);
    let pids: Vec<u32> = run.iter().map(|p| p.pid).collect();
    assert_eq!(pids, [100, 101, 102, 103, 110, 111, 300]);
    let s = sample(&run);
    assert_eq!(
        s,
        Sample {
            mem_kb: 603_140,
            jobs_mem_kb: 600_000,
            sessions: 2,
            jobs: 1,
            procs: 7
        }
    );
}

#[test]
fn memory_is_tops_footprint_where_top_has_one() {
    let top =
        "Processes: 9 total\nPID    MEM\n103    614M\n100    1.5G+\n101    12K-\n102    900B\n";
    let fp = parse_top(top);
    assert_eq!(fp.get(&103), Some(&628_736));
    assert_eq!(fp.get(&100), Some(&1_572_864));
    assert_eq!(fp.get(&101), Some(&12));
    assert_eq!(fp.get(&102), Some(&0));
    let mut all = parse_ps(PS);
    footprints(&mut all, &fp);
    let mem: Vec<u64> = all
        .iter()
        .filter(|p| [103, 110].contains(&p.pid))
        .map(|p| p.mem_kb)
        .collect();
    assert_eq!(
        mem,
        [628_736, 50],
        "the job's footprint; RSS where top has none"
    );
}

#[test]
fn the_reaper_kills_the_newest_heavy_callers_task_and_what_runs_under_it() {
    let all = parse_ps(PS);
    let run = in_run(&all, LSOF, Path::new("/trials/rp-x-1"), 500);
    assert_eq!(reap_target(&all, &run), [110, 111]);
    let older: Vec<Proc> = run.iter().filter(|p| p.pid < 110).cloned().collect();
    assert_eq!(
        reap_target(&all, &older),
        [101, 102, 103],
        "a job under its caller dies with it"
    );
    let none: Vec<Proc> = run
        .iter()
        .filter(|p| p.pid == 100 || p.pid == 103)
        .cloned()
        .collect();
    assert!(
        reap_target(&all, &none).is_empty(),
        "a job no caller runs is left alone"
    );
}

/// A detached heavy runner (reparented to launchd, cwd in the run) is no session's background
/// task, so Claude Code's reaper can't reach it; the stand-in mustn't either.
#[test]
fn the_reaper_only_kills_heavy_callers_under_a_session() {
    let ps = format!(
        "{PS}\n120 1 20 /x/reeve heavy __run /s/jobs/1\n121 120 600000 node scripts/job.mjs --id w2-1"
    );
    let lsof = format!("{LSOF}\np120\nfcwd\nn/trials/rp-x-1");
    let all = parse_ps(&ps);
    let run = in_run(&all, &lsof, Path::new("/trials/rp-x-1"), 500);
    assert!(
        run.iter().any(|p| p.pid == 121),
        "its job still counts in the run"
    );
    assert_eq!(reap_target(&all, &run), [110, 111]);
}

/// Plain Claude Code runs a job straight from its Bash tool, a shell under the session: with no
/// heavy caller to take, the reaper takes the newest such job's shell.
#[test]
fn without_a_heavy_caller_the_reaper_kills_the_newest_job_a_session_runs_itself() {
    let ps = "100 1 1000 /opt/claude/claude -p\n\
130 100 50 /bin/zsh -c node scripts/job.mjs --id w1-1\n\
131 130 600000 node scripts/job.mjs --id w1-1\n\
140 100 50 /bin/zsh -c node scripts/job.mjs --id w2-1\n\
141 140 600000 node scripts/job.mjs --id w2-1\n\
150 1 600000 node scripts/job.mjs --id w3-1";
    let lsof = "p100\nfcwd\nn/trials/rp-v-1\np150\nfcwd\nn/trials/rp-v-1";
    let all = parse_ps(ps);
    let run = in_run(&all, lsof, Path::new("/trials/rp-v-1"), 500);
    assert_eq!(reap_target(&all, &run), [140, 141]);
    let first: Vec<Proc> = run.iter().filter(|p| p.pid < 140).cloned().collect();
    assert_eq!(reap_target(&all, &first), [130, 131]);
    let outside: Vec<Proc> = run.iter().filter(|p| p.pid == 150).cloned().collect();
    assert!(
        reap_target(&all, &outside).is_empty(),
        "a job under no session is out of reach"
    );
}

#[test]
fn the_vanilla_prompt_asks_for_one_subagent_per_worker_and_no_reeve() {
    let ids = |w: usize| (1..=2).map(|j| format!("w{w}-{j}")).collect::<Vec<_>>();
    let p = vanilla_prompt(&[ids(1), ids(2)]);
    assert!(p.contains("2 subagents"), "{p}");
    assert!(p.contains("- `node scripts/job.mjs --id w2-2`"), "{p}");
    assert!(!p.contains("reeve"), "{p}");
}

#[test]
fn a_job_holds_its_memory_logs_its_run_and_clears_its_live_file() {
    let t = tempfile::tempdir().unwrap();
    let (dir, ledger) = (t.path().join("repo"), t.path().join("ledger"));
    copy_dir(&super::super::root().join("pressure/repo"), &dir).unwrap();
    let cfg = json!({ "ledger": ledger, "mb": 30, "seconds": 1 });
    fs::write(dir.join("job.json"), cfg.to_string()).unwrap();
    let mut a = Command::new("node")
        .arg(dir.join("scripts/job.mjs"))
        .args(["--id", "a"])
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let log = ledger.join("events.jsonl");
    for _ in 0..100 {
        if fs::read_to_string(&log).is_ok_and(|l| l.contains("\"start\"")) {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(live_jobs(&ledger.join("live")), 1);
    assert!(a.wait().unwrap().success());
    let j = jobs(&fs::read_to_string(&log).unwrap(), &["a".to_string()]);
    assert_eq!((j.completed, j.live), (1, 0));
    assert_eq!(live_jobs(&ledger.join("live")), 0);
}

const FAKE_CLAUDE: &str = r#"#!/bin/bash
# A fake claude for the pressure bench: a background worker runs the jobs its prompt names.
case "$1" in
  agents) echo '[]'; exit 0 ;;
  stop|kill|rm) exit 0 ;;
esac
echo "$*" >> "$FAKE_LOG"
prompt="${@: -1}"
proj="$HOME/.claude/projects/$(pwd | sed 's/[^A-Za-z0-9]/-/g')"; mkdir -p "$proj"
printf '{"type":"assistant","message":{"id":"m-%s","model":"claude-sonnet-4-5","content":[],"usage":{"input_tokens":10,"cache_read_input_tokens":1000,"cache_creation_input_tokens":0,"output_tokens":5}}}\n' "$$" >> "$proj/$$.jsonl"
if [ "${@: -2:1}" = -p ]; then
  # Vanilla: one session runs every job itself, at once, as parallel subagents would.
  for id in $(printf '%s' "$prompt" | grep -o 'job.mjs --id [a-z0-9-]*' | awk '{print $3}'); do
    (reeve heavy -- true || node scripts/job.mjs --id "$id") >/dev/null 2>&1 &
  done
  wait; exit 0
fi
for id in $(printf '%s' "$prompt" | grep -o 'job.mjs --id [a-z0-9-]*' | awk '{print $3}'); do
  reeve heavy -- node scripts/job.mjs --id "$id" >/dev/null 2>&1
done
"#;

const FAKE_REEVE: &str = r#"#!/bin/bash
# A fake reeve for the pressure bench: `run` starts a background worker, `heavy` runs the command.
ev="$XDG_STATE_HOME/reeve/events.jsonl"; mkdir -p "${ev%/*}"
case "$1" in
  run) shift; while [ "$1" != "--" ]; do [ "$1" = --cwd ] && cwd="$2"; shift; done; shift
    printf '{"command":"run","exit":0}\n' >> "$ev"
    (cd "$cwd" && nohup claude --bg "$1" >/dev/null 2>&1 &) ;;
  heavy) shift; [ "$1" = "--" ] && shift
    "$@"; c=$?
    printf '{"command":"heavy","exit":%s,"heavy_wait_ms":0}\n' "$c" >> "$ev"; exit $c ;;
  *) printf '{"command":"%s","exit":0}\n' "$1" >> "$ev" ;;
esac
"#;

#[test]
fn a_fake_run_starts_the_workers_measures_the_jobs_and_is_scored() {
    let t = tempfile::tempdir().unwrap();
    let bin = t.path().join("fakes");
    fs::create_dir_all(&bin).unwrap();
    write_exe(&bin.join("claude"), FAKE_CLAUDE).unwrap();
    write_exe(&bin.join("reeve"), FAKE_REEVE).unwrap();
    let log = t.path().join("claude-args.log");
    let arm = r#"{ "settingSources": "project,local", "setup": "arms/reeve", "workers": 2, "jobs": 2, "jobMb": 20, "jobSeconds": 1, "budgetMb": 100, "env": { "REEVE_HEAVY_SLOTS": "3" } }"#;
    let o = Opts {
        arm: "fake".into(),
        n: "1".into(),
        root: super::super::root(),
        trials: t.path().join("trials"),
        claude: bin.join("claude"),
        reeve: bin.join("reeve"),
        home: t.path().join("home"),
        tick_ms: 100,
        quiet_ms: 3_000,
        total_ms: 60_000,
        sample_ms: 200,
        extra_env: vec![("FAKE_LOG".into(), log.clone().into())],
        arm_json: Some(arm.into()),
    };
    assert_eq!(run(&o).unwrap(), 0);
    let logs = o.trials.join("logs/rp-fake-1");
    let r: Value =
        serde_json::from_str(&fs::read_to_string(logs.join("result.json")).unwrap()).unwrap();
    assert_eq!(r["end"], "all done", "{r:#}");
    assert_eq!(
        (
            r["jobs"].as_u64(),
            r["completed"].as_u64(),
            r["lost"].as_u64()
        ),
        (Some(4), Some(4), Some(0))
    );
    assert!(r["doneAt"].as_i64().unwrap() > 0, "{r:#}");
    assert_eq!(r["lastDoneAt"], r["doneAt"], "{r:#}");
    assert!(
        r["peakMemMb"].as_f64().unwrap() >= 20.0,
        "a job's memory shows: {r:#}"
    );
    assert_eq!(r["reeve"]["heavyRuns"], 4, "{r:#}");
    assert_eq!(r["reeve"]["workersStarted"], 2, "{r:#}");
    assert_eq!(r["sessionCount"], 2, "{r:#}");
    let args = fs::read_to_string(&log).unwrap();
    assert!(
        args.contains("--model sonnet --setting-sources project,local --settings "),
        "{args}"
    );
    let s: Value =
        serde_json::from_str(&fs::read_to_string(logs.join("settings.json")).unwrap()).unwrap();
    assert_eq!(s["env"]["REEVE_HEAVY_SLOTS"], "3", "{s:#}");
    assert_eq!(
        s["env"]["REEVE_HANDOFF_TOKENS"], "350000",
        "not the daemon's: {s:#}"
    );
    assert!(s["hooks"].is_object(), "{s:#}");
    assert!(
        !o.trials.join("rp-fake-1").exists(),
        "the run's worktree is cleaned up"
    );
    let table = score(&o.trials, &[]).unwrap();
    assert!(
        table
            .lines()
            .nth(1)
            .unwrap()
            .starts_with("fake\t1\t4 [4–4]\t4 [4–4]\t0 [0–0]"),
        "{table}"
    );
}

#[test]
fn a_fake_vanilla_run_has_one_session_run_every_job_without_reeve() {
    let t = tempfile::tempdir().unwrap();
    let bin = t.path().join("fakes");
    fs::create_dir_all(&bin).unwrap();
    write_exe(&bin.join("claude"), FAKE_CLAUDE).unwrap();
    // A reeve the session could reach would run the job's stand-in `true` instead of the job.
    write_exe(&bin.join("reeve"), FAKE_REEVE).unwrap();
    let log = t.path().join("claude-args.log");
    let arm = r#"{ "setup": "arms/b0-iso", "vanilla": true, "workers": 3, "jobs": 2, "jobMb": 20, "jobSeconds": 1, "budgetMb": 1000 }"#;
    let o = Opts {
        arm: "fakev".into(),
        n: "1".into(),
        root: super::super::root(),
        trials: t.path().join("trials"),
        claude: bin.join("claude"),
        reeve: bin.join("reeve"),
        home: t.path().join("home"),
        tick_ms: 100,
        quiet_ms: 15_000,
        total_ms: 60_000,
        sample_ms: 200,
        extra_env: vec![("FAKE_LOG".into(), log.clone().into())],
        arm_json: Some(arm.into()),
    };
    assert_eq!(run(&o).unwrap(), 0);
    let r: Value = serde_json::from_str(
        &fs::read_to_string(o.trials.join("logs/rp-fakev-1/result.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(r["end"], "all done", "{r:#}");
    assert_eq!(
        (r["completed"].as_u64(), r["lost"].as_u64()),
        (Some(6), Some(0))
    );
    assert_eq!(r["sessionCount"], 1, "{r:#}");
    assert_eq!(r["reeve"]["workersStarted"], 0, "{r:#}");
    let args = fs::read_to_string(&log).unwrap();
    assert_eq!(
        args.lines().filter(|l| l.contains(" -p ")).count(),
        1,
        "{args}"
    );
}

#[test]
fn every_arm_in_the_repo_parses() {
    let arms = super::super::root().join("pressure/arms");
    for e in fs::read_dir(&arms).unwrap().flatten() {
        let text = fs::read_to_string(e.path().join("arm.json")).unwrap();
        let a: Result<Arm, _> = serde_json::from_str(&text);
        assert!(a.is_ok(), "{:?}: {a:?}", e.file_name());
    }
}

#[test]
fn the_arm_in_the_repo_parses() {
    let text =
        fs::read_to_string(super::super::root().join("pressure/arms/reeve/arm.json")).unwrap();
    let a: Arm = serde_json::from_str(&text).unwrap();
    assert_eq!((a.workers, a.jobs), (6, 2));
    assert!(
        a.job_mb * 2 <= a.budget_mb && a.job_mb * 3 > a.budget_mb,
        "three at once don't fit"
    );
    assert_eq!(
        a.env["REEVE_HEAVY_SLOTS"], "3",
        "three slots, so the reaper has work"
    );
}
