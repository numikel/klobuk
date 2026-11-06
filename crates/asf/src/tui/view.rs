//! Rendering of an `App` into a ratatui `Frame`. Pure function of the state; tested with `TestBackend`.

use std::time::Duration;

use asf_core::*;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, Tabs, Wrap};
use ratatui::{Frame, Terminal};

use super::app::{board_columns, App, Mode, View, Waiting};
use super::keys::{footer_hints, help_lines, HelpLine, ModeKind};
use super::now;
use super::theme::{self, Mark};

/// Smallest terminal the views are laid out for; below it one centred line says so.
pub const MIN_WIDTH: u16 = 80;
pub const MIN_HEIGHT: u16 = 20;

/// Style of a waiting marker and of the row it sits on: bold red, never colour alone (the symbol differs too).
fn blocked_style() -> Style {
    Style::default().red().bold()
}

/// Whether a run in `state` waits for the human or has gone wrong (the red `■`).
fn run_needs_you(state: RunState) -> bool {
    matches!(
        state,
        RunState::Blocked | RunState::Stalled | RunState::OverBudget | RunState::Failed
    )
}

/// The status symbol of a board card: a run decides first, then a failed or blocked merge item,
/// then the issue's own status.
fn issue_mark(app: &App, issue: &Issue) -> Mark {
    if let Some(r) = app.run_for(&issue.id) {
        return if run_needs_you(r.state) {
            Mark::Needs
        } else if issue.kind == IssueType::Review {
            Mark::Review
        } else {
            Mark::Working
        };
    }
    let failed_run = app.snap.failed_runs.iter().any(|r| r.issue_id == issue.id);
    let failed_merge = app
        .snap
        .queue
        .iter()
        .any(|m| m.issue_id == issue.id && matches!(m.state, MergeState::Failed | MergeState::Blocked));
    match issue.status {
        _ if failed_run || failed_merge => Mark::Needs,
        Status::Validating | Status::InReview => Mark::Review,
        Status::Done => Mark::Done,
        _ => Mark::Waiting,
    }
}

fn trunc(s: &str, n: usize) -> String {
    let s = s.lines().next().unwrap_or("");
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

fn header_line(app: &App) -> Line<'static> {
    let s = &app.snap;
    let rt = match s.runtime_ok {
        Some(true) => "runtime ok".green(),
        Some(false) => "runtime DOWN".red(),
        None => "runtime ?".dark_gray(),
    };
    let mut spans = vec![
        Span::raw(" asf · ").bold(),
        Span::raw(s.repo_name.clone()).bold(),
        Span::raw(format!(" · {} · ", s.branch)),
        rt,
        Span::raw(format!(
            " · workers {}/{} · queue {}",
            s.runs.len(),
            s.max_parallel,
            s.queue.len()
        )),
    ];
    // The same count as the Now view's NEEDS YOU section.
    let waiting = now::needs(s).len();
    if waiting > 0 {
        spans.push(Span::raw(" · "));
        spans.push(Span::styled(
            format!("{waiting} waiting"),
            Style::default().cyan().bold(),
        ));
    }
    if let Some(label) = &app.busy {
        spans.push(Span::raw(" · "));
        spans.push(Span::styled(
            format!("{} {label}…", spinner(app.clock)),
            Style::default().cyan().bold(),
        ));
    }
    Line::from(spans)
}

const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// The braille spinner frame for the app clock: one frame per 100 ms.
fn spinner(clock: Duration) -> char {
    SPINNER[(clock.as_millis() / 100) as usize % SPINNER.len()]
}

/// The one-line banner naming the oldest waiter, cut to `width` with ` (+K more)` always kept.
fn banner_line(waiting: &[Waiting], width: usize) -> Line<'static> {
    let more = waiting.len().saturating_sub(1);
    let suffix = if more > 0 {
        format!(" (+{more} more)")
    } else {
        String::new()
    };
    let room = width.saturating_sub(suffix.chars().count());
    let head = match waiting.first() {
        Some(Waiting::Run { issue, .. }) => trunc(
            &format!("■ {issue} is waiting for your answer · a answer · f focus pane"),
            room,
        ),
        Some(Waiting::Question { issue, body, .. }) => {
            let prefix = format!("■ {issue} asks: ");
            let left = room.saturating_sub(prefix.chars().count()).max(1);
            format!("{prefix}{}", trunc(body, left))
        }
        None => String::new(),
    };
    Line::styled(format!("{head}{suffix}"), Style::default().yellow().bold())
}

