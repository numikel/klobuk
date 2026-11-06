//! The Inbox view: the unread comments addressed to the human.

use asf_core::*;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::tui::app::{Action, View};
use crate::tui::bindings::*;
use crate::tui::snapshot::{answer_blocked, move_selection, reply_input, trunc, Snapshot};
use crate::tui::theme::Theme;

#[derive(Debug, Clone, Default)]
pub struct State {
    pub selected: usize,
}

const INBOX: &[View] = &[View::Inbox];

pub const BINDINGS: &[Binding] = &[
    b(
        "Enter",
        "open",
        "open the selected comment in full",
        Group::Inbox,
        INBOX,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "m",
        "mark read",
        "mark this issue's messages as read",
        Group::Inbox,
        INBOX,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "M",
        "mark all",
        "mark all as read (asks first)",
        Group::Inbox,
        INBOX,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "a",
        "answer",
        HELP_ANSWER,
        Group::NeedsYou,
        INBOX,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "r",
        "reply",
        HELP_REPLY,
        Group::NeedsYou,
        INBOX,
        NORMAL,
        Slot::Hint,
    ),
];

pub fn rows(snap: &Snapshot) -> usize {
    snap.inbox.len()
}

pub fn selected(snap: &Snapshot, state: &State) -> usize {
    state.selected.min(rows(snap).saturating_sub(1))
}

pub fn selected_issue(snap: &Snapshot, state: &State) -> Option<IssueId> {
    snap.inbox.get(selected(snap, state)).map(|c| c.issue_id.clone())
}

pub fn render(frame: &mut Frame, area: Rect, snap: &Snapshot, state: &State, theme: &Theme) {
    let width = area.width as usize;
    let block = Block::bordered().title(format!(
        " inbox (human) {} unread · Enter open · m mark read · M mark all ",
        snap.inbox.len()
    ));
    if snap.inbox.is_empty() {
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new(Line::styled(
                " No unread messages. Workers' questions and blockers for you appear here.",
                theme.dim(),
            )),
            inner,
        );
        return;
    }
    let items: Vec<ListItem> = snap
        .inbox
        .iter()
        .map(|c| {
            let head = format!(
                "#{} {} [{}] {} -> {}",
                c.id, c.issue_id, c.author, c.kind, c.to_lane
            );
            let body: Vec<Line> = std::iter::once(Line::styled(head, theme.bold()))
                .chain(
                    c.body
                        .lines()
                        .take(6)
                        .map(|l| Line::from(format!("  {}", trunc(l, width.saturating_sub(4))))),
                )
                .collect();
            ListItem::new(body)
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
        return Action::None;
    }
    match key.code {
        KeyCode::Char('M') => {
            let n = snap.inbox.len();
            if n == 0 {
                return Action::None;
            }
            Action::Confirm {
                what: format!("mark all {n} read?"),
                then: Box::new(Action::MarkAllRead),
            }
        }
        _ => {
            let Some(id) = selected_issue(snap, state) else {
                return Action::None;
            };
            match key.code {
                KeyCode::Enter => snap
                    .inbox
                    .get(selected(snap, state))
                    .cloned()
                    .map_or(Action::None, Action::OpenComment),
                KeyCode::Char('m') => Action::MarkRead(id),
                KeyCode::Char('a') => answer_blocked(snap, id),
                KeyCode::Char('r') => reply_input(id),
                _ => Action::None,
            }
        }
    }
}
