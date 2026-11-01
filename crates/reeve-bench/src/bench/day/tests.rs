use super::*;

fn user() -> User {
    serde_json::from_str(include_str!("../../../../../bench/day/user.json")).unwrap()
}

#[test]
fn scripted_rules_answer_only_questions_and_once_rules_once() {
    let u = user();
    let q = "Price and VAT are merged.\nWhat should the new name for lowStock be?";
    assert_eq!(scripted(&u, q, &[]).unwrap().id, "rename");
    assert_eq!(scripted(&u, q, &["rename".into()]).map(|r| &r.id), None);
    let m = "Renamed to reorderLevel. Shall I merge it?";
    assert_eq!(scripted(&u, m, &["rename".into()]).unwrap().id, "merge");
    assert!(scripted(&u, "I renamed lowStock and merged it.", &[]).is_none());
    assert!(scripted(&u, "Do you want fries with that?", &[]).is_none());
    assert!(claimed("Everything is merged.\nALL DONE"));
    assert!(!claimed("all done soon"));
}

#[test]
fn the_wrapper_puts_the_arms_flags_first_and_leaves_subcommands_alone() {
    let arm = Arm {
        effort: Some("low".into()),
        setting_sources: Some("project,local".into()),
        ..Arm::default()
    };
    let flags = arm.flags(Path::new("/s/settings.json"));
    assert_eq!(
        flags,
        [
            "--model", "sonnet", "--effort", "low", "--setting-sources", "project,local",
            "--settings", "/s/settings.json"
        ]
    );
    let w = claude_wrapper(Path::new("/bin/claude it's"), &flags);
    assert!(w.contains("agents|attach|"), "{w}");
    assert!(
        w.ends_with("exec '/bin/claude it'\\''s' '--model' 'sonnet' '--effort' 'low' '--setting-sources' 'project,local' '--settings' '/s/settings.json' \"$@\"\n"),
        "{w}"
    );
    let r = reeve_wrapper(Path::new("/r/reeve"), Path::new("/m.md"));
    assert!(r.contains("exec cat '/m.md'") && r.contains("exec '/r/reeve' \"$@\""), "{r}");
}

