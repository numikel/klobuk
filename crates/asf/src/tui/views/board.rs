//! The Board view: issues grouped under their epic, one header per epic with a progress bar, and (from 120
//! columns) a bordered detail panel of the selected row next to the list. `b` switches to a kanban, one
//! column per status; both layouts share the selection, the filter, the detail and the action keys.
//!
//! The row model ([`Listing`]) is a pure function of the snapshot and the [`State`] (folds and filter),
//! so the key handling walks exactly the rows the renderer draws; the kanban ([`Kanban`]) is built the
//! same way. An epic is never an issue row or a card: it is a group header only, and it is not counted in
//! the status counts.

use std::collections::BTreeMap;

use asf_core::cost::{rollup, Costs};
use asf_core::*;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use crate::tui::app::{Action, View};
use crate::tui::bindings::*;
use crate::tui::snapshot::{
    abort_run, age, answer_blocked, first_line, fit, issue_mark, money, move_selection, offset_of,
    parse_time, reply_input, text_width, trunc, wrap_text, Snapshot,
};
use crate::tui::theme::{Mark, Theme};

/// Width from which the detail panel sits next to the list.
const PANEL_MIN_WIDTH: u16 = 120;
/// Cells of a progress bar.
const BAR: usize = 10;
/// Width of the state label column (`merge failed` is the longest label).
const LABEL_W: usize = 12;
/// Columns of an issue row before the title: margin 3, selector, blank, symbol, blank, then the id, two
/// blanks, the label and a blank (the id's width is added).
const ROW_FIXED: usize = 3 + 1 + 1 + 1 + 1 + 2 + LABEL_W + 1;
/// Width of the labels (`Depends on`, `Scope`) in the detail.
const FIELD_W: usize = 12;
/// Share of the width the kanban gets next to the detail panel: seven columns need more than the list.
const KANBAN_SHARE: u16 = 70;
/// The kanban columns, left to right (`cancelled` is not shown anywhere on the Board).
const COLUMNS: [Status; 7] = [
    Status::Backlog,
    Status::Ready,
    Status::InProgress,
    Status::Blocked,
    Status::InReview,
    Status::Validating,
    Status::Done,
];
/// Index of `done` in [`COLUMNS`]: its newest [`DONE_CAP`] cards are shown.
const DONE_COL: usize = 6;
/// Done cards shown (newest first) unless a filter is typed; the rest is `+K more`.
const DONE_CAP: usize = 5;
/// Lines of a card: the id, two title lines and a blank line between cards.
const CARD_H: usize = 4;

/// How the Board lays the issues out. It lives in the TUI state for the session, not in the config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BoardLayout {
    /// Issues under their epic, with a progress bar per epic (the default).
    #[default]
    Epics,
    /// One column per status.
    Kanban,
}

/// Selection, folds, filter and whether the full-screen detail is open.
#[derive(Debug, Clone)]
pub struct State {
    /// Index into the visible rows (headers included); clamped when read.
    pub selected: usize,
    /// The layout `b` switched to.
    pub layout: BoardLayout,
    /// Kanban cursor: the column (index into [`COLUMNS`]) and the card in it; both clamped when read.
    col: usize,
    card: usize,
    /// The done column shows all its cards: a selection reached one beyond the newest [`DONE_CAP`].
    done_open: bool,
    /// The detail of the selected issue is wanted (the event loop loads it: `App::detail_wanted`).
    pub detail: bool,
    /// The detail fills the whole area (Enter); Esc comes back.
    full: bool,
    filter: String,
    /// Keys are text for the filter (`/` starts it, Enter keeps it, Esc clears it).
    typing: bool,
    /// Group (`None` = No epic) to whether the user folded it; absent = the default.
    folds: BTreeMap<Option<IssueId>, bool>,
}

impl Default for State {
    fn default() -> State {
        State {
            selected: 0,
            layout: BoardLayout::default(),
            col: 0,
            card: 0,
            done_open: false,
            detail: true,
            full: false,
            filter: String::new(),
            typing: false,
            folds: BTreeMap::new(),
        }
    }
}

impl State {
    /// Whether keys are text for the filter: `App::handle_key` then hands every key to the Board.
    pub fn typing(&self) -> bool {
        self.typing
    }

    #[cfg(test)]
    pub fn filter(&self) -> &str {
        &self.filter
    }
}

const BOARD: &[View] = &[View::Board];

pub const BINDINGS: &[Binding] = &[
    b(
        "j / k",
        "move",
        HELP_MOVE,
        Group::Everywhere,
        BOARD,
        NORMAL,
        Slot::None,
    ),
    b(
        "h / l",
        "fold or column",
        "fold / unfold the epic the selection is in\n(kanban: move to the next column)",
        Group::Board,
        BOARD,
        NORMAL,
        Slot::Hint,
    ),
    // Not a footer hint: the first line of the Board already says `/ filter`, and the footer has room
    // for five contextual hints only.
    b(
        "/",
        "filter",
        "filter by id, title or status\n(Enter keeps it, Esc clears it)",
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
        "Enter",
        "full details",
        "full details of the selected issue\n(Esc comes back)",
        Group::Board,
        BOARD,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "b",
        "layout",
        "switch between the epic list\nand a kanban with a column per status",
        Group::Board,
        BOARD,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "Esc",
        "back",
        "close the full details, or clear the filter",
        Group::Board,
        BOARD,
        NORMAL,
        Slot::None,
    ),
    b(
        "m",
        "merge",
        "put a verified branch in the merge queue",
        Group::Board,
        BOARD,
        NORMAL,
        Slot::None,
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

// ---------------------------------------------------------------------------------------------
// The row model
// ---------------------------------------------------------------------------------------------

/// One epic (or the issues without one) with the issues listed under it.
struct EpicGroup<'a> {
    /// `None` = the last group, `No epic`.
    epic: Option<&'a Issue>,
    /// Tasks and reviews, in row order.
    members: Vec<&'a Issue>,
    /// Finished and all tasks (reviews are internal and do not count for the bar).
    done: usize,
    total: usize,
    /// Nothing under it is open: it is folded by default and drawn dim.
    complete: bool,
}

impl EpicGroup<'_> {
    fn key(&self) -> Option<IssueId> {
        self.epic.map(|e| e.id.clone())
    }
}

