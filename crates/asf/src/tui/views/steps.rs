//! The steps of one queue item, the way a CI job page lists them: the rebase, every configured gate by
//! its name, the review, the merge and the gates on main, each with a mark and a time, and the earlier
//! attempts of the item folded into one line each.
//!
//! Like `stages`, this is a pure function of the snapshot: the merge item (state, gate results, error),
//! the events (`merge.state` moves, review events, sent-back events) and the gate run in flight that
//! `asf serve` reports. A time is a difference of two of those timestamps; none is invented, so a step
//! whose start or end is unknown shows no time.

use asf_core::config::GateWhen;
use asf_core::*;
use time::OffsetDateTime;

use super::queue::{gates_reused, review_round, stages, Stage, GATES_REUSED_TEXT};
use crate::tui::snapshot::{parse_time, Snapshot};
use crate::worker::{GateLive, FIX_SENT_BACK};

/// What the step list says of a gate that no stage of the pipeline runs for this item.
pub const WHEN_SKIPPED: &str = "gate skipped: when-condition";
/// What it says of a review that was not held because the patch was approved before.
pub const REVIEW_SKIPPED: &str = "review skipped: this patch was approved before";
/// What it says of the gates that the worker's verification already passed on this commit.
pub const REUSED: &str = GATES_REUSED_TEXT;
/// A step behind a failure.
pub const NOT_RUN: &str = "not run";

const MERGE_REVIEW_SKIPPED: &str = "merge.review_skipped";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepMark {
    Done,
    Running,
    Waiting,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepKind {
    Rebase,
    Gate(String),
    /// A failure that is not a gate (`scope`): the line the failed gates stage is named by.
    Check(String),
    Review,
    Merge,
    MainGates,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub kind: StepKind,
    pub mark: StepMark,
    pub label: String,
    /// Said next to the time: `running`, why it was skipped, what a waiting item waits for.
    pub note: String,
    /// Seconds a step that is over took.
    pub took: Option<i64>,
    /// When a running step began (RFC 3339): the view shows the time since, at the snapshot's `now`.
    pub since: Option<String>,
}

impl Step {
    fn new(kind: StepKind, mark: StepMark, label: impl Into<String>) -> Step {
        Step {
            kind,
            mark,
            label: label.into(),
            note: String::new(),
            took: None,
            since: None,
        }
    }

    fn note(mut self, note: impl Into<String>) -> Step {
        self.note = note.into();
        self
    }

    fn took(mut self, took: Option<i64>) -> Step {
        self.took = took;
        self
    }

    fn since(mut self, since: Option<String>) -> Step {
        self.since = since;
        self
    }

    /// Seconds the step took, or has taken so far when it runs.
    pub fn seconds(&self, snap: &Snapshot) -> Option<i64> {
        self.took.or_else(|| {
            let since = parse_time(self.since.as_deref()?)?;
            Some((parse_time(&snap.now)? - since).whole_seconds().max(0))
        })
    }
}

/// An earlier attempt of the item, folded into the line `text`; `phases` is what it shows opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    pub n: usize,
    pub text: String,
    pub phases: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemSteps {
    /// The attempt the steps belong to (the item's `attempts`, at least 1).
    pub attempt: i64,
    /// Seconds since the attempt began; `None` while it has not.
    pub elapsed: Option<i64>,
    /// The attempts before this one, oldest first.
    pub earlier: Vec<Attempt>,
    pub steps: Vec<Step>,
}

/// `1 s`, `6 min 12 s`, `2 h 5 min`.
pub fn duration(secs: i64) -> String {
    let secs = secs.max(0);
    match secs {
        0..=59 => format!("{secs} s"),
        60..=3599 if secs % 60 == 0 => format!("{} min", secs / 60),
        60..=3599 => format!("{} min {} s", secs / 60, secs % 60),
        _ if secs % 3600 / 60 == 0 => format!("{} h", secs / 3600),
        _ => format!("{} h {} min", secs / 3600, secs % 3600 / 60),
    }
}

/// One `merge.state` move of the item.
#[derive(Debug, Clone)]
struct Move {
    at: OffsetDateTime,
    to: String,
}

/// The item's moves between merge states, oldest first, cut into passes: each pass begins with the move
/// into `rebasing` (the queue starts an attempt there) and holds everything up to the next one.
fn passes(snap: &Snapshot, item: &MergeItem) -> Vec<Vec<Move>> {
    let mut found: Vec<(u64, Move)> = snap
        .events
        .iter()
        .filter(|e| e.kind == "merge.state" && e.payload["merge_id"] == item.id)
        .filter_map(|e| {
            Some((
                e.seq,
                Move {
                    at: parse_time(&e.ts)?,
                    to: e.payload["to"].as_str()?.to_string(),
                },
            ))
        })
        .collect();
    found.sort_by_key(|(seq, m)| (m.at, *seq));
    let mut out: Vec<Vec<Move>> = Vec::new();
    for (_, m) in found {
        if m.to == "rebasing" {
            out.push(vec![m]);
        } else if let Some(last) = out.last_mut() {
            last.push(m);
        }
    }
    out
}

fn first_move<'a>(pass: &'a [Move], to: &str) -> Option<&'a Move> {
    pass.iter().find(|m| m.to == to)
}

