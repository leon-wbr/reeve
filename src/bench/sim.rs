//! The simulated user: reads what the communicator said and replies only from the answer key.

use serde_json::{Value, json};
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;

pub struct Sim<'a> {
    pub asks: &'a str,
    pub key: &'a str,
    pub items: &'a [&'a str],
    pub history: &'a [(&'static str, String)],
    pub silent_minutes: i64,
    pub env: &'a [(OsString, OsString)],
    pub cwd: &'a Path,
}

fn schema() -> String {
    json!({
        "type": "object",
        "properties": {
            "claimedDone": { "type": "array", "items": { "type": "string" } },
            "allDone": { "type": "boolean" },
            "reply": { "type": ["string", "null"] },
            "kind": { "enum": ["answer", "merge", "poll", "recovery", "other", "none"] },
        },
        "required": ["claimedDone", "allDone", "reply", "kind"],
    })
    .to_string()
}

fn prompt(s: &Sim) -> String {
    let history: Vec<String> = s
        .history
        .iter()
        .map(|(from, text)| format!("[{from}]\n{text}"))
        .collect();
    let silent = if s.silent_minutes != 0 {
        format!(
            "\nThe communicator has said nothing for {} minutes.",
            s.silent_minutes
        )
    } else {
        String::new()
    };
    format!(
        r#"You play a busy developer who handed some asks to an AI communicator and is now reading its terminal. You know only the answer key below. You never do work yourself.

Your first message (item ids, in order: {items}):
{asks}

{key}

Conversation so far:
{history}
{silent}

Decide what to do now:
- claimedDone: the items the communicator has, at any point, said are finished and merged (not just "ready").
- allDone: true only if the communicator has said all items are finished and merged and nothing is left.
- reply: null if nothing needs you (it is still working, or it reported progress). Otherwise one short message, as the developer would type it: answer its question from the key, approve a merge, or, if it is stuck, silent for a long time, or promised something it didn't do, nudge it. Never reply just to acknowledge.
- kind: answer (a decision from the key), merge (approving a merge), poll (asking for status after silence), recovery (pushing it past its own trouble), other, or none when reply is null."#,
        items = s.items.join(", "),
        asks = s.asks,
        key = s.key,
        history = history.join("\n\n"),
    )
}

pub fn simulate(s: &Sim) -> io::Result<Value> {
    let mut p = Command::new("claude")
        .args(["-p", "--model", "sonnet", "--safe-mode", "--tools", ""])
        .args(["--output-format", "json", "--json-schema", &schema()])
        .env_clear()
        .envs(s.env.iter().map(|(k, v)| (k, v)))
        .current_dir(s.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut stdin = p.stdin.take().expect("piped");
    let text = prompt(s);
    let writer = thread::spawn(move || stdin.write_all(text.as_bytes()));
    let out = p.wait_with_output()?;
    let _ = writer.join();
    if !out.status.success() {
        return Err(io::Error::other(format!("sim exited {}", out.status)));
    }
    let r: Value = serde_json::from_slice(&out.stdout)?;
    match r.get("structured_output") {
        Some(v) if !v.is_null() => Ok(v.clone()),
        _ => Ok(serde_json::from_str(
            r["result"].as_str().unwrap_or_default(),
        )?),
    }
}
