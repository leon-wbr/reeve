//! `reeve usage`: how much of the plan limits (weekly, and 5-hour where there is one) Claude Code and Codex have used,
//! read from what each CLI already exposes. Claude Code hands its statusline command the
//! `rate_limits` it got with its last answer; `reeve usage record` keeps a copy. Codex answers
//! `account/rateLimits/read` in `codex app-server`.

use crate::msg::{fmt_utc, parse_utc};
use crate::workers::Kind;
use serde::Serialize;
use serde_json::{Value, json};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// A Claude snapshot older than this says so: no Claude Code session has answered since.
pub const STALE_SECS: u64 = 15 * 60;
const CODEX_TIMEOUT: Duration = Duration::from_secs(20);

/// One plan window a provider reports: `session` (the 5-hour one) or `weekly`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Window {
    pub window: &'static str,
    pub percent: Option<f64>,
    /// When the window resets, UTC.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
    /// Why the percent can't be given, e.g. the window reset since the last read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unknown: Option<String>,
}

impl Window {
    fn known(window: &'static str, percent: f64, resets_at: Option<String>) -> Window {
        Window {
            window,
            percent: Some(percent),
            resets_at,
            unknown: None,
        }
    }

    fn unknown(window: &'static str, why: String) -> Window {
        Window {
            window,
            percent: None,
            resets_at: None,
            unknown: Some(why),
        }
    }

    /// `(short label, long name)` for lines and messages.
    fn labels(&self) -> (&'static str, &'static str) {
        match self.window {
            "session" => ("5h", "5-hour"),
            _ => ("week", "weekly"),
        }
    }
}

/// Which windows `--max` checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum WindowSel {
    Weekly,
    /// The 5-hour window; providers without one are skipped.
    Session,
    All,
}

impl WindowSel {
    fn includes(self, window: &str) -> bool {
        match self {
            WindowSel::All => true,
            WindowSel::Weekly => window == "weekly",
            WindowSel::Session => window == "session",
        }
    }
}

/// A `--max` value: `65` for every provider, or `claude=65` for one.
#[derive(Debug, Clone, PartialEq)]
pub struct Limit {
    pub agent: Option<Kind>,
    pub percent: f64,
}

