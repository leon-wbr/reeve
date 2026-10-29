//! Messages: one JSON file per message, the same on the Mac and in a remote's box (see
//! docs/remotes.md). reeve checks the envelope and carries everything else untouched; it never
//! reads the body for meaning, and no code path acts on a message.

use serde_json::{Map, Value, json};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

pub const MAX_BODY: usize = 64 * 1024;
/// Generous: a 64 KiB body can grow several times over once its characters are escaped.
const MAX_FILE: usize = 512 * 1024;
const MAX_ID: usize = 64;
const MAX_ADDR: usize = 128;
/// Seen markers, delivered copies and read messages are kept this long, on both sides.
pub const RETAIN_DAYS: u64 = 30;

/// `$REEVE_HOME`, else `~/.reeve`: where the local inbox, spool and sent copies live.
pub fn home() -> io::Result<PathBuf> {
    std::env::var_os("REEVE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| Some(std::env::home_dir()?.join(".reeve")))
        .ok_or_else(|| io::Error::other("no REEVE_HOME or HOME"))
}

// ---- time ----

/// `YYYY-MM-DDTHH:MM:SSZ`.
pub fn fmt_utc(secs: u64) -> String {
    let (y, m, d) = crate::feedback::civil(secs / 86_400);
    let t = secs % 86_400;
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        t / 3600,
        t / 60 % 60,
        t % 60
    )
}

/// The seconds since the epoch of a `YYYY-MM-DDTHH:MM:SSZ` time, and nothing looser.
pub fn parse_utc(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    let shape = b"dddd-dd-ddTdd:dd:ddZ";
    if b.len() != shape.len() {
        return None;
    }
    let digits = b.iter().zip(shape).all(|(c, s)| match s {
        b'd' => c.is_ascii_digit(),
        _ => c == s,
    });
    if !digits {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s[r].parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (hh, mm, ss) = (n(11..13)?, n(14..16)?, n(17..19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 59 {
        return None;
    }
    // Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hh * 3600 + mm * 60 + ss).ok()
}

pub fn now_secs() -> u64 {
    crate::events::now_ms() / 1000
}

/// `24h`, `30m`, `90s` or `2d`, in seconds.
pub fn parse_ttl(s: &str) -> Option<u64> {
    let (num, unit) = s.split_at(s.len().checked_sub(1)?);
    let n: u64 = num.parse().ok().filter(|n| *n > 0)?;
    let per = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return None,
    };
    n.checked_mul(per)
}

/// `5m`, `3h`, `2d`: how long ago, for lists.
pub fn age(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

// ---- ids and addresses ----

/// Sortable time plus a random part: `20261029T140311Z-9f3a1c`.
pub fn new_id(now: u64) -> String {
    let stamp: String = fmt_utc(now)
        .chars()
        .filter(|c| *c != '-' && *c != ':')
        .collect();
    format!("{stamp}-{}", random_hex())
}

fn random_hex() -> String {
    let mut b = [0u8; 3];
    if File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .is_err()
    {
        let n = crate::events::now_ms() ^ (u64::from(std::process::id()) << 20);
        b = [n as u8, (n >> 8) as u8, (n >> 16) as u8];
    }
    format!("{:02x}{:02x}{:02x}", b[0], b[1], b[2])
}

/// What can safely be a file name and a shell word.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID
        && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
}

fn name_ok(s: &str, extra: &[char]) -> bool {
    let mut cs = s.chars();
    cs.next().is_some_and(|c| c.is_ascii_lowercase())
        && cs.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || extra.contains(&c))
        && s.len() <= 32
}

/// A remote's name, which is also a directory name.
pub fn check_remote_name(name: &str) -> Result<(), String> {
    if name_ok(name, &['-']) {
        Ok(())
    } else {
        Err(format!(
            "remote name {name:?} must match [a-z][a-z0-9-]{{0,31}}"
        ))
    }
}

/// `name` (here) or `name@remote`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub name: String,
    pub remote: Option<String>,
}

pub fn parse_address(s: &str) -> Result<Address, String> {
    let (name, remote) = match s.split_once('@') {
        Some((n, r)) => (n, Some(r)),
        None => (s, None),
    };
    if !name_ok(name, &['-', '_']) {
        return Err(format!(
            "address {s:?} must be NAME or NAME@REMOTE, with NAME matching [a-z][a-z0-9_-]{{0,31}}"
        ));
    }
    if let Some(r) = remote {
        check_remote_name(r)?;
    }
    Ok(Address {
        name: name.into(),
        remote: remote.map(Into::into),
    })
}

// ---- the envelope ----

