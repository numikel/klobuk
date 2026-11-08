//! The Queue view: the merge queue as a pipeline. Every item that is not finished shows its five
//! stages (rebase, gates, review, merge, main gates); a failed or blocked item says why and which keys
//! act on it; under the queue, what merged today.
//!
//! The stages are a pure function of the merge item (its state, gate results, error) and of the review
//! events in the snapshot: [`stages`]. The view does no IO.

use asf_core::*;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::tui::app::{Action, View};
use crate::tui::bindings::*;
use crate::tui::snapshot::{
    age, first_line, fit, move_selection, text_width, trunc, Snapshot, REVIEW_STARTED,
};
use crate::tui::theme::{Mark, Theme};

#[derive(Debug, Clone, Default)]
pub struct State {
    pub selected: usize,
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
        "gate output",
        "the full gate output of the item\nin a window",
        Group::Queue,
        QUEUE,
        NORMAL,
        Slot::Hint,
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
fn review_seen(snap: &Snapshot, id: &IssueId) -> bool {
    snap.review_of.values().any(|l| &l.reviews == id)
        || snap
            .events
            .iter()
            .any(|e| e.issue_id.as_ref() == Some(id) && REVIEW_EVENTS.contains(&e.kind.as_str()))
}

/// The round of the review of `id` that is running: the review issue's link first, then the newest
/// `merge.review_started` event.
fn review_round(snap: &Snapshot, id: &IssueId) -> Option<i64> {
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

fn gates_done(item: &MergeItem) -> Stage {
    match item.gate_results.len() {
        0 => Stage::Done("done".into()),
        n => Stage::Done(format!("{n} of {n}")),
    }
}

/// The stage a failed or blocked item stopped at, and what to call it. The queue records no stage, so
/// it is read from the gate results and from the text of the error (`commands/merge.rs`).
fn failure(item: &MergeItem) -> (usize, String) {
    if let Some(bad) = item.gate_results.iter().find(|g| !g.ok) {
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
    match item.state {
        MergeState::Queued => [Pending, Pending, review_ahead(snap, item), Pending, Pending],
        MergeState::Rebasing => [
            Running("rebasing".into()),
            Pending,
            review_ahead(snap, item),
            Pending,
            Pending,
        ],
        MergeState::Gating => [
            done(),
            Running("running".into()),
            review_ahead(snap, item),
            Pending,
            Pending,
        ],
        MergeState::Reviewing => {
            let mut detail = snap.review_model.clone();
            if let Some(round) = review_round(snap, &item.issue_id) {
                detail.push_str(&format!(" · round {round}"));
            }
            [done(), gates_done(item), Running(detail), Pending, Pending]
        }
        MergeState::PrOpen => [
            done(),
            gates_done(item),
            review_behind(snap, item),
            Running("PR open".into()),
            Pending,
        ],
        MergeState::Merging => [
            done(),
            gates_done(item),
            review_behind(snap, item),
            Running("merging".into()),
            Pending,
        ],
        MergeState::Merged => [
            done(),
            gates_done(item),
            review_behind(snap, item),
            done(),
            if item.last_error.is_some() {
                Failed("main red".into())
            } else {
                done()
            },
        ],
        MergeState::Skipped => [After, After, After, After, After],
        MergeState::Failed | MergeState::Blocked => {
            let (at, what) = failure(item);
            let before = |i: usize| match i {
                0 => done(),
                1 => gates_done(item),
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
    let source = item
        .gate_results
        .iter()
        .find(|g| !g.ok)
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

fn item_line(e: &Entry, cols: &Cols, selected: bool, avail: usize, theme: &Theme) -> Line<'static> {
    let mut spans = vec![
        Span::raw(if selected { " ▸ " } else { "   " }),
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
        ("Enter", " full gate output"),
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

/// `review approve` or `no review (review was off)`, then the state of main.
fn merged_text(snap: &Snapshot, item: &MergeItem) -> String {
    let review = if review_seen(snap, &item.issue_id) {
        "review approve"
    } else {
        "no review (review was off)"
    };
    let main = if item.last_error.is_some() {
        "main red"
    } else {
        "gates on main green"
    };
    format!("{review} · {main}")
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
            lines.push(item_line(e, &cols, i == sel, avail, theme));
            if needs_a_fix(e.item) {
                lines.extend(detail_lines(e, &cols, avail, theme));
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
    let top = block.1.saturating_sub(height).min(block.0);
    frame.render_widget(Paragraph::new(lines).scroll((top as u16, 0)), area);
}

// ---------------------------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------------------------

/// The gate output of an item as one text: the error, then every gate with its exit code and tail.
fn gate_output(item: &MergeItem) -> String {
    let mut out = String::new();
    if let Some(e) = &item.last_error {
        out.push_str(e.trim_end());
        out.push_str("\n\n");
    }
    for g in &item.gate_results {
        let verdict = if g.ok { "ok" } else { "FAILED" };
        let exit = g.exit_code.map_or_else(|| "none".to_string(), |c| c.to_string());
        out.push_str(&format!("gate {}: {verdict} (exit {exit})\n", g.name));
        if !g.tail.trim().is_empty() {
            out.push_str(g.tail.trim_end());
            out.push('\n');
        }
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// Keys of the Queue view: navigation, `r` retry, `v` verify, `Enter` the gate output, `f` the reviewer.
pub fn handle_key(key: KeyEvent, snap: &Snapshot, state: &mut State) -> Action {
    if let Some(i) = move_selection(&key, state.selected, rows(snap)) {
        state.selected = i;
        return Action::None;
    }
    let Some(item) = snap.queue.get(selected(snap, state)) else {
        return Action::None;
    };
    let id = item.issue_id.clone();
    match key.code {
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
        KeyCode::Enter => {
            let body = gate_output(item);
            if body.is_empty() {
                return Action::Status(format!("{id} has no gate output yet"));
            }
            Action::OpenComment(Comment {
                id: item.id,
                issue_id: id,
                author: "merge queue".into(),
                kind: CommentType::Status,
                to_lane: LANE_HUMAN.to_string(),
                body,
                reply_to: None,
                resolved_at: None,
                created_at: item.updated_at.clone(),
            })
        }
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
