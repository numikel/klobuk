//! The Inbox view: what needs the human, then what is only worth reading.
//!
//! Two sections. NEEDS YOU is the snapshot's `needs_you` list, one item per row with its action keys
//! under it; a gate-failure BLOCKER is summarised in two lines instead of the raw output. FOR YOUR
//! INFORMATION is every other unread comment, newest first. `A` swaps both for one flat list of
//! everything unread.
//!
//! The row model (`rows_in`, `info`, `summarize`) is a pure function of a [`Snapshot`] and the view
//! state, so the key handling walks the same rows the renderer draws.

use asf_core::*;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::tui::app::{Action, View};
use crate::tui::bindings::*;
use crate::tui::overlays::InputTarget;
use crate::tui::snapshot::{
    age, answer_blocked, first_line, fit, move_selection, reply_input, text_width, trunc, Need, NeedSource,
    Snapshot,
};
use crate::tui::theme::{Mark, Theme};

#[derive(Debug, Clone, Default)]
pub struct State {
    pub selected: usize,
    /// `A`: the flat list of everything unread instead of the two sections.
    pub everything: bool,
}

/// One selectable row.
#[derive(Debug, Clone)]
pub enum Row {
    /// An item of the NEEDS YOU section.
    Need(Need),
    /// A comment: a row of FOR YOUR INFORMATION, or of the flat list.
    Comment(Comment),
}

impl Row {
    pub fn issue(&self) -> &IssueId {
        match self {
            Row::Need(n) => &n.issue,
            Row::Comment(c) => &c.issue_id,
        }
    }
}

const INBOX: &[View] = &[View::Inbox];

pub const BINDINGS: &[Binding] = &[
    b(
        "j / k",
        "move",
        HELP_MOVE,
        Group::Everywhere,
        INBOX,
        NORMAL,
        Slot::Hint,
    ),
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
        "a",
        "answer",
        HELP_ANSWER,
        Group::NeedsYou,
        INBOX,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "m",
        "mark issue read",
        "mark this issue's messages as read",
        Group::Inbox,
        INBOX,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "M",
        "mark all read",
        "mark all as read (asks first)",
        Group::Inbox,
        INBOX,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "A",
        "everything",
        "list every unread comment, oldest\nfirst; press again to go back",
        Group::Inbox,
        INBOX,
        NORMAL,
        Slot::None,
    ),
    b(
        "v",
        "verify",
        "Inbox, on a merge row: try it\nagain after a fix",
        Group::NeedsYou,
        INBOX,
        NORMAL,
        Slot::None,
    ),
    b(
        "r",
        "reply",
        HELP_REPLY,
        Group::NeedsYou,
        INBOX,
        NORMAL,
        Slot::None,
    ),
];

// ---------------------------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------------------------

/// The comment that stands for `need`: the question itself, or the newest BLOCKER of the issue (it
/// carries the full gate output, or the worker screen of a blocked run).
fn need_comment<'a>(snap: &'a Snapshot, need: &Need) -> Option<&'a Comment> {
    match need.source {
        NeedSource::Question(id) => snap.questions.iter().chain(&snap.inbox).find(|c| c.id == id),
        _ => snap
            .blockers
            .iter()
            .chain(&snap.inbox)
            .filter(|c| c.kind == CommentType::Blocker && c.issue_id == need.issue)
            .max_by_key(|c| c.id),
    }
}

/// The unread comments that are not already a NEEDS YOU item, newest first.
pub fn info(snap: &Snapshot) -> Vec<&Comment> {
    let shown: Vec<i64> = snap
        .needs_you
        .iter()
        .filter_map(|n| need_comment(snap, n))
        .map(|c| c.id)
        .collect();
    let mut out: Vec<&Comment> = snap.inbox.iter().filter(|c| !shown.contains(&c.id)).collect();
    out.sort_by(|a, b| (&b.created_at, b.id).cmp(&(&a.created_at, a.id)));
    out
}

