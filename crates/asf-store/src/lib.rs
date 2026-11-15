//! SQLite implementation of the `asf_core::IssueStore` port.
//!
//! One `Connection` per store, guarded by a mutex because `rusqlite` transactions need `&mut`.
//! Every write runs in an `IMMEDIATE` transaction so concurrent CLI processes queue on the
//! database lock (WAL + `busy_timeout`) instead of failing with a deferred-lock upgrade.

mod jsonl;
mod row;
mod runs;
pub mod serve_pulse;

use std::io::{BufRead, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use asf_core::scheduler;
use asf_core::*;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

const SCHEMA_SQL: &str = include_str!("schema.sql");
/// Schema version written by `schema.sql` and produced by the migration; `asf doctor` expects it.
pub const SCHEMA_VERSION: i64 = 2;
const BUSY_TIMEOUT: Duration = Duration::from_secs(10);

pub struct SqliteStore {
    conn: Mutex<Connection>,
}

pub(crate) fn backend(e: rusqlite::Error) -> StoreError {
    StoreError::Backend(e.to_string())
}

/// Lets `with_tx` carry a `ClaimError` out of a transaction without losing the `StoreError` path.
enum ClaimErrorOrStore {
    Claim(ClaimError),
    Store(StoreError),
}

impl From<StoreError> for ClaimErrorOrStore {
    fn from(e: StoreError) -> Self {
        ClaimErrorOrStore::Store(e)
    }
}

impl From<ClaimError> for ClaimErrorOrStore {
    fn from(e: ClaimError) -> Self {
        ClaimErrorOrStore::Claim(e)
    }
}

pub(crate) fn is_constraint(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation)
}

impl SqliteStore {
    /// Opens (creating if needed) the database at `path`, applies pragmas and migrations.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| StoreError::Io(format!("{}: {e}", parent.display())))?;
        }
        let conn = Connection::open(path).map_err(backend)?;
        Self::from_connection(conn)
    }

    /// In-memory store for tests.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::from_connection(Connection::open_in_memory().map_err(backend)?)
    }

    fn from_connection(conn: Connection) -> Result<Self, StoreError> {
        conn.busy_timeout(BUSY_TIMEOUT).map_err(backend)?;
        // journal_mode returns a row, so pragma_update would error on ":memory:" if unsupported; ignore value.
        let _: String = conn
            .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
            .map_err(backend)?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(backend)?;
        conn.pragma_update(None, "foreign_keys", "ON").map_err(backend)?;
        let store = SqliteStore {
            conn: Mutex::new(conn),
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<(), StoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let has_meta: bool = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='meta'",
                [],
                |r| r.get::<_, i64>(0).map(|n| n > 0),
            )
            .map_err(backend)?;
        if !has_meta {
            return conn
                .execute_batch(&format!("BEGIN IMMEDIATE;\n{SCHEMA_SQL}\nCOMMIT;"))
                .map_err(backend);
        }
        match read_schema_version(&conn)? {
            SCHEMA_VERSION => Ok(()),
            1 => migrate_v1_to_v2(&conn),
            v => Err(StoreError::Backend(format!(
                "unsupported schema version {v} (this build supports 1..={SCHEMA_VERSION})"
            ))),
        }
    }

    pub fn schema_version(&self) -> Result<i64, StoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        read_schema_version(&conn)
    }

    pub fn journal_mode(&self) -> Result<String, StoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .map_err(backend)
    }

    /// Registers the repository this database belongs to. Exactly one repo per database.
    pub fn init_repo(&self, root: &str, prefix: &str, default_branch: &str) -> Result<RepoInfo, StoreError> {
        if !is_valid_prefix(prefix) {
            return Err(StoreError::Validation(format!("invalid prefix `{prefix}`")));
        }
        self.with_tx(|tx| {
            let existing: Option<String> =
                tx.query_row("SELECT id FROM repos LIMIT 1", [], |r| r.get(0)).optional().map_err(backend)?;
            if existing.is_some() {
                return Err(StoreError::Conflict("repository already initialised in this store".into()));
            }
            let now = now_rfc3339();
            let id = ulid::Ulid::generate().to_string();
            tx.execute(
                "INSERT INTO repos (id, root, id_prefix, next_seq, default_branch, created_at) VALUES (?1, ?2, ?3, 1, ?4, ?5)",
                params![id, root, prefix, default_branch, now],
            )
            .map_err(backend)?;
            row::repo(tx)
        })
    }

    /// Runs `f` inside an IMMEDIATE transaction, committing on `Ok`.
    pub(crate) fn with_tx<T, E>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T, E>) -> Result<T, E>
    where
        E: From<StoreError>,
    {
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| E::from(backend(e)))?;
        let out = f(&tx)?;
        tx.commit().map_err(|e| E::from(backend(e)))?;
        Ok(out)
    }

    /// Read-only access to the connection.
    pub(crate) fn read<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        f(&conn)
    }
}