impl Limit {
    pub fn parse(s: &str) -> Result<Limit, String> {
        let (agent, num) = match s.split_once('=') {
            Some((a, n)) => {
                let k = <Kind as clap::ValueEnum>::from_str(a, true)
                    .map_err(|_| format!("unknown provider '{a}' in '{s}'; use claude or codex"))?;
                (Some(k), n)
            }
            None => (None, s),
        };
        let percent = num
            .parse::<f64>()
            .map_err(|_| format!("'{num}' is not a percentage"))?;
        Ok(Limit { agent, percent })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Reading {
    pub agent: &'static str,
    /// Only the windows the provider has.
    pub windows: Vec<Window>,
    /// Why nothing could be read at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unknown: Option<String>,
    /// When the CLI last reported these numbers, UTC.
    pub read_at: Option<String>,
    /// The read is older than `STALE_SECS`.
    pub stale: bool,
    pub source: String,
}

impl Reading {
    fn unknown(agent: &'static str, source: &str, why: &str) -> Reading {
        Reading {
            agent,
            windows: vec![],
            unknown: Some(why.into()),
            read_at: None,
            stale: false,
            source: source.into(),
        }
    }
}

/// Where `reeve usage record` keeps Claude's numbers: `REEVE_CLAUDE_USAGE`, else
/// `$XDG_STATE_HOME/reeve/usage/claude.json`.
pub fn claude_path() -> io::Result<PathBuf> {
    match std::env::var_os("REEVE_CLAUDE_USAGE").filter(|v| !v.is_empty()) {
        Some(p) => Ok(PathBuf::from(p)),
        None => Ok(crate::workers::Registry::default_dir()?.join("usage/claude.json")),
    }
}

/// The snapshot to keep from a statusline payload: its `rate_limits` and when, or nothing when
/// Claude Code sent none (an API key, or no answer yet this session).
pub fn snapshot(payload: &str, now: u64) -> Option<Value> {
    let v: Value = serde_json::from_str(payload).ok()?;
    let mut limits = v.get("rate_limits")?.as_object()?.clone();
    limits.insert("at".into(), json!(fmt_utc(now)));
    Some(Value::Object(limits))
}

/// Reads a statusline payload on stdin and keeps its rate limits. It prints nothing and never
/// fails the statusline.
pub fn record(stdin: impl Read, path: &Path, now: u64) -> io::Result<()> {
    let mut payload = String::new();
    stdin.take(1 << 20).read_to_string(&mut payload)?;
    let Some(snap) = snapshot(&payload, now) else {
        return Ok(());
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(&snap)?)?;
    std::fs::rename(&tmp, path)
}

/// Claude's numbers from a snapshot `record` wrote, as of `now`.
pub fn claude_from(snap: &Value, now: u64, source: &str) -> Reading {
    let at = snap["at"].as_str().and_then(parse_utc);
    let ago = at.map(|a| crate::msg::age(now.saturating_sub(a)));
    let window = |key: &str, name: &'static str| {
        let w = &snap[key];
        let percent = w["used_percentage"].as_f64()?;
        let resets = w["resets_at"].as_u64();
        Some(match resets {
            Some(r) if r <= now => Window::unknown(
                name,
                format!(
                    "the window reset at {} after the last read{}",
                    fmt_utc(r),
                    ago.as_ref()
                        .map(|a| format!(" {a} ago"))
                        .unwrap_or_default()
                ),
            ),
            _ => Window::known(name, percent, resets.map(fmt_utc)),
        })
    };
    Reading {
        agent: "claude",
        windows: [
            window("five_hour", "session"),
            window("seven_day", "weekly"),
        ]
        .into_iter()
        .flatten()
        .collect(),
        unknown: None,
        read_at: at.map(fmt_utc),
        stale: at.is_none_or(|a| now.saturating_sub(a) > STALE_SECS),
        source: source.into(),
    }
}

pub fn claude(now: u64) -> Reading {
    let source = "Claude Code's statusline, kept by `reeve usage record`";
    let path = match claude_path() {
        Ok(p) => p,
        Err(e) => return Reading::unknown("claude", source, &e.to_string()),
    };
    let snap = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok());
    match snap {
        Some(s) => claude_from(&s, now, source),
        None => Reading::unknown(
            "claude",
            source,
            &format!(
                "no statusline snapshot at {}; pipe the statusline's input to `reeve usage record`",
                crate::setup::tilde(&path)
            ),
        ),
    }
}

/// Codex's numbers from an `account/rateLimits/read` result, read at `now`.
pub fn codex_from(result: &Value, now: u64) -> Reading {
    let limits = &result["rateLimits"];
    let window = |mins: u64, name: &'static str| {
        let w = ["primary", "secondary"]
            .iter()
            .map(|k| &limits[*k])
            .find(|w| w["windowDurationMins"].as_u64() == Some(mins));
        let (percent, w) = w.and_then(|w| Some((w["usedPercent"].as_f64()?, w)))?;
        Some(Window::known(
            name,
            percent,
            w["resetsAt"].as_u64().map(fmt_utc),
        ))
    };
    Reading {
        agent: "codex",
        windows: [window(300, "session"), window(10_080, "weekly")]
            .into_iter()
            .flatten()
            .collect(),
        unknown: None,
        read_at: Some(fmt_utc(now)),
        stale: false,
        source: "codex app-server, account/rateLimits/read".into(),
    }
}

