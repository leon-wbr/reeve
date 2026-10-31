use std::process::Command;

fn version() -> String {
    let o = Command::new(env!("CARGO_BIN_EXE_reeve"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(o.status.success());
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

/// `reeve 0.1.0 (271c42c 2026-10-31)`: the build's commit and date, `-dirty` after the commit when
/// the tree had changes. A build outside a git checkout prints just `reeve 0.1.0`.
#[test]
fn the_version_names_the_commit_and_date_the_build_came_from() {
    let v = version();
    let pkg = env!("CARGO_PKG_VERSION");
    let rest = v
        .strip_prefix(&format!("reeve {pkg}"))
        .unwrap_or_else(|| panic!("unexpected version line: {v}"));
    if rest.is_empty() {
        return; // built from a tarball
    }
    let inner = rest
        .strip_prefix(" (")
        .and_then(|r| r.strip_suffix(')'))
        .unwrap_or_else(|| panic!("unexpected version line: {v}"));
    let (commit, date) = inner.split_once(' ').expect("commit and date");
    let hex = commit.strip_suffix("-dirty").unwrap_or(commit);
    assert!(
        hex.len() == 7 && hex.chars().all(|c| c.is_ascii_hexdigit()),
        "{v}"
    );
    let parts: Vec<&str> = date.split('-').collect();
    assert!(
        parts.len() == 3 && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit())),
        "{v}"
    );
}

#[test]
fn a_checkout_build_records_its_commit() {
    // The test runs from a git checkout of reeve in CI and in a worktree; a tarball skips it.
    if !std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".git")
        .exists()
    {
        return;
    }
    assert!(version().contains(" ("), "{}", version());
}