/// A new message with its final id, ready to spool or to put in the local inbox.
pub fn compose(
    from: &str,
    to: &str,
    body: &str,
    reply_to: Option<&str>,
    ttl: Option<u64>,
    now: u64,
) -> Result<Value, String> {
    if body.len() > MAX_BODY {
        return Err(format!("message body is over {} KiB", MAX_BODY / 1024));
    }
    if let Some(r) = reply_to.filter(|r| !valid_id(r)) {
        return Err(format!("reply-to {r:?} is not a message id"));
    }
    let mut m = Map::new();
    m.insert("v".into(), json!(1));
    m.insert("id".into(), json!(new_id(now)));
    m.insert("from".into(), json!(from));
    m.insert("to".into(), json!(to));
    m.insert("at".into(), json!(fmt_utc(now)));
    if let Some(r) = reply_to {
        m.insert("reply_to".into(), json!(r));
    }
    if let Some(t) = ttl {
        m.insert("expires_at".into(), json!(fmt_utc(now + t)));
    }
    m.insert("body".into(), json!(body));
    Ok(Value::Object(m))
}

fn text_field<'a>(m: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    let s = m
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{key} is missing or not text"))?;
    Ok(s)
}

fn address_field(m: &Map<String, Value>, key: &str) -> Result<(), String> {
    let s = text_field(m, key)?;
    if s.is_empty() || s.len() > MAX_ADDR || s.chars().any(char::is_control) {
        return Err(format!(
            "{key} is empty, too long or has control characters"
        ));
    }
    Ok(())
}

/// The message in `raw`, if its envelope is sound and its id is `file_id`. Unknown keys stay.
pub fn validate(raw: &str, file_id: &str) -> Result<Value, String> {
    if raw.len() > MAX_FILE {
        return Err("file is too large".into());
    }
    let v: Value = serde_json::from_str(raw).map_err(|e| format!("not JSON ({e})"))?;
    let m = v.as_object().ok_or("not a JSON object")?;
    match m.get("v").and_then(Value::as_u64) {
        Some(1) => {}
        _ => return Err("v is not 1".into()),
    }
    let id = text_field(m, "id")?;
    if !valid_id(id) {
        return Err("id has characters outside [A-Za-z0-9-] or is too long".into());
    }
    if id != file_id {
        return Err("id does not match the file name".into());
    }
    address_field(m, "from")?;
    address_field(m, "to")?;
    if parse_utc(text_field(m, "at")?).is_none() {
        return Err("at is not a UTC time like 2026-10-29T14:03:11Z".into());
    }
    if let Some(r) = m.get("reply_to")
        && !r.as_str().is_some_and(valid_id)
    {
        return Err("reply_to is not a message id".into());
    }
    if let Some(e) = m.get("expires_at")
        && e.as_str().and_then(parse_utc).is_none()
    {
        return Err("expires_at is not a UTC time".into());
    }
    if text_field(m, "body")?.len() > MAX_BODY {
        return Err(format!("body is over {} KiB", MAX_BODY / 1024));
    }
    Ok(v)
}

/// Stamps a pulled message with the remote it came through. Identity is the box, not the
/// `from` field, so any `via` the sender put in is replaced.
pub fn stamp_via(mut m: Value, remote: &str) -> Value {
    if let Some(o) = m.as_object_mut() {
        o.insert("via".into(), json!(remote));
    }
    m
}

pub fn field<'a>(m: &'a Value, key: &str) -> &'a str {
    m[key].as_str().unwrap_or("")
}

pub fn expires_at(m: &Value) -> Option<u64> {
    m["expires_at"].as_str().and_then(parse_utc)
}

pub fn expired(m: &Value, now: u64) -> bool {
    expires_at(m).is_some_and(|e| now >= e)
}

/// Text from another machine, safe to print: terminal control characters (an escape sequence
/// can rewrite the screen) become `?`; newlines and tabs stay.
pub fn clean(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                '?'
            } else {
                c
            }
        })
        .collect()
}

// ---- files ----

/// Writes `ID.json` into `dir` by a temporary dotfile and a rename, so a reader never sees half
/// a message.
pub fn write_atomic(dir: &Path, m: &Value) -> io::Result<()> {
    let id = field(m, "id");
    if !valid_id(id) {
        return Err(io::Error::other("message has no valid id"));
    }
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".tmp-{id}.{}", std::process::id()));
    let mut f = File::create(&tmp)?;
    f.write_all(serde_json::to_string(m)?.as_bytes())?;
    f.sync_all()?;
    fs::rename(&tmp, dir.join(format!("{id}.json")))
}

/// The messages in `dir`, oldest id first. Dotfiles and files that don't parse yet are skipped.
pub fn list(dir: &Path) -> Vec<Value> {
    let Ok(rd) = fs::read_dir(dir) else {
        return vec![];
    };
    let mut ids: Vec<String> = rd
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter_map(|n| n.strip_suffix(".json").map(str::to_string))
        .filter(|id| !id.starts_with('.') && valid_id(id))
        .collect();
    ids.sort();
    ids.iter()
        .filter_map(|id| {
            let raw = fs::read_to_string(dir.join(format!("{id}.json"))).ok()?;
            validate(&raw, id).ok()
        })
        .collect()
}

