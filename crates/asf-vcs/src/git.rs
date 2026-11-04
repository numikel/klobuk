//! `GitCli`: shells out to `git`. Every operation is explicit and non-destructive by default:
//! no `--force`, no branch deletion, rebase conflicts are aborted and reported.

use std::path::Path;
use std::process::Command;

use asf_core::{RebaseOutcome, Vcs, VcsError};

pub struct GitCli;

fn run(dir: &Path, args: &[&str]) -> Result<String, VcsError> {
    if !dir.is_dir() {
        return Err(VcsError::NotFound(format!(
            "{} is not a directory",
            dir.display()
        )));
    }
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| VcsError::Io(format!("cannot run git: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        return Err(VcsError::Git(format!(
            "git {} (in {}): {}",
            args.join(" "),
            dir.display(),
            if stderr.is_empty() { stdout } else { stderr }
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

fn lines(s: &str) -> Vec<String> {
    s.lines()
        .map(|l| l.trim_end().replace('\\', "/"))
        .filter(|l| !l.is_empty())
        .collect()
}

/// Modified or staged tracked files that block integrating into `dir`, ignoring the factory's own
/// state under `.factory/` (the issue export changes with every status update).
fn blocking_dirty(dir: &Path) -> Result<Vec<String>, VcsError> {
    Ok(
        lines(&run(dir, &["status", "--porcelain", "--untracked-files=no"])?)
            .into_iter()
            .filter(|l| !l.get(3..).unwrap_or("").starts_with(".factory/"))
            .collect(),
    )
}

fn ref_exists(dir: &Path, r: &str) -> bool {
    run(dir, &["rev-parse", "--verify", "--quiet", r]).is_ok()
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// True when every commit of `branch` already has a patch-equivalent commit in `HEAD` (a rebase-merge or
/// cherry-pick rewrote the SHAs). `git cherry` marks unique commits with `+`; the `rev-list` guard keeps a branch
/// with nothing ahead of `HEAD` from being reported as merged by this path (an empty `git cherry` proves nothing).
fn merged_by_patch(root: &Path, branch: &str) -> Result<bool, VcsError> {
    let cherry = run(root, &["cherry", "HEAD", branch])?;
    if cherry.lines().any(|l| l.starts_with('+')) {
        return Ok(false);
    }
    let ahead = run(root, &["rev-list", "--count", &format!("HEAD..{branch}")])?;
    Ok(ahead.trim().parse::<u64>().map(|n| n > 0).unwrap_or(false))
}

impl Vcs for GitCli {
    fn head(&self, dir: &Path) -> Result<String, VcsError> {
        run(dir, &["rev-parse", "HEAD"])
    }

    fn current_branch(&self, dir: &Path) -> Result<String, VcsError> {
        run(dir, &["rev-parse", "--abbrev-ref", "HEAD"])
    }

    fn worktree_add(&self, root: &Path, path: &Path, branch: &str, base: &str) -> Result<String, VcsError> {
        if path.exists() {
            // Reuse a worktree that is already checked out on the requested branch (restart after abort).
            match self.current_branch(path) {
                Ok(b) if b == branch => return run(root, &["merge-base", branch, base]),
                Ok(b) => {
                    return Err(VcsError::Git(format!(
                        "{} already exists and is on branch {b}, not {branch}",
                        path.display()
                    )))
                }
                Err(_) => {
                    return Err(VcsError::Git(format!(
                        "{} already exists and is not a worktree",
                        path.display()
                    )))
                }
            }
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| VcsError::Io(e.to_string()))?;
        }
        let p = path_str(path);
        if ref_exists(root, &format!("refs/heads/{branch}")) {
            run(root, &["worktree", "add", &p, branch])?;
        } else {
            run(root, &["worktree", "add", "-b", branch, &p, base])?;
        }
        run(root, &["merge-base", branch, base])
    }

    fn worktree_remove(&self, root: &Path, path: &Path) -> Result<(), VcsError> {
        let dirty = self.status_porcelain(path)?;
        if !dirty.is_empty() {
            return Err(VcsError::Dirty(dirty));
        }
        let branch = self.current_branch(path)?;
        if run(root, &["merge-base", "--is-ancestor", &branch, "HEAD"]).is_err()
            && !merged_by_patch(root, &branch)?
        {
            return Err(VcsError::Unmerged(branch));
        }
        run(root, &["worktree", "remove", &path_str(path)])?;
        Ok(())
    }

    fn status_porcelain(&self, dir: &Path) -> Result<Vec<String>, VcsError> {
        Ok(lines(&run(dir, &["status", "--porcelain"])?))
    }

    fn changed_files(&self, dir: &Path, base: &str) -> Result<Vec<String>, VcsError> {
        let mb = run(dir, &["merge-base", base, "HEAD"])?;
        Ok(lines(&run(dir, &["diff", "--name-only", &mb, "HEAD"])?))
    }

    fn rebase(&self, dir: &Path, onto: &str) -> Result<RebaseOutcome, VcsError> {
        match run(dir, &["rebase", onto]) {
            Ok(_) => Ok(RebaseOutcome::Clean {
                head: self.head(dir)?,
            }),
            Err(VcsError::Git(_)) => {
                let files = lines(&run(dir, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default());
                // never leave a rebase in progress
                let _ = run(dir, &["rebase", "--abort"]);
                Ok(RebaseOutcome::Conflict { files })
            }
            Err(e) => Err(e),
        }
    }

    fn ff_merge(&self, root: &Path, branch: &str) -> Result<String, VcsError> {
        // Untracked files do not block a fast-forward; modified or staged tracked files do,
        // except the factory's own state files.
        let dirty = blocking_dirty(root)?;
        if !dirty.is_empty() {
            return Err(VcsError::Dirty(dirty));
        }
        run(root, &["merge", "--ff-only", branch])?;
        self.head(root)
    }

    fn ls_files(&self, root: &Path) -> Result<Vec<String>, VcsError> {
        let out = run(root, &["ls-files", "-z"])?;
        Ok(out
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(|s| s.replace('\\', "/"))
            .collect())
    }

    fn push(&self, dir: &Path, remote: &str, branch: &str, force_with_lease: bool) -> Result<(), VcsError> {
        let mut args = vec!["push", "-u"];
        if force_with_lease {
            args.push("--force-with-lease");
        }
        args.push(remote);
        args.push(branch);
        run(dir, &args).map(|_| ())
    }

    fn pull_ff(&self, dir: &Path, remote: &str, branch: &str) -> Result<String, VcsError> {
        let dirty = blocking_dirty(dir)?;
        if !dirty.is_empty() {
            return Err(VcsError::Dirty(dirty));
        }
        run(dir, &["pull", "--ff-only", remote, branch])?;
        self.head(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(cwd: &Path, args: &[&str]) {
        run(cwd, args).unwrap();
    }

    #[test]
    fn merged_by_patch_needs_commits_ahead_and_no_unique_patches() {
        let dir = tempfile::Builder::new()
            .prefix("asf-vcs-unit-")
            .tempdir()
            .unwrap();
        let root = dir.path();
        git(root, &["init", "-q", "-b", "main"]);
        git(root, &["config", "user.email", "t@x"]);
        git(root, &["config", "user.name", "t"]);
        std::fs::write(root.join("a.txt"), "a\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", "init"]);

        // zero commits ahead: git cherry prints nothing, but that must not count as merged
        git(root, &["branch", "empty", "main"]);
        assert!(!merged_by_patch(root, "empty").unwrap());

        // one unique commit: `+` line, not merged
        git(root, &["checkout", "-q", "-b", "lane"]);
        std::fs::write(root.join("c.txt"), "c\n").unwrap();
        git(root, &["add", "c.txt"]);
        git(root, &["commit", "-q", "-m", "c"]);
        git(root, &["checkout", "-q", "main"]);
        assert!(!merged_by_patch(root, "lane").unwrap());

        // main moves on first, so the cherry-pick below cannot reproduce the lane commit byte for
        // byte (same parent, tree, message and second-resolution timestamps would give the same SHA
        // and make `lane` an ancestor again)
        std::fs::write(root.join("b.txt"), "b\n").unwrap();
        git(root, &["add", "b.txt"]);
        git(root, &["commit", "-q", "-m", "main moves"]);
        assert!(!merged_by_patch(root, "lane").unwrap());

        // the same patch lands on main under a new SHA: merged
        git(root, &["cherry-pick", "lane"]);
        assert!(merged_by_patch(root, "lane").unwrap());
    }
}
