//! The only code that talks to another machine. Every remote command is the fixed script below,
//! run by `sh -c` over ssh with its arguments as quoted words (ids, a box path and a length that
//! were checked first), and message text on stdin, so nothing a message says reaches a shell.
//! The Mac always starts the connection; the remote needs a POSIX shell and a few base tools
//! (mkdir, mv, cat, rm, wc), and `find` to prune old markers.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// The default box, in the remote user's home.
pub const DEFAULT_BOX: &str = "~/.reeve/box";
/// Ends every reply, so a cut connection that still exits cleanly cannot pass for a whole one.
pub const END: &str = "@@reeve-end";

/// Argument 1 is the box path, 2 the operation, then the operation's own words. It has no single
/// quote in it, so it travels as one single-quoted word.
pub const SCRIPT: &str = r##"umask 077
box=$1; op=$2; shift 2
case $box in \~/*) box=$HOME/${box#\~/};; esac
here() { [ -d "$box/inbox" ] && [ -d "$box/outbox" ] && [ -d "$box/seen" ]; }
case $op in
init)
  mkdir -p "$box/inbox" "$box/outbox" "$box/seen" && echo ok
  ;;
check)
  for t in mkdir mv cat rm wc; do command -v $t >/dev/null 2>&1 || echo "missing $t"; done
  if here; then echo box-ok; else echo box-missing; fi
  n=0; for f in "$box"/outbox/*.json; do [ -f "$f" ] && n=$((n+1)); done; echo "outbox $n"
  n=0; for f in "$box"/inbox/*.json; do [ -f "$f" ] && n=$((n+1)); done; echo "inbox $n"
  ;;
push)
  id=$1; len=$2
  if ! here; then cat >/dev/null; echo nobox; echo @@reeve-end; exit 0; fi
  if [ -e "$box/seen/$id" ] || [ -e "$box/inbox/$id.json" ] || [ -e "$box/inbox/.read/$id.json" ]; then
    cat >/dev/null; : > "$box/seen/$id"; echo dup; echo @@reeve-end; exit 0
  fi
  tmp="$box/inbox/.tmp-$id"
  cat > "$tmp" || { rm -f "$tmp"; exit 1; }
  n=$(( $(wc -c < "$tmp") ))
  [ "$n" -eq "$len" ] || { rm -f "$tmp"; exit 1; }
  mv "$tmp" "$box/inbox/$id.json" && : > "$box/seen/$id" && echo ok
  ;;
list)
  find "$box/seen" "$box/outbox/.done" -type f -mtime +30 -exec rm -f {} + 2>/dev/null
  if ! here; then echo nobox; echo @@reeve-end; exit 0; fi
  if [ -f "$box/status.json" ]; then
    st=; IFS= read -r st < "$box/status.json"
    [ -n "$st" ] && echo "#status $st"
  fi
  n=0
  for f in "$box"/outbox/*.json; do
    [ -f "$f" ] || continue
    b=${f##*/}; echo "${b%.json}"
    n=$((n+1)); [ "$n" -ge 100 ] && break
  done
  ;;
fetch)
  [ -f "$box/outbox/$1.json" ] || exit 4
  cat "$box/outbox/$1.json"; echo
  ;;
ack|reject)
  if [ "$op" = ack ]; then to=.done; else to=.rejected; fi
  mkdir -p "$box/outbox/$to" && mv "$box/outbox/$1.json" "$box/outbox/$to/$1.json" 2>/dev/null
  [ -e "$box/outbox/$1.json" ] && exit 1
  echo ok
  ;;
*) echo "unknown op" >&2; exit 2 ;;
esac
echo @@reeve-end
"##;

/// `s` as one word for a POSIX shell.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// An ssh destination from the user's config: `host`, `user@host` or an alias. Never an option.
pub fn check_target(t: &str) -> Result<(), String> {
    let ok = !t.is_empty()
        && t.len() <= 255
        && !t.starts_with('-')
        && t.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._@:%[]-".contains(&c));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "ssh target {t:?} must be a host, user@host or ssh config alias, not an option"
        ))
    }
}

/// A box path: `~/…` or absolute, plain characters, no `..`.
pub fn check_box(b: &str) -> Result<(), String> {
    let ok = !b.is_empty()
        && b.len() <= 200
        && (b.starts_with("~/") || b.starts_with('/'))
        && b.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._/~-".contains(&c))
        && !b.split('/').any(|s| s == "..")
        && !b[1..].contains('~');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "box path {b:?} must start with ~/ or /, use only letters, digits and . _ - /, and not contain .."
        ))
    }
}

/// What ssh runs on the remote for one operation of the script.
pub fn remote_command(boxdir: &str, op: &str, args: &[&str]) -> String {
    let mut words = vec![
        "sh".to_string(),
        "-c".into(),
        sh_quote(SCRIPT),
        "reeve-box".into(),
        sh_quote(boxdir),
        sh_quote(op),
    ];
    words.extend(args.iter().map(|a| sh_quote(a)));
    words.join(" ")
}

#[derive(Debug)]
pub enum Fail {
    /// ssh could not connect or lost the connection (its exit 255), or did not start.
    Unreachable(String),
    Timeout(u64),
    /// The remote script ran and failed.
    Remote(i32),
}

