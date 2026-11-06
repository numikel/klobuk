//! The Config view: every editable key of `.factory/config.toml` with its current value.

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Rect};
use ratatui::widgets::{Block, Cell, Row, Table, TableState};
use ratatui::Frame;

use crate::tui::app::{Action, View};
use crate::tui::bindings::*;
use crate::tui::overlays::InputTarget;
use crate::tui::snapshot::{move_selection, trunc, Snapshot};
use crate::tui::theme::Theme;

#[derive(Debug, Clone, Default)]
pub struct State {
    pub selected: usize,
}

const CONFIG: &[View] = &[View::Config];

pub const BINDINGS: &[Binding] = &[
    b(
        "Enter",
        "edit",
        "type a value",
        Group::Config,
        CONFIG,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "d",
        "unset",
        "back to the default (asks first)",
        Group::Config,
        CONFIG,
        NORMAL,
        Slot::Hint,
    ),
];

pub fn rows(snap: &Snapshot) -> usize {
    snap.config.len()
}

pub fn selected(snap: &Snapshot, state: &State) -> usize {
    state.selected.min(rows(snap).saturating_sub(1))
}

pub fn render(frame: &mut Frame, area: Rect, snap: &Snapshot, state: &State, theme: &Theme) {
    let rows: Vec<Row> = snap
        .config
        .iter()
        .map(|r| {
            Row::new(vec![
                Cell::from(r.key.clone()),
                Cell::from(trunc(&r.value, 20)),
                Cell::from(r.description.clone()),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(24),
            Constraint::Length(20),
            Constraint::Min(10),
        ],
    )
    .header(Row::new(["key", "value", "description"]).style(theme.bold()))
    .row_highlight_style(theme.selected())
    .block(Block::bordered().title(" config · Enter = edit (applies after `asf serve` restart) "));
    let mut table_state = TableState::default();
    table_state.select(Some(selected(snap, state)));
    frame.render_stateful_widget(table, area, &mut table_state);
}

pub fn handle_key(key: KeyEvent, snap: &Snapshot, state: &mut State) -> Action {
    if let Some(i) = move_selection(&key, state.selected, rows(snap)) {
        state.selected = i;
        return Action::None;
    }
    let Some(row) = snap.config.get(selected(snap, state)) else {
        return Action::None;
    };
    match key.code {
        KeyCode::Enter => Action::OpenInput {
            prompt: format!("set {}", row.key),
            buf: row.raw.clone(),
            target: InputTarget::Config(row.key.clone()),
        },
        KeyCode::Char('d') => {
            let key = row.key.clone();
            if !asf_core::config_edit::unsettable_keys().contains(&key.as_str()) {
                Action::Status(format!("{key} cannot be unset"))
            } else if row.raw.is_empty() {
                Action::Status(format!("{key} is already unset"))
            } else {
                Action::Confirm {
                    what: format!("unset {key}?"),
                    then: Box::new(Action::UnsetConfig(key)),
                }
            }
        }
        _ => Action::None,
    }
}
