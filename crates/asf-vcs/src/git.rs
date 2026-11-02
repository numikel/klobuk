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

fn ref_exists(dir: &Path, r: &str) -> bool {
    run(dir, &["rev-parse", "--verify", "--quiet", r]).is_ok()
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
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
            return Err(VcsError::Git(format!("{} already exists", path.display())));
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
        if run(root, &["merge-base", "--is-ancestor", &branch, "HEAD"]).is_err() {
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
        // Untracked files do not block a fast-forward; modified or staged tracked files do.
        let dirty = lines(&run(root, &["status", "--porcelain", "--untracked-files=no"])?);
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
}
