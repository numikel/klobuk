//! Thin git subprocess helpers (read-only in M1).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::output::AppError;

fn run(cwd: &Path, args: &[&str]) -> Result<String, AppError> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| AppError::io(format!("cannot run git: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(AppError::not_found(format!(
            "git {} failed in {}: {stderr}",
            args.join(" "),
            cwd.display()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `git diff <base>...<branch>` piped into `git patch-id --stable`: a fingerprint of what the branch
/// changes relative to its merge-base with `base`, independent of commit SHAs, line offsets and of
/// where `base` has moved to. `None` when the branch changes nothing (`patch-id` prints no line).
///
/// The diff is passed on byte-for-byte (no trimming): `patch-id` hashes it, and `--stable` makes the
/// result independent of the order of files in the diff.
pub fn patch_id(cwd: &Path, base: &str, branch: &str) -> Result<Option<String>, AppError> {
    let range = format!("{base}...{branch}");
    let diff = Command::new("git")
        .args(["diff", &range])
        .current_dir(cwd)
        .output()
        .map_err(|e| AppError::io(format!("cannot run git: {e}")))?;
    if !diff.status.success() {
        return Err(AppError::not_found(format!(
            "git diff {range} failed in {}: {}",
            cwd.display(),
            String::from_utf8_lossy(&diff.stderr).trim()
        )));
    }
    let mut child = Command::new("git")
        .args(["patch-id", "--stable"])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| AppError::io(format!("cannot run git: {e}")))?;
    // feed stdin from a thread: a large diff must not deadlock against patch-id's full stdout pipe
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| AppError::io("git patch-id has no stdin".to_string()))?;
    let feeder = std::thread::spawn(move || stdin.write_all(&diff.stdout));
    let out = child
        .wait_with_output()
        .map_err(|e| AppError::io(format!("git patch-id failed: {e}")))?;
    // a broken pipe here means patch-id already stopped reading; its exit status tells the rest
    let _ = feeder.join();
    if !out.status.success() {
        return Err(AppError::not_found(format!(
            "git patch-id --stable failed for {range}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    // output is `<patch-id> <commit-id>`; for a bare diff the commit id is all zeros
    Ok(String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .map(str::to_string))
}

/// The shared `.git` directory (for a worktree: the main repository's `.git`).
pub fn common_dir(start: &Path) -> Result<PathBuf, AppError> {
    let s = run(
        start,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .map_err(|_| AppError::not_found(format!("{} is not inside a git repository", start.display())))?;
    Ok(PathBuf::from(s))
}

/// Repository root = parent of the common git dir. Works from the root, any subdirectory or a worktree.
pub fn locate_root(start: &Path) -> Result<PathBuf, AppError> {
    let common = common_dir(start)?;
    let root = common.parent().ok_or_else(|| {
        AppError::not_found(format!("cannot derive repository root from {}", common.display()))
    })?;
    Ok(root.to_path_buf())
}

/// Tracked files relative to the root, forward slashes (as `git ls-files` prints them).
pub fn tracked_files(root: &Path) -> Result<Vec<String>, AppError> {
    let s = run(root, &["ls-files", "-z"])?;
    Ok(s.split('\0')
        .filter(|x| !x.is_empty())
        .map(str::to_string)
        .collect())
}
