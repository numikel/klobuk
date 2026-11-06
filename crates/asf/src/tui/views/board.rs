//! The Board view: one column per status, with the detail panel of the selected issue underneath.

use asf_core::*;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use crate::tui::app::{Action, View};
use crate::tui::bindings::*;
use crate::tui::snapshot::{
    abort_run, answer_blocked, issue_mark, move_selection, reply_input, trunc, Snapshot,
};
use crate::tui::theme::Theme;

/// Selection over the issues in board order, and whether the detail panel is open.
#[derive(Debug, Clone)]
pub struct State {
    pub selected: usize,
    pub detail: bool,
}

impl Default for State {
    fn default() -> State {
        State {
            selected: 0,
            detail: true,
        }
    }
}

const BOARD: &[View] = &[View::Board];

pub const BINDINGS: &[Binding] = &[
    b(
        "Enter",
        "detail",
        "show or hide the detail panel",
        Group::Board,
        BOARD,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "Esc",
        "close detail",
        "hide the detail panel",
        Group::Board,
        BOARD,
        NORMAL,
        Slot::None,
    ),
    b(
        "s",
        "start",
        "start a worker on a ready issue",
        Group::Board,
        BOARD,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "a",
        "answer",
        HELP_ANSWER,
        Group::NeedsYou,
        BOARD,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "m",
        "merge",
        "put a verified branch in the merge queue",
        Group::Board,
        BOARD,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "f",
        "focus",
        HELP_FOCUS,
        Group::NeedsYou,
        BOARD,
        NORMAL,
        Slot::None,
    ),
    b(
        "r",
        "reply",
        HELP_REPLY,
        Group::NeedsYou,
        BOARD,
        NORMAL,
        Slot::None,
    ),
    b(
        "x",
        "abort",
        HELP_ABORT,
        Group::NeedsYou,
        BOARD,
        NORMAL,
        Slot::None,
    ),
];

/// Board columns, left to right: every status in `Status::ALL` order except `cancelled`, which is hidden.
pub fn columns() -> Vec<Status> {
    Status::ALL
        .iter()
        .copied()
        .filter(|s| *s != Status::Cancelled)
        .collect()
}

/// Issues in board order: column by column, by sequence number inside a column.
pub fn ordered(snap: &Snapshot) -> Vec<&Issue> {
    let mut out = Vec::new();
    for st in columns() {
        let mut col: Vec<&Issue> = snap.issues.iter().filter(|i| i.status == st).collect();
        col.sort_by(|a, b| a.id.cmp(&b.id));
        out.extend(col);
    }
    out
}

/// The selection, kept inside the rows.
pub fn selected(snap: &Snapshot, state: &State) -> usize {
    state.selected.min(ordered(snap).len().saturating_sub(1))
}

pub fn selected_issue(snap: &Snapshot, state: &State) -> Option<IssueId> {
    ordered(snap).get(selected(snap, state)).map(|i| i.id.clone())
}

pub fn rows(snap: &Snapshot) -> usize {
    snap.issues.len()
}

/// Moves the selection to `id` (it stays where it was when the Board does not list it).
pub fn select(state: &mut State, snap: &Snapshot, id: &IssueId) {
    if let Some(at) = ordered(snap).iter().position(|i| &i.id == id) {
        state.selected = at;
    }
}

pub fn render(frame: &mut Frame, area: Rect, snap: &Snapshot, state: &State, theme: &Theme) {
    let show_detail = state.detail && area.height >= 10;
    let (main, panel) = if show_detail {
        let [main, panel] = Layout::vertical([Constraint::Min(5), Constraint::Length(7)]).areas(area);
        (main, Some(panel))
    } else {
        (area, None)
    };
    board(frame, main, snap, state, theme);
    if let Some(panel) = panel {
        detail(frame, panel, snap, state, theme);
    }
}