/// The nearest epic above `issue` (a review sits under the task it reviews, a task under its epic).
fn epic_of<'a>(snap: &'a Snapshot, issue: &Issue) -> Option<&'a Issue> {
    let mut parent = issue.parent_id.as_ref();
    for _ in 0..8 {
        let p = snap.issue(parent?)?;
        if p.kind == IssueType::Epic && p.status != Status::Cancelled {
            return Some(p);
        }
        parent = p.parent_id.as_ref();
    }
    None
}

/// Backlog and ready rows come after all others; inside each part by id.
fn row_order(i: &Issue) -> (bool, IssueId) {
    (matches!(i.status, Status::Backlog | Status::Ready), i.id.clone())
}

/// Every group in display order: epics with open work (newest id first), finished epics (newest first),
/// then `No epic`. Cancelled issues and epics are not listed.
fn groups(snap: &Snapshot) -> Vec<EpicGroup<'_>> {
    let mut by_epic: BTreeMap<IssueId, Vec<&Issue>> = BTreeMap::new();
    let mut loose: Vec<&Issue> = Vec::new();
    for i in snap
        .issues
        .iter()
        .filter(|i| i.kind != IssueType::Epic && i.status != Status::Cancelled)
    {
        match epic_of(snap, i) {
            Some(e) => by_epic.entry(e.id.clone()).or_default().push(i),
            None => loose.push(i),
        }
    }
    let mut epics: Vec<&Issue> = snap
        .issues
        .iter()
        .filter(|i| i.kind == IssueType::Epic && i.status != Status::Cancelled)
        .collect();
    epics.sort_by(|a, b| b.id.cmp(&a.id));
    let (mut open, mut finished) = (Vec::new(), Vec::new());
    for e in epics {
        let mut members = by_epic.remove(&e.id).unwrap_or_default();
        members.sort_by_key(|i| row_order(i));
        let tasks = members.iter().filter(|i| i.kind == IssueType::Task);
        let total = tasks.clone().count();
        let done = tasks.filter(|i| i.status == Status::Done).count();
        let all_done = members.iter().all(|i| i.status == Status::Done);
        let complete = all_done && (!members.is_empty() || e.status == Status::Done);
        let g = EpicGroup {
            epic: Some(e),
            members,
            done,
            total,
            complete,
        };
        if complete {
            finished.push(g);
        } else {
            open.push(g);
        }
    }
    loose.sort_by_key(|i| row_order(i));
    let mut out = open;
    out.extend(finished);
    if !loose.is_empty() {
        out.push(EpicGroup {
            epic: None,
            members: loose,
            done: 0,
            total: 0,
            complete: false,
        });
    }
    out
}

#[derive(Clone, Copy)]
enum Row<'a> {
    /// The header of the group at this index of [`Listing::groups`].
    Header(usize),
    Issue(&'a Issue),
}

/// The visible rows: headers and the issues of the groups that are open and match the filter.
struct Listing<'a> {
    /// The groups that are visible (all of them without a filter).
    groups: Vec<EpicGroup<'a>>,
    folded: Vec<bool>,
    rows: Vec<Row<'a>>,
}

fn text_hit(issue: &Issue, needle: &str) -> bool {
    issue.id.as_str().contains(needle) || issue.title.to_lowercase().contains(needle)
}

fn issue_hit(issue: &Issue, needle: &str) -> bool {
    text_hit(issue, needle) || issue.status.to_string().contains(needle)
}

impl<'a> Listing<'a> {
    fn build(snap: &'a Snapshot, state: &State) -> Listing<'a> {
        let needle = state.filter.trim().to_lowercase();
        let filtering = !needle.is_empty();
        let mut out = Listing {
            groups: Vec::new(),
            folded: Vec::new(),
            rows: Vec::new(),
        };
        for g in groups(snap) {
            let shown: Vec<&Issue> = if filtering {
                g.members
                    .iter()
                    .copied()
                    .filter(|i| issue_hit(i, &needle))
                    .collect()
            } else {
                g.members.clone()
            };
            let header_hit = g.epic.is_some_and(|e| text_hit(e, &needle));
            if filtering && shown.is_empty() && !header_hit {
                continue;
            }
            // a filter shows its matches wherever they are, folded or not
            let folded = !filtering && state.folds.get(&g.key()).copied().unwrap_or(g.complete);
            out.rows.push(Row::Header(out.groups.len()));
            if !folded {
                out.rows.extend(shown.into_iter().map(Row::Issue));
            }
            out.folded.push(folded);
            out.groups.push(g);
        }
        out
    }

    fn id_at(&self, at: usize) -> Option<IssueId> {
        match self.rows.get(at)? {
            Row::Header(g) => self.groups[*g].key(),
            Row::Issue(i) => Some(i.id.clone()),
        }
    }

    fn position(&self, id: &IssueId) -> Option<usize> {
        (0..self.rows.len()).find(|at| self.id_at(*at).as_ref() == Some(id))
    }

    /// The group the row at `at` belongs to (a header belongs to its own group).
    fn group_at(&self, at: usize) -> Option<usize> {
        let end = (at + 1).min(self.rows.len());
        self.rows[..end].iter().rev().find_map(|r| match r {
            Row::Header(g) => Some(*g),
            Row::Issue(_) => None,
        })
    }
}

/// The kanban: the cards of every [`COLUMNS`] status that match the filter. Epics and cancelled issues are
/// not cards. A column runs oldest id first, `done` newest first and capped at [`DONE_CAP`].
struct Kanban<'a> {
    columns: Vec<Vec<&'a Issue>>,
    /// Done cards left out by the cap.
    capped: usize,
}

