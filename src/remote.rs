//! Remotes: machines reachable over ssh, each with a plain message box (docs/remotes.md).
//! reeve only reads and writes files there, always from this machine, and nothing waits on a
//! remote: a message for one is spooled here and pushed when it can be.

use crate::msg::{self, Store};
use crate::ssh::{self, Fail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

/// Messages pulled from one remote in one sync; the rest waits in its outbox.
const PULL_MAX: usize = 100;
/// Files in the local inbox before pulling stops, so a remote cannot fill the disk.
const INBOX_MAX: usize = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Owner {
    Known,
    #[default]
    Unknown,
}

impl std::fmt::Display for Owner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Owner::Known => "known",
            Owner::Unknown => "unknown",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Remote {
    pub name: String,
    /// What `ssh` is given: a host, `user@host` or an alias from the user's ssh config.
    pub ssh: String,
    #[serde(rename = "box", default = "default_box")]
    pub box_path: String,
    /// Whether the agent there has its own way to reach the user. Set by the user; reeve only
    /// shows it.
    #[serde(default)]
    pub owner_channel: Owner,
}

fn default_box() -> String {
    ssh::DEFAULT_BOX.into()
}

#[derive(Default, Serialize, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    remotes: Vec<Remote>,
}

/// `$XDG_CONFIG_HOME/reeve/remotes.json`, by default `~/.config/reeve/remotes.json`.
pub fn config_path() -> io::Result<PathBuf> {
    crate::context::config_path()
        .map(|p| p.with_file_name("remotes.json"))
        .ok_or_else(|| io::Error::other("no home dir"))
}

pub fn load() -> io::Result<Vec<Remote>> {
    match fs::read_to_string(config_path()?) {
        Ok(s) => serde_json::from_str::<ConfigFile>(&s)
            .map(|c| c.remotes)
            .map_err(|e| io::Error::other(format!("remotes.json: {e}"))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(e),
    }
}

fn save(remotes: Vec<Remote>) -> io::Result<()> {
    let path = config_path()?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(
        &tmp,
        serde_json::to_string_pretty(&ConfigFile { remotes })? + "\n",
    )?;
    fs::rename(tmp, path)
}

pub fn get(name: &str) -> io::Result<Remote> {
    load()?
        .into_iter()
        .find(|r| r.name == name)
        .ok_or_else(|| io::Error::other(format!("no remote {name}; see `reeve remote ls`")))
}

/// Writes the config only; nothing is contacted.
pub fn add(name: &str, target: &str, box_path: Option<&str>, owner: Owner) -> io::Result<()> {
    msg::check_remote_name(name).map_err(io::Error::other)?;
    ssh::check_target(target).map_err(io::Error::other)?;
    let box_path = box_path.unwrap_or(ssh::DEFAULT_BOX);
    ssh::check_box(box_path).map_err(io::Error::other)?;
    let mut all = load()?;
    if all.iter().any(|r| r.name == name) {
        return Err(io::Error::other(format!(
            "remote {name} exists already; `reeve remote rm {name}` first"
        )));
    }
    all.push(Remote {
        name: name.into(),
        ssh: target.into(),
        box_path: box_path.into(),
        owner_channel: owner,
    });
    save(all)
}

/// Takes a remote out of the config. Messages still queued for it are kept unless `force`.
pub fn rm(name: &str, force: bool) -> io::Result<()> {
    let mut all = load()?;
    let store = Store::open()?;
    let queued = msg::list(&store.spool(name)).len();
    if queued > 0 && !force {
        return Err(io::Error::other(format!(
            "{queued} message(s) are still queued for {name}; `reeve remote sync {name}` first, or --force to drop them"
        )));
    }
    let before = all.len();
    all.retain(|r| r.name != name);
    if all.len() == before {
        return Err(io::Error::other(format!(
            "no remote {name}; see `reeve remote ls`"
        )));
    }
    save(all)?;
    if force {
        let _ = fs::remove_dir_all(store.spool(name));
    }
    let _ = fs::remove_file(store.contact(name));
    Ok(())
}

// ---- contact: what the last attempts found ----

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Contact {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_ok: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fail: Option<u64>,
}

impl Contact {
    pub fn load(store: &Store, name: &str) -> Contact {
        fs::read_to_string(store.contact(name))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self, store: &Store, name: &str) {
        let path = store.contact(name);
        let tmp = path.with_extension("json.tmp");
        let write = || -> io::Result<()> {
            fs::create_dir_all(path.parent().unwrap_or(&path))?;
            fs::write(&tmp, serde_json::to_string(self)?)?;
            fs::rename(&tmp, &path)
        };
        let _ = write();
    }

    /// `reachable`, `unreachable` or `unknown`, from the last attempt that got an answer either way.
    pub fn state(&self) -> &'static str {
        match (self.last_ok, self.last_fail) {
            (None, None) => "unknown",
            (Some(ok), Some(fail)) if fail > ok => "unreachable",
            (None, Some(_)) => "unreachable",
            _ => "reachable",
        }
    }
}

