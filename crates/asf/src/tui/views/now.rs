//! The Now view: the operator's desk. It answers "does the factory need something from me, and if
//! not, what is it doing" with four borderless sections: NEEDS YOU, RUNNING, UP NEXT, DONE TODAY.
//!
//! The row model (`running`, `up_next`, `done_today`, and the NEEDS YOU list the snapshot carries) is a
//! pure function of a [`Snapshot`], so the key handling walks the same rows the renderer draws.

use std::collections::{BTreeMap, BTreeSet};

use asf_core::config::NotifyCfg;
use asf_core::remind::{due, WaitKind, Waiting};
use asf_core::*;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use time::OffsetDateTime;

use crate::tui::app::{Action, View};
use crate::tui::bindings::*;
use crate::tui::overlays::InputTarget;
use crate::tui::snapshot::{
    age, first_line, fit, money, move_selection, offset_of, parse_time, Need, NeedSource, Snapshot,
};
use crate::tui::theme::{Mark, Theme};

/// How many up-next ids the one-line summary names.
const UP_NEXT_IDS: usize = 3;

/// The Config row that holds `[notify] after_minutes`, the wait after which a row is overdue.
const AFTER_MINUTES_KEY: &str = "notify.after_minutes";

/// Shown before the age of an overdue NEEDS YOU row, so the mark never depends on colour alone.
const OVERDUE_MARK: &str = "!";

/// Width of the age cell of a NEEDS YOU row: the mark or two blanks, then the age (at most `59 min`),
/// and a gap before the key.
const AGE_W: usize = 10;

/// Width of the state cell of a RUNNING row: room for `fixing after review`.
const RUN_STATE_W: usize = 19;

/// Selection over the NEEDS YOU rows followed by the RUNNING rows.
#[derive(Debug, Clone, Default)]
pub struct State {
    pub selected: usize,
}

/// One RUNNING row.
#[derive(Debug, Clone, PartialEq)]
pub struct RunRow {
    pub issue: IssueId,
    pub reviewer: bool,
    /// The run state, or `review` for a reviewer.
    pub state: String,
    /// The issue title, or `<model> · round N` for a reviewer.
    pub title: String,
    pub cost: f64,
    pub budget: Option<f64>,
    pub since: String,
}

/// The row the Now view's selection points at.
#[derive(Debug, Clone, PartialEq)]
pub enum NowRow {
    Need(Need),
    Running(RunRow),
}

impl NowRow {
    pub fn issue(&self) -> &IssueId {
        match self {
            NowRow::Need(n) => &n.issue,
            NowRow::Running(r) => &r.issue,
        }
    }

    /// What to say after a key that does not fit the row.
    pub fn advice(&self) -> String {
        match self {
            NowRow::Need(n) => format!("this row takes {}", n.act.hint()),
            NowRow::Running(_) => "it is running".to_string(),
        }
    }
}

pub const BINDINGS: &[Binding] = &[
    b(
        "j / k",
        "move",
        HELP_MOVE,
        Group::Everywhere,
        NOW,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "a",
        "answer",
        HELP_ANSWER,
        Group::NeedsYou,
        NOW,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "f",
        "focus pane",
        HELP_FOCUS,
        Group::NeedsYou,
        NOW,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "Enter",
        "open issue",
        HELP_OPEN,
        Group::Everywhere,
        NOW,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "v",
        "verify",
        HELP_VERIFY,
        Group::NeedsYou,
        NOW,
        NORMAL,
        Slot::None,
    ),
    b("x", "abort", HELP_ABORT, Group::NeedsYou, NOW, NORMAL, Slot::None),
    // Listed under NEEDS YOU, where it works: on a merge row of this view. The Queue view's own `r`
    // sends a message (QUEUE gets this key from the Queue task, when that view retries).
    b(
        "r",
        "retry",
        "Now, on a merge row: try it again\nafter a fix",
        Group::NeedsYou,
        NOW,
        NORMAL,
        Slot::None,
    ),
];

const NOW: &[View] = &[View::Now];

