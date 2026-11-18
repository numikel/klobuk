//! Row ↔ model mapping and shared read queries.

use klobuk_core::*;
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::backend;

fn json_col<T: serde::de::DeserializeOwned>(row: &Row<'_>, idx: usize) -> rusqlite::Result<T> {
    let s: String = row.get(idx)?;
    serde_json::from_str(&s)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, Box::new(e)))
}

fn id_col(row: &Row<'_>, idx: usize) -> rusqlite::Result<IssueId> {
    let s: String = row.get(idx)?;
    s.parse()
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, Box::new(e)))
}

fn opt_id_col(row: &Row<'_>, idx: usize) -> rusqlite::Result<Option<IssueId>> {
    let s: Option<String> = row.get(idx)?;
    s.map(|s| {
        s.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, Box::new(e))
        })
    })
    .transpose()
}

fn enum_col<T: std::str::FromStr>(row: &Row<'_>, idx: usize) -> rusqlite::Result<T>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let s: String = row.get(idx)?;
    s.parse()
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, Box::new(e)))
}

pub(crate) const ISSUE_COLS: &str =
    "id, uid, title, body, type, status, priority, parent_id, file_scope, acceptance, \
     estimate, labels, source, created_at, updated_at, closed_at";

pub(crate) fn issue(row: &Row<'_>) -> rusqlite::Result<Issue> {
    Ok(Issue {
        id: id_col(row, 0)?,
        uid: row.get(1)?,
        title: row.get(2)?,
        body: row.get(3)?,
        kind: enum_col(row, 4)?,
        status: enum_col(row, 5)?,
        priority: row.get(6)?,
        parent_id: opt_id_col(row, 7)?,
        file_scope: json_col(row, 8)?,
        acceptance: json_col(row, 9)?,
        estimate: row.get(10)?,
        labels: json_col(row, 11)?,
        source: row.get(12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
        closed_at: row.get(15)?,
    })
}

pub(crate) fn issue_by_id(c: &Connection, id: &IssueId) -> Result<Option<Issue>, StoreError> {
    c.query_row(
        &format!("SELECT {ISSUE_COLS} FROM issues WHERE id = ?1"),
        params![id.as_str()],
        issue,
    )
    .optional()
    .map_err(backend)
}

/// Ordered by sequence number (numeric part of the id).
pub(crate) fn list_issues(c: &Connection, f: &IssueFilter) -> Result<Vec<Issue>, StoreError> {
    let mut sql = format!("SELECT {ISSUE_COLS} FROM issues WHERE 1=1");
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(s) = &f.status {
        sql.push_str(" AND status = ?");
        args.push(Box::new(s.as_str().to_string()));
    }
    if let Some(k) = &f.kind {
        sql.push_str(" AND type = ?");
        args.push(Box::new(k.as_str().to_string()));
    }
    if let Some(l) = &f.label {
        sql.push_str(" AND EXISTS (SELECT 1 FROM json_each(issues.labels) WHERE json_each.value = ?)");
        args.push(Box::new(l.clone()));
    }
    if let Some(p) = &f.parent {
        sql.push_str(" AND parent_id = ?");
        args.push(Box::new(p.as_str().to_string()));
    }
    sql.push_str(" ORDER BY CAST(substr(id, instr(id, '-') + 1) AS INTEGER)");
    let mut stmt = c.prepare(&sql).map_err(backend)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())), issue)
        .map_err(backend)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(backend)
}

pub(crate) fn deps(c: &Connection) -> Result<Vec<Dep>, StoreError> {
    let mut stmt = c
        .prepare("SELECT issue_id, depends_on FROM issue_deps ORDER BY issue_id, depends_on")
        .map_err(backend)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Dep {
                issue_id: id_col(r, 0)?,
                depends_on: id_col(r, 1)?,
            })
        })
        .map_err(backend)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(backend)
}

pub(crate) fn repo(c: &Connection) -> Result<RepoInfo, StoreError> {
    c.query_row(
        "SELECT id, root, id_prefix, next_seq, default_branch, created_at FROM repos LIMIT 1",
        [],
        |r| {
            Ok(RepoInfo {
                id: r.get(0)?,
                root: r.get(1)?,
                id_prefix: r.get(2)?,
                next_seq: r.get(3)?,
                default_branch: r.get(4)?,
                created_at: r.get(5)?,
            })
        },
    )
    .optional()
    .map_err(backend)?
    .ok_or_else(|| StoreError::NotFound("repository not initialised (run `klobuk repo init`)".into()))
}

