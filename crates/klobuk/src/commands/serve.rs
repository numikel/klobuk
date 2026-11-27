//! `klobuk serve [--interval N] [--once | --until-done [--max-ticks N]]` — the orchestrator loop:
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
//! and prints a final `{ticks, reason, open, runs, questions_waiting, waiting}` summary.
//!
//! **Waiting is the default.** Nothing runs, nothing can start and open issues remain: that is either
//! a loop *parked on the owner* or a true dead end, and the line between them is `owner_waits`, the
//! definition that the NEEDS YOU rows and the reminders use (a BLOCKER announcement apart, which
//! fades by itself). Parked on the owner: a worker waiting for input, an unanswered QUESTION to the
//! human, a blocked issue, a stalled, over-budget or failed run, a failed or blocked merge item. The
//! loop keeps ticking, announces once what it waits for, writes it to the pulse and goes on when the
//! answer arrives. A dead end ends with `stuck` (exit 3): nothing asks for the owner, so no answer
//! can change anything (a dependency on a cancelled issue or in a cycle, a ready issue when the loop
//! runs without `--dispatch`). `--exit-when-waiting` ends a parked loop with `stuck` too, for
//! scripts that want the exit code. `--max-ticks` ends either mode.
//!
//! The config is re-read at the start of every tick (see `ConfigWatch`), so edits take effect
//! without restarting the loop. Tick reports and the summary carry `questions_waiting`: the
//! unanswered QUESTIONs to the human plus the runs in `blocked`, i.e. what waits for the operator.
//!
//! The loop writes a pulse (`.factory/runs/serve.json`, see `klobuk_store::serve_pulse`) at start and after
//! every tick, and the reason when it stops on its own (`all_done`, `max_ticks`, `stuck`, `error: ..`).
//! The TUI header and banner and `klobuk repo doctor` read it; a killed loop leaves a pulse that goes
//! stale. A second loop on a repo whose pulse is fresh and whose pid is alive is refused, a stale or
//! stopped pulse is taken over. `--interval` and `--max-ticks` win over `[serve]`; the config is
//! re-read every tick. `--once` writes no pulse.
//!
//! The loop also tells the human what waits for them (`[notify]`, see `remind`): each tick report
//! lists the `alerts` sent for things that just started to wait and the `reminders` for things that
//! waited too long. One-shot commands never notify, only `serve`.

use std::io::Write;
use std::time::Duration;

use klobuk_core::remind::{EventIndex, WaitKind};
use klobuk_core::{IssueFilter, IssueId, IssueStore, IssueType, MergeState, RunState};
use klobuk_store::serve_pulse::{self, LoopState, Pulse, PulseWait, ServeMode};
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

/// One thing the idle loop waits for the owner on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OwnerWait {
    pub issue: IssueId,
    pub kind: WaitKind,
    /// A few plain words on why (a question's text, a merge error); may be empty.
    pub reason: String,
}

impl OwnerWait {
    /// What the pulse, the TUI header and the notice call it.
    fn what(&self) -> &'static str {
        match self.kind {
            WaitKind::Question => "question",
            WaitKind::RunBlocked => "worker waits for input",
            WaitKind::RunStalled => "worker stalled",
            WaitKind::RunOverBudget => "worker over budget",
            WaitKind::RunFailed => "run failed",
            WaitKind::IssueBlocked => "issue blocked",
            WaitKind::MergeFailed => "merge failed",
            WaitKind::MergeBlocked => "merge blocked",
            WaitKind::Blocker => "blocker",
        }
    }

    fn line(&self) -> String {
        match self.reason.as_str() {
            "" => format!("{}: {}", self.issue, self.what()),
            r => format!("{}: {} — {r}", self.issue, self.what()),
        }
    }
}

