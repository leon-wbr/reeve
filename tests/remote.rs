//! The message box end to end. The remote side is a fake `ssh` on PATH that runs the command
//! locally against a temp HOME per target, and can fail on a plan: before running anything, after
//! running it (the answer is lost), or with its input cut short. Nothing here reaches a real host.

use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const FAKE_SSH: &str = r#"#!/bin/sh
ctl="$FAKE_SSH_DIR"
while [ $# -gt 0 ]; do
  case $1 in -o) shift 2;; --) shift; break;; *) break;; esac
done
target=$1; cmd=$2
printf '%s\t%s\n' "$target" "$cmd" >> "$ctl/calls.log"
mode=ok
if [ -e "$ctl/down" ]; then
  mode=before
elif [ -s "$ctl/plan" ]; then
  mode=$(head -n 1 "$ctl/plan")
  sed '1d' "$ctl/plan" > "$ctl/plan.tmp"; mv "$ctl/plan.tmp" "$ctl/plan"
fi
home="$ctl/homes/$target"
mkdir -p "$home"
case $mode in
  before) echo "ssh: connect to host $target port 22: Network is unreachable" >&2; exit 255 ;;
  hang) exec sleep 30 ;;
  after) HOME="$home" sh -c "$cmd" >/dev/null; echo "client_loop: send disconnect" >&2; exit 255 ;;
  cut) head -c 20 | HOME="$home" sh -c "$cmd" >/dev/null; echo "client_loop: send disconnect" >&2; exit 255 ;;
  *) HOME="$home" exec sh -c "$cmd" ;;
esac
"#;

struct Env {
    dir: tempfile::TempDir,
}