impl<'a> Kanban<'a> {
    fn build(snap: &'a Snapshot, state: &State) -> Kanban<'a> {
        let needle = state.filter.trim().to_lowercase();
        let filtering = !needle.is_empty();
        let mut columns: Vec<Vec<&Issue>> = vec![Vec::new(); COLUMNS.len()];
        for i in snap
            .issues
            .iter()
            .filter(|i| i.kind != IssueType::Epic && i.status != Status::Cancelled)
            .filter(|i| !filtering || issue_hit(i, &needle))
        {
            if let Some(c) = COLUMNS.iter().position(|s| *s == i.status) {
                columns[c].push(i);
            }
        }
        for cards in &mut columns {
            cards.sort_by(|a, b| a.id.cmp(&b.id));
        }
        columns[DONE_COL].reverse();
        let mut capped = 0;
        // a filter shows its matches wherever they are, like it does in the epic list
        if !filtering && !state.done_open && columns[DONE_COL].len() > DONE_CAP {
            capped = columns[DONE_COL].len() - DONE_CAP;
            columns[DONE_COL].truncate(DONE_CAP);
        }
        Kanban { columns, capped }
    }

    /// The `(column, card)` the cursor rests on: an empty column hands it to the nearest one with cards
    /// (to the right first); `None` when there is no card at all.
    fn cursor(&self, state: &State) -> Option<(usize, usize)> {
        let col = state.col.min(COLUMNS.len() - 1);
        let col = (col..COLUMNS.len())
            .chain((0..col).rev())
            .find(|c| !self.columns[*c].is_empty())?;
        Some((col, state.card.min(self.columns[col].len() - 1)))
    }

    fn id_at(&self, (col, card): (usize, usize)) -> IssueId {
        self.columns[col][card].id.clone()
    }

    fn position(&self, id: &IssueId) -> Option<(usize, usize)> {
        self.columns
            .iter()
            .enumerate()
            .find_map(|(c, cards)| cards.iter().position(|i| &i.id == id).map(|card| (c, card)))
    }

    /// Cards on the board, the capped ones not counted.
    fn total(&self) -> usize {
        self.columns.iter().map(Vec::len).sum()
    }
}

/// Issues in board order: group by group, as listed (folds and the filter do not apply). Epics are
/// headers, not issues, and cancelled issues are not listed.
#[cfg(test)]
pub fn ordered(snap: &Snapshot) -> Vec<&Issue> {
    groups(snap).into_iter().flat_map(|g| g.members).collect()
}

/// The rows of the default layout (every group in its default fold state, no filter), headers included.
#[cfg(test)]
pub fn rows(snap: &Snapshot) -> usize {
    Listing::build(snap, &State::default()).rows.len()
}

/// The selection, kept inside the visible rows.
#[cfg(test)]
pub fn selected(snap: &Snapshot, state: &State) -> usize {
    state
        .selected
        .min(Listing::build(snap, state).rows.len().saturating_sub(1))
}

/// The issue the selection points at; in the epic list a header stands for its epic and `No epic` for
/// nothing, in the kanban only a card is selected.
pub fn selected_issue(snap: &Snapshot, state: &State) -> Option<IssueId> {
    match state.layout {
        BoardLayout::Epics => {
            let listing = Listing::build(snap, state);
            listing.id_at(state.selected.min(listing.rows.len().saturating_sub(1)))
        }
        BoardLayout::Kanban => {
            let kanban = Kanban::build(snap, state);
            kanban.cursor(state).map(|at| kanban.id_at(at))
        }
    }
}

/// Points the kanban cursor at `id`. The done column opens up when the card is older than its cap; the
/// cursor stays where it was when the kanban has no card `id` (an epic, a cancelled issue).
fn point_kanban(state: &mut State, snap: &Snapshot, id: &IssueId) {
    if Kanban::build(snap, state).position(id).is_none() {
        state.done_open = true;
        if Kanban::build(snap, state).position(id).is_none() {
            state.done_open = false;
            return;
        }
    }
    if let Some((col, card)) = Kanban::build(snap, state).position(id) {
        state.col = col;
        state.card = card;
    }
}

/// Moves the selection to `id` in both layouts, opening its group (and dropping a filter that hides it).
/// It stays where it was when the Board does not list `id`.
pub fn select(state: &mut State, snap: &Snapshot, id: &IssueId) {
    state.full = false;
    for g in groups(snap) {
        if g.members.iter().any(|i| &i.id == id) {
            state.folds.insert(g.key(), false);
        }
    }
    let mut at = Listing::build(snap, state).position(id);
    if at.is_none() && !state.filter.is_empty() {
        state.filter.clear();
        state.typing = false;
        at = Listing::build(snap, state).position(id);
    }
    if let Some(at) = at {
        state.selected = at;
    }
    point_kanban(state, snap, id);
}

/// `asking` for a blocked run, `merge failed` for a failed or blocked merge item, else the status.
fn state_label(snap: &Snapshot, issue: &Issue) -> String {
    if snap
        .run_for(&issue.id)
        .is_some_and(|r| r.state == RunState::Blocked)
    {
        "asking".into()
    } else if snap
        .queue
        .iter()
        .any(|m| m.issue_id == issue.id && matches!(m.state, MergeState::Failed | MergeState::Blocked))
    {
        "merge failed".into()
    } else {
        issue.status.to_string()
    }
}

/// The first dependency of `issue` that is not done.
fn unmet_dep(snap: &Snapshot, issue: &Issue) -> Option<IssueId> {
    snap.deps
        .iter()
        .filter(|d| d.issue_id == issue.id)
        .find(|d| snap.issue(&d.depends_on).is_none_or(|x| x.status != Status::Done))
        .map(|d| d.depends_on.clone())
}

// ---------------------------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------------------------

/// Keys of the Board. Moving the selection to another issue asks for its comments
/// (`Action::LoadDetail`), which the snapshot does not carry until then.
pub fn handle_key(key: KeyEvent, snap: &Snapshot, state: &mut State) -> Action {
    let before = selected_issue(snap, state);
    let action = if state.typing {
        type_key(key, snap, state);
        Action::None
    } else {
        list_key(key, snap, state)
    };
    if action != Action::None {
        return action;
    }
    match selected_issue(snap, state) {
        Some(id) if before.as_ref() != Some(&id) => Action::LoadDetail(id),
        _ => Action::None,
    }
}

fn list_key(key: KeyEvent, snap: &Snapshot, state: &mut State) -> Action {
    let moved = match state.layout {
        BoardLayout::Epics => {
            let listing_rows = Listing::build(snap, state).rows.len();
            move_selection(&key, state.selected, listing_rows)
                .map(|i| state.selected = i)
                .is_some()
        }
        BoardLayout::Kanban => kanban_move(&key, snap, state),
    };
    if moved {
        return Action::None;
    }
    let sel = selected_issue(snap, state);
    let epic = sel
        .as_ref()
        .and_then(|id| snap.issue(id))
        .is_some_and(|i| i.kind == IssueType::Epic);
    match (key.code, sel) {
        (KeyCode::Esc, _) => {
            if state.full {
                state.full = false;
            } else if !state.filter.is_empty() {
                clear_filter(snap, state);
            }
            Action::None
        }
        (KeyCode::Enter, Some(id)) => {
            state.full = true;
            if snap.detail_of(&id).is_none() {
                Action::LoadDetail(id)
            } else {
                Action::None
            }
        }
        (KeyCode::Char('/'), _) => {
            state.typing = true;
            state.full = false;
            Action::None
        }
        // in the kanban `kanban_move` took h and l above
        (KeyCode::Char('h'), _) => fold(snap, state, true),
        (KeyCode::Char('l'), _) => fold(snap, state, false),
        (KeyCode::Char('b'), _) => {
            toggle_layout(snap, state);
            Action::None
        }
        (KeyCode::Char('s'), Some(id)) if epic => {
            Action::Status(format!("{id} is an epic; only ready tasks can be started"))
        }
        (KeyCode::Char('s'), Some(id)) => match snap.issue(&id).map(|i| i.status) {
            Some(Status::Ready) => Action::Start(id),
            Some(st) => Action::Status(format!("{id} is {st}; only ready issues can be started")),
            None => Action::None,
        },
        (KeyCode::Char('m'), Some(id)) if epic => {
            Action::Status(format!("{id} is an epic; epics are not merged"))
        }
        (KeyCode::Char('m'), Some(id)) => Action::Enqueue(id),
        (KeyCode::Char('f'), Some(id)) if epic => {
            Action::Status(format!("{id} is an epic; it has no worker pane"))
        }
        (KeyCode::Char('f'), Some(id)) => Action::Focus(id),
        (KeyCode::Char('a'), Some(id)) => answer_blocked(snap, id),
        (KeyCode::Char('r'), Some(id)) if epic => {
            Action::Status(format!("{id} is an epic; there is no worker to reply to"))
        }
        (KeyCode::Char('r'), Some(id)) => reply_input(id),
        (KeyCode::Char('x'), Some(id)) => abort_run(snap, id),
        _ => Action::None,
    }
}

/// The navigation keys of the kanban: `j`/`k`/`g`/`G`/arrows inside the column, `h`/`l`/Left/Right to the
/// next column that has cards (so the cursor always rests on a card; the row is kept when it fits).
/// Returns whether `key` navigates.
fn kanban_move(key: &KeyEvent, snap: &Snapshot, state: &mut State) -> bool {
    let kanban = Kanban::build(snap, state);
    let step: isize = match key.code {
        KeyCode::Char('h') | KeyCode::Left => -1,
        KeyCode::Char('l') | KeyCode::Right => 1,
        _ => 0,
    };
    let Some((col, card)) = kanban.cursor(state) else {
        return step != 0 || move_selection(key, 0, 0).is_some();
    };
    if step == 0 {
        return match move_selection(key, card, kanban.columns[col].len()) {
            Some(at) => {
                state.col = col;
                state.card = at;
                true
            }
            None => false,
        };
    }
    let mut next = col as isize + step;
    while let Some(cards) = usize::try_from(next).ok().and_then(|c| kanban.columns.get(c)) {
        if !cards.is_empty() {
            state.col = next as usize;
            state.card = card.min(cards.len() - 1);
            break;
        }
        next += step;
    }
    true
}

/// `b`: the other layout, with the selected issue still selected. In the epic list an epic header
/// stands for its first card.
fn toggle_layout(snap: &Snapshot, state: &mut State) {
    let keep = selected_issue(snap, state);
    state.done_open = false;
    match state.layout {
        BoardLayout::Epics => {
            state.layout = BoardLayout::Kanban;
            let kanban = Kanban::build(snap, state);
            let card = keep.and_then(|id| match snap.issue(&id) {
                Some(i) if i.kind == IssueType::Epic => groups(snap)
                    .into_iter()
                    .find(|g| g.key().as_ref() == Some(&id))
                    .and_then(|g| {
                        g.members
                            .iter()
                            .find(|m| kanban.position(&m.id).is_some())
                            .map(|m| m.id.clone())
                    }),
                _ => Some(id),
            });
            if let Some(id) = card {
                point_kanban(state, snap, &id);
            }
        }
        BoardLayout::Kanban => {
            state.layout = BoardLayout::Epics;
            if let Some(id) = keep {
                let full = state.full;
                select(state, snap, &id);
                state.full = full;
            }
        }
    }
}

/// `h` (`fold == true`) and `l`: the group of the selection closes or opens. A closed group takes the
/// selection to its header, so it never points at a row that is gone. Folds do not apply under a filter.
fn fold(snap: &Snapshot, state: &mut State, fold: bool) -> Action {
    if !state.filter.trim().is_empty() {
        return Action::None;
    }
    let listing = Listing::build(snap, state);
    let at = state.selected.min(listing.rows.len().saturating_sub(1));
    let Some(g) = listing.group_at(at) else {
        return Action::None;
    };
    let header = listing
        .rows
        .iter()
        .position(|r| matches!(r, Row::Header(x) if *x == g));
    state.folds.insert(listing.groups[g].key(), fold);
    if fold {
        if let Some(h) = header {
            state.selected = h;
        }
    }
    Action::None
}

/// Drops the filter and leaves the typing; the selected issue stays selected.
fn clear_filter(snap: &Snapshot, state: &mut State) {
    let keep = selected_issue(snap, state);
    state.filter.clear();
    state.typing = false;
    match keep {
        Some(id) => select(state, snap, &id),
        None => {
            state.selected = 0;
            state.col = 0;
            state.card = 0;
        }
    }
}

/// While typing: text goes to the filter (also `q`, `t`, digits ...), Enter keeps it, Esc clears it.
fn type_key(key: KeyEvent, snap: &Snapshot, state: &mut State) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Esc => clear_filter(snap, state),
        KeyCode::Enter => state.typing = false,
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
    if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
        return;
    }
    retarget(snap, state);
}

