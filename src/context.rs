//! `reeve hook context`: a Claude Code or Codex session doesn't see how much context it has used, so
//! this hook tells it once it is past the handoff threshold: on every prompt from then on, and
//! right after the tool call that crossed it, for a session busy with one long turn.

use serde_json::{Value, json};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub const DEFAULT_TOKENS: u64 = 350_000;
pub const ENV: &str = "REEVE_HANDOFF_TOKENS";
/// The key in reeve's config file.
pub const KEY: &str = "handoff_tokens";
/// The usage lines of the last turns sit near the end of a transcript.
const TAIL: u64 = 256 * 1024;

/// `$XDG_CONFIG_HOME/reeve/config.json`, by default `~/.config/reeve/config.json`.
pub fn config_path() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| Some(std::env::home_dir()?.join(".config")))
        .map(|d| d.join("reeve/config.json"))
}

fn positive(v: &Value) -> Option<u64> {
    v.as_u64().filter(|n| *n > 0)
}

/// The env var, then `handoff_tokens` in the config file, then the default; a value that isn't a
/// positive whole number is skipped.
pub fn threshold(env: Option<&str>, config: Option<&str>) -> u64 {
    env.and_then(|e| e.trim().parse().ok())
        .filter(|n| *n > 0)
        .or_else(|| positive(&serde_json::from_str::<Value>(config?).ok()?[KEY]))
        .unwrap_or(DEFAULT_TOKENS)
}

/// Context the assistant turn `back` turns before the last one read, from a transcript's tail:
/// Claude Code's `usage` lines, or the `token_count` events in a Codex rollout, whose input
/// tokens already include the cached ones.
pub fn tokens(tail: &str, back: usize) -> Option<u64> {
    tail.lines()
        .rev()
        .filter(|l| l.contains("\"usage\"") || l.contains("\"token_count\""))
        .filter_map(|l| {
            let v: Value = serde_json::from_str(l).ok()?;
            if let Some(last) = codex_info(&v).map(|i| &i["last_token_usage"]) {
                return last["input_tokens"].as_u64();
            }
            let u = v["message"]["usage"].as_object()?.clone();
            let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
            Some(
                n("input_tokens") + n("cache_read_input_tokens") + n("cache_creation_input_tokens"),
            )
        })
        .nth(back)
}

fn codex_info(v: &Value) -> Option<&Value> {
    let p = &v["payload"];
    (v["type"] == "event_msg" && p["type"] == "token_count")
        .then(|| &p["info"])
        .filter(|i| i.is_object())
}

/// The model's context window, which only a Codex rollout names.
pub fn window(tail: &str) -> Option<u64> {
    tail.lines()
        .rev()
        .filter(|l| l.contains("\"model_context_window\""))
        .find_map(|l| {
            let v: Value = serde_json::from_str(l).ok()?;
            positive(&codex_info(&v)?["model_context_window"])
        })
}

/// Hand off at this share of a known window, the same share as Claude's 350K of 1M.
const WINDOW_PERCENT: u64 = 35;

fn k(n: u64) -> String {
    format!("{}K", (n + 500) / 1000)
}

pub fn text(used: u64, limit: u64) -> String {
    format!(
        "Context check: this session has used {} tokens, past the {} handoff threshold. Finish \
         the step in hand, write your handoff notes (goal, what's done, what's next, open \
         questions) to a file, run `reeve handoff --name NAME --notes FILE` with a new name for \
         your successor, and stop once it returns. Don't take on new work.",
        k(used),
        k(limit)
    )
}

/// The notice for a hook payload, given the transcript's tail. A threshold above the share of
/// the window that the transcript names is lowered to it.
pub fn notice(payload: &Value, tail: &str, limit: u64) -> Option<String> {
    let event = payload["hook_event_name"].as_str()?;
    if event != "UserPromptSubmit" && event != "PostToolUse" {
        return None;
    }
    let limit = window(tail).map_or(limit, |w| limit.min(w * WINDOW_PERCENT / 100));
    let used = tokens(tail, 0).filter(|u| *u >= limit)?;
    if event == "PostToolUse" && tokens(tail, 1).unwrap_or(0) >= limit {
        return None;
    }
    Some(text(used, limit))
}

pub fn output(event: &str, notice: &str) -> Value {
    json!({ "hookSpecificOutput": { "hookEventName": event, "additionalContext": notice } })
}

fn tail_of(path: &Path) -> io::Result<String> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(TAIL)))?;
    let mut buf = vec![];
    f.read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Reads the hook payload from stdin and prints the hook's output, if any. A hook must never
/// get in the session's way, so every failure means no output. A session that handed off
/// passes its prompts on instead (see `handoff::on_prompt`).
pub fn hook(stdin: impl Read) -> Option<String> {
    hook_with(stdin, messages)
}

