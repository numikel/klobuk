//! `asf merge …` — the local merge queue: rebase → gates → fast-forward onto the default branch.
//!
//! Conflicts are never resolved automatically: the rebase is aborted, the item and the issue go
//! `blocked`, and a BLOCKER comment names the files.

use asf_core::*;
use asf_vcs::{run_gates, GitCli};
use serde::Serialize;

use crate::cli::{Cli, MergeCmd};
use crate::ctx::Ctx;
use crate::output::{AppError, ErrorCode, Rendered};

#[derive(Serialize)]
pub struct TickResult {
    pub processed: usize,
    pub items: Vec<MergeItem>,
}

pub fn run(cli: &Cli, cmd: &MergeCmd) -> Result<Rendered, AppError> {
    let ctx = Ctx::load(cli)?;
    match cmd {
        MergeCmd::Enqueue { id } => {
            let run_id = ctx.store.runs(Some(id), false)?.into_iter().last().map(|r| r.id);
            let item = ctx
                .store
                .enqueue_merge(id, run_id.as_deref(), &format!("feat/{id}"))?;
            Rendered::new(
                &item,
                format!("#{} {} queued ({})", item.id, item.issue_id, item.branch),
            )
        }
        MergeCmd::List => {
            let q = ctx.store.merge_queue()?;
            let text = if q.is_empty() {
                "merge queue empty".to_string()
            } else {
                q.iter().map(item_line).collect::<Vec<_>>().join("\n")
            };
            Rendered::new(&q, text)
        }
        MergeCmd::Tick { once } => {
            let res = tick_all(&ctx, *once)?;
            let text = if res.items.is_empty() {
                "merge queue: nothing queued".to_string()
            } else {
                res.items.iter().map(item_line).collect::<Vec<_>>().join("\n")
            };
            Rendered::new(&res, text)
        }
        MergeCmd::Retry { id } => {
            let item = find(&ctx, id)?;
            if !matches!(item.state, MergeState::Failed | MergeState::Blocked) {
                return Err(AppError::conflict(format!(
                    "#{} {} is {}, only failed/blocked items can be retried",
                    item.id, id, item.state
                )));
            }
            let updated = ctx.store.update_merge(
                item.id,
                &MergePatch {
                    state: Some(MergeState::Queued),
                    last_error: Some(None),
                    ..MergePatch::default()
                },
            )?;
            if ctx.store.get_issue(id)?.map(|i| i.status) == Some(Status::Blocked) {
                ctx.store.set_status(id, Status::InReview, LANE_ORCHESTRATOR)?;
            }
            Rendered::new(&updated, format!("#{} {} re-queued", updated.id, id))
        }
        MergeCmd::Skip { id } => {
            let item = find(&ctx, id)?;
            if item.state.is_terminal() {
                return Err(AppError::conflict(format!(
                    "#{} {} is already {}",
                    item.id, id, item.state
                )));
            }
            let updated = ctx.store.update_merge(
                item.id,
                &MergePatch {
                    state: Some(MergeState::Skipped),
                    ..MergePatch::default()
                },
            )?;
            Rendered::new(&updated, format!("#{} {} skipped", updated.id, id))
        }
    }
}

/// Processes queued items in FIFO order (`once` = at most one).
pub fn tick_all(ctx: &Ctx, once: bool) -> Result<TickResult, AppError> {
    let mut items = Vec::new();
    while let Some(next) = ctx
        .store
        .merge_queue()?
        .into_iter()
        .find(|m| m.state == MergeState::Queued)
    {
        let done = process(ctx, &next)?;
        items.push(done);
        if once {
            break;
        }
    }
    Ok(TickResult {
        processed: items.len(),
        items,
    })
}

fn find(ctx: &Ctx, id: &IssueId) -> Result<MergeItem, AppError> {
    ctx.store
        .merge_queue()?
        .into_iter()
        .find(|m| &m.issue_id == id)
        .ok_or_else(|| AppError::not_found(format!("{id} is not in the merge queue")))
}

