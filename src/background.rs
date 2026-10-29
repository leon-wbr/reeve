//! `reeve hook background`: a `reeve heavy` can wait long for a slot, and in the foreground that
//! blocks the session. This PreToolUse hook sets Claude Code's own `run_in_background` on any Bash
//! call that starts with `reeve heavy`, so the session keeps working and is told when it ends.

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

/// The hook's output for a payload: the same Bash input with `run_in_background` set, or `None`
/// to leave the call alone.
pub fn output(payload: &Value) -> Option<Value> {
    if payload["hook_event_name"] != "PreToolUse" || payload["tool_name"] != "Bash" {
        return None;
    }
    let input = payload["tool_input"].as_object()?;
    if input.get("run_in_background") == Some(&json!(true))
        || !starts_heavy(input.get("command")?.as_str()?)
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

    #[test]
    fn a_heavy_call_is_sent_to_the_background_with_its_input_kept() {
        let p = bash(
            json!({ "command": "reeve heavy -- cargo test", "description": "Run tests", "timeout": 600000 }),
        );
        let out = output(&p).unwrap();
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
    fn other_calls_are_left_alone() {
        let already = json!({ "command": "reeve heavy -- x", "run_in_background": true });
        assert_eq!(output(&bash(already)), None);
        assert_eq!(output(&bash(json!({ "command": "cargo test" }))), None);
        assert_eq!(output(&bash(json!({}))), None);
        let mut other = bash(json!({ "command": "reeve heavy -- x" }));
        other["tool_name"] = json!("Read");
        assert_eq!(output(&other), None);
        other["tool_name"] = json!("Bash");
        other["hook_event_name"] = json!("PostToolUse");
        assert_eq!(output(&other), None);
    }

    #[test]
    fn bad_input_gives_no_output() {
        assert_eq!(hook(&b"not json"[..]), None);
        let p = bash(json!({ "command": "reeve heavy -- x" }));
        let out: Value = serde_json::from_str(&hook(p.to_string().as_bytes()).unwrap()).unwrap();
        assert_eq!(
            out["hookSpecificOutput"]["updatedInput"]["run_in_background"],
            true
        );
    }
}
