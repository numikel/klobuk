//! Exports `ASF_GIT_SHA` (short commit of the checkout) so `asf --version` shows which build it is.
//! std only; `unknown` when git is missing or this is not a git checkout.

use std::path::Path;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Rebuild when the checked-out commit moves: `HEAD` (branch switch), the branch ref (new commit)
/// and `packed-refs` (ref packed by gc). `--git-path` resolves each of them inside a worktree too.
fn watch_git() {
    let Some(head) = git(&["rev-parse", "--git-path", "HEAD"]) else {
        return;
    };
    let mut paths = vec![head.clone()];
    if let Some(target) = std::fs::read_to_string(&head)
        .ok()
        .and_then(|s| s.trim().strip_prefix("ref: ").map(str::to_string))
    {
        paths.extend(git(&["rev-parse", "--git-path", &target]));
    }
    paths.extend(git(&["rev-parse", "--git-path", "packed-refs"]));
    for path in paths.into_iter().filter(|p| Path::new(p).exists()) {
        println!("cargo:rerun-if-changed={path}");
    }
}

fn main() {
    // Once any rerun-if-changed is emitted cargo stops scanning the package, so name this file too.
    println!("cargo:rerun-if-changed=build.rs");
    watch_git();
    let sha = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=ASF_GIT_SHA={sha}");
}
