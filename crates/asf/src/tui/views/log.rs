//! The Log view: what happened in the factory, one plain sentence per event, newest first.
//!
//! [`entries`] turns the events of the snapshot into [`Entry`] rows: a time, the issue, a symbol and a
//! sentence with dim details. Events that only repeat another line are left out (a claim, a status
//! comment, the rebasing step that the gate line folds in). An event kind the view does not know
//! renders as `<kind> <compact payload>`, so nothing is lost. The filter, the selection and the raw
//! JSON window are the [`State`]; keys are a pure function of it.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use asf_core::{Event, IssueId, IssueType, MergeItem};

use crate::tui::app::{Action, View};
use crate::tui::bindings::{b, Binding, Group, Slot, HELP_MOVE, HELP_OPEN, NORMAL};
use crate::tui::snapshot::{move_selection, offset_of, parse_time, text_width, trunc, Snapshot};
use crate::tui::theme::{Mark, Theme};
use crate::tui::views::agents;

/// The sentence of an empty log.
pub const EMPTY: &str = "Nothing has happened yet. Events appear here as the factory works.";
/// The sentence when the filter hides every event.
const NO_MATCH: &str = "Nothing matches the filter. Press Esc to clear it.";

/// Selection, filter and the raw JSON window.
#[derive(Debug, Clone, Default)]
pub struct State {
    pub selected: usize,
    filter: String,
    /// Keys are text for the filter (`/` starts it, Enter keeps it, Esc clears it).
    typing: bool,
    /// The raw JSON of the selected event is open in a window.
    raw: bool,
}

impl State {
    /// Whether this view takes every key: typing a filter, or the raw JSON window is open (a window
    /// swallows keys like the app's other modals, so `q` or a digit does not act behind it).
    pub fn takes_every_key(&self) -> bool {
        self.typing || self.raw
    }
}

/// The filter text as typed.
#[cfg(test)]
pub fn filter(state: &State) -> &str {
    &state.filter
}

const LOG: &[View] = &[View::Log];

pub const BINDINGS: &[Binding] = &[
    b(
        "j / k",
        "move",
        HELP_MOVE,
        Group::Everywhere,
        LOG,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "g / G",
        "newest / oldest",
        "jump to the newest / oldest event",
        Group::Everywhere,
        LOG,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "/",
        "filter",
        "filter the log by issue id or text\n(Enter keeps it, Esc clears it)",
        Group::Everywhere,
        LOG,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "Enter",
        "open issue",
        HELP_OPEN,
        Group::Everywhere,
        LOG,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "J",
        "raw event",
        "show the raw JSON of the selected event\n(J or Esc closes it)",
        Group::Everywhere,
        LOG,
        NORMAL,
        Slot::Hint,
    ),
];

// ---------------------------------------------------------------------------------------------
// ROWS: EVENT KIND AND PAYLOAD TO SENTENCE
// ---------------------------------------------------------------------------------------------

/// The symbol in front of a sentence. Never the only carrier of meaning: each differs in shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sym {
    /// No symbol (the plan import line).
    None,
    Working,
    Needs,
    Review,
    Done,
    Waiting,
    /// A failure: `✗` in the danger style.
    Failed,
}

impl Sym {
    pub fn glyph(self) -> &'static str {
        match self {
            Sym::None => "",
            Sym::Working => Mark::Working.glyph(),
            Sym::Needs => Mark::Needs.glyph(),
            Sym::Review => Mark::Review.glyph(),
            Sym::Done => Mark::Done.glyph(),
            Sym::Waiting => Mark::Waiting.glyph(),
            Sym::Failed => "✗",
        }
    }

    fn style(self, theme: &Theme) -> Style {
        match self {
            Sym::None => Style::default(),
            Sym::Working => theme.mark(Mark::Working),
            Sym::Needs => theme.mark(Mark::Needs),
            Sym::Review => theme.mark(Mark::Review),
            Sym::Done => theme.mark(Mark::Done),
            Sym::Waiting => theme.mark(Mark::Waiting),
            Sym::Failed => theme.danger(),
        }
    }
}

