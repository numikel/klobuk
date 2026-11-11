//! Lane resolution: which issue a CLI invocation acts on behalf of.
//!
//! Precedence: explicit `--lane` > env `ASF_ISSUE` > cwd under `<root>/<worktrees_dir>/<id>` >
//! `orchestrator`. Pure path arithmetic, no filesystem access.
//!
//! A worker (a process with `ASF_ISSUE` set) is pinned to its own lane: `--lane` may repeat that
//! issue but never name `human`, `orchestrator` or another issue, because reviews and the scope
//! guard treat those authors as authoritative.

use std::path::{Component, Path};

use thiserror::Error;

use crate::ids::IssueId;
use crate::model::LANE_ORCHESTRATOR;

/// `--lane` named a lane other than the worker's own.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error(
    "a worker speaks only as its own lane: ASF_ISSUE={own}, so `--lane {asked}` is refused \
     (omit --lane; to answer or approve something, ask the operator with a QUESTION)"
)]
pub struct ForeignLane {
    pub own: String,
    pub asked: String,
}

/// Resolves the lane name. Returns `orchestrator` when nothing else applies.
///
/// `env_issue` is the worker's own issue (`ASF_ISSUE`, or the issue of the `ASF_RUN` run); when it
/// is set, an explicit lane must equal it.
pub fn resolve(
    explicit: Option<&str>,
    env_issue: Option<&str>,
    cwd: &Path,
    repo_root: &Path,
    worktrees_dir: &str,
) -> Result<String, ForeignLane> {
    let env_issue = env_issue.map(str::trim).filter(|s| !s.is_empty());
    if let Some(e) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return match env_issue {
            Some(own) if own != e => Err(ForeignLane {
                own: own.to_string(),
                asked: e.to_string(),
            }),
            _ => Ok(e.to_string()),
        };
    }
    if let Some(e) = env_issue {
        return Ok(e.to_string());
    }
    Ok(lane_from_cwd(cwd, repo_root, worktrees_dir).unwrap_or_else(|| LANE_ORCHESTRATOR.to_string()))
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

    fn ok(explicit: Option<&str>, env: Option<&str>, cwd: &str) -> String {
        resolve(explicit, env, Path::new(cwd), Path::new(ROOT), ".worktrees").unwrap()
    }

    #[test]
    fn explicit_wins_without_a_worker_env() {
        assert_eq!(ok(Some("asf-9"), None, "D:/repo/.worktrees/asf-2"), "asf-9");
        assert_eq!(ok(Some("human"), None, "D:/repo/.worktrees/asf-2"), "human");
        assert_eq!(ok(Some("orchestrator"), None, "D:/repo"), LANE_ORCHESTRATOR);
    }

    #[test]
    fn a_worker_may_name_its_own_lane() {
        assert_eq!(
            ok(Some("asf-7"), Some("asf-7"), "D:/repo/.worktrees/asf-7"),
            "asf-7"
        );
        assert_eq!(ok(Some(" asf-7 "), Some("asf-7"), "D:/repo"), "asf-7");
    }

    #[test]
    fn a_worker_cannot_speak_as_human_orchestrator_or_another_issue() {
        for asked in ["human", "orchestrator", "asf-9"] {
            let e = resolve(
                Some(asked),
                Some("asf-7"),
                Path::new("D:/repo/.worktrees/asf-7"),
                Path::new(ROOT),
                ".worktrees",
            )
            .unwrap_err();
            assert_eq!(e.own, "asf-7");
            assert_eq!(e.asked, asked);
            let msg = e.to_string();
            assert!(msg.contains("a worker speaks only as its own lane"), "{msg}");
            assert!(msg.contains("asf-7") && msg.contains(asked), "{msg}");
        }
    }

    #[test]
    fn env_beats_cwd() {
        assert_eq!(ok(None, Some("asf-1"), "D:/repo/.worktrees/asf-2"), "asf-1");
    }

    #[test]
    fn cwd_in_worktree() {
        assert_eq!(ok(None, None, "D:/repo/.worktrees/asf-3/src/x"), "asf-3");
    }

    #[test]
    fn cwd_in_worktrees_dir_but_not_an_id() {
        assert_eq!(ok(None, None, "D:/repo/.worktrees/scratch"), LANE_ORCHESTRATOR);
    }

    #[test]
    fn root_is_orchestrator() {
        assert_eq!(ok(None, None, "D:/repo/crates/asf"), LANE_ORCHESTRATOR);
    }

    #[test]
    fn empty_env_is_ignored() {
        assert_eq!(ok(None, Some(""), "D:/repo"), LANE_ORCHESTRATOR);
        // an empty ASF_ISSUE is no worker identity, so it restricts nothing
        assert_eq!(ok(Some("human"), Some("  "), "D:/repo"), "human");
    }

    #[test]
    fn outside_repo_is_orchestrator() {
        assert_eq!(ok(None, None, "C:/elsewhere/.worktrees/asf-3"), LANE_ORCHESTRATOR);
    }

    #[test]
    fn nested_worktrees_dir() {
        let l = resolve(
            None,
            None,
            Path::new("D:/repo/tmp/wt/asf-4"),
            Path::new(ROOT),
            "tmp/wt",
        )
        .unwrap();
        assert_eq!(l, "asf-4");
    }
}
