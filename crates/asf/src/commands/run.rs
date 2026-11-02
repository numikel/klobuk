//! `asf run …` — allocation plan and worker lifecycle.

use asf_core::scheduler::{self, AllocationPlan, SchedulerInput};
use asf_core::{IssueFilter, IssueStore};
use serde::Serialize;

use crate::cli::{Cli, RunCmd};
use crate::ctx::Ctx;
use crate::git;
use crate::output::{AppError, Rendered};
use crate::worker::{self, Worker};

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
        RunCmd::Start { id, approve } => {
            let w = Worker::new(&ctx)?;
            let res = w.start(id, approve.as_deref())?;
            let text = format!(
                "{} started: run {} in {} (workspace {}, pane {}){}",
                res.run.issue_id,
                res.run.id,
                res.run.worktree,
                res.workspace_id,
                res.pane_id,
                if res.trusted {
                    " [trust dialog answered]"
                } else {
                    ""
                }
            );
            Rendered::new(&worker::start_json(&res), text)
        }
        RunCmd::List => {
            let runs = Worker::new(&ctx)?.list()?;
            let text = if runs.is_empty() {
                "no runs".to_string()
            } else {
                runs.iter().map(worker::run_line).collect::<Vec<_>>().join("\n")
            };
            Rendered::new(&runs, text)
        }
        RunCmd::Show { id } => {
            let s = Worker::new(&ctx)?.show(id)?;
            let text = format!(
                "{}\nagent: {}",
                worker::run_line(&s.run),
                s.agent
                    .as_ref()
                    .map(|a| a.state.to_string())
                    .unwrap_or_else(|| "unknown".into())
            );
            Rendered::new(&s, text)
        }
        RunCmd::Reply { id, text } => {
            let r = Worker::new(&ctx)?.reply(id, text)?;
            Rendered::new(&r, format!("sent to {} ({})", r.issue_id, r.state))
        }
        RunCmd::Keys { id, keys } => {
            let r = Worker::new(&ctx)?.keys(id, keys)?;
            Rendered::new(&r, format!("keys sent to {} ({})", r.issue_id, r.state))
        }
        RunCmd::Nudge { id } => {
            let r = Worker::new(&ctx)?.nudge(id)?;
            Rendered::new(&r, format!("nudged {} ({})", r.issue_id, r.state))
        }
        RunCmd::Abort { id } => {
            let r = Worker::new(&ctx)?.abort(id)?;
            Rendered::new(
                &r,
                format!("{} aborted (claim released, worktree kept)", r.issue_id),
            )
        }
        RunCmd::Watch { once: _ } => {
            let w = Worker::new(&ctx)?.watch()?;
            let text = worker::transitions_text(&w);
            Rendered::new(&w, text)
        }
    }
}

/// The allocation plan for the current store snapshot (also used by `run start --approve`).
pub fn current_plan(ctx: &Ctx) -> Result<AllocationPlan, AppError> {
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
    Ok(scheduler::plan(&input, &ctx.config.repo.worktrees_dir)?)
}

fn plan(ctx: &Ctx) -> Result<Rendered, AppError> {
    let plan = current_plan(ctx)?;
    let running = ctx.store.active_claims()?.len();
    let view = PlanView {
        capacity: ctx.config.scheduler.max_parallel,
        running,
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
