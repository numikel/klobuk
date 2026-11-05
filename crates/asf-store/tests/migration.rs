//! v1 -> v2 schema migration: `issues.status` gains `validating` via an in-place table rebuild.

use std::collections::BTreeMap;
use std::path::Path;

use asf_core::*;
use asf_store::{SqliteStore, SCHEMA_VERSION};
use rusqlite::Connection;

/// The exact schema SQL shipped before `validating` existed.
const SCHEMA_V1: &str = include_str!("fixtures/schema_v1.sql");

const TABLES: [&str; 10] = [
    "repos",
    "issues",
    "issue_deps",
    "claims",
    "comments",
    "cursors",
    "runs",
    "merge_queue",
    "events",
    "meta",
];

/// Creates a v1 database with rows in every table that references `issues`.
fn seed_v1(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(SCHEMA_V1).unwrap();
    conn.execute_batch(
        "INSERT INTO repos (id, root, id_prefix, next_seq, default_branch, created_at)
           VALUES ('R1', 'D:/repo', 'asf', 4, 'main', '2026-11-01T10:00:00Z');
         INSERT INTO issues (id, uid, title, type, status, parent_id, created_at, updated_at) VALUES
           ('asf-1', 'U1', 'epic', 'epic', 'backlog', NULL, '2026-11-01T10:00:00Z', '2026-11-01T10:00:00Z'),
           ('asf-2', 'U2', 'child', 'task', 'in-review', 'asf-1', '2026-11-01T10:00:00Z', '2026-11-01T10:00:00Z'),
           ('asf-3', 'U3', 'next', 'task', 'ready', 'asf-1', '2026-11-01T10:00:00Z', '2026-11-01T10:00:00Z');
         INSERT INTO issue_deps (issue_id, depends_on) VALUES ('asf-3', 'asf-2');
         INSERT INTO claims (issue_id, worker, claimed_at) VALUES ('asf-2', 'asf-2', '2026-11-01T10:05:00Z');
         INSERT INTO comments (issue_id, author, type, to_lane, body, reply_to, created_at) VALUES
           ('asf-2', 'human', 'QUESTION', 'all', 'why?', NULL, '2026-11-01T10:06:00Z'),
           ('asf-2', 'agent:asf-2', 'ANSWER', 'human', 'because', 1, '2026-11-01T10:07:00Z');
         INSERT INTO cursors (lane, issue_id, last_comment_id) VALUES ('human', 'asf-2', 1);
         INSERT INTO runs (id, issue_id, runtime, worktree, branch, base_sha, state, started_at)
           VALUES ('run-1', 'asf-2', 'herdr', '.worktrees/asf-2', 'feat/asf-2', 'abc123', 'succeeded', '2026-11-01T10:05:00Z');
         INSERT INTO merge_queue (issue_id, branch, state, enqueued_at, updated_at)
           VALUES ('asf-2', 'feat/asf-2', 'queued', '2026-11-01T10:10:00Z', '2026-11-01T10:10:00Z');
         INSERT INTO events (ts, kind, issue_id, payload) VALUES ('2026-11-01T10:00:00Z', 'issue.created', 'asf-1', '{}');",
    )
    .unwrap();
    assert_eq!(version(&conn), "1", "fixture must really be a v1 database");
}

fn version(conn: &Connection) -> String {
    conn.query_row("SELECT value FROM meta WHERE key='schema_version'", [], |r| {
        r.get(0)
    })
    .unwrap()
}

fn row_counts(path: &Path) -> BTreeMap<&'static str, i64> {
    let conn = Connection::open(path).unwrap();
    TABLES
        .iter()
        .map(|t| {
            let n = conn
                .query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))
                .unwrap();
            (*t, n)
        })
        .collect()
}

fn id(s: &str) -> IssueId {
    s.parse().unwrap()
}

#[test]
fn v1_database_is_migrated_to_v2_and_keeps_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.db");
    seed_v1(&path);
    let before = row_counts(&path);
    assert_eq!(before["issues"], 3);
    assert_eq!(before["comments"], 2);
    assert_eq!(before["runs"], 1);

    let store = SqliteStore::open(&path).unwrap();

    assert_eq!(SCHEMA_VERSION, 2);
    assert_eq!(store.schema_version().unwrap(), 2);
    assert_eq!(row_counts(&path), before, "migration must not add or drop rows");

    // Data survives intact and is reachable through the port.
    let child = store.get_issue(&id("asf-2")).unwrap().unwrap();
    assert_eq!(child.status, Status::InReview);
    assert_eq!(child.parent_id, Some(id("asf-1")));
    assert_eq!(store.comments(&id("asf-2")).unwrap().len(), 2);
    assert_eq!(store.deps().unwrap().len(), 1);
    assert_eq!(store.active_claims().unwrap().len(), 1);
    assert_eq!(store.repo().unwrap().next_seq, 4);

    // The rebuilt table accepts the new status.
    let validating = store
        .set_status(&id("asf-2"), Status::Validating, "human")
        .unwrap();
    assert_eq!(validating.status, Status::Validating);
}

#[test]
fn migration_leaves_no_dangling_references_or_temp_table() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.db");
    seed_v1(&path);
    drop(SqliteStore::open(&path).unwrap());

    let conn = Connection::open(&path).unwrap();
    let violations: i64 = conn
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(violations, 0, "foreign_key_check must be clean");
    let leftover: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'issues_new'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(leftover, 0, "the temporary rebuild table must be renamed away");
    let ddl: String = conn
        .query_row("SELECT sql FROM sqlite_master WHERE name = 'issues'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(ddl.contains("'validating'"), "{ddl}");
    // Children still point at `issues`, not at the dropped/renamed table.
    for child in ["comments", "runs", "claims", "issue_deps", "merge_queue"] {
        let sql: String = conn
            .query_row("SELECT sql FROM sqlite_master WHERE name = ?1", [child], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(!sql.contains("issues_new"), "{child}: {sql}");
    }
}

#[test]
fn migration_is_idempotent_on_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.db");
    seed_v1(&path);
    drop(SqliteStore::open(&path).unwrap());
    let migrated = row_counts(&path);
    let again = SqliteStore::open(&path).unwrap();
    assert_eq!(again.schema_version().unwrap(), 2);
    assert_eq!(
        row_counts(&path),
        migrated,
        "a second open must not touch any rows"
    );
}

#[test]
fn fresh_database_is_created_at_v2() {
    let store = SqliteStore::open_in_memory().unwrap();
    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
}

#[test]
fn newer_schema_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.db");
    seed_v1(&path);
    Connection::open(&path)
        .unwrap()
        .execute("UPDATE meta SET value = '99' WHERE key = 'schema_version'", [])
        .unwrap();
    let err = SqliteStore::open(&path)
        .err()
        .expect("open must refuse a newer schema");
    assert!(matches!(err, StoreError::Backend(_)), "{err}");
    assert!(err.to_string().contains("99"), "{err}");
}