fn tabs(app: &App) -> Tabs<'static> {
    let titles: Vec<Line> = View::ALL
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let extra = match v {
                View::Inbox if !app.snap.inbox.is_empty() => format!("({})", app.snap.inbox.len()),
                _ => String::new(),
            };
            Line::from(format!("[{}] {}{}", i + 1, v.title(), extra))
        })
        .collect();
    let selected = View::ALL.iter().position(|v| *v == app.view).unwrap_or(0);
    Tabs::new(titles)
        .select(selected)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
}

/// `key action` pairs, the key bold and the action dim, two blanks apart.
fn hint_spans(view: View, kind: ModeKind) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (i, h) in footer_hints(view, kind).into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(h.key, Style::default().bold()));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(h.label, Style::default().dark_gray()));
    }
    spans
}

fn footer(app: &App) -> Paragraph<'static> {
    let kind = app.mode.kind();
    let lead = |text: String| {
        let mut spans = vec![Span::raw(text)];
        spans.extend(hint_spans(app.view, kind));
        Line::from(spans)
    };
    let line = match &app.mode {
        Mode::Input { prompt, buf, .. } => lead(format!(" {prompt}: {buf}▏  ")),
        Mode::Confirm { what, .. } => lead(format!(" {what}  ")),
        Mode::Answer { id, .. } => Line::from(format!(" answering {id}")),
        Mode::Comment { .. } | Mode::Help { .. } => lead(" ".into()),
        Mode::Normal => match &app.status {
            Some(s) => Line::from(format!(" {s}")),
            None => lead(" ".into()),
        },
    };
    Paragraph::new(line)
}

fn board(frame: &mut Frame, app: &App, area: Rect) {
    let columns = board_columns();
    let cols =
        Layout::horizontal(vec![Constraint::Ratio(1, columns.len() as u32); columns.len()]).split(area);
    let ordered = app.board_issues();
    let sel = app.selected(View::Board);
    let selected_id = ordered.get(sel).map(|i| i.id.clone());
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
                let mark = issue_mark(app, i);
                // A worker that sits on a prompt keeps its `?`: it is the one the `a` key answers.
                let blocked = app.run_for(&i.id).is_some_and(|r| r.state == RunState::Blocked);
                let marker = Span::styled(
                    format!("{} {}", mark.glyph(), if blocked { "? " } else { "" }),
                    mark.style(),
                );
                ListItem::new(Text::from(vec![
                    Line::from(vec![marker, Span::raw(i.id.to_string())]),
                    Line::from(format!("  {}", trunc(&i.title, width))),
                ]))
            })
            .collect();
        let mut state = ListState::default();
        if let Some(sid) = &selected_id {
            state.select(items.iter().position(|i| &i.id == sid));
        }
        let list = List::new(list_items)
            .block(Block::bordered().title(format!(" {st} {n} ")))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        frame.render_stateful_widget(list, *col, &mut state);
    }
}

fn detail(frame: &mut Frame, app: &App, area: Rect) {
    let Some(id) = app.selected_issue() else {
        frame.render_widget(
            Paragraph::new("").block(Block::bordered().title(" detail ")),
            area,
        );
        return;
    };
    let Some(i) = app.issue(id) else { return };
    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            Span::raw(format!("{} ", i.id)).bold(),
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
    if let Some(c) = app.snap.claims.iter().find(|c| &c.issue_id == id) {
        lines.push(Line::from(format!("claimed by {} since {}", c.worker, c.claimed_at)).dark_gray());
    }
    if let Some(r) = app.run_for(id) {
        let agent = app
            .snap
            .agents
            .get(id)
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
    if let Some(c) = app.snap.inbox.iter().rev().find(|c| &c.issue_id == id) {
        lines.push(Line::from(format!("[{}] {}: {}", c.author, c.kind, trunc(&c.body, 100))).yellow());
    }
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::bordered().title(" detail ")),
        area,
    );
}

