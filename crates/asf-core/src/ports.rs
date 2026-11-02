//! Ports: the interfaces adapters implement.

use std::io::{BufRead, Write};

use serde::{Deserialize, Serialize};

use crate::error::{ClaimError, StoreError};
use crate::ids::IssueId;
use crate::model::*;

/// How `import_jsonl` treats existing rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportMode {
    /// Match on `uid`; the record with the newer `updated_at` wins. Deps and comments are idempotent.
    Upsert,
    /// Wipe issues, deps, comments and cursors first.
    Replace,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportReport {
    pub issues_created: usize,
    pub issues_updated: usize,
    pub deps: usize,
    pub comments: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportOpts {
    pub include_comments: bool,
}

impl Default for ExportOpts {
    fn default() -> Self {
        ExportOpts {
            include_comments: true,
        }
    }
}

/// The issue store port.
///
/// Transactional guarantees every implementation must honour:
/// - `create_issue` allocates the next id, inserts the issue and its deps in **one** write
///   transaction; an unknown dep or a cycle leaves the store untouched.
/// - `claim` is atomic across processes: of N concurrent claims on one issue exactly one succeeds,
///   the others get `ClaimError::AlreadyClaimed`.
/// - Every mutation appends an `events` row in the same transaction.
pub trait IssueStore: Send + Sync {
    fn repo(&self) -> Result<RepoInfo, StoreError>;

    fn create_issue(&self, n: &NewIssue) -> Result<Issue, StoreError>;
    fn get_issue(&self, id: &IssueId) -> Result<Option<Issue>, StoreError>;
    /// Ordered by sequence number.
    fn list_issues(&self, f: &IssueFilter) -> Result<Vec<Issue>, StoreError>;
    fn update_issue(&self, id: &IssueId, p: &IssuePatch) -> Result<Issue, StoreError>;
    /// `Validation` when the current status is terminal.
    fn set_status(&self, id: &IssueId, s: Status, actor: &str) -> Result<Issue, StoreError>;

    fn deps(&self) -> Result<Vec<Dep>, StoreError>;
    /// `Validation` on cycle or unknown issue.
    fn add_dep(&self, issue: &IssueId, on: &IssueId) -> Result<(), StoreError>;
    fn remove_dep(&self, issue: &IssueId, on: &IssueId) -> Result<(), StoreError>;
    /// Moves `backlog` issues whose deps are all `done` to `ready`; returns the promoted ids.
    fn promote_ready(&self) -> Result<Vec<IssueId>, StoreError>;

    fn claim(&self, id: &IssueId, c: &NewClaim) -> Result<Claim, ClaimError>;
    /// Deletes the claim; status `in-progress` goes back to `ready`.
    fn release(&self, id: &IssueId) -> Result<(), StoreError>;
    fn active_claims(&self) -> Result<Vec<Claim>, StoreError>;

    fn add_comment(&self, c: &NewComment) -> Result<Comment, StoreError>;
    fn comments(&self, id: &IssueId) -> Result<Vec<Comment>, StoreError>;
    /// Unread comments relevant to `lane` (see crate docs for the relevance rule).
    /// `peek` leaves the read cursor untouched; `all` ignores relevance but still honours the cursor.
    fn inbox(&self, lane: &str, peek: bool, all: bool) -> Result<Vec<Comment>, StoreError>;

    fn append_event(&self, e: &NewEvent) -> Result<u64, StoreError>;
    fn events_since(&self, seq: u64, limit: usize) -> Result<Vec<Event>, StoreError>;

    fn export_jsonl(&self, w: &mut dyn Write, o: &ExportOpts) -> Result<(), StoreError>;
    fn import_jsonl(&self, r: &mut dyn BufRead, m: ImportMode) -> Result<ImportReport, StoreError>;
}
