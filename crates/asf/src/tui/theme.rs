//! Status symbols and the `NO_COLOR` rule, shared by the Now view, the Board and the Agents view.
//!
//! The symbol always differs per state, so colour is never the only signal; with `NO_COLOR` set the
//! renderer strips every foreground and background colour and keeps bold, dim and reverse.

use std::ffi::OsStr;

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier, Style};

/// What a row of work is doing, as the operator reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// A worker is running: green `●`.
    Working,
    /// Needs the human or failed: red bold `■`.
    Needs,
    /// Validating or in review: yellow `◆`.
    Review,
    /// Closed: dim `✓`.
    Done,
    /// Waiting for its turn: `○` in the default colour.
    Waiting,
}

impl Mark {
    pub fn glyph(self) -> &'static str {
        match self {
            Mark::Working => "●",
            Mark::Needs => "■",
            Mark::Review => "◆",
            Mark::Done => "✓",
            Mark::Waiting => "○",
        }
    }

    pub fn style(self) -> Style {
        match self {
            Mark::Working => Style::default().green(),
            Mark::Needs => Style::default().red().bold(),
            Mark::Review => Style::default().yellow(),
            Mark::Done => dim(),
            Mark::Waiting => Style::default(),
        }
    }
}

/// The dim text style (a modifier, so it survives `NO_COLOR`).
pub fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

/// Whether `NO_COLOR` (the value as read from the environment) asks for no colours: set and non-empty,
/// as <https://no-color.org> defines it.
pub fn no_color_requested(value: Option<&OsStr>) -> bool {
    value.is_some_and(|v| !v.is_empty())
}

/// Reads `NO_COLOR` from the process environment.
pub fn no_color_from_env() -> bool {
    no_color_requested(std::env::var_os("NO_COLOR").as_deref())
}

/// Removes every foreground and background colour from `buf`; bold, dim, reverse and the like stay.
pub fn strip_colors(buf: &mut Buffer) {
    for pos in buf.area.positions() {
        let cell = &mut buf[pos];
        cell.set_fg(Color::Reset);
        cell.set_bg(Color::Reset);
    }
}