pub(crate) fn events_since(c: &Connection, seq: u64, limit: usize) -> Result<Vec<Event>, StoreError> {
    let mut stmt = c
        .prepare(
            "SELECT seq, ts, kind, issue_id, run_id, actor, payload FROM events WHERE seq > ?1 ORDER BY seq LIMIT ?2",
        )
        .map_err(backend)?;
    let rows = stmt
        .query_map(params![seq as i64, limit as i64], |r| {
            Ok(Event {
                seq: r.get::<_, i64>(0)? as u64,
                ts: r.get(1)?,
                kind: r.get(2)?,
                issue_id: opt_id_col(r, 3)?,
                run_id: r.get(4)?,
                actor: r.get(5)?,
                payload: json_col(r, 6)?,
            })
        })
        .map_err(backend)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(backend)
}

pub(crate) fn last_run_event(c: &Connection, run_id: &str, kind: &str) -> Result<Option<Event>, StoreError> {
    c.query_row(
        "SELECT seq, ts, kind, issue_id, run_id, actor, payload FROM events \
         WHERE run_id = ?1 AND kind = ?2 ORDER BY seq DESC LIMIT 1",
        params![run_id, kind],
        |r| {
            Ok(Event {
                seq: r.get::<_, i64>(0)? as u64,
                ts: r.get(1)?,
                kind: r.get(2)?,
                issue_id: opt_id_col(r, 3)?,
                run_id: r.get(4)?,
                actor: r.get(5)?,
                payload: json_col(r, 6)?,
            })
        },
    )
    .optional()
    .map_err(backend)
}

pub(crate) const CLAIM_COLS: &str = "issue_id, worker, run_id, worktree, branch, claimed_at, note";

pub(crate) fn claim(row: &Row<'_>) -> rusqlite::Result<Claim> {
    Ok(Claim {
        issue_id: id_col(row, 0)?,
        worker: row.get(1)?,
        run_id: row.get(2)?,
        worktree: row.get(3)?,
        branch: row.get(4)?,
        claimed_at: row.get(5)?,
        note: row.get(6)?,
    })
}

pub(crate) fn claim_by_issue(c: &Connection, id: &IssueId) -> Result<Option<Claim>, StoreError> {
    c.query_row(
        &format!("SELECT {CLAIM_COLS} FROM claims WHERE issue_id = ?1"),
        params![id.as_str()],
        claim,
    )
    .optional()
    .map_err(backend)
}

pub(crate) fn active_claims(c: &Connection) -> Result<Vec<Claim>, StoreError> {
    let mut stmt = c
        .prepare(&format!(
            "SELECT {CLAIM_COLS} FROM claims ORDER BY claimed_at, issue_id"
        ))
        .map_err(backend)?;
    let rows = stmt.query_map([], claim).map_err(backend)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(backend)
}

pub(crate) const COMMENT_COLS: &str =
    "id, issue_id, author, type, to_lane, body, reply_to, resolved_at, created_at";

pub(crate) fn comment(row: &Row<'_>) -> rusqlite::Result<Comment> {
    Ok(Comment {
        id: row.get(0)?,
        issue_id: id_col(row, 1)?,
        author: row.get(2)?,
        kind: enum_col(row, 3)?,
        to_lane: row.get(4)?,
        body: row.get(5)?,
        reply_to: row.get(6)?,
        resolved_at: row.get(7)?,
        created_at: row.get(8)?,
    })
}

pub(crate) fn comment_by_id(c: &Connection, id: i64) -> Result<Option<Comment>, StoreError> {
    c.query_row(
        &format!("SELECT {COMMENT_COLS} FROM comments WHERE id = ?1"),
        params![id],
        comment,
    )
    .optional()
    .map_err(backend)
}

pub(crate) fn comments(c: &Connection, id: &IssueId) -> Result<Vec<Comment>, StoreError> {
    let mut stmt = c
        .prepare(&format!(
            "SELECT {COMMENT_COLS} FROM comments WHERE issue_id = ?1 ORDER BY id"
        ))
        .map_err(backend)?;
    let rows = stmt.query_map(params![id.as_str()], comment).map_err(backend)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(backend)
}

/// Unread comments for `lane`. With `all`, relevance is ignored (cursor and own-author exclusion still apply).
pub(crate) fn inbox(
    c: &Connection,
    lane: &str,
    own_author: &str,
    all: bool,
) -> Result<Vec<Comment>, StoreError> {
    let sql = format!(
        "SELECT {} FROM comments c
         LEFT JOIN cursors cu ON cu.lane = ?1 AND cu.issue_id = c.issue_id
         WHERE c.id > COALESCE(cu.last_comment_id, 0)
           AND c.author <> ?2
           AND ( ?3
                 OR c.to_lane IN (?1, 'all')
                 OR c.author = 'human'
                 OR EXISTS (SELECT 1 FROM claims cl WHERE cl.issue_id = c.issue_id AND cl.worker = ?1) )
         ORDER BY c.id",
        COMMENT_COLS
            .split(", ")
            .map(|col| format!("c.{col}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut stmt = c.prepare(&sql).map_err(backend)?;
    let rows = stmt
        .query_map(params![lane, own_author, all], comment)
        .map_err(backend)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(backend)
}
