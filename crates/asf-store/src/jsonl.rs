//! JSONL export/import for `SqliteStore`.

use std::collections::HashMap;
use std::io::{BufRead, Write};

use asf_core::jsonl::{CommentRecord, IssueRecord, Record, FORMAT};
use asf_core::*;
use rusqlite::{params, OptionalExtension, Transaction};

use crate::{append_event_tx, backend, row, SqliteStore};

fn json_err(e: serde_json::Error) -> StoreError {
    StoreError::Backend(format!("json: {e}"))
}

fn write_line<W: Write + ?Sized>(w: &mut W, r: &Record) -> Result<(), StoreError> {
    let line = serde_json::to_string(r).map_err(json_err)?;
    w.write_all(line.as_bytes())
        .map_err(|e| StoreError::Io(e.to_string()))?;
    w.write_all(b"\n").map_err(|e| StoreError::Io(e.to_string()))
}

fn issue_record(i: &Issue) -> IssueRecord {
    IssueRecord {
        id: i.id.clone(),
        uid: i.uid.clone(),
        title: i.title.clone(),
        body: i.body.clone(),
        kind: i.kind,
        status: i.status,
        priority: i.priority,
        parent: i.parent_id.clone(),
        file_scope: i.file_scope.clone(),
        acceptance: i.acceptance.clone(),
        estimate: i.estimate.clone(),
        labels: i.labels.clone(),
        source: i.source.clone(),
        created_at: i.created_at.clone(),
        updated_at: i.updated_at.clone(),
        closed_at: i.closed_at.clone(),
    }
}

pub(crate) fn export(store: &SqliteStore, w: &mut dyn Write, o: &ExportOpts) -> Result<(), StoreError> {
    store.read(|c| {
        let repo = row::repo(c)?;
        let issues = row::list_issues(c, &IssueFilter::default())?;
        let deps = row::deps(c)?;
        write_line(
            w,
            &Record::Meta {
                format: FORMAT,
                prefix: repo.id_prefix,
                next_seq: repo.next_seq,
                exported_at: now_rfc3339(),
            },
        )?;
        for i in &issues {
            write_line(w, &Record::Issue(issue_record(i)))?;
        }
        for d in &deps {
            write_line(
                w,
                &Record::Dep {
                    issue: d.issue_id.clone(),
                    depends_on: d.depends_on.clone(),
                },
            )?;
        }
        if o.include_comments {
            for i in &issues {
                let comments = row::comments(c, &i.id)?;
                let n_of: HashMap<i64, i64> = comments
                    .iter()
                    .enumerate()
                    .map(|(k, cm)| (cm.id, k as i64 + 1))
                    .collect();
                for (k, cm) in comments.iter().enumerate() {
                    write_line(
                        w,
                        &Record::Comment(CommentRecord {
                            issue: i.id.clone(),
                            n: k as i64 + 1,
                            author: cm.author.clone(),
                            kind: cm.kind,
                            to: cm.to_lane.clone(),
                            body: cm.body.clone(),
                            reply_to_n: cm.reply_to.and_then(|r| n_of.get(&r).copied()),
                            created_at: cm.created_at.clone(),
                        }),
                    )?;
                }
            }
        }
        Ok(())
    })
}

fn parse_all(r: &mut dyn BufRead) -> Result<Vec<Record>, StoreError> {
    let mut out = Vec::new();
    for (idx, line) in r.lines().enumerate() {
        let line = line.map_err(|e| StoreError::Io(e.to_string()))?;
        let line = line.trim_start_matches('\u{feff}');
        if line.trim().is_empty() {
            continue;
        }
        let rec: Record = serde_json::from_str(line)
            .map_err(|e| StoreError::Validation(format!("line {}: {e}", idx + 1)))?;
        out.push(rec);
    }
    match out.first() {
        Some(Record::Meta { format, .. }) if *format == FORMAT => Ok(out),
        Some(Record::Meta { format, .. }) => Err(StoreError::Validation(format!(
            "unsupported jsonl format {format} (expected {FORMAT})"
        ))),
        _ => Err(StoreError::Validation("first record must be `meta`".into())),
    }
}

