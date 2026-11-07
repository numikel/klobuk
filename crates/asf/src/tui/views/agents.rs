//! The Agents view: one row per active run (who works on what, at what cost, how long, what it did
//! last) and, under the table, the last lines of the selected worker's pane.

use asf_core::*;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, HighlightSpacing, Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;

use crate::tui::app::{Action, View};
use crate::tui::bindings::*;
use crate::tui::snapshot::{
    abort_run, age, answer_blocked, move_selection, parse_time, reply_input, run_needs_you, text_width,
    trunc, NeedSource, Snapshot,
};
use crate::tui::theme::{Mark, Theme};

#[derive(Debug, Clone, Default)]
pub struct State {
    pub selected: usize,
}

const AGENTS: &[View] = &[View::Agents];

pub const BINDINGS: &[Binding] = &[
    b(
        "a",
        "answer",
        HELP_ANSWER,
        Group::NeedsYou,
        AGENTS,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "f",
        "focus pane",
        HELP_FOCUS,
        Group::NeedsYou,
        AGENTS,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "r",
        "reply",
        HELP_REPLY,
        Group::NeedsYou,
        AGENTS,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "x",
        "abort",
        HELP_ABORT,
        Group::NeedsYou,
        AGENTS,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "Enter",
        "open issue",
        HELP_OPEN,
        Group::Everywhere,
        AGENTS,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "m",
        "merge",
        "put a verified branch in the merge queue",
        Group::Board,
        AGENTS,
        NORMAL,
        Slot::None,
    ),
];

/// Cells of the budget bar.
const BAR_CELLS: usize = 10;
/// The narrowest the task column is squeezed to before the last-activity column gives way.
const TASK_MIN: i32 = 10;
/// The narrowest the last-activity column is squeezed to.
const ACTIVITY_MIN: i32 = 8;
/// Rows the preview panel needs to be worth drawing: two borders and four lines of pane.
const PREVIEW_MIN: u16 = 6;
/// Rows the table keeps when the preview is drawn: the header, its margin and one run.
const TABLE_MIN: u16 = 3;
/// Width of the selection marker (`▸ `) in front of every row.
const MARKER_WIDTH: i32 = 2;

pub const EMPTY: &str = "No worker is running. Start one from the Board with s, or run asf serve --dispatch.";

/// The active runs in display order: the ones that need the human first, then by start time.
pub fn ordered(snap: &Snapshot) -> Vec<&Run> {
    let mut runs: Vec<&Run> = snap.runs.iter().collect();
    runs.sort_by(|a, b| {
        (!run_needs_you(a.state), &a.started_at, &a.issue_id).cmp(&(
            !run_needs_you(b.state),
            &b.started_at,
            &b.issue_id,
        ))
    });
    runs
}

pub fn rows(snap: &Snapshot) -> usize {
    snap.runs.len()
}

pub fn selected(snap: &Snapshot, state: &State) -> usize {
    state.selected.min(rows(snap).saturating_sub(1))
}

pub fn selected_issue(snap: &Snapshot, state: &State) -> Option<IssueId> {
    ordered(snap)
        .get(selected(snap, state))
        .map(|r| r.issue_id.clone())
}

/// Filled cells of the budget bar: one per tenth of the budget spent, at most [`BAR_CELLS`]; none when
/// the run has no budget.
pub fn bar_cells(cost: f64, budget: Option<f64>) -> usize {
    match budget {
        Some(b) if b > 0.0 => (cost / b * BAR_CELLS as f64).round().clamp(0.0, BAR_CELLS as f64) as usize,
        _ => 0,
    }
}

/// The display name Claude Code's session gets (`claude --name`): `prefix`, a hyphen and a slug of
/// `title`. The same rule as `worker::session_name`, which the store does not record: lowercase ASCII
/// letters and digits, single hyphens, at most 60 characters.
pub fn session_name(prefix: &str, title: &str) -> String {
    const MAX: usize = 60;
    let mut slug = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.truncate(MAX.saturating_sub(prefix.len() + 1));
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}-{slug}")
    }
}

/// One table row, as text, before it is laid out.
struct Entry<'a> {
    run: &'a Run,
    mark: Mark,
    reviewer: bool,
    state: String,
    task: String,
    money: String,
    filled: usize,
    elapsed: String,
    activity: String,
}

impl Entry<'_> {
    fn role(&self) -> &'static str {
        if self.reviewer {
            "reviewer"
        } else {
            "worker"
        }
    }

    /// The symbol and the issue id, as one cell.
    fn issue(&self) -> String {
        format!("{} {}", self.mark.glyph(), self.run.issue_id)
    }
}