/// The rows on screen: the NEEDS YOU items then the information, or with `A` every unread comment,
/// oldest first.
pub fn rows_in(snap: &Snapshot, state: &State) -> Vec<Row> {
    if state.everything {
        let mut all: Vec<&Comment> = snap.inbox.iter().collect();
        all.sort_by(|a, b| (&a.created_at, a.id).cmp(&(&b.created_at, b.id)));
        all.into_iter().cloned().map(Row::Comment).collect()
    } else {
        snap.needs_you
            .iter()
            .cloned()
            .map(Row::Need)
            .chain(info(snap).into_iter().cloned().map(Row::Comment))
            .collect()
    }
}

/// Number of selectable rows in the default layout (NEEDS YOU, then the information).
pub fn rows(snap: &Snapshot) -> usize {
    snap.needs_you.len() + info(snap).len()
}

/// Number of selectable rows in the layout `state` asks for.
fn row_count(snap: &Snapshot, state: &State) -> usize {
    if state.everything {
        snap.inbox.len()
    } else {
        rows(snap)
    }
}

/// The selection, kept inside the rows.
pub fn selected(snap: &Snapshot, state: &State) -> usize {
    state.selected.min(row_count(snap, state).saturating_sub(1))
}

pub fn selected_row(snap: &Snapshot, state: &State) -> Option<Row> {
    rows_in(snap, state).into_iter().nth(selected(snap, state))
}

pub fn selected_issue(snap: &Snapshot, state: &State) -> Option<IssueId> {
    selected_row(snap, state).map(|r| r.issue().clone())
}

/// What a row of `issue` acts on: the issue's oldest open NEEDS YOU item.
fn need_of<'a>(snap: &'a Snapshot, row: &'a Row) -> Option<&'a Need> {
    match row {
        Row::Need(n) => Some(n),
        Row::Comment(c) => snap.needs_you.iter().find(|n| n.issue == c.issue_id),
    }
}

// ---------------------------------------------------------------------------------------------
// What a row says
// ---------------------------------------------------------------------------------------------

/// The two lines that stand for a gate-failure BLOCKER.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// `Merge failed: gate `test` after rebase` or `Verification failed: gate `test``.
    pub title: String,
    /// The failed tests, or the first error line; `None` when the gate printed nothing.
    pub detail: Option<String>,
}

/// The first line of a gate's section in a BLOCKER body (`gate `test` failed (exit …)`).
fn gate_name(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("gate `")?;
    let name = &rest[..rest.find('`')?];
    rest[name.len()..].starts_with("` failed").then_some(name)
}

/// The names listed under `failures:` (libtest prints them indented, one per line), each once.
fn failed_tests(output: &[&str]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut i = 0;
    while i < output.len() {
        if output[i].trim() != "failures:" {
            i += 1;
            continue;
        }
        i += 1;
        while let Some(line) = output.get(i) {
            let name = line.trim();
            if !line.starts_with(char::is_whitespace) || name.is_empty() || name.contains(char::is_whitespace)
            {
                break;
            }
            if !names.iter().any(|n| n == name) {
                names.push(name.to_string());
            }
            i += 1;
        }
    }
    names
}

/// The first line of `output` that looks like an error, else its first line.
fn first_error_line(output: &[&str]) -> Option<String> {
    let lines = || output.iter().map(|l| l.trim()).filter(|l| !l.is_empty());
    lines()
        .find(|l| {
            let low = l.to_lowercase();
            low.starts_with("error")
                || low.contains("error:")
                || low.contains("error[")
                || low.contains("panicked at")
        })
        .or_else(|| lines().next())
        .map(str::to_string)
}

/// Summarises a BLOCKER body that reports a failed gate: which gate, and which tests (or which error).
/// `merge` is true for a failure of the merge queue (after the rebase), false for the verification
/// of a run. `None` when the body names no gate.
pub fn gate_summary(body: &str, merge: bool) -> Option<Summary> {
    let lines: Vec<&str> = body.lines().collect();
    let at = lines.iter().position(|l| gate_name(l).is_some())?;
    let name = gate_name(lines[at])?;
    // the gate's own output: up to the next gate or the closing "Fix …" advice
    let output: Vec<&str> = lines[at + 1..]
        .iter()
        .copied()
        .take_while(|l| {
            gate_name(l).is_none() && !l.starts_with("Fix and") && !l.starts_with("Fix in the worktree")
        })
        .collect();
    let tests = failed_tests(&output);
    let detail = if tests.is_empty() {
        first_error_line(&output)
    } else {
        let n = tests.len();
        Some(format!(
            "{n} test{} failed: {}",
            if n == 1 { "" } else { "s" },
            tests.join(", ")
        ))
    };
    let title = if merge {
        format!("Merge failed: gate `{name}` after rebase")
    } else {
        format!("Verification failed: gate `{name}`")
    };
    Some(Summary { title, detail })
}