/// `fixing after review` / `fixing after gate`: what a working run is doing when its issue still has a
/// failed merge item, i.e. a reviewer or a post-rebase gate sent the worker back (the item stays
/// `failed` until the worker finishes again). The text of the item's error tells which of the two.
fn fixing_label(snap: &Snapshot, issue: &IssueId) -> Option<&'static str> {
    let item = snap
        .queue
        .iter()
        .find(|m| &m.issue_id == issue && m.state == MergeState::Failed)?;
    let error = first_line(item.last_error.as_deref()?)?;
    if error.starts_with("review asks for changes") {
        Some("fixing after review")
    } else if error.starts_with("gate `") {
        Some("fixing after gate")
    } else {
        None
    }
}

/// Active runs that are not already a NEEDS YOU row (a blocked, stalled or over-budget run is listed
/// there, once), including reviewer runs, oldest first. A worker fixing what a reviewer or a gate
/// found says so (`fixing after review`), which is why its failed merge item is not in NEEDS YOU.
pub fn running(snap: &Snapshot) -> Vec<RunRow> {
    snap.runs
        .iter()
        .filter(|r| {
            matches!(
                r.state,
                RunState::Starting | RunState::Working | RunState::Finishing | RunState::Verifying
            )
        })
        .map(|r| {
            let issue = snap.issue(&r.issue_id);
            let reviewer = issue.is_some_and(|i| i.kind == IssueType::Review);
            let title = if reviewer {
                reviewer_title(snap, &r.issue_id, issue)
            } else {
                issue.map(|i| i.title.clone()).unwrap_or_default()
            };
            RunRow {
                issue: r.issue_id.clone(),
                reviewer,
                state: if reviewer {
                    "review".into()
                } else if let Some(fixing) =
                    fixing_label(snap, &r.issue_id).filter(|_| r.state == RunState::Working)
                {
                    fixing.into()
                } else {
                    r.state.to_string()
                },
                title,
                cost: r.cost_usd,
                budget: r.budget_usd,
                since: r.started_at.clone(),
            }
        })
        .collect()
}

/// `opus · round 1`: the review model and the round the merge queue started for the reviewed issue.
fn reviewer_title(snap: &Snapshot, review: &IssueId, issue: Option<&Issue>) -> String {
    let round = snap.review_of.get(review).and_then(|l| l.round);
    let mut parts = Vec::new();
    if !snap.review_model.is_empty() {
        parts.push(snap.review_model.clone());
    }
    if let Some(n) = round {
        parts.push(format!("round {n}"));
    }
    if parts.is_empty() {
        issue.map(|i| i.title.clone()).unwrap_or_default()
    } else {
        parts.join(" · ")
    }
}

/// The UP NEXT line: how many `ready` + `backlog` tasks there are, the first few ids and, when they
/// all wait for the same single issue, which one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpNext {
    pub count: usize,
    pub ids: Vec<IssueId>,
    pub waits_for: Option<IssueId>,
}

pub fn up_next(snap: &Snapshot) -> UpNext {
    let mut items: Vec<&Issue> = snap
        .issues
        .iter()
        .filter(|i| i.kind == IssueType::Task && matches!(i.status, Status::Ready | Status::Backlog))
        .collect();
    // ready before backlog, then by id
    items.sort_by(|a, b| (a.status != Status::Ready, &a.id).cmp(&(b.status != Status::Ready, &b.id)));
    let unmet = |id: &IssueId| -> BTreeSet<IssueId> {
        snap.deps
            .iter()
            .filter(|d| &d.issue_id == id)
            .filter(|d| {
                snap.issues
                    .iter()
                    .any(|i| i.id == d.depends_on && !i.status.is_terminal())
            })
            .map(|d| d.depends_on.clone())
            .collect()
    };
    let mut shared: Option<BTreeSet<IssueId>> = None;
    for i in &items {
        let mine = unmet(&i.id);
        shared = Some(match shared {
            None => mine,
            Some(s) => s.intersection(&mine).cloned().collect(),
        });
    }
    let waits_for = shared.filter(|s| s.len() == 1).and_then(|s| s.into_iter().next());
    UpNext {
        count: items.len(),
        ids: items.iter().take(UP_NEXT_IDS).map(|i| i.id.clone()).collect(),
        waits_for,
    }
}

