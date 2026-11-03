//! `Vcs` port: the git operations the factory needs, and gate specs/results.

use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum VcsError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("working tree has uncommitted changes: {}", .0.join(", "))]
    Dirty(Vec<String>),
    #[error("branch not merged: {0}")]
    Unmerged(String),
    #[error("git: {0}")]
    Git(String),
    #[error("io: {0}")]
    Io(String),
}

impl VcsError {
    pub fn code(&self) -> &'static str {
        match self {
            VcsError::NotFound(_) => "not_found",
            VcsError::Dirty(_) => "dirty",
            VcsError::Unmerged(_) => "unmerged",
            VcsError::Git(_) => "git",
            VcsError::Io(_) => "io",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "lowercase")]
pub enum RebaseOutcome {
    Clean { head: String },
    Conflict { files: Vec<String> },
}

/// Git operations. Paths are absolute; `root` is the main checkout, `dir` any checkout/worktree.
pub trait Vcs {
    fn head(&self, dir: &Path) -> Result<String, VcsError>;
    fn current_branch(&self, dir: &Path) -> Result<String, VcsError>;
    /// Creates `path` on `branch` (new from `base`, or reused if it exists). Returns the base sha.
    fn worktree_add(&self, root: &Path, path: &Path, branch: &str, base: &str) -> Result<String, VcsError>;
    /// Refuses when the worktree is dirty or its branch is not merged into `base`. Never forces.
    fn worktree_remove(&self, root: &Path, path: &Path) -> Result<(), VcsError>;
    fn status_porcelain(&self, dir: &Path) -> Result<Vec<String>, VcsError>;
    /// Files changed by commits since `base` (merge-base), forward slashes.
    fn changed_files(&self, dir: &Path, base: &str) -> Result<Vec<String>, VcsError>;
    /// `git rebase <onto>`; on conflict aborts and reports the conflicting files.
    fn rebase(&self, dir: &Path, onto: &str) -> Result<RebaseOutcome, VcsError>;
    /// Fast-forwards the current branch of `root` to `branch`; returns the new head.
    fn ff_merge(&self, root: &Path, branch: &str) -> Result<String, VcsError>;
    fn ls_files(&self, root: &Path) -> Result<Vec<String>, VcsError>;
    /// `git push -u <remote> <branch>`; `force_with_lease` for rebased feature branches only.
    fn push(&self, dir: &Path, remote: &str, branch: &str, force_with_lease: bool) -> Result<(), VcsError>;
    /// `git pull --ff-only <remote> <branch>` on `dir` (its current branch); returns the new head.
    fn pull_ff(&self, dir: &Path, remote: &str, branch: &str) -> Result<String, VcsError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateSpec {
    pub name: String,
    pub cmd: Vec<String>,
    pub timeout_s: u64,
    pub shell: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateResult {
    pub name: String,
    pub ok: bool,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    /// Last lines of combined stdout/stderr.
    pub tail: String,
    /// How many times the gate was run; 0 = not run / unknown (legacy records).
    #[serde(default)]
    pub attempts: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_result_without_attempts_deserialises_to_zero() {
        let json = r#"{"name":"fmt","ok":true,"exit_code":0,"duration_ms":5,"tail":""}"#;
        let g: GateResult = serde_json::from_str(json).unwrap();
        assert_eq!(g.attempts, 0);
    }

    #[test]
    fn vcs_error_display_and_code() {
        let e = VcsError::Dirty(vec!["src/a.rs".into(), "b.txt".into()]);
        assert_eq!(e.code(), "dirty");
        assert!(e.to_string().contains("src/a.rs"));
        assert_eq!(VcsError::Git("boom".into()).code(), "git");
    }

    #[test]
    fn rebase_outcome_serde() {
        let c = RebaseOutcome::Conflict {
            files: vec!["x.rs".into()],
        };
        let s = serde_json::to_string(&c).unwrap();
        assert_eq!(s, r#"{"outcome":"conflict","files":["x.rs"]}"#);
        let k = RebaseOutcome::Clean { head: "abc".into() };
        assert_eq!(
            serde_json::to_string(&k).unwrap(),
            r#"{"outcome":"clean","head":"abc"}"#
        );
    }
}