fn secs_between(from: OffsetDateTime, to: OffsetDateTime) -> i64 {
    (to - from).whole_seconds().max(0)
}

/// Seconds from the first move into `state` to the move after it.
fn stay(pass: &[Move], state: &str) -> Option<i64> {
    let i = pass.iter().position(|m| m.to == state)?;
    let next = pass.get(i + 1)?;
    Some(secs_between(pass[i].at, next.at))
}

fn rfc(at: OffsetDateTime) -> Option<String> {
    at.format(&time::format_description::well_known::Rfc3339).ok()
}

/// `waiting for the gate slot (asf-163 is on gate test, 12 min) · 2 ahead`, for a queued merge item.
pub fn slot_wait(snap: &Snapshot, ahead: usize) -> String {
    let mut text = String::from("waiting for the gate slot");
    match (
        &snap.gate_live,
        snap.serve.as_ref().is_some_and(|s| !s.state.is_running()),
    ) {
        (Some(live), _) => text.push_str(&format!(
            " ({} is on gate {}, {})",
            live.issue,
            live.gate,
            on_gate(snap, live)
        )),
        (None, true) => text.push_str(" (asf serve is not running)"),
        (None, false) => {}
    }
    if ahead > 0 {
        text.push_str(&format!(" · {ahead} ahead"));
    }
    text
}

/// How long the run in flight has been on its gate: `12 min 3 s`.
pub fn on_gate(snap: &Snapshot, live: &GateLive) -> String {
    match (parse_time(&snap.now), parse_time(&live.gate_started_at)) {
        (Some(now), Some(at)) => duration(secs_between(at, now)),
        _ => "-".into(),
    }
}

/// How many merge items come before the queued item `id` for the gate slot: the queue is FIFO by item id.
pub fn items_ahead(snap: &Snapshot, id: i64) -> usize {
    snap.queue
        .iter()
        .filter(|m| m.state == MergeState::Queued && m.id < id)
        .count()
}

/// How many jobs come before the verification of the run of `issue` in the gate slot: every queued
/// merge item goes first, then the runs that wait for their verification and started earlier.
pub fn verifications_ahead(snap: &Snapshot, issue: &IssueId) -> usize {
    let queued = snap
        .queue
        .iter()
        .filter(|m| m.state == MergeState::Queued)
        .count();
    let Some(mine) = snap.run_for(issue) else {
        return queued;
    };
    let earlier = snap
        .runs
        .iter()
        .filter(|r| {
            r.state == RunState::Finishing
                && (&r.started_at, &r.issue_id) < (&mine.started_at, &mine.issue_id)
        })
        .count();
    queued + earlier
}