/// The one or two lines of a NEEDS YOU item: a gate failure is summarised from its BLOCKER (or from
/// the error the merge queue / the run kept), anything else shows the text of the item.
pub fn summarize(snap: &Snapshot, need: &Need) -> Summary {
    let merge = matches!(need.source, NeedSource::Merge(_));
    if merge || need.source == NeedSource::Run(RunState::Failed) {
        let kept = if merge {
            snap.queue
                .iter()
                .find(|m| {
                    m.issue_id == need.issue && matches!(m.state, MergeState::Failed | MergeState::Blocked)
                })
                .and_then(|m| m.last_error.as_deref())
        } else {
            snap.failed_runs
                .iter()
                .find(|r| r.issue_id == need.issue)
                .and_then(|r| r.exit_reason.as_deref())
        };
        let found = need_comment(snap, need)
            .map(|c| c.body.as_str())
            .into_iter()
            .chain(kept)
            .find_map(|body| gate_summary(body, merge));
        if let Some(summary) = found {
            return summary;
        }
    }
    Summary {
        title: need.text.clone(),
        detail: None,
    }
}

/// `QUESTION` for what waits for an answer, `BLOCKER` for what went wrong.
fn need_kind(need: &Need) -> &'static str {
    match need.source {
        NeedSource::BlockedRun | NeedSource::Question(_) => "QUESTION",
        NeedSource::Run(_) | NeedSource::Merge(_) => "BLOCKER",
    }
}

/// One piece of the dim line under a NEEDS YOU item.
enum Part {
    Text(&'static str),
    /// A key and what it does.
    Key(&'static str, &'static str),
}

/// What to do about `need` and the keys that do it (`Enter` only when there is a comment to open).
fn hint_parts(snap: &Snapshot, need: &Need) -> Vec<Part> {
    let enter = |label| need_comment(snap, need).map(|_| Part::Key("Enter", label));
    let parts: Vec<Option<Part>> = match need.source {
        NeedSource::BlockedRun => vec![
            Some(Part::Text("asked in the worker pane")),
            Some(Part::Key("a", "answer")),
        ],
        NeedSource::Question(_) => vec![enter("full question"), Some(Part::Key("a", "answer"))],
        NeedSource::Run(RunState::Stalled) => vec![
            enter("full comment"),
            Some(Part::Key("a", "answer")),
            Some(Part::Key("r", "reply to the worker")),
        ],
        NeedSource::Run(RunState::OverBudget) => vec![
            enter("full comment"),
            Some(Part::Text("abort it in the Now view (1, then x)")),
            Some(Part::Key("r", "reply to the worker")),
        ],
        NeedSource::Run(_) | NeedSource::Merge(_) => vec![
            enter("full output"),
            Some(Part::Key("v", "verify again")),
            Some(Part::Key("r", "reply to the worker")),
        ],
    };
    parts.into_iter().flatten().collect()
}

/// The key that handles `need` here, for the message a wrong key gets.
fn takes(need: &Need) -> &'static str {
    match need.source {
        NeedSource::BlockedRun | NeedSource::Question(_) | NeedSource::Run(RunState::Stalled) => "a answer",
        NeedSource::Run(RunState::OverBudget) => "x in the Now view (abort)",
        NeedSource::Run(_) | NeedSource::Merge(_) => "v verify again",
    }
}

/// `feat/asf-24 → main at 9c1e4aa` for the FYI the merge queue posts (`<branch> merged into <base> at <sha> …`).
fn merged_text(body: &str) -> Option<String> {
    let (branch, rest) = first_line(body)?.split_once(" merged into ")?;
    let (base, rest) = rest.split_once(" at ")?;
    let sha: String = rest.split_whitespace().next()?.chars().take(7).collect();
    Some(format!("{branch} → {base} at {sha}"))
}

