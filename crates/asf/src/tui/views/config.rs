//! The Config view: the settings of `.factory/config.toml` in groups (models, budgets, flow,
//! notifications), each with a plain label, its current value, the default when the value differs, and
//! one line saying what it does.
//!
//! A choice is stepped with `h`/`l`, an on/off setting toggled with `Space`, a number or a custom model
//! id typed with `Enter`; `d` puts a key back to its default. Every change is an [`Action`] the event
//! loop carries out (`SetConfig` / `UnsetConfig`): the file is written there, validated by
//! `config_edit`, and the next snapshot shows the result. The view does no IO.

use std::cell::Cell;

use asf_core::config_edit::{fields, unsettable_keys, ConfigField, ConfigGroup, FieldKind};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::tui::app::{Action, View};
use crate::tui::bindings::*;
use crate::tui::overlays::InputTarget;
use crate::tui::snapshot::{move_selection, text_width, trunc, Snapshot};
use crate::tui::theme::Theme;

#[derive(Debug, Clone, Default)]
pub struct State {
    pub selected: usize,
    /// A value the file refused: the key and the reason, drawn under the row until the next key.
    error: Option<(String, String)>,
    /// The first line of the list that is on screen. Drawing moves it just far enough to keep the
    /// selected row visible, so the list does not jump while the selection moves inside the window.
    top: Cell<usize>,
}

impl State {
    /// Remembers that `key` refused the value just typed; the row of `key` shows `message` under it.
    // Called by the event loop with the result of `Action::SetConfig` (`tui::note_config_result`).
    pub fn fail(&mut self, key: &str, message: &str) {
        let message = message.strip_prefix("error: ").unwrap_or(message);
        self.error = Some((key.to_string(), message.to_string()));
    }
}

const CONFIG: &[View] = &[View::Config];

pub const BINDINGS: &[Binding] = &[
    b(
        "j / k",
        "move",
        HELP_MOVE,
        Group::Everywhere,
        CONFIG,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "h / l",
        "choose",
        "previous / next choice of a setting\n(saved at once)",
        Group::Config,
        CONFIG,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "Space",
        "toggle",
        "switch an on/off setting",
        Group::Config,
        CONFIG,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "Enter",
        "type a value",
        "type a number, or a model id that is\nnot in the list (checked before it is saved)",
        Group::Config,
        CONFIG,
        NORMAL,
        Slot::Hint,
    ),
    b(
        "d",
        "back to default",
        "back to the default (asks first)",
        Group::Config,
        CONFIG,
        NORMAL,
        Slot::Hint,
    ),
];

/// The hint on the right of the first group title.
const MODELS_HINT: &str = "who thinks, who types, who checks";

/// Under the list: where a change goes and when it takes effect.
const NOTE: &str = "Saved to .factory/config.toml · comments in the file are kept · applies on the next tick";

/// Columns: the marker, the label, then the value.
const LABEL_X: usize = 3;
const VALUE_X: usize = 24;
/// A value narrower than this is padded to it, so `default ...` lines up from row to row.
const VALUE_W: usize = 10;
/// Gap between the value cell and the help column.
const GAP: usize = 2;

pub fn rows(_snap: &Snapshot) -> usize {
    fields().len()
}

pub fn selected(snap: &Snapshot, state: &State) -> usize {
    state.selected.min(rows(snap).saturating_sub(1))
}

// ---------------------------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------------------------

/// What `.factory/config.toml` says about one key.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Current {
    /// An optional key that is absent.
    Unset,
    /// The file does not parse.
    Unreadable,
    Value(String),
}

fn current(snap: &Snapshot, field: &ConfigField) -> Current {
    match snap.config.iter().find(|r| r.key == field.key) {
        // A snapshot without rows (built by hand) shows what the program would use.
        None => field.default_value().map_or(Current::Unset, Current::Value),
        Some(r) if r.raw.is_empty() && r.value == "(unreadable)" => Current::Unreadable,
        Some(r) if r.raw.is_empty() && r.value == "(unset)" => Current::Unset,
        Some(r) => Current::Value(r.raw.clone()),
    }
}