/// The line the Now and Agents views show for a run that is `finishing` (waits for its verification) or
/// `verifying` (its gates run): who holds the gate slot, on which gate and for how long, how many jobs
/// are ahead, or the gate in flight with a timer and the gates that are over. `None` for any other run.
pub fn run_gate_text(snap: &Snapshot, run: &Run) -> Option<String> {
    match run.state {
        RunState::Finishing => {
            let mut text = String::from("waiting for the gate slot");
            match &snap.gate_live {
                Some(live) => text.push_str(&format!(
                    ": {} is on gate {}, {}",
                    live.issue,
                    live.gate,
                    on_gate(snap, live)
                )),
                None if snap.serve.as_ref().is_some_and(|s| !s.state.is_running()) => {
                    text.push_str(": asf serve is not running");
                }
                None => {}
            }
            let ahead = verifications_ahead(snap, &run.issue_id);
            if ahead > 0 {
                text.push_str(&format!(" · {ahead} ahead"));
            }
            Some(text)
        }
        RunState::Verifying => Some(
            match snap.gate_live.as_ref().filter(|l| l.issue == run.issue_id) {
                Some(live) => {
                    let mut text = format!("gate {} running {}", live.gate, on_gate(snap, live));
                    if !live.finished.is_empty() {
                        let done: Vec<String> = live
                            .finished
                            .iter()
                            .map(|g| {
                                let mark = if g.ok { "✓" } else { "✗" };
                                format!("{mark} {} {}", g.name, duration((g.duration_ms / 1000) as i64))
                            })
                            .collect();
                        text.push_str(&format!(" · {}", done.join(", ")));
                    }
                    text
                }
                None => "verifying: no gate run is reported".to_string(),
            },
        ),
        _ => None,
    }
}

/// The short form of [`run_gate_text`] for a table cell: `waiting for the gate slot`, `gate test 3 min 2 s`.
pub fn run_gate_short(snap: &Snapshot, run: &Run) -> Option<String> {
    match run.state {
        RunState::Finishing => Some("waiting for the gate slot".to_string()),
        RunState::Verifying => Some(
            match snap.gate_live.as_ref().filter(|l| l.issue == run.issue_id) {
                Some(live) => format!("gate {} {}", live.gate, on_gate(snap, live)),
                None => "verifying".to_string(),
            },
        ),
        _ => None,
    }
}

/// The gate run in flight when it is the one of the merge item `item`.
fn live_of<'a>(snap: &'a Snapshot, item: &MergeItem) -> Option<&'a GateLive> {
    snap.gate_live
        .as_ref()
        .filter(|l| l.kind == "merge" && l.issue == item.issue_id)
}

fn short(sha: &str) -> String {
    sha.chars().take(7).collect()
}

/// The gates of the merge stage, in config order, each with whether this stage runs it. Without config
/// (a snapshot built by hand) the names come from what ran.
fn gate_names(snap: &Snapshot, item: &MergeItem) -> Vec<(String, bool)> {
    if !snap.gates.is_empty() {
        return snap
            .gates
            .iter()
            .map(|g| (g.name.clone(), g.when.contains(&GateWhen::Merge)))
            .collect();
    }
    let ran: Vec<String> = match live_of(snap, item) {
        Some(l) => l.gates.clone(),
        None => item.gate_results.iter().map(|g| g.name.clone()).collect(),
    };
    ran.into_iter().map(|n| (n, true)).collect()
}