/// `hook`, with where the session's messages come from.
fn hook_with(stdin: impl Read, messages: impl FnOnce(&Value) -> Option<String>) -> Option<String> {
    let payload: Value = serde_json::from_reader(stdin).ok()?;
    if let Some(out) = crate::handoff::on_prompt(&payload) {
        return Some(out.to_string());
    }
    let event = payload["hook_event_name"].as_str()?;
    if event != "UserPromptSubmit" && event != "PostToolUse" {
        return None;
    }
    let check = || {
        let tail = tail_of(Path::new(payload["transcript_path"].as_str()?)).ok()?;
        let env = std::env::var(ENV).ok();
        let config = config_path().and_then(|p| fs::read_to_string(p).ok());
        notice(
            &payload,
            &tail,
            threshold(env.as_deref(), config.as_deref()),
        )
    };
    let parts: Vec<String> = [messages(&payload), check()]
        .into_iter()
        .flatten()
        .collect();
    (!parts.is_empty()).then(|| output(event, &parts.join("\n\n")).to_string())
}

/// The messages sent to this session's worker that it hasn't been shown yet, claimed so no
/// other hook or follow shows them again. Who the session is gets looked up only when some
/// worker has a message, so a quiet inbox costs one directory read.
fn messages(payload: &Value) -> Option<String> {
    use crate::{inbox, msg, workers};
    let store = msg::Store::open().ok()?;
    let now = msg::now_secs();
    if inbox::unread_counts(&store, now).is_empty() {
        return None;
    }
    let mut c = workers::Caller::from_env();
    let unset = c.claude_session.is_none() && c.codex_thread.is_none();
    if let Some(sid) = payload["session_id"].as_str().filter(|_| unset) {
        c.claude_session = Some(sid.to_string());
    }
    let r = workers::Registry::at(workers::Registry::default_dir().ok()?);
    let me = workers::caller(&r, &c)?;
    let got = inbox::claim(&store, &me.name, now);
    (!got.is_empty()).then(|| inbox::context(&got))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(input: u64, read: u64, created: u64) -> String {
        json!({
            "type": "assistant",
            "message": { "usage": {
                "input_tokens": input,
                "cache_read_input_tokens": read,
                "cache_creation_input_tokens": created,
                "output_tokens": 99
            }}
        })
        .to_string()
    }

    fn transcript(turns: &[u64]) -> String {
        turns
            .iter()
            .flat_map(|t| {
                [
                    r#"{"type":"user","message":{"content":"hi"}}"#.into(),
                    turn(1, *t - 1, 0),
                ]
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn payload(event: &str) -> Value {
        json!({ "hook_event_name": event, "transcript_path": "/t.jsonl" })
    }

    #[test]
    fn the_last_turn_counts_its_input_and_cache_but_not_its_output() {
        let t = [turn(10, 200, 3000), "not json".into(), turn(1, 2, 3)].join("\n");
        assert_eq!(tokens(&t, 0), Some(6));
        assert_eq!(tokens(&t, 1), Some(3210));
        assert_eq!(tokens(&t, 2), None);
        assert_eq!(tokens("", 0), None);
    }

    fn codex_turn(input: u64, window: u64) -> String {
        json!({
            "type": "event_msg",
            "payload": { "type": "token_count", "info": {
                "total_token_usage": { "input_tokens": input * 3, "cached_input_tokens": 9 },
                "last_token_usage": { "input_tokens": input, "cached_input_tokens": input / 2, "output_tokens": 99 },
                "model_context_window": window
            }}
        })
        .to_string()
    }

    #[test]
    fn a_codex_turn_counts_its_last_input_cache_included() {
        let empty = r#"{"type":"event_msg","payload":{"type":"token_count","info":null}}"#;
        let t = [
            codex_turn(1000, 258_400),
            empty.into(),
            codex_turn(2000, 258_400),
        ]
        .join("\n");
        assert_eq!(tokens(&t, 0), Some(2000));
        assert_eq!(tokens(&t, 1), Some(1000));
        assert_eq!(tokens(&t, 2), None);
        assert_eq!(window(&t), Some(258_400));
        assert_eq!(window(&transcript(&[5])), None);
    }

    #[test]
    fn a_codex_session_is_told_before_its_window_fills() {
        let p = payload("UserPromptSubmit");
        let t = |used| [codex_turn(10, 258_400), codex_turn(used, 258_400)].join("\n");
        assert_eq!(notice(&p, &t(90_000), 350_000), None);
        let n = notice(&p, &t(95_000), 350_000).unwrap();
        assert!(
            n.contains("95K tokens, past the 90K handoff threshold"),
            "{n}"
        );
        assert!(notice(&p, &t(100_000), 90_000).is_some());
    }

    #[test]
    fn the_env_beats_the_config_which_beats_the_default() {
        let config = r#"{"handoff_tokens": 200000}"#;
        assert_eq!(threshold(None, None), 350_000);
        assert_eq!(threshold(None, Some(config)), 200_000);
        assert_eq!(threshold(Some("500000"), Some(config)), 500_000);
        for bad in ["", "x", "0", "-5"] {
            assert_eq!(threshold(Some(bad), Some(config)), 200_000, "{bad}");
        }
        for bad in [
            r#"{"handoff_tokens": "x"}"#,
            r#"{"handoff_tokens": 0}"#,
            "{ nope",
            "{}",
        ] {
            assert_eq!(threshold(None, Some(bad)), 350_000, "{bad}");
        }
    }

    #[test]
    fn prompts_are_told_on_every_turn_past_the_threshold() {
        let p = payload("UserPromptSubmit");
        assert_eq!(notice(&p, &transcript(&[100_000, 349_999]), 350_000), None);
        for t in [&[100_000, 350_000][..], &[400_000, 410_000]] {
            let n = notice(&p, &transcript(t), 350_000).unwrap();
            assert!(n.starts_with("Context check:"), "{n}");
        }
        assert_eq!(notice(&p, "", 350_000), None);
    }

    #[test]
    fn a_tool_call_is_told_only_when_it_crossed_the_threshold() {
        let p = payload("PostToolUse");
        assert!(notice(&p, &transcript(&[300_000, 360_000]), 350_000).is_some());
        assert!(notice(&p, &transcript(&[360_000]), 350_000).is_some());
        assert_eq!(notice(&p, &transcript(&[360_000, 370_000]), 350_000), None);
        assert_eq!(notice(&p, &transcript(&[300_000, 340_000]), 350_000), None);
    }

    #[test]
    fn other_events_are_never_told() {
        for e in ["SessionStart", "Stop", "PreToolUse"] {
            assert_eq!(
                notice(&payload(e), &transcript(&[900_000]), 350_000),
                None,
                "{e}"
            );
        }
    }

    #[test]
    fn the_notice_names_both_numbers_and_the_handoff_command() {
        let n = text(412_345, 350_000);
        assert!(
            n.contains("412K tokens, past the 350K handoff threshold"),
            "{n}"
        );
        assert!(n.contains("reeve handoff --name NAME --notes FILE"), "{n}");
        assert!(n.contains("stop"), "{n}");
        let o = output("PostToolUse", &n);
        assert_eq!(o["hookSpecificOutput"]["hookEventName"], "PostToolUse");
        assert_eq!(o["hookSpecificOutput"]["additionalContext"], n);
    }

    fn missing_msg() -> String {
        json!({ "hook_event_name": "PostToolUse", "transcript_path": "/nope" }).to_string()
    }

    #[test]
    fn the_hook_reads_only_the_transcripts_tail_and_stays_quiet_on_bad_input() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("t.jsonl");
        let filler = r#"{"type":"user","message":{"content":"x"}}"#.repeat(10_000);
        fs::write(&file, [filler, transcript(&[900_000])].join("\n")).unwrap();
        let p = json!({ "hook_event_name": "UserPromptSubmit", "transcript_path": file });
        let hook = |b: &[u8]| hook_with(b, |_| None);
        let out: Value = serde_json::from_str(&hook(p.to_string().as_bytes()).unwrap()).unwrap();
        assert_eq!(
            out["hookSpecificOutput"]["hookEventName"],
            "UserPromptSubmit"
        );
        assert_eq!(hook(&b"not json"[..]), None);
        // Messages show without a transcript, and beside the context check.
        let mail = |b: &[u8]| hook_with(b, |_| Some("reeve: a message for you".into()));
        let m: Value = serde_json::from_str(&mail(missing_msg().as_bytes()).unwrap()).unwrap();
        assert_eq!(
            m["hookSpecificOutput"]["additionalContext"],
            "reeve: a message for you"
        );
        let both: Value = serde_json::from_str(&mail(p.to_string().as_bytes()).unwrap()).unwrap();
        let ctx = both["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(
            ctx.starts_with("reeve: a message") && ctx.contains("Context check"),
            "{ctx}"
        );
        let start = json!({ "hook_event_name": "SessionStart" }).to_string();
        assert_eq!(mail(start.as_bytes()), None);
        let missing = json!({ "hook_event_name": "UserPromptSubmit", "transcript_path": "/nope" });
        assert_eq!(hook(missing.to_string().as_bytes()), None);
    }
}
