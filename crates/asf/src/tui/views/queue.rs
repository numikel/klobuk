//! The Queue view: the merge queue, one row per item that is not finished.

use asf_core::*;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState};
use ratatui::Frame;

use crate::tui::app::{Action, View};
use crate::tui::bindings::*;
use crate::tui::snapshot::{abort_run, move_selection, reply_input, trunc, Snapshot};
use crate::tui::theme::Theme;

#[derive(Debug, Clone, Default)]
pub struct State {
    pub selected: usize,
}

const QUEUE: &[View] = &[View::Queue];

pub const BINDINGS: &[Binding] = &[
    b(
        "f",
        "focus",
        HELP_FOCUS,
        Group::NeedsYou,
        QUEUE,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "r",
        "reply",
        HELP_REPLY,
        Group::NeedsYou,
        QUEUE,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "x",
        "abort",
        HELP_ABORT,
        Group::NeedsYou,
        QUEUE,
        NORMAL,
        Slot::Hint,
    ),
];

pub fn rows(snap: &Snapshot) -> usize {
    snap.queue.len()
}

pub fn selected(snap: &Snapshot, state: &State) -> usize {
    state.selected.min(rows(snap).saturating_sub(1))
}

pub fn selected_issue(snap: &Snapshot, state: &State) -> Option<IssueId> {
    snap.queue.get(selected(snap, state)).map(|m| m.issue_id.clone())
}

pub fn render(frame: &mut Frame, area: Rect, snap: &Snapshot, state: &State, theme: &Theme) {
    let block = Block::bordered().title(" merge queue ");
    if snap.queue.is_empty() {
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new(Line::styled(
                " The merge queue is empty. A verified branch waits here after m on the Board.",
                theme.dim(),
            )),
            inner,
        );
        return;
    }
    let rows: Vec<Row> = snap
        .queue
        .iter()
        .map(|m| {
            Row::new(vec![
                Cell::from(format!("#{}", m.id)),
                Cell::from(m.issue_id.to_string()),
                Cell::from(m.state.to_string()),
                Cell::from(m.branch.clone()),
                Cell::from(m.attempts.to_string()),
                Cell::from(trunc(m.last_error.as_deref().unwrap_or(""), 60)),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(5),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(18),
            Constraint::Length(4),
            Constraint::Min(10),
        ],
    )
    .header(Row::new(["#", "issue", "state", "branch", "att", "last error"]).style(theme.bold()))
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
        KeyCode::Char('f') => Action::Focus(id),
        KeyCode::Char('r') => reply_input(id),
        KeyCode::Char('x') => abort_run(snap, id),
        _ => Action::None,
    }
}