fn gate_steps(snap: &Snapshot, item: &MergeItem, stage: &Stage) -> Vec<Step> {
    let reused = gates_reused(snap, item) && matches!(stage, Stage::Done(_));
    let live = live_of(snap, item).filter(|_| item.state == MergeState::Gating);
    let mut steps = Vec::new();
    let mut seen_failure = false;
    for (name, runs) in gate_names(snap, item) {
        let label = format!("gate {name}");
        let kind = StepKind::Gate(name.clone());
        if !runs {
            steps.push(Step::new(kind, StepMark::Skipped, label).note(WHEN_SKIPPED));
            continue;
        }
        let result = item.gate_results.iter().find(|g| g.name == name);
        let step = match stage {
            Stage::Done(_) if reused => Step::new(kind, StepMark::Skipped, label).note(REUSED),
            Stage::Done(_) => {
                Step::new(kind, StepMark::Done, label).took(result.map(|g| (g.duration_ms / 1000) as i64))
            }
            Stage::Failed(what) => match result {
                Some(g) if g.ok => {
                    Step::new(kind, StepMark::Done, label).took(Some((g.duration_ms / 1000) as i64))
                }
                Some(g) => {
                    seen_failure = true;
                    Step::new(kind, StepMark::Failed, label)
                        .note("failed")
                        .took(Some((g.duration_ms / 1000) as i64))
                }
                None => Step::new(kind, StepMark::Skipped, label).note(if what == &name {
                    "failed"
                } else {
                    NOT_RUN
                }),
            },
            Stage::Running(_) => match live {
                Some(l) => match l.finished.iter().find(|g| g.name == name) {
                    Some(g) => Step::new(kind, if g.ok { StepMark::Done } else { StepMark::Failed }, label)
                        .took(Some((g.duration_ms / 1000) as i64)),
                    None if l.gate == name => Step::new(kind, StepMark::Running, label)
                        .note("running")
                        .since(Some(l.gate_started_at.clone())),
                    None => Step::new(kind, StepMark::Waiting, label),
                },
                None => Step::new(kind, StepMark::Waiting, label).note("no live report"),
            },
            Stage::Pending => Step::new(kind, StepMark::Waiting, label),
            Stage::After => Step::new(kind, StepMark::Skipped, label).note(NOT_RUN),
            Stage::NotNeeded => Step::new(kind, StepMark::Skipped, label),
        };
        steps.push(step);
    }
    // a gates stage that failed on something that is not a gate (the scope check)
    if let Stage::Failed(what) = stage {
        if !seen_failure && !steps.iter().any(|s| s.kind == StepKind::Gate(what.clone())) {
            steps.insert(
                0,
                Step::new(StepKind::Check(what.clone()), StepMark::Failed, what.clone()).note("failed"),
            );
        }
    }
    steps
}

/// `review · round 1 · opus · run-id`: the reviewer once there is one.
fn review_label(snap: &Snapshot, item: &MergeItem) -> String {
    let mut label = String::from("review");
    if let Some(round) = review_round(snap, &item.issue_id) {
        label.push_str(&format!(" · round {round}"));
    }
    if !snap.review_model.is_empty() {
        label.push_str(&format!(" · {}", snap.review_model));
    }
    let reviewer = snap
        .review_of
        .iter()
        .find(|(_, l)| l.reviews == item.issue_id)
        .map(|(rid, _)| rid);
    if let Some(rid) = reviewer {
        let run = snap
            .run_for(rid)
            .map(|r| r.id.clone())
            .or_else(|| snap.last_run(rid).map(|r| r.run_id.clone()));
        label.push_str(&format!(" · {}", run.unwrap_or_else(|| rid.to_string())));
    }
    label
}

/// Whether the review of `item` was not held because the patch was approved before.
fn review_was_skipped(snap: &Snapshot, item: &MergeItem, since: Option<OffsetDateTime>) -> bool {
    snap.events.iter().any(|e| {
        e.kind == MERGE_REVIEW_SKIPPED
            && e.issue_id.as_ref() == Some(&item.issue_id)
            && since.is_none_or(|s| parse_time(&e.ts).is_some_and(|t| t >= s))
    })
}

