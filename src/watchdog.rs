//! `reeve watchdog`: keeps unattended workers going. Each pass looks at every opted-in worker
//! without a report and, in this order, tells it once to wrap up when its agent's weekly usage
//! is over a cap, tells it to carry on when its pane shows a usage-limit stop, or tells it to
//! carry on when it has sat idle for two passes without a `reeve heavy` job of its own.
//! Configured under `watchdog` in reeve's config file; off until it names workers or `all`.

use crate::usage::{self, Limit, Reading, WindowSel};
use crate::workers::{self, Backend, Kind, Registry, State, Worker};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

pub const LIMIT_TEXT: &str =
    "Carry on where you left off. Re-check git and PR state first, and never redo a merge.";
pub const IDLE_TEXT: &str = "Carry on with your brief and decide for yourself. If you are waiting on your own `reeve heavy` job, keep waiting; if you are done, write your report.";

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub all: bool,
    pub workers: Vec<String>,
    /// Weekly usage caps, as `reeve usage --max` takes them.
    pub caps: Vec<Limit>,
    pub every: Duration,
}

impl Config {
    /// The `watchdog` object in reeve's config file; caps that don't parse are skipped.
    pub fn from_json(config: Option<&str>) -> Config {
        let v: Value = config
            .and_then(|c| serde_json::from_str::<Value>(c).ok())
            .map(|v| v["watchdog"].clone())
            .unwrap_or_default();
        let strs = |k: &str| -> Vec<String> {
            v[k].as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        };
        Config {
            all: v["all"].as_bool().unwrap_or(false),
            workers: strs("workers"),
            caps: strs("caps")
                .iter()
                .filter_map(|c| Limit::parse(c).ok())
                .collect(),
            every: Duration::from_secs(
                60 * v["every_min"].as_u64().filter(|m| *m > 0).unwrap_or(15),
            ),
        }
    }

    pub fn watches(&self, name: &str) -> bool {
        self.all || self.workers.iter().any(|w| w == name)
    }

    /// Watched by name, or started by a watched worker, as a handoff's successor is.
    pub fn watches_in(&self, w: &Worker, all: &[Worker]) -> bool {
        if self.watches(&w.name) {
            return true;
        }
        let mut parent = w.parent.as_deref();
        for _ in 0..32 {
            let Some(p) = parent else { return false };
            if self.watches(p) {
                return true;
            }
            parent = all
                .iter()
                .find(|x| x.name == p)
                .and_then(|x| x.parent.as_deref());
        }
        false
    }
}

/// What a pass remembers for the next one, so a pass a minute or an hour later carries on.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Memory {
    pub idle: HashMap<String, u32>,
    pub wrapped: HashSet<String>,
}

/// What a pass sees of one worker.
#[derive(Debug, Clone, Default)]
pub struct Seen {
    pub state: Option<State>,
    /// A `reeve heavy` job of its own runs or waits.
    pub busy: bool,
    /// Its pane shows a usage-limit stop; only looked at while it is idle.
    pub limit_stop: bool,
    /// Which cap its agent is over, as `reeve usage --max` words it.
    pub over: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Nudge {
    WrapUp(String),
    Limit,
    Idle,
}

impl Nudge {
    pub fn text(&self) -> String {
        match self {
            Nudge::WrapUp(over) => format!(
                "Usage is past the watchdog's cap ({over}). Stop new work, push or park your branch, and write your report now."
            ),
            Nudge::Limit => LIMIT_TEXT.into(),
            Nudge::Idle => IDLE_TEXT.into(),
        }
    }

    fn what(&self) -> &str {
        match self {
            Nudge::WrapUp(_) => "told to wrap up",
            Nudge::Limit => "nudged after a usage-limit stop",
            Nudge::Idle => "nudged after 2 idle checks",
        }
    }
}

/// How many passes in a row a worker may sit idle before it is nudged.
pub const IDLE_PASSES: u32 = 2;

pub fn decide(mem: &mut Memory, name: &str, s: &Seen) -> Option<Nudge> {
    if !matches!(s.state, Some(State::Idle | State::Running | State::Blocked)) {
        mem.idle.remove(name);
        return None;
    }
    if mem.wrapped.contains(name) {
        return None;
    }
    if let Some(over) = &s.over {
        mem.wrapped.insert(name.to_string());
        mem.idle.remove(name);
        return Some(Nudge::WrapUp(over.clone()));
    }
    if s.state != Some(State::Idle) || s.busy {
        mem.idle.remove(name);
        return None;
    }
    if s.limit_stop {
        mem.idle.remove(name);
        return Some(Nudge::Limit);
    }
    let idle = mem.idle.entry(name.to_string()).or_default();
    *idle += 1;
    if *idle < IDLE_PASSES {
        return None;
    }
    mem.idle.remove(name);
    Some(Nudge::Idle)
}

/// Whether a pane's last lines show the agent stopped at its plan's usage limit.
pub fn limit_stop(pane: &str) -> bool {
    let lines: Vec<&str> = pane.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(15)..].iter().any(|l| {
        let l = l.to_lowercase();
        l.contains("usage limit") || l.contains("hit your limit")
    })
}

