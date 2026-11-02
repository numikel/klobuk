//! Thin git subprocess helpers (read-only in M1).

use std::path::{Path, PathBuf};
use std::process::Command;

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
