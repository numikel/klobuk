//! The windows that sit on top of the views: the answer overlay, the help view, the Inbox comment
//! modal, the confirmation and the one-line text input. Each is a [`Mode`] on the app's overlay stack;
//! [`handle_key`] is its pure key handling and [`render`] draws it.
//!
//! Confirmation and text input have no box: their prompt is the footer line, which `chrome.rs` draws.

use std::cell::Cell;

use asf_core::*;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

use super::app::{Action, View};
use super::bindings::{b, footer_hints, Binding, Group, ModeKind, Slot, ANY};
use super::chrome;
use super::snapshot::{text_width, trunc, wrap_text, Snapshot};
use super::theme::{Mark, Theme};

/// What the input line is typing for.
#[derive(Debug, Clone, PartialEq)]
pub enum InputTarget {
    Reply(IssueId),
    Config(String),
    /// An answer to QUESTION comment `.1` on the issue.
    Answer(IssueId, i64),
}

/// One window on the overlay stack.
#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    /// No window: the keyboard talks to the views. Never stored on the stack.
    Normal,
    Input {
        prompt: String,
        buf: String,
        target: InputTarget,
    },
    Confirm {
        what: String,
        action: Box<Action>,
    },
    /// The answer overlay for a blocked run. `text` is `Some` while a free-text reply is typed.
    Answer {
        id: IssueId,
        /// Last lines of the worker pane (empty until the peek arrives).
        screen: String,
        text: Option<String>,
    },
    /// The Inbox modal with one comment in full, scrolled `scroll` lines down.
    Comment {
        comment: Comment,
        scroll: u16,
    },
    /// The key reference page.
    Help {
        scroll: u16,
    },
}

impl Mode {
    pub fn kind(&self) -> ModeKind {
        match self {
            Mode::Normal => ModeKind::Normal,
            Mode::Input { .. } => ModeKind::Input,
            Mode::Confirm { .. } => ModeKind::Confirm,
            Mode::Answer { text: None, .. } => ModeKind::Answer,
            Mode::Answer { text: Some(_), .. } => ModeKind::AnswerText,
            Mode::Comment { .. } => ModeKind::Comment,
            Mode::Help { .. } => ModeKind::Help,
        }
    }
}

use ModeKind as M;

/// The keys of the windows themselves (typing and the question are in the global table).
pub const BINDINGS: &[Binding] = &[
    // ---- Help page ----
    // The page's own hint row is "j/k scroll   Esc close   ? close" (the mockup), so the two closing
    // keys are two hints.
    b(
        "j/k",
        "scroll",
        "scroll the key reference",
        Group::Window,
        ANY,
        &[M::Help],
        Slot::Hint,
    ),
    b(
        "Esc",
        "close",
        "close the key reference",
        Group::Window,
        ANY,
        &[M::Help],
        Slot::Hint,
    ),
    b(
        "?",
        "close",
        "close the key reference",
        Group::Window,
        ANY,
        &[M::Help],
        Slot::Hint,
    ),
    b(
        "PageUp / PageDown",
        "page",
        "scroll the key reference by ten lines",
        Group::Window,
        ANY,
        &[M::Help],
        Slot::None,
    ),
    b(
        "g / Home",
        "top",
        "jump to the top of the key reference",
        Group::Window,
        ANY,
        &[M::Help],
        Slot::None,
    ),
    b(
        "G / End",
        "bottom",
        "jump to the bottom of the key reference",
        Group::Window,
        ANY,
        &[M::Help],
        Slot::None,
    ),
    // ---- Inbox comment ----
    b(
        "j / k",
        "scroll",
        "scroll the open comment",
        Group::Window,
        ANY,
        &[M::Comment],
        Slot::Hint,
    ),
    b(
        "Esc",
        "close",
        "close the open comment (nothing is marked read)",
        Group::Window,
        ANY,
        &[M::Comment],
        Slot::Hint,
    ),
    b(
        "PageUp / PageDown",
        "page",
        "scroll the open comment by ten lines",
        Group::Window,
        ANY,
        &[M::Comment],
        Slot::None,
    ),
    b(
        "g / Home",
        "top",
        "jump to the top of the open comment",
        Group::Window,
        ANY,
        &[M::Comment],
        Slot::None,
    ),
    b(
        "G / End",
        "bottom",
        "jump to the bottom of the open comment",
        Group::Window,
        ANY,
        &[M::Comment],
        Slot::None,
    ),
    // ---- Answer overlay ----
    b(
        "1 - 9",
        "send",
        "send that number key to the worker",
        Group::Window,
        ANY,
        &[M::Answer],
        Slot::Hint,
    ),
    b(
        "Enter / Up / Down",
        "send",
        "send that key to the worker",
        Group::Window,
        ANY,
        &[M::Answer],
        Slot::Hint,
    ),
    b(
        "y / n",
        "send",
        "send y or n to the worker",
        Group::Window,
        ANY,
        &[M::Answer],
        Slot::Hint,
    ),
    b(
        "t",
        "type",
        "type a free-text reply instead",
        Group::Window,
        ANY,
        &[M::Answer],
        Slot::Hint,
    ),
    b(
        "Esc",
        "close",
        "close the overlay without sending anything",
        Group::Window,
        ANY,
        &[M::Answer],
        Slot::Hint,
    ),
    b(
        "Enter",
        "send",
        "send the typed reply",
        Group::Window,
        ANY,
        &[M::AnswerText],
        Slot::Hint,
    ),
    b(
        "Esc",
        "close",
        "close the overlay without sending",
        Group::Window,
        ANY,
        &[M::AnswerText],
        Slot::Hint,
    ),
];