fn entry<'a>(snap: &'a Snapshot, run: &'a Run) -> Entry<'a> {
    let issue = snap.issue(&run.issue_id);
    let reviewer = issue.is_some_and(|i| i.kind == IssueType::Review);
    let mark = if run_needs_you(run.state) {
        Mark::Needs
    } else if reviewer {
        Mark::Review
    } else {
        Mark::Working
    };
    // A blocked run is asking; a reviewer reviews, unless its run has gone wrong (then the run state
    // says so, as it does for a worker).
    let state = match run.state {
        RunState::Blocked => "asking".to_string(),
        s if reviewer && !run_needs_you(s) => "reviewing".to_string(),
        s => s.to_string(),
    };
    let own = issue.map_or("(issue not found)", |i| i.title.as_str());
    let task = if reviewer {
        let link = snap.review_of.get(&run.issue_id);
        let reviewed = link
            .and_then(|l| snap.issue(&l.reviews))
            .map_or(own, |i| i.title.as_str());
        let mut task = format!("{reviewed} · {}", snap.review_model);
        if let Some(round) = link.and_then(|l| l.round) {
            task.push_str(&format!(" · r{round}"));
        }
        task
    } else {
        own.to_string()
    };
    Entry {
        run,
        mark,
        reviewer,
        state,
        task,
        money: crate::tui::snapshot::money(run.cost_usd, run.budget_usd),
        filled: bar_cells(run.cost_usd, run.budget_usd),
        elapsed: age(&snap.now, &run.started_at),
        activity: activity(snap, run),
    }
}

/// `asked 4 min ago` for a blocked run (since it began to wait), else how long ago the run was last
/// seen: `12 s ago`, `5 min ago`; `-` when it never was.
fn activity(snap: &Snapshot, run: &Run) -> String {
    if run.state == RunState::Blocked {
        let since = snap
            .needs_you
            .iter()
            .find(|n| n.issue == run.issue_id && n.source == NeedSource::BlockedRun)
            .map(|n| n.since.as_str())
            .or(run.last_seen_at.as_deref())
            .unwrap_or(&run.started_at);
        return format!("asked {} ago", age(&snap.now, since));
    }
    let Some(seen) = run.last_seen_at.as_deref() else {
        return "-".into();
    };
    match (parse_time(&snap.now), parse_time(seen)) {
        (Some(now), Some(seen_at)) => {
            let secs = (now - seen_at).whole_seconds().max(0);
            if secs < 60 {
                format!("{secs} s ago")
            } else {
                format!("{} ago", age(&snap.now, seen))
            }
        }
        _ => "-".into(),
    }
}

/// `s` padded on the left to `width` columns (right-aligned text).
fn right(s: &str, width: usize) -> String {
    format!("{}{s}", " ".repeat(width.saturating_sub(text_width(s))))
}

pub fn render(frame: &mut Frame, area: Rect, snap: &Snapshot, state: &State, theme: &Theme) {
    let area = Rect {
        x: area.x.saturating_add(1),
        width: area.width.saturating_sub(1),
        ..area
    };
    if snap.runs.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(EMPTY, theme.dim())).wrap(Wrap { trim: true }),
            area,
        );
        return;
    }
    let runs = ordered(snap);
    let entries: Vec<Entry> = runs.iter().map(|r| entry(snap, r)).collect();
    let sel = selected(snap, state);

    // The table keeps header + margin + its rows; what is left under it is the preview, when that is
    // tall enough to show something.
    let want = 2 + entries.len() as u16;
    let (table_area, preview_area) = if area.height >= TABLE_MIN + PREVIEW_MIN {
        let [t, p] = Layout::vertical([
            Constraint::Length(want.min(area.height - PREVIEW_MIN)),
            Constraint::Min(PREVIEW_MIN),
        ])
        .areas(area);
        (t, Some(p))
    } else {
        (area, None)
    };
    table(frame, table_area, &entries, sel, theme);
    if let (Some(preview_area), Some(entry)) = (preview_area, entries.get(sel)) {
        preview(frame, preview_area, snap, entry, theme);
    }
}