/// After the filter text changed: the first match is selected (the top again once the filter is empty).
fn retarget(snap: &Snapshot, state: &mut State) {
    if state.layout == BoardLayout::Kanban {
        let kanban = Kanban::build(snap, state);
        state.col = kanban.columns.iter().position(|c| !c.is_empty()).unwrap_or(0);
        state.card = 0;
        return;
    }
    let listing = Listing::build(snap, state);
    state.selected = if state.filter.trim().is_empty() {
        0
    } else {
        listing
            .rows
            .iter()
            .position(|r| matches!(r, Row::Issue(_)))
            .unwrap_or(0)
    };
}

// ---------------------------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------------------------

pub fn render(frame: &mut Frame, area: Rect, snap: &Snapshot, state: &State, theme: &Theme) {
    let [top, rest] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    frame.render_widget(Paragraph::new(counts_line(snap, state, theme)), top);
    match state.layout {
        BoardLayout::Epics => render_epics(frame, rest, snap, state, theme),
        BoardLayout::Kanban => render_kanban(frame, rest, snap, state, theme),
    }
}

/// The epic list, with the detail panel next to it from [`PANEL_MIN_WIDTH`] columns.
fn render_epics(frame: &mut Frame, rest: Rect, snap: &Snapshot, state: &State, theme: &Theme) {
    let listing = Listing::build(snap, state);
    let at = state.selected.min(listing.rows.len().saturating_sub(1));
    let id = listing.id_at(at);
    if state.full && id.is_some() {
        render_detail(frame, rest, snap, id.as_ref(), true, theme);
        return;
    }
    if rest.width >= PANEL_MIN_WIDTH && !listing.rows.is_empty() {
        let [list, panel] = Layout::horizontal([Constraint::Percentage(60), Constraint::Fill(1)]).areas(rest);
        render_list(frame, list, snap, &listing, at, theme);
        render_detail(frame, panel, snap, id.as_ref(), false, theme);
    } else {
        render_list(frame, rest, snap, &listing, at, theme);
    }
}

