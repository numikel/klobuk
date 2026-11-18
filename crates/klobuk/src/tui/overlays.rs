//! The windows that sit on top of the views: the answer overlay, the help view, the Inbox comment
//! modal, the confirmation and the one-line text input. Each is a [`Mode`] on the app's overlay stack;
//! [`handle_key`] is its pure key handling and [`render`] draws it.
//!
//! Confirmation and text input have no box: their prompt is the footer line, which `chrome.rs` draws.

use std::cell::Cell;
use std::time::Duration;

use klobuk_core::*;
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
use crate::worker::{dialog_step, is_free_text, options, DialogStep, SUBMIT_KEY};

/// What the input line is typing for.
#[derive(Debug, Clone, PartialEq)]
pub enum InputTarget {
    Reply(IssueId),
    Config(String),
    /// The new budget in USD of the issue's run.
    Budget(IssueId),
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
        /// Where the overlay is in a dialog with several questions.
        walk: Walk,
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
    /// The live output of the gate in flight, read from the snapshot on every frame, so it moves while
    /// the window is open. `asked` is the issue it was opened from; `back` is how many lines it is
    /// scrolled up from the newest one (0 follows the tail).
    Gate {
        asked: IssueId,
        back: u16,
    },
}

/// How long the overlay waits for the pane to move on after a key before it takes keys again.
pub const WAIT_TTL: Duration = Duration::from_secs(3);

/// What the answer overlay remembers while it walks through a dialog with several questions (tabs).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Walk {
    /// Key mode (`k`): every key goes straight to the pane and the overlay stays open.
    pub raw: bool,
    /// What the human picked so far, one entry per answered tab (`First: 1 A`).
    pub picked: Vec<String>,
    /// Set while a key just went out: the pane has to show the next step before the overlay takes another.
    pub wait: Option<Wait>,
}

/// A key is on its way: the pane text before it, and how far the dialog has to get.
#[derive(Debug, Clone, PartialEq)]
pub struct Wait {
    before: String,
    /// Tabs that have to read as answered (the same count as before for a key that only moves the cursor).
    answered: usize,
    /// When the overlay first saw the wait (the app stamps it in `advance`), for the [`WAIT_TTL`].
    pub since: Option<Duration>,
}

impl Wait {
    fn new(screen: &str, answered: usize) -> Wait {
        Wait {
            before: screen.to_string(),
            answered,
            since: None,
        }
    }
}

