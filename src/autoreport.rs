//! Problems reeve recognises as its own fault (a panic, a failed invariant, a registry it can't
//! read) filed as issues on its repo, labelled `auto-reported`. Off unless `REEVE_AUTOREPORT=1` or
//! `"autoreport": true` in the config file. The repo is public, so everything sent passes `redact`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const LABEL: &str = "auto-reported";
pub const REPO: &str = "leon-wbr/reeve";
const MAX_NEW_PER_DAY: u32 = 5;
const MAX_MESSAGE: usize = 300;
const MAX_FRAMES: usize = 12;

pub fn enabled(env: Option<&str>, config: Option<&str>) -> bool {
    flag(env, config, "autoreport")
}

/// An on/off switch: the env var if set, else `key` in the config file.
pub fn flag(env: Option<&str>, config: Option<&str>, key: &str) -> bool {
    match env.map(|e| e.trim().to_ascii_lowercase()).as_deref() {
        Some("1" | "true" | "on" | "yes" | "dry") => true,
        Some(_) => false,
        None => config
            .and_then(|c| serde_json::from_str::<serde_json::Value>(c).ok())
            .is_some_and(|v| v[key] == true),
    }
}

fn is_enabled() -> bool {
    let config = crate::context::config_path().and_then(|p| std::fs::read_to_string(p).ok());
    enabled(
        std::env::var("REEVE_AUTOREPORT").ok().as_deref(),
        config.as_deref(),
    )
}

/// What identifies this machine's user; each is blanked wherever it appears.
#[derive(Default)]
pub struct Own {
    pub user: Option<String>,
    pub host: Option<String>,
    pub home: Option<String>,
}

impl Own {
    pub fn here() -> Own {
        let var = |k| std::env::var(k).ok().filter(|v| v.len() > 1);
        let mut buf = [0 as libc::c_char; 256];
        let host = (unsafe { libc::gethostname(buf.as_mut_ptr(), buf.len()) } == 0)
            .then(|| {
                let bytes: Vec<u8> = buf
                    .iter()
                    .take_while(|c| **c != 0)
                    .map(|c| *c as u8)
                    .collect();
                String::from_utf8_lossy(&bytes).into_owned()
            })
            .filter(|h| h.len() > 1);
        Own {
            user: var("USER").or_else(|| var("LOGNAME")),
            host,
            home: var("HOME"),
        }
    }
}

fn is_path_start(prev: char) -> bool {
    prev.is_whitespace() || "\"'=(`[:".contains(prev)
}

/// A path token kept only as far as it points into reeve's own source.
fn source_relative(path: &str) -> Option<&str> {
    ["reeve/src/", "reeve/crates/"]
        .iter()
        .filter_map(|m| path.rfind(m).map(|i| &path[i + "reeve/".len()..]))
        .max_by_key(|p| p.len())
        .or_else(|| {
            let p = path.trim_start_matches("./");
            (p.starts_with("src/") || p.starts_with("crates/")).then_some(p)
        })
}

