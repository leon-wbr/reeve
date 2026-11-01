//! `reeve hook background`: a `reeve heavy` can wait long for a slot, and in the foreground that
//! blocks the session. This PreToolUse hook sets Claude Code's own `run_in_background` on a Bash
//! call that starts with `reeve heavy` and would wait for a slot, so the session keeps working
//! and is told when it ends. One that would start at once stays where the agent put it.

use serde_json::{Value, json};
use std::io::Read;

/// Whether a shell command starts with `reeve heavy`, after any `VAR=value` assignments and with
/// reeve named by any path. `reeve heavy ls` is quick and stays in the foreground.
pub fn starts_heavy(command: &str) -> bool {
    let mut words = command
        .split_whitespace()
        .skip_while(|w| w.split_once('=').is_some_and(|(k, _)| is_name(k)));
    let reeve = words
        .next()
        .is_some_and(|w| w.rsplit('/').next() == Some("reeve"));
    reeve && words.next() == Some("heavy") && !matches!(words.next(), Some("ls" | "--help" | "-h"))
}

fn is_name(k: &str) -> bool {
    let mut cs = k.chars();
    cs.next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && cs.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// The `--lock KEY` of a `reeve heavy` command, if it has one.
pub fn lock_key(command: &str) -> Option<&str> {
    let mut words = command.split_whitespace().take_while(|w| *w != "--");
    while let Some(w) = words.next() {
        if w == "--lock" {
            return words.next();
        }
        if let Some(k) = w.strip_prefix("--lock=") {
            return Some(k);
        }
    }
    None
}

/// Whether a `reeve heavy` started now would wait: someone is in line, every slot is taken, or
/// another run holds its lock key.
pub fn would_wait(
    command: &str,
    waiting: usize,
    running: &[Option<String>],
    slots: usize,
) -> bool {
    let key = lock_key(command);
    waiting > 0
        || running.len() >= slots
        || key.is_some_and(|k| running.iter().any(|l| l.as_deref() == Some(k)))
}

fn would_wait_now(command: &str) -> bool {
    use crate::heavy::{Queue, default_slots};
    let q = Queue::at(Queue::default_root());
    let slots = std::env::var("REEVE_HEAVY_SLOTS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or_else(|| {
            default_slots(std::thread::available_parallelism().map_or(1, |n| n.get()))
        });
    let running: Vec<Option<String>> = q.running().into_iter().map(|e| e.lock).collect();
    would_wait(command, q.waiting().len(), &running, slots)
}

/// The hook's output for a payload: the same Bash input with `run_in_background` set, or `None`
/// to leave the call alone.
pub fn output(payload: &Value) -> Option<Value> {
    output_with(payload, would_wait_now)
}

/// `output`, with how to tell whether the command would wait for a slot. A run that starts at
/// once stays where the agent put it: in the foreground a one-minute suite costs no extra turn,
/// while a background one comes back as a notification the session has to wake for.
pub fn output_with(payload: &Value, would_wait: impl FnOnce(&str) -> bool) -> Option<Value> {
    if payload["hook_event_name"] != "PreToolUse" || payload["tool_name"] != "Bash" {
        return None;
    }
    let input = payload["tool_input"].as_object()?;
    let command = input.get("command")?.as_str()?;
    if input.get("run_in_background") == Some(&json!(true))
        || !starts_heavy(command)
        || !would_wait(command)
    {
        return None;
    }
    let mut input = input.clone();
    input.insert("run_in_background".into(), json!(true));
    Some(json!({
        "hookSpecificOutput": { "hookEventName": "PreToolUse", "updatedInput": input }
    }))
}

/// Reads the hook payload from stdin and prints the hook's output, if any. Every failure means no
/// output, so the call goes ahead unchanged.
pub fn hook(stdin: impl Read) -> Option<String> {
    let payload: Value = serde_json::from_reader(stdin).ok()?;
    Some(output(&payload)?.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bash(input: Value) -> Value {
        json!({ "hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": input })
    }

    #[test]
    fn heavy_commands_are_found_behind_assignments_and_paths() {
        for c in [
            "reeve heavy -- cargo test",
            "  reeve heavy --label e2e -- pnpm test:e2e",
            "RUST_LOG=debug A_1=x reeve heavy -- cargo test",
            "/Users/me/.local/bin/reeve heavy -- make",
            "~/.cargo/bin/reeve\theavy -- make",
        ] {
            assert!(starts_heavy(c), "{c}");
        }
        for c in [
            "reeve heavy ls",
            "reeve heavy --help",
            "reeve ls",
            "reeve",
            "echo reeve heavy -- x",
            "notreeve heavy -- x",
            "cargo test",
            "=x reeve heavy -- x",
            "1A=x reeve heavy -- x",
            "",
        ] {
            assert!(!starts_heavy(c), "{c}");
        }
    }

    fn busy(p: &Value) -> Option<Value> {
        output_with(p, |_| true)
    }

    #[test]
    fn a_heavy_call_that_would_wait_is_sent_to_the_background_with_its_input_kept() {
        let p = bash(
            json!({ "command": "reeve heavy -- cargo test", "description": "Run tests", "timeout": 600000 }),
        );
        let out = busy(&p).unwrap();
        assert_eq!(out["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(
            out["hookSpecificOutput"]["updatedInput"],
            json!({ "command": "reeve heavy -- cargo test", "description": "Run tests", "timeout": 600000, "run_in_background": true })
        );
        assert!(
            out["hookSpecificOutput"]
                .get("permissionDecision")
                .is_none()
        );
    }

    #[test]
    fn a_heavy_call_with_a_free_slot_stays_where_the_agent_put_it() {
        let p = bash(json!({ "command": "reeve heavy -- npm test" }));
        let mut asked = None;
        let out = output_with(&p, |c| {
            asked = Some(c.to_string());
            false
        });
        assert_eq!(out, None);
        assert_eq!(asked.as_deref(), Some("reeve heavy -- npm test"));
    }

    #[test]
    fn a_run_waits_behind_a_line_full_slots_or_its_lock_key() {
        let free = [None, Some("db".to_string())];
        assert!(!would_wait("reeve heavy -- npm test", 0, &free, 4));
        assert!(would_wait("reeve heavy -- npm test", 1, &free, 4));
        assert!(would_wait("reeve heavy -- npm test", 0, &free, 2));
        assert!(would_wait("reeve heavy --lock db -- npm test", 0, &free, 4));
        assert!(would_wait("reeve heavy --lock=db -- npm test", 0, &free, 4));
        assert!(!would_wait("reeve heavy --lock web -- npm test", 0, &free, 4));
        assert_eq!(lock_key("reeve heavy --label x -- run --lock db"), None);
    }

    #[test]
    fn other_calls_are_left_alone() {
        let already = json!({ "command": "reeve heavy -- x", "run_in_background": true });
        assert_eq!(busy(&bash(already)), None);
        assert_eq!(busy(&bash(json!({ "command": "cargo test" }))), None);
        assert_eq!(busy(&bash(json!({}))), None);
        let mut other = bash(json!({ "command": "reeve heavy -- x" }));
        other["tool_name"] = json!("Read");
        assert_eq!(busy(&other), None);
        other["tool_name"] = json!("Bash");
        other["hook_event_name"] = json!("PostToolUse");
        assert_eq!(busy(&other), None);
    }

    #[test]
    fn bad_input_gives_no_output() {
        assert_eq!(hook(&b"not json"[..]), None);
        assert_eq!(hook(&b"{}"[..]), None);
    }
}
