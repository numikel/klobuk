//! `asf serve [--interval N] [--once | --until-done [--max-ticks N]]` — the orchestrator loop:
//! watch active runs → process the merge queue → promote ready issues → close completed epics
//! → (with `--dispatch`) start workers for the planned slots → sleep.
//!
//! A gate run takes 10 to 40 minutes, so in the loop it does not happen inside the tick. The tick
//! hands the gates to a `GateSlot` (one run in flight at a time, a thread that runs the gate commands
//! and nothing else) and the tick that finds the result concludes it. A tick therefore reads: conclude
//! a finished gate run → watch the agents → read review verdicts → promote, close epics, dispatch →
//! start the next gate run (a pending check of main, else the head of the merge queue, else the
//! verification of a finished run) → notify. Dispatch never waits for the queue, and a tick ends in
//! seconds. The tick report shows the run in flight as `gate_run`. When the loop starts, an item that a
//! stopped serve left in `rebasing`/`gating` goes back to the queue. `--once` has no loop to return to:
//! it runs the gates inside the call and returns when they are done.
//!
//! `--until-done` also checks after each tick whether to stop (`all_done`, `stuck`, `max_ticks`)
//! and prints a final `{ticks, reason, open, runs, questions_waiting}` summary.
//!
//! The config is re-read at the start of every tick (see `ConfigWatch`), so edits take effect
//! without restarting the loop. Tick reports and the summary carry `questions_waiting`: the
//! unanswered QUESTIONs to the human plus the runs in `blocked`, i.e. what waits for the operator.
//!
//! The loop also tells the human what waits for them (`[notify]`, see `remind`): each tick report
//! lists the `alerts` sent for things that just started to wait and the `reminders` for things that
//! waited too long. One-shot commands never notify, only `serve`.

use std::io::Write;
use std::time::Duration;

use asf_core::{IssueFilter, IssueId, IssueStore, IssueType, MergeState, RunState};
use asf_store::serve_pulse::{self, LoopState, Pulse, ServeMode};
use serde::Serialize;
use serde_json::json;

use crate::cli::{Cli, ServeArgs};
use crate::ctx::{ConfigWatch, Ctx};
use crate::output::{print_ok, AppError, Rendered};
use crate::tui::app::unanswered_questions;
use crate::worker::{self, GateMode, GateRun, GateSlot, Job, WatchResult, Worker};

use super::merge::{self, tick_all, TickResult};
use super::remind::{self, RemindState};

#[derive(Serialize)]
pub struct TickReport {
    pub promoted: Vec<IssueId>,
    pub closed_epics: Vec<IssueId>,
    pub watch: WatchResult,
    pub merge: TickResult,
    pub started: Vec<IssueId>,
    pub start_errors: Vec<serde_json::Value>,
    /// The gate run in flight when the tick ended (whose it is, which gate, for how many seconds);
    /// `null` when none. Always `null` for `--once`, which waits for its gates.
    pub gate_run: Option<GateRun>,
    /// This tick started a gate run or concluded one, which is news for the operator even when nothing
    /// else changed.
    #[serde(skip)]
    pub gate_event: bool,
    /// Unanswered QUESTIONs to the human plus runs in `blocked`; always present, `0` when none.
    pub questions_waiting: usize,
    /// Notifications sent in this tick for things that just started to wait for the human
    /// (`[notify] on_new`).
    pub alerts: Vec<remind::Sent>,
    /// Reminders sent in this tick for things that waited too long (`[notify]`).
    pub reminders: Vec<remind::Sent>,
}

fn blocked_runs(states: impl IntoIterator<Item = RunState>) -> usize {
    states.into_iter().filter(|s| *s == RunState::Blocked).count()
}

