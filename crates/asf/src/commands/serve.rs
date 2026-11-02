//! `asf serve [--interval N] [--once]` — the orchestrator loop:
//! watch active runs → process the merge queue → promote ready issues → close completed epics
//! → (with `--dispatch`) start workers for the planned slots → sleep.

use std::time::Duration;

use asf_core::{IssueId, IssueStore};
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
    loop {
        match tick(&ctx, a.dispatch) {
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
            }
            Err(e) => eprintln!("asf serve: tick failed: {}", e.message),
        }
        std::thread::sleep(Duration::from_secs(a.interval));
    }
}
