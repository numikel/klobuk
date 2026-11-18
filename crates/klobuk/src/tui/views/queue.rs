//! The Queue view: the merge queue as a pipeline. Every item that is not finished shows its five
//! stages (rebase, gates, review, merge, main gates); a failed or blocked item says why and which keys
//! act on it; under the queue, what merged today.
//!
//! The stages are a pure function of the merge item (its state, gate results, error) and of the review
//! events in the snapshot: [`stages`]. The view does no IO.

use klobuk_core::*;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::steps::{duration, item_steps, ItemSteps, Step, StepKind, StepMark};
use crate::commands::merge::GATES_REUSED;
use crate::tui::app::{Action, View};
use crate::tui::bindings::*;
use crate::tui::snapshot::{
    age, first_line, fit, move_selection, text_width, trunc, Snapshot, REVIEW_STARTED,
};
use crate::tui::theme::{Mark, Theme};

#[derive(Debug, Clone, Default)]
pub struct State {
    pub selected: usize,
    /// The selected item shows its steps under its row. Moving to another item folds it again.
    pub expanded: bool,
    /// The line of the open item the cursor is on (an earlier attempt, then a step); `None` is the item's
    /// own row.
    pub cursor: Option<usize>,
    /// Numbers of the earlier attempts of the selected item that are shown opened.
    pub opened: Vec<usize>,
}

impl State {
    /// Selects item `i`; the steps of the item that was selected fold.
    fn select(&mut self, i: usize) {
        if i != self.selected {
            self.selected = i;
            self.expanded = false;
            self.cursor = None;
            self.opened.clear();
        }
    }
}

/// A line the cursor can stand on under an open item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cursor {
    /// The folded line of earlier attempt `n`.
    Attempt(usize),
    /// Step `i` of the current attempt.
    Step(usize),
}

fn cursor_lines(steps: &ItemSteps) -> Vec<Cursor> {
    steps
        .earlier
        .iter()
        .map(|a| Cursor::Attempt(a.n))
        .chain((0..steps.steps.len()).map(Cursor::Step))
        .collect()
}

const QUEUE: &[View] = &[View::Queue];

pub const BINDINGS: &[Binding] = &[
    b(
        "j / k",
        "move",
        HELP_MOVE,
        Group::Everywhere,
        QUEUE,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "r",
        "retry",
        "try the failed or blocked merge again\nafter a fix",
        Group::Queue,
        QUEUE,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "v",
        "verify",
        HELP_VERIFY,
        Group::Queue,
        QUEUE,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "Enter",
        "steps",
        "open or fold the steps of the item;\non a step: the live gate output,\nthe output of a finished gate or\nthe reviewer verdict",
        Group::Queue,
        QUEUE,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "l / h",
        "open / fold",
        "open or fold the steps of the item\n(an earlier attempt opens into phases)",
        Group::Queue,
        QUEUE,
        NORMAL,
        Slot::None,
    ),
    b(
        "o",
        "gate output",
        "the full gate output of the item\nin a window",
        Group::Queue,
        QUEUE,
        NORMAL,
        Slot::None,
    ),
    b(
        "f",
        "focus reviewer",
        "focus the reviewer's own Herdr pane",
        Group::Queue,
        QUEUE,
        NORMAL,
        Slot::Hint,
    ),
];

/// What the Queue view says of an item whose gates were not run again (the Log view says it too).
pub const GATES_REUSED_TEXT: &str = "gates reused from verification";

pub const EMPTY: &str = "Queue is empty. A branch arrives here after its worker's run is verified.";

/// The review events that say a reviewer was (or is) involved with an issue's branch.
const REVIEW_EVENTS: [&str; 3] = [REVIEW_STARTED, "merge.review_approved", "merge.review_skipped"];

/// The spinner frame of a running stage. The view has no clock, so it does not turn; the header's
/// spinner (drawn by the chrome) does.
const RUNNING: &str = "⠋";

pub fn rows(snap: &Snapshot) -> usize {
    snap.queue.len()
}

pub fn selected(snap: &Snapshot, state: &State) -> usize {
    state.selected.min(rows(snap).saturating_sub(1))
}

/// The issue of the selected item; the tests' `App::selected_issue` reads it (the keys use the item).
#[cfg(test)]
pub fn selected_issue(snap: &Snapshot, state: &State) -> Option<IssueId> {
    snap.queue.get(selected(snap, state)).map(|m| m.issue_id.clone())
}

// ---------------------------------------------------------------------------------------------
// Stages
// ---------------------------------------------------------------------------------------------