#[test]
fn arms_in_the_repo_parse_and_unknown_fields_are_refused() {
    for e in fs::read_dir(super::super::root().join("day/arms")).unwrap() {
        let p = e.unwrap().path().join("arm.json");
        let a: Result<Arm, _> = serde_json::from_str(&fs::read_to_string(&p).unwrap());
        assert!(a.is_ok(), "{}: {a:?}", p.display());
    }
    assert!(serde_json::from_str::<Arm>(r#"{"modle": "opus"}"#).is_err());
}

#[test]
fn usage_prices_each_model_counts_each_message_once_and_keeps_the_peak() {
    let line = |id: &str, model: &str, read: u64| {
        json!({ "type": "assistant", "message": { "id": id, "model": model, "usage": {
            "input_tokens": 1_000_000, "cache_read_input_tokens": read,
            "cache_creation_input_tokens": 0, "output_tokens": 0 } } })
        .to_string()
    };
    let text = [
        line("a", "claude-sonnet-4-5", 0),
        line("a", "claude-sonnet-4-5", 0),
        line("b", "claude-haiku-4-5", 2_000_000),
        r#"{"type":"user"}"#.into(),
    ]
    .join("\n");
    let mut u = Usage::default();
    u.add(&text, &mut HashSet::new());
    assert_eq!(u.turns, 2);
    assert!((u.cost - (3.0 + 1.0 + 0.2)).abs() < 1e-9, "{}", u.cost);
    assert_eq!(u.peak, 3_000_000.0);
}

#[test]
fn reeve_and_database_records_become_metrics() {
    let events = [
        r#"{"command":"skill","exit":0}"#,
        r#"{"command":"run","exit":0,"worker":"a"}"#,
        r#"{"command":"run","exit":2}"#,
        r#"{"command":"heavy","exit":0,"heavy_wait_ms":1500}"#,
        r#"{"command":"heavy","exit":1,"heavy_wait_ms":4000}"#,
        r#"{"command":"handoff","exit":0}"#,
        "not json",
    ]
    .join("\n");
    let m = reeve_metrics(&events);
    assert_eq!(m["workersStarted"], 1);
    assert_eq!(m["runFailures"], 1);
    assert_eq!(m["heavyRuns"], 2);
    assert_eq!(m["heavyWaitS"], json!(5.5));
    assert_eq!(m["heavyMaxWaitS"], 4);
    assert_eq!(m["handoffs"], 1);
    assert_eq!(m["skillReads"], 1);
    let runs = [
        r#"{"what":"suite","ok":true,"busy":false,"heavy":true}"#,
        r#"{"what":"suite","ok":false,"busy":true,"heavy":false}"#,
        r#"{"what":"rebuild","ok":true,"busy":false,"heavy":false}"#,
    ]
    .join("\n");
    let d = db_metrics(&runs);
    assert_eq!(
        d,
        json!({ "suiteRuns": 1, "suiteRunsHeavy": 1, "busy": 1, "rebuilds": 1, "rebuildsHeavy": 0 })
    );
}

/// A scratch git repo holding the scenario repo, unsolved or solved, on branch `main`.
fn scenario(solved: bool) -> (tempfile::TempDir, PathBuf, String) {
    let t = tempfile::tempdir().unwrap();
    let (repo, db) = (t.path().join("repo"), t.path().join("db"));
    let root = super::super::root();
    copy_dir(&root.join("day/repo"), &repo).unwrap();
    if solved {
        copy_dir(&root.join("day/solution"), &repo).unwrap();
    }
    let text = db_json(&db, 0, 0);
    fs::write(repo.join("test/db.json"), &text).unwrap();
    git!("-C", repo, "init", "-q", "-b", "main").unwrap();
    git!("-C", repo, "add", "-A").unwrap();
    git!("-C", repo, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "x").unwrap();
    (t, db, text)
}

#[test]
fn the_check_fails_the_scenario_and_passes_the_reference_solution() {
    let root = super::super::root();
    let (t, db, text) = scenario(false);
    let c = check(&root, &t.path().join("repo"), "main", &db, &text).unwrap();
    assert_eq!(c["tests"], false, "{c}");
    assert_eq!(c["done"], false);

    let (t, db, text) = scenario(true);
    let repo = t.path().join("repo");
    let c = check(&root, &repo, "main", &db, &text).unwrap();
    assert_eq!(c["tests"], true, "{c}");
    assert_eq!(c["rebuilt"], false, "{c}");
    assert_eq!(c["items"]["fixtures"], false, "a copied fixture without a rebuild: {c}");
    fs::create_dir_all(&db).unwrap();
    fs::write(db.join("runs.jsonl"), r#"{"what":"rebuild","ok":true,"busy":false}"#).unwrap();
    let c = check(&root, &repo, "main", &db, &text).unwrap();
    assert_eq!(c["done"], true, "{c}");
    let c = check(&root, &repo, "main", &db, &db_json(&db, 60, 360)).unwrap();
    assert_eq!(c["tampered"], true);
    assert_eq!(c["done"], false, "{c}");
}

const FAKE_CLAUDE: &str = r#"#!/bin/bash
# A fake claude for the day bench's tests: a session that works the backlog from the solution.
case "$1" in
  agents) echo '[]'; exit 0 ;;
  stop|kill|rm) exit 0 ;;
esac
echo "$*" >> "$FAKE_LOG"
sid="fake-$$"
proj="$HOME/.claude/projects/$(pwd | sed 's/[^A-Za-z0-9]/-/g')"
mkdir -p "$proj"
n=0
say() {
  n=$((n+1))
  printf '{"type":"assistant","message":{"id":"%s-m%s","model":"claude-sonnet-4-5","content":[{"type":"text","text":"%s"}],"usage":{"input_tokens":10,"cache_read_input_tokens":%s,"cache_creation_input_tokens":100,"output_tokens":50}}}\n' "$sid" "$n" "$1" "$((n*1000))" | tee -a "$proj/$sid.jsonl"
}
result() { printf '{"type":"result","subtype":"success"}\n'; }
commit() { git add -A && git -c user.name=fake -c user.email=f@x commit -qm "$1"; }
sol="$FAKE_SOLUTION"
printf '{"type":"system","subtype":"init","session_id":"%s"}\n' "$sid"
renamed=0; discounted=0
while IFS= read -r line; do
  if [[ "$line" == *backlog* ]]; then
    cp "$sol/src/price.js" "$sol/src/tax.js" "$sol/src/slug.js" src/ && commit "price, vat, slug"
    if [ "$FAKE_MODE" = reeve ]; then
      reeve --skill >/dev/null
      reeve heavy -- node scripts/rebuild-fixtures.mjs >/dev/null
      reeve run --name "${PWD##*/}-w" -- "x" >/dev/null
    else
      node scripts/rebuild-fixtures.mjs >/dev/null
    fi
    commit "fixtures"
    say "Four are merged. What should the new name for lowStock be?"; result; continue
  fi
  if [[ "$line" == *reorderLevel* ]]; then
    cp "$sol/src/config.js" "$sol/src/inventory.js" src/ && cp "$sol/README.md" . && commit "rename"; renamed=1
  fi
  if [[ "$line" == *discount* ]]; then
    cp "$sol/src/cart.js" src/ && commit "discount"; discounted=1
  fi
  if [ $renamed = 1 ] && [ $discounted = 1 ]; then say "Everything is merged.\nALL DONE"; else say "On it."; fi
  result
done
"#;

const FAKE_REEVE: &str = r#"#!/bin/bash
# A fake reeve for the day bench's tests: logs events where the real one does.
ev="$XDG_STATE_HOME/reeve/events.jsonl"; mkdir -p "${ev%/*}"
case "$1" in
  heavy) shift; [ "$1" = "--" ] && shift
    REEVE_HEAVY_JOB=1 "$@"; c=$?
    printf '{"command":"heavy","exit":%s,"heavy_wait_ms":1500}\n' "$c" >> "$ev"; exit $c ;;
  run) printf '{"command":"run","exit":0,"worker":"w"}\n' >> "$ev" ;;
  --skill) printf '{"command":"skill","exit":0}\n' >> "$ev"; echo manual ;;
  *) printf '{"command":"%s","exit":0}\n' "$1" >> "$ev" ;;
