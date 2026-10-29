//! `reeve setup claude`: a SessionStart hook that tells each new Claude Code session about
//! running workers, prompt and tool hooks that tell it when to hand off, a Bash hook that sends
//! `reeve heavy` to the background, a skill stub that says
//! what reeve offers and, with `--claude-md`, a block in CLAUDE.md. All are marked so that
//! `--check` and `--remove` find exactly what setup wrote.

use serde_json::{Value, json};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// A shell comment at the end of the hook command; Claude Code's settings schema has no place
/// for a marker of our own.
pub const MARKER: &str = "# reeve setup claude";
pub const MATCHER: &str = "startup|resume|clear|compact";
pub const DESCRIPTION: &str = "reeve is installed: test suites, e2e, builds and installs go \
through `reeve heavy`, in the background; named workers outlive this session; `reeve handoff` \
starts your successor. Read before heavy, long or parallel work, or a handoff.";
const STUB_LINE: &str = "Written by `reeve setup claude` for reeve ";

pub fn hook_command() -> String {
    format!("command -v reeve >/dev/null && reeve ls --brief || true {MARKER}")
}

pub fn context_command() -> String {
    format!("command -v reeve >/dev/null && reeve hook context || true {MARKER}")
}

pub fn background_command() -> String {
    format!("command -v reeve >/dev/null && reeve hook background || true {MARKER}")
}