// ---- one operation, as an io::Result ----

fn fail_error(name: &str, f: &Fail) -> io::Error {
    let what = ssh::describe(f);
    io::Error::other(match f {
        Fail::Unreachable(_) => format!("remote {name} is unreachable: {what}"),
        Fail::Timeout(_) => format!("remote {name}: {what}"),
        Fail::Remote(..) => format!("remote {name}: {what}"),
    })
}

/// Why a push or pull stopped.
struct Stop {
    err: io::Error,
    /// The connection is the problem, as opposed to what the box holds.
    down: bool,
}

impl Stop {
    fn of(name: &str, f: Fail) -> Stop {
        Stop {
            down: matches!(f, Fail::Unreachable(_) | Fail::Timeout(_)),
            err: fail_error(name, &f),
        }
    }

    fn box_invalid(name: &str, why: &str) -> Stop {
        Stop {
            down: false,
            err: io::Error::other(format!(
                "remote {name}: box is invalid ({why}); `reeve remote init {name}` makes it"
            )),
        }
    }
}

fn last_word(reply: &str) -> &str {
    reply
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or("")
}

// ---- sync ----

#[derive(Default)]
pub struct Report {
    pub pushed: usize,
    /// Pushes the remote already had: an earlier attempt landed but its answer was lost.
    pub duplicates: usize,
    pub pulled: usize,
    pub rejected: usize,
    pub inbox_full: bool,
    /// Whether any ssh got through.
    pub reached: bool,
    pub error: Option<io::Error>,
}

impl Report {
    pub fn line(&self, name: &str, queued: usize) -> String {
        if let Some(e) = &self.error {
            let left = if queued > 0 {
                format!(" ({queued} queued)")
            } else {
                String::new()
            };
            return format!("{name}: {}{left}", e);
        }
        let mut parts = vec![];
        if self.pushed + self.duplicates > 0 {
            parts.push(format!("sent {}", self.pushed + self.duplicates));
        }
        if self.pulled > 0 {
            parts.push(format!("received {}", self.pulled));
        }
        if self.rejected > 0 {
            parts.push(format!(
                "{} rejected (kept in the box's outbox/.rejected)",
                self.rejected
            ));
        }
        if self.inbox_full {
            parts.push("local inbox is full, the rest waits on the remote".into());
        }
        if parts.is_empty() {
            parts.push("nothing to do".into());
        }
        format!("{name}: {}", parts.join(", "))
    }
}

struct Ctx<'a> {
    store: &'a Store,
    r: &'a Remote,
    limit: Duration,
}

impl Ctx<'_> {
    fn call(&self, op: &str, args: &[&str], stdin: &[u8]) -> Result<String, Stop> {
        ssh::call(&self.r.ssh, &self.r.box_path, op, args, stdin, self.limit)
            .map_err(|f| Stop::of(&self.r.name, f))
    }
}