/// The kanban, with the same detail panel (and the same full-screen detail) as the epic list.
fn render_kanban(frame: &mut Frame, rest: Rect, snap: &Snapshot, state: &State, theme: &Theme) {
    let kanban = Kanban::build(snap, state);
    let cursor = kanban.cursor(state);
    let id = cursor.map(|at| kanban.id_at(at));
    if state.full && id.is_some() {
        render_detail(frame, rest, snap, id.as_ref(), true, theme);
        return;
    }
    if rest.width >= PANEL_MIN_WIDTH && id.is_some() {
        let [board, panel] =
            Layout::horizontal([Constraint::Percentage(KANBAN_SHARE), Constraint::Fill(1)]).areas(rest);
        render_columns(frame, board, snap, state, &kanban, theme);
        render_detail(frame, panel, snap, id.as_ref(), false, theme);
    } else {
        render_columns(frame, rest, snap, state, &kanban, theme);
    }
}

/// `area` cut into one rectangle per column, a blank column between them; the remainder goes to the
/// leftmost columns.
fn column_rects(area: Rect) -> Vec<Rect> {
    let n = COLUMNS.len() as u16;
    let usable = area.width.saturating_sub(n - 1);
    let (each, extra) = (usable / n, usable % n);
    let mut x = area.x;
    (0..n)
        .map(|c| {
            let width = each + u16::from(c < extra);
            let rect = Rect::new(x, area.y, width, area.height);
            x += width + 1;
            rect
        })
        .collect()
}

/// `in-progress 3` when it fits, else the status alone (cut with `…` when even that does not fit).
fn column_title(status: Status, count: usize, width: usize) -> String {
    let full = format!("{status} {count}");
    if text_width(&full) <= width {
        full
    } else {
        trunc(&status.to_string(), width)
    }
}

fn render_columns(
    frame: &mut Frame,
    area: Rect,
    snap: &Snapshot,
    state: &State,
    kanban: &Kanban<'_>,
    theme: &Theme,
) {
    let cursor = kanban.cursor(state);
    for (c, rect) in column_rects(area).into_iter().enumerate() {
        let width = rect.width as usize;
        let count = kanban.columns[c].len() + if c == DONE_COL { kanban.capped } else { 0 };
        let mut header = Line::styled(fit(&column_title(COLUMNS[c], count, width), width), theme.bold());
        if cursor.is_some_and(|(col, _)| col == c) {
            header = header.style(theme.bold().patch(theme.selected()));
        }
        let selected = cursor.and_then(|(col, card)| (col == c).then_some(card));
        let mut lines = vec![header];
        lines.extend(column_body(
            snap,
            &kanban.columns[c],
            if c == DONE_COL { kanban.capped } else { 0 },
            selected,
            rect.height.saturating_sub(1) as usize,
            width,
            theme,
        ));
        frame.render_widget(Paragraph::new(lines), rect);
    }
    if kanban.total() == 0 && area.height > 2 {
        let text = if state.filter.trim().is_empty() {
            "No issues yet. Create one with `asf issue create`."
        } else {
            "Nothing matches the filter. Press Esc to clear it."
        };
        let line = Line::styled(
            format!(" {}", trunc(text, (area.width as usize).saturating_sub(2))),
            theme.dim(),
        );
        frame.render_widget(Paragraph::new(line), Rect::new(area.x, area.y + 2, area.width, 1));
    }
}

/// The lines under a column header, `height` lines at most. When the cards do not all fit, the window
/// scrolls just far enough to show the selected card, `↑ N` stands for the cards above it and `+K more`
/// for those below it (and for the `capped` done cards that are not listed at all).
fn column_body(
    snap: &Snapshot,
    cards: &[&Issue],
    capped: usize,
    selected: Option<usize>,
    height: usize,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let all_fit = capped == 0 && cards.len() <= height / CARD_H;
    let page = if all_fit {
        cards.len()
    } else {
        (height.saturating_sub(2) / CARD_H).max(1)
    };
    let first = if all_fit {
        0
    } else {
        selected.map_or(0, |s| (s + 1).saturating_sub(page))
    };
    let shown = &cards[first.min(cards.len())..(first + page).min(cards.len())];
    let mut lines = Vec::new();
    if first > 0 {
        lines.push(Line::styled(format!("↑ {first}"), theme.dim()));
    }
    for (n, issue) in shown.iter().enumerate() {
        lines.extend(card_lines(snap, issue, selected == Some(first + n), width, theme));
        lines.push(Line::raw(""));
    }
    let below = cards.len() - (first + shown.len()).min(cards.len()) + capped;
    if below > 0 {
        lines.push(Line::styled(format!("+{below} more"), theme.dim()));
    }
    lines
}

