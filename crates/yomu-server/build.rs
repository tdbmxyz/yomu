//! Expose the build commit for the About page. Nix builds have no .git
//! directory: the flake injects YOMU_BUILD_COMMIT instead (see flake.nix);
//! dev builds fall back to asking git. Absent both, the About page just
//! shows the version.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=YOMU_BUILD_COMMIT");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    let commit = std::env::var("YOMU_BUILD_COMMIT").ok().or_else(|| {
        Command::new("git")
            .args(["rev-parse", "--short=9", "HEAD"])
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
    });
    if let Some(commit) = commit.filter(|c| !c.is_empty()) {
        println!("cargo:rustc-env=YOMU_BUILD_COMMIT={commit}");
    }
}
