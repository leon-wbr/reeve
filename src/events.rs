//! The event log: every reeve command appends one JSON line to `events.jsonl` saying what ran,
//! who ran it, how it ended and how long it took. It never holds prompts, file contents,
//! reports or env values, and it stays on this machine (see docs/events.md).

use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// ms since the epoch, when the command started.
    pub ts: u64,
    pub version: String,
    /// The subcommand path, e.g. `ls` or `sandbox build`; never its arguments.
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
    /// A heavy command's `--label` or program name, or a sandbox name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Missing when reeve replaced itself with the command (a nested `reeve heavy`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heavy_wait_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heavy_run_ms: Option<u64>,
    /// From `reeve run --name` until the new worker's agent is up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_ms: Option<u64>,
    /// `agent` or `person`.
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

impl Event {
    pub fn start(command: &str) -> Event {
        let (actor, agent) = actor(|k| std::env::var(k).ok());
        Event {
            ts: now_ms(),
            version: env!("CARGO_PKG_VERSION").into(),
            command: command.into(),
            actor: actor.into(),
            agent: agent.map(Into::into),
            ..Event::default()
        }
    }

    pub fn finish(&mut self, exit: i32, error: Option<&io::Error>) {
        self.exit = Some(exit);
        self.error = error.map(error_kind);
        self.ms = now_ms().saturating_sub(self.ts);
    }
}

const AGENTS: [&str; 8] = [
    "claude-code",
    "codex",
    "gemini",
    "cursor",
    "aider",
    "opencode",
    "amp",
    "copilot",
];

/// Who runs reeve, from the variables agent CLIs set for their tools. `AI_AGENT`'s value is
/// only kept when it names a known agent, so no free-form env value reaches the log.
pub fn actor(var: impl Fn(&str) -> Option<String>) -> (&'static str, Option<&'static str>) {
    let set = |k| var(k).is_some_and(|v| !v.is_empty());
    let agent = if set("CLAUDECODE") {
        Some("claude-code")
    } else if set("CODEX_SANDBOX") || set("CODEX_SANDBOX_NETWORK_DISABLED") {
        Some("codex")
    } else if let Some(v) = var("AI_AGENT").filter(|v| !v.is_empty()) {
        let v = v.to_ascii_lowercase();
        Some(
            AGENTS
                .into_iter()
                .find(|a| v.starts_with(a))
                .unwrap_or("other"),
        )
    } else {
        None
    };
    (if agent.is_some() { "agent" } else { "person" }, agent)
}

/// Messages carry names and paths, so only a fixed kind is logged.
const KINDS: [(&str, &str); 18] = [
    ("no worker ", "no_worker"),
    ("must match [a-z]", "bad_name"),
    ("needs its prompt", "no_prompt"),
    ("still has an agent", "worker_busy"),
    ("is gone without", "worker_gone"),
    ("is idle without a report", "worker_idle"),
    ("is waiting for the user", "worker_blocked"),
    ("has no report yet", "no_report"),
    ("timed out", "timeout"),
    ("need --sandbox", "usage"),
    ("needs --sandbox", "usage"),
    ("nothing to run", "usage"),
    ("no sandbox env file", "no_env_file"),
    ("firewall", "firewall"),
    ("is not a reeve sandbox", "no_sandbox"),
    ("claude --bg", "claude_start"),
    ("herdr", "herdr"),
    ("docker", "docker"),
];

pub fn error_kind(e: &io::Error) -> String {
    if e.kind() != io::ErrorKind::Other {
        return snake(&format!("{:?}", e.kind()));
    }
    let msg = e.to_string();
    KINDS
        .iter()
        .find(|(needle, _)| msg.contains(needle))
        .map_or("other", |(_, kind)| kind)
        .into()
}

fn snake(camel: &str) -> String {
    let mut out = String::new();
    for (i, c) in camel.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// `REEVE_EVENTS`, else `events.jsonl` in reeve's state dir.
pub fn path() -> io::Result<PathBuf> {
    match std::env::var_os("REEVE_EVENTS").filter(|v| !v.is_empty()) {
        Some(p) => Ok(p.into()),
        None => crate::workers::Registry::default_dir().map(|d| d.join("events.jsonl")),
    }
}

/// One `write` on an `O_APPEND` file, so lines from concurrent commands never interleave.
pub fn append(e: &Event) -> io::Result<()> {
    let path = path()?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut line = serde_json::to_vec(e)?;
    line.push(b'\n');
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    let n = f.write(&line)?;
    if n < line.len() {
        return Err(io::Error::other("short write"));
    }
    Ok(())
}

/// Appends `e`; a failure is only reported, never passed on to the command.
pub fn log(e: &Event) {
    if let Err(err) = append(e) {
        eprintln!("reeve: could not log this command ({err}); it ran as usual");
    }
}

/// All events in the log, skipping lines that don't parse.
pub fn read() -> io::Result<Vec<Event>> {
    match fs::read_to_string(path()?) {
        Ok(s) => Ok(s
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(e),
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn agents_are_told_by_the_variables_their_clis_set() {
        assert_eq!(actor(env(&[])), ("person", None));
        assert_eq!(
            actor(env(&[("CLAUDECODE", "1")])),
            ("agent", Some("claude-code"))
        );
        assert_eq!(actor(env(&[("CLAUDECODE", "")])), ("person", None));
        assert_eq!(
            actor(env(&[("CODEX_SANDBOX", "seatbelt")])),
            ("agent", Some("codex"))
        );
        assert_eq!(
            actor(env(&[("AI_AGENT", "Cursor-1.2")])),
            ("agent", Some("cursor"))
        );
    }

    #[test]
    fn an_unknown_ai_agent_value_is_not_logged() {
        assert_eq!(
            actor(env(&[("AI_AGENT", "token=hunter2")])),
            ("agent", Some("other"))
        );
    }

    #[test]
    fn error_kinds_come_from_the_real_messages_and_never_carry_them() {
        let kind = |m: &str| error_kind(&io::Error::other(m.to_string()));
        let bad_name = crate::workers::check_name("Fix").unwrap_err();
        assert_eq!(kind(&bad_name), "bad_name");
        let no_prompt = crate::workers::split_prompt(&[]).unwrap_err();
        assert_eq!(kind(&no_prompt), "no_prompt");
        assert_eq!(kind("no worker x; see `reeve ls`"), "no_worker");
        assert_eq!(
            kind("worker x is gone without writing its report"),
            "worker_gone"
        );
        assert_eq!(
            kind("worker x is idle without a report; send it a message, then wait again"),
            "worker_idle"
        );
        assert_eq!(
            kind("worker x is waiting for the user, such as a permission prompt"),
            "worker_blocked"
        );
        assert_eq!(kind("worker x timed out"), "timeout");
        assert_eq!(kind("worker x has no report yet (running)"), "no_report");
        assert_eq!(kind("sandbox options need --sandbox"), "usage");
        assert_eq!(kind("something new at /secret/path"), "other");
        assert_eq!(
            error_kind(&io::Error::from(io::ErrorKind::PermissionDenied)),
            "permission_denied"
        );
    }

    #[test]
    fn empty_fields_are_left_out_of_the_line() {
        let e = Event {
            ts: 1,
            version: "0.1.0".into(),
            command: "ls".into(),
            exit: Some(0),
            ms: 2,
            actor: "person".into(),
            ..Event::default()
        };
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"ts":1,"version":"0.1.0","command":"ls","exit":0,"ms":2,"actor":"person"}"#
        );
    }
}