/// Issues closed today (local date of the snapshot), newest first. Review issues are internal and left out.
pub fn done_today(snap: &Snapshot) -> Vec<IssueId> {
    let offset = offset_of(snap);
    let Some(today) = parse_time(&snap.now).map(|t: OffsetDateTime| t.to_offset(offset).date()) else {
        return Vec::new();
    };
    let mut closed: Vec<(&str, &IssueId)> = snap
        .issues
        .iter()
        .filter(|i| i.status == Status::Done && i.kind != IssueType::Review)
        .filter_map(|i| {
            let at = i.closed_at.as_deref()?;
            (parse_time(at)?.to_offset(offset).date() == today).then_some((at, &i.id))
        })
        .collect();
    closed.sort_by(|a, b| b.cmp(a));
    closed.into_iter().map(|(_, id)| id.clone()).collect()
}

/// Number of selectable rows: NEEDS YOU, then RUNNING.
pub fn rows(snap: &Snapshot) -> usize {
    snap.needs_you.len() + running(snap).len()
}

/// The selection, kept inside the rows.
pub fn selected(snap: &Snapshot, state: &State) -> usize {
    state.selected.min(rows(snap).saturating_sub(1))
}

/// What the selection points at: the NEEDS YOU rows first, then the RUNNING rows.
pub fn selected_row(snap: &Snapshot, state: &State) -> Option<NowRow> {
    let i = selected(snap, state);
    let n = snap.needs_you.len();
    if i < n {
        snap.needs_you.get(i).cloned().map(NowRow::Need)
    } else {
        running(snap).into_iter().nth(i - n).map(NowRow::Running)
    }
}

/// `[notify] after_minutes` as the snapshot's Config rows hold it (`0` = reminders, and so the marks,
/// are off). A snapshot without that row has the default; a value that does not parse marks nothing.
fn after_minutes(snap: &Snapshot) -> u64 {
    match snap.config.iter().find(|r| r.key == AFTER_MINUTES_KEY) {
        None => NotifyCfg::default().after_minutes,
        Some(row) => row.raw.trim().parse().unwrap_or(0),
    }
}

/// What `need` waits on, in the words of the reminder rules.
fn wait_kind(source: &NeedSource) -> WaitKind {
    match source {
        NeedSource::BlockedRun => WaitKind::RunBlocked,
        NeedSource::Question(_) => WaitKind::Question,
        NeedSource::Run(RunState::Failed) => WaitKind::RunFailed,
        NeedSource::Run(RunState::Stalled) => WaitKind::RunStalled,
        NeedSource::Run(_) => WaitKind::RunOverBudget,
        NeedSource::Merge(MergeState::Failed) => WaitKind::MergeFailed,
        NeedSource::Merge(_) => WaitKind::MergeBlocked,
    }
}

/// Whether `need` has waited at least `[notify] after_minutes`. It asks [`due`] with nothing reminded
/// yet, so the threshold is the one `asf serve` applies when it sends the first reminder.
pub fn is_overdue(snap: &Snapshot, need: &Need) -> bool {
    let Some(now) = parse_time(&snap.now) else {
        return false;
    };
    let cfg = NotifyCfg {
        after_minutes: after_minutes(snap),
        ..NotifyCfg::default()
    };
    let waiting = [Waiting {
        issue: need.issue.clone(),
        kind: wait_kind(&need.source),
        since: need.since.clone(),
        reason: String::new(),
        key: String::new(),
    }];
    !due(&waiting, &BTreeMap::new(), &BTreeMap::new(), now, &cfg).is_empty()
}

fn title(text: String, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(format!(" {text}"), theme.bold()))
}

