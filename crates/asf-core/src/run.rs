//! Worker runs and the merge queue model.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::ids::IssueId;
use crate::model::{str_enum, ParseEnumError};
use crate::vcs::GateResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Starting,
    Working,
    Blocked,
    Finishing,
    Verifying,
    Succeeded,
    Failed,
    Stalled,
    OverBudget,
    Aborted,
}
str_enum!(RunState, "run state", {
    Starting => "starting", Working => "working", Blocked => "blocked", Finishing => "finishing",
    Verifying => "verifying", Succeeded => "succeeded", Failed => "failed", Stalled => "stalled",
    OverBudget => "over_budget", Aborted => "aborted",
});

impl RunState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, RunState::Succeeded | RunState::Failed | RunState::Aborted)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub issue_id: IssueId,
    pub attempt: i64,
    pub runtime: String,
    pub agent_name: Option<String>,
    pub workspace_id: Option<String>,
    pub pane_id: Option<String>,
    pub claude_session_id: Option<String>,
    pub transcript_path: Option<String>,
    pub worktree: String,
    pub branch: String,
    pub base_sha: String,
    pub state: RunState,
    pub budget_usd: Option<f64>,
    pub cost_usd: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub turns: i64,
    pub tool_calls: i64,
    pub exit_reason: Option<String>,
    pub report_path: Option<String>,
    pub started_at: String,
    pub last_seen_at: Option<String>,
    pub finished_at: Option<String>,
    pub herdr_revision: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewRun {
    pub issue_id: IssueId,
    pub runtime: String,
    pub worktree: String,
    pub branch: String,
    pub base_sha: String,
    pub budget_usd: Option<f64>,
}

/// Partial update; `None` leaves the column untouched.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunPatch {
    pub state: Option<RunState>,
    pub agent_name: Option<String>,
    pub workspace_id: Option<String>,
    pub pane_id: Option<String>,
    pub claude_session_id: Option<String>,
    pub transcript_path: Option<String>,
    pub cost_usd: Option<f64>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub turns: Option<i64>,
    pub tool_calls: Option<i64>,
    pub exit_reason: Option<String>,
    pub report_path: Option<String>,
    pub last_seen_at: Option<String>,
    pub finished_at: Option<String>,
    pub herdr_revision: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeState {
    Queued,
    Rebasing,
    Gating,
    Reviewing,
    PrOpen,
    Merging,
    Merged,
    Failed,
    Blocked,
    Skipped,
}
str_enum!(MergeState, "merge state", {
    Queued => "queued", Rebasing => "rebasing", Gating => "gating", Reviewing => "reviewing", PrOpen => "pr_open",
    Merging => "merging", Merged => "merged", Failed => "failed", Blocked => "blocked", Skipped => "skipped",
});

impl MergeState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, MergeState::Merged | MergeState::Skipped)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeItem {
    pub id: i64,
    pub issue_id: IssueId,
    pub run_id: Option<String>,
    pub branch: String,
    pub state: MergeState,
    pub attempts: i64,
    pub head_sha: Option<String>,
    pub rebased_onto: Option<String>,
    pub pr_url: Option<String>,
    pub gate_results: Vec<GateResult>,
    pub last_error: Option<String>,
    pub enqueued_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MergePatch {
    pub state: Option<MergeState>,
    pub attempts: Option<i64>,
    pub head_sha: Option<String>,
    pub rebased_onto: Option<String>,
    pub pr_url: Option<String>,
    pub gate_results: Option<Vec<GateResult>>,
    pub last_error: Option<Option<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = include_str!("../../asf-store/src/schema.sql");

    /// Values of the `CHECK (col IN (...))` list that follows the first occurrence of `marker`.
    fn check_list(marker: &str) -> Vec<String> {
        let idx = SCHEMA.find(marker).expect("marker in schema");
        let rest = &SCHEMA[idx..];
        let start = rest.find("IN (").expect("CHECK IN") + 4;
        let end = rest[start..].find(')').unwrap() + start;
        rest[start..end]
            .split(',')
            .map(|s| s.trim().trim_matches('\'').to_string())
            .collect()
    }

    #[test]
    fn run_state_strings_match_schema() {
        let schema = check_list("CREATE TABLE runs");
        let ours: Vec<String> = RunState::ALL.iter().map(|s| s.to_string()).collect();
        assert_eq!(ours, schema);
        assert_eq!("over_budget".parse::<RunState>().unwrap(), RunState::OverBudget);
        assert_eq!(
            serde_json::to_string(&RunState::OverBudget).unwrap(),
            "\"over_budget\""
        );
        assert!(RunState::Succeeded.is_terminal());
        assert!(RunState::Aborted.is_terminal());
        assert!(!RunState::Blocked.is_terminal());
    }

    #[test]
    fn merge_state_strings_match_schema() {
        let schema = check_list("CREATE TABLE merge_queue");
        let ours: Vec<String> = MergeState::ALL.iter().map(|s| s.to_string()).collect();
        assert_eq!(ours, schema);
        assert_eq!(serde_json::to_string(&MergeState::PrOpen).unwrap(), "\"pr_open\"");
        assert!(MergeState::Merged.is_terminal());
        assert!(MergeState::Skipped.is_terminal());
        assert!(!MergeState::Blocked.is_terminal());
    }

    #[test]
    fn patches_default_to_no_change() {
        let p = RunPatch::default();
        assert!(p.state.is_none() && p.cost_usd.is_none() && p.tool_calls.is_none());
        let m = MergePatch::default();
        assert!(m.state.is_none() && m.last_error.is_none());
    }

    #[test]
    fn gate_result_serde() {
        let g = GateResult {
            name: "fmt".into(),
            ok: false,
            exit_code: Some(1),
            duration_ms: 12,
            tail: "x".into(),
            attempts: 1,
        };
        let s = serde_json::to_string(&g).unwrap();
        assert!(s.contains("\"name\":\"fmt\"") && s.contains("\"ok\":false"));
    }
}
