//! The frame around every view: the header line, the tab bar, the waiting banner, the rules, the key
//! hints in the footer and the too-small gate. Nothing else draws those rows; a view draws only the
//! area [`render`] hands it.
//!
//! Rows from the top: header, tabs, the banner (only while something needs the human and the view is
//! not the Now view, which lists it itself), a rule, the view; from the bottom: a rule, then the hints.

use std::time::Duration;

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Tabs};
use ratatui::{Frame, Terminal};

use super::app::{App, View};
use super::bindings::{footer_hints, Binding};
use super::overlays::{self, Mode, Screen};
use super::snapshot::{trunc, Need, NeedSource, Snapshot};
use super::theme::{Mark, Theme};

/// Smallest terminal the views are laid out for; below it one centred line says so.
pub const MIN_WIDTH: u16 = 80;
pub const MIN_HEIGHT: u16 = 20;

const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// The braille spinner frame for the app clock: one frame per 100 ms.
fn spinner(clock: Duration) -> char {
    SPINNER[(clock.as_millis() / 100) as usize % SPINNER.len()]
}

/// A horizontal rule across `width` columns.
pub fn rule(width: u16, theme: &Theme) -> Paragraph<'static> {
    Paragraph::new(Line::styled("─".repeat(width as usize), theme.dim()))
}

/// The header: repo, branch, runtime health, workers, queue, how many things need the human and the
/// background action that runs (with a spinner).
fn header_line(app: &App) -> Line<'static> {
    let (s, t) = (&app.snap, &app.theme);
    let counts = s.run_counts();
    let runtime = match s.runtime_ok {
        Some(true) => Span::styled("runtime ok", t.ok()),
        Some(false) => Span::styled("runtime DOWN", t.danger()),
        None => Span::styled("runtime ?", t.dim()),
    };
    let mut spans = vec![
        Span::styled(" asf · ", t.bold()),
        Span::styled(s.repo_name.clone(), t.bold()),
        Span::raw(format!(" · {} · ", s.branch)),
        runtime,
    ];
    // right after the runtime so a narrow terminal never cuts it off
    if let Some(info) = &s.serve {
        spans.push(Span::raw(" · "));
        let style = if info.state.is_running() {
            t.dim()
        } else {
            t.warn()
        };
        spans.push(Span::styled(info.state.header(), style));
    }
    spans.push(Span::raw(format!(
        " · workers {}/{}",
        counts.workers, s.max_parallel
    )));
    // Reviewers have their own limit; they are named only when one runs, so a RUNNING list of
    // `workers + reviewers` rows can be read off the header.
    if counts.reviewers > 0 {
        spans.push(Span::raw(format!(" · reviewers {}", counts.reviewers)));
    }
    spans.push(Span::raw(format!(" · queue {}", s.queue.len())));
    let waiting = s.needs_you.len();
    if waiting > 0 {
        spans.push(Span::raw(" · "));
        spans.push(Span::styled(
            format!("{} {waiting} waiting", Mark::Needs.glyph()),
            t.mark(Mark::Needs),
        ));
    }
    if let Some(label) = &app.busy {
        spans.push(Span::raw(" · "));
        spans.push(Span::styled(format!("{} {label}…", spinner(app.clock)), t.busy()));
    }
    Line::from(spans)
}

/// The tab bar: `[1] Now  [2] Board ...`, the Inbox with its unread count, the open view highlighted.
fn tab_bar(app: &App) -> Tabs<'static> {
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
        .highlight_style(app.theme.tab_active())
}

/// The one-line banner naming the oldest thing that needs the human, cut to `width` with
/// ` (+K more)` always kept.
fn banner_line(needs: &[Need], width: usize, theme: &Theme) -> Line<'static> {
    let more = needs.len().saturating_sub(1);
    let suffix = if more > 0 {
        format!(" (+{more} more)")
    } else {
        String::new()
    };
    let room = width.saturating_sub(super::snapshot::text_width(&suffix));
    let mark = Mark::Needs.glyph();
    let head = match needs.first() {
        Some(n) if n.source == NeedSource::BlockedRun => trunc(
            &format!(
                "{mark} {} is waiting for your answer · a answer · f focus pane",
                n.issue
            ),
            room,
        ),
        Some(n) => {
            let prefix = match n.source {
                NeedSource::Question(_) => format!("{mark} {} asks: ", n.issue),
                _ => format!("{mark} {} {}: ", n.issue, n.label),
            };
            let left = room.saturating_sub(super::snapshot::text_width(&prefix)).max(1);
            format!("{prefix}{}", trunc(&n.text, left))
        }
        None => String::new(),
    };
    Line::styled(format!("{head}{suffix}"), theme.warn())
}

