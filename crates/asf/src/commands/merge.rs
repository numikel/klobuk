//! `asf merge …` — the local merge queue: rebase → gates → fast-forward onto the default branch.
//!
//! Conflicts are never resolved automatically: the rebase is aborted, the item and the issue go
//! `blocked`, and a BLOCKER comment names the files.

use asf_core::brief::{parse_verdict, Verdict};
use asf_core::config::GateWhen;
use asf_core::github::{ChecksState, GitHub, Mergeable, PrState};
use asf_core::vcs::VcsError;
use asf_core::*;
use asf_vcs::gates::stage_specs;
use asf_vcs::{run_gates, GitCli};
use serde::Serialize;

use crate::cli::{Cli, MergeCmd};
use crate::ctx::Ctx;
use crate::output::{AppError, ErrorCode, Rendered};
use crate::worker::Worker;

/// Event the queue appends when a review round starts; its `after_comment` is the id of the newest
/// comment at that moment, so verdicts of earlier rounds are never read again.
const REVIEW_STARTED: &str = "merge.review_started";

/// Event the queue appends when a reviewer approves; its `patch_id` is the `git patch-id --stable`
/// fingerprint of the approved branch. Approvals never expire: a later review of an identical patch is skipped.
const REVIEW_APPROVED: &str = "merge.review_approved";

/// Event the queue appends when a review is skipped because the patch matches an earlier approval.
const REVIEW_SKIPPED: &str = "merge.review_skipped";

/// Queues `feat/<id>` linked to the issue's latest run. Without `force` the latest run must be
/// `succeeded` (independently verified): a worker cannot push its own branch into the queue.
pub fn enqueue_checked(ctx: &Ctx, id: &IssueId, force: bool) -> Result<MergeItem, AppError> {
    if ctx.store.get_issue(id)?.is_none() {
        return Err(AppError::not_found(format!("issue {id} not found")));
    }
    let latest = ctx.store.runs(Some(id), false)?.into_iter().last();
    if !force {
        match &latest {
            Some(r) if r.state == RunState::Succeeded => {}
            Some(r) => {
                return Err(AppError::conflict(format!(
                    "{id}: latest run {} is `{}`, not `succeeded`; let `asf run watch` verify it, or `asf merge enqueue {id} --force` as the operator",
                    r.id, r.state
                )))
            }
            None => {
                return Err(AppError::conflict(format!(
                    "{id} has no run to verify; `asf merge enqueue {id} --force` queues the branch by hand (operator only)"
                )))
            }
        }
    }
    let run_id = latest.map(|r| r.id);
    Ok(ctx
        .store
        .enqueue_merge(id, run_id.as_deref(), &format!("feat/{id}"))?)
}

#[derive(Serialize)]
pub struct TickResult {
    pub processed: usize,
    pub items: Vec<MergeItem>,
}