/// The cap a reading of `kind`'s usage is over, if any; only weekly windows count, since the
/// 5-hour one resets on its own.
pub fn over_cap(readings: &[Reading], caps: &[Limit], kind: Kind) -> Option<String> {
    let mine: Vec<Reading> = readings
        .iter()
        .filter(|r| r.agent == kind.bin())
        .cloned()
        .collect();
    usage::over(&mine, caps, WindowSel::Weekly)
        .into_iter()
        .next()
}

/// One pass: decides for each watched worker and delivers with `say`. Returns a log line for
/// each nudge.
pub fn pass(
    cfg: &Config,
    mem: &mut Memory,
    all: &[Worker],
    seen: impl Fn(&Worker) -> Seen,
    mut say: impl FnMut(&Worker, &str) -> io::Result<()>,
) -> Vec<String> {
    let mut log = vec![];
    for w in all.iter().filter(|w| cfg.watches_in(w, all)) {
        let Some(n) = decide(mem, &w.name, &seen(w)) else {
            continue;
        };
        log.push(match say(w, &n.text()) {
            Ok(()) => format!("{} {}", w.name, n.what()),
            Err(e) => format!("{} not {}: {e}", w.name, n.what()),
        });
    }
    let live: HashSet<&str> = all.iter().map(|w| w.name.as_str()).collect();
    mem.idle.retain(|n, _| live.contains(n.as_str()));
    mem.wrapped.retain(|n| live.contains(n.as_str()));
    log
}

fn herdr(args: &[&str]) -> io::Result<()> {
    let s = Command::new("herdr").args(args).status()?;
    if s.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "herdr {} failed",
            args[..2].join(" ")
        )))
    }
}

/// Types the text into a pane worker's prompt (Codex takes a second Enter), or puts it in a
/// background worker's inbox.
fn say(w: &Worker, text: &str) -> io::Result<()> {
    if w.backend == Backend::Bg {
        let store = crate::msg::Store::open()?;
        let m = crate::msg::compose("watchdog", &w.name, text, None, None, workers::now())
            .map_err(io::Error::other)?;
        crate::msg::write_atomic(&store.sent(), &m)?;
        return crate::msg::write_atomic(&store.inbox(), &m);
    }
    herdr(&["pane", "send-text", &w.handle, text])?;
    let enters = if w.agent == Kind::Codex { 2 } else { 1 };
    for _ in 0..enters {
        std::thread::sleep(Duration::from_secs(1));
        herdr(&["pane", "send-keys", &w.handle, "enter"])?;
    }
    Ok(())
}

fn load_memory(path: &Path) -> Memory {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// A pass with the real probes, appending each nudge to `watchdog.log` in reeve's state dir.
pub fn run_pass(r: &Registry, cfg: &Config) -> io::Result<Vec<String>> {
    let dir = Registry::default_dir()?;
    let mem_path = dir.join("watchdog.json");
    let mut mem = load_memory(&mem_path);
    let all = r.all()?;
    let live = all
        .iter()
        .any(|w| !w.report.is_file() && cfg.watches_in(w, &all));
    let readings = if cfg.caps.is_empty() || !live {
        vec![]
    } else {
        usage::read(None, workers::now())
    };
    let busy = crate::heavy::Queue::at(crate::heavy::Queue::default_root()).busy_workers();
    let log = pass(
        cfg,
        &mut mem,
        &all,
        |w| {
            if w.report.is_file() {
                return Seen::default();
            }
            let state = workers::state(w);
            let idle_pane = state == State::Idle && w.backend == Backend::Pane;
            Seen {
                state: Some(state),
                busy: busy.contains(&w.name),
                limit_stop: idle_pane && workers::read_tab(&w.handle).is_ok_and(|t| limit_stop(&t)),
                over: over_cap(&readings, &cfg.caps, w.agent),
            }
        },
        say,
    );
    fs::write(&mem_path, serde_json::to_string(&mem)?)?;
    let at = crate::msg::fmt_utc(workers::now());
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("watchdog.log"))?;
    for l in &log {
        writeln!(f, "{at} {l}")?;
    }
    Ok(log)
}