/// One row of the Log.
#[derive(Debug, Clone)]
pub struct Entry<'a> {
    pub event: &'a Event,
    /// Local time of the event, `HH:MM:SS`.
    pub time: String,
    /// The issue column (blank for a plan import and for events without an issue).
    pub issue: Option<IssueId>,
    pub sym: Sym,
    /// The sentence before the first ` · `.
    pub head: String,
    /// The dim details after it.
    pub details: Vec<String>,
}

impl Entry<'_> {
    /// The whole sentence: the head and the details joined by ` · `.
    pub fn sentence(&self) -> String {
        let mut out = self.head.clone();
        for d in &self.details {
            out.push_str(" · ");
            out.push_str(d);
        }
        out
    }

    /// Whether the filter (already lower-cased) matches the issue, the kind or the sentence.
    fn matches(&self, needle: &str) -> bool {
        let issue = self
            .event
            .issue_id
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        format!("{issue} {} {}", self.event.kind, self.sentence())
            .to_lowercase()
            .contains(needle)
    }
}

/// What an event says, before the row is dressed.
struct Said {
    sym: Sym,
    head: String,
    details: Vec<String>,
    /// Whether the issue column shows the issue of the event.
    show_issue: bool,
}

fn said(sym: Sym, head: impl Into<String>, details: &[Option<String>]) -> Option<Said> {
    Some(Said {
        sym,
        head: head.into(),
        details: details.iter().flatten().cloned().collect(),
        show_issue: true,
    })
}

fn some(s: impl Into<String>) -> Option<String> {
    Some(s.into())
}

/// The first non-blank line of `s`.
fn first_line(s: &str) -> Option<String> {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

fn text<'e>(e: &'e Event, key: &str) -> Option<&'e str> {
    e.payload.get(key).and_then(|v| v.as_str())
}

fn count(e: &Event, key: &str) -> Option<i64> {
    e.payload.get(key).and_then(|v| v.as_i64())
}