/// The symbol of an information row, by kind: `✓` merged, `◆` DECISION, `●` STATUS.
fn info_mark(c: &Comment) -> Mark {
    match c.kind {
        CommentType::Fyi if merged_text(&c.body).is_some() => Mark::Done,
        CommentType::Decision | CommentType::Handoff => Mark::Review,
        CommentType::Status | CommentType::Answer => Mark::Working,
        CommentType::Question | CommentType::Blocker => Mark::Needs,
        CommentType::Fyi => Mark::Waiting,
    }
}

fn info_kind(c: &Comment) -> String {
    if c.kind == CommentType::Fyi && merged_text(&c.body).is_some() {
        "merged".into()
    } else {
        c.kind.to_string()
    }
}

fn info_text(c: &Comment) -> String {
    if c.kind == CommentType::Fyi {
        if let Some(t) = merged_text(&c.body) {
            return t;
        }
    }
    first_line(&c.body).unwrap_or("(empty)").to_string()
}

// ---------------------------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------------------------

/// Where the columns of a row start.
struct Cols {
    id_w: usize,
    /// First column of the text (and of the lines under it).
    text_col: usize,
    text_w: usize,
}

const KIND_W: usize = 8;
const AGE_W: usize = 6;

impl Cols {
    fn new(width: usize, id_w: usize) -> Cols {
        // margin 1, selector 2, mark 2, id, gap 2, kind 8, gap 2, age 6, gap 2
        let text_col = 1 + 2 + 2 + id_w + 2 + KIND_W + 2 + AGE_W + 2;
        Cols {
            id_w,
            text_col,
            text_w: width.saturating_sub(text_col).max(1),
        }
    }
}

/// Cuts `spans` to `max` columns; the span that does not fit ends in `…`.
fn clip(spans: Vec<Span<'static>>, max: usize) -> Vec<Span<'static>> {
    let mut used = 0;
    let mut out = Vec::new();
    for span in spans {
        let w = text_width(&span.content);
        if used + w <= max {
            used += w;
            out.push(span);
            continue;
        }
        let left = max - used;
        if left > 0 {
            out.push(Span::styled(trunc(&span.content, left), span.style));
        }
        break;
    }
    out
}

/// `title` on the left, the `A` toggle (key and what it does) at the right end.
fn title_line(
    title: &str,
    toggle: Option<(&'static str, String)>,
    width: usize,
    theme: &Theme,
) -> Line<'static> {
    let left = format!(" {title}");
    let mut spans = vec![Span::styled(left.clone(), theme.bold())];
    if let Some((key, label)) = toggle {
        let label = format!(" {label}");
        let right = text_width(key) + text_width(&label);
        if width >= text_width(&left) + right + 2 {
            spans.push(Span::raw(" ".repeat(width - text_width(&left) - right - 1)));
            spans.push(Span::styled(key, theme.key()));
            spans.push(Span::styled(label, theme.dim()));
        }
    }
    Line::from(spans)
}

/// The first line of a row: selector, symbol, issue id, kind, age and the text.
fn head_line(
    selected: bool,
    mark: Mark,
    (id, kind, age, text): (&str, &str, &str, &str),
    cols: &Cols,
    theme: &Theme,
) -> Line<'static> {
    Line::from(vec![
        Span::raw(format!(" {}", if selected { "▸ " } else { "  " })),
        Span::styled(format!("{} ", mark.glyph()), theme.mark(mark)),
        Span::styled(format!("{}  ", fit(id, cols.id_w)), theme.bold()),
        Span::raw(format!("{}  ", fit(kind, KIND_W))),
        Span::raw(format!("{age:>AGE_W$}  ")),
        Span::raw(trunc(text, cols.text_w)),
    ])
}

