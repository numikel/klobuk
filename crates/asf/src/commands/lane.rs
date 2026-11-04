//! `asf lane list|check|clean` — claims joined with worktree existence; removal of finished lanes.

use asf_core::{Claim, IssueId, IssueStore, Vcs, VcsError};
use asf_vcs::GitCli;
use serde::Serialize;

use crate::cli::{Cli, LaneCmd};
use crate::ctx::Ctx;
use crate::output::{AppError, ErrorCode, Rendered};

#[derive(Serialize)]
struct LaneView {
    #[serde(flatten)]
    claim: Claim,
    worktree_exists: bool,
}

fn lanes(ctx: &Ctx) -> Result<Vec<LaneView>, AppError> {
    let wt_dir = ctx.config.repo.worktrees_dir.clone();
    Ok(ctx
        .store
        .active_claims()?
        .into_iter()
        .map(|claim| {
            let rel = claim
                .worktree
                .clone()
                .unwrap_or_else(|| format!("{wt_dir}/{}", claim.issue_id));
            let worktree_exists = ctx.root.join(rel).is_dir();
            LaneView {
                claim,
                worktree_exists,
            }
        })
        .collect())
}

fn vcs_err(e: VcsError) -> AppError {
    match e {
        VcsError::NotFound(m) => AppError::not_found(m),
        VcsError::Dirty(_) | VcsError::Unmerged(_) => AppError::conflict(e.to_string()),
        VcsError::Git(m) => AppError::new(ErrorCode::Backend, m),
        VcsError::Io(m) => AppError::io(m),
    }
}

#[derive(Serialize)]
struct Cleaned {
    id: IssueId,
    worktree: String,
    removed: bool,
}

/// Removes `<worktrees_dir>/<id>` without force and keeps the branch. Refuses a claimed lane, a dirty
/// worktree, a lane without a commit of its own (`worktree_remove` would take it through the ancestor
/// path) and a branch that is not merged (`worktree_remove`). `dry_run` runs every check except that last
/// one, which only `worktree_remove` can answer, and removes nothing.
fn clean(ctx: &Ctx, id: &IssueId, dry_run: bool) -> Result<Rendered, AppError> {
    if ctx.store.active_claims()?.iter().any(|c| &c.issue_id == id) {
        return Err(AppError::conflict(format!(
            "{id} is claimed; release or finish it before cleaning its lane"
        )));
    }
    let rel = format!("{}/{id}", ctx.config.repo.worktrees_dir.trim_end_matches('/'));
    let wt = ctx.root.join(&rel);
    if !wt.is_dir() {
        return Err(AppError::not_found(format!("no worktree {rel} for {id}")));
    }
    let vcs = GitCli;
    let dirty = vcs.status_porcelain(&wt).map_err(vcs_err)?;
    if !dirty.is_empty() {
        return Err(vcs_err(VcsError::Dirty(dirty)));
    }
    // A lane whose HEAD is still the base of its latest run has no work of its own: not "finished".
    if let Some(run) = ctx.store.runs(Some(id), false)?.last() {
        if vcs.head(&wt).map_err(vcs_err)? == run.base_sha {
            return Err(AppError::conflict(format!(
                "{id} has no commits of its own (HEAD is still the run's base {}); nothing to clean",
                run.base_sha
            )));
        }
    }
    if !dry_run {
        vcs.worktree_remove(&ctx.root, &wt).map_err(vcs_err)?;
    }
    let removed = !dry_run;
    let text = if removed {
        format!("removed worktree {rel} (branch kept)")
    } else {
        format!("dry-run: would remove worktree {rel} (branch kept)")
    };
    Rendered::new(
        &Cleaned {
            id: id.clone(),
            worktree: rel,
            removed,
        },
        text,
    )
}

pub fn run(cli: &Cli, cmd: &LaneCmd) -> Result<Rendered, AppError> {
    let ctx = Ctx::load(cli)?;
    match cmd {
        LaneCmd::Clean { id, dry_run } => clean(&ctx, id, *dry_run),
        LaneCmd::List => {
            let v = lanes(&ctx)?;
            let text = if v.is_empty() {
                "no active lanes".to_string()
            } else {
                v.iter()
                    .map(|l| {
                        format!(
                            "{:<10} {:<14} {:<22} {}",
                            l.claim.issue_id,
                            l.claim.worker,
                            l.claim.worktree.as_deref().unwrap_or("-"),
                            if l.worktree_exists { "ok" } else { "MISSING" }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            Rendered::new(&v, text)
        }
        LaneCmd::Check => {
            #[derive(Serialize)]
            struct Check {
                stale: Vec<IssueId>,
                ok: bool,
            }
            let stale: Vec<IssueId> = lanes(&ctx)?
                .into_iter()
                .filter(|l| !l.worktree_exists)
                .map(|l| l.claim.issue_id)
                .collect();
            let text = if stale.is_empty() {
                "OK: every claim has a worktree".to_string()
            } else {
                format!(
                    "STALE (claim without worktree): {}",
                    stale
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            };
            Rendered::new(
                &Check {
                    ok: stale.is_empty(),
                    stale,
                },
                text,
            )
        }
    }
}
