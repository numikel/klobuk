//! The Board view: issues grouped under their epic, one header per epic with a progress bar, and (from 120
//! columns) a bordered detail panel of the selected row next to the list.
//!
//! The row model ([`Listing`]) is a pure function of the snapshot and the [`State`] (folds and filter),
//! so the key handling walks exactly the rows the renderer draws. An epic is never an issue row: it is a
//! group header only, and it is not counted in the status counts.

use std::collections::BTreeMap;

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

/// Selection, folds, filter and whether the full-screen detail is open.
#[derive(Debug, Clone)]
pub struct State {
    /// Index into the visible rows (headers included); clamped when read.
    pub selected: usize,
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
        "fold epic",
        "fold / unfold the epic the selection is in",
        Group::Board,
        BOARD,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "/",
        "filter",
        "filter by id, title or status\n(Enter keeps it, Esc clears it)",
        Group::Board,
        BOARD,
        NORMAL,
        Slot::Hint,
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

/// The issue the selection points at; an epic header stands for its epic, `No epic` for nothing.
pub fn selected_issue(snap: &Snapshot, state: &State) -> Option<IssueId> {
    let listing = Listing::build(snap, state);
    listing.id_at(state.selected.min(listing.rows.len().saturating_sub(1)))
}

/// Moves the selection to `id`, opening its group (and dropping a filter that hides it). It stays where
/// it was when the Board does not list `id`.
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
    let listing_rows = Listing::build(snap, state).rows.len();
    if let Some(i) = move_selection(&key, state.selected, listing_rows) {
        state.selected = i;
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
        (KeyCode::Char('h'), _) => fold(snap, state, true),
        (KeyCode::Char('l'), _) => fold(snap, state, false),
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
        None => state.selected = 0,
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
    // header: margin, marker, blank, id, two blanks, title, two blanks, bar, two blanks, count
    let count_w = listing
        .groups
        .iter()
        .filter(|g| g.epic.is_some())
        .map(|g| text_width(&count_text(g)))
        .max()
        .unwrap_or(0);
    let head_title_w = width
        .saturating_sub(1 + 1 + 1 + id_w + 2 + 2 + BAR + 2 + count_w)
        .max(8);
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
                header_line(g, listing.folded[*gi], id_w, head_title_w, theme).style(style)
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

fn header_line(g: &EpicGroup<'_>, folded: bool, id_w: usize, title_w: usize, theme: &Theme) -> Line<'static> {
    let marker = if folded { "▸" } else { "▾" };
    let mut spans = vec![Span::raw(format!(" {marker} "))];
    match g.epic {
        Some(e) => {
            let filled = bar_cells(g);
            spans.push(Span::styled(fit(e.id.as_str(), id_w), theme.bold()));
            spans.push(Span::raw(format!("  {}  ", fit(&e.title, title_w))));
            spans.push(Span::raw(format!(
                "{}{}  {}",
                "▓".repeat(filled),
                "░".repeat(BAR - filled),
                count_text(g)
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
