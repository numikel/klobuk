//! `asf run plan` — the deterministic allocation plan (dispatch itself lands in M2).

use asf_core::scheduler::{self, AllocationPlan, SchedulerInput};
use asf_core::{IssueFilter, IssueStore};
use serde::Serialize;

use crate::cli::{Cli, RunCmd};
use crate::ctx::Ctx;
use crate::git;
use crate::output::{AppError, Rendered};

#[derive(Serialize)]
struct PlanView {
    #[serde(flatten)]
    plan: AllocationPlan,
    /// `max_parallel` from config.
    capacity: usize,
    /// Claims counted against capacity.
    running: usize,
}

pub fn run(cli: &Cli, cmd: &RunCmd) -> Result<Rendered, AppError> {
    let ctx = Ctx::load(cli)?;
    match cmd {
        RunCmd::Plan => plan(&ctx),
    }
}

fn plan(ctx: &Ctx) -> Result<Rendered, AppError> {
    scheduler::validate_scope(&ctx.config.scheduler.hotspots)
        .map_err(|e| AppError::validation(format!("scheduler.hotspots: {e}")))?;
    let issues = ctx.store.list_issues(&IssueFilter::default())?;
    let deps = ctx.store.deps()?;
    let claims = ctx.store.active_claims()?;
    let tracked = git::tracked_files(&ctx.root)?;
    let input = SchedulerInput {
        issues: &issues,
        deps: &deps,
        claims: &claims,
        tracked_files: &tracked,
        max_parallel: ctx.config.scheduler.max_parallel,
        hotspots: &ctx.config.scheduler.hotspots,
    };
    let plan = scheduler::plan(&input, &ctx.config.repo.worktrees_dir)?;
    let view = PlanView {
        capacity: ctx.config.scheduler.max_parallel,
        running: claims.len(),
        plan,
    };

    let mut text = format!(
        "plan {} — capacity {} (running {})\n",
        view.plan.inputs_hash, view.capacity, view.running
    );
    if view.plan.slots.is_empty() {
        text.push_str("slots: none\n");
    } else {
        text.push_str("slots:\n");
        for s in &view.plan.slots {
            text.push_str(&format!(
                "  {:<10} P{} {:<24} {:<22} scope: {}\n",
                s.issue,
                s.priority,
                s.branch,
                s.worktree,
                if s.scope.is_empty() {
                    "(none)".to_string()
                } else {
                    s.scope.join(" ")
                }
            ));
        }
    }
    if !view.plan.deferred.is_empty() {
        text.push_str("deferred:\n");
        for d in &view.plan.deferred {
            let reason = serde_json::to_value(&d.reason)
                .ok()
                .and_then(|v| {
                    let r = v.get("reason")?.as_str()?.to_string();
                    let detail = v.get("detail").map(|x| x.to_string()).unwrap_or_default();
                    Some(if detail.is_empty() {
                        r
                    } else {
                        format!("{r} {detail}")
                    })
                })
                .unwrap_or_default();
            text.push_str(&format!("  {:<10} {reason}\n", d.issue));
        }
    }
    Rendered::new(&view, text.trim_end())
}