// ---------------------------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------------------------

/// Ctrl+C (and only that, not the AltGr Ctrl+Alt+C of a Polish layout).
pub fn is_ctrl_c(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::ALT)
        && matches!(key.code, KeyCode::Char('c' | 'C'))
}

/// A Ctrl or Alt chord: never text, never a plain binding.
pub fn is_chord(key: &KeyEvent) -> bool {
    key.modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

/// Deletes the last word and the blanks after it, like readline's Ctrl+W.
fn delete_word(buf: &mut String) {
    while buf.ends_with(char::is_whitespace) {
        buf.pop();
    }
    while buf.chars().last().is_some_and(|c| !c.is_whitespace()) {
        buf.pop();
    }
}

/// Applies one key to a single-line text buffer. Ctrl+U clears, Ctrl+W deletes a word, and a
/// Ctrl or Alt chord never inserts its character. The one exception is Ctrl+Alt together: Windows
/// reports AltGr (Polish ą ę ł ó ...) that way, and that is text.
pub fn edit_line(buf: &mut String, key: &KeyEvent) {
    let m = key.modifiers;
    let (ctrl, alt) = (m.contains(KeyModifiers::CONTROL), m.contains(KeyModifiers::ALT));
    match key.code {
        KeyCode::Backspace => {
            buf.pop();
        }
        KeyCode::Char('u') if ctrl && !alt => buf.clear(),
        KeyCode::Char('w') if ctrl && !alt => delete_word(buf),
        KeyCode::Char(c) if (!ctrl && !alt) || (ctrl && alt) => buf.push(c),
        _ => {}
    }
}

/// New scroll offset after `key` in a scrollable window, or `None` when the key does not scroll.
fn scrolled(key: &KeyEvent, scroll: u16, max: u16) -> Option<u16> {
    if is_chord(key) {
        return None;
    }
    Some(match key.code {
        KeyCode::Char('j') | KeyCode::Down => scroll.saturating_add(1).min(max),
        KeyCode::Char('k') | KeyCode::Up => scroll.saturating_sub(1),
        KeyCode::PageDown => scroll.saturating_add(10).min(max),
        KeyCode::PageUp => scroll.saturating_sub(10),
        KeyCode::Char('g') | KeyCode::Home => 0,
        KeyCode::Char('G') | KeyCode::End => max,
        _ => return None,
    })
}

/// Handles `key` for the open window `mode` and returns the window that stays open (`None` = closed)
/// with the action the key asked for. `scroll_max` is the largest useful scroll the renderer reported.
/// Ctrl+C is the app's business and never gets here.
pub fn handle_key(mode: Mode, key: KeyEvent, scroll_max: u16) -> (Option<Mode>, Action) {
    match mode {
        Mode::Normal => (None, Action::None),
        Mode::Input {
            prompt,
            mut buf,
            target,
        } => match key.code {
            KeyCode::Esc => (None, Action::None),
            KeyCode::Enter => {
                if buf.trim().is_empty() {
                    return (None, Action::None);
                }
                let text = buf.trim().to_string();
                let action = match target {
                    InputTarget::Reply(id) => Action::Reply(id, text),
                    InputTarget::Config(key) => Action::SetConfig(key, text),
                    InputTarget::Answer(id, question) => Action::Answer(id, question, text),
                };
                (None, action)
            }
            _ => {
                edit_line(&mut buf, &key);
                (Some(Mode::Input { prompt, buf, target }), Action::None)
            }
        },
        Mode::Confirm { action, .. } => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') if !is_chord(&key) => (None, *action),
            _ => (None, Action::None),
        },
        Mode::Answer { id, screen, text } => handle_answer_key(key, id, screen, text),
        Mode::Comment { comment, scroll } => {
            if key.code == KeyCode::Esc && !is_chord(&key) {
                (None, Action::None)
            } else {
                let scroll = scrolled(&key, scroll, scroll_max).unwrap_or(scroll);
                (Some(Mode::Comment { comment, scroll }), Action::None)
            }
        }
        Mode::Help { scroll } => {
            let closes = matches!(key.code, KeyCode::Esc | KeyCode::Char('?' | 'q')) && !is_chord(&key);
            if closes {
                (None, Action::None)
            } else {
                let scroll = scrolled(&key, scroll, scroll_max).unwrap_or(scroll);
                (Some(Mode::Help { scroll }), Action::None)
            }
        }
    }
}

