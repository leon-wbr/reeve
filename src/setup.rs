//! `reeve setup claude` and `reeve setup codex`: a SessionStart hook that tells each new session
//! about running workers, prompt and tool hooks that tell it when to hand off, for Claude Code a
//! Bash hook that sends `reeve heavy` to the background, a skill stub that says what reeve
//! offers and, with `--claude-md` or `--agents-md`, a block in CLAUDE.md or AGENTS.md. For Codex
//! also a rules file that lets reeve's bookkeeping out of the sandbox. All are marked so that
//! `--check` and `--remove` find exactly what setup wrote.

use crate::workers::Kind;
use serde_json::{Value, json};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// A shell comment at the end of the hook command; the agents' hook schemas have no place
/// for a marker of our own.
pub fn marker(k: Kind) -> String {
    format!("# reeve setup {k}")
}
pub const MATCHER: &str = "startup|resume|clear|compact";
pub const DESCRIPTION: &str = "reeve is installed: test suites, e2e, builds and installs go \
through `reeve heavy`, in the background; named workers outlive this session; `reeve handoff` \
starts your successor. Read before heavy, long or parallel work, or a handoff.";
const STUB_START: &str = "Written by `reeve setup ";
const STUB_FOR: &str = "` for reeve ";

fn command(k: Kind, reeve: &str) -> String {
    format!(
        "command -v reeve >/dev/null && reeve {reeve} || true {}",
        marker(k)
    )
}

pub fn hook_command(k: Kind) -> String {
    command(k, "ls --brief")
}

pub fn context_command(k: Kind) -> String {
    command(k, "hook context")
}

pub fn background_command() -> String {
    command(Kind::Claude, "hook background")
}

/// Each hook event setup writes to, with the one group it adds there. Codex needs no
/// PreToolUse hook: its shell tool already leaves a long command in a background terminal.
fn groups(k: Kind) -> Vec<(&'static str, Value)> {
    let context = || json!({ "hooks": [{ "type": "command", "command": context_command(k) }] });
    let mut gs = vec![
        (
            "SessionStart",
            json!({
                "matcher": MATCHER,
                "hooks": [{ "type": "command", "command": hook_command(k) }]
            }),
        ),
        ("UserPromptSubmit", context()),
        ("PostToolUse", context()),
    ];
    if k == Kind::Claude {
        gs.push((
            "PreToolUse",
            json!({
                "matcher": "Bash",
                "hooks": [{ "type": "command", "command": background_command() }]
            }),
        ));
    }
    gs
}

fn ours(hook: &Value, k: Kind) -> bool {
    hook["command"]
        .as_str()
        .is_some_and(|c| c.contains(&marker(k)))
}