/// A line under the first one, starting in the text column.
fn under_line(spans: Vec<Span<'static>>, cols: &Cols) -> Line<'static> {
    let mut all = vec![Span::raw(" ".repeat(cols.text_col))];
    all.extend(clip(spans, cols.text_w));
    Line::from(all)
}

/// The dim line with what to do and the keys that do it, key letters accent + bold.
fn hint_line(parts: &[Part], cols: &Cols, theme: &Theme) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", theme.dim()));
        }
        match part {
            Part::Text(text) => spans.push(Span::styled(*text, theme.dim())),
            Part::Key(key, label) => {
                spans.push(Span::styled(*key, theme.key()));
                spans.push(Span::styled(format!(" {label}"), theme.dim()));
            }
        }
    }
    under_line(spans, cols)
}

fn id_width(rows: &[Row]) -> usize {
    rows.iter()
        .map(|r| text_width(r.issue().as_str()))
        .max()
        .unwrap_or(0)
        .max(6)
}

/// The lines of one NEEDS YOU item.
fn need_lines(
    snap: &Snapshot,
    need: &Need,
    selected: bool,
    cols: &Cols,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let summary = summarize(snap, need);
    let since = age(&snap.now, &need.since);
    let mut lines = vec![head_line(
        selected,
        Mark::Needs,
        (need.issue.as_str(), need_kind(need), &since, &summary.title),
        cols,
        theme,
    )];
    if let Some(detail) = summary.detail {
        lines.push(under_line(vec![Span::raw(detail)], cols));
    }
    lines.push(hint_line(&hint_parts(snap, need), cols, theme));
    lines
}

fn comment_line(snap: &Snapshot, c: &Comment, selected: bool, cols: &Cols, theme: &Theme) -> Line<'static> {
    head_line(
        selected,
        info_mark(c),
        (
            c.issue_id.as_str(),
            &info_kind(c),
            &age(&snap.now, &c.created_at),
            &info_text(c),
        ),
        cols,
        theme,
    )
}

fn dim_line(text: &str, theme: &Theme) -> Line<'static> {
    Line::styled(text.to_string(), theme.dim())
}

/// Draws the two sections (or the flat list) into `area`, scrolled so the selected item stays visible.
pub fn render(frame: &mut Frame, area: Rect, snap: &Snapshot, state: &State, theme: &Theme) {
    let width = area.width as usize;
    if snap.needs_you.is_empty() && snap.inbox.is_empty() {
        frame.render_widget(
            Paragraph::new(dim_line(
                " Nothing needs you and nothing new to read. Questions, blockers and results from the workers appear here.",
                theme,
            )),
            area,
        );
        return;
    }
    let rows = rows_in(snap, state);
    let cols = Cols::new(width, id_width(&rows));
    let selected = selected(snap, state);
    let mut lines: Vec<Line<'static>> = Vec::new();
    // the lines of the selected item, so it can be highlighted and kept on screen
    let mut chosen = (0, 0);
    let mut push_item = |lines: &mut Vec<Line<'static>>, i: usize, item: Vec<Line<'static>>| {
        let start = lines.len();
        let paint = |l: Line<'static>| {
            if i == selected {
                l.style(theme.selected())
            } else {
                l
            }
        };
        lines.extend(item.into_iter().map(paint));
        if i == selected {
            chosen = (start, lines.len() - 1);
        }
    };
    let blank = || Line::raw("");

    if state.everything {
        lines.push(title_line(
            &format!("EVERYTHING UNREAD ({})", rows.len()),
            Some(("A", "back to the summary".into())),
            width,
            theme,
        ));
        lines.push(blank());
        if rows.is_empty() {
            lines.push(dim_line("   Nothing unread.", theme));
        }
        for (i, row) in rows.iter().enumerate() {
            if let Row::Comment(c) = row {
                push_item(
                    &mut lines,
                    i,
                    vec![comment_line(snap, c, i == selected, &cols, theme)],
                );
            }
        }
    } else {
        lines.push(title_line(
            &format!("NEEDS YOU ({})", snap.needs_you.len()),
            Some(("A", format!("show everything ({})", snap.inbox.len()))),
            width,
            theme,
        ));
        lines.push(blank());
        if snap.needs_you.is_empty() {
            lines.push(dim_line("   Nothing needs you.", theme));
            lines.push(blank());
        }
        for (i, row) in rows.iter().enumerate() {
            match row {
                Row::Need(n) => {
                    push_item(&mut lines, i, need_lines(snap, n, i == selected, &cols, theme));
                    lines.push(blank());
                }
                Row::Comment(_) => break,
            }
        }
        let unread: Vec<&Row> = rows.iter().filter(|r| matches!(r, Row::Comment(_))).collect();
        lines.push(Line::from(Span::styled(
            format!(" FOR YOUR INFORMATION ({} unread)", unread.len()),
            theme.bold(),
        )));
        lines.push(blank());
        if unread.is_empty() {
            lines.push(dim_line("   Nothing new to read.", theme));
        }
        for (i, row) in rows.iter().enumerate().skip(snap.needs_you.len()) {
            if let Row::Comment(c) = row {
                push_item(
                    &mut lines,
                    i,
                    vec![comment_line(snap, c, i == selected, &cols, theme)],
                );
            }
        }
    }

    let height = area.height as usize;
    let top = if lines.len() > height {
        (chosen.1 + 1)
            .saturating_sub(height)
            .min(lines.len() - height)
            .min(chosen.0)
    } else {
        0
    };
    let shown: Vec<Line> = lines.into_iter().skip(top).take(height).collect();
    frame.render_widget(Paragraph::new(shown), area);
}