/// A value as the rows print it: `[x] on`, `$5.00`, `sonnet`.
fn show(field: &ConfigField, raw: &str) -> String {
    match field.kind {
        FieldKind::Bool => match raw {
            "true" => "[x] on".into(),
            _ => "[ ] off".into(),
        },
        FieldKind::Number if field.key.ends_with("budget_usd") => raw
            .parse::<f64>()
            .map_or_else(|_| raw.to_string(), |usd| format!("${usd:.2}")),
        _ => raw.to_string(),
    }
}

/// The default as the `default ...` note and the confirmation print it: `$1.00`, `off`, `(unset)`.
fn show_default(field: &ConfigField) -> String {
    match (field.default_value(), field.kind) {
        (None, _) => "(unset)".into(),
        (Some(raw), FieldKind::Bool) => if raw == "true" { "on" } else { "off" }.into(),
        (Some(raw), _) => show(field, &raw),
    }
}

/// Whether the key already has its default (an unreadable file counts: there is nothing to put back).
fn at_default(snap: &Snapshot, field: &ConfigField) -> bool {
    match current(snap, field) {
        Current::Unreadable => true,
        Current::Unset => field.default_value().is_none(),
        Current::Value(v) => field.default_value().as_deref() == Some(v.as_str()),
    }
}

/// The choice `step` (1 or -1) away from `current`, wrapping around. A value outside the list steps
/// to the first choice going forward and to the last going back.
fn step_choice(choices: &'static [&'static str], current: Option<&str>, step: isize) -> &'static str {
    let n = choices.len() as isize;
    match current.and_then(|c| choices.iter().position(|x| *x == c)) {
        Some(i) => choices[(i as isize + step).rem_euclid(n) as usize],
        None if step > 0 => choices[0],
        None => choices[choices.len() - 1],
    }
}

// ---------------------------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------------------------

type Seg = (String, Style);

/// `segs` cut to `width` columns with a trailing `…` when they do not fit, otherwise padded with
/// blanks to exactly `width`.
fn clip(segs: Vec<Seg>, width: usize) -> Vec<Span<'static>> {
    let total: usize = segs.iter().map(|(t, _)| text_width(t)).sum();
    let mut out = Vec::new();
    if total <= width {
        out.extend(segs.into_iter().map(|(t, s)| Span::styled(t, s)));
        if total < width {
            out.push(Span::raw(" ".repeat(width - total)));
        }
        return out;
    }
    if width == 0 {
        return out;
    }
    let mut used = 0;
    'cut: for (text, style) in segs {
        let mut piece = String::new();
        for c in text.chars() {
            let w = text_width(&c.to_string());
            if used + w > width - 1 {
                if !piece.is_empty() {
                    out.push(Span::styled(piece, style));
                }
                out.push(Span::styled("…", style));
                used += 1;
                break 'cut;
            }
            piece.push(c);
            used += w;
        }
        if !piece.is_empty() {
            out.push(Span::styled(piece, style));
        }
    }
    if used < width {
        out.push(Span::raw(" ".repeat(width - used)));
    }
    out
}

/// Where the help column starts: further right on a wide screen, so the longest choice list fits.
fn help_x(width: usize) -> usize {
    let wanted = if width >= 120 { 70 } else { 62 };
    wanted
        .min(width.saturating_sub(VALUE_X))
        .max(VALUE_X + VALUE_W + GAP)
}