fn plural(n: i64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `12:41:07` in the local time zone of the snapshot.
fn clock(snap: &Snapshot, ts: &str) -> String {
    match parse_time(ts) {
        Some(t) => {
            let t = t.to_offset(offset_of(snap));
            format!("{:02}:{:02}:{:02}", t.hour(), t.minute(), t.second())
        }
        None => ts.get(11..19).unwrap_or(ts).to_string(),
    }
}

fn is_review(snap: &Snapshot, id: Option<&IssueId>) -> bool {
    id.and_then(|id| snap.issue(id))
        .is_some_and(|i| i.kind == IssueType::Review)
}

fn find_merge(snap: &Snapshot, merge_id: i64) -> Option<&MergeItem> {
    snap.queue
        .iter()
        .chain(&snap.merged_today)
        .find(|m| m.id == merge_id)
}

/// `worker started · <model> · <session name>`.
fn worker_started(snap: &Snapshot, e: &Event) -> Option<Said> {
    let issue = e.issue_id.as_ref()?;
    let model = snap.last_run(issue).and_then(|r| r.model.clone());
    let session = snap
        .issue(issue)
        .map(|i| agents::session_name(issue.as_str(), &i.title));
    said(Sym::Working, "worker started", &[model, session])
}

/// How the gates of one merge attempt ended.
enum Gates {
    Passed(Vec<String>),
    Failed(Option<String>),
    Running,
}

/// The merge state a `merge.state` event moved to, for the merge item `merge_id`.
fn merge_to(e: &Event, merge_id: i64) -> Option<&str> {
    (e.kind == "merge.state" && count(e, "merge_id") == Some(merge_id))
        .then(|| text(e, "to"))
        .flatten()
}

/// How the gates after the rebase ended, for the `gating` event at `idx`. The newest attempt reads the
/// results of the merge item; an earlier attempt reads what the item did next (the item only keeps
/// the last attempt, so the names of an older one are gone).
fn gates_after(snap: &Snapshot, idx: usize, merge_id: i64) -> Gates {
    let newer = &snap.events[..idx];
    let later_attempt = newer.iter().any(|e| merge_to(e, merge_id) == Some("gating"));
    if !later_attempt {
        if let Some(item) = find_merge(snap, merge_id).filter(|m| !m.gate_results.is_empty()) {
            return match item.gate_results.iter().find(|g| !g.ok) {
                Some(bad) => Gates::Failed(Some(bad.name.clone())),
                None => Gates::Passed(item.gate_results.iter().map(|g| g.name.clone()).collect()),
            };
        }
    }
    for e in newer.iter().rev() {
        match merge_to(e, merge_id) {
            Some("gating") => break,
            Some("reviewing" | "merging" | "merged" | "pr_open") => return Gates::Passed(Vec::new()),
            Some("failed" | "blocked" | "queued") => return Gates::Failed(None),
            _ => {}
        }
    }
    Gates::Running
}

/// What a `reviewing` to `failed` move was. A `changes` verdict that is sent back writes
/// `merge.sent_back` right after it (that line is the verdict); one that is not (the worker is still
/// running, or `review.max_rounds` is reached) leaves this move as the only trace of it. A reviewer
/// that could not start has its own line, and any other error is a failed merge with its reason.
fn reviewed_failed(snap: &Snapshot, idx: usize, e: &Event, reason: Option<String>) -> Option<Said> {
    for next in snap.events[..idx]
        .iter()
        .rev()
        .filter(|n| n.issue_id == e.issue_id)
    {
        match next.kind.as_str() {
            "merge.sent_back" | "merge.review_start_failed" => return None,
            "merge.state"
            | "merge.review_started"
            | "merge.enqueued"
            | "merge.requeued"
            | "merge.retried"
            | "merge.merged" => break,
            _ => {}
        }
    }
    let changes = merge_id_of(e)
        .and_then(|id| find_merge(snap, id))
        .and_then(|m| m.last_error.as_deref())
        .is_none_or(|err| err.starts_with("review asks for changes"));
    if changes {
        said(Sym::Review, "review verdict: changes", &[])
    } else {
        said(Sym::Failed, "merge failed", &[reason])
    }
}

fn merge_id_of(e: &Event) -> Option<i64> {
    count(e, "merge_id")
}

fn merge_state(snap: &Snapshot, idx: usize, e: &Event) -> Option<Said> {
    let merge_id = count(e, "merge_id")?;
    let item = find_merge(snap, merge_id);
    let reason = item.and_then(|m| m.last_error.as_deref()).and_then(first_line);
    match text(e, "to")? {
        // rebasing is folded into the gate line that follows it
        "gating" => match gates_after(snap, idx, merge_id) {
            Gates::Passed(names) => {
                let names = (!names.is_empty()).then(|| names.join(", "));
                said(Sym::Done, "gates passed after rebase", &[names])
            }
            Gates::Failed(Some(name)) => said(
                Sym::Failed,
                format!("merge failed · gate {name} after rebase"),
                &[],
            ),
            Gates::Failed(None) => said(Sym::Failed, "merge failed · gates after rebase", &[]),
            Gates::Running => said(Sym::Working, "gates running after rebase", &[]),
        },
        // a failed gate has its line already, a reviewer's `changes` verdict has its own
        "failed" if text(e, "from") == Some("gating") => None,
        "failed" if text(e, "from") == Some("reviewing") => reviewed_failed(snap, idx, e, reason),
        "failed" => said(Sym::Failed, "merge failed", &[reason]),
        "blocked" => said(Sym::Needs, "merge blocked", &[reason]),
        "skipped" => said(Sym::Waiting, "merge skipped", &[]),
        _ => None,
    }
}

fn run_state(snap: &Snapshot, idx: usize, e: &Event) -> Option<Said> {
    let reviewer = is_review(snap, e.issue_id.as_ref());
    let reason = text(e, "reason").and_then(first_line);
    match text(e, "to")? {
        "working" => {
            if reviewer {
                return None;
            }
            let created = snap.events[idx + 1..]
                .iter()
                .any(|o| o.kind == "run.created" && o.run_id == e.run_id);
            match text(e, "from") {
                Some("starting") if created => None,
                Some("starting") | None => worker_started(snap, e),
                Some(_) => said(Sym::Working, "worker is working again", &[]),
            }
        }
        "starting" => (!reviewer)
            .then(|| said(Sym::Waiting, "worker is starting", &[]))
            .flatten(),
        // a worker that never came up is blocked with the start error as the reason (`block_start`);
        // the dialog case (`worker asked ...`) has no reason
        "blocked" if reason.is_some() || text(e, "from") == Some("starting") => {
            said(Sym::Needs, "worker could not start", &[reason])
        }
        "blocked" => said(Sym::Needs, "worker asked a question and is waiting", &[]),
        "finishing" => (!reviewer)
            .then(|| said(Sym::Working, "worker finished and handed over its report", &[]))
            .flatten(),
        "verifying" => None,
        "succeeded" => {
            if reviewer {
                return None;
            }
            // `run.state` carries {from, to, reason} only; the counts were dropped from the contract
            said(Sym::Done, "run verified", &[some("queued for merge")])
        }
        "failed" => said(Sym::Failed, "verification failed", &[reason]),
        "stalled" => said(Sym::Needs, "worker went quiet and may be stuck", &[reason]),
        "over_budget" => said(Sym::Needs, "worker is over its budget", &[reason]),
        "aborted" => said(Sym::Waiting, "run aborted", &[reason]),
        _ => None,
    }
}

fn issue_created(snap: &Snapshot, e: &Event) -> Option<Said> {
    let id = e.issue_id.as_ref()?;
    let issue = snap.issue(id);
    let from_plan =
        |source: &Option<String>, marker: &str| source.as_deref().is_some_and(|s| s.contains(marker));
    if let Some(i) = issue {
        if from_plan(&i.source, "#epic") {
            let tasks = snap
                .issues
                .iter()
                .filter(|t| t.parent_id.as_ref() == Some(id) && from_plan(&t.source, "#task-"))
                .count() as i64;
            return Some(Said {
                sym: Sym::None,
                head: format!(
                    "plan imported: epic {id} with {}",
                    plural(tasks, "issue", "issues")
                ),
                details: Vec::new(),
                show_issue: false,
            });
        }
        // a task of a plan whose epic line is in the log is part of that batch
        if from_plan(&i.source, "#task-") {
            let epic_logged = i.parent_id.as_ref().is_some_and(|p| {
                snap.events
                    .iter()
                    .any(|o| o.kind == "issue.created" && o.issue_id.as_ref() == Some(p))
            });
            if epic_logged {
                return None;
            }
        }
    }
    let title = text(e, "title").map(str::to_string);
    said(Sym::Waiting, "issue created", &[title])
}

fn comment_added(snap: &Snapshot, e: &Event) -> Option<Said> {
    let actor = e.actor.clone().unwrap_or_else(|| "someone".into());
    let kind = text(e, "type")?;
    // a reviewer's DECISION is the verdict the review lines already show
    let reviewer_lane = e
        .actor
        .as_deref()
        .and_then(|a| a.strip_prefix("agent:").or(Some(a)))
        .and_then(|a| a.parse::<IssueId>().ok());
    match kind {
        "QUESTION" => said(Sym::Needs, format!("{actor} asked a question"), &[]),
        "BLOCKER" => said(Sym::Needs, format!("{actor} reported a blocker"), &[]),
        "ANSWER" => said(Sym::Waiting, format!("{actor} answered a question"), &[]),
        "DECISION" if is_review(snap, reviewer_lane.as_ref()) => None,
        "DECISION" => said(Sym::Waiting, format!("{actor} recorded a decision"), &[]),
        "HANDOFF" => said(Sym::Waiting, format!("{actor} handed work over"), &[]),
        // STATUS and FYI comments restate an event that has its own line
        _ => None,
    }
}

fn issue_status(e: &Event) -> Option<Said> {
    let to = text(e, "to")?;
    let from = text(e, "from").map(|f| format!("was {f}"));
    // the other statuses restate the worker, review and merge lines
    let sym = match to {
        "blocked" => Sym::Needs,
        "backlog" | "ready" | "cancelled" => Sym::Waiting,
        _ => return None,
    };
    said(sym, format!("status changed to {to}"), &[from])
}

/// A compact rendering of a payload; empty for `{}` and `null`.
fn compact(payload: &serde_json::Value) -> String {
    match payload {
        serde_json::Value::Null => String::new(),
        serde_json::Value::Object(m) if m.is_empty() => String::new(),
        other => other.to_string(),
    }
}

/// The sentence of `snap.events[idx]`, or `None` when the event only repeats another line.
fn describe(snap: &Snapshot, idx: usize) -> Option<Said> {
    let e = &snap.events[idx];
    match e.kind.as_str() {
        "run.created" => {
            let retried = snap.events[..idx]
                .iter()
                .any(|n| n.kind == "run.start_retry" && n.run_id == e.run_id);
            if retried || is_review(snap, e.issue_id.as_ref()) {
                None
            } else {
                worker_started(snap, e)
            }
        }
        "run.start_retry" => said(
            Sym::Working,
            "worker started",
            &[some("second attempt after a start timeout")],
        ),
        "run.state" => run_state(snap, idx, e),
        "run.stop" | "claim.taken" | "claim.released" => None,
        "merge.enqueued" => {
            // `run verified` already says it was queued
            let said_so = snap.events[idx + 1..].iter().any(|o| {
                o.kind == "run.state" && o.issue_id == e.issue_id && text(o, "to") == Some("succeeded")
            });
            if said_so {
                None
            } else {
                said(
                    Sym::Waiting,
                    "queued for merge",
                    &[text(e, "branch").map(str::to_string)],
                )
            }
        }
        "merge.state" => merge_state(snap, idx, e),
        "merge.review_started" => {
            let model = Some(snap.review_model.clone()).filter(|m| !m.is_empty());
            let round = count(e, "round").map(|n| format!("round {n}"));
            said(Sym::Review, "review started", &[model, round])
        }
        "merge.review_approved" => said(Sym::Review, "review verdict: approve", &[]),
        "merge.review_skipped" => said(
            Sym::Review,
            "review skipped",
            &[some("this patch was approved before")],
        ),
        "merge.sent_back" => said(Sym::Review, "review verdict: changes", &[]),
        // a gate failure or a rebase conflict, sent to the worker: not a review verdict
        "run.fix_sent_back" => {
            let round = count(e, "round")
                .map(|n| format!(" (round {n})"))
                .unwrap_or_default();
            let head = match text(e, "headline") {
                Some(what) => format!("{what}, sent back to the worker{round}"),
                None => format!(
                    "sent back to the worker to fix a {}{round}",
                    text(e, "kind").unwrap_or("failure")
                ),
            };
            said(Sym::Working, head, &[])
        }
        "merge.review_start_failed" => said(
            Sym::Failed,
            "reviewer could not start",
            &[text(e, "cause").and_then(first_line)],
        ),
        "merge.merged" => {
            let sha = text(e, "head").map(|h| h.chars().take(7).collect::<String>());
            said(Sym::Done, format!("merged into {}", snap.branch), &[sha])
        }
        "merge.main_red" => said(
            Sym::Needs,
            "main is red after the merge",
            &[text(e, "gate").map(|g| format!("gate {g}"))],
        ),
        "merge.pr_opened" => said(
            Sym::Review,
            "pull request opened",
            &[count(e, "number").map(|n| format!("#{n}"))],
        ),
        "merge.requeued" => {
            let why = match text(e, "reason") {
                Some("main_moved") => "main moved on".to_string(),
                Some("worker_finished_again") => "the worker finished again".to_string(),
                Some(other) => other.replace('_', " "),
                None => String::new(),
            };
            said(
                Sym::Waiting,
                "back in the merge queue",
                &[(!why.is_empty()).then_some(why)],
            )
        }
        "merge.retried" => said(
            Sym::Waiting,
            "merge retried by you",
            &[text(e, "from").map(|f| format!("was {f}"))],
        ),
        "issue.created" => issue_created(snap, e),
        "issue.updated" => said(Sym::Waiting, "issue edited", &[]),
        "issue.status" => issue_status(e),
        "comment.added" => comment_added(snap, e),
        "hook.denied" => {
            let why = match text(e, "reason") {
                Some("outside_scope") => Some("outside the file scope".to_string()),
                Some("outside_worktree") => Some("outside the worktree".to_string()),
                Some(other) => Some(other.replace('_', " ")),
                None => None,
            };
            said(
                Sym::Waiting,
                "worker edit denied",
                &[text(e, "file").map(str::to_string), why],
            )
        }
        "attention.reminded" => {
            let alert = e.payload.get("immediate").and_then(|v| v.as_bool()) == Some(true);
            let head = if alert {
                "alert: this issue needs you"
            } else {
                "reminder: this issue is waiting for you"
            };
            said(Sym::Needs, head, &[])
        }
        "store.imported" => said(Sym::Waiting, "store imported from the JSONL export", &[]),
        kind => {
            let payload = compact(&e.payload);
            let head = if payload.is_empty() {
                kind.to_string()
            } else {
                format!("{kind} {payload}")
            };
            said(Sym::Waiting, head, &[])
        }
    }
}

/// The events of the snapshot as rows, newest first, without the ones that repeat another line.
pub fn entries(snap: &Snapshot) -> Vec<Entry<'_>> {
    (0..snap.events.len())
        .filter_map(|idx| {
            let e = &snap.events[idx];
            let s = describe(snap, idx)?;
            Some(Entry {
                event: e,
                time: clock(snap, &e.ts),
                issue: if s.show_issue { e.issue_id.clone() } else { None },
                sym: s.sym,
                head: s.head,
                details: s.details,
            })
        })
        .collect()
}

