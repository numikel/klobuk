//! The Now view: the operator's desk. It answers "does the factory need something from me, and if
//! not, what is it doing" with four borderless sections: NEEDS YOU, RUNNING, UP NEXT, DONE TODAY.
//!
//! The row model (`needs`, `running`, `up_next`, `done_today`) is a pure function of a [`Snapshot`],
//! so the app can walk the same rows the renderer draws.

use std::collections::BTreeSet;

use asf_core::*;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

use super::app::{App, Snapshot, View};
use super::theme::{dim, Mark};

/// The event the merge queue appends when a review round starts (`commands/merge.rs`); its payload
/// carries the `round` number the reviewer row shows.
const REVIEW_STARTED: &str = "merge.review_started";

/// How many up-next ids the one-line summary names.
const UP_NEXT_IDS: usize = 3;

/// The one key that acts on a NEEDS YOU row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    /// `a`: the answer overlay (blocked or stalled worker) or a typed answer (QUESTION comment).
    Answer,
    /// `v`: rerun verification of a failed run.
    Verify,
    /// `r`: re-queue the merge item.
    Retry,
    /// `x`: abort an over-budget worker (after a confirmation).
    Abort,
}

impl Act {
    pub fn key(self) -> char {
        match self {
            Act::Answer => 'a',
            Act::Verify => 'v',
            Act::Retry => 'r',
            Act::Abort => 'x',
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Act::Answer => "answer",
            Act::Verify => "verify",
            Act::Retry => "retry",
            Act::Abort => "abort",
        }
    }

    /// `a answer`: how the key is printed at the end of a row.
    pub fn hint(self) -> String {
        format!("{} {}", self.key(), self.label())
    }
}

/// Where a NEEDS YOU row comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NeedSource {
    /// A run in `blocked`: the agent sits on a prompt.
    BlockedRun,
    /// An unanswered QUESTION comment to the human (its comment id).
    Question(i64),
    /// A run in `failed`, `stalled` or `over_budget`.
    Run(RunState),
    /// A merge item in `failed` or `blocked`.
    Merge(MergeState),
}

/// One NEEDS YOU row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Need {
    pub issue: IssueId,
    pub source: NeedSource,
    /// `question`, `failed`, `stalled`, `over_budget`, `merge failed` or `merge blocked`.
    pub label: String,
    pub text: String,
    /// RFC 3339 UTC time the thing started waiting; the age and the order come from it.
    pub since: String,
    pub act: Act,
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

/// The first non-empty line of `s`, trimmed.
fn first_line(s: &str) -> Option<&str> {
    s.lines().map(str::trim).find(|l| !l.is_empty())
}

/// The first non-empty line of the worker screen quoted in a BLOCKER body (the fenced block after
/// `Screen:`), or `None` when the body quotes no screen.
fn screen_line(body: &str) -> Option<String> {
    let mut lines = body.lines().skip_while(|l| l.trim() != "Screen:");
    lines.next()?; // the `Screen:` line itself
    let mut fenced = false;
    for line in lines {
        if line.trim_start().starts_with("```") {
            if fenced {
                return None;
            }
            fenced = true;
        } else if fenced && !line.trim().is_empty() {
            return Some(line.trim().to_string());
        }
    }
    None
}

/// When `run` entered its current state: the newest `run.state` event that moved it there, else the
/// time the run finished or started.
fn state_since(snap: &Snapshot, run: &Run) -> String {
    snap.events
        .iter()
        .find(|e| {
            e.kind == "run.state"
                && e.run_id.as_deref() == Some(run.id.as_str())
                && e.payload["to"].as_str() == Some(run.state.as_str())
        })
        .map(|e| e.ts.clone())
        .or_else(|| run.finished_at.clone())
        .unwrap_or_else(|| run.started_at.clone())
}

