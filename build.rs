//! Records which git commit this build came from, for `reeve --version`, `reeve update` and
//! `reeve doctor`. Outside a git checkout of reeve itself (a crates.io or tarball build, or
//! no git installed) it records nothing, and the version is the plain package version.

use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The commit, its date, whether the tree has changes and the repository's main checkout, when
/// `dir` is itself the top of a git checkout (not a folder that merely sits inside some other
/// repository, such as a cargo registry under a git-managed home). `dir` may be a linked
/// worktree; the main checkout is where `reeve update` fetches.
fn checkout(dir: &Path) -> Option<(String, String, bool, PathBuf)> {
    let top = PathBuf::from(git(dir, &["rev-parse", "--show-toplevel"])?);
    if top.canonicalize().ok()? != dir.canonicalize().ok()? {
        return None;
    }
    let sha = git(dir, &["rev-parse", "HEAD"])?;
    let date = git(dir, &["log", "-1", "--format=%cs"])?;
    let dirty = !git(dir, &["status", "--porcelain"])?.is_empty();
    let common = git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let main = Path::new(&common).parent()?.to_path_buf();
    Some((sha, date, dirty, main))
}

fn main() {
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let mut long = version;
    if let Some((sha, date, dirty, main)) = checkout(&dir) {
        let short = &sha[..sha.len().min(7)];
        let flag = if dirty { "-dirty" } else { "" };
        long = format!("{long} ({short}{flag} {date})");
        println!("cargo:rustc-env=REEVE_GIT_SHA={sha}");
        println!("cargo:rustc-env=REEVE_SOURCE={}", main.display());
        // Rebuild when the commit moves or the index changes; edits to the files are covered by
        // watching the sources.
        for p in ["HEAD", "index", "refs"] {
            if let Some(path) = git(&dir, &["rev-parse", "--git-path", p]) {
                println!("cargo:rerun-if-changed={path}");
            }
        }
        if let Some(head) = git(&dir, &["symbolic-ref", "-q", "HEAD"])
            && let Some(path) = git(&dir, &["rev-parse", "--git-path", &head])
        {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    println!("cargo:rustc-env=REEVE_VERSION={long}");
    for p in ["build.rs", "Cargo.toml", "Cargo.lock", "src", "docs"] {
        println!("cargo:rerun-if-changed={p}");
    }
}
