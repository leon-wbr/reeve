//! What `build.rs` recorded about the checkout this binary was built from. All of it is absent
//! for a crates.io or tarball build.

use std::path::PathBuf;

/// `0.1.0 (271c42c 2026-10-31)`, or just `0.1.0` outside a git checkout.
pub const LONG_VERSION: &str = env!("REEVE_VERSION");

/// The full commit this binary was built from.
pub fn commit() -> Option<&'static str> {
    option_env!("REEVE_GIT_SHA")
}

/// The main checkout of the repository it was built in (a linked worktree's main one).
pub fn source() -> Option<PathBuf> {
    option_env!("REEVE_SOURCE").map(PathBuf::from)
}
