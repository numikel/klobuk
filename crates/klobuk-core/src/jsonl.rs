//! Records of the committable `.factory/issues.jsonl` export.
//!
//! One JSON object per line, LF-terminated, in this order: `meta`, issues by sequence number,
//! deps by `(issue, depends_on)`, comments by `(issue, n)`. `exported_at` in `meta` is the only
//! field that changes between two exports of the same store.

use serde::{Deserialize, Serialize};

use crate::ids::IssueId;
use crate::model::{Acceptance, CommentType, IssueType, Status};

/// Current file format version.
pub const FORMAT: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Record {
    Meta {
        format: u32,
        prefix: String,
        next_seq: i64,
        exported_at: String,
    },
    Issue(IssueRecord),
    Dep {
        issue: IssueId,
        depends_on: IssueId,
    },
    Comment(CommentRecord),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IssueRecord {
    pub id: IssueId,
    pub uid: String,
    pub title: String,
    pub body: String,
    #[serde(rename = "type")]
    pub kind: IssueType,
    pub status: Status,
    pub priority: i64,
    pub parent: Option<IssueId>,
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
pub struct CommentRecord {
    pub issue: IssueId,
    /// 1-based position of the comment within its issue (export order).
    pub n: i64,
    pub author: String,
    #[serde(rename = "type")]
    pub kind: CommentType,
    pub to: String,
    pub body: String,
    /// `n` of the comment this one replies to, when it is on the same issue.
    pub reply_to_n: Option<i64>,
    pub created_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_serde_shapes() {
        let meta = Record::Meta {
            format: FORMAT,
            prefix: "asf".into(),
            next_seq: 3,
            exported_at: "2026-11-02T10:00:00Z".into(),
        };
        assert_eq!(
            serde_json::to_string(&meta).unwrap(),
            r#"{"kind":"meta","format":1,"prefix":"asf","next_seq":3,"exported_at":"2026-11-02T10:00:00Z"}"#
        );
        let dep = Record::Dep {
            issue: "asf-2".parse().unwrap(),
            depends_on: "asf-1".parse().unwrap(),
        };
        assert_eq!(
            serde_json::to_string(&dep).unwrap(),
            r#"{"kind":"dep","issue":"asf-2","depends_on":"asf-1"}"#
        );

        let issue = Record::Issue(IssueRecord {
            id: "asf-1".parse().unwrap(),
            uid: "01J".into(),
            title: "t".into(),
            body: String::new(),
            kind: IssueType::Task,
            status: Status::Ready,
            priority: 2,
            parent: None,
            file_scope: vec![],
            acceptance: vec![],
            estimate: None,
            labels: vec![],
            source: None,
            created_at: "c".into(),
            updated_at: "u".into(),
            closed_at: None,
        });
        let s = serde_json::to_string(&issue).unwrap();
        assert!(
            s.starts_with(r#"{"kind":"issue","id":"asf-1","uid":"01J""#),
            "{s}"
        );
        assert!(s.contains(r#""type":"task""#), "{s}");
        let back: Record = serde_json::from_str(&s).unwrap();
        assert_eq!(back, issue);

        let c = Record::Comment(CommentRecord {
            issue: "asf-1".parse().unwrap(),
            n: 1,
            author: "human".into(),
            kind: CommentType::Fyi,
            to: "all".into(),
            body: "hi".into(),
            reply_to_n: None,
            created_at: "c".into(),
        });
        let s = serde_json::to_string(&c).unwrap();
        assert!(s.starts_with(r#"{"kind":"comment","issue":"asf-1","n":1"#), "{s}");
        assert!(s.contains(r#""type":"FYI""#), "{s}");
    }

    #[test]
    fn unknown_kind_is_error() {
        assert!(serde_json::from_str::<Record>(r#"{"kind":"bogus"}"#).is_err());
    }
}