/// Draws the four sections into `area`, one cell in from the left.
pub fn render(frame: &mut Frame, area: Rect, snap: &Snapshot, state: &State, theme: &Theme) {
    let width = area.width as usize;
    let needs = &snap.needs_you;
    let running = running(snap);
    let selected = selected(snap, state);
    let id_w = needs
        .iter()
        .map(|n| &n.issue)
        .chain(running.iter().map(|r| &r.issue))
        .map(|id| id.as_str().chars().count())
        .max()
        .unwrap_or(0)
        .max(6);
    let select_marker = |i: usize| if i == selected { "▸ " } else { "  " };
    let row_style = |i: usize| {
        if i == selected {
            theme.selected()
        } else {
            Style::default()
        }
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut selected_line = 0;

    lines.push(title(format!("NEEDS YOU ({})", needs.len()), theme));
    if needs.is_empty() {
        lines.push(Line::from(Span::styled("   Nothing needs you.", theme.dim())));
    }
    // fixed columns: margin 1, selector 2, mark 2, id, gap 2, label 15, gap 2, age 10, key 8
    let text_w = width
        .saturating_sub(1 + 2 + 2 + id_w + 2 + 15 + 2 + AGE_W + 8)
        .max(8);
    for (i, n) in needs.iter().enumerate() {
        if i == selected {
            selected_line = lines.len();
        }
        let mark = Mark::Needs;
        // an overdue row prefixes its age with `!` in the attention style (the banner's); the other
        // rows get blanks of the same width, so the ages line up
        let (waited, waited_style) = if is_overdue(snap, n) {
            (
                format!("{OVERDUE_MARK} {}", age(&snap.now, &n.since)),
                theme.warn(),
            )
        } else {
            (format!("  {}", age(&snap.now, &n.since)), Style::default())
        };
        lines.push(
            Line::from(vec![
                Span::raw(format!(" {}", select_marker(i))),
                Span::styled(format!("{} ", mark.glyph()), theme.mark(mark)),
                Span::styled(format!("{}  ", fit(n.issue.as_str(), id_w)), theme.bold()),
                Span::raw(fit(&n.label, 15)),
                Span::raw(format!("{}  ", fit(&n.text, text_w))),
                Span::styled(fit(&waited, AGE_W), waited_style),
                Span::styled(n.act.hint(), theme.key()),
            ])
            .style(row_style(i)),
        );
    }
    lines.push(Line::raw(""));

    lines.push(title(format!("RUNNING ({})", running.len()), theme));
    if running.is_empty() {
        lines.push(Line::from(Span::styled(
            "   No worker is running. Press Tab for the Board to start one.",
            theme.dim(),
        )));
    }
    // fixed columns: margin 1, selector 2, mark 2, id, gap 2, state 19, gap 2, money 14, gap 2, elapsed 8
    let title_w = width
        .saturating_sub(1 + 2 + 2 + id_w + 2 + RUN_STATE_W + 2 + 14 + 2 + 8)
        .max(8);
    for (j, r) in running.iter().enumerate() {
        let i = needs.len() + j;
        if i == selected {
            selected_line = lines.len();
        }
        let mark = if r.reviewer { Mark::Review } else { Mark::Working };
        lines.push(
            Line::from(vec![
                Span::raw(format!(" {}", select_marker(i))),
                Span::styled(format!("{} ", mark.glyph()), theme.mark(mark)),
                Span::styled(format!("{}  ", fit(r.issue.as_str(), id_w)), theme.bold()),
                Span::raw(format!("{}  ", fit(&r.state, RUN_STATE_W))),
                Span::raw(format!("{}  ", fit(&r.title, title_w))),
                Span::raw(format!("{}  ", fit(&money(r.cost, r.budget), 14))),
                Span::raw(age(&snap.now, &r.since)),
            ])
            .style(row_style(i)),
        );
    }
    lines.push(Line::raw(""));

    lines.push(up_next_line(snap, theme));
    lines.push(done_line(snap, width, theme));

    // Scroll so the selected row stays on screen when the sections are taller than the area.
    let height = area.height as usize;
    let top = if lines.len() > height {
        (selected_line + 1)
            .saturating_sub(height)
            .min(lines.len() - height)
    } else {
        0
    };
    let shown: Vec<Line> = lines.into_iter().skip(top).take(height).collect();
    frame.render_widget(Paragraph::new(shown), area);
}

fn up_next_line(snap: &Snapshot, theme: &Theme) -> Line<'static> {
    let next = up_next(snap);
    let head = Span::styled(format!(" UP NEXT ({})", next.count), theme.bold());
    if next.count == 0 {
        return Line::from(vec![
            head,
            Span::raw("   "),
            Span::styled("nothing is waiting to start", theme.dim()),
        ]);
    }
    let mut text = next
        .ids
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    if next.count > next.ids.len() {
        text.push_str(" …");
    }
    if let Some(dep) = &next.waits_for {
        text.push_str(&format!(" waiting for {dep}"));
    }
    Line::from(vec![head, Span::raw(format!("   {text}"))])
}

