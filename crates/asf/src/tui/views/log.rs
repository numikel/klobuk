//! The Log view: the newest events of the store, newest first.

use ratatui::crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::tui::app::Action;
use crate::tui::bindings::Binding;
use crate::tui::snapshot::{move_selection, trunc, Snapshot};
use crate::tui::theme::Theme;

#[derive(Debug, Clone, Default)]
pub struct State {
    pub selected: usize,
}

/// The Log view has no keys of its own yet: moving is in the global table.
pub const BINDINGS: &[Binding] = &[];

pub fn rows(snap: &Snapshot) -> usize {
    snap.events.len()
}

pub fn selected(snap: &Snapshot, state: &State) -> usize {
    state.selected.min(rows(snap).saturating_sub(1))
}

pub fn render(frame: &mut Frame, area: Rect, snap: &Snapshot, state: &State, theme: &Theme) {
    let width = area.width as usize;
    let block = Block::bordered().title(" log (newest first) ");
    if snap.events.is_empty() {
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new(Line::styled(
                " Nothing has happened yet. Every change in the store is listed here.",
                theme.dim(),
            )),
            inner,
        );
        return;
    }
    let items: Vec<ListItem> = snap
        .events
        .iter()
        .map(|e| {
            let ts = e.ts.get(11..19).unwrap_or(&e.ts);
            let who = e.issue_id.as_ref().map(ToString::to_string).unwrap_or_default();
            let payload = trunc(&e.payload.to_string(), width.saturating_sub(40));
            ListItem::new(format!("{ts} {:<16} {:<8} {payload}", e.kind, who))
        })
        .collect();
    let mut list_state = ListState::default();
    list_state.select(Some(selected(snap, state)));
    let list = List::new(items).block(block).highlight_style(theme.selected());
    frame.render_stateful_widget(list, area, &mut list_state);
}

pub fn handle_key(key: KeyEvent, snap: &Snapshot, state: &mut State) -> Action {
    if let Some(i) = move_selection(&key, state.selected, rows(snap)) {
        state.selected = i;
    }
    Action::None
}
