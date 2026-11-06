//! The Agents view: one row per active run.

use asf_core::*;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState};
use ratatui::Frame;

use crate::tui::app::{Action, View};
use crate::tui::bindings::*;
use crate::tui::snapshot::{
    abort_run, answer_blocked, move_selection, reply_input, run_needs_you, trunc, Snapshot,
};
use crate::tui::theme::{Mark, Theme};

#[derive(Debug, Clone, Default)]
pub struct State {
    pub selected: usize,
}

const AGENTS: &[View] = &[View::Agents];

pub const BINDINGS: &[Binding] = &[
    b(
        "f",
        "focus",
        HELP_FOCUS,
        Group::NeedsYou,
        AGENTS,
        NORMAL,
        Slot::Hint,
    ),
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
        "m",
        "merge",
        "put a verified branch in the merge queue",
        Group::Board,
        AGENTS,
        NORMAL,
        Slot::None,
    ),
];

pub fn rows(snap: &Snapshot) -> usize {
    snap.runs.len()
}

pub fn selected(snap: &Snapshot, state: &State) -> usize {
    state.selected.min(rows(snap).saturating_sub(1))
}

pub fn selected_issue(snap: &Snapshot, state: &State) -> Option<IssueId> {
    snap.runs.get(selected(snap, state)).map(|r| r.issue_id.clone())
}

pub fn render(frame: &mut Frame, area: Rect, snap: &Snapshot, state: &State, theme: &Theme) {
    let block = Block::bordered().title(" agents ");
    if snap.runs.is_empty() {
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new(Line::styled(
                " No worker is running. Press 2 for the Board and s to start one.",
                theme.dim(),
            )),
            inner,
        );
        return;
    }
    let rows: Vec<Row> = snap
        .runs
        .iter()
        .map(|r| {
            let agent = snap
                .agents
                .get(&r.issue_id)
                .map(|a| a.state.to_string())
                .unwrap_or_else(|| "?".into());
            let needs = run_needs_you(r.state);
            let mark = if needs {
                Mark::Needs
            } else if snap
                .issue(&r.issue_id)
                .is_some_and(|i| i.kind == IssueType::Review)
            {
                Mark::Review
            } else {
                Mark::Working
            };
            let row = Row::new(vec![
                Cell::from(Line::from(vec![
                    Span::styled(format!("{} ", mark.glyph()), theme.mark(mark)),
                    Span::raw(r.issue_id.to_string()),
                ])),
                Cell::from(r.state.to_string()),
                Cell::from(agent),
                Cell::from(format!("${:.2}", r.cost_usd)),
                Cell::from(r.tool_calls.to_string()),
                Cell::from(r.pane_id.clone().unwrap_or_else(|| "-".into())),
                Cell::from(trunc(r.exit_reason.as_deref().unwrap_or(""), 40)),
            ]);
            if needs {
                row.style(theme.danger())
            } else {
                row
            }
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(12),
            Constraint::Length(12),
            Constraint::Length(9),
            Constraint::Length(8),
            Constraint::Length(6),
            Constraint::Length(10),
            Constraint::Min(10),
        ],
    )
    .header(Row::new(["issue", "run", "agent", "cost", "calls", "pane", "note"]).style(theme.bold()))
    .row_highlight_style(theme.selected())
    .block(block);
    let mut table_state = TableState::default();
    table_state.select(Some(selected(snap, state)));
    frame.render_stateful_widget(table, area, &mut table_state);
}

pub fn handle_key(key: KeyEvent, snap: &Snapshot, state: &mut State) -> Action {
    if let Some(i) = move_selection(&key, state.selected, rows(snap)) {
        state.selected = i;
        return Action::None;
    }
    let Some(id) = selected_issue(snap, state) else {
        return Action::None;
    };
    match key.code {
        KeyCode::Char('m') => Action::Enqueue(id),
        KeyCode::Char('f') => Action::Focus(id),
        KeyCode::Char('a') => answer_blocked(snap, id),
        KeyCode::Char('r') => reply_input(id),
        KeyCode::Char('x') => abort_run(snap, id),
        _ => Action::None,
    }
}