/// A card: the status symbol and id, then the title on two lines (cut with `…`). The selected card is
/// highlighted across all three lines and marked `▸`.
fn card_lines(
    snap: &Snapshot,
    issue: &Issue,
    selected: bool,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mark = issue_mark(snap, issue);
    let text = width.saturating_sub(1).max(1);
    let wrapped = wrap_text(&issue.title, text);
    let second = if wrapped.len() > 2 {
        trunc(&wrapped[1..].join(" "), text)
    } else {
        wrapped.get(1).cloned().unwrap_or_default()
    };
    let title_style = if issue.status == Status::Done {
        theme.dim()
    } else {
        Style::default()
    };
    let mut lines = vec![
        Line::from(vec![
            Span::raw(if selected { "▸" } else { " " }),
            Span::styled(mark.glyph(), theme.mark(mark)),
            Span::raw(" "),
            Span::styled(trunc(issue.id.as_str(), width.saturating_sub(3)), theme.bold()),
        ]),
        Line::styled(
            format!(" {}", trunc(wrapped.first().map_or("", String::as_str), text)),
            title_style,
        ),
        Line::styled(format!(" {second}"), title_style),
    ];
    if selected {
        lines = lines.into_iter().map(|l| l.style(theme.selected())).collect();
    }
    lines
}

/// `backlog 5 · ready 0 · ...     / filter`: tasks and reviews per status (epics are headers and not
/// counted); `blocked` and `in-review` only when not zero. The count does not follow the filter.
fn counts_line(snap: &Snapshot, state: &State, theme: &Theme) -> Line<'static> {
    let counted = |st: Status| {
        snap.issues
            .iter()
            .filter(|i| i.kind != IssueType::Epic && i.status == st)
            .count()
    };
    let parts: Vec<String> = Status::ALL
        .iter()
        .copied()
        .filter(|st| *st != Status::Cancelled)
        .filter_map(|st| {
            let n = counted(st);
            (n > 0 || !matches!(st, Status::Blocked | Status::InReview)).then(|| format!("{st} {n}"))
        })
        .collect();
    let mut spans = vec![Span::raw(format!(" {}     ", parts.join(" · ")))];
    spans.push(Span::styled("/", theme.key()));
    if state.typing {
        spans.push(Span::raw(format!(" {}▏", state.filter)));
    } else if state.filter.trim().is_empty() {
        spans.push(Span::styled(" filter", theme.dim()));
    } else {
        spans.push(Span::raw(format!(" {}", state.filter)));
        spans.push(Span::styled("  Esc clears", theme.dim()));
    }
    Line::from(spans)
}

fn bar_cells(g: &EpicGroup<'_>) -> usize {
    if g.complete {
        BAR
    } else if g.done == 0 || g.total == 0 {
        0
    } else {
        ((g.done * BAR + g.total / 2) / g.total).clamp(1, BAR - 1)
    }
}

fn count_text(g: &EpicGroup<'_>) -> String {
    format!("{}/{}{}", g.done, g.total, if g.complete { " ✓" } else { "" })
}

/// What every epic cost so far (all its runs, and the runs of the reviews of its tasks), by epic id.
fn epic_costs(snap: &Snapshot) -> BTreeMap<IssueId, Costs> {
    rollup(&snap.issues, &snap.all_runs)
        .epics
        .into_iter()
        .map(|e| (e.epic, e.cost))
        .collect()
}

/// `$44.2`: the total in the epic header, one decimal to stay narrow.
fn header_cost(c: &Costs) -> String {
    format!("${:.1}", c.total)
}

fn plural(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

/// Column widths of an epic header; the title takes whatever the other columns leave.
struct HeaderCols {
    id_w: usize,
    title_w: usize,
    count_w: usize,
    cost_w: usize,
}

fn render_list(
    frame: &mut Frame,
    area: Rect,
    snap: &Snapshot,
    listing: &Listing<'_>,
    at: usize,
    theme: &Theme,
) {
    let width = area.width as usize;
    let mut lines: Vec<Line<'static>> = vec![Line::raw("")];
    if listing.rows.is_empty() {
        let text = if groups(snap).is_empty() {
            "No issues yet. Create one with `asf issue create`; it shows up here under its epic.".to_string()
        } else {
            "Nothing matches the filter. Press Esc to clear it.".to_string()
        };
        lines.push(Line::styled(
            format!(" {}", trunc(&text, width.saturating_sub(2))),
            theme.dim(),
        ));
        frame.render_widget(Paragraph::new(lines), area);
        return;
    }

    let id_w = listing
        .rows
        .iter()
        .filter_map(|r| match r {
            Row::Header(g) => listing.groups[*g].epic.map(|e| e.id.as_str().len()),
            Row::Issue(i) => Some(i.id.as_str().len()),
        })
        .max()
        .unwrap_or(0)
        .max(6);
    // header: margin, marker, blank, id, two blanks, title, two blanks, bar, two blanks, count, a
    // blank, cost
    let count_w = listing
        .groups
        .iter()
        .filter(|g| g.epic.is_some())
        .map(|g| text_width(&count_text(g)))
        .max()
        .unwrap_or(0);
    let costs = epic_costs(snap);
    let cost_of = |g: &EpicGroup<'_>| {
        g.epic
            .and_then(|e| costs.get(&e.id))
            .map(header_cost)
            .unwrap_or_default()
    };
    let cost_w = listing
        .groups
        .iter()
        .map(|g| text_width(&cost_of(g)))
        .max()
        .unwrap_or(0);
    let head_title_w = width
        .saturating_sub(1 + 1 + 1 + id_w + 2 + 2 + BAR + 2 + count_w + 1 + cost_w)
        .max(8);
    let cols = HeaderCols {
        id_w,
        title_w: head_title_w,
        count_w,
        cost_w,
    };
    // rows: the dependency column starts right after the longest title among the rows that have one
    let avail = width.saturating_sub(ROW_FIXED + id_w).max(8);
    let deps: Vec<(&Issue, String)> = listing
        .rows
        .iter()
        .filter_map(|r| match r {
            Row::Issue(i) => unmet_dep(snap, i).map(|d| (*i, format!("← {d}"))),
            Row::Header(_) => None,
        })
        .collect();
    let dep_w = deps.iter().map(|(_, d)| text_width(d)).max().unwrap_or(0);
    let dep_title_w = deps
        .iter()
        .map(|(i, _)| text_width(&i.title))
        .max()
        .unwrap_or(0)
        .min(avail.saturating_sub(2 + dep_w))
        .max(4);

    let mut selected_line = 0;
    let mut after_rows = false;
    for (n, row) in listing.rows.iter().enumerate() {
        let is_selected = n == at;
        let line = match row {
            Row::Header(gi) => {
                if after_rows {
                    lines.push(Line::raw(""));
                }
                after_rows = false;
                let g = &listing.groups[*gi];
                let mut style = if g.complete { theme.dim() } else { Style::default() };
                if is_selected {
                    style = style.patch(theme.selected());
                }
                header_line(g, listing.folded[*gi], &cols, &cost_of(g), theme).style(style)
            }
            Row::Issue(i) => {
                after_rows = true;
                let dep = deps
                    .iter()
                    .find(|(d, _)| d.id == i.id)
                    .map(|(_, text)| text.as_str());
                let line = issue_line(snap, i, is_selected, id_w, (avail, dep_title_w), dep, theme);
                if is_selected {
                    line.style(theme.selected())
                } else {
                    line
                }
            }
        };
        if is_selected {
            selected_line = lines.len();
        }
        lines.push(line);
    }

    // Scroll so the selected row stays on screen, centred when the list is taller than the area.
    let height = area.height as usize;
    let top = if lines.len() > height {
        selected_line.saturating_sub(height / 2).min(lines.len() - height)
    } else {
        0
    };
    let shown: Vec<Line> = lines.into_iter().skip(top).take(height).collect();
    frame.render_widget(Paragraph::new(shown), area);
}

