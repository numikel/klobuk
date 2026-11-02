//! `asf lane list|check` — claims joined with worktree existence.

use asf_core::{Claim, IssueId, IssueStore};
use serde::Serialize;

use crate::cli::{Cli, LaneCmd};
use crate::ctx::Ctx;
use crate::output::{AppError, Rendered};

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

pub fn run(cli: &Cli, cmd: &LaneCmd) -> Result<Rendered, AppError> {
    let ctx = Ctx::load(cli)?;
    match cmd {
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