pub fn run(cli: &Cli, cmd: &MergeCmd) -> Result<Rendered, AppError> {
    let ctx = Ctx::load(cli)?;
    match cmd {
        MergeCmd::Enqueue { id, force } => {
            let item = enqueue_checked(&ctx, id, *force)?;
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
                ctx.store.set_status(id, Status::Validating, LANE_ORCHESTRATOR)?;
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

/// Processes queued items in FIFO order (`once` = at most one), looking at every item waiting for
/// its reviewer on the way, then, for the GitHub target, polls every open pull request; a look that
/// changes nothing is not reported.
pub fn tick_all(ctx: &Ctx, once: bool) -> Result<TickResult, AppError> {
    let mut items = Vec::new();
    let queue = ctx.store.merge_queue()?;
    let mut main_red = false;
    for next in queue
        .iter()
        .filter(|m| matches!(m.state, MergeState::Queued | MergeState::Reviewing))
    {
        let done = if next.state == MergeState::Queued {
            process(ctx, next)?
        } else {
            match poll_review(ctx, next)? {
                Some(changed) => changed,
                None => continue,
            }
        };
        main_red = is_main_red(&done);
        items.push(done);
        if once || main_red {
            break;
        }
    }
    // a red default branch halts the whole tick: nothing else may be merged or integrated on top of it
    if !main_red && ctx.config.merge.target == "github" && (!once || items.is_empty()) {
        for open in queue.iter().filter(|m| m.state == MergeState::PrOpen) {
            if let Some(changed) = poll_pr(ctx, open)? {
                main_red = is_main_red(&changed);
                items.push(changed);
                if once || main_red {
                    break;
                }
            }
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

fn gate_specs(ctx: &Ctx, stage: GateWhen) -> Vec<GateSpec> {
    stage_specs(&ctx.config.gates, stage)
}

/// The last `n` lines of `text`, in their original order.
fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// A merged item whose post-merge gate run on the repo root failed: still `merged`, but carrying the error.
fn is_main_red(item: &MergeItem) -> bool {
    item.state == MergeState::Merged && item.last_error.is_some()
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

/// The item's run, its worktree relative to the repo root, and that worktree as an absolute path.
fn locate(ctx: &Ctx, item: &MergeItem) -> Result<(Option<Run>, String, std::path::PathBuf), AppError> {
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
    Ok((run, worktree_rel, wt))
}

/// Rebase → gates → (review) → ff-merge for one queued item. Never returns `Err` for a merge
/// problem: the problem lands in the item's state and `last_error`.
fn process(ctx: &Ctx, item: &MergeItem) -> Result<MergeItem, AppError> {
    let vcs = GitCli;
    let default_branch = ctx.config.repo.default_branch.clone();
    let (run, worktree_rel, wt) = locate(ctx, item)?;
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

    // 0. GitHub: the local default branch must match the remote before rebasing onto it
    if ctx.config.merge.target == "github" {
        let gh_cfg = &ctx.config.merge.github;
        let base = gh_cfg.base.clone().unwrap_or_else(|| default_branch.clone());
        if let Err(e) = vcs.pull_ff(&ctx.root, &gh_cfg.remote, &base) {
            return fail(
                ctx,
                item,
                MergeState::Failed,
                format!("cannot sync {default_branch} from {}/{base}: {e}", gh_cfg.remote),
                None,
            );
        }
    }

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

    // 2. scope re-check: the branch may have gained commits after verification
    if let Some(issue) = ctx.store.get_issue(&item.issue_id)? {
        let changed = vcs
            .changed_files(&wt, &main_head)
            .map_err(|e| AppError::new(ErrorCode::Backend, e.to_string()))?;
        let outside = crate::worker::files_outside_scope(&changed, &issue.file_scope);
        if !outside.is_empty() {
            let msg = format!(
                "{} touches files outside scope [{}]: {}",
                item.branch,
                issue.file_scope.join(", "),
                outside.join(", ")
            );
            let updated = fail(ctx, item, MergeState::Blocked, msg.clone(), None)?;
            comment(
                ctx,
                &item.issue_id,
                CommentType::Blocker,
                LANE_HUMAN,
                format!(
                    "{msg}\n\nDrop or move those changes in the worktree, then `asf merge retry {}`.",
                    item.issue_id
                ),
            )?;
            return Ok(updated);
        }
    }

    // 3. gates
    set(
        ctx,
        item.id,
        MergePatch {
            state: Some(MergeState::Gating),
            ..MergePatch::default()
        },
    )?;
    let results = run_gates(&gate_specs(ctx, GateWhen::Merge), &wt);
    if let Some(bad) = results.iter().find(|r| !r.ok) {
        let msg = format!(
            "gate `{}` failed (exit {:?}) after rebase:\n{}",
            bad.name,
            bad.exit_code,
            last_lines(&bad.tail, 15)
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

    // 4. a review required by config or by label parks the item until the reviewer's verdict
    if review_wanted(ctx, &item.issue_id)? {
        match approved_patch(ctx, item)? {
            Some(patch_id) => {
                ctx.store.append_event(&NewEvent {
                    kind: REVIEW_SKIPPED.into(),
                    issue_id: Some(item.issue_id.clone()),
                    run_id: item.run_id.clone(),
                    actor: Some(LANE_ORCHESTRATOR.into()),
                    payload: serde_json::json!({ "patch_id": patch_id, "branch": item.branch }),
                })?;
            }
            None => return enter_review(ctx, item, &run),
        }
    }

    // 5. integrate
    integrate(ctx, item, &run, &wt, false)
}

/// Push + pull request, or a local fast-forward. With `park_if_dirty` (an approved review) a main
/// checkout with uncommitted changes leaves the item in `reviewing` for a later tick instead of failing it.
fn integrate(
    ctx: &Ctx,
    item: &MergeItem,
    run: &Option<Run>,
    wt: &std::path::Path,
    park_if_dirty: bool,
) -> Result<MergeItem, AppError> {
    let vcs = GitCli;
    let default_branch = ctx.config.repo.default_branch.clone();
    if ctx.config.merge.target == "github" {
        return open_pr(ctx, item, run, wt);
    }
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
        Err(VcsError::Dirty(files)) if park_if_dirty => {
            eprintln!(
                "asf merge: {} is approved but {default_branch} has uncommitted changes ({}); staying in reviewing",
                item.issue_id,
                files.join(", ")
            );
            return set(
                ctx,
                item.id,
                MergePatch {
                    state: Some(MergeState::Reviewing),
                    ..MergePatch::default()
                },
            );
        }
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
    finalize_merged(ctx, item, run, new_head)
}

/// Whether `[review] required` or one of the issue's labels in `on_labels` asks for a reviewer.
fn review_wanted(ctx: &Ctx, id: &IssueId) -> Result<bool, AppError> {
    let cfg = &ctx.config.review;
    if cfg.required {
        return Ok(true);
    }
    Ok(ctx
        .store
        .get_issue(id)?
        .is_some_and(|i| i.labels.iter().any(|l| cfg.on_labels.contains(l))))
}

/// The review children of `id` (one per review issue ever created for it).
fn review_children(ctx: &Ctx, id: &IssueId) -> Result<Vec<IssueId>, AppError> {
    Ok(ctx
        .store
        .list_issues(&IssueFilter {
            kind: Some(IssueType::Review),
            parent: Some(id.clone()),
            ..IssueFilter::default()
        })?
        .into_iter()
        .map(|i| i.id)
        .collect())
}

/// Verdicts of `id`'s reviewers, oldest first, as `(comment id, verdict)`. Only a DECISION authored
/// by a review child's lane counts (`agent:<review-id>`); the reviewer posts it on the reviewed
/// issue, but a copy on its own issue is read too. Anything that is not a verdict is skipped.
fn verdicts(ctx: &Ctx, id: &IssueId) -> Result<Vec<(i64, Verdict)>, AppError> {
    let children = review_children(ctx, id)?;
    let mut sources = vec![id.clone()];
    sources.extend(children.iter().cloned());
    let lanes: Vec<String> = children
        .iter()
        .flat_map(|c| [c.to_string(), format!("agent:{c}")])
        .collect();
    let mut found = Vec::new();
    for src in &sources {
        for c in ctx.store.comments(src)? {
            if c.kind == CommentType::Decision && lanes.contains(&c.author) {
                if let Some(v) = parse_verdict(&c.body) {
                    found.push((c.id, v));
                }
            }
        }
    }
    found.sort_by_key(|(id, _)| *id);
    Ok(found)
}

/// Id of the newest comment on `id` or on one of its review issues (0 when there is none).
fn newest_comment(ctx: &Ctx, id: &IssueId) -> Result<i64, AppError> {
    let mut newest = 0;
    let mut sources = vec![id.clone()];
    sources.extend(review_children(ctx, id)?);
    for src in &sources {
        for c in ctx.store.comments(src)? {
            newest = newest.max(c.id);
        }
    }
    Ok(newest)
}

/// Comment id after which a verdict belongs to the current review round of `id`.
fn round_boundary(ctx: &Ctx, id: &IssueId) -> Result<i64, AppError> {
    let mut boundary = 0;
    let mut seq = 0;
    loop {
        let page = ctx.store.events_since(seq, 500)?;
        let Some(last) = page.last() else { break };
        seq = last.seq;
        for e in &page {
            if e.kind == REVIEW_STARTED && e.issue_id.as_ref() == Some(id) {
                boundary = e.payload["after_comment"].as_i64().unwrap_or(boundary);
            }
        }
    }
    Ok(boundary)
}

/// Patch-id fingerprint of what `item.branch` changes relative to the default branch. `None` when it
/// cannot be computed or the branch changes nothing; the caller then falls back to a normal review.
fn branch_patch_id(ctx: &Ctx, item: &MergeItem) -> Option<String> {
    crate::git::patch_id(&ctx.root, &ctx.config.repo.default_branch, &item.branch)
        .ok()
        .flatten()
}

/// Every patch-id the reviewers approved for `id`, oldest first. Never expires and ignores review config.
fn approved_patch_ids(ctx: &Ctx, id: &IssueId) -> Result<Vec<String>, AppError> {
    let mut found = Vec::new();
    let mut seq = 0;
    loop {
        let page = ctx.store.events_since(seq, 500)?;
        let Some(last) = page.last() else { break };
        seq = last.seq;
        for e in &page {
            if e.kind == REVIEW_APPROVED && e.issue_id.as_ref() == Some(id) {
                if let Some(p) = e.payload["patch_id"].as_str() {
                    found.push(p.to_string());
                }
            }
        }
    }
    Ok(found)
}

/// The patch-id of `item.branch` when it equals one the reviewers already approved for this issue.
fn approved_patch(ctx: &Ctx, item: &MergeItem) -> Result<Option<String>, AppError> {
    let Some(current) = branch_patch_id(ctx, item) else {
        return Ok(None);
    };
    Ok(approved_patch_ids(ctx, &item.issue_id)?
        .contains(&current)
        .then_some(current))
}

/// Records the approval together with the patch-id of the approved branch (once per distinct patch:
/// an approved item parked behind a dirty main is looked at on every tick).
fn record_approval(ctx: &Ctx, item: &MergeItem) -> Result<(), AppError> {
    let Some(patch_id) = branch_patch_id(ctx, item) else {
        return Ok(());
    };
    if approved_patch_ids(ctx, &item.issue_id)?.contains(&patch_id) {
        return Ok(());
    }
    ctx.store.append_event(&NewEvent {
        kind: REVIEW_APPROVED.into(),
        issue_id: Some(item.issue_id.clone()),
        run_id: item.run_id.clone(),
        actor: Some(LANE_ORCHESTRATOR.into()),
        payload: serde_json::json!({ "patch_id": patch_id, "branch": item.branch }),
    })?;
    Ok(())
}

/// Review rounds of `id` that ended in `changes`.
fn changes_rounds(ctx: &Ctx, id: &IssueId) -> Result<usize, AppError> {
    Ok(verdicts(ctx, id)?
        .iter()
        .filter(|(_, v)| matches!(v, Verdict::Changes(_)))
        .count())
}

fn bullets(findings: &[String]) -> String {
    findings
        .iter()
        .map(|f| format!("- {f}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Gates are green and a review is wanted: ask the human instead once `max_rounds` rounds ended in
/// `changes`, else mark the item `reviewing` and start the reviewer.
fn enter_review(ctx: &Ctx, item: &MergeItem, run: &Option<Run>) -> Result<MergeItem, AppError> {
    let (Some(run), Some(issue)) = (run, ctx.store.get_issue(&item.issue_id)?) else {
        return fail(
            ctx,
            item,
            MergeState::Failed,
            format!("cannot start a reviewer: {} has no recorded run", item.issue_id),
            None,
        );
    };
    let max = ctx.config.review.max_rounds as usize;
    let rounds = changes_rounds(ctx, &issue.id)?;
    if rounds >= max {
        let msg = format!(
            "{rounds} review round(s) ended in `changes` and [review] max_rounds is {max}; no further reviewer is started"
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
                "{msg}.\n\nPlease decide: fix the findings in the worktree and raise `review.max_rounds` \
                 (`asf config set review.max_rounds {}`) before `asf merge retry {}`, or merge by hand, or \
                 `asf merge skip {}`.",
                max + 1,
                item.issue_id,
                item.issue_id
            ),
        )?;
        return Ok(updated);
    }
    let after_comment = newest_comment(ctx, &issue.id)?;
    let updated = set(
        ctx,
        item.id,
        MergePatch {
            state: Some(MergeState::Reviewing),
            last_error: Some(None),
            ..MergePatch::default()
        },
    )?;
    ctx.store.append_event(&NewEvent {
        kind: REVIEW_STARTED.into(),
        issue_id: Some(issue.id.clone()),
        run_id: item.run_id.clone(),
        actor: Some(LANE_ORCHESTRATOR.into()),
        payload: serde_json::json!({ "after_comment": after_comment, "round": rounds + 1 }),
    })?;
    match Worker::new(ctx).and_then(|w| w.start_review(&issue, run)) {
        Ok(_) => Ok(updated),
        Err(e) => {
            let msg = format!("cannot start the reviewer: {}", e.message);
            let failed = fail(ctx, item, MergeState::Failed, msg.clone(), None)?;
            comment(
                ctx,
                &item.issue_id,
                CommentType::Blocker,
                LANE_HUMAN,
                format!(
                    "{msg}\n\nFix the cause, then `asf merge retry {}`.",
                    item.issue_id
                ),
            )?;
            Ok(failed)
        }
    }
}

/// One look at an item waiting for its reviewer. `None` when there is no verdict of the current
/// round yet (nothing is written) or the approved item is parked behind a dirty main.
fn poll_review(ctx: &Ctx, item: &MergeItem) -> Result<Option<MergeItem>, AppError> {
    let boundary = round_boundary(ctx, &item.issue_id)?;
    let Some((_, verdict)) = verdicts(ctx, &item.issue_id)?
        .into_iter()
        .rfind(|(id, _)| *id > boundary)
    else {
        return Ok(None);
    };
    match verdict {
        Verdict::Approve => {
            let (run, worktree_rel, wt) = locate(ctx, item)?;
            if !wt.is_dir() {
                return fail(
                    ctx,
                    item,
                    MergeState::Failed,
                    format!("worktree {worktree_rel} does not exist"),
                    None,
                )
                .map(Some);
            }
            record_approval(ctx, item)?;
            let done = integrate(ctx, item, &run, &wt, true)?;
            Ok((done.state != MergeState::Reviewing).then_some(done))
        }
        Verdict::Changes(findings) => request_changes(ctx, item, &findings).map(Some),
    }
}

/// `changes`: the item fails with the findings, the issue is blocked, the human gets a BLOCKER, and
/// a worker still running is told through a run reply.
fn request_changes(ctx: &Ctx, item: &MergeItem, findings: &[String]) -> Result<MergeItem, AppError> {
    let list = bullets(findings);
    let msg = format!("review asks for changes on {}:\n{list}", item.branch);
    let updated = fail(ctx, item, MergeState::Failed, msg.clone(), None)?;
    let _ = ctx
        .store
        .set_status(&item.issue_id, Status::Blocked, LANE_ORCHESTRATOR);
    let alive = ctx
        .store
        .runs(Some(&item.issue_id), true)?
        .into_iter()
        .last()
        .is_some();
    let replied = alive
        && Worker::new(ctx)
            .and_then(|w| {
                w.reply(
                    &item.issue_id,
                    &format!("The reviewer asks for changes, fix them, commit and finish again:\n{list}"),
                )
            })
            .is_ok();
    comment(
        ctx,
        &item.issue_id,
        CommentType::Blocker,
        LANE_HUMAN,
        format!(
            "{msg}\n\n{}Fix the findings in the worktree, then `asf merge retry {}`.",
            if replied {
                "The running worker was sent the findings. "
            } else {
                ""
            },
            item.issue_id
        ),
    )?;
    Ok(updated)
}

/// After a merge: the review issues of `id` are closed `done`, their claims released, their runs and
/// workspaces closed. Their worktrees stay for `asf lane clean`.
fn close_reviews(ctx: &Ctx, id: &IssueId) {
    let Ok(children) = review_children(ctx, id) else {
        return;
    };
    let rt = asf_runtime::from_config(&ctx.config.worker.runtime, &ctx.root).ok();
    for rid in children {
        for r in ctx.store.runs(Some(&rid), true).unwrap_or_default() {
            if let (Some(rt), Some(ws)) = (&rt, &r.workspace_id) {
                let _ = rt.close_workspace(ws);
            }
            let _ = ctx.store.update_run(
                &r.id,
                &RunPatch {
                    state: Some(RunState::Succeeded),
                    exit_reason: Some("review approved, branch merged".into()),
                    finished_at: Some(now_rfc3339()),
                    ..RunPatch::default()
                },
            );
        }
        let _ = ctx.store.release(&rid);
        let _ = ctx.store.set_status(&rid, Status::Done, LANE_ORCHESTRATOR);
    }
}

/// `merged`: issue `done`, claim released, workspace closed, FYI to all, event.
fn finalize_merged(
    ctx: &Ctx,
    item: &MergeItem,
    run: &Option<Run>,
    new_head: String,
) -> Result<MergeItem, AppError> {
    let default_branch = ctx.config.repo.default_branch.clone();
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
    close_reviews(ctx, &item.issue_id);
    if let Some(r) = run {
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
        payload: serde_json::json!({ "head": new_head, "branch": item.branch, "pr_url": updated.pr_url }),
    })?;
    if ctx.config.merge.verify_main_after {
        return verify_main(ctx, updated, &new_head);
    }
    Ok(updated)
}

/// Runs the gates in the repository root once the merge landed (local fast-forward or `pull_ff` after
/// a merged PR), regardless of `require_clean_main`. A red gate leaves the item `merged` (no automatic
/// revert) but records `last_error`, a BLOCKER comment and a `merge.main_red` event; the caller stops the tick.
fn verify_main(ctx: &Ctx, merged: MergeItem, head: &str) -> Result<MergeItem, AppError> {
    let results = run_gates(&gate_specs(ctx, GateWhen::Main), &ctx.root);
    let Some(bad) = results.iter().find(|r| !r.ok) else {
        return Ok(merged);
    };
    let exit = bad
        .exit_code
        .map_or_else(|| "none".to_string(), |c| c.to_string());
    let msg = format!("main red after merge: gate `{}` failed (exit {exit})", bad.name);
    let updated = set(
        ctx,
        merged.id,
        MergePatch {
            last_error: Some(Some(msg.clone())),
            ..MergePatch::default()
        },
    )?;
    comment(
        ctx,
        &merged.issue_id,
        CommentType::Blocker,
        LANE_HUMAN,
        format!(
            "{msg} at {head}:\n{}\n\n{} is merged and stays merged (no automatic revert); the merge queue is halted for \
             this tick. Fix or revert by hand.",
            last_lines(&bad.tail, 15),
            merged.branch
        ),
    )?;
    ctx.store.append_event(&NewEvent {
        kind: "merge.main_red".into(),
        issue_id: Some(merged.issue_id.clone()),
        run_id: merged.run_id.clone(),
        actor: Some(LANE_ORCHESTRATOR.into()),
        payload: serde_json::json!({
            "head": head,
            "branch": merged.branch,
            "gate": bad.name,
            "exit_code": bad.exit_code,
        }),
    })?;
    Ok(updated)
}

fn github(ctx: &Ctx) -> Result<(Box<dyn GitHub>, String, String), AppError> {
    let cfg = &ctx.config.merge.github;
    let gh = asf_vcs::github_from_config(&cfg.runner, &ctx.root)
        .map_err(|e| AppError::new(ErrorCode::Backend, e.to_string()))?;
    let base = cfg
        .base
        .clone()
        .unwrap_or_else(|| ctx.config.repo.default_branch.clone());
    Ok((gh, cfg.remote.clone(), base))
}

/// Pushes the rebased branch and makes sure an open PR exists; the item waits in `pr_open`.
fn open_pr(
    ctx: &Ctx,
    item: &MergeItem,
    run: &Option<Run>,
    wt: &std::path::Path,
) -> Result<MergeItem, AppError> {
    let (gh, remote, base) = github(ctx)?;
    if let Err(e) = GitCli.push(wt, &remote, &item.branch, true) {
        return fail(
            ctx,
            item,
            MergeState::Failed,
            format!("push to {remote} failed: {e}"),
            None,
        );
    }
    let existing = match gh.pr_find(&ctx.root, &item.branch) {
        Ok(pr) => pr,
        Err(e) => return fail(ctx, item, MergeState::Failed, e.to_string(), None),
    };
    let pr = match existing {
        Some(pr) if pr.state == PrState::Open => pr,
        Some(pr) if pr.state == PrState::Merged => {
            // merged on GitHub while we were not looking: just integrate
            let queued = set(
                ctx,
                item.id,
                MergePatch {
                    state: Some(MergeState::PrOpen),
                    pr_url: Some(pr.url.clone()),
                    ..MergePatch::default()
                },
            )?;
            return integrate_remote(ctx, &queued, &remote, &base);
        }
        _ => {
            let issue = ctx.store.get_issue(&item.issue_id)?;
            let title = match &issue {
                Some(i) => format!("{}: {}", item.issue_id, i.title),
                None => item.issue_id.to_string(),
            };
            let body = format!(
                "{}\n\n---\nasf issue `{}`{}",
                issue.as_ref().map(|i| i.body.clone()).unwrap_or_default(),
                item.issue_id,
                run.as_ref()
                    .map(|r| format!(" · run `{}`", r.id))
                    .unwrap_or_default()
            );
            match gh.pr_create(&ctx.root, &base, &item.branch, &title, &body) {
                Ok(pr) => {
                    comment(
                        ctx,
                        &item.issue_id,
                        CommentType::Fyi,
                        LANE_ALL,
                        format!("pull request opened for {}: {}", item.branch, pr.url),
                    )?;
                    ctx.store.append_event(&NewEvent {
                        kind: "merge.pr_opened".into(),
                        issue_id: Some(item.issue_id.clone()),
                        run_id: item.run_id.clone(),
                        actor: Some(LANE_ORCHESTRATOR.into()),
                        payload: serde_json::json!({ "pr_url": pr.url, "number": pr.number }),
                    })?;
                    pr
                }
                Err(e) => {
                    return fail(
                        ctx,
                        item,
                        MergeState::Failed,
                        format!("cannot open pull request: {e}"),
                        None,
                    )
                }
            }
        }
    };
    set(
        ctx,
        item.id,
        MergePatch {
            state: Some(MergeState::PrOpen),
            pr_url: Some(pr.url),
            last_error: Some(None),
            ..MergePatch::default()
        },
    )
}

/// One look at an open PR. `None` when nothing changed (checks still running).
fn poll_pr(ctx: &Ctx, item: &MergeItem) -> Result<Option<MergeItem>, AppError> {
    let (gh, remote, base) = github(ctx)?;
    let cfg = &ctx.config.merge.github;
    let pr = match gh.pr_find(&ctx.root, &item.branch) {
        Ok(Some(pr)) => pr,
        Ok(None) => {
            return fail(
                ctx,
                item,
                MergeState::Failed,
                format!("no pull request for {} on GitHub any more", item.branch),
                None,
            )
            .map(Some)
        }
        Err(e) => return fail(ctx, item, MergeState::Failed, e.to_string(), None).map(Some),
    };
    let url = pr.url.clone();
    match pr.state {
        PrState::Merged => integrate_remote(ctx, item, &remote, &base).map(Some),
        PrState::Closed => {
            let msg = format!("pull request {url} was closed on GitHub without merging");
            let updated = fail(ctx, item, MergeState::Failed, msg.clone(), None)?;
            comment(
                ctx,
                &item.issue_id,
                CommentType::Blocker,
                LANE_HUMAN,
                format!(
                    "{msg}\n\n`asf merge retry {}` pushes again and opens a new one.",
                    item.issue_id
                ),
            )?;
            Ok(Some(updated))
        }
        PrState::Open => {
            if pr.mergeable == Mergeable::Conflicting {
                let msg = format!("pull request {url} conflicts with {base}");
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
                        "{msg}\n\nRebase the worktree onto {base} and resolve by hand, then `asf merge retry {}`.",
                        item.issue_id
                    ),
                )?;
                return Ok(Some(updated));
            }
            match pr.checks {
                ChecksState::Failed => {
                    let msg = format!("checks failed on pull request {url}");
                    let updated = fail(ctx, item, MergeState::Failed, msg.clone(), None)?;
                    comment(
                        ctx,
                        &item.issue_id,
                        CommentType::Blocker,
                        LANE_HUMAN,
                        format!(
                            "{msg}\n\nFix the branch, then `asf merge retry {}`.",
                            item.issue_id
                        ),
                    )?;
                    Ok(Some(updated))
                }
                ChecksState::Pending if cfg.wait_checks => Ok(None),
                _ => {
                    if let Err(e) = gh.pr_merge(&ctx.root, pr.number, &cfg.method, cfg.delete_branch) {
                        return fail(
                            ctx,
                            item,
                            MergeState::Failed,
                            format!("merge of {url} failed: {e}"),
                            None,
                        )
                        .map(Some);
                    }
                    integrate_remote(ctx, item, &remote, &base).map(Some)
                }
            }
        }
    }
}

/// After GitHub merged the PR: fast-forward the local default branch from the remote and close out.
fn integrate_remote(ctx: &Ctx, item: &MergeItem, remote: &str, base: &str) -> Result<MergeItem, AppError> {
    set(
        ctx,
        item.id,
        MergePatch {
            state: Some(MergeState::Merging),
            ..MergePatch::default()
        },
    )?;
    let run = match &item.run_id {
        Some(r) => ctx.store.get_run(r)?,
        None => None,
    };
    match GitCli.pull_ff(&ctx.root, remote, base) {
        Ok(head) => finalize_merged(ctx, item, &run, head),
        Err(e) => fail(
            ctx,
            item,
            MergeState::Failed,
            format!("pull request merged, but `git pull --ff-only {remote} {base}` failed: {e}"),
            None,
        ),
    }
}