fn table(frame: &mut Frame, area: Rect, entries: &[Entry], sel: usize, theme: &Theme) {
    let widest = |f: &dyn Fn(&Entry) -> String, header: &str| -> usize {
        entries
            .iter()
            .map(|e| text_width(&f(e)))
            .chain([text_width(header)])
            .max()
            .unwrap_or(0)
    };
    const HEADER: [&str; 7] = [
        "  issue",
        "role",
        "state",
        "task",
        "cost / budget",
        "time",
        "last activity",
    ];
    let w_issue = widest(&|e| e.issue(), HEADER[0]);
    let w_role = widest(&|e| e.role().to_string(), HEADER[1]);
    let w_state = widest(&|e| e.state.clone(), HEADER[2]);
    let w_money = entries.iter().map(|e| text_width(&e.money)).max().unwrap_or(0);
    let w_cost = (w_money + 1 + BAR_CELLS).max(text_width(HEADER[4]));
    let w_time = widest(&|e| e.elapsed.clone(), HEADER[5]);
    let mut w_activity = widest(&|e| e.activity.clone(), HEADER[6]) as i32;

    let spacing: i32 = if area.width >= 110 { 2 } else { 1 };
    let fixed = (w_issue + w_role + w_state + w_cost + w_time) as i32;
    let mut w_task = area.width as i32 - MARKER_WIDTH - 6 * spacing - fixed - w_activity;
    if w_task < TASK_MIN {
        // Too narrow for everything: the last activity gives up columns before the task does.
        let take = (TASK_MIN - w_task).min((w_activity - ACTIVITY_MIN).max(0));
        w_activity -= take;
        w_task += take;
    }
    let w_task = w_task.max(1) as usize;
    let w_activity_cols = w_activity.max(1) as usize;

    let rows: Vec<Row> = entries
        .iter()
        .map(|e| {
            let bar = Line::from(vec![
                Span::raw(format!("{:<w_money$} ", e.money)),
                Span::styled("▓".repeat(e.filled), theme.ok()),
                Span::styled("░".repeat(BAR_CELLS - e.filled), theme.dim()),
            ]);
            Row::new(vec![
                Cell::from(Line::from(vec![
                    Span::styled(format!("{} ", e.mark.glyph()), theme.mark(e.mark)),
                    Span::styled(e.run.issue_id.to_string(), theme.bold()),
                ])),
                Cell::from(e.role()),
                Cell::from(e.state.clone()),
                Cell::from(trunc(&e.task, w_task)),
                Cell::from(bar),
                Cell::from(right(&e.elapsed, w_time)),
                Cell::new(trunc(&e.activity, w_activity_cols)).style(theme.dim()),
            ])
        })
        .collect();
    let header = Row::new([
        HEADER[0].to_string(),
        HEADER[1].to_string(),
        HEADER[2].to_string(),
        HEADER[3].to_string(),
        HEADER[4].to_string(),
        right(HEADER[5], w_time),
        HEADER[6].to_string(),
    ])
    .style(theme.dim())
    .bottom_margin(1);
    let widths = [
        Constraint::Length(w_issue as u16),
        Constraint::Length(w_role as u16),
        Constraint::Length(w_state as u16),
        Constraint::Length(w_task as u16),
        Constraint::Length(w_cost as u16),
        Constraint::Length(w_time as u16),
        Constraint::Length(w_activity_cols as u16),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(spacing as u16)
        .highlight_symbol("▸ ")
        .highlight_spacing(HighlightSpacing::Always)
        .row_highlight_style(theme.selected());
    let mut table_state = TableState::default();
    table_state.select(Some(sel));
    frame.render_stateful_widget(table, area, &mut table_state);
}

/// The bordered panel with the tail of the selected worker's pane.
fn preview(frame: &mut Frame, area: Rect, snap: &Snapshot, entry: &Entry, theme: &Theme) {
    let id = &entry.run.issue_id;
    let name = snap
        .last_run(id)
        .and_then(|l| l.session_name.clone())
        .unwrap_or_else(|| {
            let title = snap.issue(id).map_or("", |i| i.title.as_str());
            if entry.reviewer {
                session_name(&format!("review-{id}"), title)
            } else {
                session_name(id.as_str(), title)
            }
        });
    let title = trunc(
        &format!(" pane preview · {id} · {name} · live "),
        area.width.saturating_sub(4) as usize,
    );
    let block = Block::bordered().title(Span::styled(title, theme.bold()));
    let inner = block.inner(area);
    let body: Vec<Line> = if snap.runtime_ok == Some(false) {
        vec![Line::styled(
            "The runtime is down, so the pane cannot be read.",
            theme.dim(),
        )]
    } else if entry.run.pane_id.is_none() {
        vec![Line::styled("This run has no pane.", theme.dim())]
    } else {
        match snap.preview_of(id) {
            None => vec![Line::styled("No pane text has been read yet.", theme.dim())],
            Some(text) => tail(text, inner.height as usize, inner.width as usize, theme),
        }
    };
    frame.render_widget(Paragraph::new(body).block(block), area);
}

/// The last `rows` non-blank-ended lines of a pane, each cut to `width` columns. Control characters
/// (an escape sequence in the pane text, say) become blanks, so the pane cannot paint on the screen.
fn tail(text: &str, rows: usize, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines: Vec<String> = text
        .lines()
        .map(|l| {
            l.trim_end()
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect()
        })
        .collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        return vec![Line::styled("The pane is empty.", theme.dim())];
    }
    let skip = lines.len().saturating_sub(rows);
    lines
        .into_iter()
        .skip(skip)
        .map(|l| Line::raw(trunc(&l, width)))
        .collect()
}

/// Keys of the Agents view. Moving the selection asks for the pane of the row it lands on
/// (`Action::Peek`), which the preview shows.
pub fn handle_key(key: KeyEvent, snap: &Snapshot, state: &mut State) -> Action {
    if let Some(i) = move_selection(&key, state.selected, rows(snap)) {
        let before = selected_issue(snap, state);
        state.selected = i;
        return match selected_issue(snap, state) {
            Some(id) if before.as_ref() != Some(&id) => Action::Peek(id),
            _ => Action::None,
        };
    }
    let Some(id) = selected_issue(snap, state) else {
        return Action::None;
    };
    match key.code {
        KeyCode::Enter => Action::OpenOnBoard(id),
        KeyCode::Char('m') => Action::Enqueue(id),
        KeyCode::Char('f') => Action::Focus(id),
        KeyCode::Char('a') => answer_blocked(snap, id),
        KeyCode::Char('r') => reply_input(id),
        KeyCode::Char('x') => abort_run(snap, id),
        _ => Action::None,
    }
}
