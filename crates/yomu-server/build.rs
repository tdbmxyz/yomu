//! Expose the build commit for the About page. Nix builds have no .git
//! directory: the flake injects YOMU_BUILD_COMMIT (see flake.nix); dev and
//! local (e.g. Android) builds fall back to asking git. Absent both, the
//! About page just shows the version.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=YOMU_BUILD_COMMIT");
    let commit = std::env::var("YOMU_BUILD_COMMIT")
        .ok()
        .or_else(git_commit)
        .filter(|c| !c.is_empty());
    if let Some(commit) = commit {
        println!("cargo:rustc-env=YOMU_BUILD_COMMIT={commit}");
    }
}

/// Short HEAD commit, re-run when it changes. Watching `.git/HEAD` alone is
/// not enough: a commit on the same branch updates `.git/refs/heads/<branch>`
/// (and, once packed, `.git/packed-refs`) while HEAD's contents stay put, so
/// the baked hash would go stale. Watch the resolved ref too.
fn git_commit() -> Option<String> {
    let git = git_dir()?;
    let head = git.join("HEAD");
    println!("cargo:rerun-if-changed={}", head.display());
    println!(
        "cargo:rerun-if-changed={}",
        git.join("packed-refs").display()
    );
    if let Ok(contents) = std::fs::read_to_string(&head)
        && let Some(reference) = contents.strip_prefix("ref:").map(str::trim)
    {
        println!("cargo:rerun-if-changed={}", git.join(reference).display());
    }
    let out = Command::new("git")
        .args(["rev-parse", "--short=9", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Walk up from the crate to the repository's `.git` directory. Returns None
/// for a git worktree (`.git` is a file) or when there is no repo at all —
/// both fall back to the injected env var or no commit.
fn git_dir() -> Option<PathBuf> {
    let mut dir: PathBuf = std::env::var("CARGO_MANIFEST_DIR").ok()?.into();
    loop {
        let candidate = dir.join(".git");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}
