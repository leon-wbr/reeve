//! `reeve setup claude`: a SessionStart hook that tells each new Claude Code session about
//! running workers, and a skill stub that says what reeve offers. Both are marked so that
//! `--check` and `--remove` find exactly what setup wrote.

use serde_json::{Value, json};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// A shell comment at the end of the hook command; Claude Code's settings schema has no place
/// for a marker of our own.
pub const MARKER: &str = "# reeve setup claude";
pub const MATCHER: &str = "startup|resume|clear|compact";
pub const DESCRIPTION: &str = "reeve is installed: named workers that outlive this session, a \
shared queue for heavy commands, handoffs. Read when long or parallel work, a handoff or a busy \
machine comes up.";
const STUB_LINE: &str = "Written by `reeve setup claude` for reeve ";

pub fn hook_command() -> String {
    format!("command -v reeve >/dev/null && reeve ls --brief || true {MARKER}")
}

fn group() -> Value {
    json!({
        "matcher": MATCHER,
        "hooks": [{ "type": "command", "command": hook_command() }]
    })
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
    let groups: Vec<&Value> = settings["hooks"]["SessionStart"]
        .as_array()
        .map(|gs| gs.iter().filter(|g| has_ours(g)).collect())
        .unwrap_or_default();
    match groups[..] {
        [] => Piece::Missing,
        [g] if *g == group() => Piece::Ok,
        _ => Piece::Stale,
    }
}

/// Takes out every hook carrying the marker, and any group, `SessionStart` list or `hooks`
/// object left empty by that. Returns whether anything changed.
pub fn remove_hook(settings: &mut Value) -> bool {
    let Some(groups) = settings
        .get_mut("hooks")
        .and_then(|h| h.get_mut("SessionStart"))
        .and_then(Value::as_array_mut)
    else {
        return false;
    };
    let before = groups.len();
    let mut changed = false;
    for g in groups.iter_mut().filter(|g| has_ours(g)) {
        if let Some(hs) = g["hooks"].as_array_mut() {
            hs.retain(|h| !ours(h));
            changed = true;
        }
    }
    groups.retain(|g| g["hooks"].as_array().is_none_or(|hs| !hs.is_empty()));
    changed |= groups.len() != before;
    if !changed {
        return false;
    }
    let hooks = settings["hooks"]
        .as_object_mut()
        .expect("hooks is an object");
    if hooks["SessionStart"].as_array().is_some_and(Vec::is_empty) {
        hooks.remove("SessionStart");
    }
    if hooks.is_empty() {
        settings.as_object_mut().expect("an object").remove("hooks");
    }
    changed
}

/// Adds the hook, replacing a stale one. Returns whether anything changed.
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
    if !hooks["SessionStart"].is_array() && !hooks["SessionStart"].is_null() {
        return Err("`hooks.SessionStart` in settings.json is not a list".into());
    }
    remove_hook(settings);
    let hooks = settings
        .as_object_mut()
        .expect("an object")
        .entry("hooks")
        .or_insert_with(|| json!({}));
    hooks
        .as_object_mut()
        .expect("hooks is an object")
        .entry("SessionStart")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .expect("a list")
        .push(group());
    Ok(true)
}

