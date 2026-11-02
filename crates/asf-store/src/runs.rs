//! Runs and merge queue persistence.

use asf_core::*;
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::{append_event_tx, backend, row, SqliteStore};

const RUN_COLS: &str = "id, issue_id, attempt, runtime, agent_name, workspace_id, pane_id, claude_session_id, transcript_path, \
     worktree, branch, base_sha, state, budget_usd, cost_usd, input_tokens, output_tokens, turns, tool_calls, exit_reason, \
     report_path, started_at, last_seen_at, finished_at, herdr_revision";

fn conv<E: std::error::Error + Send + Sync + 'static>(idx: usize, e: E) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, Box::new(e))
}

fn run_row(r: &Row<'_>) -> rusqlite::Result<Run> {
    let issue: String = r.get(1)?;
    let state: String = r.get(12)?;
    Ok(Run {
        id: r.get(0)?,
        issue_id: issue.parse().map_err(|e| conv(1, e))?,
        attempt: r.get(2)?,
        runtime: r.get(3)?,
        agent_name: r.get(4)?,
        workspace_id: r.get(5)?,
        pane_id: r.get(6)?,
        claude_session_id: r.get(7)?,
        transcript_path: r.get(8)?,
        worktree: r.get(9)?,
        branch: r.get(10)?,
        base_sha: r.get(11)?,
        state: state.parse().map_err(|e| conv(12, e))?,
        budget_usd: r.get(13)?,
        cost_usd: r.get(14)?,
        input_tokens: r.get::<_, Option<i64>>(15)?.unwrap_or(0),
        output_tokens: r.get::<_, Option<i64>>(16)?.unwrap_or(0),
        turns: r.get::<_, Option<i64>>(17)?.unwrap_or(0),
        tool_calls: r.get::<_, Option<i64>>(18)?.unwrap_or(0),
        exit_reason: r.get(19)?,
        report_path: r.get(20)?,
        started_at: r.get(21)?,
        last_seen_at: r.get(22)?,
        finished_at: r.get(23)?,
        herdr_revision: r.get(24)?,
    })
}

fn run_by_id(c: &Connection, id: &str) -> Result<Option<Run>, StoreError> {
    c.query_row(
        &format!("SELECT {RUN_COLS} FROM runs WHERE id = ?1"),
        params![id],
        run_row,
    )
    .optional()
    .map_err(backend)
}

fn require_run(c: &Connection, id: &str) -> Result<Run, StoreError> {
    run_by_id(c, id)?.ok_or_else(|| StoreError::NotFound(format!("run {id}")))
}

pub(crate) fn create_run(store: &SqliteStore, n: &NewRun) -> Result<Run, StoreError> {
    store.with_tx(|tx| {
        row::issue_by_id(tx, &n.issue_id)?.ok_or_else(|| StoreError::NotFound(format!("issue {}", n.issue_id)))?;
        let attempt: i64 = tx
            .query_row("SELECT count(*) + 1 FROM runs WHERE issue_id = ?1", params![n.issue_id.as_str()], |r| r.get(0))
            .map_err(backend)?;
        let id = ulid::Ulid::generate().to_string();
        let now = now_rfc3339();
        tx.execute(
            "INSERT INTO runs (id, issue_id, attempt, runtime, worktree, branch, base_sha, state, budget_usd, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'starting', ?8, ?9)",
            params![id, n.issue_id.as_str(), attempt, n.runtime, n.worktree, n.branch, n.base_sha, n.budget_usd, now],
        )
        .map_err(backend)?;
        append_event_tx(
            tx,
            &NewEvent {
                kind: "run.created".into(),
                issue_id: Some(n.issue_id.clone()),
                run_id: Some(id.clone()),
                actor: None,
                payload: serde_json::json!({ "attempt": attempt, "runtime": n.runtime }),
            },
        )?;
        require_run(tx, &id)
    })
}