fn read_schema_version(conn: &Connection) -> Result<i64, StoreError> {
    conn.query_row("SELECT value FROM meta WHERE key='schema_version'", [], |r| {
        r.get::<_, String>(0)
    })
    .map_err(backend)?
    .parse()
    .map_err(|e| StoreError::Backend(format!("schema_version: {e}")))
}

/// v1 -> v2 rebuilds `issues` because SQLite cannot alter a CHECK constraint in place. `issues_new`
/// carries the widened `status` CHECK; every other column is copied verbatim. `issues` has no indexes
/// of its own (only the inline PRIMARY KEY / UNIQUE constraints, which the new table declares too).
const MIGRATE_V1_TO_V2_SQL: &str = "
CREATE TABLE issues_new (
  id         TEXT PRIMARY KEY,
  uid        TEXT NOT NULL UNIQUE,
  title      TEXT NOT NULL,
  body       TEXT NOT NULL DEFAULT '',
  type       TEXT NOT NULL CHECK (type IN ('task','review','epic')),
  status     TEXT NOT NULL CHECK (status IN ('backlog','ready','in-progress','blocked','in-review','validating','done','cancelled')),
  priority   INTEGER NOT NULL DEFAULT 2,
  parent_id  TEXT REFERENCES issues(id),
  file_scope TEXT NOT NULL DEFAULT '[]',
  acceptance TEXT NOT NULL DEFAULT '[]',
  estimate   TEXT,
  labels     TEXT NOT NULL DEFAULT '[]',
  source     TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  closed_at  TEXT
);
INSERT INTO issues_new (id, uid, title, body, type, status, priority, parent_id, file_scope, acceptance,
                        estimate, labels, source, created_at, updated_at, closed_at)
  SELECT id, uid, title, body, type, status, priority, parent_id, file_scope, acceptance,
         estimate, labels, source, created_at, updated_at, closed_at FROM issues;
DROP TABLE issues;
ALTER TABLE issues_new RENAME TO issues;
UPDATE meta SET value = '2' WHERE key = 'schema_version';
";

/// Foreign keys stay OFF for the whole rebuild window: `PRAGMA foreign_keys` is a no-op inside a
/// transaction, and with them ON `DROP TABLE issues` would run an implicit `DELETE` that trips every
/// child table (`comments`, `runs`, `claims`, ...). They are switched back ON even when the rebuild fails.
fn migrate_v1_to_v2(conn: &Connection) -> Result<(), StoreError> {
    conn.pragma_update(None, "foreign_keys", "OFF").map_err(backend)?;
    let rebuilt = rebuild_issues_v2(conn);
    let restored = conn.pragma_update(None, "foreign_keys", "ON").map_err(backend);
    rebuilt.and(restored)
}