fn done_line(snap: &Snapshot, width: usize, theme: &Theme) -> Line<'static> {
    let done = done_today(snap);
    let head_text = format!(" DONE TODAY ({})", done.len());
    let head = Span::styled(head_text.clone(), theme.bold());
    if done.is_empty() {
        return Line::from(vec![
            head,
            Span::raw("   "),
            Span::styled("nothing closed yet today", theme.dim()),
        ]);
    }
    // room for the items after the title, three blanks and a closing ` …`
    let room = width.saturating_sub(head_text.chars().count() + 3 + 2);
    let mut shown = String::new();
    let mut taken = 0;
    for id in &done {
        let item = format!("✓ {id}");
        let extra = item.chars().count() + if taken == 0 { 0 } else { 2 };
        if taken > 0 && shown.chars().count() + extra > room {
            break;
        }
        if taken > 0 {
            shown.push_str("  ");
        }
        shown.push_str(&item);
        taken += 1;
    }
    if taken < done.len() {
        shown.push_str(" …");
    }
    Line::from(vec![head, Span::raw("   "), Span::styled(shown, theme.dim())])
}

/// Keys of the Now view: navigation, and the one key each NEEDS YOU row takes.
pub fn handle_key(key: KeyEvent, snap: &Snapshot, state: &mut State) -> Action {
    if let Some(i) = move_selection(&key, state.selected, rows(snap)) {
        state.selected = i;
        return Action::None;
    }
    let row = selected_row(snap, state);
    match key.code {
        KeyCode::Enter => row.map_or(Action::None, |r| Action::OpenOnBoard(r.issue().clone())),
        KeyCode::Char('f') => row.map_or(Action::None, |r| Action::Focus(r.issue().clone())),
        KeyCode::Char('a') => answer(row),
        KeyCode::Char('v') => verify(row),
        KeyCode::Char('r') => retry(row),
        KeyCode::Char('x') => abort(row),
        _ => Action::None,
    }
}

/// `a`: a blocked or stalled worker gets the answer overlay (a reply moves a stalled run back to
/// working), a QUESTION comment a typed answer.
fn answer(row: Option<NowRow>) -> Action {
    match row {
        Some(NowRow::Need(n)) => match n.source {
            NeedSource::BlockedRun | NeedSource::Run(RunState::Stalled) => Action::OpenAnswer(n.issue),
            NeedSource::Question(question) => Action::OpenInput {
                prompt: format!("answer {} (#{question})", n.issue),
                buf: String::new(),
                target: InputTarget::Answer(n.issue, question),
            },
            _ => Action::Status(format!(
                "{}: a answers a question; this row takes {}",
                n.issue,
                n.act.hint()
            )),
        },
        Some(NowRow::Running(r)) => Action::Status(format!("{} is running; nothing to answer", r.issue)),
        None => Action::None,
    }
}

/// `v`: rerun verification of a failed run. `asf run verify` refuses a worker that is still active
/// (stalled, over budget), so those rows never get here.
fn verify(row: Option<NowRow>) -> Action {
    match row {
        Some(NowRow::Need(n)) if n.source == NeedSource::Run(RunState::Failed) => Action::Verify(n.issue),
        Some(row) => Action::Status(format!(
            "{}: v verifies a failed run; {}",
            row.issue(),
            row.advice()
        )),
        None => Action::None,
    }
}

/// `x`: abort an over-budget worker, after a confirmation.
fn abort(row: Option<NowRow>) -> Action {
    match row {
        Some(NowRow::Need(n)) if n.source == NeedSource::Run(RunState::OverBudget) => Action::Confirm {
            what: format!("abort {}?", n.issue),
            then: Box::new(Action::Abort(n.issue)),
        },
        Some(row) => Action::Status(format!(
            "{}: x aborts an over-budget run; {}",
            row.issue(),
            row.advice()
        )),
        None => Action::None,
    }
}

/// `r`: re-queue a failed or blocked merge item.
fn retry(row: Option<NowRow>) -> Action {
    match row {
        Some(NowRow::Need(n)) if matches!(n.source, NeedSource::Merge(_)) => Action::Retry(n.issue),
        Some(row) => Action::Status(format!(
            "{}: r retries a failed or blocked merge; {}",
            row.issue(),
            row.advice()
        )),
        None => Action::None,
    }
}