pub(crate) fn update_run(store: &SqliteStore, id: &str, p: &RunPatch) -> Result<Run, StoreError> {
    store.with_tx(|tx| {
        let cur = require_run(tx, id)?;
        let state = p.state.unwrap_or(cur.state);
        tx.execute(
            "UPDATE runs SET state=?2, agent_name=?3, workspace_id=?4, pane_id=?5, claude_session_id=?6, transcript_path=?7,
             cost_usd=?8, input_tokens=?9, output_tokens=?10, turns=?11, tool_calls=?12, exit_reason=?13, report_path=?14,
             last_seen_at=?15, finished_at=?16, herdr_revision=?17 WHERE id=?1",
            params![
                id,
                state.as_str(),
                p.agent_name.clone().or(cur.agent_name),
                p.workspace_id.clone().or(cur.workspace_id),
                p.pane_id.clone().or(cur.pane_id),
                p.claude_session_id.clone().or(cur.claude_session_id),
                p.transcript_path.clone().or(cur.transcript_path),
                p.cost_usd.unwrap_or(cur.cost_usd),
                p.input_tokens.unwrap_or(cur.input_tokens),
                p.output_tokens.unwrap_or(cur.output_tokens),
                p.turns.unwrap_or(cur.turns),
                p.tool_calls.unwrap_or(cur.tool_calls),
                p.exit_reason.clone().or(cur.exit_reason),
                p.report_path.clone().or(cur.report_path),
                p.last_seen_at.clone().or(cur.last_seen_at),
                p.finished_at.clone().or(cur.finished_at),
                p.herdr_revision.or(cur.herdr_revision),
            ],
        )
        .map_err(backend)?;
        if state != cur.state {
            append_event_tx(
                tx,
                &NewEvent {
                    kind: "run.state".into(),
                    issue_id: Some(cur.issue_id.clone()),
                    run_id: Some(id.to_string()),
                    actor: None,
                    payload: serde_json::json!({ "from": cur.state.as_str(), "to": state.as_str(), "reason": p.exit_reason }),
                },
            )?;
        }
        require_run(tx, id)
    })
}

pub(crate) fn get_run(store: &SqliteStore, id: &str) -> Result<Option<Run>, StoreError> {
    store.read(|c| run_by_id(c, id))
}

pub(crate) fn runs(
    store: &SqliteStore,
    issue: Option<&IssueId>,
    active_only: bool,
) -> Result<Vec<Run>, StoreError> {
    store.read(|c| {
        let mut sql = format!("SELECT {RUN_COLS} FROM runs WHERE 1=1");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(i) = issue {
            sql.push_str(" AND issue_id = ?");
            args.push(Box::new(i.as_str().to_string()));
        }
        if active_only {
            sql.push_str(" AND state NOT IN ('succeeded','failed','aborted')");
        }
        sql.push_str(" ORDER BY started_at, id");
        let mut stmt = c.prepare(&sql).map_err(backend)?;
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())),
                run_row,
            )
            .map_err(backend)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(backend)
    })
}

const MERGE_COLS: &str = "id, issue_id, run_id, branch, state, attempts, head_sha, rebased_onto, pr_url, gate_results, last_error, \
     enqueued_at, updated_at";

fn merge_row(r: &Row<'_>) -> rusqlite::Result<MergeItem> {
    let issue: String = r.get(1)?;
    let state: String = r.get(4)?;
    let gates: Option<String> = r.get(9)?;
    Ok(MergeItem {
        id: r.get(0)?,
        issue_id: issue.parse().map_err(|e| conv(1, e))?,
        run_id: r.get(2)?,
        branch: r.get(3)?,
        state: state.parse().map_err(|e| conv(4, e))?,
        attempts: r.get(5)?,
        head_sha: r.get(6)?,
        rebased_onto: r.get(7)?,
        pr_url: r.get(8)?,
        gate_results: match gates {
            Some(s) => serde_json::from_str(&s).map_err(|e| conv(9, e))?,
            None => Vec::new(),
        },
        last_error: r.get(10)?,
        enqueued_at: r.get(11)?,
        updated_at: r.get(12)?,
    })
}

fn merge_by_id(c: &Connection, id: i64) -> Result<Option<MergeItem>, StoreError> {
    c.query_row(
        &format!("SELECT {MERGE_COLS} FROM merge_queue WHERE id = ?1"),
        params![id],
        merge_row,
    )
    .optional()
    .map_err(backend)
}