/// What needs the human, oldest first: blocked runs, unanswered QUESTIONs to the human, runs in
/// `failed`/`stalled`/`over_budget`, and merge items in `failed`/`blocked`.
pub fn needs(snap: &Snapshot) -> Vec<Need> {
    let mut out = Vec::new();
    for run in snap.runs.iter().chain(&snap.failed_runs) {
        let reason = || {
            run.exit_reason
                .as_deref()
                .and_then(first_line)
                .unwrap_or("no exit reason recorded")
                .to_string()
        };
        match run.state {
            RunState::Blocked => {
                let screen = snap
                    .blockers
                    .iter()
                    .filter(|c| c.issue_id == run.issue_id)
                    .max_by_key(|c| c.id)
                    .and_then(|c| screen_line(&c.body));
                out.push(Need {
                    issue: run.issue_id.clone(),
                    source: NeedSource::BlockedRun,
                    label: "question".into(),
                    text: screen.unwrap_or_else(|| "waiting for your answer".into()),
                    since: state_since(snap, run),
                    act: Act::Answer,
                });
            }
            // The key on a row is one that the command behind it accepts for that state:
            // `asf run verify` refuses a worker that is still active (stalled, over budget),
            // a reply moves a stalled run back to working, and `asf run abort` takes any
            // run that is not finished.
            RunState::Failed | RunState::Stalled | RunState::OverBudget => out.push(Need {
                issue: run.issue_id.clone(),
                source: NeedSource::Run(run.state),
                label: run.state.to_string(),
                text: reason(),
                since: state_since(snap, run),
                act: match run.state {
                    RunState::Failed => Act::Verify,
                    RunState::Stalled => Act::Answer,
                    _ => Act::Abort,
                },
            }),
            _ => {}
        }
    }
    for q in &snap.questions {
        out.push(Need {
            issue: q.issue_id.clone(),
            source: NeedSource::Question(q.id),
            label: "question".into(),
            text: first_line(&q.body).unwrap_or("(empty question)").to_string(),
            since: q.created_at.clone(),
            act: Act::Answer,
        });
    }
    for m in &snap.queue {
        let label = match m.state {
            MergeState::Failed => "merge failed",
            MergeState::Blocked => "merge blocked",
            _ => continue,
        };
        out.push(Need {
            issue: m.issue_id.clone(),
            source: NeedSource::Merge(m.state),
            label: label.into(),
            text: m
                .last_error
                .as_deref()
                .and_then(first_line)
                .unwrap_or("no error recorded")
                .to_string(),
            since: m.updated_at.clone(),
            act: Act::Retry,
        });
    }
    // RFC 3339 UTC strings sort by time; the issue id keeps ties stable.
    out.sort_by(|a, b| (&a.since, &a.issue).cmp(&(&b.since, &b.issue)));
    out
}