/// `▾ asf-23  Title  ▓░░░░░░░░░  1/10 $44.2`: the count and the cost are padded to the widest of all
/// headers, so the cost column lines up (right-aligned) whatever the counts are.
fn header_line(
    g: &EpicGroup<'_>,
    folded: bool,
    cols: &HeaderCols,
    cost: &str,
    theme: &Theme,
) -> Line<'static> {
    let marker = if folded { "▸" } else { "▾" };
    let mut spans = vec![Span::raw(format!(" {marker} "))];
    match g.epic {
        Some(e) => {
            let filled = bar_cells(g);
            spans.push(Span::styled(fit(e.id.as_str(), cols.id_w), theme.bold()));
            spans.push(Span::raw(format!("  {}  ", fit(&e.title, cols.title_w))));
            spans.push(Span::raw(format!(
                "{}{}  {} {cost:>w$}",
                "▓".repeat(filled),
                "░".repeat(BAR - filled),
                fit(&count_text(g), cols.count_w),
                w = cols.cost_w,
            )));
        }
        None => spans.push(Span::styled("No epic", theme.bold())),
    }
    Line::from(spans)
}

/// `   ▸ ■ asf-27  asking       Title of the issue  ← asf-24`
fn issue_line(
    snap: &Snapshot,
    i: &Issue,
    selected: bool,
    id_w: usize,
    (avail, dep_title_w): (usize, usize),
    dep: Option<&str>,
    theme: &Theme,
) -> Line<'static> {
    let mark = issue_mark(snap, i);
    let label = state_label(snap, i);
    let label_style = if mark == Mark::Needs && matches!(label.as_str(), "asking" | "merge failed") {
        theme.mark(Mark::Needs)
    } else if i.status == Status::Done {
        theme.dim()
    } else {
        Style::default()
    };
    let mut spans = vec![
        Span::raw(format!("   {} ", if selected { "▸" } else { " " })),
        Span::styled(mark.glyph(), theme.mark(mark)),
        Span::raw(" "),
        Span::styled(fit(i.id.as_str(), id_w), theme.bold()),
        Span::raw("  "),
        Span::styled(fit(&label, LABEL_W), label_style),
        Span::raw(" "),
    ];
    match dep {
        Some(dep) => {
            spans.push(Span::raw(fit(&i.title, dep_title_w)));
            spans.push(Span::styled(format!("  {dep}"), theme.dim()));
        }
        None => spans.push(Span::raw(trunc(&i.title, avail))),
    }
    Line::from(spans)
}

// ---- the detail ------------------------------------------------------------------------------------

/// The bordered detail of `id` (the panel next to the list, or the whole area when `full`).
fn render_detail(
    frame: &mut Frame,
    area: Rect,
    snap: &Snapshot,
    id: Option<&IssueId>,
    full: bool,
    theme: &Theme,
) {
    let width = (area.width as usize).saturating_sub(4);
    let mut lines = match id {
        Some(id) => detail_lines(snap, id, width, theme),
        None => vec![Line::styled(
            "Issues without an epic are listed here.",
            theme.dim(),
        )],
    };
    if full {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("Esc", theme.key()),
            Span::styled(" back to the list", theme.dim()),
        ]));
    }
    let lines: Vec<Line> = lines
        .into_iter()
        .map(|mut l| {
            l.spans.insert(0, Span::raw(" "));
            l
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).block(Block::bordered()), area);
}