/// Where an item is in one stage of the pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    /// `✓ <detail>`.
    Done(String),
    /// A spinner and the detail (`opus · round 1` for a review).
    Running(String),
    /// `✗ <what failed>` (a gate name, `conflict`, ...).
    Failed(String),
    /// Not reached yet: `○`.
    Pending,
    /// The stage does not apply (review is off for this item): `–`.
    NotNeeded,
    /// Behind a failure, so it will not run: `·`.
    After,
}

const HEADERS: [&str; 5] = ["rebase", "gates", "review", "merge", "main gates"];

/// Whether a reviewer was involved with the branch of `id`: a review issue or a review event.
pub(super) fn review_seen(snap: &Snapshot, id: &IssueId) -> bool {
    snap.review_of.values().any(|l| &l.reviews == id)
        || snap
            .events
            .iter()
            .any(|e| e.issue_id.as_ref() == Some(id) && REVIEW_EVENTS.contains(&e.kind.as_str()))
}

/// The round of the review of `id` that is running: the review issue's link first, then the newest
/// `merge.review_started` event.
pub(super) fn review_round(snap: &Snapshot, id: &IssueId) -> Option<i64> {
    snap.review_of
        .values()
        .find(|l| &l.reviews == id)
        .and_then(|l| l.round)
        .or_else(|| {
            snap.events
                .iter()
                .find(|e| e.kind == REVIEW_STARTED && e.issue_id.as_ref() == Some(id))
                .and_then(|e| e.payload["round"].as_i64())
        })
}

/// Whether `[review] required` or one of the issue's labels in `[review] on_labels` asks for a
/// reviewer, or one was involved already.
fn review_wanted(snap: &Snapshot, item: &MergeItem) -> bool {
    snap.review_required
        || review_seen(snap, &item.issue_id)
        || snap
            .issue(&item.issue_id)
            .is_some_and(|i| i.labels.iter().any(|l| snap.review_on_labels.contains(l)))
}

/// The review stage of an item that has not reached its review: not reached yet, or `–` when no
/// review will happen (`[review] required` is off and no label asks for one).
fn review_ahead(snap: &Snapshot, item: &MergeItem) -> Stage {
    if review_wanted(snap, item) {
        Stage::Pending
    } else {
        Stage::NotNeeded
    }
}

/// The review stage of an item that is past it: done when a reviewer was involved, else it was off.
fn review_behind(snap: &Snapshot, item: &MergeItem) -> Stage {
    if review_seen(snap, &item.issue_id) {
        Stage::Done("done".into())
    } else {
        Stage::NotNeeded
    }
}

/// The main-gates stage of an item that has not merged: to come, or `–` when `[merge] verify_main_after`
/// is off and no gate will ever run on main.
fn main_ahead(snap: &Snapshot) -> Stage {
    if snap.verify_main_after {
        Stage::Pending
    } else {
        Stage::NotNeeded
    }
}

/// Whether the gates of the item's latest attempt were not run because the worker's verification had
/// passed them on this very commit (`[merge] reuse_verification`). The event names the attempt, so an
/// older reuse is not read into a later attempt that ran the gates.
pub(super) fn gates_reused(snap: &Snapshot, item: &MergeItem) -> bool {
    snap.events.iter().any(|e| {
        e.kind == GATES_REUSED && e.payload["merge_id"] == item.id && e.payload["attempt"] == item.attempts
    })
}

fn gates_done(snap: &Snapshot, item: &MergeItem) -> Stage {
    if gates_reused(snap, item) {
        return Stage::Done("reused".into());
    }
    match item.gate_results.len() {
        0 => Stage::Done("done".into()),
        n => Stage::Done(format!("{n} of {n}")),
    }
}

/// The failing gate the item stopped at. `klobuk merge retry` clears the error but keeps the gate results,
/// and a rebase or scope failure keeps them too, so a failed gate is only the cause when the error says
/// so (`gate `<name>` failed …`, `commands/merge.rs`). An item with no error at all falls back to the
/// first failing gate.
fn failed_gate(item: &MergeItem) -> Option<&GateResult> {
    let bad = |name: Option<&str>| {
        item.gate_results
            .iter()
            .find(|g| !g.ok && name.is_none_or(|n| g.name == n))
    };
    match item.last_error.as_deref().and_then(first_line) {
        None => bad(None),
        Some(error) => {
            let name = error.strip_prefix("gate `")?.split('`').next()?;
            bad(Some(name)).or_else(|| bad(None))
        }
    }
}