/// Active runs that are not already a NEEDS YOU row (a blocked, stalled or over-budget run is listed
/// there, once), including reviewer runs, oldest first.
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
            let issue = snap.issues.iter().find(|i| i.id == r.issue_id);
            let reviewer = issue.is_some_and(|i| i.kind == IssueType::Review);
            let title = if reviewer {
                reviewer_title(snap, issue)
            } else {
                issue.map(|i| i.title.clone()).unwrap_or_default()
            };
            RunRow {
                issue: r.issue_id.clone(),
                reviewer,
                state: if reviewer {
                    "review".into()
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
fn reviewer_title(snap: &Snapshot, review: Option<&Issue>) -> String {
    let round = review
        .and_then(|i| i.parent_id.as_ref())
        .and_then(|parent| {
            snap.events
                .iter()
                .find(|e| e.kind == REVIEW_STARTED && e.issue_id.as_ref() == Some(parent))
        })
        .and_then(|e| e.payload["round"].as_i64());
    let mut parts = Vec::new();
    if !snap.review_model.is_empty() {
        parts.push(snap.review_model.clone());
    }
    if let Some(n) = round {
        parts.push(format!("round {n}"));
    }
    if parts.is_empty() {
        review.map(|i| i.title.clone()).unwrap_or_default()
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

fn parse(ts: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(ts, &Rfc3339).ok()
}

/// Issues closed today (local date of the snapshot), newest first. Review issues are internal and left out.
pub fn done_today(snap: &Snapshot) -> Vec<IssueId> {
    let offset = UtcOffset::from_whole_seconds(snap.utc_offset_secs).unwrap_or(UtcOffset::UTC);
    let Some(today) = parse(&snap.now).map(|t| t.to_offset(offset).date()) else {
        return Vec::new();
    };
    let mut closed: Vec<(&str, &IssueId)> = snap
        .issues
        .iter()
        .filter(|i| i.status == Status::Done && i.kind != IssueType::Review)
        .filter_map(|i| {
            let at = i.closed_at.as_deref()?;
            (parse(at)?.to_offset(offset).date() == today).then_some((at, &i.id))
        })
        .collect();
    closed.sort_by(|a, b| b.cmp(a));
    closed.into_iter().map(|(_, id)| id.clone()).collect()
}

/// How long ago `since` was, as the rows print it: `<1 min`, `4 min`, `3 h`, `2 d`. `-` when either
/// time does not parse.
pub fn age(now: &str, since: &str) -> String {
    let (Some(now), Some(since)) = (parse(now), parse(since)) else {
        return "-".into();
    };
    let secs = (now - since).whole_seconds().max(0);
    match secs {
        0..=59 => "<1 min".into(),
        60..=3599 => format!("{} min", secs / 60),
        3600..=86399 => format!("{} h", secs / 3600),
        _ => format!("{} d", secs / 86400),
    }
}

/// `$0.50 / $10`: cost with cents, budget without them when it is a whole number.
fn money(cost: f64, budget: Option<f64>) -> String {
    match budget {
        Some(b) if b.fract() == 0.0 => format!("${cost:.2} / ${b:.0}"),
        Some(b) => format!("${cost:.2} / ${b:.2}"),
        None => format!("${cost:.2}"),
    }
}

/// `s` cut to `n` characters with a trailing `…`, then padded with blanks to exactly `n`.
fn fit(s: &str, n: usize) -> String {
    let len = s.chars().count();
    if len > n {
        let cut: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{cut}…")
    } else {
        format!("{s}{}", " ".repeat(n - len))
    }
}

fn title(text: String) -> Line<'static> {
    Line::from(Span::styled(
        format!(" {text}"),
        Style::default().add_modifier(Modifier::BOLD),
    ))
}

/// Draws the four sections into `area`, one cell in from the left.
pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let snap = &app.snap;
    let width = area.width as usize;
    let needs = needs(snap);
    let running = running(snap);
    let selected = app.selected(View::Now);
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
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        }
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut selected_line = 0;

    lines.push(title(format!("NEEDS YOU ({})", needs.len())));
    if needs.is_empty() {
        lines.push(Line::from(Span::styled("   Nothing needs you.", dim())));
    }
    // fixed columns: margin 1, selector 2, mark 2, id, gap 2, label 15, gap 2, age 8, key 8
    let text_w = width.saturating_sub(1 + 2 + 2 + id_w + 2 + 15 + 2 + 8 + 8).max(8);
    for (i, n) in needs.iter().enumerate() {
        if i == selected {
            selected_line = lines.len();
        }
        let mark = Mark::Needs;
        lines.push(
            Line::from(vec![
                Span::raw(format!(" {}", select_marker(i))),
                Span::styled(format!("{} ", mark.glyph()), mark.style()),
                Span::raw(format!("{}  ", fit(n.issue.as_str(), id_w))),
                Span::raw(fit(&n.label, 15)),
                Span::raw(format!("{}  ", fit(&n.text, text_w))),
                Span::raw(fit(&age(&snap.now, &n.since), 8)),
                Span::styled(n.act.hint(), Style::default().add_modifier(Modifier::BOLD)),
            ])
            .style(row_style(i)),
        );
    }
    lines.push(Line::raw(""));

    lines.push(title(format!("RUNNING ({})", running.len())));
    if running.is_empty() {
        lines.push(Line::from(Span::styled(
            "   No worker is running. Press Tab for the Board to start one.",
            dim(),
        )));
    }
    // fixed columns: margin 1, selector 2, mark 2, id, gap 2, state 15, gap 2, money 14, gap 2, elapsed 8
    let title_w = width
        .saturating_sub(1 + 2 + 2 + id_w + 2 + 15 + 2 + 14 + 2 + 8)
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
                Span::styled(format!("{} ", mark.glyph()), mark.style()),
                Span::raw(format!("{}  ", fit(r.issue.as_str(), id_w))),
                Span::raw(fit(&r.state, 15)),
                Span::raw(format!("{}  ", fit(&r.title, title_w))),
                Span::raw(format!("{}  ", fit(&money(r.cost, r.budget), 14))),
                Span::raw(age(&snap.now, &r.since)),
            ])
            .style(row_style(i)),
        );
    }
    lines.push(Line::raw(""));

    lines.push(up_next_line(snap, width));
    lines.push(done_line(snap, width));

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

fn up_next_line(snap: &Snapshot, _width: usize) -> Line<'static> {
    let next = up_next(snap);
    let head = Span::styled(
        format!(" UP NEXT ({})", next.count),
        Style::default().add_modifier(Modifier::BOLD),
    );
    if next.count == 0 {
        return Line::from(vec![
            head,
            Span::raw("   "),
            Span::styled("nothing is waiting to start", dim()),
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

fn done_line(snap: &Snapshot, width: usize) -> Line<'static> {
    let done = done_today(snap);
    let head_text = format!(" DONE TODAY ({})", done.len());
    let head = Span::styled(head_text.clone(), Style::default().add_modifier(Modifier::BOLD));
    if done.is_empty() {
        return Line::from(vec![
            head,
            Span::raw("   "),
            Span::styled("nothing closed yet today", dim()),
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
    Line::from(vec![head, Span::raw("   "), Span::styled(shown, dim())])
}