/// The steps of `item`'s current attempt and a line for each earlier one.
pub fn item_steps(snap: &Snapshot, item: &MergeItem) -> ItemSteps {
    let all = passes(snap, item);
    // a queued item has not begun its next attempt: everything it has is behind it
    let (before, current): (&[Vec<Move>], Option<&Vec<Move>>) = match all.split_last() {
        Some((last, rest)) if item.state != MergeState::Queued => (rest, Some(last)),
        _ => (&all, None),
    };
    let cur = current.map(Vec::as_slice);
    let started = cur.and_then(|p| p.first()).map(|m| m.at);
    let st = stages(snap, item);
    let mut steps = Vec::new();

    // rebase
    let label = match item.rebased_onto.as_deref() {
        Some(sha) => format!("rebase onto {} ({})", snap.branch, short(sha)),
        None => format!("rebase onto {}", snap.branch),
    };
    let rebase_took = cur.and_then(|p| Some(secs_between(started?, first_move(p, "gating")?.at)));
    steps.push(match &st[0] {
        Stage::Done(_) => Step::new(StepKind::Rebase, StepMark::Done, label).took(rebase_took),
        Stage::Running(_) => Step::new(StepKind::Rebase, StepMark::Running, label)
            .note("running")
            .since(started.and_then(rfc)),
        Stage::Failed(what) => Step::new(StepKind::Rebase, StepMark::Failed, label).note(what.clone()),
        Stage::Pending if item.state == MergeState::Queued => {
            Step::new(StepKind::Rebase, StepMark::Waiting, label)
                .note(slot_wait(snap, items_ahead(snap, item.id)))
        }
        Stage::Pending => Step::new(StepKind::Rebase, StepMark::Waiting, label),
        Stage::After | Stage::NotNeeded => {
            Step::new(StepKind::Rebase, StepMark::Skipped, label).note(NOT_RUN)
        }
    });

    // gates
    steps.extend(gate_steps(snap, item, &st[1]));

    // review
    let skipped = review_was_skipped(snap, item, started);
    if !matches!(st[2], Stage::NotNeeded) || skipped {
        let label = review_label(snap, item);
        let began = cur.and_then(|p| first_move(p, "reviewing")).map(|m| m.at);
        let step = match &st[2] {
            _ if skipped && !matches!(st[2], Stage::Running(_) | Stage::Failed(_)) => {
                Step::new(StepKind::Review, StepMark::Skipped, "review").note(REVIEW_SKIPPED)
            }
            Stage::Done(_) => Step::new(StepKind::Review, StepMark::Done, label)
                .took(cur.and_then(|p| stay(p, "reviewing"))),
            Stage::Running(_) => Step::new(StepKind::Review, StepMark::Running, label)
                .note("running")
                .since(began.and_then(rfc)),
            Stage::Failed(what) => Step::new(StepKind::Review, StepMark::Failed, label)
                .note(what.clone())
                .took(cur.and_then(|p| stay(p, "reviewing"))),
            Stage::Pending => Step::new(StepKind::Review, StepMark::Waiting, label),
            Stage::After | Stage::NotNeeded => {
                Step::new(StepKind::Review, StepMark::Skipped, label).note(NOT_RUN)
            }
        };
        steps.push(step);
    }

    // merge
    let merging = cur.and_then(|p| first_move(p, "merging").or_else(|| first_move(p, "pr_open")));
    steps.push(match &st[3] {
        Stage::Done(_) => Step::new(StepKind::Merge, StepMark::Done, "merge")
            .took(cur.and_then(|p| Some(secs_between(merging?.at, first_move(p, "merged")?.at)))),
        Stage::Running(what) => Step::new(StepKind::Merge, StepMark::Running, "merge")
            .note(what.clone())
            .since(merging.map(|m| m.at).and_then(rfc)),
        Stage::Failed(what) => Step::new(StepKind::Merge, StepMark::Failed, "merge").note(what.clone()),
        Stage::Pending => Step::new(StepKind::Merge, StepMark::Waiting, "merge"),
        Stage::After | Stage::NotNeeded => {
            Step::new(StepKind::Merge, StepMark::Skipped, "merge").note(NOT_RUN)
        }
    });

    // gates on main: only when they run there (a red main is shown whatever the config says now)
    if !matches!(st[4], Stage::NotNeeded) {
        let label = "gates on main";
        steps.push(match &st[4] {
            Stage::Done(_) => Step::new(StepKind::MainGates, StepMark::Done, label),
            Stage::Running(_) => Step::new(StepKind::MainGates, StepMark::Running, label).note("running"),
            Stage::Failed(what) => Step::new(StepKind::MainGates, StepMark::Failed, label).note(what.clone()),
            Stage::Pending => Step::new(StepKind::MainGates, StepMark::Waiting, label),
            Stage::After | Stage::NotNeeded => {
                Step::new(StepKind::MainGates, StepMark::Skipped, label).note(NOT_RUN)
            }
        });
    }

    let earlier = before
        .iter()
        .enumerate()
        .map(|(i, pass)| earlier_attempt(snap, item, i + 1, pass, all.get(i + 1)))
        .collect();
    ItemSteps {
        attempt: item.attempts.max(1),
        elapsed: started.and_then(|s| Some(secs_between(s, parse_time(&snap.now)?))),
        earlier,
        steps,
    }
}