// ---------------------------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------------------------

pub fn handle_key(key: KeyEvent, snap: &Snapshot, state: &mut State) -> Action {
    if let Some(i) = move_selection(&key, state.selected, row_count(snap, state)) {
        state.selected = i;
        return Action::None;
    }
    match key.code {
        KeyCode::Char('A') => {
            state.everything = !state.everything;
            state.selected = 0;
            Action::None
        }
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
        KeyCode::Char('m') => selected_issue(snap, state).map_or(Action::None, Action::MarkRead),
        _ => {
            let Some(row) = selected_row(snap, state) else {
                return Action::None;
            };
            match key.code {
                KeyCode::Enter => open(snap, &row),
                KeyCode::Char('a') => answer(snap, &row),
                KeyCode::Char('v') => verify(snap, &row),
                KeyCode::Char('r') => reply_input(row.issue().clone()),
                _ => Action::None,
            }
        }
    }
}

/// `Enter`: the full comment behind the row.
fn open(snap: &Snapshot, row: &Row) -> Action {
    match row {
        Row::Comment(c) => Action::OpenComment(c.clone()),
        Row::Need(n) => need_comment(snap, n).cloned().map_or_else(
            || Action::Status(format!("{}: there is no comment to open for this item", n.issue)),
            Action::OpenComment,
        ),
    }
}

/// `a`: a blocked or stalled worker gets the answer window (a reply moves a stalled run back to
/// working), a QUESTION comment a typed answer. A row of a plain comment acts on the issue's open item.
fn answer(snap: &Snapshot, row: &Row) -> Action {
    match need_of(snap, row) {
        Some(n) => match n.source {
            NeedSource::BlockedRun | NeedSource::Run(RunState::Stalled) => {
                Action::OpenAnswer(n.issue.clone())
            }
            NeedSource::Question(question) => Action::OpenInput {
                prompt: format!("answer {} (#{question})", n.issue),
                buf: String::new(),
                target: InputTarget::Answer(n.issue.clone(), question),
            },
            _ => Action::Status(format!(
                "{}: a answers a question; this row takes {}",
                n.issue,
                takes(n)
            )),
        },
        None => answer_blocked(snap, row.issue().clone()),
    }
}

/// `v`: rerun the verification of a failed run, or the merge of a failed or blocked merge item (which
/// runs the gates again). `asf run verify` refuses a worker that is still active, so those rows say so.
fn verify(snap: &Snapshot, row: &Row) -> Action {
    match need_of(snap, row) {
        Some(n) if n.source == NeedSource::Run(RunState::Failed) => Action::Verify(n.issue.clone()),
        Some(n) if matches!(n.source, NeedSource::Merge(_)) => Action::Retry(n.issue.clone()),
        Some(n) => Action::Status(format!(
            "{}: v checks a failed run or merge again; this row takes {}",
            n.issue,
            takes(n)
        )),
        None => Action::Status(format!(
            "{}: v checks a failed run or merge again; nothing to check here",
            row.issue()
        )),
    }
}
