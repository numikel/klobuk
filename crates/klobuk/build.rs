//! Exports `KLOBUK_GIT_SHA` (short commit of the build) so `klobuk --version` shows which build it is.
//! Source order: `.cargo_vcs_info.json` (written by `cargo package`, so a crates.io install has no git
//! checkout), then git, then `unknown`. std only; no source ever fails the build.
//! `tests/cli_version.rs` includes this file as a module to test `resolve_sha` and `sha_from_vcs_info`.

use std::path::Path;
use std::process::Command;

/// File `cargo package` puts into the package root; holds the commit the package was made from.
pub const VCS_INFO: &str = ".cargo_vcs_info.json";
/// Same floor `git rev-parse --short` uses by default.
const SHORT_LEN: usize = 7;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Short commit from the `git.sha1` field of `.cargo_vcs_info.json`
/// (`{"git":{"sha1":"<hex>","dirty":true},"path_in_vcs":""}`). `None` for anything unexpected.
pub fn sha_from_vcs_info(json: &str) -> Option<String> {
    // sha1 is plain hex, so a key scan is enough; start at "git" so another VCS block cannot match.
    let after_git = &json[json.find("\"git\"")? + "\"git\"".len()..];
    let after_key = after_git[after_git.find("\"sha1\"")? + "\"sha1\"".len()..]
        .trim_start()
        .strip_prefix(':')?
        .trim_start()
        .strip_prefix('"')?;
    let sha = &after_key[..after_key.find('"')?];
    (sha.len() >= SHORT_LEN && sha.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| sha[..SHORT_LEN].to_string())
}

/// Picks the commit: a parsable vcs info wins and git is never asked; otherwise git; otherwise `unknown`.
pub fn resolve_sha(vcs_info: Option<&str>, git_sha: impl FnOnce() -> Option<String>) -> String {
    vcs_info
        .and_then(sha_from_vcs_info)
        .or_else(git_sha)
        .unwrap_or_else(|| "unknown".to_string())
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

#[allow(dead_code)] // the test module that includes this file has its own entry point
fn main() {
    // Once any rerun-if-changed is emitted cargo stops scanning the package, so name this file too.
    println!("cargo:rerun-if-changed=build.rs");
    // A missing file is skipped: cargo would treat it as always changed and rerun on every build.
    let vcs_info = std::fs::read_to_string(VCS_INFO).ok();
    if Path::new(VCS_INFO).exists() {
        println!("cargo:rerun-if-changed={VCS_INFO}");
    }
    // Git is only consulted (and watched) when the file does not settle the commit.
    if vcs_info.as_deref().and_then(sha_from_vcs_info).is_none() {
        watch_git();
    }
    let sha = resolve_sha(vcs_info.as_deref(), || git(&["rev-parse", "--short", "HEAD"]));
    println!("cargo:rustc-env=KLOBUK_GIT_SHA={sha}");
}