fn agents(frame: &mut Frame, app: &App, area: Rect) {
    let rows: Vec<Row> = app
        .snap
        .runs
        .iter()
        .map(|r| {
            let agent = app
                .snap
                .agents
                .get(&r.issue_id)
                .map(|a| a.state.to_string())
                .unwrap_or_else(|| "?".into());
            let blocked = run_needs_you(r.state);
            let mark = if blocked {
                Mark::Needs
            } else if app
                .issue(&r.issue_id)
                .is_some_and(|i| i.kind == IssueType::Review)
            {
                Mark::Review
            } else {
                Mark::Working
            };
            let row = Row::new(vec![
                Cell::from(Line::from(vec![
                    Span::styled(format!("{} ", mark.glyph()), mark.style()),
                    Span::raw(r.issue_id.to_string()),
                ])),
                Cell::from(r.state.to_string()),
                Cell::from(agent),
                Cell::from(format!("${:.2}", r.cost_usd)),
                Cell::from(r.tool_calls.to_string()),
                Cell::from(r.pane_id.clone().unwrap_or_else(|| "-".into())),
                Cell::from(trunc(r.exit_reason.as_deref().unwrap_or(""), 40)),
            ]);
            if blocked {
                row.style(blocked_style())
            } else {
                row
            }
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(12),
            Constraint::Length(12),
            Constraint::Length(9),
            Constraint::Length(8),
            Constraint::Length(6),
            Constraint::Length(10),
            Constraint::Min(10),
        ],
    )
    .header(Row::new(["issue", "run", "agent", "cost", "calls", "pane", "note"]).bold())
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .block(Block::bordered().title(" agents "));
    let mut st = ratatui::widgets::TableState::default();
    st.select(Some(app.selected(View::Agents)));
    frame.render_stateful_widget(table, area, &mut st);
}

fn inbox(frame: &mut Frame, app: &App, area: Rect) {
    let width = area.width as usize;
    let items: Vec<ListItem> = app
        .snap
        .inbox
        .iter()
        .map(|c| {
            let head = format!(
                "#{} {} [{}] {} -> {}",
                c.id, c.issue_id, c.author, c.kind, c.to_lane
            );
            let body: Vec<Line> = std::iter::once(Line::from(head).bold())
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
    let mut st = ListState::default();
    st.select(Some(app.selected(View::Inbox)));
    let list = List::new(items)
        .block(Block::bordered().title(format!(
            " inbox (human) {} unread · Enter open · m mark read · M mark all ",
            app.snap.inbox.len()
        )))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    frame.render_stateful_widget(list, area, &mut st);
}

fn queue(frame: &mut Frame, app: &App, area: Rect) {
    let rows: Vec<Row> = app
        .snap
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
    .header(Row::new(["#", "issue", "state", "branch", "att", "last error"]).bold())
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .block(Block::bordered().title(" merge queue "));
    let mut st = ratatui::widgets::TableState::default();
    st.select(Some(app.selected(View::Queue)));
    frame.render_stateful_widget(table, area, &mut st);
}

fn log(frame: &mut Frame, app: &App, area: Rect) {
    let width = area.width as usize;
    let items: Vec<ListItem> = app
        .snap
        .events
        .iter()
        .map(|e| {
            let ts = e.ts.get(11..19).unwrap_or(&e.ts);
            let who = e.issue_id.as_ref().map(ToString::to_string).unwrap_or_default();
            let payload = trunc(&e.payload.to_string(), width.saturating_sub(40));
            ListItem::new(format!("{ts} {:<16} {:<8} {payload}", e.kind, who))
        })
        .collect();
    let mut st = ListState::default();
    st.select(Some(app.selected(View::Log)));
    let list = List::new(items)
        .block(Block::bordered().title(" log (newest first) "))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    frame.render_stateful_widget(list, area, &mut st);
}

fn config(frame: &mut Frame, app: &App, area: Rect) {
    let rows: Vec<Row> = app
        .snap
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
    .header(Row::new(["key", "value", "description"]).bold())
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .block(Block::bordered().title(" config · Enter = edit (applies after `asf serve` restart) "));
    let mut st = ratatui::widgets::TableState::default();
    st.select(Some(app.selected(View::Config)));
    frame.render_stateful_widget(table, area, &mut st);
}

/// Draws one frame: below [`MIN_WIDTH`]x[`MIN_HEIGHT`] only the too-small line, otherwise the view
/// and its overlays; with `NO_COLOR` (`app.color == false`) every colour is stripped afterwards.
pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        let [_, middle, _] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1), Constraint::Fill(1)]).areas(area);
        frame.render_widget(
            Paragraph::new(format!(
                "Terminal too small (need {MIN_WIDTH}x{MIN_HEIGHT}, have {}x{}).",
                area.width, area.height
            ))
            .centered(),
            middle,
        );
    } else {
        render_views(frame, app);
    }
    if !app.color {
        theme::strip_colors(frame.buffer_mut());
    }
}