/// Each hook event setup writes to, with the one group it adds there.
fn groups() -> [(&'static str, Value); 4] {
    let context = || json!({ "hooks": [{ "type": "command", "command": context_command() }] });
    [
        (
            "SessionStart",
            json!({
                "matcher": MATCHER,
                "hooks": [{ "type": "command", "command": hook_command() }]
            }),
        ),
        ("UserPromptSubmit", context()),
        ("PostToolUse", context()),
        (
            "PreToolUse",
            json!({
                "matcher": "Bash",
                "hooks": [{ "type": "command", "command": background_command() }]
            }),
        ),
    ]
}

fn ours(hook: &Value) -> bool {
    hook["command"].as_str().is_some_and(|c| c.contains(MARKER))
}

fn has_ours(group: &Value) -> bool {
    group["hooks"]
        .as_array()
        .is_some_and(|hs| hs.iter().any(ours))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Piece {
    Ok,
    Missing,
    /// Present but not what this reeve writes.
    Stale,
}

pub fn hook_state(settings: &Value) -> Piece {
    let found: Vec<Vec<&Value>> = groups()
        .iter()
        .map(|(event, _)| {
            settings["hooks"][event]
                .as_array()
                .map(|gs| gs.iter().filter(|g| has_ours(g)).collect())
                .unwrap_or_default()
        })
        .collect();
    if found.iter().all(Vec::is_empty) {
        return Piece::Missing;
    }
    let current = groups()
        .iter()
        .zip(&found)
        .all(|((_, want), got)| matches!(got[..], [g] if g == want));
    if current { Piece::Ok } else { Piece::Stale }
}

/// Takes out every hook carrying the marker, and any group, event list or `hooks` object left
/// empty by that. Returns whether anything changed.
pub fn remove_hook(settings: &mut Value) -> bool {
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else {
        return false;
    };
    let mut changed = false;
    for (event, _) in groups() {
        let Some(gs) = hooks.get_mut(event).and_then(Value::as_array_mut) else {
            continue;
        };
        let before = gs.len();
        let mut here = false;
        for g in gs.iter_mut().filter(|g| has_ours(g)) {
            if let Some(hs) = g["hooks"].as_array_mut() {
                hs.retain(|h| !ours(h));
                here = true;
            }
        }
        gs.retain(|g| g["hooks"].as_array().is_none_or(|hs| !hs.is_empty()));
        here |= gs.len() != before;
        if here && gs.is_empty() {
            hooks.remove(event);
        }
        changed |= here;
    }
    if changed && hooks.is_empty() {
        settings.as_object_mut().expect("an object").remove("hooks");
    }
    changed
}

/// Adds the hooks, replacing stale ones. Returns whether anything changed.
pub fn add_hook(settings: &mut Value) -> Result<bool, String> {
    if hook_state(settings) == Piece::Ok {
        return Ok(false);
    }
    let Some(top) = settings.as_object_mut() else {
        return Err("settings.json is not a JSON object".into());
    };
    let hooks = top.entry("hooks").or_insert_with(|| json!({}));
    if !hooks.is_object() {
        return Err("`hooks` in settings.json is not an object".into());
    }
    for (event, _) in groups() {
        if !hooks[event].is_array() && !hooks[event].is_null() {
            return Err(format!("`hooks.{event}` in settings.json is not a list"));
        }
    }
    remove_hook(settings);
    let hooks = settings
        .as_object_mut()
        .expect("an object")
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .expect("hooks is an object");
    for (event, group) in groups() {
        hooks
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .expect("a list")
            .push(group);
    }
    Ok(true)
}

/// When to use reeve, without syntax: `reeve --skill` prints the current manual, so nothing
/// here can go stale or teach a wrong flag.
pub const USES: &str = "reeve is installed on this machine. Use it for:\n\n\
- heavy commands: test suites, e2e, builds and installs;\n\
- long or parallel work, as named workers;\n\
- handing off to a successor when a \"Context check\" says so.\n\n\
Run `reeve --skill` before any of these; it prints the current manual with the exact commands.\n";

pub fn stub(version: &str) -> String {
    format!("---\nname: reeve\ndescription: {DESCRIPTION}\n---\n\n{USES}\n{STUB_LINE}{version}.\n")
}

/// The reeve version a stub was written for, if setup wrote it.
pub fn stub_version(text: &str) -> Option<&str> {
    text.lines()
        .find_map(|l| l.strip_prefix(STUB_LINE))
        .map(|v| v.trim_end_matches('.'))
}

pub fn stub_state(text: Option<&str>, version: &str) -> Piece {
    match text {
        None => Piece::Missing,
        Some(t) if t == stub(version) => Piece::Ok,
        Some(_) => Piece::Stale,
    }
}

pub const BLOCK_START: &str = "<!-- reeve setup claude: start -->";
pub const BLOCK_END: &str = "<!-- reeve setup claude: end -->";

pub fn block() -> String {
    format!("{BLOCK_START}\n## reeve\n\n{USES}{BLOCK_END}")
}

/// Where setup's block sits in CLAUDE.md, end marker included.
fn find_block(text: &str) -> Result<Option<std::ops::Range<usize>>, String> {
    let Some(start) = text.find(BLOCK_START) else {
        return Ok(None);
    };
    match text[start..].find(BLOCK_END) {
        Some(end) => Ok(Some(start..start + end + BLOCK_END.len())),
        None => Err(format!(
            "CLAUDE.md has `{BLOCK_START}` but no `{BLOCK_END}`"
        )),
    }
}

pub fn block_state(text: Option<&str>) -> Result<Piece, String> {
    let Some(t) = text else {
        return Ok(Piece::Missing);
    };
    Ok(match find_block(t)? {
        None => Piece::Missing,
        Some(r) if t[r.clone()] == block() => Piece::Ok,
        Some(_) => Piece::Stale,
    })
}

/// CLAUDE.md with the current block: in place of an old one, or at the end after a blank line.
pub fn add_block(text: &str) -> Result<String, String> {
    if let Some(r) = find_block(text)? {
        return Ok(format!("{}{}{}", &text[..r.start], block(), &text[r.end..]));
    }
    Ok(match text {
        "" => block() + "\n",
        t if t.ends_with('\n') => format!("{t}\n{}\n", block()),
        t => format!("{t}\n\n{}\n", block()),
    })
}

/// CLAUDE.md without the block and the blank line `add_block` put before it, or `None` when
/// there is no block.
pub fn remove_block(text: &str) -> Result<Option<String>, String> {
    let Some(r) = find_block(text)? else {
        return Ok(None);
    };
    let before = &text[..r.start];
    let after = &text[r.end..];
    let after = after.strip_prefix('\n').unwrap_or(after);
    let before = match before.strip_suffix('\n') {
        Some(b) if b.ends_with('\n') && after.is_empty() => b,
        _ => before,
    };
    Ok(Some(format!("{before}{after}")))
}

/// `~/…` for paths under the home dir.
pub fn tilde(p: &Path) -> String {
    let home = std::env::home_dir();
    let real = home.as_deref().and_then(|h| fs::canonicalize(h).ok());
    match [home, real]
        .into_iter()
        .flatten()
        .find_map(|h| p.strip_prefix(h).ok().map(Path::to_path_buf))
    {
        Some(rest) => format!("~/{}", rest.display()),
        None => p.display().to_string(),
    }
}

pub struct Paths {
    pub settings: PathBuf,
    pub skill: PathBuf,
    pub claude_md: PathBuf,
}

impl Paths {
    pub fn at(claude_dir: &Path) -> Paths {
        Paths {
            settings: claude_dir.join("settings.json"),
            skill: claude_dir.join("skills/reeve/SKILL.md"),
            claude_md: claude_dir.join("CLAUDE.md"),
        }
    }

    /// `$CLAUDE_CONFIG_DIR`, by default `~/.claude`, as Claude Code itself finds it.
    pub fn default_dir() -> io::Result<PathBuf> {
        std::env::var_os("CLAUDE_CONFIG_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| Some(std::env::home_dir()?.join(".claude")))
            .ok_or_else(|| io::Error::other("no home dir"))
    }
}

fn read_settings(path: &Path) -> io::Result<Option<Value>> {
    match fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).map(Some).map_err(|e| {
            io::Error::other(format!(
                "{} is not valid JSON ({e}); reeve left it alone",
                tilde(path)
            ))
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn read_text(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Backs up the old file, then replaces it by a rename so Claude Code never reads half of it.
/// A symlinked settings.json (dotfiles) is written through to its target.
fn write_settings(path: &Path, v: &Value, now: u64, out: &mut Vec<String>) -> io::Result<()> {
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if target.exists() {
        let backup = (1..)
            .map(|i| match i {
                1 => format!("settings.json.reeve-{now}.bak"),
                i => format!("settings.json.reeve-{now}-{i}.bak"),
            })
            .map(|n| target.with_file_name(n))
            .find(|b| !b.exists())
            .expect("a free backup name");
        fs::copy(&target, &backup)?;
        out.push(format!("backed up {} to {}", tilde(path), tilde(&backup)));
    }
    if let Some(dir) = target.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = target.with_file_name(format!("settings.json.reeve-{}", std::process::id()));
    fs::write(&tmp, serde_json::to_string_pretty(v)? + "\n")?;
    fs::rename(&tmp, &target)
}

/// Replaces the file by a rename, through a symlink to its target; an empty text removes it.
fn write_text(path: &Path, text: &str) -> io::Result<()> {
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if text.is_empty() {
        return fs::remove_file(&target);
    }
    if let Some(dir) = target.parent() {
        fs::create_dir_all(dir)?;
    }
    let name = target.file_name().expect("a file name").to_string_lossy();
    let tmp = target.with_file_name(format!("{name}.reeve-{}", std::process::id()));
    fs::write(&tmp, text)?;
    fs::rename(&tmp, &target)
}

fn md_err(p: &Paths) -> impl Fn(String) -> io::Error {
    let at = tilde(&p.claude_md);
    move |e| io::Error::other(format!("{e} ({at}); reeve left it alone"))
}

/// Writes the pieces and returns one line per file touched. The CLAUDE.md block is written when
/// asked for, and kept current once it is there.
pub fn install(p: &Paths, version: &str, claude_md: bool, now: u64) -> io::Result<Vec<String>> {
    let md = read_text(&p.claude_md)?;
    let md_piece = block_state(md.as_deref()).map_err(md_err(p))?;
    let mut out = vec![];
    let mut settings = read_settings(&p.settings)?.unwrap_or_else(|| json!({}));
    if add_hook(&mut settings).map_err(io::Error::other)? {
        write_settings(&p.settings, &settings, now, &mut out)?;
        out.push(format!(
            "wrote {}: hooks `reeve ls --brief` (session start), `reeve hook context` \
             (prompts and tool calls) and `reeve hook background` (Bash calls)",
            tilde(&p.settings)
        ));
    } else {
        out.push(format!("unchanged {}", tilde(&p.settings)));
    }
    if stub_state(read_text(&p.skill)?.as_deref(), version) == Piece::Ok {
        out.push(format!("unchanged {}", tilde(&p.skill)));
    } else {
        fs::create_dir_all(p.skill.parent().expect("skill dir"))?;
        fs::write(&p.skill, stub(version))?;
        out.push(format!("wrote {}", tilde(&p.skill)));
    }
    match md_piece {
        Piece::Ok => out.push(format!("unchanged {}", tilde(&p.claude_md))),
        Piece::Missing if !claude_md => {}
        _ => {
            let text = add_block(md.as_deref().unwrap_or("")).map_err(md_err(p))?;
            write_text(&p.claude_md, &text)?;
            out.push(format!("wrote {}: the reeve block", tilde(&p.claude_md)));
        }
    }
    Ok(out)
}

/// Takes out what setup wrote, and nothing else: a SKILL.md that setup didn't write stays.
pub fn remove(p: &Paths, now: u64) -> io::Result<Vec<String>> {
    let mut out = vec![];
    if let Some(mut settings) = read_settings(&p.settings)?
        && remove_hook(&mut settings)
    {
        write_settings(&p.settings, &settings, now, &mut out)?;
        out.push(format!("removed the hooks from {}", tilde(&p.settings)));
    }
    match read_text(&p.skill)? {
        Some(t) if stub_version(&t).is_some() => {
            fs::remove_file(&p.skill)?;
            out.push(format!("removed {}", tilde(&p.skill)));
            let dir = p.skill.parent().expect("skill dir");
            if fs::read_dir(dir)?.next().is_none() {
                fs::remove_dir(dir)?;
                out.push(format!("removed {}", tilde(dir)));
            }
        }
        Some(_) => out.push(format!(
            "left {} alone: reeve setup didn't write it",
            tilde(&p.skill)
        )),
        None => {}
    }
    if let Some(md) = read_text(&p.claude_md)?
        && let Some(text) = remove_block(&md).map_err(md_err(p))?
    {
        write_text(&p.claude_md, &text)?;
        out.push(format!(
            "removed the reeve block from {}",
            tilde(&p.claude_md)
        ));
    }
    if out.is_empty() {
        out.push("nothing to remove".into());
    }
    Ok(out)
}

/// Each piece's state with where it lives: the hooks, the stub and, when asked for or present,
/// the CLAUDE.md block.
pub fn check(p: &Paths, version: &str, claude_md: bool) -> io::Result<Vec<(String, Piece)>> {
    let settings = read_settings(&p.settings)?.unwrap_or_else(|| json!({}));
    let stub = read_text(&p.skill)?;
    let mut out = vec![
        (
            format!("hooks in {}", tilde(&p.settings)),
            hook_state(&settings),
        ),
        (tilde(&p.skill), stub_state(stub.as_deref(), version)),
    ];
    let md = block_state(read_text(&p.claude_md)?.as_deref()).map_err(md_err(p))?;
    if claude_md || md != Piece::Missing {
        out.push((format!("reeve block in {}", tilde(&p.claude_md)), md));
    }
    Ok(out)
}

pub fn render_check(pieces: &[(String, Piece)], claude_md: bool) -> String {
    let setup = if claude_md {
        "`reeve setup claude --claude-md`"
    } else {
        "`reeve setup claude`"
    };
    pieces
        .iter()
        .map(|(what, piece)| match piece {
            Piece::Ok => format!("ok       {what}"),
            Piece::Missing => format!("missing  {what}; {setup} adds it"),
            Piece::Stale => format!("stale    {what}; {setup} rewrites it"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("reeve-setup-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn others() -> Value {
        json!({
            "model": "opus",
            "hooks": {
                "SessionStart": [{
                    "matcher": "startup",
                    "hooks": [{ "type": "command", "command": "echo hi", "timeout": 10 }]
                }],
                "Stop": [{ "hooks": [{ "type": "command", "command": "true" }] }]
            },
            "env": { "A": "1" }
        })
    }

    #[test]
    fn adding_keeps_every_other_key_in_order_and_is_idempotent() {
        let mut s = others();
        assert_eq!(hook_state(&s), Piece::Missing);
        assert_eq!(add_hook(&mut s), Ok(true));
        assert_eq!(hook_state(&s), Piece::Ok);
        let once = s.clone();
        assert_eq!(add_hook(&mut s), Ok(false));
        assert_eq!(s, once);
        let keys: Vec<_> = s.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["model", "hooks", "env"]);
        let starts = s["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(starts.len(), 2);
        assert_eq!(starts[0], others()["hooks"]["SessionStart"][0]);
        assert_eq!(starts[1]["matcher"], MATCHER);
        assert_eq!(s["hooks"]["Stop"], others()["hooks"]["Stop"]);
    }

    #[test]
    fn removing_restores_the_settings_exactly() {
        for before in [others(), json!({}), json!({"hooks": {"Stop": []}})] {
            let mut s = before.clone();
            add_hook(&mut s).unwrap();
            assert!(remove_hook(&mut s));
            assert_eq!(s, before);
            assert!(!remove_hook(&mut s));
        }
    }

    #[test]
    fn a_changed_hook_is_stale_and_gets_replaced() {
        let mut s = others();
        add_hook(&mut s).unwrap();
        s["hooks"]["SessionStart"][1]["hooks"][0]["command"] = json!(format!("reeve ls {MARKER}"));
        assert_eq!(hook_state(&s), Piece::Stale);
        assert_eq!(add_hook(&mut s), Ok(true));
        assert_eq!(hook_state(&s), Piece::Ok);
        assert_eq!(s["hooks"]["SessionStart"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_marked_hook_shares_its_group_with_others_only_as_stale() {
        let mut s = others();
        s["hooks"]["SessionStart"][0]["hooks"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "type": "command", "command": hook_command() }));
        assert_eq!(hook_state(&s), Piece::Stale);
        assert!(remove_hook(&mut s));
        assert_eq!(s, others());
    }

    #[test]
    fn settings_of_the_wrong_shape_are_refused() {
        assert!(add_hook(&mut json!([])).is_err());
        assert!(add_hook(&mut json!({"hooks": []})).is_err());
        assert!(add_hook(&mut json!({"hooks": {"SessionStart": {}}})).is_err());
    }

    #[test]
    fn the_hook_runs_the_brief_listing_and_never_fails() {
        let c = hook_command();
        assert!(c.starts_with("command -v reeve >/dev/null && reeve ls --brief || true"));
        assert!(c.ends_with(MARKER));
    }

    #[test]
    fn the_stub_is_short_and_carries_its_version() {
        let s = stub("1.2.3");
        assert!(s.starts_with("---\nname: reeve\ndescription: reeve is installed"));
        assert!(s.contains(USES));
        assert!(s.len() / 4 <= 160, "{}", s.len() / 4);
        assert_eq!(stub_version(&s), Some("1.2.3"));
        assert_eq!(stub_version("---\nname: reeve\n---\nmine\n"), None);
        assert!(DESCRIPTION.len() / 4 <= 60, "{}", DESCRIPTION.len() / 4);
        assert_eq!(stub_state(Some(&s), "1.2.3"), Piece::Ok);
        assert_eq!(stub_state(Some(&s), "1.2.4"), Piece::Stale);
        assert_eq!(stub_state(None, "1.2.3"), Piece::Missing);
    }

    #[test]
    fn install_check_and_remove_round_trip_on_disk() {
        let dir = temp("disk");
        let p = Paths::at(&dir);
        let original = serde_json::to_string_pretty(&others()).unwrap() + "\n";
        fs::write(&p.settings, &original).unwrap();
        let pieces = |p: &Paths, v| -> Vec<Piece> {
            check(p, v, false)
                .unwrap()
                .into_iter()
                .map(|(_, piece)| piece)
                .collect()
        };
        assert_eq!(pieces(&p, "1.0.0"), [Piece::Missing, Piece::Missing]);

        let out = install(&p, "1.0.0", false, 7).unwrap();
        assert!(out[0].starts_with("backed up"), "{out:?}");
        assert_eq!(
            fs::read_to_string(dir.join("settings.json.reeve-7.bak")).unwrap(),
            original
        );
        assert_eq!(out.len(), 3, "{out:?}");
        assert_eq!(pieces(&p, "1.0.0"), [Piece::Ok, Piece::Ok]);
        assert_eq!(pieces(&p, "1.0.1"), [Piece::Ok, Piece::Stale]);
        let again = install(&p, "1.0.0", false, 8).unwrap();
        assert!(
            again.iter().all(|l| l.starts_with("unchanged")),
            "{again:?}"
        );
        assert!(!dir.join("settings.json.reeve-8.bak").exists());
        assert!(!p.claude_md.exists());

        remove(&p, 7).unwrap();
        assert!(dir.join("settings.json.reeve-7-2.bak").exists());
        assert_eq!(fs::read_to_string(&p.settings).unwrap(), original);
        assert!(!dir.join("skills/reeve").exists());
        assert!(dir.join("skills").exists());
        assert_eq!(remove(&p, 10).unwrap(), ["nothing to remove"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn install_creates_missing_settings_and_remove_spares_a_foreign_skill() {
        let dir = temp("fresh");
        let p = Paths::at(&dir);
        let out = install(&p, "1.0.0", false, 1).unwrap();
        assert!(out.iter().all(|l| !l.starts_with("backed up")), "{out:?}");
        assert_eq!(
            hook_state(&read_settings(&p.settings).unwrap().unwrap()),
            Piece::Ok
        );
        fs::write(&p.skill, "---\nname: reeve\n---\nmy own\n").unwrap();
        let out = remove(&p, 2).unwrap();
        assert!(out.iter().any(|l| l.starts_with("left")), "{out:?}");
        assert!(p.skill.exists());
        assert_eq!(fs::read_to_string(&p.settings).unwrap(), "{}\n");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn invalid_settings_are_left_alone() {
        let dir = temp("bad");
        let p = Paths::at(&dir);
        fs::write(&p.settings, "{ nope").unwrap();
        let e = install(&p, "1.0.0", false, 1).unwrap_err().to_string();
        assert!(e.contains("not valid JSON"), "{e}");
        assert_eq!(fs::read_to_string(&p.settings).unwrap(), "{ nope");
        assert!(!p.skill.exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_context_hook_runs_on_prompts_and_tool_calls_and_never_fails() {
        let mut s = others();
        add_hook(&mut s).unwrap();
        for event in ["UserPromptSubmit", "PostToolUse"] {
            let gs = s["hooks"][event].as_array().unwrap();
            assert_eq!(gs.len(), 1, "{event}");
            assert_eq!(gs[0]["hooks"][0]["command"], context_command());
            assert!(gs[0].get("matcher").is_none(), "{event}");
        }
        let c = context_command();
        assert!(c.starts_with("command -v reeve >/dev/null && reeve hook context || true"));
        assert!(c.ends_with(MARKER));
    }

    #[test]
    fn the_background_hook_runs_before_bash_calls_and_never_fails() {
        let mut s = others();
        add_hook(&mut s).unwrap();
        let gs = s["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(gs.len(), 1);
        assert_eq!(gs[0]["matcher"], "Bash");
        assert_eq!(gs[0]["hooks"][0]["command"], background_command());
        let c = background_command();
        assert!(c.starts_with("command -v reeve >/dev/null && reeve hook background || true"));
        assert!(c.ends_with(MARKER));
        s["hooks"].as_object_mut().unwrap().remove("PreToolUse");
        assert_eq!(hook_state(&s), Piece::Stale);
    }

    #[test]
    fn a_setup_with_only_the_session_start_hook_is_stale() {
        let mut s = others();
        add_hook(&mut s).unwrap();
        s["hooks"].as_object_mut().unwrap().remove("PostToolUse");
        assert_eq!(hook_state(&s), Piece::Stale);
        assert_eq!(add_hook(&mut s), Ok(true));
        assert_eq!(hook_state(&s), Piece::Ok);
    }

    #[test]
    fn removing_leaves_a_users_own_empty_event_list() {
        let before = json!({"hooks": {"PostToolUse": [], "Stop": []}});
        let mut s = before.clone();
        add_hook(&mut s).unwrap();
        assert!(remove_hook(&mut s));
        assert_eq!(s, json!({"hooks": {"Stop": []}}));
        let mut s = json!({"hooks": {"PostToolUse": []}});
        let mut with = s.clone();
        with["hooks"]["SessionStart"] = json!([{ "hooks": [{ "command": hook_command() }] }]);
        assert!(remove_hook(&mut with));
        assert_eq!(with, s);
        assert!(!remove_hook(&mut s));
    }

    #[test]
    fn the_block_is_added_once_and_removed_exactly() {
        for before in ["", "# Mine\n\nkeep this\n", "no newline at the end"] {
            let with = add_block(before).unwrap();
            assert_eq!(block_state(Some(&with)), Ok(Piece::Ok), "{before:?}");
            assert!(with.starts_with(before), "{with:?}");
            assert_eq!(add_block(&with).unwrap(), with);
            let back = remove_block(&with).unwrap().unwrap();
            if before.ends_with('\n') || before.is_empty() {
                assert_eq!(back, before);
            } else {
                assert_eq!(back, format!("{before}\n"));
            }
            assert_eq!(remove_block(&back).unwrap(), None);
        }
    }

    #[test]
    fn a_stale_block_is_rewritten_in_place_and_text_around_it_stays() {
        let old = format!("top\n\n{BLOCK_START}\nold words\n{BLOCK_END}\n\nbottom\n");
        assert_eq!(block_state(Some(&old)), Ok(Piece::Stale));
        let new = add_block(&old).unwrap();
        assert_eq!(new, format!("top\n\n{}\n\nbottom\n", block()));
        assert_eq!(remove_block(&new).unwrap().unwrap(), "top\n\n\nbottom\n");
        let broken = format!("top\n{BLOCK_START}\nno end\n");
        assert!(block_state(Some(&broken)).is_err());
        assert!(add_block(&broken).is_err());
        assert!(remove_block(&broken).is_err());
    }

    #[test]
    fn the_block_names_the_uses_and_points_at_the_manual() {
        let b = block();
        for part in [
            "reeve is installed",
            "test suites, e2e, builds and installs",
            "named workers",
            "Context check",
            "`reeve --skill`",
        ] {
            assert!(b.contains(part), "{part}");
        }
        assert!(b.len() / 4 <= 100, "{}", b.len() / 4);
    }

    #[test]
    fn the_uses_carry_no_syntax_flags_or_thresholds() {
        let rest = USES.replace("`reeve --skill`", "").replace("e2e", "");
        assert!(!rest.contains('`') && !rest.contains("--"), "{rest}");
        assert!(!rest.chars().any(|c| c.is_ascii_digit()), "{rest}");
    }

    #[test]
    fn claude_md_is_written_only_when_asked_and_kept_current_after() {
        let dir = temp("md");
        let p = Paths::at(&dir);
        fs::write(&p.claude_md, "# Mine\n").unwrap();
        install(&p, "1.0.0", false, 1).unwrap();
        assert_eq!(fs::read_to_string(&p.claude_md).unwrap(), "# Mine\n");
        assert_eq!(check(&p, "1.0.0", false).unwrap().len(), 2);
        let asked = check(&p, "1.0.0", true).unwrap();
        assert_eq!(asked[2].1, Piece::Missing);
        assert!(render_check(&asked, true).contains("`reeve setup claude --claude-md` adds it"));

        install(&p, "1.0.0", true, 2).unwrap();
        let md = fs::read_to_string(&p.claude_md).unwrap();
        assert_eq!(md, format!("# Mine\n\n{}\n", block()));
        assert_eq!(check(&p, "1.0.0", false).unwrap()[2].1, Piece::Ok);
        fs::write(&p.claude_md, md.replace("reeve is installed", "old")).unwrap();
        assert_eq!(check(&p, "1.0.0", false).unwrap()[2].1, Piece::Stale);
        install(&p, "1.0.0", false, 3).unwrap();
        assert_eq!(fs::read_to_string(&p.claude_md).unwrap(), md);

        let out = remove(&p, 4).unwrap();
        assert!(out.iter().any(|l| l.contains("reeve block")), "{out:?}");
        assert_eq!(fs::read_to_string(&p.claude_md).unwrap(), "# Mine\n");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_claude_md_with_a_broken_block_is_left_alone() {
        let dir = temp("mdbad");
        let p = Paths::at(&dir);
        let broken = format!("{BLOCK_START}\nmine now\n");
        fs::write(&p.claude_md, &broken).unwrap();
        let e = install(&p, "1.0.0", true, 1).unwrap_err().to_string();
        assert!(e.contains("left it alone"), "{e}");
        assert!(!p.settings.exists());
        assert!(remove(&p, 2).is_err());
        assert_eq!(fs::read_to_string(&p.claude_md).unwrap(), broken);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_claude_md_stays_a_symlink() {
        let dir = temp("mdlink");
        let p = Paths::at(&dir);
        let real = dir.join("dotfiles.md");
        fs::write(&real, "# Mine\n").unwrap();
        std::os::unix::fs::symlink(&real, &p.claude_md).unwrap();
        install(&p, "1.0.0", true, 1).unwrap();
        assert!(fs::symlink_metadata(&p.claude_md).unwrap().is_symlink());
        assert!(fs::read_to_string(&real).unwrap().contains(BLOCK_START));
        remove(&p, 2).unwrap();
        assert_eq!(fs::read_to_string(&real).unwrap(), "# Mine\n");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_settings_file_stays_a_symlink() {
        let dir = temp("link");
        let p = Paths::at(&dir);
        let real = dir.join("dotfiles.json");
        fs::write(&real, "{}\n").unwrap();
        std::os::unix::fs::symlink(&real, &p.settings).unwrap();
        install(&p, "1.0.0", false, 1).unwrap();
        assert!(fs::symlink_metadata(&p.settings).unwrap().is_symlink());
        assert!(fs::read_to_string(&real).unwrap().contains(MARKER));
        fs::remove_dir_all(&dir).unwrap();
    }
}