/// The stage a failed or blocked item stopped at, and what to call it. The queue records no stage, so
/// it is read from the gate results and from the text of the error (`commands/merge.rs`).
fn failure(item: &MergeItem) -> (usize, String) {
    if let Some(bad) = failed_gate(item) {
        return (1, bad.name.clone());
    }
    let error = item.last_error.as_deref().and_then(first_line).unwrap_or("");
    let at = |stage: usize, what: &str| (stage, what.to_string());
    if error.starts_with("rebase of") && error.contains("conflicts in") {
        at(0, "conflict")
    } else if error.starts_with("rebase failed")
        || error.starts_with("cannot sync")
        || error.starts_with("worktree ")
    {
        at(0, "rebase")
    } else if error.contains("outside scope") {
        at(1, "scope")
    } else if error.starts_with("review asks for changes") || error.contains("asked for changes") {
        at(2, "changes")
    } else if error.starts_with("cannot start") || error.contains("reviewer") || error.contains("max_rounds")
    {
        at(2, "review")
    } else if error.contains("fast-forward") || error.starts_with("main checkout") {
        at(3, "merge")
    } else if item.gate_results.is_empty() {
        at(0, "error")
    } else {
        at(3, "error")
    }
}

/// The five stages of `item`, in pipeline order: rebase, gates, review, merge, main gates.
pub fn stages(snap: &Snapshot, item: &MergeItem) -> [Stage; 5] {
    use Stage::*;
    let done = || Done("done".into());
    let main = main_ahead(snap);
    match item.state {
        MergeState::Queued => [Pending, Pending, review_ahead(snap, item), Pending, main],
        MergeState::Rebasing => [
            Running("rebasing".into()),
            Pending,
            review_ahead(snap, item),
            Pending,
            main,
        ],
        MergeState::Gating => [
            done(),
            Running("running".into()),
            review_ahead(snap, item),
            Pending,
            main,
        ],
        MergeState::Reviewing => {
            let mut detail = snap.review_model.clone();
            if let Some(round) = review_round(snap, &item.issue_id) {
                detail.push_str(&format!(" · round {round}"));
            }
            [done(), gates_done(snap, item), Running(detail), Pending, main]
        }
        MergeState::PrOpen => [
            done(),
            gates_done(snap, item),
            review_behind(snap, item),
            Running("PR open".into()),
            main,
        ],
        MergeState::Merging => [
            done(),
            gates_done(snap, item),
            review_behind(snap, item),
            Running("merging".into()),
            main,
        ],
        MergeState::Merged => [
            done(),
            gates_done(snap, item),
            review_behind(snap, item),
            done(),
            // a red main is a fact whatever the config says now; green only if it was checked
            if item.last_error.is_some() {
                Failed("main red".into())
            } else if snap.verify_main_after {
                done()
            } else {
                NotNeeded
            },
        ],
        MergeState::Skipped => [After, After, After, After, After],
        MergeState::Failed | MergeState::Blocked => {
            let (at, what) = failure(item);
            let before = |i: usize| match i {
                0 => done(),
                1 => gates_done(snap, item),
                _ => review_behind(snap, item),
            };
            std::array::from_fn(|i| match i.cmp(&at) {
                std::cmp::Ordering::Less => before(i),
                std::cmp::Ordering::Equal => Failed(what.clone()),
                std::cmp::Ordering::Greater => After,
            })
        }
    }
}

/// The text and style of one stage cell.
fn cell(stage: &Stage, theme: &Theme) -> (String, Style) {
    match stage {
        Stage::Done(t) => (format!("✓ {t}"), theme.dim()),
        Stage::Running(t) => (format!("{RUNNING} {t}"), theme.accent()),
        Stage::Failed(t) => (format!("✗ {t}"), theme.danger()),
        Stage::Pending => ("○".into(), Style::default()),
        Stage::NotNeeded => ("–".into(), theme.dim()),
        Stage::After => ("·".into(), theme.dim()),
    }
}

/// The symbol of a queue row.
fn item_mark(item: &MergeItem) -> Mark {
    match item.state {
        MergeState::Failed | MergeState::Blocked => Mark::Needs,
        MergeState::Queued => Mark::Waiting,
        MergeState::Merged | MergeState::Skipped => Mark::Done,
        _ => Mark::Review,
    }
}

fn needs_a_fix(item: &MergeItem) -> bool {
    matches!(item.state, MergeState::Failed | MergeState::Blocked)
}

// ---------------------------------------------------------------------------------------------
// Failed tests
// ---------------------------------------------------------------------------------------------