pub(crate) fn enqueue_merge(
    store: &SqliteStore,
    issue: &IssueId,
    run_id: Option<&str>,
    branch: &str,
) -> Result<MergeItem, StoreError> {
    store.with_tx(|tx| {
        row::issue_by_id(tx, issue)?.ok_or_else(|| StoreError::NotFound(format!("issue {issue}")))?;
        let existing: Option<(i64, String)> = tx
            .query_row("SELECT id, state FROM merge_queue WHERE issue_id = ?1", params![issue.as_str()], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()
            .map_err(backend)?;
        if let Some((id, state)) = existing {
            let st: MergeState = state.parse().map_err(|e: ParseEnumError| StoreError::Backend(e.to_string()))?;
            if !st.is_terminal() {
                return Err(StoreError::Conflict(format!("issue {issue} already queued (#{id}, {st})")));
            }
            // issue_id is UNIQUE: retire the old row so the issue can be queued again
            tx.execute("DELETE FROM merge_queue WHERE id = ?1", params![id]).map_err(backend)?;
        }
        let now = now_rfc3339();
        tx.execute(
            "INSERT INTO merge_queue (issue_id, run_id, branch, state, attempts, gate_results, enqueued_at, updated_at)
             VALUES (?1, ?2, ?3, 'queued', 0, '[]', ?4, ?4)",
            params![issue.as_str(), run_id, branch, now],
        )
        .map_err(backend)?;
        let id = tx.last_insert_rowid();
        append_event_tx(
            tx,
            &NewEvent {
                kind: "merge.enqueued".into(),
                issue_id: Some(issue.clone()),
                run_id: run_id.map(str::to_string),
                actor: None,
                payload: serde_json::json!({ "merge_id": id, "branch": branch }),
            },
        )?;
        merge_by_id(tx, id)?.ok_or_else(|| StoreError::Backend("merge item vanished".into()))
    })
}

pub(crate) fn merge_queue(store: &SqliteStore) -> Result<Vec<MergeItem>, StoreError> {
    store.read(|c| {
        let mut stmt = c
            .prepare(&format!("SELECT {MERGE_COLS} FROM merge_queue ORDER BY id"))
            .map_err(backend)?;
        let rows = stmt.query_map([], merge_row).map_err(backend)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(backend)
    })
}

pub(crate) fn update_merge(store: &SqliteStore, id: i64, p: &MergePatch) -> Result<MergeItem, StoreError> {
    store.with_tx(|tx| {
        let cur = merge_by_id(tx, id)?.ok_or_else(|| StoreError::NotFound(format!("merge item #{id}")))?;
        let state = p.state.unwrap_or(cur.state);
        let gates = p.gate_results.clone().unwrap_or(cur.gate_results.clone());
        let last_error = p.last_error.clone().unwrap_or(cur.last_error.clone());
        tx.execute(
            "UPDATE merge_queue SET state=?2, attempts=?3, head_sha=?4, rebased_onto=?5, pr_url=?6, gate_results=?7,
             last_error=?8, updated_at=?9 WHERE id=?1",
            params![
                id,
                state.as_str(),
                p.attempts.unwrap_or(cur.attempts),
                p.head_sha.clone().or(cur.head_sha),
                p.rebased_onto.clone().or(cur.rebased_onto),
                p.pr_url.clone().or(cur.pr_url),
                serde_json::to_string(&gates).map_err(|e| StoreError::Backend(e.to_string()))?,
                last_error,
                now_rfc3339(),
            ],
        )
        .map_err(backend)?;
        if state != cur.state {
            append_event_tx(
                tx,
                &NewEvent {
                    kind: "merge.state".into(),
                    issue_id: Some(cur.issue_id.clone()),
                    run_id: cur.run_id.clone(),
                    actor: None,
                    payload: serde_json::json!({ "merge_id": id, "from": cur.state.as_str(), "to": state.as_str() }),
                },
            )?;
        }
        merge_by_id(tx, id)?.ok_or_else(|| StoreError::Backend("merge item vanished".into()))
    })
}
