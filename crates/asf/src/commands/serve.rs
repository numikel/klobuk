//! `asf serve [--interval N] [--once]` — the orchestrator loop:
//! promote ready issues → watch active runs → process the merge queue → sleep.

use std::time::Duration;

use asf_core::{IssueId, IssueStore};
use serde::Serialize;

use crate::cli::{Cli, ServeArgs};
use crate::ctx::Ctx;
use crate::output::{print_ok, AppError, Rendered};
use crate::worker::{self, WatchResult, Worker};

use super::merge::{tick_all, TickResult};

#[derive(Serialize)]
pub struct TickReport {
    pub promoted: Vec<IssueId>,
    pub watch: WatchResult,
    pub merge: TickResult,
}

/// Order matters: merges close issues, and only then can their dependents be promoted.
fn tick(ctx: &Ctx) -> Result<TickReport, AppError> {
    let watch = Worker::new(ctx)?.watch()?;
    let merge = tick_all(ctx, false)?;
    let promoted = ctx.store.promote_ready()?;
    Ok(TickReport {
        promoted,
        watch,
        merge,
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
    s
}

pub fn run(cli: &Cli, a: &ServeArgs) -> Result<Rendered, AppError> {
    let ctx = Ctx::load(cli)?;
    if a.once {
        let t = tick(&ctx)?;
        return Rendered::new(&t, text(&t));
    }
    eprintln!("asf serve: every {}s (Ctrl+C to stop)", a.interval);
    loop {
        match tick(&ctx) {
            Ok(t) => {
                if !t.promoted.is_empty() || !t.watch.transitions.is_empty() || !t.merge.items.is_empty() {
                    print_ok(cli.json, cli.quiet, &Rendered::new(&t, text(&t))?);
                }
            }
            Err(e) => eprintln!("asf serve: tick failed: {}", e.message),
        }
        std::thread::sleep(Duration::from_secs(a.interval));
    }
}