/// The names of the tests that failed, from cargo's output: the `test <name> ... FAILED` lines, the
/// `---- <name> stdout ----` headings and the `failures:` list, each once, in order of appearance.
pub fn failed_tests(output: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut push = |name: &str| {
        if !name.is_empty() && !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
    };
    let (mut in_list, mut listed) = (false, false);
    for line in output.lines() {
        let t = line.trim();
        if in_list {
            if line.starts_with("    ") && !t.is_empty() && !t.contains(' ') {
                push(t);
                listed = true;
                continue;
            }
            if t.is_empty() && !listed {
                continue;
            }
            in_list = false;
        }
        if t == "failures:" {
            (in_list, listed) = (true, false);
        } else if let Some(name) = t
            .strip_prefix("test ")
            .and_then(|r| r.strip_suffix(" ... FAILED"))
        {
            push(name);
        } else if let Some(name) = t
            .strip_prefix("---- ")
            .and_then(|r| r.strip_suffix(" stdout ----"))
        {
            push(name);
        }
    }
    names
}

/// `2 tests failed: a, b`, from the failing gate's output (else from the error); `None` without names.
fn failed_tests_line(item: &MergeItem) -> Option<String> {
    let source = failed_gate(item)
        .map(|g| g.tail.as_str())
        .or(item.last_error.as_deref())?;
    let names = failed_tests(source);
    let n = names.len();
    (n > 0).then(|| {
        format!(
            "{n} test{} failed: {}",
            if n == 1 { "" } else { "s" },
            names.join(", ")
        )
    })
}

// ---------------------------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------------------------

/// Columns of the queue table. Every width includes the two blanks that follow the cell.
struct Cols {
    id: usize,
    /// Columns in front of the branch: margin, selection marker, symbol, id and the gap after it.
    lead: usize,
    branch: usize,
    stages: [usize; 5],
    age: usize,
}

impl Cols {
    /// Where the first stage starts, and so where the lines under an item are indented.
    fn indent(&self) -> usize {
        self.lead + self.branch
    }

    fn total(&self) -> usize {
        self.indent() + self.stages.iter().sum::<usize>() + self.age
    }
}

/// The widths of the table at `avail` columns: the mockup's when there is room, then as narrow as the
/// content allows, then as narrow as the cells can be cut.
fn layout(avail: usize, entries: &[Entry], id: usize) -> Cols {
    const GAP: usize = 2;
    const WANT: [usize; 5] = [12, 12, 18, 12, 12];
    let widest = |i: usize| {
        entries
            .iter()
            .map(|e| text_width(&e.cells[i].0))
            .chain([text_width(HEADERS[i])])
            .max()
            .unwrap_or(0)
            + GAP
    };
    let branch_len = entries
        .iter()
        .map(|e| text_width(&e.item.branch))
        .max()
        .unwrap_or(0);
    let age = entries
        .iter()
        .map(|e| text_width(&e.age))
        .chain([6])
        .max()
        .unwrap_or(6);
    let lead = 1 + 2 + 2 + id + GAP;
    // [branch, rebase, gates, review, merge, main gates]
    let mut w = [
        branch_len + 6,
        WANT[0].max(widest(0)),
        WANT[1].max(widest(1)),
        WANT[2].max(widest(2)),
        WANT[3].max(widest(3)),
        WANT[4].max(widest(4)),
    ];
    let content_min = [
        branch_len + GAP,
        widest(0),
        widest(1),
        widest(2),
        widest(3),
        widest(4),
    ];
    let cut_min = [10, 8, 8, 8, 8, 8];
    let last_min = [6, 4, 4, 4, 4, 4];
    let order = [0, 3, 2, 5, 4, 1];
    for min in [content_min, cut_min, last_min] {
        for i in order {
            let excess = (lead + w.iter().sum::<usize>() + age).saturating_sub(avail);
            if excess == 0 {
                break;
            }
            let floor = min[i].min(w[i]);
            w[i] -= excess.min(w[i] - floor);
        }
    }
    Cols {
        id,
        lead,
        branch: w[0],
        stages: [w[1], w[2], w[3], w[4], w[5]],
        age,
    }
}

/// One queue item with everything the table prints, before it is laid out.
struct Entry<'a> {
    item: &'a MergeItem,
    mark: Mark,
    cells: [(String, Style); 5],
    age: String,
}

fn entry<'a>(snap: &Snapshot, item: &'a MergeItem, theme: &Theme) -> Entry<'a> {
    Entry {
        item,
        mark: item_mark(item),
        cells: stages(snap, item).map(|s| cell(&s, theme)),
        age: age(&snap.now, &item.updated_at),
    }
}

/// `text` cut to `n` columns and followed by the two blanks of the gap.
fn cut(text: &str, n: usize) -> String {
    format!("{}  ", fit(text, n.saturating_sub(2)))
}