esac
"#;

fn fake_run(arm: &str, mode: &str) -> (tempfile::TempDir, Value, String) {
    let t = tempfile::tempdir().unwrap();
    let bin = t.path().join("fakes");
    fs::create_dir_all(&bin).unwrap();
    write_exe(&bin.join("claude"), FAKE_CLAUDE).unwrap();
    write_exe(&bin.join("reeve"), FAKE_REEVE).unwrap();
    let root = super::super::root();
    let log = t.path().join("claude-args.log");
    let o = Opts {
        arm: arm.into(),
        n: "1".into(),
        root: root.clone(),
        trials: t.path().join("trials"),
        claude: bin.join("claude"),
        reeve: Some(bin.join("reeve")),
        home: t.path().join("home"),
        timing: Timing {
            tick_ms: 50,
            check_ms: 300,
            reply_ms: 200,
            grace_ms: 5_000,
            total_ms: 60_000,
            scale: 0.004,
        },
        hold_s: Some(0),
        rebuild_s: Some(0),
        sim_model: "haiku".into(),
        extra_env: vec![
            ("FAKE_SOLUTION".into(), root.join("day/solution").into()),
            ("FAKE_MODE".into(), mode.into()),
            ("FAKE_LOG".into(), log.clone().into()),
        ],
    };
    assert_eq!(run(&o).unwrap(), 0);
    let logs = o.trials.join("logs").join(format!("rd-{arm}-1"));
    let r: Value = serde_json::from_str(&fs::read_to_string(logs.join("result.json")).unwrap()).unwrap();
    let args = fs::read_to_string(&log).unwrap();
    assert!(!o.trials.join(format!("rd-{arm}-1")).exists(), "the run's worktree is cleaned up");
    (t, r, args)
}

#[test]
fn a_fake_b0_session_runs_end_to_end_and_is_scored() {
    let (t, r, args) = fake_run("b0", "b0");
    assert_eq!(r["final"]["done"], true, "{r:#}");
    assert_eq!(r["falseDone"], false);
    assert_eq!(r["neverClaimed"], false);
    let iv = r["interventions"].as_array().unwrap();
    assert_eq!(iv.len(), 1, "only the rename decision: {iv:?}");
    assert_eq!(iv[0]["kind"], "answer");
    assert_eq!(r["followups"].as_array().unwrap().len(), 1);
    assert_eq!(r["reeve"]["workersStarted"], 0);
    assert_eq!(r["db"]["rebuilds"], 1);
    assert_eq!(r["db"]["rebuildsHeavy"], 0);
    assert_eq!(r["sessionCount"], 1);
    assert!(r["costUsd"].as_f64().unwrap() > 0.0, "{r}");
    assert!(r["usage"]["peakContext"].as_f64().unwrap() > 0.0);
    assert_eq!(r["invalid"], false);
    assert_eq!(r["params"]["model"], "sonnet");
    assert!(args.contains("--model sonnet --setting-sources project,local --settings "), "{args}");
    let table = score(&t.path().join("trials"), &[], false).unwrap();
    assert!(table.lines().nth(1).unwrap().starts_with("b0   1  1/1"), "{table}");
}

#[test]
fn a_fake_reeve_session_shows_up_in_reeves_metrics() {
    let (_t, r, args) = fake_run("reeve", "reeve");
    assert_eq!(r["final"]["done"], true, "{r:#}");
    assert_eq!(r["reeve"]["workersStarted"], 1);
    assert_eq!(r["reeve"]["heavyRuns"], 1);
    assert_eq!(r["reeve"]["skillReads"], 1);
    assert_eq!(r["reeve"]["heavyWaitS"], json!(1.5));
    assert_eq!(r["db"]["rebuildsHeavy"], 1);
    assert_eq!(r["params"]["handoffTokens"], 350_000);
    assert!(args.contains("bench/arms/reeve/settings.json"), "{args}");
}
