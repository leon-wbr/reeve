//! `reeve usage`: how much of the 5-hour and weekly plan limits Claude Code and Codex have used,
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

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Window {
    Known {
        percent: f64,
        /// When the window resets, UTC.
        resets_at: Option<String>,
    },
    Unknown {
        unknown: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Reading {
    pub agent: &'static str,
    pub five_hour: Window,
    pub weekly: Window,
    /// When the CLI last reported these numbers, UTC.
    pub read_at: Option<String>,
    /// The read is older than `STALE_SECS`.
    pub stale: bool,
    pub source: String,
}

impl Reading {
    fn unknown(agent: &'static str, source: &str, why: &str) -> Reading {
        let w = Window::Unknown {
            unknown: why.into(),
        };
        Reading {
            agent,
            five_hour: w.clone(),
            weekly: w,
            read_at: None,
            stale: false,
            source: source.into(),
        }
    }

    fn windows(&self) -> [(&'static str, &'static str, &Window); 2] {
        [
            ("5h", "5-hour", &self.five_hour),
            ("week", "weekly", &self.weekly),
        ]
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
    let window = |key: &str, label: &str| {
        let w = &snap[key];
        let Some(percent) = w["used_percentage"].as_f64() else {
            return Window::Unknown {
                unknown: format!("Claude Code's last statusline payload had no {label} window"),
            };
        };
        let resets = w["resets_at"].as_u64();
        match resets {
            Some(r) if r <= now => Window::Unknown {
                unknown: format!(
                    "the window reset at {} after the last read{}",
                    fmt_utc(r),
                    ago.as_ref()
                        .map(|a| format!(" {a} ago"))
                        .unwrap_or_default()
                ),
            },
            _ => Window::Known {
                percent,
                resets_at: resets.map(fmt_utc),
            },
        }
    };
    Reading {
        agent: "claude",
        five_hour: window("five_hour", "5-hour"),
        weekly: window("seven_day", "weekly"),
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
    let window = |mins: u64, label: &str| {
        let w = ["primary", "secondary"]
            .iter()
            .map(|k| &limits[*k])
            .find(|w| w["windowDurationMins"].as_u64() == Some(mins));
        match w.and_then(|w| Some((w["usedPercent"].as_f64()?, w))) {
            Some((percent, w)) => Window::Known {
                percent,
                resets_at: w["resetsAt"].as_u64().map(fmt_utc),
            },
            None => Window::Unknown {
                unknown: format!("Codex reported no {label} window"),
            },
        }
    };
    Reading {
        agent: "codex",
        five_hour: window(300, "5-hour"),
        weekly: window(10_080, "weekly"),
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

/// One line per agent: `claude  5h 34% (resets in 2h10m)  week 89% (resets in 1d4h)  read 2m ago`.
pub fn render(r: &Reading, now: u64) -> String {
    let head = format!("{:<6}  ", r.agent);
    if let (Window::Unknown { unknown: a }, Window::Unknown { unknown: b }) =
        (&r.five_hour, &r.weekly)
        && a == b
    {
        return format!("{head}unknown: {a}");
    }
    let parts: Vec<String> = r
        .windows()
        .iter()
        .map(|(short, _, w)| match w {
            Window::Known { percent, resets_at } => {
                let resets = resets_at
                    .as_deref()
                    .and_then(parse_utc)
                    .map(|t| format!(" (resets in {})", left(t.saturating_sub(now))))
                    .unwrap_or_default();
                format!("{short} {}{resets}", pct(*percent))
            }
            Window::Unknown { unknown } => format!("{short} unknown ({unknown})"),
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

/// Each window a reading knows that is over `max` percent, as `codex weekly 40% is over 30%`.
pub fn over(readings: &[Reading], max: f64) -> Vec<String> {
    readings
        .iter()
        .flat_map(|r| r.windows().map(move |(_, long, w)| (r.agent, long, w)))
        .filter_map(|(agent, long, w)| match w {
            Window::Known { percent, .. } if *percent > max => Some(format!(
                "{agent} {long} {} is over {}",
                pct(*percent),
                pct(max)
            )),
            _ => None,
        })
        .collect()
}

/// `reeve usage`: prints the readings, and with `max` exits 1 when a known window is over it.
pub fn main(agent: Option<Kind>, json: bool, max: Option<f64>) -> io::Result<i32> {
    let now = crate::msg::now_secs();
    let readings = read(agent, now);
    if json {
        println!("{}", serde_json::to_string_pretty(&readings)?);
    } else {
        for r in &readings {
            println!("{}", render(r, now));
        }
    }
    let over = max.map(|m| over(&readings, m)).unwrap_or_default();
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

    #[test]
    fn claude_reads_both_windows_and_says_which_one_it_lacks() {
        let r = claude_from(&snap(None, Some((89, NOW + 90_000)), NOW - 120), NOW, "s");
        assert_eq!(
            r.weekly,
            Window::Known {
                percent: 89.0,
                resets_at: Some(fmt_utc(NOW + 90_000))
            }
        );
        assert!(
            matches!(&r.five_hour, Window::Unknown { unknown } if unknown.contains("no 5-hour window"))
        );
        assert!(!r.stale);
        assert_eq!(
            render(&r, NOW),
            "claude  5h unknown (Claude Code's last statusline payload had no 5-hour window)  week 89% (resets in 1d1h)  read 2m ago"
        );
    }

    #[test]
    fn an_old_claude_snapshot_is_stale_and_a_window_that_reset_since_is_unknown() {
        let s = snap(Some((70, NOW - 60)), Some((40, NOW + 7200)), NOW - 3 * 3600);
        let r = claude_from(&s, NOW, "s");
        assert!(r.stale);
        assert!(
            matches!(&r.five_hour, Window::Unknown { unknown } if unknown.contains("reset at") && unknown.contains("3h ago"))
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
        assert!(
            matches!(&none.weekly, Window::Unknown { unknown } if unknown == "Codex reported no weekly window")
        );
    }

    #[test]
    fn a_reading_with_one_reason_for_both_windows_is_one_unknown() {
        let r = Reading::unknown("codex", "s", "codex is not on PATH");
        assert_eq!(render(&r, NOW), "codex   unknown: codex is not on PATH");
    }

    #[test]
    fn over_names_each_known_window_above_max() {
        let r = codex_from(
            &json!({"rateLimits": {
                "primary": {"usedPercent": 81, "windowDurationMins": 300},
                "secondary": {"usedPercent": 80, "windowDurationMins": 10080},
            }}),
            NOW,
        );
        assert_eq!(
            over(std::slice::from_ref(&r), 80.0),
            ["codex 5-hour 81% is over 80%"]
        );
        assert!(over(&[r, Reading::unknown("claude", "s", "x")], 90.0).is_empty());
    }

    #[test]
    fn json_has_percent_or_unknown_per_window() {
        let r = claude_from(&snap(None, Some((89, NOW + 60)), NOW), NOW, "s");
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["weekly"]["percent"], 89.0);
        assert!(v["five_hour"]["unknown"].is_string());
        assert_eq!(v["stale"], false);
    }
}