/// `reeve watchdog`: a pass every `every_min` minutes, or one with `once`. The config file is
/// read again before each pass.
pub fn main(r: &Registry, once: bool) -> io::Result<i32> {
    loop {
        let cfg = Config::from_json(
            crate::context::config_path()
                .and_then(|p| fs::read_to_string(p).ok())
                .as_deref(),
        );
        if !cfg.all && cfg.workers.is_empty() {
            eprintln!(
                "reeve watchdog: no workers to watch; set `watchdog.all` or `watchdog.workers` in {}",
                crate::context::config_path()
                    .map(|p| crate::setup::tilde(&p))
                    .unwrap_or_default()
            );
            if once {
                return Ok(0);
            }
        } else {
            for l in run_pass(r, &cfg)? {
                println!("{l}");
            }
        }
        if once {
            return Ok(0);
        }
        std::thread::sleep(cfg.every);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn worker(name: &str, agent: Kind) -> Worker {
        Worker {
            name: name.into(),
            backend: Backend::Pane,
            agent,
            cwd: "/work/x".into(),
            started: 1,
            report: format!("/st/reports/{name}.md").into(),
            handle: "w1:p1".into(),
            starting: false,
            error: None,
            parent: None,
            pushed: false,
        }
    }

    fn seen(state: State) -> Seen {
        Seen {
            state: Some(state),
            ..Seen::default()
        }
    }

    fn cfg(json: &str) -> Config {
        Config::from_json(Some(json))
    }

    #[test]
    fn a_usage_limit_stop_gets_a_carry_on_that_rechecks_git_and_never_redoes_a_merge() {
        let mut mem = Memory::default();
        let s = Seen {
            limit_stop: true,
            ..seen(State::Idle)
        };
        assert_eq!(decide(&mut mem, "a", &s), Some(Nudge::Limit));
        let t = Nudge::Limit.text();
        assert!(
            t.contains("git and PR state") && t.contains("never redo a merge"),
            "{t}"
        );
        let pane = "working…\n\n  You've hit your usage limit. Try again in 3 hours.\n\n> \n";
        assert!(limit_stop(pane));
        assert!(limit_stop("Claude usage limit reached\n"));
        assert!(!limit_stop("all tests pass\n> "));
    }

    #[test]
    fn a_limit_message_scrolled_far_up_is_not_a_stop() {
        let mut pane = "usage limit\n".to_string();
        pane.push_str(&"line\n".repeat(20));
        assert!(!limit_stop(&pane));
    }

    #[test]
    fn idle_for_two_passes_without_its_own_job_is_nudged() {
        let mut mem = Memory::default();
        assert_eq!(decide(&mut mem, "a", &seen(State::Idle)), None);
        assert_eq!(decide(&mut mem, "a", &seen(State::Idle)), Some(Nudge::Idle));
        assert_eq!(
            decide(&mut mem, "a", &seen(State::Idle)),
            None,
            "the count starts again"
        );
    }

    #[test]
    fn its_own_heavy_job_or_work_in_between_resets_the_idle_count() {
        let mut mem = Memory::default();
        let busy = Seen {
            busy: true,
            ..seen(State::Idle)
        };
        for s in [seen(State::Idle), busy.clone(), seen(State::Idle), busy] {
            assert_eq!(decide(&mut mem, "a", &s), None);
        }
        decide(&mut mem, "a", &seen(State::Idle));
        decide(&mut mem, "a", &seen(State::Running));
        assert_eq!(decide(&mut mem, "a", &seen(State::Idle)), None);
    }

    #[test]
    fn crossing_a_cap_tells_it_once_to_wrap_up_and_then_leaves_it_alone() {
        let mut mem = Memory::default();
        let s = Seen {
            over: Some("codex weekly 52% is over 50%".into()),
            ..seen(State::Running)
        };
        let n = decide(&mut mem, "a", &s).unwrap();
        assert!(n.text().contains("write your report"), "{}", n.text());
        assert!(n.text().contains("codex weekly 52%"));
        assert_eq!(decide(&mut mem, "a", &s), None);
        for _ in 0..3 {
            assert_eq!(decide(&mut mem, "a", &seen(State::Idle)), None);
        }
    }

    #[test]
    fn caps_count_the_weekly_window_of_the_workers_own_agent() {
        let reading = |agent: &'static str, window: &'static str, pct: f64| Reading {
            agent,
            windows: vec![usage::Window {
                window,
                percent: Some(pct),
                resets_at: None,
                unknown: None,
            }],
            unknown: None,
            read_at: None,
            stale: false,
            source: "test".into(),
        };
        let caps = cfg(r#"{"watchdog":{"all":true,"caps":["codex=50"]}}"#).caps;
        let rs = [
            reading("codex", "weekly", 51.0),
            reading("claude", "weekly", 90.0),
        ];
        assert!(over_cap(&rs, &caps, Kind::Codex).is_some());
        assert_eq!(over_cap(&rs, &caps, Kind::Claude), None);
        let rs = [reading("codex", "session", 99.0)];
        assert_eq!(over_cap(&rs, &caps, Kind::Codex), None);
    }

    #[test]
    fn done_gone_and_starting_workers_are_left_alone() {
        let mut mem = Memory::default();
        let s = Seen {
            limit_stop: true,
            over: Some("x".into()),
            ..Seen::default()
        };
        for st in [State::Done, State::Gone, State::Starting, State::Unknown] {
            let s = Seen {
                state: Some(st),
                ..s.clone()
            };
            assert_eq!(decide(&mut mem, "a", &s), None, "{st:?}");
        }
    }

    #[test]
    fn it_is_off_until_configured_and_watches_only_whom_it_is_told() {
        let off = Config::from_json(None);
        assert!(!off.watches("a"));
        assert_eq!(off.every, Duration::from_secs(900));
        let some = cfg(r#"{"watchdog":{"workers":["a"],"every_min":5,"caps":["nonsense"]}}"#);
        assert!(some.watches("a") && !some.watches("b"));
        assert_eq!(some.every, Duration::from_secs(300));
        assert!(some.caps.is_empty());
        assert!(cfg(r#"{"watchdog":{"all":true}}"#).watches("b"));
    }

    #[test]
    fn a_pass_nudges_only_watched_workers_and_logs_each_nudge() {
        let c = cfg(r#"{"watchdog":{"workers":["a","c"]}}"#);
        let mut mem = Memory::default();
        let all = [
            worker("a", Kind::Claude),
            worker("b", Kind::Claude),
            worker("c", Kind::Codex),
        ];
        let said = RefCell::new(vec![]);
        let probe = |w: &Worker| Seen {
            limit_stop: w.name != "a",
            ..seen(State::Idle)
        };
        let mut say = |w: &Worker, _: &str| {
            said.borrow_mut().push(w.name.clone());
            if w.name == "c" {
                Err(io::Error::other("herdr is gone"))
            } else {
                Ok(())
            }
        };
        assert_eq!(
            pass(&c, &mut mem, &all, probe, &mut say),
            ["c not nudged after a usage-limit stop: herdr is gone"]
        );
        assert_eq!(
            pass(&c, &mut mem, &all, probe, &mut say)[0],
            "a nudged after 2 idle checks"
        );
        assert_eq!(*said.borrow(), ["c", "a", "c"]);
    }

    #[test]
    fn a_successor_or_any_worker_a_watched_one_started_is_watched_too() {
        let c = cfg(r#"{"watchdog":{"workers":["a"]}}"#);
        let child = |name: &str, parent: &str| Worker {
            parent: Some(parent.into()),
            ..worker(name, Kind::Claude)
        };
        let all = [child("a-2", "a"), child("a-3", "a-2"), child("x", "y")];
        assert!(c.watches_in(&all[1], &all), "a-2 left the registry or not");
        assert!(!c.watches_in(&all[2], &all));
    }

    #[test]
    fn memory_forgets_workers_that_left_the_registry() {
        let c = cfg(r#"{"watchdog":{"all":true}}"#);
        let mut mem = Memory::default();
        mem.idle.insert("old".into(), 1);
        mem.wrapped.insert("old".into());
        pass(&c, &mut mem, &[], |_| Seen::default(), |_, _| Ok(()));
        assert_eq!(mem, Memory::default());
    }
}