/// The text of the detail, `width` columns wide: identity, title, the run, dependencies, scope,
/// acceptance and the latest two comments or run events.
fn detail_lines(snap: &Snapshot, id: &IssueId, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let Some(i) = snap.issue(id) else {
        return vec![Line::styled(
            format!("{id} is not in the store any more."),
            theme.dim(),
        )];
    };
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut head = vec![
        Span::styled(i.id.to_string(), theme.bold()),
        Span::raw(format!(" · {} · P{}", i.kind, i.priority)),
    ];
    if let Some(e) = &i.estimate {
        head.push(Span::raw(format!(" · {e}")));
    }
    out.push(Line::from(head));
    out.extend(wrap_text(&i.title, width).into_iter().map(Line::from));

    if i.kind == IssueType::Epic {
        out.extend(cost_lines(snap, &i.id, theme));
    }

    if let Some((state, facts)) = run_lines(snap, i) {
        out.push(Line::raw(""));
        let mark = issue_mark(snap, i);
        out.push(Line::from(vec![
            Span::styled(format!("{} ", mark.glyph()), theme.mark(mark)),
            Span::raw(format!("{} {state}", state_label(snap, i))),
        ]));
        out.push(Line::from(format!(
            "  {}",
            trunc(&facts, width.saturating_sub(2))
        )));
    }

    out.push(Line::raw(""));
    let deps: Vec<String> = snap
        .deps
        .iter()
        .filter(|d| d.issue_id == i.id)
        .map(|d| match snap.issue(&d.depends_on) {
            Some(x) if x.status == Status::Done => format!("✓ {}", d.depends_on),
            _ => d.depends_on.to_string(),
        })
        .collect();
    let deps = if deps.is_empty() {
        "none".to_string()
    } else {
        deps.join("  ")
    };
    out.extend(field(
        "Depends on",
        wrap_text(&deps, width.saturating_sub(FIELD_W)),
        theme,
    ));

    let mut scope: Vec<String> = i
        .file_scope
        .iter()
        .take(2)
        .map(|g| trunc(g, width.saturating_sub(FIELD_W)))
        .collect();
    if i.file_scope.len() > 2 {
        scope.push(format!("+{} more", i.file_scope.len() - 2));
    }
    if scope.is_empty() {
        scope.push("(none)".into());
    }
    out.extend(field("Scope", scope, theme));

    out.push(Line::raw(""));
    out.push(Line::styled("Acceptance", theme.bold()));
    if i.acceptance.is_empty() {
        out.push(Line::styled(" (none)", theme.dim()));
    }
    for a in &i.acceptance {
        for (n, l) in wrap_text(&a.text, width.saturating_sub(3))
            .into_iter()
            .enumerate()
        {
            out.push(Line::from(format!("{}{l}", if n == 0 { " ☐ " } else { "   " })));
        }
    }

    out.push(Line::raw(""));
    out.push(Line::styled("Latest", theme.bold()));
    let latest = latest(snap, i);
    if latest.is_empty() {
        out.push(Line::styled(" nothing yet", theme.dim()));
    }
    for (clock, text) in latest {
        for (n, l) in wrap_text(&text, width.saturating_sub(7)).into_iter().enumerate() {
            let prefix = if n == 0 {
                format!(" {clock} ")
            } else {
                "       ".to_string()
            };
            out.push(Line::from(format!("{prefix}{l}")));
        }
    }
    out
}

/// The cost breakdown of epic `id`: what its tasks cost, what the reviews of those tasks cost, the sum.
fn cost_lines(snap: &Snapshot, id: &IssueId, theme: &Theme) -> Vec<Line<'static>> {
    let Some(c) = epic_costs(snap).remove(id) else {
        return Vec::new();
    };
    let mut out = vec![Line::raw(""), Line::styled("Cost", theme.bold())];
    out.extend(field(
        "Tasks",
        vec![format!("${:.2} · {}/{} done", c.task_cost, c.done, c.tasks)],
        theme,
    ));
    out.extend(field(
        "Reviews",
        vec![format!("${:.2} · {}", c.review_cost, plural(c.reviews, "run"))],
        theme,
    ));
    out.extend(field("Total", vec![format!("${:.2}", c.total)], theme));
    out
}

/// A labelled field: the label on the first line, the continuation lines indented under the text.
fn field(label: &str, text: Vec<String>, theme: &Theme) -> Vec<Line<'static>> {
    text.into_iter()
        .enumerate()
        .map(|(n, t)| {
            let lead = if n == 0 {
                Span::styled(format!("{label:<FIELD_W$}"), theme.dim())
            } else {
                Span::raw(" ".repeat(FIELD_W))
            };
            Line::from(vec![lead, Span::raw(t)])
        })
        .collect()
}

/// `since 4 min` and `sonnet · $1.12 / $10 · 21 calls · 14 min` for the active run of `issue`, else for
/// its latest run; `None` when it never ran.
fn run_lines(snap: &Snapshot, issue: &Issue) -> Option<(String, String)> {
    let last = snap.last_run(&issue.id);
    let model = last
        .and_then(|l| l.model.clone())
        .unwrap_or_else(|| "default".to_string());
    let (since, started, end, cost, budget, calls) = if let Some(r) = snap.run_for(&issue.id) {
        let since = snap
            .events
            .iter()
            .find(|e| {
                e.kind == "run.state"
                    && e.run_id.as_deref() == Some(r.id.as_str())
                    && e.payload["to"].as_str() == Some(r.state.as_str())
            })
            .map(|e| e.ts.clone())
            .unwrap_or_else(|| r.started_at.clone());
        (
            since,
            r.started_at.clone(),
            snap.now.clone(),
            r.cost_usd,
            r.budget_usd,
            r.tool_calls,
        )
    } else {
        let l = last?;
        let end = l.finished_at.clone().unwrap_or_else(|| snap.now.clone());
        (
            end.clone(),
            l.started_at.clone(),
            end,
            l.cost,
            l.budget,
            l.tool_calls,
        )
    };
    Some((
        format!("since {}", age(&snap.now, &since)),
        format!(
            "{model} · {} · {calls} calls · {}",
            money(cost, budget),
            age(&end, &started)
        ),
    ))
}

/// The two newest comments or run events of `issue`, as `(HH:MM, text)`.
fn latest(snap: &Snapshot, issue: &Issue) -> Vec<(String, String)> {
    let mut all: Vec<(String, String)> = Vec::new();
    if let Some(d) = snap.detail_of(&issue.id) {
        for c in &d.comments {
            all.push((
                c.created_at.clone(),
                format!("{} {}", c.kind, first_line(&c.body).unwrap_or("")),
            ));
        }
    }
    for e in snap
        .events
        .iter()
        .filter(|e| e.issue_id.as_ref() == Some(&issue.id) && e.kind.starts_with("run."))
    {
        let text = match e.kind.as_str() {
            "run.created" => format!(
                "run started · worker {}",
                snap.claims
                    .iter()
                    .find(|c| c.issue_id == issue.id)
                    .map_or_else(|| issue.id.to_string(), |c| c.worker.clone())
            ),
            "run.state" => format!("run {}", e.payload["to"].as_str().unwrap_or("?")),
            other => other.replace('.', " "),
        };
        all.push((e.ts.clone(), text));
    }
    all.sort_by(|a, b| b.0.cmp(&a.0)); // stable: comments stay before events at the same time
    let offset = offset_of(snap);
    all.into_iter()
        .take(2)
        .map(|(ts, text)| {
            let clock = parse_time(&ts).map_or_else(
                || "--:--".to_string(),
                |t| {
                    let t = t.to_offset(offset);
                    format!("{:02}:{:02}", t.hour(), t.minute())
                },
            );
            (clock, text)
        })
        .collect()
}