fn render_views(frame: &mut Frame, app: &App) {
    let area = frame.area();
    // The Now view lists the waiters itself, so the one-line banner is for the other views.
    let waiting = if app.view == View::Now {
        Vec::new()
    } else {
        app.waiting()
    };
    let show_detail = app.detail && app.view == View::Board && area.height >= 14;
    // The banner line exists only while something waits, so the layout is stable otherwise.
    let mut constraints = vec![Constraint::Length(1), Constraint::Length(1)];
    if !waiting.is_empty() {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(if show_detail { 5 } else { 3 }));
    if show_detail {
        constraints.push(Constraint::Length(7));
    }
    constraints.push(Constraint::Length(1));
    let rows = Layout::vertical(constraints).split(area);
    frame.render_widget(Paragraph::new(header_line(app)), rows[0]);
    frame.render_widget(tabs(app), rows[1]);
    let mut next = 2;
    if !waiting.is_empty() {
        frame.render_widget(
            Paragraph::new(banner_line(&waiting, area.width as usize)),
            rows[next],
        );
        next += 1;
    }
    let main = rows[next];
    match app.view {
        View::Now => now::render(frame, app, main),
        View::Board => board(frame, app, main),
        View::Agents => agents(frame, app, main),
        View::Inbox => inbox(frame, app, main),
        View::Queue => queue(frame, app, main),
        View::Log => log(frame, app, main),
        View::Config => config(frame, app, main),
    }
    if show_detail {
        detail(frame, app, rows[next + 1]);
    }
    // Overlays leave the footer row alone so the hints stay visible under them.
    let above_footer = Rect {
        height: area.height.saturating_sub(1),
        ..area
    };
    match &app.mode {
        Mode::Answer { id, screen, text } => {
            answer_overlay(frame, above_footer, id, screen, text.as_deref());
        }
        Mode::Comment { comment, scroll } => comment_modal(frame, above_footer, app, comment, *scroll),
        Mode::Help { scroll } => help_view(frame, above_footer, app, *scroll),
        _ => {}
    }
    frame.render_widget(footer(app), rows[rows.len() - 1]);
}

/// Greedy word wrap to `width` columns, so the scroll range is known exactly. Blank lines survive,
/// leading indentation is kept, and a word longer than a line is split.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for raw in text.lines() {
        if raw.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let indent = (raw.len() - raw.trim_start().len()).min(width / 2);
        let mut cur = " ".repeat(indent);
        let mut cur_len = indent;
        let mut fresh = true; // no word on this line yet
        for word in raw.split_whitespace() {
            let chars: Vec<char> = word.chars().collect();
            for piece in chars.chunks(width) {
                let pl = piece.len();
                if !fresh && cur_len + 1 + pl > width {
                    out.push(std::mem::replace(&mut cur, " ".repeat(indent)));
                    cur_len = indent;
                    fresh = true;
                }
                if fresh && cur_len + pl > width {
                    cur.clear(); // a full-width piece leaves no room for the indent
                    cur_len = 0;
                }
                if !fresh {
                    cur.push(' ');
                    cur_len += 1;
                }
                cur.extend(piece);
                cur_len += pl;
                fresh = false;
            }
        }
        if !fresh {
            out.push(cur);
        }
    }
    out
}

/// Offset of the first visible line, clamped to the scrollable range; publishes the range to the
/// app so that `j` stops at the end.
fn clamp_scroll(app: &App, total: usize, visible: usize, scroll: u16) -> usize {
    let max = total.saturating_sub(visible).min(u16::MAX as usize);
    app.scroll_max.set(max as u16);
    (scroll as usize).min(max)
}