fn rebuild_issues_v2(conn: &Connection) -> Result<(), StoreError> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate).map_err(backend)?;
    // Another process may have finished the migration while this one waited for the write lock.
    if read_schema_version(&tx)? != 1 {
        return Ok(());
    }
    tx.execute_batch(MIGRATE_V1_TO_V2_SQL).map_err(backend)?;
    // Dropping `tx` on this error path rolls the whole rebuild back.
    let dangling: i64 = tx
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get(0))
        .map_err(backend)?;
    if dangling > 0 {
        return Err(StoreError::Backend(format!(
            "schema migration 1 -> 2 would leave {dangling} dangling foreign key reference(s)"
        )));
    }
    tx.commit().map_err(backend)
}

fn validate_globs(scope: &[String]) -> Result<(), StoreError> {
    scheduler::validate_scope(scope).map_err(StoreError::Validation)
}

fn require_issue(tx: &Transaction<'_>, id: &IssueId) -> Result<Issue, StoreError> {
    row::issue_by_id(tx, id)?.ok_or_else(|| StoreError::NotFound(format!("issue {id}")))
}

pub(crate) fn append_event_tx(tx: &Transaction<'_>, e: &NewEvent) -> Result<u64, StoreError> {
    tx.execute(
        "INSERT INTO events (ts, kind, issue_id, run_id, actor, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            now_rfc3339(),
            e.kind,
            e.issue_id.as_ref().map(|i| i.as_str()),
            e.run_id,
            e.actor,
            e.payload.to_string()
        ],
    )
    .map_err(backend)?;
    Ok(tx.last_insert_rowid() as u64)
}

fn event(kind: &str, issue: &IssueId, actor: Option<&str>, payload: serde_json::Value) -> NewEvent {
    NewEvent {
        kind: kind.to_string(),
        issue_id: Some(issue.clone()),
        run_id: None,
        actor: actor.map(str::to_string),
        payload,
    }
}

impl IssueStore for SqliteStore {
    fn repo(&self) -> Result<RepoInfo, StoreError> {
        self.read(row::repo)
    }