/// The rows the filter lets through.
fn shown<'a>(all: &'a [Entry<'a>], state: &State) -> Vec<&'a Entry<'a>> {
    let needle = state.filter.trim().to_lowercase();
    all.iter()
        .filter(|e| needle.is_empty() || e.matches(&needle))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// STATE
// ---------------------------------------------------------------------------------------------

/// Events the Log lists (the filter is not applied).
#[cfg(test)]
pub fn rows(snap: &Snapshot) -> usize {
    entries(snap).len()
}

/// The selected row among the rows the filter lets through.
#[cfg(test)]
pub fn selected(snap: &Snapshot, state: &State) -> usize {
    let all = entries(snap);
    state.selected.min(shown(&all, state).len().saturating_sub(1))
}

/// Moves or edits; anything that needs the app is an [`Action`].
pub fn handle_key(key: KeyEvent, snap: &Snapshot, state: &mut State) -> Action {
    if state.typing {
        type_key(key, state);
        return Action::None;
    }
    if state.raw {
        if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('J')) {
            state.raw = false;
        }
        return Action::None;
    }
    let all = entries(snap);
    let rows = shown(&all, state);
    let at = state.selected.min(rows.len().saturating_sub(1));
    match key.code {
        KeyCode::Char('/') => state.typing = true,
        KeyCode::Char('J') if !rows.is_empty() => state.raw = true,
        KeyCode::Enter => {
            return match rows.get(at) {
                None => Action::None,
                Some(row) => match &row.event.issue_id {
                    Some(id) => Action::OpenOnBoard(id.clone()),
                    None => Action::Status("this event belongs to no issue".into()),
                },
            };
        }
        KeyCode::Esc if !state.filter.is_empty() => {
            state.filter.clear();
            state.selected = 0;
        }
        _ => {
            if let Some(i) = move_selection(&key, at, rows.len()) {
                state.selected = i;
            }
        }
    }
    Action::None
}

