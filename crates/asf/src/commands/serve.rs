//! `asf serve [--interval N] [--once | --until-done [--max-ticks N]]` — the orchestrator loop:
//! watch active runs → process the merge queue → promote ready issues → close completed epics
//! → (with `--dispatch`) start workers for the planned slots → sleep.
//!
//! `--until-done` also checks after each tick whether to stop (`all_done`, `stuck`, `max_ticks`)
//! and prints a final `{ticks, reason, open, runs}` summary.

use std::io::Write;
use std::time::Duration;

use asf_core::{IssueFilter, IssueId, IssueStore, IssueType, MergeState, RunState};
use serde::Serialize;
use serde_json::json;

use crate::cli::{Cli, ServeArgs};
use crate::ctx::Ctx;
use crate::output::{print_ok, AppError, Rendered};
use crate::worker::{self, WatchResult, Worker};

use super::merge::{tick_all, TickResult};

#[derive(Serialize)]
pub struct TickReport {
    pub promoted: Vec<IssueId>,
    pub closed_epics: Vec<IssueId>,
    pub watch: WatchResult,
    pub merge: TickResult,
    pub started: Vec<IssueId>,
    pub start_errors: Vec<serde_json::Value>,
}

/// Order matters: merges close issues, and only then can their dependents be promoted.
fn tick(ctx: &Ctx, dispatch: bool) -> Result<TickReport, AppError> {
    let worker = Worker::new(ctx)?;
    let watch = worker.watch()?;
    let merge = tick_all(ctx, false)?;
    let promoted = ctx.store.promote_ready()?;
    let closed_epics = ctx.store.close_completed_epics()?;
    let mut started = Vec::new();
    let mut start_errors = Vec::new();
    if dispatch {
        let plan = super::run::current_plan(ctx)?;
        for slot in plan.slots {
            match worker.start(&slot.issue, None) {
                Ok(_) => started.push(slot.issue),
                Err(e) => {
                    start_errors.push(json!({ "issue": slot.issue, "code": e.code, "message": e.message }))
                }
            }
        }
    }
    Ok(TickReport {
        promoted,
        closed_epics,
        watch,
        merge,
        started,
        start_errors,
    })
}

fn text(t: &TickReport) -> String {
    let mut s = format!(
        "tick: promoted {} · watched {} run(s), {} transition(s) · merged {} item(s)",
        t.promoted.len(),
        t.watch.checked,
        t.watch.transitions.len(),
        t.merge
            .items
            .iter()
            .filter(|m| m.state == asf_core::MergeState::Merged)
            .count()
    );
    if !t.watch.transitions.is_empty() {
        s.push('\n');
        s.push_str(&worker::transitions_text(&t.watch));
    }
    for m in &t.merge.items {
        s.push_str(&format!("\nmerge #{} {} -> {}", m.id, m.issue_id, m.state));
    }
    for id in &t.closed_epics {
        s.push_str(&format!("\nclosed epic {id}"));
    }
    for id in &t.started {
        s.push_str(&format!("\nstarted {id}"));
    }
    for e in &t.start_errors {
        s.push_str(&format!("\nstart failed {}: {}", e["issue"], e["message"]));
    }
    s
}

/// Why `--until-done` stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    AllDone,
    MaxTicks,
    Stuck,
}