/// Keys inside the answer overlay: nothing else is bound while it is open. Digits, Enter, Up, Down,
/// `y` and `n` go to the worker pane as keys and close the overlay; `t` types a reply; Esc closes
/// without sending.
fn handle_answer_key(
    key: KeyEvent,
    id: IssueId,
    screen: String,
    text: Option<String>,
) -> (Option<Mode>, Action) {
    let keys = |name: &str, id: IssueId| (None, Action::Keys(id, vec![name.to_string()]));
    if let Some(mut buf) = text {
        return match key.code {
            KeyCode::Esc => (None, Action::None),
            KeyCode::Enter if !buf.trim().is_empty() => (None, Action::Reply(id, buf.trim().to_string())),
            _ => {
                edit_line(&mut buf, &key);
                (
                    Some(Mode::Answer {
                        id,
                        screen,
                        text: Some(buf),
                    }),
                    Action::None,
                )
            }
        };
    }
    let stay = |id, screen, text| (Some(Mode::Answer { id, screen, text }), Action::None);
    match key.code {
        // A chord is never an answer: Ctrl+3 must not send "3" to the worker.
        _ if is_chord(&key) => stay(id, screen, None),
        KeyCode::Esc => (None, Action::None),
        KeyCode::Char(c @ ('1'..='9' | 'y' | 'n')) => keys(&c.to_string(), id),
        KeyCode::Enter => keys("enter", id),
        KeyCode::Up => keys("up", id),
        KeyCode::Down => keys("down", id),
        KeyCode::Char('t') => stay(id, screen, Some(String::new())),
        _ => stay(id, screen, None),
    }
}

// ---------------------------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------------------------

/// Offset of the first visible line, clamped to the scrollable range; publishes the range so that
/// `j` stops at the end.
fn clamp_scroll(scroll_max: &Cell<u16>, total: usize, visible: usize, scroll: u16) -> usize {
    let max = total.saturating_sub(visible).min(u16::MAX as usize);
    scroll_max.set(max as u16);
    (scroll as usize).min(max)
}

/// What the windows are drawn against.
pub struct Screen<'a> {
    /// The whole frame: the help page covers it and draws its own title and hints.
    pub area: Rect,
    /// The rows between the banner and the rule: boxed windows are centred in it, so the chrome stays
    /// visible around them.
    pub body: Rect,
    pub view: View,
    pub snap: &'a Snapshot,
    pub theme: &'a Theme,
    /// Written by the windows that scroll: the largest useful scroll.
    pub scroll_max: &'a Cell<u16>,
}