/// While typing: text goes to the filter (also `q`, `t`, digits ...), Enter keeps it, Esc clears it.
fn type_key(key: KeyEvent, state: &mut State) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Esc => {
            state.filter.clear();
            state.typing = false;
        }
        KeyCode::Enter => {
            state.typing = false;
            return;
        }
        KeyCode::Backspace => {
            state.filter.pop();
        }
        KeyCode::Char('u') if ctrl => state.filter.clear(),
        KeyCode::Char('w') if ctrl => {
            let kept = state.filter.trim_end().rfind(' ').map_or(0, |at| at + 1);
            state.filter.truncate(kept);
        }
        KeyCode::Char(c) if !ctrl && !alt => state.filter.push(c),
        _ => return,
    }
    state.selected = 0; // the newest match
}

// ---------------------------------------------------------------------------------------------
// RENDER
// ---------------------------------------------------------------------------------------------

/// Columns in front of the issue: the marker (3), the time (8) and two spaces.
const LEAD: usize = 3 + 8 + 2;
/// Narrowest issue column (`asf-27`).
const ISSUE_MIN: usize = 6;

pub fn render(frame: &mut Frame, area: Rect, snap: &Snapshot, state: &State, theme: &Theme) {
    let all = entries(snap);
    if all.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(format!(" {EMPTY}"), theme.dim())),
            area,
        );
        return;
    }
    let rows = shown(&all, state);
    let [head, _gap, body] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(0)]).areas(area);
    frame.render_widget(Paragraph::new(header(state, rows.len(), all.len(), theme)), head);
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(format!(" {NO_MATCH}"), theme.dim())),
            body,
        );
        return;
    }
    let issue_w = all
        .iter()
        .filter_map(|e| e.issue.as_ref())
        .map(|i| text_width(i.as_str()))
        .max()
        .unwrap_or(0)
        .max(ISSUE_MIN);
    let items: Vec<ListItem> = rows
        .iter()
        .map(|e| ListItem::new(row_line(e, issue_w, area.width as usize, theme)))
        .collect();
    let at = state.selected.min(rows.len() - 1);
    let mut list_state = ListState::default();
    list_state.select(Some(at));
    let list = List::new(items)
        .highlight_symbol(" ▸ ")
        .highlight_style(theme.selected());
    frame.render_stateful_widget(list, body, &mut list_state);
    if state.raw {
        raw_window(frame, area, rows[at].event, theme);
    }
}

