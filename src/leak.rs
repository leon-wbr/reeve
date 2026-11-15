//! The last check before text leaves this machine for a public repo: issue bodies, comments, PR
//! titles and bodies, commit messages and diffs. A hit blocks the post; it names what matched and
//! where, never the matched text.

use crate::autoreport::Own;
use std::collections::BTreeSet;
use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub struct Hit {
    pub kind: &'static str,
    pub line: usize,
}

pub struct Scanner {
    own: Own,
    /// Worker names, worker directories and project names under the user's work dirs.
    terms: BTreeSet<String>,
    paths: BTreeSet<String>,
    repo: String,
}

const TOKEN_PREFIXES: [&str; 9] = [
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "sk-",
    "xoxb-",
    "AKIA",
];

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether `term` appears in `text` as a whole word, so `ann` doesn't match `planning`.
fn has_word(text: &str, term: &str) -> bool {
    let mut from = 0;
    while let Some(i) = text[from..].find(term) {
        let (s, e) = (from + i, from + i + term.len());
        let before = text[..s].chars().next_back().is_none_or(|c| !is_word(c));
        let after = text[e..].chars().next().is_none_or(|c| !is_word(c));
        if before && after {
            return true;
        }
        from = s + term.len().max(1);
    }
    false
}

fn entropy(s: &str) -> f64 {
    let mut counts = std::collections::HashMap::new();
    for c in s.chars() {
        *counts.entry(c).or_insert(0usize) += 1;
    }
    let n = s.chars().count() as f64;
    counts
        .values()
        .map(|&c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum()
}

fn looks_like_ip(tok: &str) -> bool {
    let parts: Vec<&str> = tok.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 3 && p.parse::<u8>().is_ok())
}

impl Scanner {
    pub fn new(own: Own, names: &[&str], paths: &[&str], repo: &str) -> Scanner {
        Scanner {
            own,
            terms: names
                .iter()
                .filter(|n| n.len() >= 3 && !n.starts_with("autofix-"))
                .map(|n| n.to_string())
                .collect(),
            paths: paths.iter().map(|p| p.to_string()).collect(),
            repo: repo.to_string(),
        }
    }

    /// This machine's user, host, workers and work dirs; `repo` is `owner/name` of the repository
    /// the text is for, whose own name and URLs are fine.
    pub fn here(repo: &str) -> Scanner {
        let name = repo.rsplit('/').next().unwrap_or("");
        let mut names: Vec<String> = vec![];
        let mut paths: Vec<String> = vec![];
        if let Ok(dir) = crate::workers::Registry::default_dir()
            && let Ok(all) = crate::workers::Registry::at(dir).all()
        {
            for w in all {
                names.push(w.name);
                paths.push(w.cwd.display().to_string());
            }
        }
        let own = Own::here();
        if let Some(home) = &own.home
            && let Ok(rd) = std::fs::read_dir(Path::new(home).join("work"))
        {
            names.extend(
                rd.flatten()
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|n| !n.starts_with('.')),
            );
        }
        let names: Vec<&str> = names
            .iter()
            .map(String::as_str)
            .filter(|n| *n != name)
            .collect();
        let paths: Vec<&str> = paths
            .iter()
            .map(String::as_str)
            .filter(|p| p.len() > 1)
            .collect();
        Scanner::new(own, &names, &paths, repo)
    }

    fn line_hits(&self, line: &str) -> Vec<&'static str> {
        let mut hits = vec![];
        let slug = self.repo.to_ascii_lowercase();
        let mut stripped = line.to_string();
        let mut urls = vec![];
        for tok in line.split(|c: char| c.is_whitespace() || "()<>\"'`[]".contains(c)) {
            if tok.contains("://") {
                urls.push(tok.to_string());
            }
        }
        for u in &urls {
            let ok = u.split_once("://").is_some_and(|(_, rest)| {
                rest.to_ascii_lowercase()
                    .starts_with(&format!("github.com/{slug}"))
            });
            if ok {
                stripped = stripped.replace(u.as_str(), "");
            } else {
                hits.push("url");
            }
        }
        stripped = stripped.replace(&self.repo, "");
        for (v, kind) in [
            (&self.own.home, "home-dir"),
            (&self.own.user, "username"),
            (&self.own.host, "hostname"),
        ] {
            if let Some(v) = v
                && (if kind == "home-dir" {
                    stripped.contains(v.as_str())
                } else {
                    has_word(&stripped, v)
                })
            {
                hits.push(kind);
            }
        }
        for tok in stripped.split(|c: char| c.is_whitespace() || "()<>\"'`[],;:".contains(c)) {
            let tok = tok.trim_matches(|c: char| ".!?".contains(c));
            if tok.contains('@')
                && tok
                    .split_once('@')
                    .is_some_and(|(a, d)| !a.is_empty() && d.contains('.'))
            {
                hits.push("email");
            }
            if looks_like_ip(tok) {
                hits.push("ip-address");
            }
            if TOKEN_PREFIXES
                .iter()
                .any(|p| tok.starts_with(p) && tok.len() >= p.len() + 8)
            {
                hits.push("token");
            } else if tok.len() >= 32
                && tok
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+/_-=".contains(c))
                && tok.chars().any(|c| c.is_ascii_digit())
                && tok.chars().any(|c| c.is_ascii_alphabetic())
                && !tok.chars().all(|c| c.is_ascii_hexdigit())
                && entropy(tok) > 4.0
            {
                hits.push("high-entropy-string");
            }
        }
        if self.terms.iter().any(|t| has_word(&stripped, t)) {
            hits.push("worker-or-project-name");
        }
        if self.paths.iter().any(|p| stripped.contains(p.as_str())) {
            hits.push("worker-directory");
        }
        hits.dedup();
        hits
    }

    pub fn scan(&self, text: &str) -> Vec<Hit> {
        text.lines()
            .enumerate()
            .flat_map(|(i, l)| {
                self.line_hits(l)
                    .into_iter()
                    .map(move |kind| Hit { kind, line: i + 1 })
            })
            .collect()
    }

    /// Only the lines a diff adds count; what a change keeps or removes was already public.
    pub fn scan_diff(&self, diff: &str) -> Vec<Hit> {
        diff.lines()
            .enumerate()
            .filter(|(_, l)| l.starts_with('+') && !l.starts_with("+++"))
            .flat_map(|(i, l)| {
                self.line_hits(&l[1..])
                    .into_iter()
                    .map(move |kind| Hit { kind, line: i + 1 })
            })
            .collect()
    }
}