    fn create_issue(&self, n: &NewIssue) -> Result<Issue, StoreError> {
        if n.title.trim().is_empty() {
            return Err(StoreError::Validation("title must not be empty".into()));
        }
        validate_globs(&n.file_scope)?;
        self.with_tx(|tx| {
            for d in &n.deps {
                require_issue(tx, d).map_err(|_| StoreError::Validation(format!("unknown dependency {d}")))?;
            }
            if let Some(p) = &n.parent_id {
                require_issue(tx, p).map_err(|_| StoreError::Validation(format!("unknown parent {p}")))?;
            }
            let repo = row::repo(tx)?;
            let id = IssueId::new(&repo.id_prefix, repo.next_seq);
            tx.execute("UPDATE repos SET next_seq = next_seq + 1 WHERE id = ?1", params![repo.id]).map_err(backend)?;
            let now = now_rfc3339();
            let uid = ulid::Ulid::generate().to_string();
            let closed_at = n.status.is_terminal().then(|| now.clone());
            tx.execute(
                "INSERT INTO issues (id, uid, title, body, type, status, priority, parent_id, file_scope, acceptance,
                 estimate, labels, source, created_at, updated_at, closed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14, ?15)",
                params![
                    id.as_str(),
                    uid,
                    n.title.trim(),
                    n.body,
                    n.kind.as_str(),
                    n.status.as_str(),
                    n.priority,
                    n.parent_id.as_ref().map(|p| p.as_str()),
                    serde_json::to_string(&n.file_scope).map_err(|e| StoreError::Backend(e.to_string()))?,
                    serde_json::to_string(&n.acceptance).map_err(|e| StoreError::Backend(e.to_string()))?,
                    n.estimate,
                    serde_json::to_string(&n.labels).map_err(|e| StoreError::Backend(e.to_string()))?,
                    n.source,
                    now,
                    closed_at,
                ],
            )
            .map_err(backend)?;
            for d in &n.deps {
                tx.execute(
                    "INSERT OR IGNORE INTO issue_deps (issue_id, depends_on) VALUES (?1, ?2)",
                    params![id.as_str(), d.as_str()],
                )
                .map_err(backend)?;
            }
            append_event_tx(tx, &event("issue.created", &id, None, serde_json::json!({ "title": n.title.trim() })))?;
            require_issue(tx, &id)
        })
    }

    fn get_issue(&self, id: &IssueId) -> Result<Option<Issue>, StoreError> {
        self.read(|c| row::issue_by_id(c, id))
    }

    fn list_issues(&self, f: &IssueFilter) -> Result<Vec<Issue>, StoreError> {
        self.read(|c| row::list_issues(c, f))
    }

    fn update_issue(&self, id: &IssueId, p: &IssuePatch) -> Result<Issue, StoreError> {
        if let Some(scope) = &p.file_scope {
            validate_globs(scope)?;
        }
        if let Some(t) = &p.title {
            if t.trim().is_empty() {
                return Err(StoreError::Validation("title must not be empty".into()));
            }
        }
        self.with_tx(|tx| {
            let cur = require_issue(tx, id)?;
            if let Some(Some(parent)) = &p.parent_id {
                require_issue(tx, parent)
                    .map_err(|_| StoreError::Validation(format!("unknown parent {parent}")))?;
            }
            let title = p.title.as_deref().map(str::trim).unwrap_or(&cur.title);
            let body = p.body.as_deref().unwrap_or(&cur.body);
            let priority = p.priority.unwrap_or(cur.priority);
            let file_scope = p.file_scope.as_ref().unwrap_or(&cur.file_scope);
            let acceptance = p.acceptance.as_ref().unwrap_or(&cur.acceptance);
            let estimate = p.estimate.clone().unwrap_or(cur.estimate.clone());
            let labels = p.labels.as_ref().unwrap_or(&cur.labels);
            let parent_id = p.parent_id.clone().unwrap_or(cur.parent_id.clone());
            tx.execute(
                "UPDATE issues SET title=?2, body=?3, priority=?4, file_scope=?5, acceptance=?6, estimate=?7,
                 labels=?8, parent_id=?9, updated_at=?10 WHERE id=?1",
                params![
                    id.as_str(),
                    title,
                    body,
                    priority,
                    serde_json::to_string(file_scope).map_err(|e| StoreError::Backend(e.to_string()))?,
                    serde_json::to_string(acceptance).map_err(|e| StoreError::Backend(e.to_string()))?,
                    estimate,
                    serde_json::to_string(labels).map_err(|e| StoreError::Backend(e.to_string()))?,
                    parent_id.as_ref().map(|p| p.as_str()),
                    now_rfc3339(),
                ],
            )
            .map_err(backend)?;
            append_event_tx(tx, &event("issue.updated", id, None, serde_json::json!({})))?;
            require_issue(tx, id)
        })
    }

    fn set_status(&self, id: &IssueId, s: Status, actor: &str) -> Result<Issue, StoreError> {
        self.with_tx(|tx| {
            let cur = require_issue(tx, id)?;
            if cur.status.is_terminal() && cur.status != s {
                return Err(StoreError::Conflict(format!(
                    "issue {id} is {} and cannot change status",
                    cur.status
                )));
            }
            let now = now_rfc3339();
            let closed_at = s.is_terminal().then(|| now.clone());
            tx.execute(
                "UPDATE issues SET status=?2, closed_at=?3, updated_at=?4 WHERE id=?1",
                params![id.as_str(), s.as_str(), closed_at, now],
            )
            .map_err(backend)?;
            append_event_tx(
                tx,
                &event(
                    "issue.status",
                    id,
                    Some(actor),
                    serde_json::json!({ "from": cur.status.as_str(), "to": s.as_str() }),
                ),
            )?;
            require_issue(tx, id)
        })
    }

    fn deps(&self) -> Result<Vec<Dep>, StoreError> {
        self.read(row::deps)
    }

    fn add_dep(&self, issue: &IssueId, on: &IssueId) -> Result<(), StoreError> {
        if issue == on {
            return Err(StoreError::Validation(format!("{issue} cannot depend on itself")));
        }
        self.with_tx(|tx| {
            require_issue(tx, issue)?;
            require_issue(tx, on)?;
            let issues = row::list_issues(tx, &IssueFilter::default())?;
            let mut deps = row::deps(tx)?;
            deps.push(Dep {
                issue_id: issue.clone(),
                depends_on: on.clone(),
            });
            scheduler::validate_dag(&issues, &deps).map_err(|e| StoreError::Validation(e.to_string()))?;
            tx.execute(
                "INSERT OR IGNORE INTO issue_deps (issue_id, depends_on) VALUES (?1, ?2)",
                params![issue.as_str(), on.as_str()],
            )
            .map_err(backend)?;
            append_event_tx(
                tx,
                &event("dep.added", issue, None, serde_json::json!({ "on": on.as_str() })),
            )?;
            // a ready issue must not wait on open work: send it back so no tick can dispatch it
            let on_open = issues.iter().any(|i| &i.id == on && i.status != Status::Done);
            if on_open && issues.iter().any(|i| &i.id == issue && i.status == Status::Ready) {
                let mut waits_for: Vec<&str> = Vec::new();
                for d in deps.iter().filter(|d| &d.issue_id == issue) {
                    let open = issues
                        .iter()
                        .any(|i| i.id == d.depends_on && i.status != Status::Done);
                    if open && !waits_for.contains(&d.depends_on.as_str()) {
                        waits_for.push(d.depends_on.as_str());
                    }
                }
                tx.execute(
                    "UPDATE issues SET status='backlog', updated_at=?2 WHERE id=?1 AND status='ready'",
                    params![issue.as_str(), now_rfc3339()],
                )
                .map_err(backend)?;
                append_event_tx(
                    tx,
                    &event(
                        "issue.demoted",
                        issue,
                        None,
                        serde_json::json!({ "from": "ready", "to": "backlog", "waits_for": waits_for }),
                    ),
                )?;
            }
            Ok(())
        })
    }

    fn remove_dep(&self, issue: &IssueId, on: &IssueId) -> Result<(), StoreError> {
        self.with_tx(|tx| {
            let n = tx
                .execute(
                    "DELETE FROM issue_deps WHERE issue_id=?1 AND depends_on=?2",
                    params![issue.as_str(), on.as_str()],
                )
                .map_err(backend)?;
            if n == 0 {
                return Err(StoreError::NotFound(format!("dependency {issue} -> {on}")));
            }
            append_event_tx(
                tx,
                &event(
                    "dep.removed",
                    issue,
                    None,
                    serde_json::json!({ "on": on.as_str() }),
                ),
            )?;
            Ok(())
        })
    }

    fn promote_ready(&self) -> Result<Vec<IssueId>, StoreError> {
        self.with_tx(|tx| {
            let issues = row::list_issues(tx, &IssueFilter::default())?;
            let deps = row::deps(tx)?;
            let promoted = scheduler::promotable(&issues, &deps);
            let now = now_rfc3339();
            for id in &promoted {
                tx.execute(
                    "UPDATE issues SET status='ready', updated_at=?2 WHERE id=?1 AND status='backlog'",
                    params![id.as_str(), now],
                )
                .map_err(backend)?;
                append_event_tx(
                    tx,
                    &event(
                        "issue.status",
                        id,
                        Some("scheduler"),
                        serde_json::json!({ "from": "backlog", "to": "ready" }),
                    ),
                )?;
            }
            Ok(promoted)
        })
    }

    fn close_completed_epics(&self) -> Result<Vec<IssueId>, StoreError> {
        self.with_tx(|tx| {
            let issues = row::list_issues(tx, &IssueFilter::default())?;
            let closed = scheduler::completable_epics(&issues);
            let now = now_rfc3339();
            for id in &closed {
                let from = issues
                    .iter()
                    .find(|i| &i.id == id)
                    .map(|i| i.status.as_str())
                    .unwrap_or("backlog");
                tx.execute(
                    "UPDATE issues SET status='done', updated_at=?2, closed_at=?2
                     WHERE id=?1 AND status NOT IN ('done','cancelled')",
                    params![id.as_str(), now],
                )
                .map_err(backend)?;
                append_event_tx(
                    tx,
                    &event(
                        "issue.status",
                        id,
                        Some("scheduler"),
                        serde_json::json!({ "from": from, "to": "done", "reason": "children-complete" }),
                    ),
                )?;
            }
            Ok(closed)
        })
    }

    fn claim(&self, id: &IssueId, c: &NewClaim) -> Result<Claim, ClaimError> {
        if c.worker.trim().is_empty() {
            return Err(ClaimError::Store(StoreError::Validation(
                "worker must not be empty".into(),
            )));
        }
        self.with_tx(|tx| -> Result<Claim, ClaimErrorOrStore> {
            let issue = require_issue(tx, id)?;
            if let Some(cur) = row::claim_by_issue(tx, id)? {
                return Err(ClaimError::AlreadyClaimed { by: cur.worker }.into());
            }
            if issue.status != Status::Ready {
                return Err(ClaimError::NotReady { status: issue.status }.into());
            }
            let now = now_rfc3339();
            tx.execute(
                "INSERT INTO claims (issue_id, worker, run_id, worktree, branch, claimed_at, note)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    id.as_str(),
                    c.worker.trim(),
                    c.run_id,
                    c.worktree,
                    c.branch,
                    now,
                    c.note
                ],
            )
            .map_err(|e| {
                if is_constraint(&e) {
                    StoreError::Conflict(format!("issue {id} already claimed"))
                } else {
                    backend(e)
                }
            })?;
            tx.execute(
                "UPDATE issues SET status='in-progress', updated_at=?2 WHERE id=?1",
                params![id.as_str(), now],
            )
            .map_err(backend)?;
            append_event_tx(
                tx,
                &event(
                    "claim.taken",
                    id,
                    Some(c.worker.trim()),
                    serde_json::json!({ "worker": c.worker.trim() }),
                ),
            )?;
            row::claim_by_issue(tx, id)?
                .ok_or_else(|| ClaimErrorOrStore::Store(StoreError::Backend("claim vanished".into())))
        })
        .map_err(|e| match e {
            ClaimErrorOrStore::Claim(c) => c,
            ClaimErrorOrStore::Store(s) => ClaimError::Store(s),
        })
    }

    fn release(&self, id: &IssueId) -> Result<(), StoreError> {
        self.with_tx(|tx| {
            let cur = row::claim_by_issue(tx, id)?
                .ok_or_else(|| StoreError::NotFound(format!("no active claim on {id}")))?;
            tx.execute("DELETE FROM claims WHERE issue_id=?1", params![id.as_str()])
                .map_err(backend)?;
            let now = now_rfc3339();
            tx.execute(
                "UPDATE issues SET status='ready', updated_at=?2 WHERE id=?1 AND status='in-progress'",
                params![id.as_str(), now],
            )
            .map_err(backend)?;
            append_event_tx(
                tx,
                &event(
                    "claim.released",
                    id,
                    Some(&cur.worker),
                    serde_json::json!({ "worker": cur.worker }),
                ),
            )?;
            Ok(())
        })
    }

    fn set_claim_run(&self, id: &IssueId, run_id: &str) -> Result<(), StoreError> {
        self.with_tx(|tx| {
            let n = tx
                .execute(
                    "UPDATE claims SET run_id = ?2 WHERE issue_id = ?1",
                    params![id.as_str(), run_id],
                )
                .map_err(backend)?;
            if n == 0 {
                return Err(StoreError::NotFound(format!("no active claim on {id}")));
            }
            Ok(())
        })
    }

    fn active_claims(&self) -> Result<Vec<Claim>, StoreError> {
        self.read(row::active_claims)
    }

    fn add_comment(&self, c: &NewComment) -> Result<Comment, StoreError> {
        if c.body.trim().is_empty() {
            return Err(StoreError::Validation("comment body must not be empty".into()));
        }
        if c.author.trim().is_empty() || c.to_lane.trim().is_empty() {
            return Err(StoreError::Validation(
                "author and to_lane must not be empty".into(),
            ));
        }
        self.with_tx(|tx| {
            require_issue(tx, &c.issue_id)?;
            if let Some(r) = c.reply_to {
                let exists: Option<i64> = tx
                    .query_row("SELECT id FROM comments WHERE id=?1", params![r], |row| {
                        row.get(0)
                    })
                    .optional()
                    .map_err(backend)?;
                if exists.is_none() {
                    return Err(StoreError::NotFound(format!("comment #{r}")));
                }
            }
            tx.execute(
                "INSERT INTO comments (issue_id, author, type, to_lane, body, reply_to, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    c.issue_id.as_str(),
                    c.author.trim(),
                    c.kind.as_str(),
                    c.to_lane.trim(),
                    c.body,
                    c.reply_to,
                    now_rfc3339()
                ],
            )
            .map_err(backend)?;
            let cid = tx.last_insert_rowid();
            // An ANSWER that replies to a QUESTION of the same issue resolves it. `resolved_at IS
            // NULL` keeps the first resolution time when a second answer comes in.
            if let (CommentType::Answer, Some(r)) = (c.kind, c.reply_to) {
                tx.execute(
                    "UPDATE comments SET resolved_at = ?1
                     WHERE id = ?2 AND issue_id = ?3 AND type = ?4 AND resolved_at IS NULL",
                    params![
                        now_rfc3339(),
                        r,
                        c.issue_id.as_str(),
                        CommentType::Question.as_str()
                    ],
                )
                .map_err(backend)?;
            }
            append_event_tx(
                tx,
                &event(
                    "comment.added",
                    &c.issue_id,
                    Some(c.author.trim()),
                    serde_json::json!({ "comment_id": cid, "type": c.kind.as_str(), "to": c.to_lane.trim() }),
                ),
            )?;
            row::comment_by_id(tx, cid)?.ok_or_else(|| StoreError::Backend("comment vanished".into()))
        })
    }

    fn comments(&self, id: &IssueId) -> Result<Vec<Comment>, StoreError> {
        self.read(|c| row::comments(c, id))
    }

    fn inbox(&self, lane: &str, peek: bool, all: bool) -> Result<Vec<Comment>, StoreError> {
        let lane = lane.trim();
        if lane.is_empty() {
            return Err(StoreError::Validation("lane must not be empty".into()));
        }
        let own_author = inbox_own_author(lane);
        let items = self.read(|c| row::inbox(c, lane, &own_author, all || lane == LANE_ORCHESTRATOR))?;
        let items = without_resolved_questions(items);
        if !peek && !items.is_empty() {
            self.with_tx(|tx| advance_cursors(tx, lane, &items))?;
        }
        Ok(items)
    }

    fn mark_read(&self, lane: &str, issue: &IssueId) -> Result<usize, StoreError> {
        let lane = lane.trim();
        if lane.is_empty() {
            return Err(StoreError::Validation("lane must not be empty".into()));
        }
        let own_author = inbox_own_author(lane);
        // Read and write in one transaction so a comment added in between is not marked unseen-as-read.
        self.with_tx(|tx| {
            let items: Vec<Comment> =
                without_resolved_questions(row::inbox(tx, lane, &own_author, lane == LANE_ORCHESTRATOR)?)
                    .into_iter()
                    .filter(|c| &c.issue_id == issue)
                    .collect();
            advance_cursors(tx, lane, &items)?;
            Ok(items.len())
        })
    }

    fn append_event(&self, e: &NewEvent) -> Result<u64, StoreError> {
        self.with_tx(|tx| append_event_tx(tx, e))
    }

    fn events_since(&self, seq: u64, limit: usize) -> Result<Vec<Event>, StoreError> {
        self.read(|c| row::events_since(c, seq, limit))
    }

    fn last_run_event(&self, run_id: &str, kind: &str) -> Result<Option<Event>, StoreError> {
        self.read(|c| row::last_run_event(c, run_id, kind))
    }

    fn export_jsonl(&self, w: &mut dyn Write, o: &ExportOpts) -> Result<(), StoreError> {
        jsonl::export(self, w, o)
    }

    fn import_jsonl(&self, r: &mut dyn BufRead, m: ImportMode) -> Result<ImportReport, StoreError> {
        jsonl::import(self, r, m)
    }

    fn create_run(&self, n: &NewRun) -> Result<Run, StoreError> {
        runs::create_run(self, n)
    }

    fn update_run(&self, id: &str, p: &RunPatch) -> Result<Run, StoreError> {
        runs::update_run(self, id, p)
    }

    fn get_run(&self, id: &str) -> Result<Option<Run>, StoreError> {
        runs::get_run(self, id)
    }

    fn runs(&self, issue: Option<&IssueId>, active_only: bool) -> Result<Vec<Run>, StoreError> {
        runs::runs(self, issue, active_only)
    }

    fn enqueue_merge(
        &self,
        issue: &IssueId,
        run_id: Option<&str>,
        branch: &str,
    ) -> Result<MergeItem, StoreError> {
        runs::enqueue_merge(self, issue, run_id, branch)
    }

    fn merge_queue(&self) -> Result<Vec<MergeItem>, StoreError> {
        runs::merge_queue(self)
    }

    fn update_merge(&self, id: i64, p: &MergePatch) -> Result<MergeItem, StoreError> {
        runs::update_merge(self, id, p)
    }
}