impl Env {
    fn new() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("ssh"), FAKE_SSH).unwrap();
        fs::set_permissions(bin.join("ssh"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::create_dir_all(dir.path().join("fake/homes")).unwrap();
        let env = Env { dir };
        // Every test goes through the fake: if this found a real ssh, nothing here would be safe.
        let found = env
            .reeve_env(Command::new("sh").args(["-c", "command -v ssh"]))
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&found.stdout).trim(),
            bin.join("ssh").display().to_string()
        );
        env
    }

    fn p(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn reeve_env<'a>(&self, c: &'a mut Command) -> &'a mut Command {
        let path = format!("{}:/usr/bin:/bin", self.p("bin").display());
        c.env_clear()
            .env("PATH", path)
            .env("HOME", self.p("localhome"))
            .env("REEVE_HOME", self.p("reeve-home"))
            .env("XDG_CONFIG_HOME", self.p("config"))
            .env("XDG_STATE_HOME", self.p("state"))
            .env("FAKE_SSH_DIR", self.p("fake"))
            .env("REEVE_SSH_TIMEOUT", "20")
            .stdin(Stdio::null())
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_reeve"));
        c.args(args);
        self.reeve_env(&mut c);
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    /// Runs reeve and requires success; returns stdout.
    fn ok(&self, args: &[&str]) -> String {
        let o = self.run(args);
        assert!(
            o.status.success(),
            "reeve {args:?}\nstdout: {}\nstderr: {}",
            out(&o),
            String::from_utf8_lossy(&o.stderr)
        );
        out(&o)
    }

    fn down(&self, on: bool) {
        let f = self.p("fake/down");
        if on {
            fs::write(f, "").unwrap();
        } else {
            let _ = fs::remove_file(f);
        }
    }

    /// What the fake does on its next calls, one word per call (`ok` once the plan runs out).
    fn plan(&self, modes: &[&str]) {
        fs::write(self.p("fake/plan"), modes.join("\n") + "\n").unwrap();
    }

    /// The remote's own HOME for an ssh target.
    fn remote_home(&self, target: &str) -> PathBuf {
        self.p("fake/homes").join(target)
    }

    fn remote_box(&self, target: &str) -> PathBuf {
        self.remote_home(target).join(".reeve/box")
    }

    fn calls(&self) -> String {
        fs::read_to_string(self.p("fake/calls.log")).unwrap_or_default()
    }

    fn add_remote(&self, name: &str) {
        self.ok(&["remote", "add", name, name]);
    }

    fn local(&self, rel: &str) -> PathBuf {
        self.p("reeve-home").join(rel)
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// File names (not dotfiles) ending in `.json` in `dir`, sorted.
fn jsons(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n.ends_with(".json") && !n.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn all_files(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        for e in fs::read_dir(dir).into_iter().flatten().flatten() {
            let rel = e.path().strip_prefix(base).unwrap().display().to_string();
            out.push(rel);
            if e.path().is_dir() {
                walk(&e.path(), base, out);
            }
        }
    }
    let mut v = vec![];
    walk(dir, dir, &mut v);
    v.sort();
    v
}

fn message(id: &str, from: &str, body: &str) -> Value {
    json!({"v": 1, "id": id, "from": from, "to": "me", "at": "2026-10-29T14:03:11Z", "body": body})
}

/// What a remote agent does: drop a finished message file in the box's outbox.
fn outbox_put(env: &Env, target: &str, m: &Value) {
    let dir = env.remote_box(target).join("outbox");
    fs::create_dir_all(&dir).unwrap();
    let id = m["id"].as_str().unwrap();
    fs::write(dir.join(format!("{id}.json")), m.to_string()).unwrap();
}

fn last_id(stdout: &str) -> String {
    stdout.split_whitespace().nth(1).unwrap().to_string()
}

fn events(env: &Env) -> String {
    fs::read_to_string(env.p("state/reeve/events.jsonl")).unwrap_or_default()
}

#[test]
fn adding_a_remote_only_writes_config_and_refuses_options_as_targets() {
    let e = Env::new();
    e.ok(&["remote", "add", "myserver", "me@myserver.example.net"]);
    assert_eq!(e.calls(), "", "add contacts nothing");
    let cfg = fs::read_to_string(e.p("config/reeve/remotes.json")).unwrap();
    assert!(cfg.contains("myserver.example.net") && cfg.contains("~/.reeve/box"));
    for bad in [
        &["remote", "add", "x", "-oProxyCommand=evil"][..],
        &["remote", "add", "Bad_Name", "host"],
        &["remote", "add", "myserver", "other"],
    ] {
        assert!(!e.run(bad).status.success(), "{bad:?}");
    }
    assert!(
        !e.run(&["remote", "add", "x", "h", "--box", "~/../etc"])
            .status
            .success()
    );
    assert_eq!(e.calls(), "");
}

#[test]
fn check_reads_and_init_creates_only_the_box() {
    let e = Env::new();
    e.add_remote("myserver");
    let checked = e.run(&["remote", "check", "myserver"]);
    assert!(!checked.status.success(), "no box yet");
    assert!(out(&checked).contains("no box yet"), "{}", out(&checked));
    assert_eq!(
        all_files(&e.remote_home("myserver")),
        Vec::<String>::new(),
        "check wrote nothing"
    );
    let init = e.ok(&["remote", "init", "myserver"]);
    assert!(init.contains("box ready"), "{init}");
    assert_eq!(
        all_files(&e.remote_home("myserver")),
        [
            ".reeve",
            ".reeve/box",
            ".reeve/box/inbox",
            ".reeve/box/outbox",
            ".reeve/box/seen"
        ]
    );
    let mode = fs::metadata(e.remote_box("myserver"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0, "the box is private: {mode:o}");
    let again = e.ok(&["remote", "check", "myserver"]);
    assert!(
        again.contains("box ok") && again.contains("owner channel unknown"),
        "{again}"
    );
}

#[test]
fn a_message_reaches_the_remote_inbox_whole_with_a_seen_marker_and_nothing_else() {
    let e = Env::new();
    e.add_remote("myserver");
    e.ok(&["remote", "init", "myserver"]);
    let sent = e.ok(&["send", "hermes@myserver", "hello there", "--ttl", "24h"]);
    assert!(sent.starts_with("delivered "), "{sent}");
    let id = last_id(&sent);
    let inbox = e.remote_box("myserver").join("inbox");
    assert_eq!(jsons(&inbox), [format!("{id}.json")]);
    assert!(e.remote_box("myserver").join("seen").join(&id).exists());
    let m: Value =
        serde_json::from_str(&fs::read_to_string(inbox.join(format!("{id}.json"))).unwrap())
            .unwrap();
    assert_eq!(m["body"], "hello there");
    assert_eq!(m["to"], "hermes@myserver");
    assert_eq!(m["v"], 1);
    assert!(m["expires_at"].is_string());
    assert!(
        all_files(&inbox).iter().all(|f| !f.starts_with(".tmp-")),
        "no half-written file is left"
    );
    assert_eq!(jsons(&e.local("spool/myserver")), Vec::<String>::new());
}

#[test]
fn a_message_sent_while_offline_is_queued_then_delivered_exactly_once() {
    let e = Env::new();
    e.add_remote("myserver");
    e.ok(&["remote", "init", "myserver"]);
    e.down(true);
    let sent = e.ok(&["send", "hermes@myserver", "later"]);
    assert!(sent.starts_with("queued "), "{sent}");
    let id = last_id(&sent);
    assert_eq!(jsons(&e.local("spool/myserver")), [format!("{id}.json")]);
    let failed = e.run(&["remote", "sync", "myserver"]);
    assert!(!failed.status.success());
    assert!(err(&failed).contains("is unreachable"), "{}", err(&failed));
    assert_eq!(
        jsons(&e.local("spool/myserver")).len(),
        1,
        "the spool survives"
    );
    e.down(false);
    assert!(e.ok(&["remote", "sync", "myserver"]).contains("sent 1"));
    assert!(
        e.ok(&["remote", "sync", "myserver"])
            .contains("nothing to do")
    );
    assert_eq!(
        jsons(&e.remote_box("myserver").join("inbox")),
        [format!("{id}.json")]
    );
    assert_eq!(jsons(&e.local("spool/myserver")), Vec::<String>::new());
}

#[test]
fn a_push_whose_answer_was_lost_is_a_duplicate_on_the_retry_not_a_second_message() {
    let e = Env::new();
    e.add_remote("myserver");
    e.ok(&["remote", "init", "myserver"]);
    e.plan(&["after"]);
    let sent = e.ok(&["send", "hermes@myserver", "once"]);
    assert!(
        sent.starts_with("queued "),
        "the remote committed but reeve never heard: {sent}"
    );
    let id = last_id(&sent);
    assert_eq!(jsons(&e.remote_box("myserver").join("inbox")).len(), 1);
    e.ok(&["remote", "sync", "myserver"]);
    assert_eq!(
        jsons(&e.remote_box("myserver").join("inbox")),
        [format!("{id}.json")]
    );
    assert_eq!(jsons(&e.local("spool/myserver")), Vec::<String>::new());
    // The agent reads it and moves it away; reeve still must not send it again.
    let inbox = e.remote_box("myserver").join("inbox");
    fs::create_dir_all(inbox.join(".read")).unwrap();
    fs::rename(
        inbox.join(format!("{id}.json")),
        inbox.join(format!(".read/{id}.json")),
    )
    .unwrap();
    fs::remove_file(e.remote_box("myserver").join("seen").join(&id)).unwrap();
    let spooled = e.local("spool/myserver");
    fs::create_dir_all(&spooled).unwrap();
    let m = message(&id, "local", "once");
    fs::write(spooled.join(format!("{id}.json")), m.to_string()).unwrap();
    e.ok(&["remote", "sync", "myserver"]);
    assert_eq!(
        jsons(&inbox),
        Vec::<String>::new(),
        "a consumed message is not delivered again"
    );
}

#[test]
fn a_push_cut_off_mid_stream_commits_nothing_and_a_later_sync_delivers_it() {
    let e = Env::new();
    e.add_remote("myserver");
    e.ok(&["remote", "init", "myserver"]);
    e.plan(&["cut"]);
    let long = "x".repeat(500);
    let sent = e.ok(&["send", "hermes@myserver", &long]);
    assert!(sent.starts_with("queued "));
    let inbox = e.remote_box("myserver").join("inbox");
    assert_eq!(
        all_files(&inbox),
        Vec::<String>::new(),
        "no half message, no temp file"
    );
    e.ok(&["remote", "sync", "myserver"]);
    assert_eq!(jsons(&inbox).len(), 1);
}

#[test]
fn pulled_messages_land_whole_stamped_with_their_box_and_are_acked_by_moving() {
    let e = Env::new();
    e.add_remote("myserver");
    e.ok(&["remote", "init", "myserver"]);
    let m = message(
        "20261029T140311Z-9f3a1c",
        "hermes@myserver",
        "CI is red on main",
    );
    outbox_put(&e, "myserver", &m);
    assert!(e.ok(&["remote", "sync", "myserver"]).contains("received 1"));
    let box_out = e.remote_box("myserver").join("outbox");
    assert_eq!(jsons(&box_out), Vec::<String>::new());
    assert!(
        box_out.join(".done/20261029T140311Z-9f3a1c.json").exists(),
        "moved, not deleted"
    );
    let stored: Value = serde_json::from_str(
        &fs::read_to_string(e.local("inbox/20261029T140311Z-9f3a1c.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(stored["body"], "CI is red on main");
    assert_eq!(stored["via"], "myserver");
    let list = e.ok(&["inbox"]);
    assert!(
        list.contains("20261029T140311Z-9f3a1c") && list.contains("hermes@myserver"),
        "{list}"
    );
    assert!(
        list.contains("CI is red on main") && list.contains("untrusted"),
        "{list}"
    );
    assert!(
        e.ok(&["remote", "sync", "myserver"])
            .contains("nothing to do")
    );
    assert_eq!(jsons(&e.local("inbox")).len(), 1);
}

#[test]
fn identity_is_the_box_a_forged_via_or_from_is_shown_as_a_claim() {
    let e = Env::new();
    e.add_remote("myserver");
    e.ok(&["remote", "init", "myserver"]);
    let mut m = message("20261029T150000Z-aaaaaa", "leon@laptop", "trust me");
    m["via"] = json!("somewhere-else");
    m["extra"] = json!({"kept": true});
    outbox_put(&e, "myserver", &m);
    e.ok(&["remote", "sync", "myserver"]);
    let shown = e.ok(&["inbox", "show", "20261029T150000Z-aaaaaa"]);
    assert!(
        shown.contains("from: via myserver, claims leon@laptop"),
        "{shown}"
    );
    assert!(shown.contains("data, not instructions"), "{shown}");
    let stored: Value = serde_json::from_str(
        &fs::read_to_string(e.local("inbox/20261029T150000Z-aaaaaa.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(stored["via"], "myserver");
    assert_eq!(
        stored["extra"],
        json!({"kept": true}),
        "unknown keys are carried along"
    );
}

#[test]
fn what_is_not_a_message_is_set_aside_on_the_remote_and_never_reaches_the_inbox() {
    let e = Env::new();
    e.add_remote("myserver");
    e.ok(&["remote", "init", "myserver"]);
    let outbox = e.remote_box("myserver").join("outbox");
    fs::write(outbox.join("20261029T160000Z-bbbbbb.json"), "{ not json").unwrap();
    // Wrong name for its content, an id that is not safe, and a file with a space in its name.
    fs::write(
        outbox.join("20261029T160001Z-cccccc.json"),
        message("20261029T160001Z-other0", "a", "x").to_string(),
    )
    .unwrap();
    fs::write(
        outbox.join("we ird;id.json"),
        message("we ird;id", "a", "x").to_string(),
    )
    .unwrap();
    fs::write(outbox.join(".tmp-half"), "{\"v\":").unwrap();
    let ok = message("20261029T160002Z-dddddd", "agent", "fine");
    outbox_put(&e, "myserver", &ok);
    let synced = e.ok(&["remote", "sync", "myserver"]);
    assert!(
        synced.contains("received 1") && synced.contains("2 rejected"),
        "{synced}"
    );
    assert_eq!(jsons(&e.local("inbox")), ["20261029T160002Z-dddddd.json"]);
    assert!(
        outbox
            .join(".rejected/20261029T160000Z-bbbbbb.json")
            .exists()
    );
    assert!(
        outbox.join("we ird;id.json").exists(),
        "an unsafe name is never touched"
    );
    assert!(
        outbox.join(".tmp-half").exists(),
        "a half-written file is left for its writer"
    );
    assert!(
        !e.calls().contains("we ird"),
        "an id outside [A-Za-z0-9-] never reaches a remote command"
    );
    // Rejected files are not retried on every sync.
    assert!(
        e.ok(&["remote", "sync", "myserver"])
            .contains("nothing to do")
    );
}

#[test]
fn message_text_and_odd_ids_never_reach_a_remote_command_line() {
    let e = Env::new();
    e.add_remote("myserver");
    e.ok(&["remote", "init", "myserver"]);
    let nasty = "'; touch PWNED; echo '$(touch PWNED2) `touch PWNED3` \"\n\\";
    let sent = e.ok(&["send", "hermes@myserver", nasty]);
    let id = last_id(&sent);
    let stored =
        fs::read_to_string(e.remote_box("myserver").join(format!("inbox/{id}.json"))).unwrap();
    let m: Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(m["body"], nasty, "the text arrives untouched");
    assert!(
        !e.calls().contains("PWNED"),
        "only stdin carries message text"
    );
    for f in all_files(&e.remote_home("myserver")) {
        assert!(!f.contains("PWNED"), "{f}");
    }
    for bad in [
        &["send", "hermes@myserver", "x", "--reply-to", "a;b"][..],
        &["send", "a b@myserver", "x"],
        &["send", "x@my;server", "x"],
        &["inbox", "done", "../x"],
        &["inbox", "show", "a/b"],
    ] {
        assert!(!e.run(bad).status.success(), "{bad:?}");
    }
}

#[test]
fn a_local_send_goes_straight_to_the_local_inbox_and_inbox_filters_work() {
    let e = Env::new();
    e.add_remote("myserver");
    e.ok(&["remote", "init", "myserver"]);
    let local = e.ok(&["send", "leon", "note to self", "--from", "scout"]);
    assert!(local.contains("(local)"), "{local}");
    let a = message("20261029T170000Z-aaaaaa", "hermes@myserver", "question one");
    let b = message("20261029T170001Z-bbbbbb", "hermes@myserver", "question two");
    outbox_put(&e, "myserver", &a);
    outbox_put(&e, "myserver", &b);
    e.ok(&["remote", "sync"]);
    assert_eq!(
        e.ok(&["inbox", "--from", "hermes"])
            .matches("question")
            .count(),
        2
    );
    assert_eq!(
        e.ok(&["inbox", "--from", "scout"])
            .matches("note to self")
            .count(),
        1
    );
    // Answering one takes it off the unanswered list; no typed "ask" involved.
    e.ok(&[
        "send",
        "hermes@myserver",
        "yes",
        "--reply-to",
        "20261029T170000Z-aaaaaa",
    ]);
    let open = e.ok(&["inbox", "--unanswered", "--from", "hermes"]);
    assert!(
        open.contains("question two") && !open.contains("question one"),
        "{open}"
    );
    e.ok(&["inbox", "done", "20261029T170001Z-bbbbbb"]);
    let unread = e.ok(&["inbox", "--from", "hermes"]);
    assert!(
        unread.contains("question one") && !unread.contains("question two"),
        "{unread}"
    );
    assert!(
        e.ok(&["inbox", "--all", "--from", "hermes"])
            .contains("[read]")
    );
    assert!(
        !e.run(&["inbox", "done", "20261029T170001Z-bbbbbb"])
            .status
            .success()
    );
    let reply = e.remote_box("myserver").join("inbox");
    let delivered: Vec<Value> = jsons(&reply)
        .iter()
        .map(|f| serde_json::from_str(&fs::read_to_string(reply.join(f)).unwrap()).unwrap())
        .collect();
    assert_eq!(delivered[0]["reply_to"], "20261029T170000Z-aaaaaa");
}

#[test]
fn nothing_but_the_command_name_and_a_fixed_error_kind_reaches_the_events() {
    let e = Env::new();
    e.ok(&[
        "remote",
        "add",
        "private-label",
        "bob@secret-host.example.net",
    ]);
    e.ok(&["remote", "init", "private-label"]);
    e.ok(&["send", "hermes@private-label", "the launch code is 12345"]);
    outbox_put(
        &e,
        "bob@secret-host.example.net",
        &message("20261029T180000Z-eeeeee", "agent", "reply text 777"),
    );
    e.ok(&["remote", "sync"]);
    e.down(true);
    let failed = e.run(&["remote", "sync", "private-label"]);
    assert!(!failed.status.success());
    e.run(&["remote", "check", "private-label"]);
    e.run(&["send", "hermes@nowhere", "x"]);
    let log = events(&e);
    let commands: Vec<String> = log
        .lines()
        .map(|l| {
            serde_json::from_str::<Value>(l).unwrap()["command"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert!(
        commands.contains(&"remote sync".to_string()) && commands.contains(&"send".to_string())
    );
    let kinds: Vec<Value> = log
        .lines()
        .filter_map(|l| {
            serde_json::from_str::<Value>(l)
                .unwrap()
                .get("error")
                .cloned()
        })
        .collect();
    assert!(kinds.contains(&json!("remote_unreachable")), "{kinds:?}");
    assert!(kinds.contains(&json!("no_remote")), "{kinds:?}");
    // `feedback` attaches the last events and the version, so it must be clean too.
    let fb = e.ok(&["feedback", "remotes are odd"]);
    let note = fs::read_to_string(
        fs::read_dir(e.p("state/reeve/feedback"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path(),
    )
    .unwrap();
    for text in [&log, &note, &fb] {
        for secret in [
            "secret-host",
            "example.net",
            "bob@",
            "private-label",
            "launch code",
            "12345",
            "reply text",
            "20261029T",
            ".ssh",
            "BEGIN",
        ] {
            assert!(!text.contains(secret), "{secret} leaked into:\n{text}");
        }
    }
}

#[test]
fn doctor_says_what_is_configured_and_what_is_stuck_without_contacting_anyone() {
    let e = Env::new();
    assert!(out(&e.run(&["doctor"])).contains("no remotes configured"));
    e.add_remote("myserver");
    e.down(true);
    e.ok(&["send", "hermes@myserver", "stuck"]);
    let before = e.calls().lines().count();
    let report = out(&e.run(&["doctor"]));
    assert!(report.contains("remotes: myserver"), "{report}");
    assert!(
        report.contains("1 message(s) queued for myserver"),
        "{report}"
    );
    assert_eq!(
        e.calls().lines().count(),
        before,
        "doctor opens no connection"
    );
}

#[test]
fn a_remote_with_queued_messages_cannot_be_removed_without_force() {
    let e = Env::new();
    e.add_remote("myserver");
    e.down(true);
    e.ok(&["send", "hermes@myserver", "stuck"]);
    let refused = e.run(&["remote", "rm", "myserver"]);
    assert!(
        !refused.status.success() && err(&refused).contains("still queued"),
        "{}",
        err(&refused)
    );
    e.ok(&["remote", "rm", "myserver", "--force"]);
    assert!(e.ok(&["remote", "ls"]).contains("no remotes"));
    assert!(!e.local("spool/myserver").exists());
}

#[test]
fn a_hung_ssh_is_cut_at_the_limit_and_leaves_the_spool_alone() {
    let e = Env::new();
    e.add_remote("myserver");
    e.down(true);
    e.ok(&["send", "hermes@myserver", "stuck"]);
    e.down(false);
    e.plan(&["hang"]);
    let slow = e
        .cmd(&["remote", "sync", "myserver"])
        .env("REEVE_SSH_TIMEOUT", "1")
        .output()
        .unwrap();
    assert!(!slow.status.success());
    assert!(err(&slow).contains("ssh timed out"), "{}", err(&slow));
    assert_eq!(jsons(&e.local("spool/myserver")).len(), 1);
    assert!(events(&e).contains("remote_timeout"));
}

#[test]
fn remote_ls_shows_unreachable_then_reachable() {
    let e = Env::new();
    e.add_remote("myserver");
    assert!(e.ok(&["remote", "ls"]).contains("unknown"));
    e.down(true);
    e.run(&["remote", "sync", "myserver"]);
    assert!(e.ok(&["remote", "ls"]).contains("unreachable"));
    e.down(false);
    e.ok(&["remote", "init", "myserver"]);
    e.ok(&["remote", "sync", "myserver"]);
    let ls = e.ok(&["remote", "ls"]);
    assert!(
        ls.contains("reachable") && !ls.contains("unreachable"),
        "{ls}"
    );
}

/// Kills the connection at every step of one sync, in each way it can die, and checks that the
/// next syncs end in the same state: every message exactly once on each side.
#[test]
fn a_connection_that_drops_at_any_step_never_loses_or_duplicates_a_message() {
    let ids = ["20261029T190000Z-aaaaaa", "20261029T190001Z-bbbbbb"];
    for mode in ["before", "after", "cut"] {
        for k in 0..9 {
            let e = Env::new();
            e.add_remote("myserver");
            e.ok(&["remote", "init", "myserver"]);
            e.down(true);
            let sent = e.ok(&["send", "hermes@myserver", "outgoing"]);
            let out_id = last_id(&sent);
            e.down(false);
            for id in ids {
                outbox_put(&e, "myserver", &message(id, "hermes@myserver", id));
            }
            let mut plan = vec!["ok"; k];
            plan.push(mode);
            e.plan(&plan);
            let first = e.run(&["remote", "sync", "myserver"]);
            assert!(
                first.status.success() || k < 6,
                "{mode} {k}: only a planned failure may fail"
            );
            for _ in 0..3 {
                e.run(&["remote", "sync", "myserver"]);
            }
            let why = format!("{mode} at call {k}");
            let inbox = e.remote_box("myserver").join("inbox");
            assert_eq!(jsons(&inbox), [format!("{out_id}.json")], "{why}");
            assert_eq!(
                all_files(&inbox)
                    .iter()
                    .filter(|f| f.starts_with(".tmp-"))
                    .count(),
                0,
                "{why}"
            );
            assert_eq!(
                jsons(&e.local("spool/myserver")),
                Vec::<String>::new(),
                "{why}"
            );
            let want: Vec<String> = ids.iter().map(|i| format!("{i}.json")).collect();
            assert_eq!(jsons(&e.local("inbox")), want, "{why}");
            let outbox = e.remote_box("myserver").join("outbox");
            assert_eq!(jsons(&outbox), Vec::<String>::new(), "{why}");
            assert_eq!(jsons(&outbox.join(".done")), want, "{why}");
            assert_eq!(
                all_files(&e.local("inbox"))
                    .iter()
                    .filter(|f| f.starts_with(".tmp-"))
                    .count(),
                0,
                "{why}"
            );
        }
    }
}