/// `showing all issues · newest first · / filter by issue or text`, or the state of the filter.
fn header(state: &State, shown: usize, total: usize, theme: &Theme) -> Line<'static> {
    if state.typing || !state.filter.trim().is_empty() {
        let cursor = if state.typing { "▏" } else { "" };
        return Line::from(vec![
            Span::raw(format!(" showing {}{cursor}", state.filter)),
            Span::styled(format!(" · {shown} of {total} events · Esc clears"), theme.dim()),
        ]);
    }
    Line::from(vec![
        Span::styled(" showing all issues · newest first · ", theme.dim()),
        Span::styled("/", theme.key()),
        Span::styled(" filter by issue or text", theme.dim()),
    ])
}

/// One row, cut to `width` display columns.
fn row_line(e: &Entry, issue_w: usize, width: usize, theme: &Theme) -> Line<'static> {
    let issue = e.issue.as_ref().map(ToString::to_string).unwrap_or_default();
    let glyph_w = if e.sym == Sym::None { 0 } else { 2 };
    let room = width.saturating_sub(LEAD + issue_w + 2 + glyph_w);
    let sentence = e.sentence();
    let cut = trunc(&sentence, room);
    // the cut is a prefix of the sentence (plus an ellipsis): the head ends where the first detail starts
    let head_len = if cut.starts_with(&e.head) {
        e.head.len()
    } else {
        cut.len()
    };
    let (head, details) = cut.split_at(head_len.min(cut.len()));
    let mut spans = vec![
        Span::styled(e.time.clone(), theme.dim()),
        Span::raw("  "),
        Span::styled(format!("{issue:<issue_w$}"), theme.bold()),
        Span::raw("  "),
    ];
    if e.sym != Sym::None {
        spans.push(Span::styled(e.sym.glyph(), e.sym.style(theme)));
        spans.push(Span::raw(" "));
    }
    spans.push(Span::raw(head.to_string()));
    if !details.is_empty() {
        spans.push(Span::styled(details.to_string(), theme.dim()));
    }
    Line::from(spans)
}

/// The raw JSON of one event in a window over the list.
fn raw_window(frame: &mut Frame, area: Rect, event: &Event, theme: &Theme) {
    let popup = area.centered(Constraint::Percentage(80), Constraint::Percentage(80));
    let block = Block::bordered()
        .title(format!(" raw event #{} · {} ", event.seq, event.kind))
        .title_bottom(" J or Esc closes ")
        .border_style(theme.accent());
    let inner = block.inner(popup);
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);
    let json =
        serde_json::to_string_pretty(event).unwrap_or_else(|e| format!("cannot render the event: {e}"));
    let all: Vec<&str> = json.lines().collect();
    let room = inner.height as usize;
    let mut lines: Vec<Line> = all
        .iter()
        .take(room)
        .map(|l| Line::raw(trunc(l, inner.width as usize)))
        .collect();
    if all.len() > room && room > 0 {
        let hidden = all.len() - room + 1;
        lines[room - 1] = Line::styled(format!("… {hidden} more lines"), theme.dim());
    }
    frame.render_widget(Paragraph::new(lines), inner);
}