/// A QUESTION that an ANSWER has resolved (`resolved_at` set by `add_comment`) needs nobody any more,
/// so the inbox does not list it. Other comment types ignore `resolved_at`.
fn without_resolved_questions(items: Vec<Comment>) -> Vec<Comment> {
    items
        .into_iter()
        .filter(|c| !(c.kind == CommentType::Question && c.resolved_at.is_some()))
        .collect()
}

/// The author name whose comments a lane never sees in its own inbox.
fn inbox_own_author(lane: &str) -> String {
    if lane == LANE_ORCHESTRATOR {
        LANE_ORCHESTRATOR.to_string()
    } else {
        format!("agent:{lane}")
    }
}

/// Moves the per-issue read cursor of `lane` up to the newest comment of `items` (never backwards).
fn advance_cursors(tx: &Transaction<'_>, lane: &str, items: &[Comment]) -> Result<(), StoreError> {
    let mut last: std::collections::BTreeMap<&IssueId, i64> = std::collections::BTreeMap::new();
    for it in items {
        let e = last.entry(&it.issue_id).or_insert(it.id);
        *e = (*e).max(it.id);
    }
    for (issue, id) in last {
        tx.execute(
            "INSERT INTO cursors (lane, issue_id, last_comment_id) VALUES (?1, ?2, ?3)
             ON CONFLICT(lane, issue_id) DO UPDATE SET last_comment_id = max(last_comment_id, excluded.last_comment_id)",
            params![lane, issue.as_str(), id],
        )
        .map_err(backend)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rebuild switches `foreign_keys` OFF on the shared connection; it must come back ON, or every
    /// later write on this store would silently skip referential checks.
    #[test]
    fn foreign_keys_are_on_again_after_migrating_a_v1_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.db");
        Connection::open(&path)
            .unwrap()
            .execute_batch(include_str!("../tests/fixtures/schema_v1.sql"))
            .unwrap();
        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let on: i64 = store
            .read(|c| {
                c.query_row("PRAGMA foreign_keys", [], |r| r.get(0))
                    .map_err(backend)
            })
            .unwrap();
        assert_eq!(on, 1);
    }
}
