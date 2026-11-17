//! `asf run …` — allocation plan and worker lifecycle.

use asf_core::scheduler::{self, AllocationPlan, RunCounts, SchedulerInput};
use asf_core::{IssueFilter, IssueId, IssueStore, RunState};
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
    /// `[scheduler] max_parallel` from config.
    capacity: usize,
    /// Active worker runs (they use up `capacity`; a claim without an active run does not).
    running: usize,
    /// `[review] max_parallel` from config.
    review_capacity: usize,
    /// Active reviewer runs (they use up `review_capacity`, never worker slots).
    reviewers_running: usize,
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
        RunCmd::Reply { id, text, budget } => {
            let w = Worker::new(&ctx)?;
            // Raised before the prompt goes out: the worker's next hook call must already see the new limit.
            let raised = budget.map(|usd| raise_budget(&ctx, id, usd)).transpose()?;
            let r = w.reply(id, text)?;
            let note = raised.map(|b| format!("; {}", b.text())).unwrap_or_default();
            Rendered::new(&r, format!("sent to {} ({}){note}", r.issue_id, r.state))
        }
        RunCmd::Budget { id, usd } => {
            let b = raise_budget(&ctx, id, *usd)?;
            let text = b.text();
            Rendered::new(&b, text)
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
        RunCmd::Verify { id } => {
            let t = Worker::new(&ctx)?.verify_issue(id)?;
            let text = format!(
                "{} {} -> {}: {}",
                t.issue_id,
                t.from,
                t.to,
                t.detail.lines().next().unwrap_or("")
            );
            Rendered::new(&t, text)
        }
        RunCmd::Watch { once: _ } => {
            let w = Worker::new(&ctx)?.watch()?;
            let text = worker::transitions_text(&w);
            Rendered::new(&w, text)
        }
    }
}

/// What `asf run budget` did to the latest run of an issue.
#[derive(Serialize)]
pub struct BudgetRaised {
    pub issue_id: IssueId,
    pub run_id: String,
    /// `None` when the run had no budget.
    pub old: Option<f64>,
    pub new: f64,
    pub cost_usd: f64,
    pub state_before: RunState,
    pub state: RunState,
}

impl BudgetRaised {
    pub fn text(&self) -> String {
        let usd = |v: f64| format!("${v:.2}");
        let moved = if self.state_before == self.state {
            String::new()
        } else {
            format!("; {} -> {}", self.state_before, self.state)
        };
        format!(
            "{} budget {} -> {} (cost {}{moved})",
            self.issue_id,
            self.old.map_or_else(|| "none".to_string(), usd),
            usd(self.new),
            usd(self.cost_usd),
        )
    }
}

/// Sets the budget of the latest run of `id` (also for the TUI key). An `over_budget` run goes back to
/// `working`; a value below the run's cost is a usage error, because the hook would trip it again.
pub fn raise_budget(ctx: &Ctx, id: &IssueId, usd: f64) -> Result<BudgetRaised, AppError> {
    if !usd.is_finite() || usd < 0.0 {
        return Err(AppError::usage(format!(
            "budget must be a number of USD >= 0, got {usd}"
        )));
    }
    let run = ctx
        .store
        .runs(Some(id), false)?
        .pop()
        .ok_or_else(|| AppError::not_found(format!("no run for {id}")))?;
    if usd < run.cost_usd {
        return Err(AppError::usage(format!(
            "budget ${usd:.2} is below the ${:.2} {id} has already cost; use at least that",
            run.cost_usd
        )));
    }
    let (before, after) = ctx.store.raise_run_budget(&run.id, usd, &ctx.author())?;
    Ok(BudgetRaised {
        issue_id: after.issue_id,
        run_id: after.id,
        old: before.budget_usd,
        new: usd,
        cost_usd: after.cost_usd,
        state_before: before.state,
        state: after.state,
    })
}

/// Active runs of the current store snapshot, workers apart from reviewers.
pub fn active_run_counts(ctx: &Ctx) -> Result<RunCounts, AppError> {
    let issues = ctx.store.list_issues(&IssueFilter::default())?;
    let runs = ctx.store.runs(None, true)?;
    Ok(scheduler::count_active_runs(&issues, &runs))
}

/// The allocation plan for the current store snapshot (also used by `run start --approve`).
///
/// Claims decide scope conflicts; only active worker runs use up `[scheduler] max_parallel`.
pub fn current_plan(ctx: &Ctx) -> Result<AllocationPlan, AppError> {
    scheduler::validate_scope(&ctx.config.scheduler.hotspots)
        .map_err(|e| AppError::validation(format!("scheduler.hotspots: {e}")))?;
    let issues = ctx.store.list_issues(&IssueFilter::default())?;
    let deps = ctx.store.deps()?;
    let claims = ctx.store.active_claims()?;
    let runs = ctx.store.runs(None, true)?;
    let tracked = git::tracked_files(&ctx.root)?;
    let input = SchedulerInput {
        issues: &issues,
        deps: &deps,
        claims: &claims,
        tracked_files: &tracked,
        active_workers: scheduler::count_active_runs(&issues, &runs).workers,
        max_parallel: ctx.config.scheduler.max_parallel,
        hotspots: &ctx.config.scheduler.hotspots,
    };
    Ok(scheduler::plan(&input, &ctx.config.repo.worktrees_dir)?)
}

fn plan(ctx: &Ctx) -> Result<Rendered, AppError> {
    let plan = current_plan(ctx)?;
    let counts = active_run_counts(ctx)?;
    let view = PlanView {
        capacity: ctx.config.scheduler.max_parallel,
        running: counts.workers,
        review_capacity: ctx.config.review.max_parallel as usize,
        reviewers_running: counts.reviewers,
        plan,
    };

    let mut text = format!(
        "plan {} — workers {}/{} running, reviewers {}/{} running\n",
        view.plan.inputs_hash, view.running, view.capacity, view.reviewers_running, view.review_capacity
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