/// Draws the open window `mode`.
pub fn render(frame: &mut Frame, s: &Screen, mode: &Mode) {
    match mode {
        Mode::Answer { id, screen, text } => {
            answer_overlay(frame, s.body, id, screen, text.as_deref(), s.snap, s.theme);
        }
        Mode::Comment { comment, scroll } => {
            comment_modal(frame, s.body, comment, *scroll, s.theme, s.scroll_max);
        }
        Mode::Help { scroll } => help_view(frame, s.area, s.view, *scroll, s.theme, s.scroll_max),
        Mode::Normal | Mode::Input { .. } | Mode::Confirm { .. } => {}
    }
}

/// The Inbox modal: header line fixed on top, the wrapped body scrolling below it.
fn comment_modal(
    frame: &mut Frame,
    area: Rect,
    c: &Comment,
    scroll: u16,
    theme: &Theme,
    scroll_max: &Cell<u16>,
) {
    let popup = area.centered(Constraint::Percentage(80), Constraint::Percentage(80));
    let block = Block::bordered()
        .title(" comment ")
        .title_bottom(" j/k scroll · Esc close (nothing is marked read) ")
        .border_style(theme.warn());
    let inner = block.inner(popup);
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);
    let [head, _gap, body] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(0)]).areas(inner);
    let header = format!(
        "#{} {} [{}] {} -> {}",
        c.id, c.issue_id, c.author, c.kind, c.to_lane
    );
    frame.render_widget(Paragraph::new(Line::styled(header, theme.bold())), head);
    let lines = wrap_text(&c.body, body.width as usize);
    let top = clamp_scroll(scroll_max, lines.len(), body.height as usize, scroll);
    let shown: Vec<Line> = lines
        .into_iter()
        .skip(top)
        .take(body.height as usize)
        .map(Line::from)
        .collect();
    frame.render_widget(Paragraph::new(shown), body);
}

/// The largest option number the pane shows (`  2. No, remove it`), 9 when it shows none.
fn option_count(screen: &str) -> u32 {
    screen
        .lines()
        .filter_map(|l| {
            let l = l.trim_start_matches(|c: char| c.is_whitespace() || c == '❯' || c == '>');
            let digits: String = l.chars().take_while(char::is_ascii_digit).collect();
            (!digits.is_empty() && l[digits.len()..].starts_with(". "))
                .then(|| digits.parse::<u32>().ok())
                .flatten()
        })
        .max()
        .unwrap_or(9)
        .clamp(1, 9)
}

/// `key text` pairs for the hint row of a window: key letters accent + bold, the words dim.
fn hint_row(theme: &Theme, pairs: &[(String, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, (key, text)) in pairs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled(key.clone(), theme.key()));
        spans.push(Span::styled(format!(" {text}"), theme.dim()));
    }
    Line::from(spans)
}

/// The centred answer box: the worker's own screen, the last lines of its pane (that is where its
/// question and numbered choices are), and one row of key hints (or the reply being typed) under it.
fn answer_overlay(
    frame: &mut Frame,
    area: Rect,
    id: &IssueId,
    screen: &str,
    text: Option<&str>,
    snap: &Snapshot,
    theme: &Theme,
) {
    let popup = area.centered(Constraint::Max(104), Constraint::Length(24.min(area.height)));
    let title = snap.issue(id).map(|i| i.title.as_str()).unwrap_or("");
    let heading = if title.is_empty() {
        format!(" answer {id} · worker pane, live ")
    } else {
        format!(" answer {id} · {title} · worker pane, live ")
    };
    let block = Block::bordered()
        .title(trunc(&heading, popup.width.saturating_sub(2) as usize))
        .title_style(theme.bold())
        .border_style(theme.accent());
    let inner = block.inner(popup);
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);
    let [body, hints] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);
    let lines: Vec<&str> = screen.lines().collect();
    let tail = &lines[lines.len().saturating_sub(body.height as usize)..];
    let shown: Vec<Line> = if tail.is_empty() {
        vec![Line::styled("(reading the pane...)", theme.dim())]
    } else {
        tail.iter().map(|l| Line::from((*l).to_string())).collect()
    };
    frame.render_widget(Paragraph::new(shown), body);
    let choose = format!("1-{}", option_count(screen));
    let row = match text {
        // The long wording needs 97 columns; a narrower box gets the short one rather than a cut-off line.
        None if hints.width >= 97 => hint_row(
            theme,
            &[
                (choose, "choose"),
                ("Enter".into(), "send the highlighted one"),
                ("↑/↓".into(), "move"),
                ("t".into(), "type a reply"),
                ("Esc".into(), "close, send nothing"),
            ],
        ),
        None => hint_row(
            theme,
            &[
                (choose, "choose"),
                ("Enter".into(), "send"),
                ("↑/↓".into(), "move"),
                ("t".into(), "type"),
                ("Esc".into(), "close"),
            ],
        ),
        Some(buf) => Line::from(vec![
            Span::styled("reply: ", theme.key()),
            Span::raw(format!("{buf}▏")),
            Span::styled("  (Enter send · Esc close)", theme.dim()),
        ]),
    };
    frame.render_widget(Paragraph::new(row), hints);
}