pub fn render(hits: &[Hit]) -> String {
    hits.iter()
        .map(|h| format!("line {}: {}", h.line, h.kind))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scanner() -> Scanner {
        let own = Own {
            user: Some("jdoe".into()),
            host: Some("jdoes-mbp".into()),
            home: Some("/Users/jdoe".into()),
        };
        Scanner::new(
            own,
            &["acme-checkout", "night-shift", "autofix-7"],
            &["/Users/jdoe/work/acme-checkout"],
            "owner/reeve",
        )
    }

    fn kinds(text: &str) -> Vec<&'static str> {
        scanner().scan(text).into_iter().map(|h| h.kind).collect()
    }

    #[test]
    fn clean_text_and_this_repos_own_urls_pass() {
        assert!(kinds("Fixes #4: the slot count was off by one in src/heavy.rs:12").is_empty());
        assert!(kinds("see https://github.com/owner/reeve/issues/3 and owner/reeve").is_empty());
        assert!(
            kinds("reeve 0.1.0 (271c42c 2026-10-31) macos, 4 retries, version 1.2.3").is_empty()
        );
        assert!(kinds("a 40 char sha 5721b8a3c0ffee5721b8a3c0ffee5721b8a3c0ff").is_empty());
    }

    #[test]
    fn the_home_dir_and_username_block() {
        assert_eq!(kinds("failed at /Users/jdoe/x"), ["home-dir", "username"]);
        assert_eq!(kinds("pushed by jdoe"), ["username"]);
        assert!(kinds("jdoes are fine, jdoeX too").is_empty());
    }

    #[test]
    fn hostnames_emails_and_ips_block() {
        assert_eq!(kinds("on jdoes-mbp"), ["hostname"]);
        assert_eq!(kinds("mail someone@example.org please"), ["email"]);
        assert_eq!(kinds("connect to 192.168.1.20 failed"), ["ip-address"]);
        assert!(kinds("a version 0.1.0.9999 and 1.2.3").is_empty());
    }

    #[test]
    fn worker_names_directories_and_project_names_block() {
        assert_eq!(kinds("worker night-shift lost"), ["worker-or-project-name"]);
        assert_eq!(kinds("the acme-checkout repo"), ["worker-or-project-name"]);
        assert_eq!(
            kinds("cwd /Users/jdoe/work/acme-checkout"),
            [
                "home-dir",
                "username",
                "worker-or-project-name",
                "worker-directory"
            ]
        );
        assert!(kinds("autofix-7 is ours and night shifts are words").is_empty());
    }

    #[test]
    fn tokens_and_keys_block_including_unprefixed_high_entropy_ones() {
        for prefix in TOKEN_PREFIXES {
            assert_eq!(kinds(&format!("key {prefix}{}", "0a1B2c3D4e5F")), ["token"]);
        }
        let random = ["Zx9Qv3Lk8Rt2", "Wn7Bm4Hc6Jd1", "Fg5Ys0Pa8Ue"].concat();
        assert_eq!(kinds(&format!("secret={random}")), ["high-entropy-string"]);
    }

    #[test]
    fn urls_other_than_this_repo_block() {
        assert_eq!(kinds("see https://example.com/a"), ["url"]);
        assert_eq!(kinds("see https://github.com/other/thing"), ["url"]);
        assert_eq!(kinds("ssh://git@host.corp/x"), ["url", "email"]);
    }

    #[test]
    fn a_real_looking_leaked_transcript_line_is_blocked() {
        let leak = format!(
            "worker acme-checkout at /Users/jdoe/work/acme-checkout/.claude/worktrees/x said \"mail jdoe@acme.example the token {}AbCdEf0123456789xyzXYZ\"",
            "gh".to_owned() + "p_"
        );
        let k = kinds(&leak);
        for want in [
            "home-dir",
            "username",
            "worker-or-project-name",
            "worker-directory",
            "email",
            "token",
        ] {
            assert!(k.contains(&want), "{want} missing from {k:?}");
        }
    }

    #[test]
    fn a_diff_is_scanned_on_its_added_lines_only() {
        let diff = "--- a/x\n+++ b/x\n-old jdoe line\n context jdoe\n+new line\n+leak jdoe here\n";
        assert_eq!(
            scanner().scan_diff(diff),
            [Hit {
                kind: "username",
                line: 6
            }]
        );
    }
}