/// The hint row of a window that draws its own (the help page): `key label` pairs, the key accent +
/// bold, the label dim, three blanks apart as the mockup has them (the footer packs them closer).
pub fn window_hints_line(hints: &[&Binding], theme: &Theme) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    spans.extend(hint_spans(hints, theme, "   "));
    Line::from(spans)
}

fn hint_spans(hints: &[&Binding], theme: &Theme, gap: &'static str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (i, h) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(gap));
        }
        spans.push(Span::styled(h.key, theme.key()));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(h.label, theme.dim()));
    }
    spans
}

/// The last row: the prompt of a text input or a question, the status message, or the key hints.
fn footer(app: &App) -> Paragraph<'static> {
    let kind = app.mode().kind();
    let hints = footer_hints(app.view, kind);
    let lead = |text: String| {
        let mut spans = vec![Span::raw(text)];
        spans.extend(hint_spans(&hints, &app.theme, "  "));
        Line::from(spans)
    };
    let line = match app.mode() {
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

/// The rows the chrome leaves to the view, and where each chrome row is.
struct Rows {
    header: Rect,
    tabs: Rect,
    banner: Option<Rect>,
    /// The rule that closes the top chrome (under the banner, or under the tabs without one).
    top_rule: Rect,
    body: Rect,
    /// The rule above the key hints.
    rule: Rect,
    footer: Rect,
}

fn split(area: Rect, banner: bool) -> Rows {
    let mut constraints = vec![Constraint::Length(1), Constraint::Length(1)];
    if banner {
        constraints.push(Constraint::Length(1));
    }
    constraints.extend([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ]);
    let rows = Layout::vertical(constraints).split(area);
    let n = rows.len();
    Rows {
        header: rows[0],
        tabs: rows[1],
        banner: banner.then(|| rows[2]),
        top_rule: rows[n - 4],
        body: rows[n - 3],
        rule: rows[n - 2],
        footer: rows[n - 1],
    }
}

/// Whether the banner row exists for `snap` on `view`: something needs the human, or the loop is
/// not running. The Now view lists both itself.
fn has_banner(snap: &Snapshot, view: View) -> bool {
    view != View::Now && (!snap.needs_you.is_empty() || snap.loop_notice().is_some())
}

/// The banner row: what needs the human wins over the state of the loop.
fn banner(snap: &Snapshot, width: usize, theme: &Theme) -> Line<'static> {
    match (snap.needs_you.is_empty(), snap.loop_notice()) {
        (true, Some(notice)) => Line::styled(
            trunc(&format!("{} {notice}", Mark::Needs.glyph()), width),
            theme.warn(),
        ),
        _ => banner_line(&snap.needs_you, width, theme),
    }
}

/// Draws one frame: below [`MIN_WIDTH`]x[`MIN_HEIGHT`] only the too-small line, otherwise the chrome,
/// the active view and the open window.
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
        return;
    }
    let rows = split(area, has_banner(&app.snap, app.view));
    frame.render_widget(Paragraph::new(header_line(app)), rows.header);
    frame.render_widget(tab_bar(app), rows.tabs);
    if let Some(r) = rows.banner {
        frame.render_widget(
            Paragraph::new(banner(&app.snap, area.width as usize, &app.theme)),
            r,
        );
    }
    frame.render_widget(rule(area.width, &app.theme), rows.top_rule);
    app.render_view(frame, rows.body);
    frame.render_widget(rule(area.width, &app.theme), rows.rule);
    frame.render_widget(footer(app), rows.footer);
    let screen = Screen {
        area,
        body: rows.body,
        view: app.view,
        snap: &app.snap,
        theme: &app.theme,
        scroll_max: &app.scroll_max,
    };
    overlays::render(frame, &screen, app.mode());
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
