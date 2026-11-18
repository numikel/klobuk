//! `klobuk lane list|check|clean` — claims joined with worktree existence; removal of finished lanes.

use klobuk_core::{Claim, IssueId, IssueStore, Vcs, VcsError};
use klobuk_vcs::GitCli;
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

#[derive(Serialize, Clone, Debug)]
pub struct Cleaned {
    pub id: IssueId,
    pub worktree: String,
    pub removed: bool,
}

/// A lane `lane clean --all` left alone, and why.
#[derive(Serialize, Clone, Debug)]
pub struct Refused {
    pub id: IssueId,
    pub worktree: String,
    pub reason: String,
}

/// `<worktrees_dir>/<id>`, relative to the repo root.
pub(crate) fn lane_rel(ctx: &Ctx, id: &IssueId) -> String {
    format!("{}/{id}", ctx.config.repo.worktrees_dir.trim_end_matches('/'))
}

/// Removes `<worktrees_dir>/<id>` without force and keeps the branch. Refuses a claimed lane, a dirty
/// worktree, a lane without a commit of its own (`worktree_remove` would take it through the ancestor
/// path) and a branch that is not merged (`worktree_remove`). `dry_run` runs every check except that last
/// one, which only `worktree_remove` can answer, and removes nothing.
pub(crate) fn clean_lane(ctx: &Ctx, id: &IssueId, dry_run: bool) -> Result<Cleaned, AppError> {
    if ctx.store.active_claims()?.iter().any(|c| &c.issue_id == id) {
        return Err(AppError::conflict(format!(
            "{id} is claimed; release or finish it before cleaning its lane"
        )));
    }
    let rel = lane_rel(ctx, id);
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
    Ok(Cleaned {
        id: id.clone(),
        worktree: rel,
        removed: !dry_run,
    })
}

/// `clean_lane` for the callers that sweep lanes nobody asked about by name (`--all`, the merge queue):
/// a lane whose run is still active is theirs and stays, even if its claim was dropped.
pub(crate) fn clean_finished_lane(ctx: &Ctx, id: &IssueId, dry_run: bool) -> Result<Cleaned, AppError> {
    if !ctx.store.runs(Some(id), true)?.is_empty() {
        return Err(AppError::conflict(format!(
            "{id} has a run that is still active; abort or finish it first"
        )));
    }
    let done = clean_lane(ctx, id, dry_run)?;
    if dry_run {
        // only `worktree_remove` answers "merged?" in a real run; a dry run asks git the same question
        let wt = ctx.root.join(&done.worktree);
        let branch = GitCli.current_branch(&wt).map_err(vcs_err)?;
        if !merged_into_head(&ctx.root, &branch) {
            return Err(vcs_err(VcsError::Unmerged(branch)));
        }
    }
    Ok(done)
}

/// Whether `branch` is in `HEAD` of `root`, as `GitCli::worktree_remove` judges it: an ancestor, or every
/// commit has a patch-equivalent in `HEAD` (a rebase-merge rewrote the SHAs) and at least one is ahead.
/// A git failure counts as "not merged", the safe answer for a preview.
fn merged_into_head(root: &std::path::Path, branch: &str) -> bool {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .stdin(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    };
    if git(&["merge-base", "--is-ancestor", branch, "HEAD"]).is_some() {
        return true;
    }
    let range = format!("HEAD..{branch}");
    match (
        git(&["cherry", "HEAD", branch]),
        git(&["rev-list", "--count", &range]),
    ) {
        (Some(cherry), Some(ahead)) => {
            !cherry.lines().any(|l| l.starts_with('+')) && ahead.trim().parse::<u64>().is_ok_and(|n| n > 0)
        }
        _ => false,
    }
}

fn clean(ctx: &Ctx, id: &IssueId, dry_run: bool) -> Result<Rendered, AppError> {
    let done = clean_lane(ctx, id, dry_run)?;
    let text = if done.removed {
        format!("removed worktree {} (branch kept)", done.worktree)
    } else {
        format!("dry-run: would remove worktree {} (branch kept)", done.worktree)
    };
    Rendered::new(&done, text)
}

#[derive(Serialize)]
struct Swept {
    dry_run: bool,
    cleaned: Vec<Cleaned>,
    refused: Vec<Refused>,
}

/// `lane clean` for every directory under the worktrees directory, by name. A refusal (claimed, dirty,
/// unmerged, ...) is an entry of `refused`, never an error, so one stuck lane does not hide the rest.
fn clean_all(ctx: &Ctx, dry_run: bool) -> Result<Rendered, AppError> {
    let dir = ctx.root.join(&ctx.config.repo.worktrees_dir);
    let mut ids: Vec<IssueId> = match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().to_str()?.parse().ok())
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(AppError::io(format!("{}: {e}", dir.display()))),
    };
    ids.sort_by_key(ToString::to_string);
    let (mut cleaned, mut refused) = (Vec::new(), Vec::new());
    for id in ids {
        match clean_finished_lane(ctx, &id, dry_run) {
            Ok(done) => cleaned.push(done),
            Err(e) => refused.push(Refused {
                worktree: lane_rel(ctx, &id),
                id,
                reason: e.message,
            }),
        }
    }
    let verb = if dry_run {
        "dry-run: would remove"
    } else {
        "removed"
    };
    let mut lines: Vec<String> = cleaned
        .iter()
        .map(|c| format!("{verb} worktree {} (branch kept)", c.worktree))
        .collect();
    lines.extend(
        refused
            .iter()
            .map(|r| format!("kept {}: {}", r.worktree, r.reason)),
    );
    if lines.is_empty() {
        lines.push("no worktrees to clean".into());
    }
    Rendered::new(
        &Swept {
            dry_run,
            cleaned,
            refused,
        },
        lines.join(
            "
",
        ),
    )
}

pub fn run(cli: &Cli, cmd: &LaneCmd) -> Result<Rendered, AppError> {
    let ctx = Ctx::load(cli)?;
    match cmd {
        LaneCmd::Clean {
            all: true, dry_run, ..
        } => clean_all(&ctx, *dry_run),
        LaneCmd::Clean {
            id: Some(id),
            dry_run,
            ..
        } => clean(&ctx, id, *dry_run),
        // clap guarantees an id unless `--all`
        LaneCmd::Clean { id: None, .. } => Err(AppError::usage("`klobuk lane clean` needs an id or --all")),
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