/// Width of the key column of the help page.
const HELP_KEY_W: usize = 13;

/// The lines of one help column: a heading, its rows (key, then the explanation wrapped to `width`).
fn help_column(
    sections: &[&super::bindings::HelpSection],
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let text_w = width.saturating_sub(HELP_KEY_W + 2).max(10);
    let mut out = Vec::new();
    for (n, s) in sections.iter().enumerate() {
        if n > 0 {
            out.push(Line::raw(""));
        }
        out.push(Line::styled(s.title.to_string(), theme.bold()));
        for (key, help) in &s.items {
            let wrapped = wrap_text(help, text_w);
            for (i, line) in wrapped.into_iter().enumerate() {
                let lead = if i == 0 {
                    Span::styled(format!(" {key:<HELP_KEY_W$}"), theme.key())
                } else {
                    Span::raw(" ".repeat(HELP_KEY_W + 1))
                };
                out.push(Line::from(vec![
                    lead,
                    Span::raw(format!(" {}", line.trim_start())),
                ]));
            }
        }
    }
    out
}

/// The key reference page: a title row, two columns of grouped bindings generated from the table,
/// the symbols legend, and the key hints of the page itself.
fn help_view(frame: &mut Frame, area: Rect, view: View, scroll: u16, theme: &Theme, scroll_max: &Cell<u16>) {
    frame.render_widget(Clear, area);
    let [title, rule_top, body, legend, rule_bottom, hints] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);
    let left_text = " Keys · everything the TUI understands, in plain words";
    let right_text = "Esc close ";
    let gap = (area.width as usize).saturating_sub(text_width(left_text) + text_width(right_text));
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(left_text, theme.bold()),
            Span::raw(" ".repeat(gap)),
            Span::styled(right_text, theme.dim()),
        ])),
        title,
    );
    frame.render_widget(chrome::rule(area.width, theme), rule_top);

    let sections = super::bindings::help_sections();
    let weight = |s: &super::bindings::HelpSection| s.items.len() + 2;
    let total: usize = sections.iter().map(weight).sum();
    let (mut left, mut right) = (Vec::new(), Vec::new());
    let mut used = 0;
    for s in &sections {
        // A section goes left while the left column is still shorter than half of everything.
        if used + weight(s) / 2 < total / 2 {
            left.push(s);
        } else {
            right.push(s);
        }
        used += weight(s);
    }
    let [left_area, right_area] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(body);
    let left_lines = help_column(&left, left_area.width as usize, theme);
    let right_lines = help_column(&right, right_area.width as usize, theme);
    let top = clamp_scroll(
        scroll_max,
        left_lines.len().max(right_lines.len()),
        body.height as usize,
        scroll,
    );
    for (lines, rect) in [(left_lines, left_area), (right_lines, right_area)] {
        let shown: Vec<Line> = lines.into_iter().skip(top).take(rect.height as usize).collect();
        frame.render_widget(Paragraph::new(shown), rect);
    }

    let mut legend_spans = vec![Span::styled(" SYMBOLS", theme.bold())];
    for (mark, word) in Mark::LEGEND {
        legend_spans.push(Span::raw("   "));
        legend_spans.push(Span::styled(mark.glyph(), theme.mark(mark)));
        legend_spans.push(Span::raw(format!(" {word}")));
    }
    frame.render_widget(Paragraph::new(Line::from(legend_spans)), legend);
    frame.render_widget(chrome::rule(area.width, theme), rule_bottom);
    frame.render_widget(
        Paragraph::new(chrome::window_hints_line(
            &footer_hints(view, ModeKind::Help),
            theme,
        )),
        hints,
    );
}