/// The value cell of a row: the value, then the `default ...` note or the other choices.
fn value_segs(snap: &Snapshot, f: &ConfigField, selected: bool, cell: usize, theme: &Theme) -> Vec<Seg> {
    let plain = Style::default();
    let cur = current(snap, f);
    let mut segs: Vec<Seg> = Vec::new();
    let mut others = String::new();
    match (&cur, f.kind) {
        (Current::Unreadable, _) => segs.push(("(unreadable)".into(), theme.danger())),
        (_, FieldKind::Choice(choices)) if selected => {
            let name = match &cur {
                Current::Value(v) => v.clone(),
                _ => "(unset)".into(),
            };
            segs.push((format!("‹ {name} ›"), plain));
            for c in choices.iter().filter(|c| **c != name) {
                others.push_str("   ");
                others.push_str(c);
            }
            segs.push((others.clone(), theme.dim()));
        }
        (Current::Unset, _) => segs.push(("(unset)".into(), theme.dim())),
        (Current::Value(v), _) => segs.push((show(f, v), plain)),
    }
    let note = if cur != Current::Unreadable && !at_default(snap, f) {
        Some(format!("default {}", show_default(f)))
    } else if let (FieldKind::Choice(choices), false) = (f.kind, selected) {
        // two options fit beside the value; a list of models would only be noise on every row
        (choices.len() <= 2).then(|| choices.join(" · "))
    } else {
        None
    };
    if let Some(note) = note {
        let used: usize = segs.iter().map(|(t, _)| text_width(t)).sum();
        let gap = if used < VALUE_W { VALUE_W - used } else { 3 };
        // On the selected row the choices matter more: a note that would be cut is left out.
        if !selected || used + gap + text_width(&note) <= cell {
            segs.push((" ".repeat(gap), plain));
            segs.push((note, theme.dim()));
        }
    }
    segs
}

/// One field's row.
fn field_line(
    snap: &Snapshot,
    f: &ConfigField,
    selected: bool,
    width: usize,
    theme: &Theme,
) -> Line<'static> {
    let help_at = help_x(width);
    let cell = help_at - VALUE_X - GAP;
    let plain = Style::default();
    let mut spans = vec![Span::raw(if selected { " ▸ " } else { "   " })];
    spans.extend(clip(vec![(f.label.to_string(), plain)], VALUE_X - LABEL_X));
    spans.extend(clip(value_segs(snap, f, selected, cell, theme), cell));
    spans.push(Span::raw(" ".repeat(GAP)));
    spans.extend(clip(
        vec![(f.help.to_string(), theme.dim())],
        width.saturating_sub(help_at + 1),
    ));
    spans.push(Span::raw(" ")); // the last column belongs to the row too, so the highlight is unbroken
    if selected {
        // Patched span by span: a `Line` style reaches only as far as its spans do.
        spans = spans
            .into_iter()
            .map(|s| s.patch_style(theme.selected()))
            .collect();
    }
    Line::from(spans)
}

/// Everything the list prints, and where the selected row (with its error) is in it.
struct Listing {
    lines: Vec<Line<'static>>,
    /// First and last line to keep on screen for the selection; a first row brings its group title.
    keep: (usize, usize),
}

fn listing(snap: &Snapshot, state: &State, width: usize, theme: &Theme) -> Listing {
    let sel = selected(snap, state);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut keep = (0, 0);
    for (g, group) in ConfigGroup::ALL.iter().enumerate() {
        if g > 0 {
            lines.push(Line::raw(""));
        }
        let title_at = lines.len();
        let mut title = vec![Span::styled(
            format!(" {}", group.title().to_uppercase()),
            theme.bold(),
        )];
        if g == 0 {
            let at = help_x(width);
            let used = 1 + text_width(group.title());
            title.push(Span::raw(" ".repeat(at.saturating_sub(used))));
            title.extend(clip(
                vec![(MODELS_HINT.to_string(), theme.dim())],
                width.saturating_sub(at + 1),
            ));
        }
        lines.push(Line::from(title));
        lines.push(Line::raw(""));
        let mut first = true;
        for (i, f) in fields().iter().enumerate().filter(|(_, f)| f.group == *group) {
            let start = lines.len();
            lines.push(field_line(snap, f, i == sel, width, theme));
            if let Some((key, message)) = &state.error {
                if key == f.key {
                    let room = width.saturating_sub(VALUE_X + 1);
                    let text = trunc(&format!("error: {message}"), room);
                    lines.push(Line::from(vec![
                        Span::raw(" ".repeat(VALUE_X)),
                        Span::styled(text, theme.danger()),
                    ]));
                }
            }
            if i == sel {
                keep = (if first { title_at } else { start }, lines.len() - 1);
            }
            first = false;
        }
    }
    Listing { lines, keep }
}

