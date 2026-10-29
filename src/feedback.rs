//! `reeve feedback`: a note about reeve itself, saved locally with enough context to act on,
//! then offered for posting as a GitHub issue. reeve never posts without the user's yes.

use crate::events::Event;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const REPO: &str = "leon-wbr/reeve";
const NEW_ISSUE: &str = "https://github.com/leon-wbr/reeve/issues/new";
/// Browsers and GitHub cope with URLs well beyond this, but not much further.
const MAX_URL: usize = 8000;
const LAST: usize = 20;

/// `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub fn iso(ms: u64) -> String {
    let secs = ms / 1000;
    let (y, m, d) = civil(secs / 86_400);
    let t = secs % 86_400;
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        t / 3600,
        t / 60 % 60,
        t % 60,
        ms % 1000
    )
}

/// Days since 1970-01-01 to a date (Howard Hinnant's `civil_from_days`).
pub(crate) fn civil(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + u64::from(m <= 2), m, d)
}

/// Replaces anything that looks like a path, absolute or under `~`, with `<path>`.
pub fn redact(s: &str) -> String {
    let mut out = String::new();
    let mut prev = ' ';
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        let starts = c == '/' || (c == '~' && chars.peek() == Some(&'/'));
        if starts && (prev.is_whitespace() || "\"'=(".contains(prev)) {
            while chars
                .peek()
                .is_some_and(|&n| !n.is_whitespace() && !"\"'),;".contains(n))
            {
                chars.next();
            }
            out.push_str("<path>");
            prev = '>';
        } else {
            out.push(c);
            prev = c;
        }
    }
    out
}

pub fn note(text: &str, now_ms: u64, events: &[Event], with_paths: bool) -> String {
    let start = events.len().saturating_sub(LAST);
    let lines: Vec<String> = events[start..]
        .iter()
        .filter_map(|e| serde_json::to_string(e).ok())
        .map(|l| if with_paths { l } else { redact(&l) })
        .collect();
    format!(
        "# reeve feedback\n\n{text}\n\n- reeve {}\n- {} {}\n- {}\n\n## Last {} events\n\n```json\n{}\n```\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        iso(now_ms),
        lines.len(),
        lines.join("\n")
    )
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The first line of the note, cut to a title's length.
pub fn title(text: &str) -> String {
    let first = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if first.chars().count() > 80 {
        first.chars().take(79).chain(['…']).collect()
    } else {
        first.into()
    }
}

/// A new-issue URL within `MAX_URL`; a long body is cut and says where the full note is.
pub fn issue_url(title: &str, body: &str, saved: &Path) -> String {
    let head = format!("{NEW_ISSUE}?title={}&body=", encode(title));
    let full = encode(body);
    if head.len() + full.len() <= MAX_URL {
        return head + &full;
    }
    let tail = encode(&format!(
        "\n\n… cut to fit a URL; the full note is {}",
        redact(&saved.display().to_string())
    ));
    let budget = MAX_URL.saturating_sub(head.len() + tail.len());
    let mut kept = String::new();
    for c in body.chars() {
        let e = encode(&c.to_string());
        if kept.len() + e.len() > budget {
            break;
        }
        kept += &e;
    }
    head + &kept + &tail
}

/// Writes the note to `dir/<time>.md` and returns its path.
pub fn save(dir: &Path, now_ms: u64, note: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let stamp: String = iso(now_ms).chars().filter(|c| *c != ':').collect();
    let path = dir.join(format!("{stamp}.md"));
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?
        .write_all(note.as_bytes())?;
    Ok(path)
}

/// `$BROWSER`, else the system's opener.
pub fn open(url: &str) -> io::Result<()> {
    let opener = std::env::var("BROWSER")
        .ok()
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| {
            if cfg!(target_os = "macos") {
                "open".into()
            } else {
                "xdg-open".into()
            }
        });
    let status = Command::new(&opener).arg(url).status()?;
    if !status.success() {
        return Err(io::Error::other(format!("{opener} failed ({status})")));
    }
    Ok(())
}

/// How a saved note is offered for posting.
#[derive(Debug, PartialEq, Eq)]
pub enum Post {
    /// In a terminal with gh signed in: show the issue and post it on a yes.
    Ask,
    /// Run by an agent or a script: print the issue, for it to offer posting with the user's OK.
    Print,
    /// In a terminal without gh: the user posts it from the prefilled page.
    Manual,
}

