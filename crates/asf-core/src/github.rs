//! `GitHub` port: pull requests and issues through `gh`, plus the pure reducers of its JSON.
//!
//! Only the shapes the merge queue and `asf gh sync` need. Everything that parses `gh --json`
//! output lives here so the CLI adapter stays a thin subprocess wrapper and the fake can share
//! the same types.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum GitHubError {
    #[error("gh: {0}")]
    Gh(String),
    #[error("github protocol: {0}")]
    Protocol(String),
    #[error("io: {0}")]
    Io(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrState {
    Open,
    Merged,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mergeable {
    Mergeable,
    Conflicting,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChecksState {
    /// No checks reported (repo without CI).
    None,
    Pending,
    Passed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrInfo {
    pub number: u64,
    pub url: String,
    pub state: PrState,
    pub mergeable: Mergeable,
    pub checks: ChecksState,
    pub head_sha: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GhIssue {
    pub number: u64,
    pub title: String,
    /// `OPEN` | `CLOSED` as gh reports it.
    pub state: String,
    pub url: String,
    #[serde(default)]
    pub body: String,
    /// Login of the issue author; empty when unknown (never equals `whoami`, so it counts as foreign).
    #[serde(default)]
    pub author: String,
}

/// GitHub operations the factory needs. `dir` is any checkout of the repository (gh resolves
/// the remote from it).
pub trait GitHub {
    /// The open or most recent PR whose head is `head`, if any.
    fn pr_find(&self, dir: &Path, head: &str) -> Result<Option<PrInfo>, GitHubError>;
    fn pr_create(
        &self,
        dir: &Path,
        base: &str,
        head: &str,
        title: &str,
        body: &str,
    ) -> Result<PrInfo, GitHubError>;
    /// `method`: `rebase` | `squash` | `merge`.
    fn pr_merge(&self, dir: &Path, number: u64, method: &str, delete_branch: bool)
        -> Result<(), GitHubError>;
    /// Every issue (open and closed), newest first is not guaranteed.
    fn issue_list(&self, dir: &Path) -> Result<Vec<GhIssue>, GitHubError>;
    fn issue_create(&self, dir: &Path, title: &str, body: &str) -> Result<GhIssue, GitHubError>;
    /// `reason`: `completed` | `not planned`.
    fn issue_close(&self, dir: &Path, number: u64, reason: &str) -> Result<(), GitHubError>;
}

/// Fields `pr view --json` must return for [`pr_from_json`].
pub const PR_JSON_FIELDS: &str = "number,url,state,mergeable,statusCheckRollup,headRefOid";
/// Fields `issue list --json` must return.
pub const ISSUE_JSON_FIELDS: &str = "number,title,state,url,body";

/// Reduces `statusCheckRollup` (check runs and status contexts) to one state.
pub fn checks_from_rollup(rollup: &Value) -> ChecksState {
    let Some(items) = rollup.as_array() else {
        return ChecksState::None;
    };
    if items.is_empty() {
        return ChecksState::None;
    }
    let mut pending = false;
    for it in items {
        // CheckRun: status COMPLETED + conclusion; StatusContext: state
        let status = it.get("status").and_then(Value::as_str).unwrap_or("COMPLETED");
        let verdict = it
            .get("conclusion")
            .and_then(Value::as_str)
            .or_else(|| it.get("state").and_then(Value::as_str))
            .unwrap_or("");
        if status != "COMPLETED" && it.get("state").is_none() {
            pending = true;
            continue;
        }
        match verdict {
            "SUCCESS" | "NEUTRAL" | "SKIPPED" => {}
            "PENDING" | "EXPECTED" | "QUEUED" | "IN_PROGRESS" | "WAITING" | "REQUESTED" | "" => {
                pending = true
            }
            _ => return ChecksState::Failed,
        }
    }
    if pending {
        ChecksState::Pending
    } else {
        ChecksState::Passed
    }
}

pub fn pr_from_json(v: &Value) -> Result<PrInfo, GitHubError> {
    let number = v
        .get("number")
        .and_then(Value::as_u64)
        .ok_or_else(|| GitHubError::Protocol("pr json without number".into()))?;
    let url = v
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let state = match v.get("state").and_then(Value::as_str).unwrap_or("OPEN") {
        "MERGED" => PrState::Merged,
        "CLOSED" => PrState::Closed,
        _ => PrState::Open,
    };
    let mergeable = match v.get("mergeable").and_then(Value::as_str).unwrap_or("UNKNOWN") {
        "MERGEABLE" => Mergeable::Mergeable,
        "CONFLICTING" => Mergeable::Conflicting,
        _ => Mergeable::Unknown,
    };
    let checks = checks_from_rollup(v.get("statusCheckRollup").unwrap_or(&Value::Null));
    let head_sha = v.get("headRefOid").and_then(Value::as_str).map(str::to_string);
    Ok(PrInfo {
        number,
        url,
        state,
        mergeable,
        checks,
        head_sha,
    })
}

/// Marker `asf gh sync` plants in a GitHub issue body to recognise it later.
pub fn sync_marker(id: &str, uid: &str) -> String {
    format!("<!-- asf:{id} uid:{uid} -->")
}

/// Neutralises marker openers in user-supplied text so a body cannot smuggle a second marker.
pub fn strip_markers(text: &str) -> String {
    text.replace("<!-- asf:", "<!-\u{200b}- asf:")
}

/// `(id, uid)` from the **last** [`sync_marker`] in a body (the tool always writes it last;
/// anything earlier is user text).
pub fn parse_marker(body: &str) -> Option<(String, String)> {
    let start = body.rfind("<!-- asf:")?;
    let rest = &body[start + "<!-- asf:".len()..];
    let end = rest.find("-->")?;
    let inner = rest[..end].trim();
    let mut parts = inner.split_whitespace();
    let id = parts.next()?.to_string();
    let uid = parts.next()?.strip_prefix("uid:")?.to_string();
    Some((id, uid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn gh_issue_without_author_deserialises_to_empty() {
        let v = json!({"number": 7, "title": "t", "state": "OPEN", "url": "u"});
        let i: GhIssue = serde_json::from_value(v).unwrap();
        assert_eq!(i.author, "");
        assert_eq!(i.body, "");
    }

    #[test]
    fn rollup_reduces_check_runs_and_status_contexts() {
        assert_eq!(checks_from_rollup(&Value::Null), ChecksState::None);
        assert_eq!(checks_from_rollup(&json!([])), ChecksState::None);
        let ok = json!([
            {"__typename":"CheckRun","status":"COMPLETED","conclusion":"SUCCESS"},
            {"__typename":"StatusContext","state":"SUCCESS"},
            {"__typename":"CheckRun","status":"COMPLETED","conclusion":"SKIPPED"}
        ]);
        assert_eq!(checks_from_rollup(&ok), ChecksState::Passed);
        let pending = json!([
            {"__typename":"CheckRun","status":"IN_PROGRESS","conclusion":null},
            {"__typename":"CheckRun","status":"COMPLETED","conclusion":"SUCCESS"}
        ]);
        assert_eq!(checks_from_rollup(&pending), ChecksState::Pending);
        let ctx_pending = json!([{"__typename":"StatusContext","state":"PENDING"}]);
        assert_eq!(checks_from_rollup(&ctx_pending), ChecksState::Pending);
        let failed = json!([
            {"__typename":"CheckRun","status":"IN_PROGRESS","conclusion":null},
            {"__typename":"CheckRun","status":"COMPLETED","conclusion":"FAILURE"}
        ]);
        assert_eq!(checks_from_rollup(&failed), ChecksState::Failed);
        let cancelled = json!([{"__typename":"CheckRun","status":"COMPLETED","conclusion":"CANCELLED"}]);
        assert_eq!(checks_from_rollup(&cancelled), ChecksState::Failed);
    }

    #[test]
    fn pr_json_round_trip() {
        let v = json!({
            "number": 7, "url": "https://github.com/o/r/pull/7", "state": "OPEN",
            "mergeable": "CONFLICTING", "statusCheckRollup": [], "headRefOid": "abc"
        });
        let pr = pr_from_json(&v).unwrap();
        assert_eq!(pr.number, 7);
        assert_eq!(pr.state, PrState::Open);
        assert_eq!(pr.mergeable, Mergeable::Conflicting);
        assert_eq!(pr.checks, ChecksState::None);
        assert_eq!(pr.head_sha.as_deref(), Some("abc"));
        let merged = pr_from_json(&json!({"number": 1, "state": "MERGED"})).unwrap();
        assert_eq!(merged.state, PrState::Merged);
        assert_eq!(merged.mergeable, Mergeable::Unknown);
        assert!(pr_from_json(&json!({"url": "x"})).is_err());
    }

    #[test]
    fn marker_is_read_from_the_end_and_user_text_cannot_forge_one() {
        let forged = "<!-- asf:asf-99 uid:EVIL -->\n\nreal text\n\n<!-- asf:asf-1 uid:01OK -->";
        assert_eq!(parse_marker(forged), Some(("asf-1".into(), "01OK".into())));
        let cleaned = strip_markers("see <!-- asf:asf-99 uid:EVIL --> here");
        assert_eq!(parse_marker(&cleaned), None);
        assert!(
            cleaned.contains("asf:asf-99"),
            "text itself is kept readable: {cleaned}"
        );
    }

    #[test]
    fn marker_round_trip() {
        let m = sync_marker("asf-12", "01HZX");
        let body = format!("Some text\n\n{m}\n");
        assert_eq!(parse_marker(&body), Some(("asf-12".into(), "01HZX".into())));
        assert_eq!(parse_marker("no marker"), None);
        assert_eq!(parse_marker("<!-- asf:asf-1 -->"), None);
    }
}