fn header_line(cols: &Cols, title: &str, theme: &Theme) -> Line<'static> {
    let mut spans = vec![
        Span::raw(" "),
        Span::styled(fit(title, cols.indent() - 1), theme.bold()),
    ];
    for (i, h) in HEADERS.iter().enumerate() {
        spans.push(Span::styled(cut(h, cols.stages[i]), theme.dim()));
    }
    Line::from(spans)
}

fn item_line(
    e: &Entry,
    cols: &Cols,
    (selected, open): (bool, bool),
    avail: usize,
    theme: &Theme,
) -> Line<'static> {
    let mut spans = vec![
        Span::raw(match (selected, open) {
            (true, true) => " ▾ ",
            (true, false) => " ▸ ",
            _ => "   ",
        }),
        Span::styled(format!("{} ", e.mark.glyph()), theme.mark(e.mark)),
        Span::styled(fit(e.item.issue_id.as_str(), cols.id), theme.bold()),
        Span::raw("  "),
        Span::raw(cut(&e.item.branch, cols.branch)),
    ];
    for (i, (text, style)) in e.cells.iter().enumerate() {
        spans.push(Span::styled(cut(text, cols.stages[i]), *style));
    }
    spans.push(Span::styled(format!("{:>w$}", e.age, w = cols.age), theme.dim()));
    let used: usize = spans.iter().map(|s| s.width()).sum();
    spans.push(Span::raw(" ".repeat(avail.saturating_sub(used))));
    let line = Line::from(spans);
    if selected {
        line.style(theme.selected())
    } else {
        line
    }
}

/// The dim lines under a failed or blocked item: the error with the attempt, the failed tests when
/// the gate output has them, and the keys that act on it.
fn detail_lines(e: &Entry, cols: &Cols, avail: usize, theme: &Theme) -> Vec<Line<'static>> {
    let indent = cols.indent();
    let room = avail.saturating_sub(indent).max(1);
    let pad = || Span::raw(" ".repeat(indent));
    let item = e.item;
    let error = item
        .last_error
        .as_deref()
        .and_then(first_line)
        .unwrap_or("no error recorded")
        .trim_end_matches(':');
    let attempt = format!(" · attempt {}", item.attempts);
    let error = format!(
        "{}{attempt}",
        trunc(error, room.saturating_sub(text_width(&attempt)))
    );
    let mut lines = vec![Line::from(vec![pad(), Span::styled(error, theme.dim())])];
    if let Some(tests) = failed_tests_line(item) {
        lines.push(Line::from(vec![
            pad(),
            Span::styled(trunc(&tests, room), theme.dim()),
        ]));
    }
    let keys = [
        ("r", " retry after a fix"),
        ("v", " verify the run again"),
        ("Enter", " steps"),
        ("o", " full gate output"),
    ];
    let plain = keys
        .iter()
        .map(|(k, t)| format!("{k}{t}"))
        .collect::<Vec<_>>()
        .join(" · ");
    let mut spans = vec![pad()];
    if text_width(&plain) <= room {
        for (i, (k, t)) in keys.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" · ", theme.dim()));
            }
            spans.push(Span::styled(*k, theme.key()));
            spans.push(Span::styled(*t, theme.dim()));
        }
    } else {
        spans.push(Span::styled(trunc(&plain, room), theme.dim()));
    }
    lines.push(Line::from(spans));
    lines
}

/// `review approve` or `no review (review was off)`, then the state of main: `main red`, or
/// `gates on main green` when `[merge] verify_main_after` ran them. With it off nothing checked main,
/// so nothing is said about it (as in the mockup's rows without a main part).
fn merged_text(snap: &Snapshot, item: &MergeItem) -> String {
    let review = if review_seen(snap, &item.issue_id) {
        "review approve"
    } else {
        "no review (review was off)"
    };
    if item.last_error.is_some() {
        format!("{review} · main red")
    } else if snap.verify_main_after {
        format!("{review} · gates on main green")
    } else {
        review.to_string()
    }
}

fn merged_line(snap: &Snapshot, item: &MergeItem, id: usize, edge: usize, theme: &Theme) -> Line<'static> {
    let sha: String = item.head_sha.as_deref().unwrap_or("-").chars().take(7).collect();
    let ago = format!("{} ago", age(&snap.now, &item.updated_at));
    let used = 3 + 2 + id + 2 + 7 + 2;
    let room = edge.saturating_sub(used + text_width(&ago) + 2).max(1);
    let text = fit(&merged_text(snap, item), room);
    let text_style = if item.last_error.is_some() {
        theme.danger()
    } else {
        Style::default()
    };
    Line::from(vec![
        Span::raw("   "),
        Span::styled("✓ ", theme.mark(Mark::Done)),
        Span::styled(fit(item.issue_id.as_str(), id), theme.bold()),
        Span::raw("  "),
        Span::raw(format!("{sha:<7}  ")),
        Span::styled(text, text_style),
        Span::raw("  "),
        Span::styled(ago, theme.dim()),
    ])
}

