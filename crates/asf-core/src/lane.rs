//! Lane resolution: which issue a CLI invocation acts on behalf of.
//!
//! Precedence: explicit `--lane` > env `ASF_ISSUE` > cwd under `<root>/<worktrees_dir>/<id>` >
//! `orchestrator`. Pure path arithmetic, no filesystem access.

use std::path::{Component, Path};

use crate::ids::IssueId;
use crate::model::LANE_ORCHESTRATOR;

/// Resolves the lane name. Returns `orchestrator` when nothing else applies.
pub fn resolve(
    explicit: Option<&str>,
    env_issue: Option<&str>,
    cwd: &Path,
    repo_root: &Path,
    worktrees_dir: &str,
) -> String {
    if let Some(e) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return e.to_string();
    }
    if let Some(e) = env_issue.map(str::trim).filter(|s| !s.is_empty()) {
        return e.to_string();
    }
    lane_from_cwd(cwd, repo_root, worktrees_dir).unwrap_or_else(|| LANE_ORCHESTRATOR.to_string())
}

fn lane_from_cwd(cwd: &Path, repo_root: &Path, worktrees_dir: &str) -> Option<String> {
    let rel = cwd.strip_prefix(repo_root).ok()?;
    let mut comps = rel.components().filter_map(|c| match c {
        Component::Normal(s) => s.to_str(),
        _ => None,
    });
    // worktrees_dir may itself contain separators ("tmp/wt"); compare component by component.
    for want in Path::new(worktrees_dir).components().filter_map(|c| match c {
        Component::Normal(s) => s.to_str(),
        _ => None,
    }) {
        if comps.next()? != want {
            return None;
        }
    }
    let candidate = comps.next()?;
    candidate.parse::<IssueId>().ok().map(|id| id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const ROOT: &str = "D:/repo";

    #[test]
    fn explicit_wins() {
        let l = resolve(
            Some("asf-9"),
            Some("asf-1"),
            Path::new("D:/repo/.worktrees/asf-2"),
            Path::new(ROOT),
            ".worktrees",
        );
        assert_eq!(l, "asf-9");
    }

    #[test]
    fn env_beats_cwd() {
        let l = resolve(
            None,
            Some("asf-1"),
            Path::new("D:/repo/.worktrees/asf-2"),
            Path::new(ROOT),
            ".worktrees",
        );
        assert_eq!(l, "asf-1");
    }

    #[test]
    fn cwd_in_worktree() {
        let l = resolve(
            None,
            None,
            Path::new("D:/repo/.worktrees/asf-3/src/x"),
            Path::new(ROOT),
            ".worktrees",
        );
        assert_eq!(l, "asf-3");
    }

    #[test]
    fn cwd_in_worktrees_dir_but_not_an_id() {
        let l = resolve(
            None,
            None,
            Path::new("D:/repo/.worktrees/scratch"),
            Path::new(ROOT),
            ".worktrees",
        );
        assert_eq!(l, LANE_ORCHESTRATOR);
    }

    #[test]
    fn root_is_orchestrator() {
        let l = resolve(
            None,
            None,
            Path::new("D:/repo/crates/asf"),
            Path::new(ROOT),
            ".worktrees",
        );
        assert_eq!(l, LANE_ORCHESTRATOR);
    }

    #[test]
    fn empty_env_is_ignored() {
        let l = resolve(
            None,
            Some(""),
            Path::new("D:/repo"),
            Path::new(ROOT),
            ".worktrees",
        );
        assert_eq!(l, LANE_ORCHESTRATOR);
    }

    #[test]
    fn outside_repo_is_orchestrator() {
        let l = resolve(
            None,
            None,
            Path::new("C:/elsewhere/.worktrees/asf-3"),
            Path::new(ROOT),
            ".worktrees",
        );
        assert_eq!(l, LANE_ORCHESTRATOR);
    }

    #[test]
    fn nested_worktrees_dir() {
        let l = resolve(
            None,
            None,
            Path::new("D:/repo/tmp/wt/asf-4"),
            Path::new(ROOT),
            "tmp/wt",
        );
        assert_eq!(l, "asf-4");
    }
}