impl StopReason {
    fn as_str(self) -> &'static str {
        match self {
            StopReason::AllDone => "all_done",
            StopReason::MaxTicks => "max_ticks",
            StopReason::Stuck => "stuck",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RunBrief {
    pub issue: IssueId,
    pub state: RunState,
}

/// What the store looks like right after a tick, as far as the stop decision cares.
#[derive(Debug, Clone)]
struct Assessment {
    /// Open (non-terminal) issues that are not epics.
    open: Vec<IssueId>,
    /// Every non-terminal run, including `blocked`, `stalled` and `over_budget`.
    runs: Vec<RunBrief>,
    /// A run is starting/working/finishing/verifying or a merge item is in flight.
    busy: bool,
    /// This serve will start a worker next tick: `--dispatch` is on and the allocation plan has a
    /// slot. Without `--dispatch` nothing here ever starts work, so a plan slot is no progress.
    can_start: bool,
}

fn run_is_busy(s: RunState) -> bool {
    matches!(
        s,
        RunState::Starting | RunState::Working | RunState::Finishing | RunState::Verifying
    )
}

fn merge_is_busy(s: MergeState) -> bool {
    matches!(
        s,
        MergeState::Queued
            | MergeState::Rebasing
            | MergeState::Gating
            | MergeState::Merging
            | MergeState::PrOpen
    )
}

fn assess(ctx: &Ctx, dispatch: bool) -> Result<Assessment, AppError> {
    let open: Vec<IssueId> = ctx
        .store
        .list_issues(&IssueFilter::default())?
        .into_iter()
        .filter(|i| i.kind != IssueType::Epic && !i.status.is_terminal())
        .map(|i| i.id)
        .collect();
    let runs = ctx.store.runs(None, true)?;
    let busy = runs.iter().any(|r| run_is_busy(r.state))
        || ctx.store.merge_queue()?.iter().any(|m| merge_is_busy(m.state));
    // The plan shells out to git, so only ask for it when its answer decides "stuck".
    let can_start = if !dispatch || open.is_empty() || busy {
        false
    } else {
        !super::run::current_plan(ctx)?.slots.is_empty()
    };
    Ok(Assessment {
        open,
        runs: runs
            .into_iter()
            .map(|r| RunBrief {
                issue: r.issue_id,
                state: r.state,
            })
            .collect(),
        busy,
        can_start,
    })
}

/// Checked after every tick. `all_done` wins over `stuck`, which wins over `max_ticks`, so the
/// most informative reason is reported when several hold at once. A failed tick never yields
/// `stuck`: its half-applied state is not evidence that nothing can move.
fn decide(a: &Assessment, ticks: u64, max_ticks: Option<u64>, tick_ok: bool) -> Option<StopReason> {
    if a.open.is_empty() {
        return Some(StopReason::AllDone);
    }
    if tick_ok && !a.busy && !a.can_start {
        return Some(StopReason::Stuck);
    }
    if max_ticks.is_some_and(|n| ticks >= n) {
        return Some(StopReason::MaxTicks);
    }
    None
}

#[derive(Serialize)]
pub struct Summary {
    pub ticks: u64,
    pub reason: StopReason,
    pub open: Vec<IssueId>,
    pub runs: Vec<RunBrief>,
}

fn summary_text(s: &Summary) -> String {
    let mut out = format!(
        "serve stopped: {} after {} tick(s) · {} open issue(s) · {} unfinished run(s)",
        s.reason.as_str(),
        s.ticks,
        s.open.len(),
        s.runs.len()
    );
    if s.reason != StopReason::AllDone {
        for id in &s.open {
            out.push_str(&format!("\nopen {id}"));
        }
        for r in &s.runs {
            out.push_str(&format!("\nrun {} {}", r.issue, r.state));
        }
    }
    out
}

/// `stuck` is a successful report with exit 3, which `Rendered` cannot express, so it is printed
/// and the process exits here. The `ok:false` envelope stays reserved for errors.
fn finish(cli: &Cli, ticks: u64, reason: StopReason, a: Assessment) -> Result<Rendered, AppError> {
    let summary = Summary {
        ticks,
        reason,
        open: a.open,
        runs: a.runs,
    };
    let rendered = Rendered::new(&summary, summary_text(&summary))?;
    if reason == StopReason::Stuck {
        print_ok(cli.json, cli.quiet, &rendered);
        let _ = std::io::stdout().flush();
        std::process::exit(3);
    }
    Ok(rendered)
}

pub fn run(cli: &Cli, a: &ServeArgs) -> Result<Rendered, AppError> {
    let ctx = Ctx::load(cli)?;
    if a.once {
        let t = tick(&ctx, a.dispatch)?;
        return Rendered::new(&t, text(&t));
    }
    eprintln!(
        "asf serve: every {}s{} (Ctrl+C to stop)",
        a.interval,
        if a.dispatch { " with dispatch" } else { "" }
    );
    let mut ticks: u64 = 0;
    loop {
        ticks += 1;
        let tick_ok = match tick(&ctx, a.dispatch) {
            Ok(t) => {
                if !t.promoted.is_empty()
                    || !t.closed_epics.is_empty()
                    || !t.watch.transitions.is_empty()
                    || !t.merge.items.is_empty()
                    || !t.started.is_empty()
                    || !t.start_errors.is_empty()
                {
                    print_ok(cli.json, cli.quiet, &Rendered::new(&t, text(&t))?);
                }
                true
            }
            Err(e) => {
                eprintln!("asf serve: tick failed: {}", e.message);
                false
            }
        };
        if a.until_done {
            let assessment = assess(&ctx, a.dispatch)?;
            if let Some(reason) = decide(&assessment, ticks, a.max_ticks, tick_ok) {
                return finish(cli, ticks, reason, assessment);
            }
        }
        std::thread::sleep(Duration::from_secs(a.interval));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u32) -> IssueId {
        format!("asf-{n}").parse().unwrap()
    }

    fn assessment(open: usize, busy: bool, can_start: bool) -> Assessment {
        Assessment {
            open: (1..=open as u32).map(id).collect(),
            runs: Vec::new(),
            busy,
            can_start,
        }
    }

    #[test]
    fn no_open_issue_is_all_done_even_at_the_tick_limit() {
        let a = assessment(0, false, false);
        assert_eq!(decide(&a, 1, None, true), Some(StopReason::AllDone));
        assert_eq!(decide(&a, 3, Some(3), true), Some(StopReason::AllDone));
    }

    #[test]
    fn idle_without_a_slot_is_stuck_after_one_tick() {
        let a = assessment(2, false, false);
        assert_eq!(decide(&a, 1, None, true), Some(StopReason::Stuck));
        assert_eq!(decide(&a, 5, Some(5), true), Some(StopReason::Stuck));
    }

    #[test]
    fn busy_or_plannable_work_keeps_going_until_the_limit() {
        for a in [assessment(1, true, false), assessment(1, false, true)] {
            assert_eq!(decide(&a, 1, None, true), None);
            assert_eq!(decide(&a, 1, Some(2), true), None);
            assert_eq!(decide(&a, 2, Some(2), true), Some(StopReason::MaxTicks));
        }
    }

    #[test]
    fn a_failed_tick_is_never_stuck_but_still_counts_towards_the_limit() {
        let a = assessment(1, false, false);
        assert_eq!(decide(&a, 1, Some(3), false), None);
        assert_eq!(decide(&a, 3, Some(3), false), Some(StopReason::MaxTicks));
    }

    #[test]
    fn busy_states_follow_the_spec() {
        for s in RunState::ALL {
            let expect = matches!(
                s,
                RunState::Starting | RunState::Working | RunState::Finishing | RunState::Verifying
            );
            assert_eq!(run_is_busy(*s), expect, "{s}");
        }
        for s in MergeState::ALL {
            let expect = matches!(
                s,
                MergeState::Queued
                    | MergeState::Rebasing
                    | MergeState::Gating
                    | MergeState::Merging
                    | MergeState::PrOpen
            );
            assert_eq!(merge_is_busy(*s), expect, "{s}");
        }
    }
}