/// The gate a worker was sent back for in `pass`, from the headline of the `run.fix_sent_back` event
/// (`after the rebase onto abc1234 the gate `clippy` failed`).
fn sent_back_gate(
    snap: &Snapshot,
    item: &MergeItem,
    from: OffsetDateTime,
    until: Option<OffsetDateTime>,
) -> Option<(String, String)> {
    snap.events
        .iter()
        .filter(|e| e.kind == FIX_SENT_BACK && e.issue_id.as_ref() == Some(&item.issue_id))
        .filter(|e| parse_time(&e.ts).is_some_and(|t| t >= from && until.is_none_or(|u| t < u)))
        .max_by_key(|e| e.seq)
        .map(|e| {
            let kind = e.payload["kind"].as_str().unwrap_or("").to_string();
            let gate = e.payload["headline"]
                .as_str()
                .and_then(|h| h.split('`').nth(1))
                .unwrap_or("")
                .to_string();
            (kind, gate)
        })
}

/// The line of an attempt that is over, and the phases it opens into.
fn earlier_attempt(
    snap: &Snapshot,
    item: &MergeItem,
    n: usize,
    pass: &[Move],
    next: Option<&Vec<Move>>,
) -> Attempt {
    let start = pass[0].at;
    // the attempt ended where it failed or merged; a move back to `queued` only requeued it
    let end = pass
        .iter()
        .rev()
        .find(|m| m.to != "queued")
        .or(pass.last())
        .map_or(start, |m| m.at);
    let after = duration(secs_between(start, end));
    let until = next.and_then(|p| p.first()).map(|m| m.at);
    let outcome = pass
        .iter()
        .rev()
        .map(|m| m.to.as_str())
        .find(|t| !matches!(*t, "queued"));
    let sent = sent_back_gate(snap, item, start, until);
    let verdict = match (outcome, &sent) {
        (Some("failed" | "blocked"), Some((kind, gate))) if kind == "gate" && !gate.is_empty() => {
            format!("gate {gate} failed")
        }
        (Some("failed" | "blocked"), Some((kind, _))) if kind == "conflict" => "rebase conflict".to_string(),
        (Some("failed"), _) => "failed".to_string(),
        (Some("blocked"), _) => "blocked".to_string(),
        (Some("merged"), _) => "merged".to_string(),
        _ => "sent back".to_string(),
    };
    // each stay in a state, up to the move that ended it
    let names = [
        ("rebasing", "rebase"),
        ("gating", "gates"),
        ("reviewing", "review"),
        ("merging", "merge"),
    ];
    let phases = pass
        .windows(2)
        .filter_map(|w| {
            let (_, label) = names.iter().find(|(state, _)| *state == w[0].to)?;
            let bad = matches!(w[1].to.as_str(), "failed" | "blocked");
            Some(
                Step::new(
                    StepKind::Check((*label).to_string()),
                    if bad { StepMark::Failed } else { StepMark::Done },
                    *label,
                )
                .took(Some(secs_between(w[0].at, w[1].at))),
            )
        })
        .collect();
    Attempt {
        n,
        text: format!("attempt {n}: {verdict} after {after}"),
        phases,
    }
}
