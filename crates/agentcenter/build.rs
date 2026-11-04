//! Build stamp for the About dialog: the build day and the git commit.
//! `SOURCE_DATE_EPOCH` wins; otherwise the commit's own date, so the stamp
//! moves with the code; the clock only when there is no git.

use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn git(args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn main() {
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    // Rerun when HEAD moves: HEAD itself, the branch it points at, packed refs.
    // `--git-path` resolves them inside a linked worktree too.
    for path in ["HEAD", "packed-refs"] {
        if let Some(p) = git(&["rev-parse", "--git-path", path]) {
            println!("cargo:rerun-if-changed={}", PathBuf::from(p).display());
        }
    }
    if let Some(r) =
        git(&["symbolic-ref", "-q", "HEAD"]).and_then(|r| git(&["rev-parse", "--git-path", &r]))
    {
        // A ref packed by `git gc` has no loose file; watching a missing file
        // would rerun this script, and rebuild the crate, on every build.
        let r = PathBuf::from(r);
        if r.exists() {
            println!("cargo:rerun-if-changed={}", r.display());
        }
    }
    let epoch = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .or_else(|| git(&["log", "-1", "--format=%ct"]).and_then(|v| v.parse().ok()))
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs())
        });
    let rev = git(&["rev-parse", "--short=7", "HEAD"])
        .filter(|s| s.len() == 7 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .unwrap_or_default();
    println!("cargo:rustc-env=AGENTCENTER_BUILD_EPOCH={epoch}");
    println!("cargo:rustc-env=AGENTCENTER_GIT_REV={rev}");
}