/// A hard limit on one ssh, whatever the connection does. `REEVE_SSH_TIMEOUT` (seconds) changes it.
pub fn limit() -> Duration {
    let secs = std::env::var("REEVE_SSH_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(60);
    Duration::from_secs(secs)
}

fn first_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .chars()
        .take(200)
        .collect()
}

fn drain<R: Read + Send + 'static>(r: Option<R>) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buf = vec![];
        if let Some(mut r) = r {
            let _ = r.read_to_end(&mut buf);
        }
        buf
    })
}

/// Runs `command` on `target` with `stdin`, and returns what it printed once ssh exits 0.
pub fn run_command(
    target: &str,
    command: &str,
    stdin: &[u8],
    limit: Duration,
) -> Result<Vec<u8>, Fail> {
    check_target(target).map_err(Fail::Unreachable)?;
    let mut child = Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
            "--",
            target,
            command,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Fail::Unreachable(format!("could not start ssh: {e}")))?;
    let mut to_child = child.stdin.take();
    let input = stdin.to_vec();
    let writer = thread::spawn(move || {
        if let Some(mut w) = to_child.take() {
            let _ = w.write_all(&input);
        }
    });
    let (out_t, err_t) = (drain(child.stdout.take()), drain(child.stderr.take()));
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Ok(s),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(Fail::Timeout(limit.as_secs()));
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(e) => break Err(Fail::Unreachable(e.to_string())),
        }
    };
    let _ = writer.join();
    let (stdout, stderr) = (
        out_t.join().unwrap_or_default(),
        err_t.join().unwrap_or_default(),
    );
    let status = status?;
    match status.code() {
        Some(0) => Ok(stdout),
        Some(255) | None => Err(Fail::Unreachable(first_line(&stderr))),
        Some(c) => Err(Fail::Remote(c)),
    }
}

/// One operation of the box script. The reply is its lines, without the closing marker; a reply
/// that lacks the marker was cut short.
pub fn call(
    target: &str,
    boxdir: &str,
    op: &str,
    args: &[&str],
    stdin: &[u8],
    limit: Duration,
) -> Result<String, Fail> {
    let raw = run_command(target, &remote_command(boxdir, op, args), stdin, limit)?;
    frame(&String::from_utf8_lossy(&raw))
        .ok_or_else(|| Fail::Unreachable("the reply was cut short".into()))
}

/// A reply without its closing marker was cut short.
fn frame(text: &str) -> Option<String> {
    let body = text.trim_end_matches('\n').strip_suffix(END)?;
    (body.is_empty() || body.ends_with('\n')).then(|| body.to_string())
}

/// What went wrong, for the person at the terminal. ssh's own message is kept (cleaned), but a
/// remote script's stderr is not: it is text from the remote.
pub fn describe(f: &Fail) -> String {
    match f {
        Fail::Unreachable(d) if d.is_empty() => "ssh could not connect".into(),
        Fail::Unreachable(d) => crate::msg::clean(d),
        Fail::Timeout(s) => format!("ssh timed out after {s}s"),
        Fail::Remote(c) => format!("the box script failed (exit {c})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_word_per_argument_whatever_the_characters() {
        assert_eq!(sh_quote("abc"), "'abc'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(
            sh_quote("a b; rm -rf ~ $(x) `y`"),
            "'a b; rm -rf ~ $(x) `y`'"
        );
    }

    #[test]
    fn the_script_travels_as_one_single_quoted_word() {
        assert!(
            !SCRIPT.contains('\''),
            "a quote would need escaping on every call"
        );
        let cmd = remote_command("~/.reeve/box", "push", &["20261029T140311Z-9f3a1c", "120"]);
        assert!(cmd.starts_with("sh -c '"));
        assert!(cmd.ends_with(" reeve-box '~/.reeve/box' 'push' '20261029T140311Z-9f3a1c' '120'"));
    }

    #[test]
    fn targets_and_box_paths_that_could_be_options_or_shell_are_refused() {
        for ok in [
            "myserver",
            "me@myserver",
            "my-server.example.net",
            "user@[::1]",
            "h:22",
        ] {
            assert!(check_target(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "-oProxyCommand=x",
            "a b",
            "a;b",
            "a$(b)",
            "a\nb",
            "a`b`",
        ] {
            assert!(check_target(bad).is_err(), "{bad:?}");
        }
        for ok in ["~/.reeve/box", "/srv/reeve-box", "~/a/b_c.d"] {
            assert!(check_box(ok).is_ok(), "{ok}");
        }
        for bad in [
            "", "box", "~/../x", "/a/../b", "~/a b", "~/a;b", "~/$HOME", "~/a~b", "~",
        ] {
            assert!(check_box(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_reply_without_its_closing_marker_was_cut_short() {
        assert_eq!(frame("ok\n@@reeve-end\n"), Some("ok\n".into()));
        assert_eq!(frame("@@reeve-end\n"), Some(String::new()));
        assert_eq!(frame("ok\n"), None);
        assert_eq!(frame("ok\n@@reeve-e"), None);
    }
}