fn item_line(m: &MergeItem) -> String {
    format!(
        "#{:<4} {:<10} {:<10} {:<16} attempts {} {}",
        m.id,
        m.issue_id,
        m.state,
        m.branch,
        m.attempts,
        m.last_error
            .as_deref()
            .map(|e| e.lines().next().unwrap_or(""))
            .unwrap_or("")
    )
}

fn set(ctx: &Ctx, id: i64, patch: MergePatch) -> Result<MergeItem, AppError> {
    Ok(ctx.store.update_merge(id, &patch)?)
}

fn fail(
    ctx: &Ctx,
    item: &MergeItem,
    state: MergeState,
    error: String,
    gates: Option<Vec<GateResult>>,
) -> Result<MergeItem, AppError> {
    set(
        ctx,
        item.id,
        MergePatch {
            state: Some(state),
            last_error: Some(Some(error)),
            gate_results: gates,
            ..MergePatch::default()
        },
    )
}

fn comment(ctx: &Ctx, issue: &IssueId, kind: CommentType, to: &str, body: String) -> Result<(), AppError> {
    ctx.store.add_comment(&NewComment {
        issue_id: issue.clone(),
        author: LANE_ORCHESTRATOR.to_string(),
        kind,
        to_lane: to.to_string(),
        body,
        reply_to: None,
    })?;
    Ok(())
}