pub fn stub(version: &str) -> String {
    format!(
        "---\nname: reeve\ndescription: {DESCRIPTION}\n---\n\nRun `reeve --skill` for what it offers.\n\n{STUB_LINE}{version}.\n"
    )
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

/// `~/…` for paths under the home dir.
pub fn tilde(p: &Path) -> String {
    match std::env::home_dir().and_then(|h| p.strip_prefix(h).ok().map(Path::to_path_buf)) {
        Some(rest) => format!("~/{}", rest.display()),
        None => p.display().to_string(),
    }
}

pub struct Paths {
    pub settings: PathBuf,
    pub skill: PathBuf,
}

impl Paths {
    pub fn at(claude_dir: &Path) -> Paths {
        Paths {
            settings: claude_dir.join("settings.json"),
            skill: claude_dir.join("skills/reeve/SKILL.md"),
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

fn read_stub(path: &Path) -> io::Result<Option<String>> {
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

/// Writes both pieces and returns one line per file touched.
pub fn install(p: &Paths, version: &str, now: u64) -> io::Result<Vec<String>> {
    let mut out = vec![];
    let mut settings = read_settings(&p.settings)?.unwrap_or_else(|| json!({}));
    if add_hook(&mut settings).map_err(io::Error::other)? {
        write_settings(&p.settings, &settings, now, &mut out)?;
        out.push(format!(
            "wrote {}: SessionStart hook `reeve ls --brief`",
            tilde(&p.settings)
        ));
    } else {
        out.push(format!("unchanged {}", tilde(&p.settings)));
    }
    if stub_state(read_stub(&p.skill)?.as_deref(), version) == Piece::Ok {
        out.push(format!("unchanged {}", tilde(&p.skill)));
    } else {
        fs::create_dir_all(p.skill.parent().expect("skill dir"))?;
        fs::write(&p.skill, stub(version))?;
        out.push(format!("wrote {}", tilde(&p.skill)));
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
        out.push(format!("removed the hook from {}", tilde(&p.settings)));
    }
    match read_stub(&p.skill)? {
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
    if out.is_empty() {
        out.push("nothing to remove".into());
    }
    Ok(out)
}

/// The hook's and the stub's state, each with the file it lives in.
pub fn check(p: &Paths, version: &str) -> io::Result<[(String, Piece); 2]> {
    let settings = read_settings(&p.settings)?.unwrap_or_else(|| json!({}));
    let stub = read_stub(&p.skill)?;
    Ok([
        (
            format!("SessionStart hook in {}", tilde(&p.settings)),
            hook_state(&settings),
        ),
        (tilde(&p.skill), stub_state(stub.as_deref(), version)),
    ])
}

pub fn render_check(pieces: &[(String, Piece)]) -> String {
    pieces
        .iter()
        .map(|(what, piece)| match piece {
            Piece::Ok => format!("ok       {what}"),
            Piece::Missing => format!("missing  {what}; `reeve setup claude` adds it"),
            Piece::Stale => format!("stale    {what}; `reeve setup claude` rewrites it"),
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
        assert!(s.contains("Run `reeve --skill` for what it offers."));
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
        let pieces = |p: &Paths, v| check(p, v).unwrap().map(|(_, piece)| piece);
        assert_eq!(pieces(&p, "1.0.0"), [Piece::Missing, Piece::Missing]);

        let out = install(&p, "1.0.0", 7).unwrap();
        assert!(out[0].starts_with("backed up"), "{out:?}");
        assert_eq!(
            fs::read_to_string(dir.join("settings.json.reeve-7.bak")).unwrap(),
            original
        );
        assert_eq!(out.len(), 3, "{out:?}");
        assert_eq!(pieces(&p, "1.0.0"), [Piece::Ok, Piece::Ok]);
        assert_eq!(pieces(&p, "1.0.1"), [Piece::Ok, Piece::Stale]);
        let again = install(&p, "1.0.0", 8).unwrap();
        assert!(
            again.iter().all(|l| l.starts_with("unchanged")),
            "{again:?}"
        );
        assert!(!dir.join("settings.json.reeve-8.bak").exists());

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
        let out = install(&p, "1.0.0", 1).unwrap();
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
        let e = install(&p, "1.0.0", 1).unwrap_err().to_string();
        assert!(e.contains("not valid JSON"), "{e}");
        assert_eq!(fs::read_to_string(&p.settings).unwrap(), "{ nope");
        assert!(!p.skill.exists());
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
        install(&p, "1.0.0", 1).unwrap();
        assert!(fs::symlink_metadata(&p.settings).unwrap().is_symlink());
        assert!(fs::read_to_string(&real).unwrap().contains(MARKER));
        fs::remove_dir_all(&dir).unwrap();
    }
}