fn board(frame: &mut Frame, area: Rect, snap: &Snapshot, state: &State, theme: &Theme) {
    let columns = columns();
    let cols =
        Layout::horizontal(vec![Constraint::Ratio(1, columns.len() as u32); columns.len()]).split(area);
    let ordered = ordered(snap);
    let selected_id = ordered.get(selected(snap, state)).map(|i| i.id.clone());
    let width = cols
        .first()
        .map(|c| c.width as usize)
        .unwrap_or(10)
        .saturating_sub(4);
    for (st, col) in columns.iter().zip(cols.iter()) {
        let items: Vec<&Issue> = ordered.iter().copied().filter(|i| i.status == *st).collect();
        let n = items.len();
        let list_items: Vec<ListItem> = items
            .iter()
            .map(|i| {
                let mark = issue_mark(snap, i);
                // A worker that sits on a prompt keeps its `?`: it is the one the `a` key answers.
                let blocked = snap.run_for(&i.id).is_some_and(|r| r.state == RunState::Blocked);
                let marker = Span::styled(
                    format!("{} {}", mark.glyph(), if blocked { "? " } else { "" }),
                    theme.mark(mark),
                );
                ListItem::new(Text::from(vec![
                    Line::from(vec![marker, Span::styled(i.id.to_string(), theme.bold())]),
                    Line::from(format!("  {}", trunc(&i.title, width))),
                ]))
            })
            .collect();
        let mut list_state = ListState::default();
        if let Some(sid) = &selected_id {
            list_state.select(items.iter().position(|i| &i.id == sid));
        }
        let list = List::new(list_items)
            .block(Block::bordered().title(format!(" {st} {n} ")))
            .highlight_style(theme.selected());
        frame.render_stateful_widget(list, *col, &mut list_state);
    }
}

fn detail(frame: &mut Frame, area: Rect, snap: &Snapshot, state: &State, theme: &Theme) {
    let block = Block::bordered().title(" detail ");
    let Some(id) = selected_issue(snap, state) else {
        frame.render_widget(Paragraph::new("").block(block), area);
        return;
    };
    let Some(i) = snap.issue(&id) else { return };
    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            Span::styled(format!("{} ", i.id), theme.bold()),
            Span::raw(i.title.clone()),
        ]),
        Line::from(format!(
            "status {} · type {} · P{} · estimate {} · scope: {}",
            i.status,
            i.kind,
            i.priority,
            i.estimate.as_deref().unwrap_or("-"),
            if i.file_scope.is_empty() {
                "(none)".to_string()
            } else {
                i.file_scope.join(" ")
            }
        )),
    ];
    for a in &i.acceptance {
        lines.push(Line::from(format!("  - {}", a.text)));
    }
    if let Some(c) = snap.claims.iter().find(|c| c.issue_id == id) {
        lines.push(Line::styled(
            format!("claimed by {} since {}", c.worker, c.claimed_at),
            theme.dim(),
        ));
    }
    if let Some(r) = snap.run_for(&id) {
        let agent = snap
            .agents
            .get(&id)
            .map(|a| a.state.to_string())
            .unwrap_or_else(|| "?".into());
        lines.push(Line::from(format!(
            "run {} · agent {agent} · ${:.2}/{} · tool_calls {} · pane {}",
            r.state,
            r.cost_usd,
            r.budget_usd
                .map(|b| format!("${b:.2}"))
                .unwrap_or_else(|| "-".into()),
            r.tool_calls,
            r.pane_id.as_deref().unwrap_or("-")
        )));
    }
    if let Some(c) = snap.inbox.iter().rev().find(|c| c.issue_id == id) {
        lines.push(Line::styled(
            format!("[{}] {}: {}", c.author, c.kind, trunc(&c.body, 100)),
            theme.warn(),
        ));
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(block),
        area,
    );
}

/// Keys of the Board. Moving the selection asks for the comments and dependencies of the issue it lands
/// on (`Action::LoadDetail`), which the snapshot does not carry until then.
pub fn handle_key(key: KeyEvent, snap: &Snapshot, state: &mut State) -> Action {
    if let Some(i) = move_selection(&key, state.selected, rows(snap)) {
        let before = selected_issue(snap, state);
        state.selected = i;
        let after = selected_issue(snap, state);
        return match after {
            Some(id) if before != Some(id.clone()) => Action::LoadDetail(id),
            _ => Action::None,
        };
    }
    let selected = selected_issue(snap, state);
    match (key.code, selected) {
        (KeyCode::Esc, _) => {
            state.detail = false;
            Action::None
        }
        (KeyCode::Enter, id) => {
            state.detail = !state.detail;
            match id {
                Some(id) if state.detail => Action::LoadDetail(id),
                _ => Action::None,
            }
        }
        (KeyCode::Char('s'), Some(id)) => match snap.issue(&id).map(|i| i.status) {
            Some(Status::Ready) => Action::Start(id),
            Some(st) => Action::Status(format!("{id} is {st}; only ready issues can be started")),
            None => Action::None,
        },
        (KeyCode::Char('m'), Some(id)) => Action::Enqueue(id),
        (KeyCode::Char('f'), Some(id)) => Action::Focus(id),
        (KeyCode::Char('a'), Some(id)) => answer_blocked(snap, id),
        (KeyCode::Char('r'), Some(id)) => reply_input(id),
        (KeyCode::Char('x'), Some(id)) => abort_run(snap, id),
        _ => Action::None,
    }
}