/// The glyph and style of a step mark.
fn step_mark(mark: StepMark, theme: &Theme) -> (&'static str, Style) {
    match mark {
        StepMark::Done => ("✓", theme.ok()),
        StepMark::Running => ("●", theme.accent()),
        StepMark::Waiting => ("○", Style::default()),
        StepMark::Failed => ("✗", theme.danger()),
        StepMark::Skipped => ("–", theme.dim()),
    }
}

/// One line of a step list: `indent`, a cursor, the mark, the label and, at the right edge, the note and
/// the time (the one of a running step counts up with the snapshot's clock).
fn step_line(
    snap: &Snapshot,
    step: &Step,
    under: usize,
    on_cursor: bool,
    avail: usize,
    theme: &Theme,
) -> Line<'static> {
    let (glyph, style) = step_mark(step.mark, theme);
    let time = step.seconds(snap).map(duration).unwrap_or_default();
    let right = match (step.note.is_empty(), time.is_empty()) {
        (true, true) => String::new(),
        (false, true) => step.note.clone(),
        (true, false) => time,
        (false, false) => format!("{}  {time}", step.note),
    };
    let lead = under + 2 + 2;
    let right_w = text_width(&right);
    let room = avail.saturating_sub(lead + right_w + 2).max(8);
    let label = fit(&step.label, room.min(text_width(&step.label).max(1)));
    let gap = avail.saturating_sub(lead + text_width(&label) + right_w);
    let mut line = Line::from(vec![
        Span::raw(" ".repeat(under)),
        Span::raw(if on_cursor { "▸ " } else { "  " }),
        Span::styled(format!("{glyph} "), style),
        Span::raw(label),
        Span::raw(" ".repeat(gap)),
        Span::styled(
            right,
            if step.mark == StepMark::Failed {
                theme.danger()
            } else {
                theme.dim()
            },
        ),
    ]);
    if on_cursor {
        line = line.style(theme.selected());
    }
    line
}

/// The lines under the row of the open item, and the index (among them) of the one the cursor is on.
fn open_lines(
    snap: &Snapshot,
    item: &MergeItem,
    state: &State,
    cols: &Cols,
    avail: usize,
    theme: &Theme,
) -> (Vec<Line<'static>>, Option<usize>) {
    let steps = item_steps(snap, item);
    let under = cols.lead;
    let room = avail.saturating_sub(under + 2).max(1);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut at = None;
    let mut index = 0;
    let mut head = format!("attempt {}", steps.attempt);
    if let Some(secs) = steps.elapsed {
        head.push_str(&format!(" · {}", duration(secs)));
    }
    out.push(Line::from(vec![
        Span::raw(" ".repeat(under)),
        Span::styled(trunc(&head, room), theme.dim()),
    ]));
    for a in &steps.earlier {
        let on_cursor = state.cursor == Some(index);
        if on_cursor {
            at = Some(out.len());
        }
        let open = state.opened.contains(&a.n);
        let text = format!("{} {}", if open { "▾" } else { "▸" }, a.text);
        let mut line = Line::from(vec![
            Span::raw(" ".repeat(under)),
            Span::styled(fit(&text, room), theme.dim()),
        ]);
        if on_cursor {
            line = line.style(theme.selected());
        }
        out.push(line);
        if open {
            out.extend(
                a.phases
                    .iter()
                    .map(|p| step_line(snap, p, under + 4, false, avail, theme)),
            );
        }
        index += 1;
    }
    for step in &steps.steps {
        let on_cursor = state.cursor == Some(index);
        if on_cursor {
            at = Some(out.len());
        }
        out.push(step_line(snap, step, under, on_cursor, avail, theme));
        index += 1;
    }
    (out, at)
}