/// What waits for the operator: QUESTIONs to `human` nobody has answered (on issues that are not
/// finished) plus runs in `blocked` (an agent sitting on a prompt).
fn questions_waiting(ctx: &Ctx) -> Result<usize, AppError> {
    let mut n = 0;
    for issue in ctx
        .store
        .list_issues(&IssueFilter::default())?
        .iter()
        .filter(|i| !i.status.is_terminal())
    {
        n += unanswered_questions(&ctx.store.comments(&issue.id)?).len();
    }
    Ok(n + blocked_runs(ctx.store.runs(None, true)?.iter().map(|r| r.state)))
}

/// The line that tells the operator something needs them; empty when nothing does.
fn waiting_line(n: usize) -> String {
    if n == 0 {
        String::new()
    } else {
        format!("\n{n} waiting for you")
    }
}

/// One tick. With no `gate` slot (`--once`) the gates run inside the call, in the order the loop used
/// before gate runs left the tick; with one (the loop) they are handed to the slot.
fn tick(
    ctx: &Ctx,
    dispatch: bool,
    reminders: &mut RemindState,
    gate: Option<&mut GateSlot>,
) -> Result<TickReport, AppError> {
    match gate {
        None => tick_inline(ctx, dispatch, reminders),
        Some(slot) => tick_background(ctx, dispatch, reminders, slot),
    }
}

/// What `promote_and_dispatch` did.
struct Dispatched {
    promoted: Vec<IssueId>,
    closed_epics: Vec<IssueId>,
    started: Vec<IssueId>,
    start_errors: Vec<serde_json::Value>,
}

/// Promotes what became ready, closes finished epics and, with `--dispatch`, starts the planned slots.
fn promote_and_dispatch(ctx: &Ctx, worker: &Worker<'_>, dispatch: bool) -> Result<Dispatched, AppError> {
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
    Ok(Dispatched {
        promoted,
        closed_epics,
        started,
        start_errors,
    })
}

/// Order matters: merges close issues, and only then can their dependents be promoted.
fn tick_inline(ctx: &Ctx, dispatch: bool, reminders: &mut RemindState) -> Result<TickReport, AppError> {
    let worker = Worker::new(ctx)?;
    let watch = worker.watch()?;
    let merge = tick_all(ctx, false)?;
    let done = promote_and_dispatch(ctx, &worker, dispatch)?;
    // Last, so the notifications see the state this tick has just produced.
    let remind::Notices { alerts, reminders } =
        remind::send_due(ctx, &*worker.rt, reminders, time::OffsetDateTime::now_utc())?;
    Ok(TickReport {
        promoted: done.promoted,
        closed_epics: done.closed_epics,
        watch,
        merge,
        started: done.started,
        start_errors: done.start_errors,
        gate_run: None,
        gate_event: false,
        questions_waiting: questions_waiting(ctx)?,
        alerts,
        reminders,
    })
}