fn upsert_issue(tx: &Transaction<'_>, r: &IssueRecord, report: &mut ImportReport) -> Result<(), StoreError> {
    let existing: Option<(String, String)> = tx
        .query_row(
            "SELECT id, updated_at FROM issues WHERE uid = ?1",
            params![r.uid],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(backend)?;
    let cols = (
        serde_json::to_string(&r.file_scope).map_err(json_err)?,
        serde_json::to_string(&r.acceptance).map_err(json_err)?,
        serde_json::to_string(&r.labels).map_err(json_err)?,
    );
    match existing {
        None => {
            let id_taken: Option<String> = tx
                .query_row(
                    "SELECT uid FROM issues WHERE id = ?1",
                    params![r.id.as_str()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(backend)?;
            if let Some(other) = id_taken {
                return Err(StoreError::Conflict(format!(
                    "issue id {} already exists with a different uid ({other} vs {})",
                    r.id, r.uid
                )));
            }
            tx.execute(
                "INSERT INTO issues (id, uid, title, body, type, status, priority, parent_id, file_scope, acceptance,
                 estimate, labels, source, created_at, updated_at, closed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    r.id.as_str(),
                    r.uid,
                    r.title,
                    r.body,
                    r.kind.as_str(),
                    r.status.as_str(),
                    r.priority,
                    r.parent.as_ref().map(|p| p.as_str()),
                    cols.0,
                    cols.1,
                    r.estimate,
                    cols.2,
                    r.source,
                    r.created_at,
                    r.updated_at,
                    r.closed_at,
                ],
            )
            .map_err(backend)?;
            report.issues_created += 1;
        }
        Some((id, updated_at)) => {
            if r.updated_at <= updated_at {
                report.skipped += 1;
                return Ok(());
            }
            tx.execute(
                "UPDATE issues SET title=?2, body=?3, type=?4, status=?5, priority=?6, parent_id=?7, file_scope=?8,
                 acceptance=?9, estimate=?10, labels=?11, source=?12, updated_at=?13, closed_at=?14 WHERE id=?1",
                params![
                    id,
                    r.title,
                    r.body,
                    r.kind.as_str(),
                    r.status.as_str(),
                    r.priority,
                    r.parent.as_ref().map(|p| p.as_str()),
                    cols.0,
                    cols.1,
                    r.estimate,
                    cols.2,
                    r.source,
                    r.updated_at,
                    r.closed_at,
                ],
            )
            .map_err(backend)?;
            report.issues_updated += 1;
        }
    }
    Ok(())
}

pub(crate) fn import(
    store: &SqliteStore,
    r: &mut dyn BufRead,
    mode: ImportMode,
) -> Result<ImportReport, StoreError> {
    let records = parse_all(r)?;
    store.with_tx(|tx| {
        let repo = row::repo(tx)?;
        let mut report = ImportReport::default();
        if mode == ImportMode::Replace {
            for t in ["cursors", "comments", "claims", "issue_deps", "issues"] {
                tx.execute(&format!("DELETE FROM {t}"), []).map_err(backend)?;
            }
        }
        let mut meta_next_seq = 1;
        // Parents may be declared after children; issues first (two passes for parent FK), then deps, then comments.
        for rec in &records {
            if let Record::Meta { next_seq, .. } = rec {
                meta_next_seq = *next_seq;
            }
            if let Record::Issue(i) = rec {
                if i.id.prefix() != repo.id_prefix {
                    return Err(StoreError::Validation(format!(
                        "issue {} has prefix `{}` but this repo uses `{}`",
                        i.id,
                        i.id.prefix(),
                        repo.id_prefix
                    )));
                }
                upsert_issue(tx, i, &mut report)?;
            }
        }
        for rec in &records {
            if let Record::Dep { issue, depends_on } = rec {
                let n = tx
                    .execute(
                        "INSERT OR IGNORE INTO issue_deps (issue_id, depends_on) VALUES (?1, ?2)",
                        params![issue.as_str(), depends_on.as_str()],
                    )
                    .map_err(backend)?;
                report.deps += n;
            }
        }
        let issues = row::list_issues(tx, &IssueFilter::default())?;
        let deps = row::deps(tx)?;
        scheduler::validate_dag(&issues, &deps).map_err(|e| StoreError::Validation(e.to_string()))?;

        // Comments: (issue, n) is the identity; existing comments keep their position.
        let mut existing_count: HashMap<IssueId, Vec<i64>> = HashMap::new();
        for rec in &records {
            if let Record::Comment(c) = rec {
                let ids = match existing_count.get(&c.issue) {
                    Some(v) => v.clone(),
                    None => {
                        let v: Vec<i64> = row::comments(tx, &c.issue)?.iter().map(|x| x.id).collect();
                        existing_count.insert(c.issue.clone(), v.clone());
                        v
                    }
                };
                if (c.n as usize) <= ids.len() {
                    continue;
                }
                if c.n as usize != ids.len() + 1 {
                    return Err(StoreError::Validation(format!(
                        "comment {}#{} out of order (have {})",
                        c.issue,
                        c.n,
                        ids.len()
                    )));
                }
                let reply_to = c.reply_to_n.and_then(|n| ids.get((n - 1) as usize).copied());
                tx.execute(
                    "INSERT INTO comments (issue_id, author, type, to_lane, body, reply_to, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        c.issue.as_str(),
                        c.author,
                        c.kind.as_str(),
                        c.to,
                        c.body,
                        reply_to,
                        c.created_at
                    ],
                )
                .map_err(backend)?;
                let new_id = tx.last_insert_rowid();
                existing_count
                    .get_mut(&c.issue)
                    .expect("inserted above")
                    .push(new_id);
                report.comments += 1;
            }
        }

        let max_seq = issues.iter().map(|i| i.id.seq()).max().unwrap_or(0) + 1;
        let next_seq = repo.next_seq.max(meta_next_seq).max(max_seq);
        tx.execute(
            "UPDATE repos SET next_seq = ?2 WHERE id = ?1",
            params![repo.id, next_seq],
        )
        .map_err(backend)?;
        append_event_tx(
            tx,
            &NewEvent {
                kind: "store.imported".into(),
                issue_id: None,
                run_id: None,
                actor: None,
                payload: serde_json::to_value(&report).map_err(json_err)?,
            },
        )?;
        Ok(report)
    })
}