/// Sends the spool, oldest first, and stops at the first failure. Each file is deleted only after
/// the remote said `ok` or `dup`, so a lost answer costs one repeat that the remote recognises.
fn push_spool(cx: &Ctx, rep: &mut Report) -> Result<(), Stop> {
    for m in msg::list(&cx.store.spool(&cx.r.name)) {
        let id = msg::field(&m, "id").to_string();
        let mut line = serde_json::to_string(&m).map_err(|e| Stop {
            err: io::Error::other(e),
            down: false,
        })?;
        line.push('\n');
        let reply = cx.call("push", &[&id, &line.len().to_string()], line.as_bytes())?;
        rep.reached = true;
        match last_word(&reply) {
            "ok" => rep.pushed += 1,
            "dup" => rep.duplicates += 1,
            "nobox" => return Err(Stop::box_invalid(&cx.r.name, "it is not set up")),
            other => {
                return Err(Stop::box_invalid(
                    &cx.r.name,
                    &format!("unexpected answer {other:?}"),
                ));
            }
        }
        let _ = fs::remove_file(cx.store.spool(&cx.r.name).join(format!("{id}.json")));
    }
    Ok(())
}

fn local_inbox_len(store: &Store) -> usize {
    fs::read_dir(store.inbox()).map_or(0, |rd| {
        rd.flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
            .count()
    })
}

/// Copies new messages from the remote's outbox. A message is acked on the remote only after it
/// is whole in the local inbox, so a crash anywhere repeats a step and never loses one.
fn pull_outbox(cx: &Ctx, rep: &mut Report) -> Result<(), Stop> {
    let listing = cx.call("list", &[], &[])?;
    rep.reached = true;
    if listing.lines().next().map(str::trim) == Some("nobox") {
        return Err(Stop::box_invalid(&cx.r.name, "it is not set up"));
    }
    let ids: Vec<&str> = listing
        .lines()
        .map(str::trim)
        .filter(|l| msg::valid_id(l))
        .take(PULL_MAX)
        .collect();
    let mut held = local_inbox_len(cx.store);
    for id in ids {
        if !cx.store.has(id) {
            if held >= INBOX_MAX {
                rep.inbox_full = true;
                break;
            }
            let reply = cx.call("fetch", &[id], &[])?;
            match msg::validate(reply.trim(), id) {
                Ok(m) => {
                    msg::write_atomic(&cx.store.inbox(), &msg::stamp_via(m, &cx.r.name)).map_err(
                        |e| Stop {
                            err: e,
                            down: false,
                        },
                    )?;
                    rep.pulled += 1;
                    held += 1;
                }
                Err(_) => {
                    cx.call("reject", &[id], &[])?;
                    rep.rejected += 1;
                    continue;
                }
            }
        }
        cx.call("ack", &[id], &[])?;
    }
    Ok(())
}

/// Pushes what is spooled for `r`, then (unless `push_only`) pulls its outbox. Never panics and
/// never blocks longer than a few ssh limits; an unreachable remote is a result, not a hang.
pub fn sync(store: &Store, r: &Remote, limit: Duration, push_only: bool) -> Report {
    let cx = Ctx { store, r, limit };
    let mut rep = Report::default();
    let mut result = push_spool(&cx, &mut rep);
    if result.is_ok() && !push_only {
        result = pull_outbox(&cx, &mut rep);
    }
    let now = msg::now_secs();
    let mut contact = Contact::load(store, &r.name);
    if rep.reached {
        contact.last_ok = Some(now);
    }
    if let Err(stop) = result {
        if stop.down {
            contact.last_fail = Some(now);
        }
        rep.error = Some(stop.err);
    }
    contact.save(store, &r.name);
    if !push_only {
        store.prune();
    }
    rep
}