/// The Inbox modal: header line fixed on top, the wrapped body scrolling below it.
fn comment_modal(frame: &mut Frame, area: Rect, app: &App, c: &Comment, scroll: u16) {
    let popup = area.centered(Constraint::Percentage(80), Constraint::Percentage(80));
    let block = Block::bordered()
        .title(" comment ")
        .title_bottom(" j/k scroll · Esc close (nothing is marked read) ")
        .border_style(Style::default().yellow().bold());
    let inner = block.inner(popup);
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);
    let [head, _gap, body] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(0)]).areas(inner);
    let header = format!(
        "#{} {} [{}] {} -> {}",
        c.id, c.issue_id, c.author, c.kind, c.to_lane
    );
    frame.render_widget(Paragraph::new(Line::from(header).bold()), head);
    let lines = wrap_text(&c.body, body.width as usize);
    let top = clamp_scroll(app, lines.len(), body.height as usize, scroll);
    let shown: Vec<Line> = lines
        .into_iter()
        .skip(top)
        .take(body.height as usize)
        .map(Line::from)
        .collect();
    frame.render_widget(Paragraph::new(shown), body);
}

/// The full-screen key reference, generated from the binding table.
fn help_view(frame: &mut Frame, area: Rect, app: &App, scroll: u16) {
    frame.render_widget(Clear, area);
    let [title, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    frame.render_widget(
        Paragraph::new(" Keys · j/k scroll · Esc, ? or q close")
            .style(Style::default().add_modifier(Modifier::REVERSED)),
        title,
    );
    let lines: Vec<Line> = help_lines()
        .into_iter()
        .map(|l| match l {
            HelpLine::Heading(g) => Line::styled(format!(" {g}"), Style::default().cyan().bold()),
            HelpLine::Item { key, help } => Line::from(vec![
                Span::styled(format!("  {key:<20}"), Style::default().bold()),
                Span::raw(help),
            ]),
            HelpLine::Blank => Line::raw(""),
        })
        .collect();
    let top = clamp_scroll(app, lines.len(), body.height as usize, scroll);
    let shown: Vec<Line> = lines.into_iter().skip(top).take(body.height as usize).collect();
    frame.render_widget(Paragraph::new(shown), body);
}

/// The centred answer modal: the last lines of the worker pane, with the key hints (or the reply
/// being typed) on the bottom border so they stay visible however long the screen is.
fn answer_overlay(frame: &mut Frame, area: Rect, id: &IssueId, screen: &str, text: Option<&str>) {
    let popup = area.centered(
        Constraint::Percentage(80),
        Constraint::Length(24.min(area.height)),
    );
    let lines: Vec<&str> = screen.lines().collect();
    let tail = &lines[lines.len().saturating_sub(20)..];
    let body: Vec<Line> = if tail.is_empty() {
        vec![Line::from("(reading the pane...)").dark_gray()]
    } else {
        tail.iter().map(|l| Line::from((*l).to_string())).collect()
    };
    let hint = match text {
        None => " 1-9 / Enter / Up / Down send keys · y/n · t type text · Esc close ".to_string(),
        Some(buf) => format!(" reply: {buf}▏ (Enter send · Esc close) "),
    };
    let block = Block::bordered()
        .title(format!(" answer {id} "))
        .title_bottom(hint)
        .border_style(Style::default().yellow().bold());
    frame.render_widget(Clear, popup);
    frame.render_widget(Paragraph::new(body).block(block), popup);
}

/// Renders one frame into a `TestBackend` and returns the cell buffer (symbols and styles).
pub fn render_to_buffer(app: &App, width: u16, height: u16) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test backend");
    terminal.draw(|f| render(f, app)).expect("draw");
    terminal.backend().buffer().clone()
}

/// Renders one frame into a `TestBackend` and returns it as plain text (no ANSI).
pub fn render_to_text(app: &App, width: u16, height: u16) -> String {
    let buf = render_to_buffer(app, width, height);
    let mut out = String::new();
    for y in 0..height {
        let mut line = String::new();
        for x in 0..width {
            line.push_str(buf[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}