/// The local boxes under one reeve home.
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn at(root: PathBuf) -> Store {
        Store { root }
    }

    pub fn open() -> io::Result<Store> {
        home().map(Store::at)
    }

    pub fn inbox(&self) -> PathBuf {
        self.root.join("inbox")
    }

    pub fn read_dir(&self) -> PathBuf {
        self.inbox().join(".read")
    }

    pub fn spool(&self, remote: &str) -> PathBuf {
        self.root.join("spool").join(remote)
    }

    /// A copy of every message sent from here, to tell which incoming ones were answered.
    pub fn sent(&self) -> PathBuf {
        self.root.join("sent")
    }

    pub fn contact(&self, remote: &str) -> PathBuf {
        self.root.join("contact").join(format!("{remote}.json"))
    }

    /// Whether a message with this id is in the inbox, read or not.
    pub fn has(&self, id: &str) -> bool {
        valid_id(id)
            && [self.inbox(), self.read_dir()]
                .iter()
                .any(|d| d.join(format!("{id}.json")).exists())
    }

    pub fn find(&self, id: &str) -> Option<(Value, bool)> {
        if !valid_id(id) {
            return None;
        }
        [(self.inbox(), false), (self.read_dir(), true)]
            .into_iter()
            .find_map(|(d, read)| {
                let raw = fs::read_to_string(d.join(format!("{id}.json"))).ok()?;
                validate(&raw, id).ok().map(|m| (m, read))
            })
    }

    /// Moves a message to `.read/`; false if it was not unread.
    pub fn mark_read(&self, id: &str) -> io::Result<bool> {
        if !valid_id(id) {
            return Ok(false);
        }
        let from = self.inbox().join(format!("{id}.json"));
        if !from.exists() {
            return Ok(false);
        }
        fs::create_dir_all(self.read_dir())?;
        fs::rename(from, self.read_dir().join(format!("{id}.json")))?;
        Ok(true)
    }

    /// Read copies older than the retention time.
    pub fn prune(&self) {
        let cutoff = std::time::Duration::from_secs(RETAIN_DAYS * 86_400);
        let Ok(rd) = fs::read_dir(self.read_dir()) else {
            return;
        };
        for e in rd.flatten() {
            let old = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|d| d > cutoff);
            if old {
                let _ = fs::remove_file(e.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        compose("hermes@box", "me", "hi", None, None, 1_793_016_045).unwrap()
    }

    #[test]
    fn times_round_trip_and_only_the_exact_shape_parses() {
        assert_eq!(fmt_utc(1_793_016_045), "2026-10-26T12:00:45Z");
        assert_eq!(parse_utc("2026-10-26T12:00:45Z"), Some(1_793_016_045));
        assert_eq!(parse_utc("2000-02-29T00:00:00Z"), Some(951_782_400));
        assert_eq!(parse_utc("1970-01-01T00:00:00Z"), Some(0));
        for bad in [
            "2026-10-26 12:00:45Z",
            "2026-10-26T12:00:45",
            "2026-10-26T12:00:45+00:00",
            "2026-13-26T12:00:45Z",
            "2026-10-26T24:00:45Z",
            "2026-10-26T12:00:4xZ",
            "",
        ] {
            assert_eq!(parse_utc(bad), None, "{bad}");
        }
    }

    #[test]
    fn ids_sort_by_time_and_use_only_safe_characters() {
        let a = new_id(1_793_016_045);
        let b = new_id(1_793_016_046);
        assert!(a.starts_with("20261026T120045Z-") && valid_id(&a), "{a}");
        assert!(a < b);
        assert_ne!(new_id(5), new_id(5));
        for bad in [
            "",
            "a/b",
            "a b",
            "../x",
            "a.json",
            "a;b",
            "é",
            &"x".repeat(65),
        ] {
            assert!(!valid_id(bad), "{bad}");
        }
    }

    #[test]
    fn addresses_are_a_name_or_name_at_remote() {
        assert_eq!(
            parse_address("hermes@my-server").unwrap(),
            Address {
                name: "hermes".into(),
                remote: Some("my-server".into())
            }
        );
        assert_eq!(parse_address("leon").unwrap().remote, None);
        for bad in [
            "", "@x", "a@", "a@b@c", "A@b", "a@B", "a b@c", "a@b;ls", "-a@b",
        ] {
            assert!(parse_address(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn ttl_is_a_number_and_a_unit() {
        assert_eq!(parse_ttl("24h"), Some(86_400));
        assert_eq!(parse_ttl("90s"), Some(90));
        assert_eq!(parse_ttl("2d"), Some(172_800));
        for bad in ["", "h", "0h", "5", "5w", "-1h", "1.5h"] {
            assert_eq!(parse_ttl(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_composed_message_is_valid_and_optional_fields_appear_only_when_given() {
        let m = sample();
        let id = field(&m, "id").to_string();
        let raw = serde_json::to_string(&m).unwrap();
        assert!(validate(&raw, &id).is_ok());
        assert!(m.get("reply_to").is_none() && m.get("expires_at").is_none());
        let r = compose(
            "a",
            "b",
            "x",
            Some("20261026T120045Z-aaaaaa"),
            Some(3600),
            1_793_016_045,
        )
        .unwrap();
        assert_eq!(r["reply_to"], "20261026T120045Z-aaaaaa");
        assert_eq!(r["expires_at"], "2026-10-26T13:00:45Z");
        assert!(compose("a", "b", "x", Some("../x"), None, 0).is_err());
        assert!(compose("a", "b", &"x".repeat(MAX_BODY + 1), None, None, 0).is_err());
    }

    #[test]
    fn validation_rejects_what_is_not_a_message_and_keeps_unknown_keys() {
        let m = sample();
        let id = field(&m, "id").to_string();
        let with = |key: &str, val: Value| {
            let mut m = m.clone();
            m[key] = val;
            serde_json::to_string(&m).unwrap()
        };
        assert!(validate("not json", &id).is_err());
        assert!(validate("[1]", &id).is_err());
        assert!(validate(&with("v", json!(2)), &id).is_err());
        assert!(validate(&with("id", json!("../../etc")), "../../etc").is_err());
        assert!(validate(&with("body", json!(7)), &id).is_err());
        assert!(validate(&with("at", json!("yesterday")), &id).is_err());
        assert!(validate(&with("from", json!("a\u{1b}[2Jb")), &id).is_err());
        assert!(validate(&with("reply_to", json!("a b")), &id).is_err());
        assert!(validate(&with("expires_at", json!("soon")), &id).is_err());
        assert!(validate(&with("body", json!("x".repeat(MAX_BODY + 1))), &id).is_err());
        let raw = serde_json::to_string(&m).unwrap();
        assert!(
            validate(&raw, "20261026T120045Z-other").is_err(),
            "file name must match"
        );
        let extra = validate(&with("extra", json!({"k": [1]})), &id).unwrap();
        assert_eq!(extra["extra"], json!({"k": [1]}));
    }

    #[test]
    fn via_is_set_by_the_receiver_whatever_the_sender_claimed() {
        let mut m = sample();
        m["via"] = json!("somewhere-else");
        assert_eq!(stamp_via(m, "box")["via"], "box");
    }

    #[test]
    fn expiry_is_compared_with_the_writers_utc() {
        let m = compose("a", "b", "x", None, Some(60), 1000).unwrap();
        assert!(!expired(&m, 1059) && expired(&m, 1060));
        assert!(!expired(&sample(), u64::MAX), "no expires_at never expires");
    }

    #[test]
    fn terminal_escapes_in_remote_text_are_defanged() {
        assert_eq!(clean("a\u{1b}[31mb\nc\td"), "a?[31mb\nc\td");
    }

    #[test]
    fn files_are_written_whole_listed_in_id_order_and_dotfiles_are_ignored() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (
            compose("a", "b", "first", None, None, 100).unwrap(),
            compose("a", "b", "second", None, None, 200).unwrap(),
        );
        write_atomic(d.path(), &b).unwrap();
        write_atomic(d.path(), &a).unwrap();
        fs::write(d.path().join(".tmp-half"), "{\"v\":").unwrap();
        fs::write(d.path().join("junk.json"), "{}").unwrap();
        let bodies: Vec<_> = list(d.path())
            .iter()
            .map(|m| field(m, "body").to_string())
            .collect();
        assert_eq!(bodies, ["first", "second"]);
        let names: Vec<_> = fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(
            !names
                .iter()
                .any(|n| n.starts_with(".tmp-") && n != ".tmp-half")
        );
    }

    #[test]
    fn reading_moves_a_message_to_read_and_it_still_counts_as_here() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::at(d.path().into());
        let m = sample();
        let id = field(&m, "id").to_string();
        assert!(!s.has(&id));
        write_atomic(&s.inbox(), &m).unwrap();
        assert!(s.has(&id));
        assert_eq!(s.find(&id).map(|(_, read)| read), Some(false));
        assert!(s.mark_read(&id).unwrap());
        assert!(s.has(&id) && list(&s.inbox()).is_empty());
        assert_eq!(s.find(&id).map(|(_, read)| read), Some(true));
        assert!(!s.mark_read(&id).unwrap());
        assert!(!s.has("../x"));
    }
}