pub fn render(frame: &mut Frame, area: Rect, snap: &Snapshot, state: &State, theme: &Theme) {
    let width = area.width as usize;
    let height = area.height as usize;
    let Listing { lines, keep } = listing(snap, state, width, theme);
    // The note follows the list after a blank line; when the list is taller it keeps the last row.
    let fits = lines.len() + 2 <= height;
    let list_h = if fits {
        lines.len()
    } else {
        height.saturating_sub(2)
    };
    let mut top = state.top.get().min(lines.len().saturating_sub(list_h));
    if keep.0 < top {
        top = keep.0;
    } else if keep.1 >= top + list_h {
        top = keep.1 + 1 - list_h;
    }
    state.top.set(top);
    let visible: Vec<Line> = lines.into_iter().skip(top).take(list_h).collect();
    frame.render_widget(
        Paragraph::new(visible),
        Rect {
            height: list_h as u16,
            ..area
        },
    );
    let note_row = if fits {
        list_h + 1
    } else {
        height.saturating_sub(1)
    };
    if note_row < height {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(" "),
                Span::styled(trunc(NOTE, width.saturating_sub(2)), theme.dim()),
            ])),
            Rect {
                y: area.y + note_row as u16,
                height: 1,
                ..area
            },
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------------------------

/// What a key that does not apply to this kind of setting points to.
fn wrong_key(f: &ConfigField) -> Action {
    Action::Status(match f.kind {
        FieldKind::Choice(_) => format!("{}: h / l choose, Enter types another value", f.label),
        FieldKind::Bool => format!("{} is on or off: Space switches it", f.label),
        FieldKind::Number | FieldKind::Text => {
            format!("{} is typed: Enter opens the input", f.label)
        }
    })
}

pub fn handle_key(key: KeyEvent, snap: &Snapshot, state: &mut State) -> Action {
    state.error = None;
    if let Some(i) = move_selection(&key, state.selected, rows(snap)) {
        state.selected = i;
        return Action::None;
    }
    let Some(f) = fields().get(selected(snap, state)) else {
        return Action::None;
    };
    let cur = current(snap, f);
    let set = |value: &str| Action::SetConfig(f.key.to_string(), value.to_string());
    match key.code {
        KeyCode::Char('h' | 'l') | KeyCode::Left | KeyCode::Right => {
            let step = if matches!(key.code, KeyCode::Char('h') | KeyCode::Left) {
                -1
            } else {
                1
            };
            match f.kind {
                FieldKind::Choice(choices) => {
                    let name = match &cur {
                        Current::Value(v) => Some(v.as_str()),
                        _ => None,
                    };
                    set(step_choice(choices, name, step))
                }
                _ => wrong_key(f),
            }
        }
        KeyCode::Char(' ') => match f.kind {
            FieldKind::Bool => set(if cur == Current::Value("true".into()) {
                "false"
            } else {
                "true"
            }),
            _ => wrong_key(f),
        },
        KeyCode::Enter => match f.kind {
            FieldKind::Bool => wrong_key(f),
            _ => Action::OpenInput {
                prompt: format!("set {}", f.label),
                buf: match cur {
                    Current::Value(v) => v,
                    _ => String::new(),
                },
                target: InputTarget::Config(f.key.to_string()),
            },
        },
        KeyCode::Char('d') => {
            if at_default(snap, f) {
                return Action::Status(format!("{} is already at its default", f.label));
            }
            let then = if unsettable_keys().contains(&f.key) {
                Action::UnsetConfig(f.key.to_string())
            } else {
                match f.default_value() {
                    Some(default) => Action::SetConfig(f.key.to_string(), default),
                    None => Action::UnsetConfig(f.key.to_string()),
                }
            };
            Action::Confirm {
                what: format!("put {} back to its default ({})?", f.label, show_default(f)),
                then: Box::new(then),
            }
        }
        _ => Action::None,
    }
}
