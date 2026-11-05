//! Rendering of an `App` into a ratatui `Frame`. Pure function of the state; tested with `TestBackend`.

use asf_core::*;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, Tabs, Wrap};
use ratatui::{Frame, Terminal};

use super::app::{board_columns, App, Mode, View, Waiting};

/// Style of a waiting marker and of the row it sits on: bold red, never colour alone (the symbol differs too).
fn blocked_style() -> Style {
    Style::default().red().bold()
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
    let waiting = app.waiting().len();
    if waiting > 0 {
        spans.push(Span::raw(" · "));
        spans.push(Span::styled(
            format!("{waiting} waiting"),
            Style::default().cyan().bold(),
        ));
    }
    Line::from(spans)
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

fn footer(app: &App) -> Paragraph<'static> {
    let text = match &app.mode {
        Mode::Input { prompt, buf, .. } => format!(" {prompt}: {buf}▏  (Enter send · Esc cancel)"),
        Mode::Confirm { what, .. } => format!(" {what} (y/n)"),
        Mode::Answer { id, .. } => format!(" answering {id}"),
        Mode::Normal => match &app.status {
            Some(s) => format!(" {s}"),
            None => " 1-6 views · j/k move · Enter detail · s start · f focus · r reply · a answer · d unset · m merge · x abort · t tick · R refresh · q quit".into(),
        },
    };
    Paragraph::new(text)
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
                let marker = match app.run_for(&i.id) {
                    Some(r) if r.state == RunState::Blocked => Span::styled("■ ? ", blocked_style()),
                    Some(_) => Span::raw("▶ "),
                    None => Span::raw(""),
                };
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
            let blocked = r.state == RunState::Blocked;
            let row = Row::new(vec![
                Cell::from(format!("{}{}", if blocked { "■ " } else { "  " }, r.issue_id)),
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
            " inbox (human) {} unread · Enter = mark read ",
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

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let waiting = app.waiting();
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
    frame.render_widget(footer(app), rows[rows.len() - 1]);
    if let Mode::Answer { id, screen, text } = &app.mode {
        answer_overlay(frame, area, id, screen, text.as_deref());
    }
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