/// Stores a freshly read pane and ends the wait when it shows what the wait was for: the text changed
/// and, in a tabbed dialog, as many tabs are answered as the key should have answered.
pub fn take_screen(screen: &mut String, walk: &mut Walk, text: String) {
    if let Some(w) = &walk.wait {
        let moved = text != w.before
            && match dialog_step(&text) {
                DialogStep::Question { answered, .. } => answered >= w.answered,
                DialogStep::Review { .. } | DialogStep::Single => true,
            };
        if moved {
            walk.wait = None;
        }
    }
    *screen = text;
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
            Mode::Gate { .. } => ModeKind::Gate,
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
    // ---- Gate output ----
    b(
        "j / k",
        "scroll",
        "scroll the live gate output; it follows the newest line until you scroll up",
        Group::Window,
        ANY,
        &[M::Gate],
        Slot::Hint,
    ),
    b(
        "G",
        "follow",
        "jump to the newest line and follow the output again",
        Group::Window,
        ANY,
        &[M::Gate],
        Slot::Hint,
    ),
    b(
        "Esc",
        "close",
        "close the live gate output",
        Group::Window,
        ANY,
        &[M::Gate],
        Slot::Hint,
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
        "k",
        "key mode",
        "send every key straight to the pane, Left/Right/Tab too, and stay open (k again leaves it); a dialog with several questions is otherwise answered tab by tab and submitted by klobuk",
        Group::Window,
        ANY,
        &[M::Answer],
        Slot::None,
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
                    InputTarget::Budget(id) => Action::RaiseBudget(id, text),
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
        Mode::Answer {
            id,
            screen,
            text,
            walk,
        } => handle_answer_key(key, id, screen, text, walk),
        Mode::Comment { comment, scroll } => {
            if key.code == KeyCode::Esc && !is_chord(&key) {
                (None, Action::None)
            } else {
                let scroll = scrolled(&key, scroll, scroll_max).unwrap_or(scroll);
                (Some(Mode::Comment { comment, scroll }), Action::None)
            }
        }
        Mode::Gate { asked, back } => {
            let closes = matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) && !is_chord(&key);
            if closes {
                return (None, Action::None);
            }
            // scrolled from the newest line: down is towards it, up is away from it
            let back = match key.code {
                _ if is_chord(&key) => back,
                KeyCode::Char('j') | KeyCode::Down => back.saturating_sub(1),
                KeyCode::Char('k') | KeyCode::Up => back.saturating_add(1).min(scroll_max),
                KeyCode::PageDown => back.saturating_sub(10),
                KeyCode::PageUp => back.saturating_add(10).min(scroll_max),
                KeyCode::Char('g') | KeyCode::Home => scroll_max,
                KeyCode::Char('G') | KeyCode::End => 0,
                _ => back,
            };
            (Some(Mode::Gate { asked, back }), Action::None)
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

/// The key name Herdr's `send-keys` takes for `code`, for the keys the overlay forwards as they are.
fn key_name(code: KeyCode, raw: bool) -> Option<String> {
    match code {
        KeyCode::Char(c @ ('1'..='9' | 'y' | 'n')) => Some(c.to_string()),
        KeyCode::Enter => Some("enter".into()),
        KeyCode::Up => Some("up".into()),
        KeyCode::Down => Some("down".into()),
        // Moving between tabs by hand is what key mode is for; the one-key flow never needed it.
        KeyCode::Left if raw => Some("left".into()),
        KeyCode::Right if raw => Some("right".into()),
        KeyCode::Tab if raw => Some("tab".into()),
        _ => None,
    }
}

/// Keys inside the answer overlay: nothing else is bound while it is open.
///
/// A dialog with one question (or none the overlay knows): digits, Enter, Up, Down, `y` and `n` go to
/// the worker pane as keys and close the overlay, as they always did. A dialog with several questions
/// (a tab row `☐ First  ☐ Second  ✔ Submit`) is walked one question at a time: a digit (or Enter on the
/// highlighted option) answers the open tab and the overlay waits for the pane to show the next one;
/// the answer to the last tab also sends the Submit step ([`SUBMIT_KEY`]) and closes. `k` switches to
/// key mode (every key goes to the pane, Left/Right/Tab too, the overlay stays open), `t` types a
/// reply, Esc closes without sending.
fn handle_answer_key(
    key: KeyEvent,
    id: IssueId,
    screen: String,
    text: Option<String>,
    mut walk: Walk,
) -> (Option<Mode>, Action) {
    if let Some(mut buf) = text {
        return match key.code {
            KeyCode::Esc => (None, Action::None),
            KeyCode::Enter if !buf.trim().is_empty() => {
                let reply = buf.trim().to_string();
                match dialog_step(&screen) {
                    // A typed answer on a question tab that is not the last: the worker types it into the
                    // tab's free-text option and the walk goes on with the next question. On the last tab
                    // the worker also submits, so the overlay closes like it does for a digit.
                    DialogStep::Question {
                        tabs,
                        index,
                        answered,
                    } if answered + 1 != tabs.len() => {
                        let label = tabs.get(index).map_or("", |t| t.label.as_str());
                        walk.picked.push(format!("{label}: typed {reply}"));
                        walk.wait = Some(Wait::new(&screen, answered + 1));
                        let mode = Mode::Answer {
                            id: id.clone(),
                            screen,
                            text: None,
                            walk,
                        };
                        (Some(mode), Action::Reply(id, reply))
                    }
                    _ => (None, Action::Reply(id, reply)),
                }
            }
            _ => {
                edit_line(&mut buf, &key);
                let text = Some(buf);
                (
                    Some(Mode::Answer {
                        id,
                        screen,
                        text,
                        walk,
                    }),
                    Action::None,
                )
            }
        };
    }
    let keep = |id, screen, walk| {
        (
            Some(Mode::Answer {
                id,
                screen,
                text: None,
                walk,
            }),
            Action::None,
        )
    };
    // The overlay stays open while the keys go out.
    let stay_and_send = |id: IssueId, screen, walk, keys| {
        let (mode, _) = keep(id.clone(), screen, walk);
        (mode, Action::Keys(id, keys))
    };
    match key.code {
        // A chord is never an answer: Ctrl+3 must not send "3" to the worker.
        _ if is_chord(&key) => return keep(id, screen, walk),
        KeyCode::Esc => return (None, Action::None),
        KeyCode::Char('t') => {
            let mode = Mode::Answer {
                id,
                screen,
                text: Some(String::new()),
                walk,
            };
            return (Some(mode), Action::None);
        }
        KeyCode::Char('k') => {
            walk.raw = !walk.raw;
            // A key sent in key mode may have left the pane as it was: do not make the next step wait for it.
            walk.wait = None;
            return keep(id, screen, walk);
        }
        _ => {}
    }
    let Some(name) = key_name(key.code, walk.raw) else {
        return keep(id, screen, walk);
    };
    if walk.raw {
        // Key mode: the key goes out and the overlay stays, re-reading the pane to show what it did.
        walk.wait = Some(Wait::new(&screen, 0));
        return stay_and_send(id, screen, walk, vec![name]);
    }
    match dialog_step(&screen) {
        DialogStep::Question {
            tabs,
            index,
            answered,
        } => {
            if walk.wait.is_some() {
                // The pane is still moving: a digit now would land on the tab that is going away.
                return keep(id, screen, walk);
            }
            let opts = options(&screen);
            let found = match name.as_str() {
                "enter" => opts.iter().find(|o| o.cursor),
                digits => digits
                    .parse::<u32>()
                    .ok()
                    .and_then(|n| opts.iter().find(|o| o.n == n)),
            };
            match (name.as_str(), found) {
                // Up/Down, or an option that opens a text box: only the key goes out and the tab stays
                // open; the next peek shows what the pane did.
                ("up" | "down", _) => {
                    walk.wait = Some(Wait::new(&screen, answered));
                    stay_and_send(id, screen, walk, vec![name])
                }
                (_, Some(o)) if is_free_text(o) => {
                    walk.wait = Some(Wait::new(&screen, answered));
                    stay_and_send(id, screen, walk, vec![name])
                }
                // A digit beyond the options, y/n, Enter with no cursor on the pane: nothing to answer.
                (_, None) => keep(id, screen, walk),
                (_, Some(o)) => {
                    let label = tabs.get(index).map_or("", |t| t.label.as_str());
                    walk.picked.push(format!("{label}: {} {}", o.n, o.text));
                    walk.wait = Some(Wait::new(&screen, answered + 1));
                    if answered + 1 == tabs.len() {
                        // The last question: the Submit step goes with it, and the dialog is done.
                        (None, Action::Keys(id, vec![name, SUBMIT_KEY.to_string()]))
                    } else {
                        stay_and_send(id, screen, walk, vec![name])
                    }
                }
            }
        }
        // One question, the review page, or a screen the overlay does not know: the one-key flow.
        DialogStep::Single | DialogStep::Review { .. } => (None, Action::Keys(id, vec![name])),
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
        Mode::Answer {
            id,
            screen,
            text,
            walk,
        } => {
            answer_overlay(frame, s.body, id, screen, text.as_deref(), walk, s.snap, s.theme);
        }
        Mode::Comment { comment, scroll } => {
            comment_modal(frame, s.body, comment, *scroll, s.theme, s.scroll_max);
        }
        Mode::Help { scroll } => help_view(frame, s.area, s.view, *scroll, s.theme, s.scroll_max),
        Mode::Gate { asked, back } => {
            gate_overlay(frame, s.body, asked, *back, s.snap, s.theme, s.scroll_max)
        }
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

/// A line of command output made safe to draw: control characters (an escape sequence, the carriage
/// return of a progress bar) become blanks, so the output cannot paint on the screen.
fn plain(line: &str) -> String {
    line.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// The window of the gate in flight: who runs which gate for how long, the gates that are over, and the
/// newest lines the running gate printed. It reads the snapshot on every frame, so it stays current.
fn gate_overlay(
    frame: &mut Frame,
    area: Rect,
    asked: &IssueId,
    back: u16,
    snap: &Snapshot,
    theme: &Theme,
    scroll_max: &Cell<u16>,
) {
    use super::snapshot::parse_time;
    use super::views::steps::duration;
    let popup = area.centered(Constraint::Percentage(90), Constraint::Percentage(90));
    let live = snap.gate_live.as_ref();
    let heading = match live {
        Some(l) => format!(" gate {} · {} · {} · live ", l.gate, l.issue, l.kind),
        None => format!(" gate output · {asked} "),
    };
    let block = Block::bordered()
        .title(trunc(&heading, popup.width.saturating_sub(2) as usize))
        .title_style(theme.bold())
        .title_bottom(" j/k scroll · G follow · Esc close ")
        .border_style(theme.accent());
    let inner = block.inner(popup);
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);
    let Some(live) = live else {
        scroll_max.set(0);
        frame.render_widget(
            Paragraph::new(Line::styled(
                "No gate run is in flight: it is over, or klobuk serve is not running.",
                theme.dim(),
            )),
            inner,
        );
        return;
    };
    let since = |at: &str| match (parse_time(&snap.now), parse_time(at)) {
        (Some(now), Some(at)) => duration((now - at).whole_seconds().max(0)),
        _ => "-".into(),
    };
    let mut head = vec![Line::from(vec![
        Span::styled(format!("gate {} ", live.gate), theme.bold()),
        Span::styled(
            format!("running {}", since(&live.gate_started_at)),
            theme.accent(),
        ),
        Span::raw(format!(
            " · {} run of {}, started {} ago",
            live.kind,
            live.issue,
            since(&live.started_at)
        )),
    ])];
    if &live.issue != asked {
        head.push(Line::styled(
            format!("{asked} waits for the gate slot; this is the gate that holds it"),
            theme.warn(),
        ));
    }
    let done: Vec<String> = live
        .finished
        .iter()
        .map(|g| {
            format!(
                "{} {} {}",
                if g.ok { "✓" } else { "✗" },
                g.name,
                duration((g.duration_ms / 1000) as i64)
            )
        })
        .collect();
    let next: Vec<&str> = live
        .gates
        .iter()
        .skip(live.finished.len() + 1)
        .map(String::as_str)
        .collect();
    let done = if done.is_empty() {
        "nothing yet".to_string()
    } else {
        done.join(", ")
    };
    let next = if next.is_empty() {
        "nothing".to_string()
    } else {
        next.join(", ")
    };
    head.push(Line::styled(
        trunc(&format!("done: {done} · next: {next}"), inner.width as usize),
        theme.dim(),
    ));
    head.push(Line::styled("─".repeat(inner.width as usize), theme.dim()));
    let [top, body] =
        Layout::vertical([Constraint::Length(head.len() as u16), Constraint::Min(0)]).areas(inner);
    frame.render_widget(Paragraph::new(head), top);
    let lines: Vec<String> = live.tail.lines().map(plain).collect();
    let visible = body.height as usize;
    let max = lines.len().saturating_sub(visible).min(u16::MAX as usize);
    scroll_max.set(max as u16);
    let end = lines.len().saturating_sub((back as usize).min(max));
    let start = end.saturating_sub(visible);
    let shown: Vec<Line> = if lines.is_empty() {
        vec![Line::styled("(the gate has printed nothing yet)", theme.dim())]
    } else {
        lines[start..end]
            .iter()
            .map(|l| Line::raw(trunc(l, body.width as usize)))
            .collect()
    };
    frame.render_widget(Paragraph::new(shown), body);
}

/// The largest option number the pane shows (`  2. No, remove it`), 9 when it shows none.
fn option_count(screen: &str) -> u32 {
    options(screen).iter().map(|o| o.n).max().unwrap_or(9).clamp(1, 9)
}

/// The line above the pane text that says where the overlay is in the dialog, or `None` for the plain
/// one-key flow (one question, nothing to add).
fn dialog_note(screen: &str, walk: &Walk, theme: &Theme) -> Option<Line<'static>> {
    if walk.raw {
        return Some(Line::styled(
            "key mode: every key goes straight to the pane, ←/→/Tab too · k leaves it",
            theme.warn(),
        ));
    }
    match dialog_step(screen) {
        DialogStep::Question { tabs, index, .. } => {
            let mut note = format!("question {} of {}", index + 1, tabs.len());
            if !walk.picked.is_empty() {
                note.push_str(&format!(" · picked {}", walk.picked.join(", ")));
            }
            if walk.wait.is_some() {
                note.push_str(" · waiting for Claude Code...");
            }
            Some(Line::styled(note, theme.bold()))
        }
        DialogStep::Review { .. } => Some(Line::styled(
            "review page: 1 or Enter submits the answers, 2 cancels",
            theme.bold(),
        )),
        DialogStep::Single if !screen.trim().is_empty() && options(screen).is_empty() => Some(Line::styled(
            "not a known dialog shape: keys go straight to the pane · k key mode · t type a reply",
            theme.warn(),
        )),
        DialogStep::Single => None,
    }
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
#[allow(clippy::too_many_arguments)]
fn answer_overlay(
    frame: &mut Frame,
    area: Rect,
    id: &IssueId,
    screen: &str,
    text: Option<&str>,
    walk: &Walk,
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
    let tabbed = !walk.raw && matches!(dialog_step(screen), DialogStep::Question { .. });
    let body = match text.is_none().then(|| dialog_note(screen, walk, theme)).flatten() {
        Some(note) => {
            let [note_row, rest] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(body);
            frame.render_widget(Paragraph::new(note), note_row);
            rest
        }
        None => body,
    };
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
        None if walk.raw => hint_row(
            theme,
            &[
                ("1-9 y n Enter ↑/↓ ←/→ Tab".into(), "send"),
                ("k".into(), "back"),
                ("Esc".into(), "close"),
            ],
        ),
        // A dialog with several questions offers `k` (key mode) too; the box is at most 104 wide, so the
        // wording is shorter than the one-question row to fit its 88 columns.
        None if tabbed && hints.width >= 88 => hint_row(
            theme,
            &[
                (choose, "choose"),
                ("Enter".into(), "send highlighted"),
                ("↑/↓".into(), "move"),
                ("k".into(), "raw keys"),
                ("t".into(), "type a reply"),
                ("Esc".into(), "close"),
            ],
        ),
        None if tabbed => hint_row(
            theme,
            &[
                (choose, "choose"),
                ("Enter".into(), "send"),
                ("↑/↓".into(), "move"),
                ("k".into(), "keys"),
                ("t".into(), "type"),
                ("Esc".into(), "close"),
            ],
        ),
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