pub fn render(frame: &mut Frame, area: Rect, snap: &Snapshot, state: &State, theme: &Theme) {
    let avail = area.width as usize;
    let entries: Vec<Entry> = snap.queue.iter().map(|m| entry(snap, m, theme)).collect();
    let id_w = entries
        .iter()
        .map(|e| text_width(e.item.issue_id.as_str()))
        .chain(snap.merged_today.iter().map(|m| text_width(m.issue_id.as_str())))
        .max()
        .unwrap_or(0);
    let cols = layout(avail, &entries, id_w);
    let sel = selected(snap, state);

    let mut lines: Vec<Line> = Vec::new();
    let mut block = (0, 0);
    let mut cursor_line = None;
    if entries.is_empty() {
        lines.push(Line::styled(format!(" {EMPTY}"), theme.dim()));
    } else {
        lines.push(header_line(
            &cols,
            &format!("IN THE QUEUE ({})", entries.len()),
            theme,
        ));
        lines.push(Line::raw(""));
        for (i, e) in entries.iter().enumerate() {
            let start = lines.len();
            let open = i == sel && state.expanded;
            lines.push(item_line(e, &cols, (i == sel, open), avail, theme));
            if needs_a_fix(e.item) {
                lines.extend(detail_lines(e, &cols, avail, theme));
            }
            if open {
                let (steps, at) = open_lines(snap, e.item, state, &cols, avail, theme);
                cursor_line = at.map(|a| lines.len() + a);
                lines.extend(steps);
            } else if !needs_a_fix(e.item) && gates_reused(snap, e.item) {
                let indent = cols.indent();
                let room = avail.saturating_sub(indent).max(1);
                lines.push(Line::from(vec![
                    Span::raw(" ".repeat(indent)),
                    Span::styled(trunc(GATES_REUSED_TEXT, room), theme.dim()),
                ]));
            }
            if i == sel {
                block = (start, lines.len());
            }
            lines.push(Line::raw(""));
        }
    }
    if !snap.merged_today.is_empty() {
        if entries.is_empty() {
            lines.push(Line::raw(""));
        }
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                format!("MERGED TODAY ({})", snap.merged_today.len()),
                theme.bold(),
            ),
        ]));
        lines.push(Line::raw(""));
        let edge = if entries.is_empty() {
            avail
        } else {
            cols.total().min(avail)
        };
        for m in &snap.merged_today {
            lines.push(merged_line(snap, m, id_w, edge, theme));
        }
    }

    // Keep the selected item (all its lines, as far as they fit) on screen.
    let height = area.height as usize;
    let mut top = block.1.saturating_sub(height).min(block.0);
    if let Some(at) = cursor_line {
        top = top.min(at).max((at + 1).saturating_sub(height));
    }
    frame.render_widget(Paragraph::new(lines).scroll((top as u16, 0)), area);
}

// ---------------------------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------------------------

/// One gate as the output window shows it: its verdict and exit code, then the tail it kept.
fn gate_section(g: &GateResult) -> String {
    let verdict = if g.ok { "ok" } else { "FAILED" };
    let exit = g.exit_code.map_or_else(|| "none".to_string(), |c| c.to_string());
    let mut out = format!("gate {}: {verdict} (exit {exit})\n", g.name);
    if !g.tail.trim().is_empty() {
        out.push_str(g.tail.trim_end());
        out.push('\n');
    }
    out
}