/// `reeve remote sync [NAME]`: every remote, or the one.
pub fn sync_cmd(name: Option<&str>) -> io::Result<i32> {
    let store = Store::open()?;
    let remotes = match name {
        Some(n) => vec![get(n)?],
        None => load()?,
    };
    let mut first_err = None;
    for r in &remotes {
        let rep = sync(&store, r, ssh::limit(), false);
        let queued = msg::list(&store.spool(&r.name)).len();
        println!("{}", rep.line(&r.name, queued));
        if first_err.is_none() {
            first_err = rep.error;
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(0),
    }
}

// ---- init, check, ls ----

pub fn init(name: &str) -> io::Result<i32> {
    let r = get(name)?;
    let reply = ssh::call(&r.ssh, &r.box_path, "init", &[], &[], ssh::limit())
        .map_err(|f| fail_error(name, &f))?;
    if last_word(&reply) != "ok" {
        return Err(io::Error::other(format!(
            "remote {name}: box is invalid (init did not finish)"
        )));
    }
    println!(
        "{name}: box ready at {} (inbox/, outbox/, seen/)",
        r.box_path
    );
    Ok(0)
}

/// Read-only: ssh works, the box exists, the tools are there.
pub fn check(name: &str) -> io::Result<i32> {
    let r = get(name)?;
    let reply = ssh::call(&r.ssh, &r.box_path, "check", &[], &[], ssh::limit())
        .map_err(|f| fail_error(name, &f))?;
    let mut problems = vec![];
    let mut counts = String::new();
    for l in reply.lines().map(str::trim) {
        if let Some(t) = l.strip_prefix("missing ") {
            problems.push(format!("the remote has no `{t}`"));
        } else if l == "box-missing" {
            problems.push(format!("no box yet; `reeve remote init {name}` creates it"));
        } else if let Some(n) = l.strip_prefix("outbox ") {
            counts += &format!("{n} waiting in its outbox");
        } else if let Some(n) = l.strip_prefix("inbox ") {
            counts += &format!(", {n} unread in its inbox");
        }
    }
    println!("{name}: ssh ok");
    if problems.is_empty() {
        println!("{name}: box ok at {} ({counts})", r.box_path);
    }
    for p in &problems {
        println!("{name}: {p}");
    }
    if r.owner_channel == Owner::Unknown {
        println!(
            "{name}: owner channel unknown: what its agent writes reaches you only through `reeve inbox`"
        );
    }
    Ok(i32::from(!problems.is_empty()))
}

pub fn ls() -> io::Result<String> {
    let remotes = load()?;
    if remotes.is_empty() {
        return Ok("no remotes; `reeve remote add NAME SSH_TARGET` adds one".into());
    }
    let store = Store::open()?;
    let now = msg::now_secs();
    let width = remotes
        .iter()
        .map(|r| r.name.len())
        .max()
        .unwrap_or(0)
        .max(4);
    let mut out = format!(
        "{:width$}  STATE        LAST OK  QUEUED  OWNER CHANNEL  SSH",
        "NAME"
    );
    for r in &remotes {
        let c = Contact::load(&store, &r.name);
        let last = c
            .last_ok
            .map_or("never".into(), |t| msg::age(now.saturating_sub(t)));
        out += &format!(
            "\n{:width$}  {:11}  {last:>7}  {:>6}  {:13}  {}",
            r.name,
            c.state(),
            msg::list(&store.spool(&r.name)).len(),
            r.owner_channel.to_string(),
            r.ssh
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contact_state_follows_the_latest_attempt() {
        let c = |ok, fail| Contact {
            last_ok: ok,
            last_fail: fail,
        };
        assert_eq!(c(None, None).state(), "unknown");
        assert_eq!(c(Some(5), None).state(), "reachable");
        assert_eq!(c(Some(5), Some(9)).state(), "unreachable");
        assert_eq!(c(Some(9), Some(5)).state(), "reachable");
        assert_eq!(c(None, Some(1)).state(), "unreachable");
    }

    #[test]
    fn a_cut_connection_is_down_and_a_bad_box_is_not() {
        let down = Stop::of("r", Fail::Unreachable("x".into()));
        assert!(down.down && down.err.to_string().contains("is unreachable"));
        let slow = Stop::of("r", Fail::Timeout(60));
        assert!(slow.down && slow.err.to_string().contains("ssh timed out"));
        let bad = Stop::box_invalid("r", "it is not set up");
        assert!(!bad.down && bad.err.to_string().contains("box is invalid"));
    }

    #[test]
    fn the_config_keeps_only_what_the_user_set() {
        let r = Remote {
            name: "myserver".into(),
            ssh: "myserver".into(),
            box_path: default_box(),
            owner_channel: Owner::Unknown,
        };
        let s = serde_json::to_string(&ConfigFile {
            remotes: vec![r.clone()],
        })
        .unwrap();
        assert!(
            s.contains(r#""box":"~/.reeve/box""#) && s.contains(r#""owner_channel":"unknown""#)
        );
        let back: ConfigFile =
            serde_json::from_str(r#"{"remotes":[{"name":"a","ssh":"h"}]}"#).unwrap();
        assert_eq!(back.remotes[0].box_path, "~/.reeve/box");
        assert_eq!(back.remotes[0].owner_channel, Owner::Unknown);
    }
}