/// What waits for the owner right now, most urgent first: the NEEDS YOU / reminder rule
/// (`remind::collect`). A BLOCKER comment is left out: it is an announcement that fades after hours, not
/// a state, and whatever it blocks shows up as a blocked issue or run.
fn owner_waits(ctx: &Ctx) -> Result<Vec<OwnerWait>, AppError> {
    let now = time::OffsetDateTime::now_utc();
    Ok(remind::collect(ctx, &EventIndex::default(), now)?
        .into_iter()
        .filter(|w| w.kind != WaitKind::Blocker)
        .map(|w| OwnerWait {
            issue: w.issue,
            kind: w.kind,
            reason: w.reason,
        })
        .collect())
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
///
/// `beat` tells the pulse that the tick is alive; it runs around every worker start, which can block
/// for the whole start window.
fn tick(
    ctx: &Ctx,
    dispatch: bool,
    reminders: &mut RemindState,
    gate: Option<&mut GateSlot>,
    beat: &mut dyn FnMut(),
) -> Result<TickReport, AppError> {
    match gate {
        None => tick_inline(ctx, dispatch, reminders, beat),
        Some(slot) => tick_background(ctx, dispatch, reminders, slot, beat),
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
fn promote_and_dispatch(
    ctx: &Ctx,
    worker: &Worker<'_>,
    dispatch: bool,
    beat: &mut dyn FnMut(),
) -> Result<Dispatched, AppError> {
    let promoted = ctx.store.promote_ready()?;
    let closed_epics = ctx.store.close_completed_epics()?;
    let mut started = Vec::new();
    let mut start_errors = Vec::new();
    if dispatch {
        let plan = super::run::current_plan(ctx)?;
        let slots = plan.slots.into_iter().map(|s| s.issue).collect();
        (started, start_errors) = start_slots(slots, beat, |id| worker.start(id, None).map(drop));
    }
    Ok(Dispatched {
        promoted,
        closed_epics,
        started,
        start_errors,
    })
}

/// Starts the planned issues one after another. A start blocks for up to the start window, so a batch
/// outlasts any pulse limit unless `beat` runs around each one (before it, and after it, failed or not).
fn start_slots(
    slots: Vec<IssueId>,
    beat: &mut dyn FnMut(),
    mut start: impl FnMut(&IssueId) -> Result<(), AppError>,
) -> (Vec<IssueId>, Vec<serde_json::Value>) {
    let mut started = Vec::new();
    let mut start_errors = Vec::new();
    for issue in slots {
        beat();
        let outcome = start(&issue);
        beat();
        match outcome {
            Ok(()) => started.push(issue),
            Err(e) => start_errors.push(json!({ "issue": issue, "code": e.code, "message": e.message })),
        }
    }
    (started, start_errors)
}

/// Order matters: merges close issues, and only then can their dependents be promoted.
fn tick_inline(
    ctx: &Ctx,
    dispatch: bool,
    reminders: &mut RemindState,
    beat: &mut dyn FnMut(),
) -> Result<TickReport, AppError> {
    let worker = Worker::new(ctx)?;
    let watch = worker.watch()?;
    let merge = tick_all(ctx, false)?;
    let done = promote_and_dispatch(ctx, &worker, dispatch, beat)?;
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
    beat: &mut dyn FnMut(),
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
    let done = promote_and_dispatch(ctx, &worker, dispatch, beat)?;

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
            .filter(|m| m.state == klobuk_core::MergeState::Merged)
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
    /// Everything that waits for the owner (see `owner_waits`), whether or not the loop is idle.
    waits: Vec<OwnerWait>,
}

impl Assessment {
    /// Nothing runs and nothing can start, but something waits for the owner: the loop is parked on them.
    fn parked_on_owner(&self) -> bool {
        !self.busy && !self.can_start && !self.waits.is_empty()
    }
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
        waits: owner_waits(ctx)?,
    })
}

/// Checked after every tick. `all_done` wins over `stuck`, which wins over `max_ticks`, so the
/// most informative reason is reported when several hold at once. A failed tick never yields
/// `stuck`: its half-applied state is not evidence that nothing can move. A gate run still in flight
/// (or a check of main still owed) keeps the loop going even with nothing open and past the tick limit:
/// the gate command is a child of this process and would outlive it, and the next serve would start a
/// second gate run in the same worktree. The run has a timeout, so the wait is bounded.
///
/// `stuck` needs a dead end: nothing runs, nothing can start and nothing waits for the owner. A loop
/// that is only parked on the owner (`Assessment::parked_on_owner`) goes on, unless `exit_when_waiting`.
fn decide(
    a: &Assessment,
    ticks: u64,
    max_ticks: Option<u64>,
    tick_ok: bool,
    exit_when_waiting: bool,
) -> Option<StopReason> {
    if a.open.is_empty() && !a.gate_busy {
        return Some(StopReason::AllDone);
    }
    if tick_ok && !a.busy && !a.can_start && (exit_when_waiting || a.waits.is_empty()) {
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
    /// What waits for the owner when the loop stopped; `[]` when nothing does.
    pub waiting: Vec<OwnerWait>,
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
        for w in &s.waiting {
            out.push_str(&format!("\nwaits for you: {}", w.line()));
        }
    }
    out.push_str(&waiting_line(s.questions_waiting));
    out
}

/// Printed once when the loop becomes idle on the owner (and again when what it waits for changes), so
/// that a quiet loop is not mistaken for a dead one.
#[derive(Serialize)]
pub struct WaitingNotice {
    pub ticks: u64,
    pub waiting: Vec<OwnerWait>,
}

fn notice_text(n: &WaitingNotice) -> String {
    let mut out = format!(
        "serve is alive and waiting for you (tick {}): nothing runs, it goes on when you answer",
        n.ticks
    );
    for w in &n.waiting {
        out.push_str(&format!("\nwaits for you: {}", w.line()));
    }
    out
}

/// What identifies a wait for the notice: a new reason on the same issue and kind is not news.
fn wait_keys(waits: &[OwnerWait]) -> Vec<(IssueId, WaitKind)> {
    waits.iter().map(|w| (w.issue.clone(), w.kind)).collect()
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
        waiting: a.waits,
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
fn limits(a: &ServeArgs, cfg: &klobuk_core::Config) -> (u64, Option<u64>) {
    (
        a.interval.unwrap_or(cfg.serve.interval_s),
        a.max_ticks.or(cfg.serve.max_ticks),
    )
}

/// How long one worker start may block the loop, in whole seconds (rounded up, at least 1).
fn start_window_s(cfg: &klobuk_core::Config) -> u64 {
    cfg.worker.start_timeout_ms.div_ceil(1000).max(1)
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
            "another `klobuk serve` loop is running on this repo (pid {}, last tick {} ago, started {}); \
             stop it first, or wait until it has been silent for {} s and this one takes over",
            p.pid,
            serve_pulse::fmt_age(age_s),
            p.started_at,
            serve_pulse::stale_after_s(p)
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
    fn start(root: &std::path::Path, mode: ServeMode, cfg: &klobuk_core::Config, a: &ServeArgs) -> Heartbeat {
        let mut hb = Heartbeat {
            root: root.to_path_buf(),
            pulse: Pulse::start(mode, 0, None, &now_rfc3339()),
            warned: false,
        };
        hb.set_limits(cfg, a);
        hb.save();
        hb
    }

    /// The limits in force this tick (an edit of `[serve]` or `[worker]` reaches the next tick).
    fn set_limits(&mut self, cfg: &klobuk_core::Config, a: &ServeArgs) {
        (self.pulse.interval_s, self.pulse.max_ticks) = limits(a, cfg);
        self.pulse.start_timeout_s = start_window_s(cfg);
    }

    /// The tick in progress is alive (a worker start is about to block, or just did).
    fn touch(&mut self) {
        self.pulse.touch(&now_rfc3339());
        self.save();
    }

    fn beat(&mut self, ticks: u64) {
        self.pulse.tick(ticks, &now_rfc3339());
        self.save();
    }

    fn stop(&mut self, reason: impl Into<String>) {
        self.pulse.stop(reason, &now_rfc3339());
        self.save();
    }

    /// What the idle loop waits for the owner on (empty while it works); written when it changed.
    fn set_waiting(&mut self, waits: &[OwnerWait]) {
        let list = waits
            .iter()
            .map(|w| PulseWait {
                issue: w.issue.to_string(),
                what: w.what().to_string(),
            })
            .collect();
        if self.pulse.set_waiting(list) {
            self.save();
        }
    }

    fn save(&mut self) {
        if let Err(e) = serve_pulse::write(&self.root, &self.pulse) {
            if !self.warned {
                self.warned = true;
                eprintln!(
                    "klobuk serve: cannot write the pulse {}: {e}",
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
        let t = tick(&ctx, a.dispatch, &mut reminders, None, &mut || {})?;
        return Rendered::new(&t, text(&t));
    }
    let (interval, _) = limits(a, &ctx.config);
    let on_disk = serve_pulse::read(&ctx.root).ok().flatten();
    let alive = on_disk
        .as_ref()
        .filter(|p| p.stopped_at.is_none())
        .and_then(|p| serve_pulse::pid_alive(p.pid));
    takeover_check(on_disk.as_ref(), time::OffsetDateTime::now_utc(), alive).map_err(AppError::conflict)?;
    if on_disk.as_ref().is_some_and(|p| p.stopped_at.is_none()) {
        eprintln!(
            "klobuk serve: the previous loop left no stop reason (killed, crashed or restarted); taking over"
        );
    }
    let mode = if a.dispatch {
        ServeMode::Dispatch
    } else {
        ServeMode::Observe
    };
    eprintln!(
        "klobuk serve: every {interval}s{} (Ctrl+C to stop)",
        if a.dispatch { " with dispatch" } else { "" }
    );
    // What a stopped serve left mid-way would never be touched again: no gate run is behind it now.
    let recovered = merge::recover_orphans(&ctx)?;
    if !recovered.is_empty() {
        let ids: Vec<String> = recovered.iter().map(ToString::to_string).collect();
        eprintln!(
            "klobuk serve: {} merge item(s) left in rebasing/gating by an earlier run went back to the queue: {}",
            ids.len(),
            ids.join(", ")
        );
    }
    let mut gate = GateSlot::reporting_to(&ctx.root);
    let mut hb = Heartbeat::start(&ctx.root, mode, &ctx.config, a);
    let result = serve_loop(cli, a, &mut ctx, &mut reminders, &mut gate, &mut hb);
    if let Err(e) = &result {
        if hb.pulse.stopped_at.is_none() {
            hb.stop(format!("error: {}", e.message));
        }
    }
    // Whatever ended the loop (an error included), a gate command still running is a child of this
    // process: wait for it so that it does not outlive klobuk. Its result is not concluded here; the item
    // is still `gating`/`verifying` in the store and the next start deals with it like with any other.
    if gate.is_busy() {
        eprintln!("klobuk serve: waiting for the gate run in flight before it exits");
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
    // what the last printed notice said the loop waits for; empty while it does not wait
    let mut announced: Vec<(IssueId, WaitKind)> = Vec::new();
    loop {
        ticks += 1;
        if let Some(warning) = watch.refresh(ctx) {
            eprintln!("klobuk serve: {warning}");
        }
        hb.set_limits(&ctx.config, a);
        let tick_ok = match tick(ctx, a.dispatch, reminders, Some(&mut *gate), &mut || hb.touch()) {
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
                eprintln!("klobuk serve: tick failed: {}", e.message);
                false
            }
        };
        hb.beat(ticks);
        if a.until_done {
            let assessment = assess(ctx, a.dispatch, gate)?;
            if let Some(reason) = decide(
                &assessment,
                ticks,
                hb.pulse.max_ticks,
                tick_ok,
                a.exit_when_waiting,
            ) {
                // before `finish`, which exits the process for `stuck`
                hb.stop(reason.as_str());
                return finish(cli, ticks, reason, assessment);
            }
            // Going on: when the loop is idle on the owner, say so in the pulse and (once) on stdout.
            let parked: &[OwnerWait] = if assessment.parked_on_owner() {
                &assessment.waits
            } else {
                &[]
            };
            hb.set_waiting(parked);
            let keys = wait_keys(parked);
            if keys != announced {
                if !parked.is_empty() {
                    let notice = WaitingNotice {
                        ticks,
                        waiting: parked.to_vec(),
                    };
                    print_ok(
                        cli.json,
                        cli.quiet,
                        &Rendered::new(&notice, notice_text(&notice))?,
                    );
                }
                announced = keys;
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
            waits: Vec::new(),
        }
    }

    fn asks(n: u32) -> OwnerWait {
        OwnerWait {
            issue: id(n),
            kind: WaitKind::Question,
            reason: "which crate?".into(),
        }
    }

    /// `decide` as the loop runs by default (it waits for the owner).
    fn decide_waiting(
        a: &Assessment,
        ticks: u64,
        max_ticks: Option<u64>,
        tick_ok: bool,
    ) -> Option<StopReason> {
        decide(a, ticks, max_ticks, tick_ok, false)
    }

    #[test]
    fn a_flag_wins_over_the_config_and_the_config_over_the_default() {
        let mut cfg = klobuk_core::Config::default();
        let args = |interval, max_ticks| ServeArgs {
            interval,
            once: false,
            dispatch: false,
            until_done: true,
            max_ticks,
            exit_when_waiting: false,
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
            msg.contains("another `klobuk serve` loop") && msg.contains("5 s"),
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
        // the "silent for N s" figure is the limit `judge` applies, not a second computation
        let mut slow = fresh.clone();
        slow.start_timeout_s = 200;
        let msg = takeover_check(Some(&slow), now, Some(true)).unwrap_err();
        assert!(
            msg.contains(&format!("{} s", serve_pulse::stale_after_s(&slow))),
            "{msg}"
        );
        let mut stopped = fresh.clone();
        stopped.stop("all_done", "2026-11-11T19:59:59Z");
        assert!(
            takeover_check(Some(&stopped), now, Some(true)).is_ok(),
            "a clean stop is free"
        );
    }

    #[test]
    fn the_pulse_is_refreshed_around_every_worker_start() {
        // a start blocks for up to the start window, a batch of them for much longer than one beat
        let log = std::cell::RefCell::new(Vec::<String>::new());
        let (started, errors) = start_slots(
            vec![id(1), id(2)],
            &mut || log.borrow_mut().push("beat".into()),
            |i| {
                log.borrow_mut().push(format!("start {i}"));
                if *i == id(2) {
                    Err(AppError::conflict("no room"))
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(started, vec![id(1)]);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0]["issue"], "asf-2");
        assert_eq!(
            *log.borrow(),
            ["beat", "start asf-1", "beat", "beat", "start asf-2", "beat"],
            "a beat before and after each start, a failed one included"
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
        assert_eq!(decide_waiting(&a, 1, None, true), Some(StopReason::AllDone));
        assert_eq!(decide_waiting(&a, 3, Some(3), true), Some(StopReason::AllDone));
    }

    #[test]
    fn a_gate_run_in_flight_is_not_all_done_even_with_nothing_open() {
        let a = Assessment {
            gate_busy: true,
            busy: true,
            ..assessment(0, false, false)
        };
        assert_eq!(
            decide_waiting(&a, 1, None, true),
            None,
            "stopping would kill the gate run"
        );
        assert_eq!(
            decide_waiting(&a, 4, Some(4), true),
            None,
            "not even the tick limit ends the loop under a gate run: its process would outlive klobuk"
        );
        let over = Assessment {
            gate_busy: false,
            busy: false,
            ..assessment(1, false, true)
        };
        assert_eq!(
            decide_waiting(&over, 5, Some(4), true),
            Some(StopReason::MaxTicks),
            "once the run is concluded the limit applies"
        );
    }

    #[test]
    fn idle_without_a_slot_and_nobody_to_wait_for_is_stuck_after_one_tick() {
        let a = assessment(2, false, false);
        for exit_when_waiting in [false, true] {
            assert_eq!(
                decide(&a, 1, None, true, exit_when_waiting),
                Some(StopReason::Stuck)
            );
            assert_eq!(
                decide(&a, 5, Some(5), true, exit_when_waiting),
                Some(StopReason::Stuck)
            );
        }
    }

    #[test]
    fn idle_with_something_that_waits_for_the_owner_goes_on_unless_told_to_exit() {
        let a = Assessment {
            waits: vec![asks(1)],
            ..assessment(2, false, false)
        };
        assert!(a.parked_on_owner());
        assert_eq!(decide(&a, 1, None, true, false), None, "waiting is the default");
        assert_eq!(decide(&a, 99, None, true, false), None);
        assert_eq!(
            decide(&a, 3, Some(3), true, false),
            Some(StopReason::MaxTicks),
            "--max-ticks ends a waiting loop"
        );
        assert_eq!(
            decide(&a, 1, None, true, true),
            Some(StopReason::Stuck),
            "--exit-when-waiting"
        );
        // the answer arrives: the question is gone and the loop is at a dead end again
        let answered = Assessment {
            waits: Vec::new(),
            ..a.clone()
        };
        assert_eq!(decide(&answered, 2, None, true, false), Some(StopReason::Stuck));
    }

    #[test]
    fn a_wait_is_not_a_park_while_work_runs_or_can_start() {
        for (busy, can_start) in [(true, false), (false, true)] {
            let a = Assessment {
                waits: vec![asks(1)],
                ..assessment(2, busy, can_start)
            };
            assert!(!a.parked_on_owner(), "busy={busy} can_start={can_start}");
        }
    }

    #[test]
    fn busy_or_plannable_work_keeps_going_until_the_limit() {
        for a in [assessment(1, true, false), assessment(1, false, true)] {
            assert_eq!(decide_waiting(&a, 1, None, true), None);
            assert_eq!(decide_waiting(&a, 1, Some(2), true), None);
            assert_eq!(decide_waiting(&a, 2, Some(2), true), Some(StopReason::MaxTicks));
        }
    }

    #[test]
    fn a_failed_tick_is_never_stuck_but_still_counts_towards_the_limit() {
        let a = assessment(1, false, false);
        assert_eq!(decide_waiting(&a, 1, Some(3), false), None);
        assert_eq!(decide_waiting(&a, 3, Some(3), false), Some(StopReason::MaxTicks));
    }

    #[test]
    fn the_notice_says_the_loop_is_alive_and_lists_what_it_waits_for() {
        let n = WaitingNotice {
            ticks: 4,
            waiting: vec![asks(7)],
        };
        assert_eq!(
            notice_text(&n),
            "serve is alive and waiting for you (tick 4): nothing runs, it goes on when you answer\n\
             waits for you: asf-7: question — which crate?"
        );
        // a new reason on the same issue and kind is not news, a new issue is
        let same = [OwnerWait {
            reason: "another text".into(),
            ..asks(7)
        }];
        assert_eq!(wait_keys(&same), wait_keys(&n.waiting));
        assert_ne!(wait_keys(&[asks(8)]), wait_keys(&n.waiting));
    }

    #[test]
    fn every_kind_that_the_owner_can_resolve_has_words() {
        for kind in [
            WaitKind::Question,
            WaitKind::RunBlocked,
            WaitKind::RunStalled,
            WaitKind::RunOverBudget,
            WaitKind::RunFailed,
            WaitKind::IssueBlocked,
            WaitKind::MergeFailed,
            WaitKind::MergeBlocked,
        ] {
            let w = OwnerWait {
                issue: id(7),
                kind,
                reason: String::new(),
            };
            assert!(w.line().starts_with("asf-7: ") && !w.what().is_empty(), "{kind}");
        }
        assert_eq!(asks(7).line(), "asf-7: question — which crate?");
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
