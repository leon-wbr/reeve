//! The harness for the remote tests. The remote side is a fake `ssh` on PATH that runs the command
//! locally against a temp HOME per target, and can fail on a plan: before running anything, after
//! running it (the answer is lost), with its input cut short, or by hanging. Nothing here reaches
//! a real host.
#![allow(dead_code)]

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

pub struct Env {
    dir: tempfile::TempDir,
}

impl Env {
    pub fn new() -> Env {
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

    pub fn p(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    pub fn reeve_env<'a>(&self, c: &'a mut Command) -> &'a mut Command {
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

    pub fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_reeve"));
        c.args(args);
        self.reeve_env(&mut c);
        c
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    /// Runs reeve and requires success; returns stdout.
    pub fn ok(&self, args: &[&str]) -> String {
        let o = self.run(args);
        assert!(
            o.status.success(),
            "reeve {args:?}\nstdout: {}\nstderr: {}",
            out(&o),
            String::from_utf8_lossy(&o.stderr)
        );
        out(&o)
    }

    pub fn down(&self, on: bool) {
        let f = self.p("fake/down");
        if on {
            fs::write(f, "").unwrap();
        } else {
            let _ = fs::remove_file(f);
        }
    }

    /// What the fake does on its next calls, one word per call (`ok` once the plan runs out).
    pub fn plan(&self, modes: &[&str]) {
        fs::write(self.p("fake/plan"), modes.join("\n") + "\n").unwrap();
    }

    /// The remote's own HOME for an ssh target.
    pub fn remote_home(&self, target: &str) -> PathBuf {
        self.p("fake/homes").join(target)
    }

    pub fn remote_box(&self, target: &str) -> PathBuf {
        self.remote_home(target).join(".reeve/box")
    }

    pub fn calls(&self) -> String {
        fs::read_to_string(self.p("fake/calls.log")).unwrap_or_default()
    }

    pub fn add_remote(&self, name: &str) {
        self.ok(&["remote", "add", name, name]);
    }

    pub fn local(&self, rel: &str) -> PathBuf {
        self.p("reeve-home").join(rel)
    }
}

pub fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

pub fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// File names (not dotfiles) ending in `.json` in `dir`, sorted.
pub fn jsons(dir: &Path) -> Vec<String> {
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

pub fn all_files(dir: &Path) -> Vec<String> {
    pub fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
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

pub fn message(id: &str, from: &str, body: &str) -> Value {
    json!({"v": 1, "id": id, "from": from, "to": "me", "at": "2026-10-29T14:03:11Z", "body": body})
}

/// What a remote agent does: drop a finished message file in the box's outbox.
pub fn outbox_put(env: &Env, target: &str, m: &Value) {
    let dir = env.remote_box(target).join("outbox");
    fs::create_dir_all(&dir).unwrap();
    let id = m["id"].as_str().unwrap();
    fs::write(dir.join(format!("{id}.json")), m.to_string()).unwrap();
}

pub fn last_id(stdout: &str) -> String {
    stdout.split_whitespace().nth(1).unwrap().to_string()
}

pub fn events(env: &Env) -> String {
    fs::read_to_string(env.p("state/reeve/events.jsonl")).unwrap_or_default()
}