/// Asks `codex app-server` over stdio, the way Codex's own clients do.
fn codex_query(timeout: Duration) -> Result<Value, String> {
    let mut child = Command::new("codex")
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => "codex is not on PATH".to_string(),
            _ => format!("codex app-server didn't start: {e}"),
        })?;
    let mut stdin = child.stdin.take().expect("piped");
    let stdout = child.stdout.take().expect("piped");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&line)
                && v["id"] == 2
            {
                let _ = tx.send(v);
                return;
            }
        }
    });
    let client = json!({"name": "reeve", "version": crate::VERSION});
    let requests = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"clientInfo": client}}),
        json!({"jsonrpc": "2.0", "method": "initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "account/rateLimits/read"}),
    ];
    let sent = requests
        .iter()
        .try_for_each(|r| writeln!(stdin, "{r}"))
        .and_then(|_| stdin.flush());
    let answer = sent
        .map_err(|e| format!("codex app-server: {e}"))
        .and_then(|_| {
            rx.recv_timeout(timeout).map_err(|e| match e {
                mpsc::RecvTimeoutError::Timeout => {
                    format!("codex app-server gave no answer in {}s", timeout.as_secs())
                }
                mpsc::RecvTimeoutError::Disconnected => {
                    "codex app-server exited without an answer".to_string()
                }
            })
        });
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    let v = answer?;
    match v.get("error") {
        Some(e) => Err(format!(
            "codex app-server: {}",
            e["message"].as_str().unwrap_or("error")
        )),
        None => Ok(v["result"].clone()),
    }
}

pub fn codex(now: u64) -> Reading {
    match codex_query(CODEX_TIMEOUT) {
        Ok(result) => codex_from(&result, now),
        Err(why) => Reading::unknown("codex", "codex app-server, account/rateLimits/read", &why),
    }
}

pub fn read(agent: Option<Kind>, now: u64) -> Vec<Reading> {
    let want = |k| agent.is_none_or(|a| a == k);
    let claude = want(Kind::Claude).then(|| std::thread::spawn(move || claude(now)));
    let codex = want(Kind::Codex).then(|| codex(now));
    claude
        .map(|h| h.join().expect("claude reading"))
        .into_iter()
        .chain(codex)
        .collect()
}

fn pct(p: f64) -> String {
    if p.fract() == 0.0 {
        format!("{p:.0}%")
    } else {
        format!("{p:.1}%")
    }
}

/// `45m`, `2h10m`, `3d4h`: time left, for a line.
fn left(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h{:02}m", secs / 3600, secs / 60 % 60),
        _ => format!("{}d{}h", secs / 86_400, secs / 3600 % 24),
    }
}

/// One line per agent: `claude  week 89% (resets in 1d4h)  read 2m ago`; a provider shows only
/// the windows it has.
pub fn render(r: &Reading, now: u64) -> String {
    let head = format!("{:<6}  ", r.agent);
    if let Some(why) = &r.unknown {
        return format!("{head}unknown: {why}");
    }
    let parts: Vec<String> = r
        .windows
        .iter()
        .map(|w| {
            let short = w.labels().0;
            match w.percent {
                Some(percent) => {
                    let resets = w
                        .resets_at
                        .as_deref()
                        .and_then(parse_utc)
                        .map(|t| format!(" (resets in {})", left(t.saturating_sub(now))))
                        .unwrap_or_default();
                    format!("{short} {}{resets}", pct(percent))
                }
                None => format!("{short} unknown ({})", w.unknown.as_deref().unwrap_or("")),
            }
        })
        .collect();
    let read = match r.read_at.as_deref().and_then(parse_utc) {
        Some(t) if now.saturating_sub(t) < 5 => "read now".to_string(),
        Some(t) => format!("read {} ago", crate::msg::age(now.saturating_sub(t))),
        None => "read at an unknown time".to_string(),
    };
    let stale = if r.stale {
        "; stale: no Claude Code session has answered since"
    } else {
        ""
    };
    format!("{head}{}  {read}{stale}", parts.join("  "))
}