/// The loop's tick: it never waits for a gate run. A result that is ready is concluded first (merges
/// close issues, and only then can their dependents be promoted); the cheap watching comes next, so a
/// blocked or asking worker and a reviewer's verdict are seen while gates run; promotion and dispatch
/// come before a new gate run starts, so a long queue never delays a worker start. A red `main` after a
/// merge halts the tick: no further gate run starts on top of it.
fn tick_background(
    ctx: &Ctx,
    dispatch: bool,
    reminders: &mut RemindState,
    slot: &mut GateSlot,
) -> Result<TickReport, AppError> {
    let worker = Worker::new(ctx)?;
    let started_before = slot.started();
    let mut gate_event = false;
    let mut transitions = Vec::new();
    let mut items = Vec::new();
    if let Some(finished) = slot.take_finished() {
        gate_event = true;
        match finished.job {
            Job::Verify(job) => transitions.extend(worker.conclude_verification(job, finished.results)?),
            Job::Merge(job) => items.extend(merge::conclude_merge(ctx, job, finished.results)?),
            Job::Main(job) => items.push(merge::conclude_main(ctx, job, finished.results)?),
        }
    }
    let main_red = items.iter().any(merge::is_main_red);
    // what the clean-up did for the merges concluded above; poll and start_next report their own
    let mut cleanup = merge::take_cleanup();

    let mut watch = worker.watch_in(GateMode::Background)?;
    transitions.append(&mut watch.transitions);
    watch.transitions = transitions;
    // a merge would change the tree that a running check of main is testing (or that just came back red)
    let polled = merge::poll(ctx, main_red || slot.checks_main())?;
    items.extend(polled.items);
    cleanup.append(polled.cleanup);
    let done = promote_and_dispatch(ctx, &worker, dispatch)?;

    if !main_red {
        let next = merge::start_next(ctx, slot)?;
        items.extend(next.items);
        cleanup.append(next.cleanup);
        watch.transitions.extend(worker.start_verification(slot)?);
    }
    gate_event |= slot.started() != started_before;

    let remind::Notices { alerts, reminders } =
        remind::send_due(ctx, &*worker.rt, reminders, time::OffsetDateTime::now_utc())?;
    Ok(TickReport {
        promoted: done.promoted,
        closed_epics: done.closed_epics,
        watch,
        merge: TickResult {
            processed: items.len(),
            items,
            cleanup,
        },
        started: done.started,
        start_errors: done.start_errors,
        gate_run: slot.current(),
        gate_event,
        questions_waiting: questions_waiting(ctx)?,
        alerts,
        reminders,
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
    for c in &t.merge.cleanup.cleaned {
        s.push_str(&format!("\ncleaned {} ({})", c.target, c.issue));
    }
    for c in &t.merge.cleanup.refused {
        s.push_str(&format!(
            "\nkept {} ({}): {}",
            c.target,
            c.issue,
            c.reason.as_deref().unwrap_or("")
        ));
    }
    for id in &t.closed_epics {
        s.push_str(&format!("\nclosed epic {id}"));
    }
    for id in &t.started {
        s.push_str(&format!("\nstarted {id}"));
    }
    if let Some(g) = &t.gate_run {
        s.push_str(&format!(
            "\ngate run: {} of {} · gate {} · {}s",
            g.kind, g.issue, g.gate, g.seconds
        ));
    }
    for e in &t.start_errors {
        s.push_str(&format!("\nstart failed {}: {}", e["issue"], e["message"]));
    }
    for (label, list) in [("notified", &t.alerts), ("reminded", &t.reminders)] {
        for r in list {
            s.push_str(&format!("\n{label}: {}", r.text));
            if let Some(e) = &r.notify_error {
                s.push_str(&format!("\n  notification failed: {e}"));
            }
            if let Some(e) = &r.command_error {
                s.push_str(&format!("\n  command failed: {e}"));
            }
        }
    }
    s.push_str(&waiting_line(t.questions_waiting));
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
    /// A gate run is in flight, or the check of main after a merge is still owed. Such a run is not
    /// over until it has been concluded, even when no issue is open any more.
    gate_busy: bool,
    /// This serve will start a worker next tick: `--dispatch` is on and the allocation plan has a
    /// slot. Without `--dispatch` nothing here ever starts work, so a plan slot is no progress.
    can_start: bool,
    /// See `questions_waiting`.
    questions_waiting: usize,
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

fn assess(ctx: &Ctx, dispatch: bool, gate: &GateSlot) -> Result<Assessment, AppError> {
    let open: Vec<IssueId> = ctx
        .store
        .list_issues(&IssueFilter::default())?
        .into_iter()
        .filter(|i| i.kind != IssueType::Epic && !i.status.is_terminal())
        .map(|i| i.id)
        .collect();
    let runs = ctx.store.runs(None, true)?;
    let gate_busy = gate.is_busy() || merge::main_check_owed(ctx)?;
    let busy = gate_busy
        || runs.iter().any(|r| run_is_busy(r.state))
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
        gate_busy,
        can_start,
        questions_waiting: questions_waiting(ctx)?,
    })
}

/// Checked after every tick. `all_done` wins over `stuck`, which wins over `max_ticks`, so the
/// most informative reason is reported when several hold at once. A failed tick never yields
/// `stuck`: its half-applied state is not evidence that nothing can move. A gate run still in flight
/// (or a check of main still owed) keeps the loop going even with nothing open and past the tick limit:
/// the gate command is a child of this process and would outlive it, and the next serve would start a
/// second gate run in the same worktree. The run has a timeout, so the wait is bounded.
fn decide(a: &Assessment, ticks: u64, max_ticks: Option<u64>, tick_ok: bool) -> Option<StopReason> {
    if a.open.is_empty() && !a.gate_busy {
        return Some(StopReason::AllDone);
    }
    if tick_ok && !a.busy && !a.can_start {
        return Some(StopReason::Stuck);
    }
    if max_ticks.is_some_and(|n| ticks >= n) && !a.gate_busy {
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
    pub questions_waiting: usize,
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
    out.push_str(&waiting_line(s.questions_waiting));
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
        questions_waiting: a.questions_waiting,
    };
    let rendered = Rendered::new(&summary, summary_text(&summary))?;
    if reason == StopReason::Stuck {
        print_ok(cli.json, cli.quiet, &rendered);
        let _ = std::io::stdout().flush();
        std::process::exit(3);
    }
    Ok(rendered)
}

/// Seconds between ticks and the tick limit: a flag wins, else `[serve]` of the config in force.
fn limits(a: &ServeArgs, cfg: &asf_core::Config) -> (u64, Option<u64>) {
    (
        a.interval.unwrap_or(cfg.serve.interval_s),
        a.max_ticks.or(cfg.serve.max_ticks),
    )
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// Whether a new loop may start next to what the pulse on disk says. A fresh pulse of a live process
/// refuses (the message names the pid and the age); a clean stop, a stale pulse or a dead pid is taken
/// over. `alive` is the answer of the process lookup, `None` when it could not be made.
fn takeover_check(
    pulse: Option<&Pulse>,
    now: time::OffsetDateTime,
    alive: Option<bool>,
) -> Result<(), String> {
    match (pulse, serve_pulse::judge(pulse, now, alive)) {
        (Some(p), LoopState::Running { age_s, .. }) => Err(format!(
            "another `asf serve` loop is running on this repo (pid {}, last tick {} ago, started {}); \
             stop it first, or wait until it has been silent for {} s and this one takes over",
            p.pid,
            serve_pulse::fmt_age(age_s),
            p.started_at,
            (p.interval_s * serve_pulse::STALE_AFTER_INTERVALS).max(serve_pulse::MIN_STALE_SECS)
        )),
        _ => Ok(()),
    }
}

/// Writes the pulse every tick and the reason of a clean stop. A failed write only warns (once): the
/// loop's work matters more than its signal.
struct Heartbeat {
    root: std::path::PathBuf,
    pulse: Pulse,
    warned: bool,
}

impl Heartbeat {
    /// Starts the pulse and writes it at once, so the TUI sees the loop before its first tick ends.
    fn start(root: &std::path::Path, mode: ServeMode, interval_s: u64, max_ticks: Option<u64>) -> Heartbeat {
        let mut hb = Heartbeat {
            root: root.to_path_buf(),
            pulse: Pulse::start(mode, interval_s, max_ticks, &now_rfc3339()),
            warned: false,
        };
        hb.save();
        hb
    }

    /// The limits in force this tick (an edit of `[serve]` reaches the next tick).
    fn set_limits(&mut self, interval_s: u64, max_ticks: Option<u64>) {
        self.pulse.interval_s = interval_s;
        self.pulse.max_ticks = max_ticks;
    }

    fn beat(&mut self, ticks: u64) {
        self.pulse.tick(ticks, &now_rfc3339());
        self.save();
    }

    fn stop(&mut self, reason: impl Into<String>) {
        self.pulse.stop(reason, &now_rfc3339());
        self.save();
    }

    fn save(&mut self) {
        if let Err(e) = serve_pulse::write(&self.root, &self.pulse) {
            if !self.warned {
                self.warned = true;
                eprintln!(
                    "asf serve: cannot write the pulse {}: {e}",
                    serve_pulse::pulse_path(&self.root).display()
                );
            }
        }
    }
}

pub fn run(cli: &Cli, a: &ServeArgs) -> Result<Rendered, AppError> {
    let mut ctx = Ctx::load(cli)?;
    let mut reminders = RemindState::new();
    if a.once {
        let t = tick(&ctx, a.dispatch, &mut reminders, None)?;
        return Rendered::new(&t, text(&t));
    }
    let (interval, max_ticks) = limits(a, &ctx.config);
    let on_disk = serve_pulse::read(&ctx.root).ok().flatten();
    let alive = on_disk
        .as_ref()
        .filter(|p| p.stopped_at.is_none())
        .and_then(|p| serve_pulse::pid_alive(p.pid));
    takeover_check(on_disk.as_ref(), time::OffsetDateTime::now_utc(), alive).map_err(AppError::conflict)?;
    if on_disk.as_ref().is_some_and(|p| p.stopped_at.is_none()) {
        eprintln!(
            "asf serve: the previous loop left no stop reason (killed, crashed or restarted); taking over"
        );
    }
    let mode = if a.dispatch {
        ServeMode::Dispatch
    } else {
        ServeMode::Observe
    };
    eprintln!(
        "asf serve: every {interval}s{} (Ctrl+C to stop)",
        if a.dispatch { " with dispatch" } else { "" }
    );
    // What a stopped serve left mid-way would never be touched again: no gate run is behind it now.
    let recovered = merge::recover_orphans(&ctx)?;
    if !recovered.is_empty() {
        let ids: Vec<String> = recovered.iter().map(ToString::to_string).collect();
        eprintln!(
            "asf serve: {} merge item(s) left in rebasing/gating by an earlier run went back to the queue: {}",
            ids.len(),
            ids.join(", ")
        );
    }
    let mut gate = GateSlot::new();
    let mut hb = Heartbeat::start(&ctx.root, mode, interval, max_ticks);
    let result = serve_loop(cli, a, &mut ctx, &mut reminders, &mut gate, &mut hb);
    if let Err(e) = &result {
        if hb.pulse.stopped_at.is_none() {
            hb.stop(format!("error: {}", e.message));
        }
    }
    // Whatever ended the loop (an error included), a gate command still running is a child of this
    // process: wait for it so that it does not outlive asf. Its result is not concluded here; the item
    // is still `gating`/`verifying` in the store and the next start deals with it like with any other.
    if gate.is_busy() {
        eprintln!("asf serve: waiting for the gate run in flight before it exits");
        let _ = gate.wait_finished();
    }
    result
}

fn serve_loop(
    cli: &Cli,
    a: &ServeArgs,
    ctx: &mut Ctx,
    reminders: &mut RemindState,
    gate: &mut GateSlot,
    hb: &mut Heartbeat,
) -> Result<Rendered, AppError> {
    let mut watch = ConfigWatch::new(ctx);
    let mut ticks: u64 = 0;
    loop {
        ticks += 1;
        if let Some(warning) = watch.refresh(ctx) {
            eprintln!("asf serve: {warning}");
        }
        let (interval, max_ticks) = limits(a, &ctx.config);
        hb.set_limits(interval, max_ticks);
        let tick_ok = match tick(ctx, a.dispatch, reminders, Some(&mut *gate)) {
            Ok(t) => {
                if !t.promoted.is_empty()
                    || !t.closed_epics.is_empty()
                    || !t.watch.transitions.is_empty()
                    || !t.merge.items.is_empty()
                    || !t.started.is_empty()
                    || !t.start_errors.is_empty()
                    || !t.alerts.is_empty()
                    || !t.reminders.is_empty()
                    || t.gate_event
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
        hb.beat(ticks);
        if a.until_done {
            let assessment = assess(ctx, a.dispatch, gate)?;
            if let Some(reason) = decide(&assessment, ticks, hb.pulse.max_ticks, tick_ok) {
                // before `finish`, which exits the process for `stuck`
                hb.stop(reason.as_str());
                return finish(cli, ticks, reason, assessment);
            }
        }
        std::thread::sleep(Duration::from_secs(hb.pulse.interval_s));
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
            gate_busy: false,
            can_start,
            questions_waiting: 0,
        }
    }

    #[test]
    fn a_flag_wins_over_the_config_and_the_config_over_the_default() {
        let mut cfg = asf_core::Config::default();
        let args = |interval, max_ticks| ServeArgs {
            interval,
            once: false,
            dispatch: false,
            until_done: true,
            max_ticks,
        };
        assert_eq!(limits(&args(None, None), &cfg), (10, None));
        cfg.serve.interval_s = 3;
        cfg.serve.max_ticks = Some(600);
        assert_eq!(limits(&args(None, None), &cfg), (3, Some(600)));
        assert_eq!(limits(&args(Some(7), Some(2)), &cfg), (7, Some(2)));
    }

    fn pulse_at(last_tick: &str, interval: u64) -> Pulse {
        Pulse::start(ServeMode::Dispatch, interval, None, last_tick)
    }

    #[test]
    fn a_second_loop_is_refused_only_while_the_first_one_is_fresh_and_alive() {
        use time::format_description::well_known::Rfc3339;
        let now = time::OffsetDateTime::parse("2026-11-11T20:00:00Z", &Rfc3339).unwrap();
        let fresh = pulse_at("2026-11-11T19:59:55Z", 10);
        let stale = pulse_at("2026-11-11T19:00:00Z", 10);
        let msg = takeover_check(Some(&fresh), now, Some(true)).unwrap_err();
        assert!(
            msg.contains("another `asf serve` loop") && msg.contains("5 s"),
            "{msg}"
        );
        assert!(
            takeover_check(Some(&fresh), now, None).is_err(),
            "unknown liveness: age decides"
        );
        assert!(
            takeover_check(Some(&fresh), now, Some(false)).is_ok(),
            "dead pid is taken over"
        );
        assert!(
            takeover_check(Some(&stale), now, Some(true)).is_ok(),
            "a stale pulse is taken over"
        );
        assert!(takeover_check(None, now, None).is_ok());
        let mut stopped = fresh.clone();
        stopped.stop("all_done", "2026-11-11T19:59:59Z");
        assert!(
            takeover_check(Some(&stopped), now, Some(true)).is_ok(),
            "a clean stop is free"
        );
    }

    #[test]
    fn only_blocked_runs_wait_for_the_operator() {
        assert_eq!(blocked_runs(RunState::ALL.iter().copied()), 1);
        assert_eq!(
            blocked_runs([RunState::Blocked, RunState::Working, RunState::Blocked]),
            2
        );
        assert_eq!(blocked_runs(Vec::new()), 0);
    }

    #[test]
    fn the_waiting_line_is_silent_at_zero() {
        assert_eq!(waiting_line(0), "");
        assert_eq!(waiting_line(3), "\n3 waiting for you");
    }

    #[test]
    fn no_open_issue_is_all_done_even_at_the_tick_limit() {
        let a = assessment(0, false, false);
        assert_eq!(decide(&a, 1, None, true), Some(StopReason::AllDone));
        assert_eq!(decide(&a, 3, Some(3), true), Some(StopReason::AllDone));
    }

    #[test]
    fn a_gate_run_in_flight_is_not_all_done_even_with_nothing_open() {
        let a = Assessment {
            gate_busy: true,
            busy: true,
            ..assessment(0, false, false)
        };
        assert_eq!(
            decide(&a, 1, None, true),
            None,
            "stopping would kill the gate run"
        );
        assert_eq!(
            decide(&a, 4, Some(4), true),
            None,
            "not even the tick limit ends the loop under a gate run: its process would outlive asf"
        );
        let over = Assessment {
            gate_busy: false,
            busy: false,
            ..assessment(1, false, true)
        };
        assert_eq!(
            decide(&over, 5, Some(4), true),
            Some(StopReason::MaxTicks),
            "once the run is concluded the limit applies"
        );
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
