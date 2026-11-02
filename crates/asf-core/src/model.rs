//! Domain model shared by the store, scheduler and CLI.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::ids::IssueId;

/// Lane name of the orchestrator session (sees every comment).
pub const LANE_ORCHESTRATOR: &str = "orchestrator";
/// Pseudo-lane for the human operator.
pub const LANE_HUMAN: &str = "human";
/// Broadcast address for comments.
pub const LANE_ALL: &str = "all";

/// Current UTC time as an RFC 3339 string (`2026-11-02T10:00:00Z`).
pub fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .replace_nanosecond(0)
        .expect("zero nanoseconds is valid")
        .format(&Rfc3339)
        .expect("RFC 3339 formatting cannot fail for UTC")
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("unknown {kind} `{value}`")]
pub struct ParseEnumError {
    pub kind: &'static str,
    pub value: String,
}

/// Defines `FromStr`/`Display` for a fieldless enum using the given string forms.
macro_rules! str_enum {
    ($name:ident, $kind:literal, { $($variant:ident => $s:literal),+ $(,)? }) => {
        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn as_str(&self) -> &'static str {
                match self { $($name::$variant => $s),+ }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = ParseEnumError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($s => Ok($name::$variant),)+
                    _ => Err(ParseEnumError { kind: $kind, value: s.to_string() }),
                }
            }
        }
    };
}

pub(crate) use str_enum;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IssueType {
    Task,
    Review,
    Epic,
}
str_enum!(IssueType, "issue type", { Task => "task", Review => "review", Epic => "epic" });

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Backlog,
    Ready,
    InProgress,
    Blocked,
    InReview,
    Done,
    Cancelled,
}
str_enum!(Status, "status", {
    Backlog => "backlog", Ready => "ready", InProgress => "in-progress", Blocked => "blocked",
    InReview => "in-review", Done => "done", Cancelled => "cancelled",
});

impl Status {
    /// Terminal statuses cannot transition anywhere else.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Status::Done | Status::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum CommentType {
    Question,
    Answer,
    Decision,
    Handoff,
    Blocker,
    Status,
    Fyi,
}
str_enum!(CommentType, "comment type", {
    Question => "QUESTION", Answer => "ANSWER", Decision => "DECISION", Handoff => "HANDOFF",
    Blocker => "BLOCKER", Status => "STATUS", Fyi => "FYI",
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Acceptance {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Issue {
    pub id: IssueId,
    pub uid: String,
    pub title: String,
    pub body: String,
    #[serde(rename = "type")]
    pub kind: IssueType,
    pub status: Status,
    pub priority: i64,
    pub parent_id: Option<IssueId>,
    pub file_scope: Vec<String>,
    pub acceptance: Vec<Acceptance>,
    pub estimate: Option<String>,
    pub labels: Vec<String>,
    pub source: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewIssue {
    pub title: String,
    pub body: String,
    pub kind: IssueType,
    pub status: Status,
    pub priority: i64,
    pub parent_id: Option<IssueId>,
    pub file_scope: Vec<String>,
    pub acceptance: Vec<Acceptance>,
    pub estimate: Option<String>,
    pub labels: Vec<String>,
    pub source: Option<String>,
    pub deps: Vec<IssueId>,
}

impl Default for NewIssue {
    fn default() -> Self {
        NewIssue {
            title: String::new(),
            body: String::new(),
            kind: IssueType::Task,
            status: Status::Backlog,
            priority: 2,
            parent_id: None,
            file_scope: Vec::new(),
            acceptance: Vec::new(),
            estimate: None,
            labels: Vec::new(),
            source: None,
            deps: Vec::new(),
        }
    }
}

/// Partial update; `None` leaves a field untouched. Double options clear a nullable field with `Some(None)`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct IssuePatch {
    pub title: Option<String>,
    pub body: Option<String>,
    pub priority: Option<i64>,
    pub file_scope: Option<Vec<String>>,
    pub acceptance: Option<Vec<Acceptance>>,
    pub estimate: Option<Option<String>>,
    pub labels: Option<Vec<String>>,
    pub parent_id: Option<Option<IssueId>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Dep {
    pub issue_id: IssueId,
    pub depends_on: IssueId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    pub issue_id: IssueId,
    pub worker: String,
    pub run_id: Option<String>,
    pub worktree: Option<String>,
    pub branch: Option<String>,
    pub claimed_at: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NewClaim {
    pub worker: String,
    pub run_id: Option<String>,
    pub worktree: Option<String>,
    pub branch: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    pub id: i64,
    pub issue_id: IssueId,
    pub author: String,
    #[serde(rename = "type")]
    pub kind: CommentType,
    pub to_lane: String,
    pub body: String,
    pub reply_to: Option<i64>,
    pub resolved_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewComment {
    pub issue_id: IssueId,
    pub author: String,
    pub kind: CommentType,
    pub to_lane: String,
    pub body: String,
    pub reply_to: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    pub ts: String,
    pub kind: String,
    pub issue_id: Option<IssueId>,
    pub run_id: Option<String>,
    pub actor: Option<String>,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewEvent {
    pub kind: String,
    pub issue_id: Option<IssueId>,
    pub run_id: Option<String>,
    pub actor: Option<String>,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepoInfo {
    pub id: String,
    pub root: String,
    pub id_prefix: String,
    pub next_seq: i64,
    pub default_branch: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct IssueFilter {
    pub status: Option<Status>,
    pub kind: Option<IssueType>,
    pub label: Option<String>,
    pub parent: Option<IssueId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_serde_kebab() {
        assert_eq!(
            serde_json::to_string(&Status::InProgress).unwrap(),
            "\"in-progress\""
        );
        let s: Status = serde_json::from_str("\"in-review\"").unwrap();
        assert_eq!(s, Status::InReview);
        assert_eq!("blocked".parse::<Status>().unwrap(), Status::Blocked);
        assert_eq!(Status::Done.to_string(), "done");
        assert!(Status::Done.is_terminal());
        assert!(Status::Cancelled.is_terminal());
        assert!(!Status::Ready.is_terminal());
    }

    #[test]
    fn issue_type_serde_lower() {
        assert_eq!(serde_json::to_string(&IssueType::Review).unwrap(), "\"review\"");
        assert_eq!("epic".parse::<IssueType>().unwrap(), IssueType::Epic);
        assert!("Epic".parse::<IssueType>().is_err());
    }

    #[test]
    fn comment_type_serde_upper() {
        assert_eq!(serde_json::to_string(&CommentType::Fyi).unwrap(), "\"FYI\"");
        assert_eq!("HANDOFF".parse::<CommentType>().unwrap(), CommentType::Handoff);
        assert_eq!(CommentType::Question.to_string(), "QUESTION");
    }

    #[test]
    fn new_issue_default_is_backlog_priority_2() {
        let n = NewIssue::default();
        assert_eq!(n.status, Status::Backlog);
        assert_eq!(n.priority, 2);
        assert_eq!(n.kind, IssueType::Task);
        assert!(n.deps.is_empty());
    }

    #[test]
    fn now_is_rfc3339_utc() {
        let s = now_rfc3339();
        assert!(s.ends_with('Z'), "{s}");
        assert!(s.len() >= 20);
    }
}