/// Each selected window a reading knows that is over its limit, as `codex weekly 40% is over
/// 30%`. A limit with a provider prefix beats a bare one; a provider without the window is skipped.
pub fn over(readings: &[Reading], limits: &[Limit], sel: WindowSel) -> Vec<String> {
    readings
        .iter()
        .flat_map(|r| {
            let kind = if r.agent == "claude" {
                Kind::Claude
            } else {
                Kind::Codex
            };
            let max = limits
                .iter()
                .rev()
                .find(|l| l.agent == Some(kind))
                .or_else(|| limits.iter().rev().find(|l| l.agent.is_none()))
                .map(|l| l.percent);
            r.windows.iter().filter_map(move |w| {
                let (percent, max) = (w.percent?, max?);
                (sel.includes(w.window) && percent > max).then(|| {
                    format!(
                        "{} {} {} is over {}",
                        r.agent,
                        w.labels().1,
                        pct(percent),
                        pct(max)
                    )
                })
            })
        })
        .collect()
}

/// `reeve usage`: prints the readings, and with `max` exits 1 when a selected window is over it.
pub fn main(agent: Option<Kind>, json: bool, max: &[Limit], window: WindowSel) -> io::Result<i32> {
    let now = crate::msg::now_secs();
    let readings = read(agent, now);
    if json {
        println!("{}", serde_json::to_string_pretty(&readings)?);
    } else {
        for r in &readings {
            println!("{}", render(r, now));
        }
    }
    let over = over(&readings, max, window);
    for o in &over {
        eprintln!("reeve: {o}");
    }
    Ok(i32::from(!over.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_793_800_000;

    fn snap(five: Option<(u64, u64)>, week: Option<(u64, u64)>, at: u64) -> Value {
        let w = |x: Option<(u64, u64)>| {
            x.map(|(p, r)| json!({"used_percentage": p, "resets_at": r}))
                .unwrap_or(Value::Null)
        };
        let mut v = json!({"at": fmt_utc(at)});
        if five.is_some() {
            v["five_hour"] = w(five);
        }
        if week.is_some() {
            v["seven_day"] = w(week);
        }
        v
    }

    #[test]
    fn the_snapshot_keeps_the_statusline_rate_limits_and_when() {
        let p = r#"{"model":{"id":"x"},"rate_limits":{"five_hour":{"used_percentage":3,"resets_at":5},"seven_day":{"used_percentage":89,"resets_at":9}}}"#;
        let s = snapshot(p, NOW).unwrap();
        assert_eq!(s["five_hour"]["used_percentage"], 3);
        assert_eq!(s["seven_day"]["resets_at"], 9);
        assert_eq!(s["at"], fmt_utc(NOW));
        assert_eq!(s.get("model"), None);
        assert_eq!(snapshot(r#"{"model":{}}"#, NOW), None);
        assert_eq!(snapshot("not json", NOW), None);
    }

    #[test]
    fn record_writes_the_snapshot_and_leaves_it_alone_without_rate_limits() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("usage/claude.json");
        let p = r#"{"rate_limits":{"seven_day":{"used_percentage":50,"resets_at":9}}}"#;
        record(p.as_bytes(), &path, NOW).unwrap();
        record(&b"{}"[..], &path, NOW + 60).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["seven_day"]["used_percentage"], 50);
        assert_eq!(v["at"], fmt_utc(NOW));
    }

    fn known(r: &Reading, name: &str) -> Option<f64> {
        r.windows.iter().find(|w| w.window == name)?.percent
    }

    #[test]
    fn claude_reports_only_the_windows_it_has() {
        let r = claude_from(&snap(None, Some((89, NOW + 90_000)), NOW - 120), NOW, "s");
        assert_eq!(r.windows.len(), 1);
        assert_eq!(known(&r, "weekly"), Some(89.0));
        assert!(!r.stale);
        assert_eq!(
            render(&r, NOW),
            "claude  week 89% (resets in 1d1h)  read 2m ago"
        );
    }

    #[test]
    fn an_old_claude_snapshot_is_stale_and_a_window_that_reset_since_is_unknown() {
        let s = snap(Some((70, NOW - 60)), Some((40, NOW + 7200)), NOW - 3 * 3600);
        let r = claude_from(&s, NOW, "s");
        assert!(r.stale);
        let five = r.windows.iter().find(|w| w.window == "session").unwrap();
        assert!(
            five.unknown
                .as_ref()
                .is_some_and(|u| u.contains("reset at") && u.contains("3h ago"))
        );
        let line = render(&r, NOW);
        assert!(line.contains("week 40% (resets in 2h00m)"), "{line}");
        assert!(
            line.ends_with("read 3h ago; stale: no Claude Code session has answered since"),
            "{line}"
        );
    }

    #[test]
    fn codex_windows_are_matched_by_length_not_by_name() {
        let res = json!({"rateLimits": {
            "primary": {"usedPercent": 5, "windowDurationMins": 10080, "resetsAt": NOW + 600},
            "secondary": {"usedPercent": 1.5, "windowDurationMins": 300, "resetsAt": NOW + 60},
        }});
        let r = codex_from(&res, NOW);
        assert_eq!(
            render(&r, NOW),
            "codex   5h 1.5% (resets in 1m)  week 5% (resets in 10m)  read now"
        );
        let none = codex_from(&json!({"rateLimits": {"primary": null}}), NOW);
        assert!(none.windows.is_empty());
    }

    #[test]
    fn a_reading_that_failed_is_one_unknown() {
        let r = Reading::unknown("codex", "s", "codex is not on PATH");
        assert_eq!(render(&r, NOW), "codex   unknown: codex is not on PATH");
    }

    fn both() -> Vec<Reading> {
        vec![
            claude_from(&snap(None, Some((63, NOW + 9000)), NOW), NOW, "s"),
            codex_from(
                &json!({"rateLimits": {
                    "primary": {"usedPercent": 87, "windowDurationMins": 300},
                    "secondary": {"usedPercent": 10, "windowDurationMins": 10080},
                }}),
                NOW,
            ),
        ]
    }

    fn lim(s: &str) -> Limit {
        Limit::parse(s).unwrap()
    }

    #[test]
    fn a_busy_codex_session_window_does_not_fail_a_weekly_max() {
        let r = both();
        assert!(over(&r, &[lim("65")], WindowSel::Weekly).is_empty());
        assert_eq!(
            over(&r, &[lim("65")], WindowSel::Session),
            ["codex 5-hour 87% is over 65%"]
        );
        assert_eq!(
            over(&r, &[lim("65")], WindowSel::All),
            ["codex 5-hour 87% is over 65%"]
        );
    }

    #[test]
    fn over_names_provider_and_window_and_skips_providers_without_the_window() {
        let r = both();
        assert_eq!(
            over(&r, &[lim("60")], WindowSel::Weekly),
            ["claude weekly 63% is over 60%"]
        );
        // Claude has no session window: only Codex can be over.
        assert_eq!(
            over(&r, &[lim("50")], WindowSel::Session),
            ["codex 5-hour 87% is over 50%"]
        );
    }

    #[test]
    fn max_takes_a_provider_prefix_and_the_prefix_beats_a_bare_number() {
        let r = both();
        assert!(over(&r, &[lim("claude=65"), lim("codex=90")], WindowSel::All).is_empty());
        assert_eq!(
            over(&r, &[lim("claude=50"), lim("codex=90")], WindowSel::All),
            ["claude weekly 63% is over 50%"]
        );
        assert_eq!(
            over(&r, &[lim("5"), lim("claude=70")], WindowSel::Weekly),
            ["codex weekly 10% is over 5%"]
        );
        assert!(Limit::parse("gemini=5").is_err());
        assert!(Limit::parse("claude=x").is_err());
    }

    #[test]
    fn json_is_a_list_of_windows_per_provider() {
        let r = claude_from(&snap(None, Some((89, NOW + 60)), NOW), NOW, "s");
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["windows"].as_array().unwrap().len(), 1);
        assert_eq!(v["windows"][0]["window"], "weekly");
        assert_eq!(v["windows"][0]["percent"], 89.0);
        assert_eq!(v["stale"], false);
    }
}