fn replace_quoted(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let opens = match c {
            '"' => true,
            '\'' => i == 0 || "( [=".contains(chars[i - 1]),
            _ => false,
        };
        if opens {
            let close = (i + 1..chars.len()).find(|&j| {
                chars[j] == c && (c == '"' || chars.get(j + 1).is_none_or(|n| !n.is_alphanumeric()))
            });
            if let Some(j) = close {
                out.push(c);
                out.push('…');
                out.push(c);
                i = j + 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

fn token_end(c: char) -> bool {
    c.is_whitespace() || "\"'),;`]".contains(c)
}

/// Everything in `s` that isn't reeve's own: quoted text, URLs, emails, paths outside reeve's
/// source, `NAME=value` settings and this machine's user, host and home.
pub fn redact_with(s: &str, own: &Own) -> String {
    let s = replace_quoted(s);
    let mut out = String::new();
    let mut prev = ' ';
    let mut rest = s.as_str();
    while let Some(c) = rest.chars().next() {
        let tail = &rest[c.len_utf8()..];
        let url = c.is_ascii_alphabetic() && is_path_start(prev) && url_len(rest).is_some();
        let path = (c == '/' || (c == '~' && tail.starts_with('/'))) && is_path_start(prev);
        let relative = is_path_start(prev)
            && (rest.starts_with("./src/")
                || rest.starts_with("src/")
                || rest.starts_with("crates/"));
        let env = c.is_ascii_uppercase() && is_path_start(prev) && env_len(rest).is_some();
        let (len, text) = if url {
            let n = url_len(rest).unwrap_or(0);
            (n, "<url>".to_string())
        } else if path || relative {
            let n = rest.find(token_end).unwrap_or(rest.len());
            let tok = &rest[..n];
            let (body, suffix) = tok.split_at(tok.find(':').unwrap_or(tok.len()));
            let kept = source_relative(body)
                .map_or_else(|| "<path>".to_string(), |p| format!("{p}{suffix}"));
            (n, kept)
        } else if env {
            let n = env_len(rest).unwrap_or(0);
            (
                n,
                format!("{}<redacted>", &rest[..n - env_value_len(rest, n)]),
            )
        } else {
            (c.len_utf8(), c.to_string())
        };
        out.push_str(&text);
        prev = text.chars().last().unwrap_or(' ');
        rest = &rest[len..];
    }
    for (v, label) in [
        (&own.home, "<home>"),
        (&own.user, "<user>"),
        (&own.host, "<host>"),
    ] {
        if let Some(v) = v {
            out = out.replace(v.as_str(), label);
        }
    }
    redact_emails(&out)
}

fn url_len(s: &str) -> Option<usize> {
    let scheme = s.find("://")?;
    s[..scheme]
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
        .then(|| s.find(token_end).unwrap_or(s.len()))
}

/// Length of `NAME=value` at the start of `s` (value up to whitespace), if NAME looks like an
/// environment variable.
fn env_len(s: &str) -> Option<usize> {
    let eq = s.find('=')?;
    let name = &s[..eq];
    (name.len() >= 3
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
    .then(|| s.find(char::is_whitespace).unwrap_or(s.len()))
}

fn env_value_len(s: &str, n: usize) -> usize {
    n - s.find('=').map_or(0, |i| i + 1)
}

fn redact_emails(s: &str) -> String {
    s.split_inclusive(char::is_whitespace)
        .map(|w| {
            let word = w.trim_end();
            match word.find('@') {
                Some(i) if i > 0 && word[i + 1..].contains('.') => {
                    format!("<email>{}", &w[word.len()..])
                }
                _ => w.to_string(),
            }
        })
        .collect()
}

fn fnv(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")[..12].to_string()
}

pub struct Problem {
    pub kind: String,
    pub message: String,
    pub backtrace: Vec<String>,
}

impl Problem {
    pub fn new(kind: &str, message: &str, backtrace: &str, own: &Own) -> Problem {
        let message: String = redact_with(message, own)
            .chars()
            .take(MAX_MESSAGE)
            .collect();
        Problem {
            kind: kind.into(),
            message,
            backtrace: reeve_frames(backtrace, own),
        }
    }

    /// Kind plus the message with its numbers blanked, so a changed count or line is one problem.
    pub fn fingerprint(&self) -> String {
        let first = self.message.lines().next().unwrap_or("");
        let mut norm = String::new();
        for c in first.chars() {
            if !c.is_ascii_digit() || !norm.ends_with('N') {
                norm.push(if c.is_ascii_digit() { 'N' } else { c });
            }
        }
        fnv(&format!("{}|{norm}", self.kind))
    }

    pub fn title(&self) -> String {
        crate::feedback::title(&format!("auto-reported: {}: {}", self.kind, self.message))
    }

    pub fn body(&self, version: &str, os: &str) -> String {
        format!(
            "Auto-reported by reeve {version} ({os}).\n\n- kind: {}\n- fingerprint: {}\n\n```text\n{}\n```\n\nBacktrace into reeve:\n\n```text\n{}\n```\n",
            self.kind,
            self.fingerprint(),
            self.message,
            self.backtrace.join("\n")
        )
    }
}

/// The frames of a captured backtrace that are in reeve, as `symbol at src/file.rs:line`.
fn reeve_frames(bt: &str, own: &Own) -> Vec<String> {
    let mut out = vec![];
    let mut lines = bt.lines().map(str::trim).peekable();
    while let Some(l) = lines.next() {
        let Some((_, sym)) = l.split_once(": ") else {
            continue;
        };
        if !sym.starts_with("reeve::") {
            continue;
        }
        let sym = sym.rsplit_once("::h").map_or(sym, |(s, _)| s);
        let at = lines
            .peek()
            .and_then(|n| n.strip_prefix("at "))
            .map(|p| redact_with(p, own))
            .filter(|p| p.starts_with("src/") || p.starts_with("crates/"));
        out.push(match at {
            Some(at) => format!("{sym} at {at}"),
            None => sym.to_string(),
        });
        if out.len() == MAX_FRAMES {
            break;
        }
    }
    out
}

#[derive(Default, Serialize, Deserialize, Debug, PartialEq)]
pub struct State {
    day: String,
    new: u32,
    seen: BTreeMap<String, u32>,
    commented: BTreeMap<String, String>,
}

impl State {
    fn load(path: &Path) -> State {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string(self)?)
    }
}

pub trait Gh {
    fn open_issue(&self, fingerprint: &str) -> io::Result<Option<u64>>;
    fn create(&self, title: &str, body: &str) -> io::Result<String>;
    fn comment(&self, issue: u64, body: &str) -> io::Result<()>;
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Created(String),
    Commented(u64),
    Quiet,
    RateLimited,
}

/// Files `p`, or comments on its open issue; at most `MAX_NEW_PER_DAY` new issues and one comment
/// per problem a day.
pub fn file(
    p: &Problem,
    gh: &dyn Gh,
    st: &mut State,
    today: &str,
    version: &str,
) -> io::Result<Outcome> {
    if st.day != today {
        st.day = today.into();
        st.new = 0;
    }
    let fp = p.fingerprint();
    let seen = st.seen.entry(fp.clone()).or_default();
    *seen += 1;
    let seen = *seen;
    if let Some(n) = gh.open_issue(&fp)? {
        if st.commented.get(&fp).is_some_and(|d| d == today) {
            return Ok(Outcome::Quiet);
        }
        gh.comment(
            n,
            &format!(
                "Seen again by reeve {version} ({}); {seen} times on that machine so far.",
                std::env::consts::OS
            ),
        )?;
        st.commented.insert(fp, today.into());
        return Ok(Outcome::Commented(n));
    }
    if st.new >= MAX_NEW_PER_DAY {
        return Ok(Outcome::RateLimited);
    }
    let url = gh.create(&p.title(), &p.body(version, std::env::consts::OS))?;
    st.new += 1;
    Ok(Outcome::Created(url))
}

struct Real {
    scanner: crate::leak::Scanner,
    dry: bool,
}

impl Real {
    /// Posts nothing that the leak scanner flags; the text is kept locally instead.
    fn guard(&self, what: &str, text: &str) -> io::Result<()> {
        let hits = self.scanner.scan(text);
        if hits.is_empty() {
            return Ok(());
        }
        let why = crate::leak::render(&hits);
        if let Ok(dir) = state_path().map(|p| p.with_file_name("autoreport-blocked")) {
            let _ = std::fs::create_dir_all(&dir);
            let stamp = crate::events::now_ms();
            let _ = std::fs::write(
                dir.join(format!("{stamp}.txt")),
                format!("{what}\n{why}\n\n{text}"),
            );
        }
        Err(io::Error::other(format!(
            "blocked by the leak scanner:\n{why}"
        )))
    }
}

pub(crate) fn gh(args: &[&str]) -> io::Result<String> {
    let out = Command::new("gh")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other("gh failed"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

impl Gh for Real {
    fn open_issue(&self, fp: &str) -> io::Result<Option<u64>> {
        let list = gh(&[
            "issue",
            "list",
            "--repo",
            REPO,
            "--label",
            LABEL,
            "--state",
            "open",
            "--limit",
            "100",
            "--json",
            "number,body",
        ])?;
        let v: serde_json::Value = serde_json::from_str(&list)?;
        let needle = format!("fingerprint: {fp}");
        Ok(v.as_array()
            .into_iter()
            .flatten()
            .find(|i| i["body"].as_str().is_some_and(|b| b.contains(&needle)))
            .and_then(|i| i["number"].as_u64()))
    }

    fn create(&self, title: &str, body: &str) -> io::Result<String> {
        self.guard("issue", &format!("{title}\n{body}"))?;
        if self.dry {
            println!("would file an issue on {REPO}\ntitle: {title}\n\n{body}");
            return Ok("(dry run)".into());
        }
        gh(&[
            "label",
            "create",
            LABEL,
            "--repo",
            REPO,
            "--force",
            "--color",
            "ededed",
            "--description",
            "Filed by reeve itself",
        ])?;
        gh(&[
            "issue", "create", "--repo", REPO, "--label", LABEL, "--title", title, "--body", body,
        ])
    }

    fn comment(&self, issue: u64, body: &str) -> io::Result<()> {
        self.guard("comment", body)?;
        if self.dry {
            println!("would comment on {REPO}#{issue}\n\n{body}");
            return Ok(());
        }
        gh(&[
            "issue",
            "comment",
            &issue.to_string(),
            "--repo",
            REPO,
            "--body",
            body,
        ])
        .map(|_| ())
    }
}

fn state_path() -> io::Result<PathBuf> {
    Ok(crate::workers::Registry::default_dir()?.join("autoreport.json"))
}

fn send(p: &Problem) -> io::Result<Outcome> {
    let path = state_path()?;
    let mut st = State::load(&path);
    let today = &crate::feedback::iso(crate::events::now_ms())[..10];
    let real = Real {
        scanner: crate::leak::Scanner::here(REPO),
        dry: std::env::var("REEVE_AUTOREPORT").is_ok_and(|v| v == "dry"),
    };
    let out = file(p, &real, &mut st, today, crate::build_info::LONG_VERSION);
    if !real.dry {
        st.save(&path)?;
    }
    out
}

/// Files a problem reeve found in itself, if auto-reporting is on. Both texts are literals in
/// reeve's source (`&'static`), so nothing of the user's data can end up in an issue. Never
/// fails the caller.
pub fn report(kind: &'static str, detail: &'static str) {
    report_at(kind, detail, None);
}

fn report_at(kind: &'static str, detail: &'static str, at: Option<String>) {
    if !is_enabled() {
        return;
    }
    let bt = std::backtrace::Backtrace::force_capture().to_string();
    let message = match at {
        Some(at) => format!("{detail} at {at}"),
        None => detail.to_string(),
    };
    if let Err(e) = send(&Problem::new(kind, &message, &bt, &Own::here())) {
        eprintln!("reeve: not auto-reported: {e}");
    }
}

/// Makes a panic report itself, after the usual panic message. The panic's own message is left
/// out; the issue says only where in reeve's source it happened.
pub fn install() {
    if !is_enabled() {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        previous(info);
        let at = info
            .location()
            .and_then(|l| source_relative(l.file()).map(|f| format!("{f}:{}", l.line())));
        report_at("panic", "reeve panicked", at);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn own() -> Own {
        Own {
            user: Some("jdoe".into()),
            host: Some("jdoes-mbp".into()),
            home: Some("/Users/jdoe".into()),
        }
    }

    #[test]
    fn it_is_off_unless_the_env_or_the_config_turns_it_on() {
        assert!(!enabled(None, None));
        assert!(!enabled(None, Some("{}")));
        assert!(enabled(None, Some(r#"{"autoreport": true}"#)));
        assert!(enabled(Some("1"), None));
        assert!(!enabled(Some("0"), Some(r#"{"autoreport": true}"#)));
        assert!(!enabled(None, Some("not json")));
    }

    #[test]
    fn paths_outside_reeve_are_stripped_and_reeves_own_source_stays_relative() {
        let r = |s| redact_with(s, &own());
        assert_eq!(r("open /Users/u/proj/a.txt failed"), "open <path> failed");
        assert_eq!(r("see ~/secrets/x"), "see <path>");
        assert_eq!(
            r("panicked at /Users/x/work/reeve/src/heavy.rs:12:5"),
            "panicked at src/heavy.rs:12:5"
        );
        assert_eq!(r("at ./src/feedback.rs:3"), "at src/feedback.rs:3");
        assert_eq!(
            r("at /Users/x/.cargo/registry/src/foo/lib.rs:9"),
            "at <path>"
        );
    }

    #[test]
    fn worker_and_project_names_in_quotes_are_stripped() {
        let r = |s| redact_with(s, &own());
        assert_eq!(
            r(r#"no worker "secret-project-7" in registry"#),
            r#"no worker "…" in registry"#
        );
        assert_eq!(r("lost 'client-acme' again"), "lost '…' again");
        assert_eq!(r("it can't open"), "it can't open");
    }

    #[test]
    fn usernames_hostnames_and_the_home_dir_are_stripped() {
        let r = redact_with("jdoe on jdoes-mbp owns /Users/jdoe/x and jdoe", &own());
        assert!(!r.contains("jdoe") && !r.contains("jdoes-mbp"), "{r}");
    }

    #[test]
    fn emails_urls_and_env_values_are_stripped() {
        let r = |s| redact_with(s, &own());
        assert_eq!(r("mail a.b@example.com now"), "mail <email> now");
        assert_eq!(
            r("fetch https://ghp_x:y@git.corp.example/a failed"),
            "fetch <url> failed"
        );
        assert_eq!(
            r("env GH_TOKEN=ghp_abc123 HOME=/x set"),
            "env GH_TOKEN=<redacted> HOME=<redacted> set"
        );
    }

    #[test]
    fn a_backtrace_keeps_only_frames_in_reeve_and_their_source_lines() {
        let bt = "   0: std::panicking::begin\n             at /rustc/abc/library/std/src/panicking.rs:1\n   1: reeve::heavy::run::h0123456789abcdef\n             at /Users/jdoe/work/reeve/src/heavy.rs:77:9\n   2: tokio::x\n             at /Users/jdoe/.cargo/registry/x.rs:1\n   3: reeve::main\n";
        assert_eq!(
            reeve_frames(bt, &own()),
            ["reeve::heavy::run at src/heavy.rs:77:9", "reeve::main"]
        );
    }

    #[test]
    fn a_problem_has_a_stable_fingerprint_across_changing_numbers() {
        let p = |m| Problem::new("panic", m, "", &own());
        assert_eq!(
            p("slot 3 of 40 gone").fingerprint(),
            p("slot 12 of 4 gone").fingerprint()
        );
        assert_ne!(
            p("slot 3 gone").fingerprint(),
            p("slot 3 lost").fingerprint()
        );
        assert_ne!(
            p("x").fingerprint(),
            Problem::new("doctor", "x", "", &own()).fingerprint()
        );
    }

    #[test]
    fn the_issue_says_it_was_auto_reported_and_carries_nothing_private() {
        let p = Problem::new(
            "panic",
            "failed for /Users/jdoe/private/x with jdoe@example.com",
            "",
            &own(),
        );
        let b = p.body("0.1.0", "macos");
        assert!(b.contains("Auto-reported by reeve 0.1.0 (macos)"));
        assert!(b.contains(&format!("fingerprint: {}", p.fingerprint())));
        assert!(!b.contains("private") && !b.contains("jdoe"), "{b}");
    }

    #[derive(Default)]
    struct Fake {
        open: Option<u64>,
        made: RefCell<Vec<String>>,
        comments: RefCell<Vec<(u64, String)>>,
    }

    impl Gh for Fake {
        fn open_issue(&self, _: &str) -> io::Result<Option<u64>> {
            Ok(self.open)
        }
        fn create(&self, title: &str, _: &str) -> io::Result<String> {
            self.made.borrow_mut().push(title.into());
            Ok(format!(
                "https://github.com/x/y/issues/{}",
                self.made.borrow().len()
            ))
        }
        fn comment(&self, n: u64, body: &str) -> io::Result<()> {
            self.comments.borrow_mut().push((n, body.into()));
            Ok(())
        }
    }

    fn prob(m: &str) -> Problem {
        Problem::new("panic", m, "", &own())
    }

    #[test]
    fn an_open_issue_gets_a_comment_with_the_count_not_a_new_issue() {
        let gh = Fake {
            open: Some(9),
            ..Fake::default()
        };
        let mut st = State::default();
        let out = file(&prob("boom"), &gh, &mut st, "2026-11-16", "0.1.0").unwrap();
        assert_eq!(out, Outcome::Commented(9));
        assert!(gh.made.borrow().is_empty());
        assert!(gh.comments.borrow()[0].1.contains("1 times"));
        let again = file(&prob("boom"), &gh, &mut st, "2026-11-16", "0.1.0").unwrap();
        assert_eq!(again, Outcome::Quiet);
        let next_day = file(&prob("boom"), &gh, &mut st, "2026-11-17", "0.1.0").unwrap();
        assert_eq!(next_day, Outcome::Commented(9));
        assert!(gh.comments.borrow()[1].1.contains("3 times"));
    }

    #[test]
    fn at_most_five_new_issues_a_day() {
        let gh = Fake::default();
        let mut st = State::default();
        for i in 0..5 {
            let out = file(
                &prob(&format!("p{}", "x".repeat(i + 1))),
                &gh,
                &mut st,
                "d1",
                "v",
            )
            .unwrap();
            assert!(matches!(out, Outcome::Created(_)));
        }
        assert_eq!(
            file(&prob("another"), &gh, &mut st, "d1", "v").unwrap(),
            Outcome::RateLimited
        );
        assert!(matches!(
            file(&prob("another"), &gh, &mut st, "d2", "v").unwrap(),
            Outcome::Created(_)
        ));
    }
}