pub fn post(interactive: bool, gh_ready: bool) -> Post {
    match (interactive, gh_ready) {
        (false, _) => Post::Print,
        (true, true) => Post::Ask,
        (true, false) => Post::Manual,
    }
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

pub fn gh_argv(title: &str, body_file: &Path) -> Vec<String> {
    [
        "issue",
        "create",
        "--repo",
        REPO,
        "--title",
        title,
        "--body-file",
    ]
    .into_iter()
    .map(String::from)
    .chain([body_file.display().to_string()])
    .collect()
}

fn gh_ready() -> bool {
    Command::new("gh")
        .args(["auth", "status"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn yes() -> bool {
    eprint!("Post this issue to {REPO} with gh? [y/N] ");
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).is_ok()
        && matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn show(title: &str, note: &str) {
    println!("\nrepo: {REPO}\ntitle: {title}\nbody:\n{note}");
}

pub fn main(text: &str, with_paths: bool, open_url: bool) -> io::Result<i32> {
    if text.trim().is_empty() {
        return Err(io::Error::other("feedback needs some text"));
    }
    let now = crate::events::now_ms();
    let events = crate::events::read().unwrap_or_default();
    let note = note(text, now, &events, with_paths);
    let dir = crate::workers::Registry::default_dir()?.join("feedback");
    let saved = save(&dir, now, &note)?;
    println!("saved: {}", saved.display());
    let title = title(text);
    let url = issue_url(&title, &note, &saved);
    let interactive = unsafe { libc::isatty(0) == 1 && libc::isatty(2) == 1 };
    match post(interactive, interactive && gh_ready()) {
        Post::Ask => {
            show(&title, &note);
            if yes() {
                let status = Command::new("gh").args(gh_argv(&title, &saved)).status()?;
                if status.success() {
                    return Ok(0);
                }
                eprintln!("reeve: gh issue create failed ({status}); post it yourself instead");
            }
            println!("\nNot posted. To post it yourself:\n{url}");
        }
        Post::Print => {
            show(&title, &note);
            println!(
                "\nNothing is posted. Offer to post it, and only with the user's OK run\n  gh {}\n\
                 or use a GitHub MCP with the repo, title and body above. Or they post it at:\n{url}",
                gh_argv(&quote(&title), &saved).join(" ")
            );
        }
        Post::Manual => println!("\nTo post it as a GitHub issue:\n{url}"),
    }
    if open_url {
        open(&url)?;
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_times_are_utc() {
        assert_eq!(iso(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso(1_793_016_045_123), "2026-10-26T12:00:45.123Z");
        assert_eq!(iso(951_782_400_000), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn paths_are_redacted_wherever_they_start() {
        assert_eq!(
            redact(r#"{"label":"/Users/x/a b","worker":"w"}"#),
            r#"{"label":"<path> b","worker":"w"}"#
        );
        assert_eq!(
            redact("see ~/x/y, then (/tmp/z)"),
            "see <path>, then (<path>)"
        );
        assert_eq!(redact("a/b and 3/4 stay"), "a/b and 3/4 stay");
        assert_eq!(redact("https://x.dev/a"), "https://x.dev/a");
    }

    #[test]
    fn a_note_carries_the_version_os_and_only_the_last_twenty_events() {
        let events: Vec<Event> = (0..25)
            .map(|i| Event {
                command: format!("c{i}"),
                ..Event::default()
            })
            .collect();
        let n = note("it broke", 0, &events, false);
        assert!(n.contains("it broke"));
        assert!(n.contains(&format!("reeve {}", env!("CARGO_PKG_VERSION"))));
        assert!(n.contains("## Last 20 events"));
        assert!(!n.contains(r#""c4""#) && n.contains(r#""c5""#) && n.contains(r#""c24""#));
    }

    #[test]
    fn only_a_person_in_a_terminal_is_asked_and_agents_get_the_issue_printed() {
        assert_eq!(post(true, true), Post::Ask);
        assert_eq!(post(true, false), Post::Manual);
        assert_eq!(post(false, true), Post::Print);
        assert_eq!(post(false, false), Post::Print);
    }

    #[test]
    fn gh_gets_the_title_and_the_saved_note_as_its_body() {
        assert_eq!(
            gh_argv("it's slow", Path::new("/f/n.md")).join(" "),
            "issue create --repo leon-wbr/reeve --title it's slow --body-file /f/n.md"
        );
        assert_eq!(quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn titles_are_the_first_line_cut_short() {
        assert_eq!(title("ls hangs\nmore detail"), "ls hangs");
        assert_eq!(title("\n  ls hangs"), "ls hangs");
        let t = title(&"x".repeat(200));
        assert_eq!(t.chars().count(), 80);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn a_long_issue_body_is_cut_to_fit_the_url_and_says_so() {
        let saved = Path::new("/home/u/.local/state/reeve/feedback/n.md");
        let short = issue_url("t", "a b", saved);
        assert_eq!(short, format!("{NEW_ISSUE}?title=t&body=a%20b"));
        let long = issue_url("t", &"é ".repeat(5000), saved);
        assert!(long.len() <= MAX_URL, "{}", long.len());
        assert!(long.contains(&encode("cut to fit a URL")));
        assert!(!long.contains("home"));
    }
}
