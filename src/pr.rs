//! `reeve pr NAME`: opens the PR a worker couldn't. A worker denied `gh pr create` writes the PR
//! next to its report, as `NAME.pr.md`, and whoever may publish opens it with one command.
//!
//! The file starts with a front matter of `gh pr create`'s own flags, then the body:
//!
//! ```text
//! ---
//! title: fix: one line
//! head: fix/branch
//! base: main
//! repo: owner/name
//! draft: true
//! ---
//! The body, in Markdown.
//! ```
//!
//! Values are one-line YAML scalars, so a title may also be quoted.

use crate::workers::Registry;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Where a worker whose report is `report` puts its PR.
pub fn path(report: &Path) -> PathBuf {
    report.with_extension("pr.md")
}

/// `gh pr create`'s arguments from a PR file, the body read from stdin; or what is wrong with it.
pub fn gh_args(text: &str) -> Result<(Vec<String>, String), String> {
    let rest = text
        .strip_prefix("---\n")
        .ok_or("it doesn't start with a `---` front matter line")?;
    let (head, body) = rest
        .split_once("\n---\n")
        .ok_or("its front matter has no closing `---` line")?;
    let mut args: Vec<String> = ["pr", "create"].map(String::from).into();
    let (mut title, mut branch) = (false, false);
    for line in head.lines().filter(|l| !l.trim().is_empty()) {
        let (key, value) = line
            .split_once(':')
            .map(|(k, v)| (k.trim(), v.trim()))
            .ok_or_else(|| format!("`{line}` is no `key: value` line"))?;
        let value = &scalar(value).map_err(|e| format!("`{line}`: {e}"))?;
        match key {
            "draft" if value == "true" => args.push("--draft".into()),
            "draft" if value == "false" => {}
            "title" | "head" | "base" | "repo" if !value.is_empty() => {
                title |= key == "title";
                branch |= key == "head";
                args.extend([format!("--{key}"), value.into()]);
            }
            _ => {
                return Err(format!(
                    "`{line}`: reeve knows title, head, base, repo and draft"
                ));
            }
        }
    }
    if !title || !branch {
        return Err("it needs a title and a head branch".into());
    }
    args.extend(["--body-file", "-"].map(String::from));
    Ok((args, body.trim().to_string() + "\n"))
}

/// A one-line YAML scalar: 'single-quoted' (`''` is a quote), "double-quoted" (backslash
/// escapes) or plain, taken as it stands.
fn scalar(value: &str) -> Result<String, String> {
    let mut chars = value.chars();
    let quote = match chars.next() {
        Some(q @ ('\'' | '"')) => q,
        _ => return Ok(value.into()),
    };
    let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            '\'' if quote == '\'' && chars.clone().next() == Some('\'') => {
                chars.next();
                out.push('\'');
            }
            '\\' if quote == '"' => out.push(match chars.next() {
                Some('\\') => '\\',
                Some('"') => '"',
                Some('/') => '/',
                Some('n') => '\n',
                Some('t') => '\t',
                Some('r') => '\r',
                Some('0') => '\0',
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    u32::from_str_radix(&hex, 16)
                        .ok()
                        .and_then(char::from_u32)
                        .ok_or(format!("`\\u{hex}` is no escape"))?
                }
                e => return Err(format!("`\\{}` is no escape", e.unwrap_or(' '))),
            }),
            c if c == quote => {
                return match chars.as_str().trim() {
                    "" => Ok(out),
                    _ => Err("text after its closing quote".into()),
                };
            }
            c => out.push(c),
        }
    }
    Err(format!("unclosed {quote}"))
}

/// Opens the PR a worker saved, with `gh` in the worker's directory while it is still there.
pub fn open(r: &Registry, name: &str) -> io::Result<i32> {
    let w = r.get(name)?;
    let file = path(&w.report);
    let text = std::fs::read_to_string(&file).map_err(|e| {
        io::Error::other(format!(
            "{name} saved no PR ({}: {e})",
            crate::setup::tilde(&file)
        ))
    })?;
    let (args, body) = gh_args(&text)
        .map_err(|e| io::Error::other(format!("{}: {e}", crate::setup::tilde(&file))))?;
    let mut gh = Command::new("gh");
    gh.args(&args).stdin(Stdio::piped());
    if w.cwd.is_dir() {
        gh.current_dir(&w.cwd);
    }
    let mut child = gh.spawn()?;
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(body.as_bytes())?;
    Ok(child.wait()?.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pr_file_is_gh_s_own_flags_then_the_body() {
        let (args, body) = gh_args(
            "---\ntitle: fix: a: b\nhead: fix/x\nbase: main\nrepo: o/r\ndraft: true\n---\n\nBody `with` $(no shell).\n",
        )
        .unwrap();
        assert_eq!(
            args.join(" "),
            "pr create --title fix: a: b --head fix/x --base main --repo o/r --draft --body-file -"
        );
        assert_eq!(body, "Body `with` $(no shell).\n");
    }

    #[test]
    fn front_matter_values_are_yaml_scalars() {
        for (value, title) in [
            (
                r#"'feat(desktop): "Your presence" opens a popover'"#,
                r#"feat(desktop): "Your presence" opens a popover"#,
            ),
            ("'it''s done'", "it's done"),
            (r#""fix: it doesn't …""#, "fix: it doesn't …"),
            (r#""a \"b\" \\ c\td""#, "a \"b\" \\ c\td"),
            ("fix: plain: with colons", "fix: plain: with colons"),
            ("it's \"plain\"", "it's \"plain\""),
        ] {
            let (args, _) = gh_args(&format!("---\ntitle: {value}\nhead: 'h'\n---\nb")).unwrap();
            assert_eq!(args[3], title, "{value}");
            assert_eq!(args[5], "h");
        }
    }

    #[test]
    fn a_pr_file_without_a_title_head_or_front_matter_is_refused() {
        for (text, why) in [
            ("title: t\n", "front matter"),
            ("---\ntitle: t\nhead: h\n", "closing"),
            ("---\nhead: h\n---\nb", "a title and a head"),
            ("---\ntitle: t\n---\nb", "a title and a head"),
            ("---\ntitle: t\nhead: h\nreviewer: x\n---\nb", "reeve knows"),
            ("---\ntitle: t\nhead: h\ndraft: yes\n---\nb", "reeve knows"),
            ("---\ntitle: 'open\nhead: h\n---\nb", "unclosed"),
            (
                "---\ntitle: \"a\" b\nhead: h\n---\nb",
                "after its closing quote",
            ),
            ("---\ntitle: \"\\q\"\nhead: h\n---\nb", "escape"),
            ("---\ntitle: ''\nhead: h\n---\nb", "reeve knows"),
        ] {
            let err = gh_args(text).unwrap_err();
            assert!(err.contains(why), "{text:?}: {err}");
        }
    }

    #[test]
    fn the_pr_file_sits_next_to_the_report() {
        assert_eq!(
            path(Path::new("/st/reports/w.md")),
            Path::new("/st/reports/w.pr.md")
        );
    }
}