fn has_ours(group: &Value, k: Kind) -> bool {
    group["hooks"]
        .as_array()
        .is_some_and(|hs| hs.iter().any(|h| ours(h, k)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Piece {
    Ok,
    Missing,
    /// Present but not what this reeve writes.
    Stale,
}

/// Every event setup may write to, so a hook left from another version is found too.
const EVENTS: [&str; 4] = [
    "SessionStart",
    "UserPromptSubmit",
    "PostToolUse",
    "PreToolUse",
];

pub fn hook_state(settings: &Value, k: Kind) -> Piece {
    let found = |event: &str| -> Vec<&Value> {
        settings["hooks"][event]
            .as_array()
            .map(|gs| gs.iter().filter(|g| has_ours(g, k)).collect())
            .unwrap_or_default()
    };
    if EVENTS.iter().all(|e| found(e).is_empty()) {
        return Piece::Missing;
    }
    let want = groups(k);
    let current = EVENTS.iter().all(|e| {
        let got = found(e);
        match want.iter().find(|(w, _)| w == e) {
            Some((_, g)) => matches!(got[..], [x] if x == g),
            None => got.is_empty(),
        }
    });
    if current { Piece::Ok } else { Piece::Stale }
}

/// Takes out every hook carrying the marker, and any group, event list or `hooks` object left
/// empty by that. Returns whether anything changed.
pub fn remove_hook(settings: &mut Value, k: Kind) -> bool {
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else {
        return false;
    };
    let mut changed = false;
    for event in EVENTS {
        let Some(gs) = hooks.get_mut(event).and_then(Value::as_array_mut) else {
            continue;
        };
        let before = gs.len();
        let mut here = false;
        for g in gs.iter_mut().filter(|g| has_ours(g, k)) {
            if let Some(hs) = g["hooks"].as_array_mut() {
                hs.retain(|h| !ours(h, k));
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
pub fn add_hook(settings: &mut Value, k: Kind) -> Result<bool, String> {
    if hook_state(settings, k) == Piece::Ok {
        return Ok(false);
    }
    let file = match k {
        Kind::Claude => "settings.json",
        Kind::Codex => "hooks.json",
    };
    let Some(top) = settings.as_object_mut() else {
        return Err(format!("{file} is not a JSON object"));
    };
    let hooks = top.entry("hooks").or_insert_with(|| json!({}));
    if !hooks.is_object() {
        return Err(format!("`hooks` in {file} is not an object"));
    }
    for event in EVENTS {
        if !hooks[event].is_array() && !hooks[event].is_null() {
            return Err(format!("`hooks.{event}` in {file} is not a list"));
        }
    }
    remove_hook(settings, k);
    let hooks = settings
        .as_object_mut()
        .expect("an object")
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .expect("hooks is an object");
    for (event, group) in groups(k) {
        hooks
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .expect("a list")
            .push(group);
    }
    Ok(true)
}

/// reeve's own subcommands, which agents run without a prompt. Claude Code resolves a matching
/// allow rule before auto mode's classifier, so the brief in a `reeve run` is never judged. Not
/// `heavy`: it runs any command, so its rule would let any command past the classifier.
const ALLOWED: [&str; 14] = [
    "ls", "watch", "report", "close", "run", "handoff", "ask", "asks", "answer", "feedback",
    "doctor", "wait", "rm", "--skill",
];

/// The `permissions.allow` entries setup writes for Claude Code. `reeve --skill` is a flag, so
/// its rule has no `:*`.
pub fn allow_rules() -> Vec<String> {
    ALLOWED
        .iter()
        .map(|s| match *s {
            "--skill" => "Bash(reeve --skill)".to_string(),
            s => format!("Bash(reeve {s}:*)"),
        })
        .collect()
}

/// Ok when every rule is there, Stale when only some are, Missing when none is.
pub fn perms_state(settings: &Value) -> Piece {
    let have = settings["permissions"]["allow"].as_array().map_or(0, |l| {
        allow_rules()
            .iter()
            .filter(|r| l.iter().any(|v| v.as_str() == Some(r)))
            .count()
    });
    match have {
        0 => Piece::Missing,
        n if n == ALLOWED.len() => Piece::Ok,
        _ => Piece::Stale,
    }
}

/// Appends the missing allow rules after the user's own. Returns whether anything changed.
pub fn add_perms(settings: &mut Value) -> Result<bool, String> {
    if perms_state(settings) == Piece::Ok {
        return Ok(false);
    }
    let Some(top) = settings.as_object_mut() else {
        return Err("settings.json is not a JSON object".into());
    };
    let perms = top.entry("permissions").or_insert_with(|| json!({}));
    let Some(perms) = perms.as_object_mut() else {
        return Err("`permissions` in settings.json is not an object".into());
    };
    let allow = perms.entry("allow").or_insert_with(|| json!([]));
    let Some(list) = allow.as_array_mut() else {
        return Err("`permissions.allow` in settings.json is not a list".into());
    };
    for r in allow_rules() {
        if !list.iter().any(|v| v.as_str() == Some(&r)) {
            list.push(json!(r));
        }
    }
    Ok(true)
}

/// Takes out the allow rules setup writes, and an `allow` list or `permissions` object left
/// empty by that. Returns whether anything changed.
pub fn remove_perms(settings: &mut Value) -> bool {
    let rules = allow_rules();
    let Some(perms) = settings
        .get_mut("permissions")
        .and_then(Value::as_object_mut)
    else {
        return false;
    };
    let Some(list) = perms.get_mut("allow").and_then(Value::as_array_mut) else {
        return false;
    };
    let before = list.len();
    list.retain(|v| !v.as_str().is_some_and(|s| rules.iter().any(|r| r == s)));
    if list.len() == before {
        return false;
    }
    if list.is_empty() {
        perms.remove("allow");
    }
    if perms.is_empty() {
        settings
            .as_object_mut()
            .expect("an object")
            .remove("permissions");
    }
    true
}

/// When to use reeve, without syntax: `reeve --skill` prints the current manual, so nothing
/// here can go stale or teach a wrong flag.
pub const USES: &str = "reeve is installed on this machine. Use it for:\n\n\
- heavy commands: test suites, e2e, builds and installs;\n\
- long or parallel work, as named workers;\n\
- handing off to a successor when a \"Context check\" says so.\n\n\
Run `reeve --skill` before any of these; it prints the current manual with the exact commands.\n";

pub fn stub(k: Kind, version: &str) -> String {
    format!(
        "---\nname: reeve\ndescription: {DESCRIPTION}\n---\n\n{USES}\n{STUB_START}{k}{STUB_FOR}{version}.\n"
    )
}

/// The reeve version a stub was written for, if setup wrote it.
pub fn stub_version(text: &str) -> Option<&str> {
    text.lines()
        .filter_map(|l| l.strip_prefix(STUB_START))
        .find_map(|l| l.split_once(STUB_FOR))
        .map(|(_, v)| v.trim_end_matches('.'))
}

pub fn stub_state(text: Option<&str>, k: Kind, version: &str) -> Piece {
    match text {
        None => Piece::Missing,
        Some(t) if t == stub(k, version) => Piece::Ok,
        Some(_) => Piece::Stale,
    }
}

const RULES_START: &str = "# Written by `reeve setup codex`.";

/// Codex's sandbox can't write reeve's state dir, so reeve's own bookkeeping runs outside it
/// without a prompt. `reeve heavy` stays out: it runs any command.
pub fn rules() -> String {
    format!(
        "{RULES_START} reeve's bookkeeping writes ~/.local/state/reeve, which Codex's sandbox\n\
         # doesn't allow. `reeve heavy` isn't here: it runs any command, so Codex asks as usual.\n\
         prefix_rule(\n    pattern = [\"reeve\", [\"ls\", \"wait\", \"watch\", \"report\", \"rm\", \"run\", \
         \"close\", \"handoff\", \"ask\", \"asks\", \"answer\", \"feedback\", \"doctor\"]],\n    decision = \"allow\",\n    \
         match = [\"reeve ls\", \"reeve run --name x -- go\"],\n    \
         not_match = [\"reeve heavy -- cargo test\"],\n)\n"
    )
}

pub fn rules_state(text: Option<&str>) -> Piece {
    match text {
        None => Piece::Missing,
        Some(t) if t == rules() => Piece::Ok,
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
    pub kind: Kind,
    /// The JSON file with the hooks: settings.json or hooks.json.
    pub settings: PathBuf,
    pub skill: PathBuf,
    /// CLAUDE.md or AGENTS.md.
    pub md: PathBuf,
    pub rules: Option<PathBuf>,
}

impl Paths {
    pub fn at(claude_dir: &Path) -> Paths {
        Paths {
            kind: Kind::Claude,
            settings: claude_dir.join("settings.json"),
            skill: claude_dir.join("skills/reeve/SKILL.md"),
            md: claude_dir.join("CLAUDE.md"),
            rules: None,
        }
    }

    /// Codex keeps hooks, AGENTS.md and rules in its home, and finds user skills in
    /// `~/.agents/skills`.
    pub fn codex(codex_home: &Path, home: &Path) -> Paths {
        Paths {
            kind: Kind::Codex,
            settings: codex_home.join("hooks.json"),
            skill: home.join(".agents/skills/reeve/SKILL.md"),
            md: codex_home.join("AGENTS.md"),
            rules: Some(codex_home.join("rules/reeve.rules")),
        }
    }

    /// Where each agent itself looks: `$CLAUDE_CONFIG_DIR` or `~/.claude`, `$CODEX_HOME` or
    /// `~/.codex`.
    pub fn of(k: Kind) -> io::Result<Paths> {
        if k == Kind::Claude {
            return Ok(Paths::at(&Paths::default_dir()?));
        }
        let home = std::env::home_dir().ok_or_else(|| io::Error::other("no home dir"))?;
        let codex_home = std::env::var_os("CODEX_HOME")
            .filter(|v| !v.is_empty())
            .map_or(home.join(".codex"), PathBuf::from);
        Ok(Paths::codex(&codex_home, &home))
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
    let file = path.file_name().expect("a file name").to_string_lossy();
    if target.exists() {
        let backup = (1..)
            .map(|i| match i {
                1 => format!("{file}.reeve-{now}.bak"),
                i => format!("{file}.reeve-{now}-{i}.bak"),
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
    let tmp = target.with_file_name(format!("{file}.reeve-{}", std::process::id()));
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
    let at = tilde(&p.md);
    move |e| io::Error::other(format!("{e} ({at}); reeve left it alone"))
}

/// Writes the pieces and returns one line per file touched. The CLAUDE.md or AGENTS.md block is
/// written when asked for, and kept current once it is there.
pub fn install(p: &Paths, version: &str, with_md: bool, now: u64) -> io::Result<Vec<String>> {
    let md = read_text(&p.md)?;
    let md_piece = block_state(md.as_deref()).map_err(md_err(p))?;
    let mut out = vec![];
    let mut settings = read_settings(&p.settings)?.unwrap_or_else(|| json!({}));
    let hooks_changed = add_hook(&mut settings, p.kind).map_err(io::Error::other)?;
    let perms_changed =
        p.kind == Kind::Claude && add_perms(&mut settings).map_err(io::Error::other)?;
    if hooks_changed || perms_changed {
        write_settings(&p.settings, &settings, now, &mut out)?;
    }
    if hooks_changed {
        let hooks = match p.kind {
            Kind::Claude => {
                "hooks `reeve ls --brief` (session start), `reeve hook context` (prompts and \
                 tool calls) and `reeve hook background` (Bash calls)"
            }
            Kind::Codex => {
                "hooks `reeve ls --brief` (session start) and `reeve hook context` (prompts and \
                 tool calls)"
            }
        };
        out.push(format!("wrote {}: {hooks}", tilde(&p.settings)));
    }
    if perms_changed {
        out.push(format!(
            "wrote {}: allows reeve's own subcommands, which auto mode's classifier then skips",
            tilde(&p.settings)
        ));
    }
    if !hooks_changed && !perms_changed {
        out.push(format!("unchanged {}", tilde(&p.settings)));
    }
    if stub_state(read_text(&p.skill)?.as_deref(), p.kind, version) == Piece::Ok {
        out.push(format!("unchanged {}", tilde(&p.skill)));
    } else {
        fs::create_dir_all(p.skill.parent().expect("skill dir"))?;
        fs::write(&p.skill, stub(p.kind, version))?;
        out.push(format!("wrote {}", tilde(&p.skill)));
    }
    if let Some(path) = &p.rules {
        if rules_state(read_text(path)?.as_deref()) == Piece::Ok {
            out.push(format!("unchanged {}", tilde(path)));
        } else {
            write_text(path, &rules())?;
            out.push(format!(
                "wrote {}: reeve's bookkeeping runs outside the sandbox",
                tilde(path)
            ));
        }
    }
    match md_piece {
        Piece::Ok => out.push(format!("unchanged {}", tilde(&p.md))),
        Piece::Missing if !with_md => {}
        _ => {
            let text = add_block(md.as_deref().unwrap_or("")).map_err(md_err(p))?;
            write_text(&p.md, &text)?;
            out.push(format!("wrote {}: the reeve block", tilde(&p.md)));
        }
    }
    Ok(out)
}

/// An AGENTS.md linked to CLAUDE.md (one instructions file for both agents) shares its block.
fn shares_claude_md(md: &Path) -> bool {
    let claude = Paths::of(Kind::Claude).map(|p| p.md);
    match (fs::canonicalize(md), claude.and_then(fs::canonicalize)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Takes out what setup wrote, and nothing else: a SKILL.md that setup didn't write stays.
pub fn remove(p: &Paths, now: u64) -> io::Result<Vec<String>> {
    let mut out = vec![];
    if let Some(mut settings) = read_settings(&p.settings)? {
        let hooks = remove_hook(&mut settings, p.kind);
        let perms = p.kind == Kind::Claude && remove_perms(&mut settings);
        if hooks || perms {
            write_settings(&p.settings, &settings, now, &mut out)?;
        }
        if hooks {
            out.push(format!("removed the hooks from {}", tilde(&p.settings)));
        }
        if perms {
            out.push(format!(
                "removed the allow rules from {}",
                tilde(&p.settings)
            ));
        }
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
    if let Some(path) = &p.rules
        && read_text(path)?.is_some_and(|t| t.starts_with(RULES_START))
    {
        fs::remove_file(path)?;
        out.push(format!("removed {}", tilde(path)));
    }
    if p.kind == Kind::Codex && shares_claude_md(&p.md) {
        out.push(format!(
            "left the reeve block in {}: it is Claude Code's CLAUDE.md too",
            tilde(&p.md)
        ));
    } else if let Some(md) = read_text(&p.md)?
        && let Some(text) = remove_block(&md).map_err(md_err(p))?
    {
        write_text(&p.md, &text)?;
        out.push(format!("removed the reeve block from {}", tilde(&p.md)));
    }
    if out.is_empty() {
        out.push("nothing to remove".into());
    }
    Ok(out)
}

/// Each piece's state with where it lives: the hooks, the stub, the Codex rules and, when asked
/// for or present, the CLAUDE.md or AGENTS.md block.
pub fn check(p: &Paths, version: &str, with_md: bool) -> io::Result<Vec<(String, Piece)>> {
    let settings = read_settings(&p.settings)?.unwrap_or_else(|| json!({}));
    let stub = read_text(&p.skill)?;
    let mut out = vec![
        (
            format!("hooks in {}", tilde(&p.settings)),
            hook_state(&settings, p.kind),
        ),
        (
            tilde(&p.skill),
            stub_state(stub.as_deref(), p.kind, version),
        ),
    ];
    if p.kind == Kind::Claude {
        out.push((
            format!("allow rules in {}", tilde(&p.settings)),
            perms_state(&settings),
        ));
    }
    if let Some(path) = &p.rules {
        out.push((tilde(path), rules_state(read_text(path)?.as_deref())));
    }
    let md = block_state(read_text(&p.md)?.as_deref()).map_err(md_err(p))?;
    if with_md || md != Piece::Missing {
        out.push((format!("reeve block in {}", tilde(&p.md)), md));
    }
    Ok(out)
}

pub fn render_check(pieces: &[(String, Piece)], k: Kind, with_md: bool) -> String {
    let md = match (with_md, k) {
        (false, _) => "",
        (true, Kind::Claude) => " --claude-md",
        (true, Kind::Codex) => " --agents-md",
    };
    let setup = format!("`reeve setup {k}{md}`");
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

    const C: Kind = Kind::Claude;

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
        assert_eq!(hook_state(&s, C), Piece::Missing);
        assert_eq!(add_hook(&mut s, C), Ok(true));
        assert_eq!(hook_state(&s, C), Piece::Ok);
        let once = s.clone();
        assert_eq!(add_hook(&mut s, C), Ok(false));
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
            add_hook(&mut s, C).unwrap();
            assert!(remove_hook(&mut s, C));
            assert_eq!(s, before);
            assert!(!remove_hook(&mut s, C));
        }
    }

    #[test]
    fn a_changed_hook_is_stale_and_gets_replaced() {
        let mut s = others();
        add_hook(&mut s, C).unwrap();
        s["hooks"]["SessionStart"][1]["hooks"][0]["command"] =
            json!(format!("reeve ls {}", marker(C)));
        assert_eq!(hook_state(&s, C), Piece::Stale);
        assert_eq!(add_hook(&mut s, C), Ok(true));
        assert_eq!(hook_state(&s, C), Piece::Ok);
        assert_eq!(s["hooks"]["SessionStart"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_marked_hook_shares_its_group_with_others_only_as_stale() {
        let mut s = others();
        s["hooks"]["SessionStart"][0]["hooks"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "type": "command", "command": hook_command(C) }));
        assert_eq!(hook_state(&s, C), Piece::Stale);
        assert!(remove_hook(&mut s, C));
        assert_eq!(s, others());
    }

    #[test]
    fn settings_of_the_wrong_shape_are_refused() {
        assert!(add_hook(&mut json!([]), C).is_err());
        assert!(add_hook(&mut json!({"hooks": []}), C).is_err());
        assert!(add_hook(&mut json!({"hooks": {"SessionStart": {}}}), C).is_err());
    }

    #[test]
    fn the_hook_runs_the_brief_listing_and_never_fails() {
        let c = hook_command(C);
        assert!(c.starts_with("command -v reeve >/dev/null && reeve ls --brief || true"));
        assert!(c.ends_with(&marker(C)));
    }

    #[test]
    fn the_stub_is_short_and_carries_its_version() {
        let s = stub(C, "1.2.3");
        assert!(s.starts_with("---\nname: reeve\ndescription: reeve is installed"));
        assert!(s.contains(USES));
        assert!(s.len() / 4 <= 160, "{}", s.len() / 4);
        assert_eq!(stub_version(&s), Some("1.2.3"));
        assert_eq!(stub_version("---\nname: reeve\n---\nmine\n"), None);
        assert!(DESCRIPTION.len() / 4 <= 60, "{}", DESCRIPTION.len() / 4);
        assert_eq!(stub_state(Some(&s), C, "1.2.3"), Piece::Ok);
        assert_eq!(stub_state(Some(&s), C, "1.2.4"), Piece::Stale);
        assert_eq!(stub_state(None, C, "1.2.3"), Piece::Missing);
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
        assert_eq!(pieces(&p, "1.0.0"), [Piece::Missing; 3]);

        let out = install(&p, "1.0.0", false, 7).unwrap();
        assert!(out[0].starts_with("backed up"), "{out:?}");
        assert_eq!(
            fs::read_to_string(dir.join("settings.json.reeve-7.bak")).unwrap(),
            original
        );
        assert_eq!(out.len(), 4, "{out:?}");
        assert_eq!(pieces(&p, "1.0.0"), [Piece::Ok; 3]);
        assert_eq!(pieces(&p, "1.0.1"), [Piece::Ok, Piece::Stale, Piece::Ok]);
        let again = install(&p, "1.0.0", false, 8).unwrap();
        assert!(
            again.iter().all(|l| l.starts_with("unchanged")),
            "{again:?}"
        );
        assert!(!dir.join("settings.json.reeve-8.bak").exists());
        assert!(!p.md.exists());

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
            hook_state(&read_settings(&p.settings).unwrap().unwrap(), C),
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
        add_hook(&mut s, C).unwrap();
        for event in ["UserPromptSubmit", "PostToolUse"] {
            let gs = s["hooks"][event].as_array().unwrap();
            assert_eq!(gs.len(), 1, "{event}");
            assert_eq!(gs[0]["hooks"][0]["command"], context_command(C));
            assert!(gs[0].get("matcher").is_none(), "{event}");
        }
        let c = context_command(C);
        assert!(c.starts_with("command -v reeve >/dev/null && reeve hook context || true"));
        assert!(c.ends_with(&marker(C)));
    }

    #[test]
    fn the_background_hook_runs_before_bash_calls_and_never_fails() {
        let mut s = others();
        add_hook(&mut s, C).unwrap();
        let gs = s["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(gs.len(), 1);
        assert_eq!(gs[0]["matcher"], "Bash");
        assert_eq!(gs[0]["hooks"][0]["command"], background_command());
        let c = background_command();
        assert!(c.starts_with("command -v reeve >/dev/null && reeve hook background || true"));
        assert!(c.ends_with(&marker(C)));
        s["hooks"].as_object_mut().unwrap().remove("PreToolUse");
        assert_eq!(hook_state(&s, C), Piece::Stale);
    }

    #[test]
    fn a_setup_with_only_the_session_start_hook_is_stale() {
        let mut s = others();
        add_hook(&mut s, C).unwrap();
        s["hooks"].as_object_mut().unwrap().remove("PostToolUse");
        assert_eq!(hook_state(&s, C), Piece::Stale);
        assert_eq!(add_hook(&mut s, C), Ok(true));
        assert_eq!(hook_state(&s, C), Piece::Ok);
    }

    #[test]
    fn removing_leaves_a_users_own_empty_event_list() {
        let before = json!({"hooks": {"PostToolUse": [], "Stop": []}});
        let mut s = before.clone();
        add_hook(&mut s, C).unwrap();
        assert!(remove_hook(&mut s, C));
        assert_eq!(s, json!({"hooks": {"Stop": []}}));
        let mut s = json!({"hooks": {"PostToolUse": []}});
        let mut with = s.clone();
        with["hooks"]["SessionStart"] = json!([{ "hooks": [{ "command": hook_command(C) }] }]);
        assert!(remove_hook(&mut with, C));
        assert_eq!(with, s);
        assert!(!remove_hook(&mut s, C));
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
        fs::write(&p.md, "# Mine\n").unwrap();
        install(&p, "1.0.0", false, 1).unwrap();
        assert_eq!(fs::read_to_string(&p.md).unwrap(), "# Mine\n");
        assert_eq!(check(&p, "1.0.0", false).unwrap().len(), 3);
        let asked = check(&p, "1.0.0", true).unwrap();
        assert_eq!(asked[3].1, Piece::Missing);
        assert!(render_check(&asked, C, true).contains("`reeve setup claude --claude-md` adds it"));

        install(&p, "1.0.0", true, 2).unwrap();
        let md = fs::read_to_string(&p.md).unwrap();
        assert_eq!(md, format!("# Mine\n\n{}\n", block()));
        assert_eq!(check(&p, "1.0.0", false).unwrap()[3].1, Piece::Ok);
        fs::write(&p.md, md.replace("reeve is installed", "old")).unwrap();
        assert_eq!(check(&p, "1.0.0", false).unwrap()[3].1, Piece::Stale);
        install(&p, "1.0.0", false, 3).unwrap();
        assert_eq!(fs::read_to_string(&p.md).unwrap(), md);

        let out = remove(&p, 4).unwrap();
        assert!(out.iter().any(|l| l.contains("reeve block")), "{out:?}");
        assert_eq!(fs::read_to_string(&p.md).unwrap(), "# Mine\n");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_claude_md_with_a_broken_block_is_left_alone() {
        let dir = temp("mdbad");
        let p = Paths::at(&dir);
        let broken = format!("{BLOCK_START}\nmine now\n");
        fs::write(&p.md, &broken).unwrap();
        let e = install(&p, "1.0.0", true, 1).unwrap_err().to_string();
        assert!(e.contains("left it alone"), "{e}");
        assert!(!p.settings.exists());
        assert!(remove(&p, 2).is_err());
        assert_eq!(fs::read_to_string(&p.md).unwrap(), broken);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_claude_md_stays_a_symlink() {
        let dir = temp("mdlink");
        let p = Paths::at(&dir);
        let real = dir.join("dotfiles.md");
        fs::write(&real, "# Mine\n").unwrap();
        std::os::unix::fs::symlink(&real, &p.md).unwrap();
        install(&p, "1.0.0", true, 1).unwrap();
        assert!(fs::symlink_metadata(&p.md).unwrap().is_symlink());
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
        assert!(fs::read_to_string(&real).unwrap().contains(&marker(C)));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn codex_gets_the_session_and_context_hooks_but_no_bash_hook() {
        let mut s = others();
        assert_eq!(add_hook(&mut s, Kind::Codex), Ok(true));
        assert_eq!(hook_state(&s, Kind::Codex), Piece::Ok);
        assert_eq!(hook_state(&s, C), Piece::Missing);
        assert!(s["hooks"].get("PreToolUse").is_none());
        let start = &s["hooks"]["SessionStart"][1]["hooks"][0]["command"];
        assert_eq!(start, &json!(hook_command(Kind::Codex)));
        assert!(hook_command(Kind::Codex).ends_with("# reeve setup codex"));
        // Claude's hooks beside Codex's are left alone by both.
        add_hook(&mut s, C).unwrap();
        assert_eq!(hook_state(&s, Kind::Codex), Piece::Ok);
        assert!(remove_hook(&mut s, Kind::Codex));
        assert_eq!(hook_state(&s, C), Piece::Ok);
        assert!(remove_hook(&mut s, C));
        assert_eq!(s, others());
    }

    #[test]
    fn a_codex_bash_hook_left_from_elsewhere_makes_its_setup_stale() {
        let mut s = json!({});
        add_hook(&mut s, Kind::Codex).unwrap();
        s["hooks"]["PreToolUse"] =
            json!([{ "hooks": [{ "command": format!("x {}", marker(Kind::Codex)) }] }]);
        assert_eq!(hook_state(&s, Kind::Codex), Piece::Stale);
        add_hook(&mut s, Kind::Codex).unwrap();
        assert!(s["hooks"].get("PreToolUse").is_none());
    }

    #[test]
    fn the_codex_stub_says_who_wrote_it() {
        let s = stub(Kind::Codex, "1.2.3");
        assert!(s.contains("Written by `reeve setup codex` for reeve 1.2.3."));
        assert_eq!(stub_version(&s), Some("1.2.3"));
        assert_eq!(stub_state(Some(&s), Kind::Codex, "1.2.3"), Piece::Ok);
        assert_eq!(stub_state(Some(&s), C, "1.2.3"), Piece::Stale);
    }

    #[test]
    fn the_rules_let_out_bookkeeping_but_not_heavy() {
        let r = rules();
        assert!(r.starts_with(RULES_START));
        for sub in [
            "ls", "wait", "watch", "report", "rm", "run", "close", "handoff", "ask", "answer",
            "feedback", "doctor",
        ] {
            assert!(r.contains(&format!("\"{sub}\"")), "{sub}");
        }
        assert!(!r.contains("\"heavy\","), "{r}");
        assert!(r.contains("decision = \"allow\""));
        assert_eq!(rules_state(Some(&r)), Piece::Ok);
        assert_eq!(rules_state(Some("old")), Piece::Stale);
        assert_eq!(rules_state(None), Piece::Missing);
    }

    #[test]
    fn codex_install_check_and_remove_round_trip_on_disk() {
        let dir = temp("codex");
        let (codex, home) = (dir.join(".codex"), dir.join("home"));
        let p = Paths::codex(&codex, &home);
        assert_eq!(p.skill, home.join(".agents/skills/reeve/SKILL.md"));
        let pieces = |p: &Paths| -> Vec<Piece> {
            check(p, "1.0.0", false)
                .unwrap()
                .into_iter()
                .map(|(_, x)| x)
                .collect()
        };
        assert_eq!(pieces(&p), [Piece::Missing; 3]);
        let out = install(&p, "1.0.0", false, 1).unwrap();
        assert_eq!(out.len(), 3, "{out:?}");
        assert!(
            out[0].contains("hooks.json") && !out[0].contains("background"),
            "{out:?}"
        );
        assert_eq!(pieces(&p), [Piece::Ok; 3]);
        assert!(!p.md.exists());
        let checked = check(&p, "1.0.0", true).unwrap();
        assert!(
            render_check(&checked, Kind::Codex, true)
                .contains("`reeve setup codex --agents-md` adds it")
        );

        install(&p, "1.0.0", true, 2).unwrap();
        assert_eq!(fs::read_to_string(&p.md).unwrap(), format!("{}\n", block()));
        remove(&p, 3).unwrap();
        assert_eq!(fs::read_to_string(&p.settings).unwrap(), "{}\n");
        assert!(!p.skill.exists());
        assert!(!p.rules.as_ref().unwrap().exists());
        assert!(!p.md.exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    fn mine() -> Value {
        json!({ "model": "opus", "permissions": { "allow": ["Bash(git status)"], "deny": ["Bash(rm:*)"] } })
    }

    #[test]
    fn the_allow_rules_cover_reeves_subcommands_and_nothing_broad() {
        let rules = allow_rules();
        for sub in [
            "ls", "watch", "report", "close", "run", "handoff", "ask", "asks", "answer",
            "feedback", "doctor", "wait", "rm",
        ] {
            assert!(rules.contains(&format!("Bash(reeve {sub}:*)")), "{sub}");
        }
        assert!(rules.contains(&"Bash(reeve --skill)".to_string()));
        assert!(
            rules
                .iter()
                .all(|r| r.starts_with("Bash(reeve ") && r.ends_with(')'))
        );
        assert!(
            !rules
                .iter()
                .any(|r| r == "Bash(reeve:*)" || r.contains('*') && !r.ends_with(":*)"))
        );
    }

    #[test]
    fn allow_rules_merge_after_the_users_own_idempotently_and_come_out_exactly() {
        for before in [mine(), json!({})] {
            let mut s = before.clone();
            assert_eq!(perms_state(&s), Piece::Missing);
            assert_eq!(add_perms(&mut s), Ok(true));
            assert_eq!(perms_state(&s), Piece::Ok);
            let once = s.clone();
            assert_eq!(add_perms(&mut s), Ok(false));
            assert_eq!(s, once);
            assert!(remove_perms(&mut s));
            assert_eq!(s, before);
            assert!(!remove_perms(&mut s));
        }
        let mut s = mine();
        add_perms(&mut s).unwrap();
        assert_eq!(s["permissions"]["allow"][0], "Bash(git status)");
        assert_eq!(s["permissions"]["deny"], mine()["permissions"]["deny"]);
    }

    #[test]
    fn some_allow_rules_missing_is_stale_and_gets_completed_without_duplicates() {
        let mut s = json!({"permissions": {"allow": ["Bash(reeve close:*)", "Bash(reeve ls:*)"]}});
        assert_eq!(perms_state(&s), Piece::Stale);
        assert_eq!(add_perms(&mut s), Ok(true));
        assert_eq!(perms_state(&s), Piece::Ok);
        let list = s["permissions"]["allow"].as_array().unwrap();
        assert_eq!(list.len(), allow_rules().len());
        assert_eq!(list[0], "Bash(reeve close:*)");
    }

    #[test]
    fn permissions_of_the_wrong_shape_are_refused() {
        assert!(add_perms(&mut json!([])).is_err());
        assert!(add_perms(&mut json!({"permissions": []})).is_err());
        assert!(add_perms(&mut json!({"permissions": {"allow": "x"}})).is_err());
    }

    #[test]
    fn claude_setup_installs_checks_and_removes_the_allow_rules_on_disk() {
        let dir = temp("perms");
        let p = Paths::at(&dir);
        let original = serde_json::to_string_pretty(&mine()).unwrap() + "\n";
        fs::write(&p.settings, &original).unwrap();
        let out = install(&p, "1.0.0", false, 1).unwrap();
        assert!(
            out.iter().any(|l| l.contains("allows reeve's own")),
            "{out:?}"
        );
        let s = read_settings(&p.settings).unwrap().unwrap();
        assert_eq!(perms_state(&s), Piece::Ok);
        let checked = check(&p, "1.0.0", false).unwrap();
        assert_eq!(checked[2].1, Piece::Ok);
        assert!(checked[2].0.starts_with("allow rules in"), "{checked:?}");
        let again = install(&p, "1.0.0", false, 2).unwrap();
        assert!(
            again.iter().all(|l| l.starts_with("unchanged")),
            "{again:?}"
        );

        // Dropping one rule shows up as stale and install restores it.
        let mut s = s;
        s["permissions"]["allow"].as_array_mut().unwrap().pop();
        fs::write(&p.settings, serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(check(&p, "1.0.0", false).unwrap()[2].1, Piece::Stale);
        install(&p, "1.0.0", false, 3).unwrap();
        assert_eq!(check(&p, "1.0.0", false).unwrap()[2].1, Piece::Ok);

        let out = remove(&p, 4).unwrap();
        assert!(out.iter().any(|l| l.contains("allow rules")), "{out:?}");
        assert_eq!(fs::read_to_string(&p.settings).unwrap(), original);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn codex_setup_leaves_permissions_alone() {
        let dir = temp("codexperms");
        let p = Paths::codex(&dir.join(".codex"), &dir.join("home"));
        install(&p, "1.0.0", false, 1).unwrap();
        let s = read_settings(&p.settings).unwrap().unwrap();
        assert!(s.get("permissions").is_none());
        assert_eq!(check(&p, "1.0.0", false).unwrap().len(), 3);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_rules_file_setup_didnt_write_stays_on_remove() {
        let dir = temp("rules");
        let p = Paths::codex(&dir.join(".codex"), &dir.join("home"));
        let rules = p.rules.clone().unwrap();
        fs::create_dir_all(rules.parent().unwrap()).unwrap();
        fs::write(&rules, "prefix_rule(pattern = [\"x\"])\n").unwrap();
        remove(&p, 1).unwrap();
        assert!(rules.exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