/// Rebase → gates → ff-merge for one queued item. Never returns `Err` for a merge problem: the
/// problem lands in the item's state and `last_error`.
fn process(ctx: &Ctx, item: &MergeItem) -> Result<MergeItem, AppError> {
    let vcs = GitCli;
    let default_branch = ctx.config.repo.default_branch.clone();
    let run = match &item.run_id {
        Some(r) => ctx.store.get_run(r)?,
        None => None,
    };
    let worktree_rel = run.as_ref().map(|r| r.worktree.clone()).unwrap_or_else(|| {
        format!(
            "{}/{}",
            ctx.config.repo.worktrees_dir.trim_end_matches('/'),
            item.issue_id
        )
    });
    let wt = ctx.root.join(&worktree_rel);
    if !wt.is_dir() {
        return fail(
            ctx,
            item,
            MergeState::Failed,
            format!("worktree {worktree_rel} does not exist"),
            None,
        );
    }
    set(
        ctx,
        item.id,
        MergePatch {
            state: Some(MergeState::Rebasing),
            attempts: Some(item.attempts + 1),
            ..MergePatch::default()
        },
    )?;

    // 1. rebase
    let main_head = vcs
        .head(&ctx.root)
        .map_err(|e| AppError::new(ErrorCode::Backend, e.to_string()))?;
    match vcs.rebase(&wt, &default_branch) {
        Ok(RebaseOutcome::Clean { head }) => {
            set(
                ctx,
                item.id,
                MergePatch {
                    head_sha: Some(head),
                    rebased_onto: Some(main_head.clone()),
                    ..MergePatch::default()
                },
            )?;
        }
        Ok(RebaseOutcome::Conflict { files }) => {
            let msg = format!(
                "rebase of {} onto {default_branch} conflicts in: {}",
                item.branch,
                files.join(", ")
            );
            let updated = fail(ctx, item, MergeState::Blocked, msg.clone(), None)?;
            let _ = ctx
                .store
                .set_status(&item.issue_id, Status::Blocked, LANE_ORCHESTRATOR);
            comment(
                ctx,
                &item.issue_id,
                CommentType::Blocker,
                LANE_HUMAN,
                format!(
                    "{msg}\n\nThe rebase was aborted; the worktree ({worktree_rel}) is clean on {}. Resolve by hand \
                     (rebase in the worktree, or `asf run reply {} \"rebase onto {default_branch} and resolve …\"`), then \
                     `asf merge retry {}`.",
                    item.branch, item.issue_id, item.issue_id
                ),
            )?;
            return Ok(updated);
        }
        Err(e) => return fail(ctx, item, MergeState::Failed, format!("rebase failed: {e}"), None),
    }

    // 2. gates
    set(
        ctx,
        item.id,
        MergePatch {
            state: Some(MergeState::Gating),
            ..MergePatch::default()
        },
    )?;
    let specs: Vec<GateSpec> = ctx
        .config
        .gates
        .iter()
        .map(|g| GateSpec {
            name: g.name.clone(),
            cmd: g.cmd.clone(),
            timeout_s: g.timeout_s,
            shell: g.shell.clone(),
        })
        .collect();
    let results = run_gates(&specs, &wt);
    if let Some(bad) = results.iter().find(|r| !r.ok) {
        let msg = format!(
            "gate `{}` failed (exit {:?}) after rebase:\n{}",
            bad.name,
            bad.exit_code,
            bad.tail
                .lines()
                .rev()
                .take(15)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n")
        );
        let updated = fail(ctx, item, MergeState::Failed, msg.clone(), Some(results.clone()))?;
        comment(
            ctx,
            &item.issue_id,
            CommentType::Blocker,
            LANE_HUMAN,
            format!("{msg}\n\nFix and `asf merge retry {}`.", item.issue_id),
        )?;
        return Ok(updated);
    }
    set(
        ctx,
        item.id,
        MergePatch {
            gate_results: Some(results),
            ..MergePatch::default()
        },
    )?;

    // 3. fast-forward
    set(
        ctx,
        item.id,
        MergePatch {
            state: Some(MergeState::Merging),
            ..MergePatch::default()
        },
    )?;
    match vcs.current_branch(&ctx.root) {
        Ok(b) if b == default_branch => {}
        Ok(b) => {
            return fail(
                ctx,
                item,
                MergeState::Failed,
                format!("main checkout is on `{b}`, expected `{default_branch}`"),
                None,
            )
        }
        Err(e) => return fail(ctx, item, MergeState::Failed, e.to_string(), None),
    }
    let new_head = match vcs.ff_merge(&ctx.root, &item.branch) {
        Ok(h) => h,
        Err(e) => {
            return fail(
                ctx,
                item,
                MergeState::Failed,
                format!("fast-forward failed: {e}"),
                None,
            )
        }
    };
    let updated = set(
        ctx,
        item.id,
        MergePatch {
            state: Some(MergeState::Merged),
            head_sha: Some(new_head.clone()),
            last_error: Some(None),
            ..MergePatch::default()
        },
    )?;
    ctx.store
        .set_status(&item.issue_id, Status::Done, LANE_ORCHESTRATOR)?;
    let _ = ctx.store.release(&item.issue_id);
    if let Some(r) = &run {
        if let Some(ws) = &r.workspace_id {
            if let Ok(rt) = asf_runtime::from_config(&ctx.config.worker.runtime, &ctx.root) {
                let _ = rt.close_workspace(ws);
                let _ = rt.notify(
                    &format!("asf {} merged", item.issue_id),
                    &format!(
                        "{} -> {default_branch} {}",
                        item.branch,
                        &new_head[..7.min(new_head.len())]
                    ),
                    Some("done"),
                );
            }
        }
    }
    comment(
        ctx,
        &item.issue_id,
        CommentType::Fyi,
        LANE_ALL,
        format!(
            "{} merged into {default_branch} at {new_head} ({} done)",
            item.branch, item.issue_id
        ),
    )?;
    ctx.store.append_event(&NewEvent {
        kind: "merge.merged".into(),
        issue_id: Some(item.issue_id.clone()),
        run_id: item.run_id.clone(),
        actor: Some(LANE_ORCHESTRATOR.into()),
        payload: serde_json::json!({ "head": new_head, "branch": item.branch }),
    })?;
    Ok(updated)
}