/// The gate output of an item as one text: the error, whether the gates were reused from the
/// verification, then every gate with its exit code and tail.
fn gate_output(snap: &Snapshot, item: &MergeItem) -> String {
    let mut out = String::new();
    if let Some(e) = &item.last_error {
        out.push_str(e.trim_end());
        out.push_str("\n\n");
    }
    if gates_reused(snap, item) {
        out.push_str(GATES_REUSED_TEXT);
        out.push_str("\n\n");
    }
    for g in &item.gate_results {
        out.push_str(&gate_section(g));
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// A window of the merge queue with `body`, titled by the item.
fn open_text(item: &MergeItem, body: String) -> Action {
    Action::OpenComment(Comment {
        id: item.id,
        issue_id: item.issue_id.clone(),
        author: "merge queue".into(),
        kind: CommentType::Status,
        to_lane: LANE_HUMAN.to_string(),
        body,
        reply_to: None,
        resolved_at: None,
        created_at: item.updated_at.clone(),
    })
}

/// The output of the gate `name` of `item`: from the gates of the run in flight that are over, or from
/// the results of its last run.
fn finished_gate_output(snap: &Snapshot, item: &MergeItem, name: &str) -> Option<String> {
    if let Some(live) = snap
        .gate_live
        .as_ref()
        .filter(|l| l.kind == "merge" && l.issue == item.issue_id && item.state == MergeState::Gating)
    {
        let g = live.finished.iter().find(|g| g.name == name)?;
        let verdict = if g.ok { "ok" } else { "FAILED" };
        let mut out = format!(
            "gate {}: {verdict} ({})\n",
            g.name,
            duration((g.duration_ms / 1000) as i64)
        );
        if !g.tail.trim().is_empty() {
            out.push_str(g.tail.trim_end());
            out.push('\n');
        }
        return Some(out);
    }
    item.gate_results
        .iter()
        .find(|g| g.name == name)
        .map(gate_section)
}

/// What `Enter` does on a step of the open item.
fn activate_step(snap: &Snapshot, item: &MergeItem, step: &Step) -> Action {
    let id = &item.issue_id;
    match (&step.kind, step.mark) {
        (StepKind::Gate(_), StepMark::Running) => Action::OpenGate(id.clone()),
        (StepKind::Gate(name), StepMark::Done | StepMark::Failed) => {
            match finished_gate_output(snap, item, name) {
                Some(body) => open_text(item, body),
                None => Action::Status(format!("{id}: no output kept for gate {name}")),
            }
        }
        (StepKind::Review, _) => match snap.verdicts.get(id) {
            Some(v) => Action::OpenComment(v.clone()),
            None => match item
                .last_error
                .as_deref()
                .filter(|e| e.starts_with("review asks for changes"))
            {
                Some(error) => open_text(item, error.to_string()),
                None => Action::Status(format!("{id}: the reviewer has given no verdict yet")),
            },
        },
        _ if !step.note.is_empty() => Action::Status(format!("{id}: {} · {}", step.label, step.note)),
        _ => Action::Status(format!("{id}: {}", step.label)),
    }
}

/// Keys of the Queue view: navigation (j/k walk the lines of the open item before they leave it), `l`/`h`
/// open and fold the steps, `Enter` acts on the line the cursor is on, `r` retry, `v` verify, `o` the
/// gate output, `f` the reviewer.
pub fn handle_key(key: KeyEvent, snap: &Snapshot, state: &mut State) -> Action {
    let sel = selected(snap, state);
    if state.expanded {
        if let Some(item) = snap.queue.get(sel) {
            let lines = cursor_lines(&item_steps(snap, item)).len();
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    let next = state.cursor.map_or(0, |c| c + 1);
                    if next < lines {
                        state.cursor = Some(next);
                        return Action::None;
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => match state.cursor {
                    Some(0) => {
                        state.cursor = None;
                        return Action::None;
                    }
                    Some(c) => {
                        state.cursor = Some(c.min(lines) - 1);
                        return Action::None;
                    }
                    None => {}
                },
                _ => {}
            }
        }
    }
    if let Some(i) = move_selection(&key, sel, rows(snap)) {
        state.select(i);
        return Action::None;
    }
    state.selected = sel;
    let Some(item) = snap.queue.get(sel) else {
        return Action::None;
    };
    let id = item.issue_id.clone();
    match key.code {
        KeyCode::Char('l') | KeyCode::Right => {
            state.expanded = true;
            Action::None
        }
        KeyCode::Char('h') | KeyCode::Left => {
            if state.cursor.is_some() {
                state.cursor = None;
            } else {
                state.expanded = false;
            }
            Action::None
        }
        KeyCode::Enter => match (state.expanded, state.cursor) {
            (false, _) => {
                state.expanded = true;
                Action::None
            }
            (true, None) => {
                state.expanded = false;
                Action::None
            }
            (true, Some(c)) => {
                let steps = item_steps(snap, item);
                match cursor_lines(&steps).get(c) {
                    Some(Cursor::Attempt(n)) => {
                        match state.opened.iter().position(|o| o == n) {
                            Some(at) => {
                                state.opened.remove(at);
                            }
                            None => state.opened.push(*n),
                        }
                        Action::None
                    }
                    Some(Cursor::Step(i)) => activate_step(snap, item, &steps.steps[*i]),
                    None => Action::None,
                }
            }
        },
        KeyCode::Char('o') => {
            let body = gate_output(snap, item);
            if body.is_empty() {
                return Action::Status(format!("{id} has no gate output yet"));
            }
            open_text(item, body)
        }
        KeyCode::Char('r') if needs_a_fix(item) => Action::Retry(id),
        KeyCode::Char('r') => Action::Status(format!(
            "{id}: r retries a failed or blocked merge; it is {}",
            item.state
        )),
        KeyCode::Char('v') if needs_a_fix(item) => Action::Verify(id),
        KeyCode::Char('v') => Action::Status(format!(
            "{id}: v verifies the run of a failed or blocked merge; it is {}",
            item.state
        )),
        KeyCode::Char('f') => {
            let reviewer = snap
                .review_of
                .iter()
                .find(|(rid, link)| link.reviews == id && snap.run_for(rid).is_some())
                .map(|(rid, _)| rid.clone());
            match reviewer {
                Some(rid) => Action::Focus(rid),
                None => Action::Status(format!("{id} has no reviewer running")),
            }
        }
        _ => Action::None,
    }
}
