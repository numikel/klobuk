//! Symbols and every `Style` of the TUI, in one place. Views never write a literal colour: they ask
//! the [`Theme`], and `NO_COLOR` is handled here and only here.
//!
//! A symbol always differs per state, so colour is never the only signal. With colours off the theme
//! keeps bold, dim and reverse video (which is how the selected row is marked then).

use std::ffi::OsStr;

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

    /// The legend line of the help view, built from the same glyphs as the rows.
    pub const LEGEND: [(Mark, &'static str); 5] = [
        (Mark::Working, "working"),
        (Mark::Needs, "needs you"),
        (Mark::Review, "being checked"),
        (Mark::Done, "done"),
        (Mark::Waiting, "waiting"),
    ];
}

/// The colour scheme (or the lack of one). Cheap to copy; the app owns one and hands it to every view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    color: bool,
}

impl Default for Theme {
    fn default() -> Theme {
        Theme::new(true)
    }
}

impl Theme {
    pub fn new(color: bool) -> Theme {
        Theme { color }
    }

    /// Colours unless the environment asks for none (`NO_COLOR`).
    pub fn from_env() -> Theme {
        Theme::new(!no_color_from_env())
    }

    /// `style` when colours are on, the colourless `plain` otherwise.
    fn pick(&self, coloured: Style, plain: Style) -> Style {
        if self.color {
            coloured
        } else {
            plain
        }
    }

    /// Section titles and issue ids.
    pub fn bold(&self) -> Style {
        Style::default().add_modifier(Modifier::BOLD)
    }

    /// Secondary text (a modifier, so it survives `NO_COLOR`).
    pub fn dim(&self) -> Style {
        Style::default().add_modifier(Modifier::DIM)
    }

    /// Emphasis colour: the active tab, borders of the overlays, the busy spinner.
    pub fn accent(&self) -> Style {
        self.pick(Style::default().fg(Color::Cyan), Style::default())
    }

    /// The background action that runs (the spinner in the header): accent and bold.
    pub fn busy(&self) -> Style {
        self.accent().add_modifier(Modifier::BOLD)
    }

    /// The open view in the tab bar: accent, reverse video.
    pub fn tab_active(&self) -> Style {
        self.accent().add_modifier(Modifier::REVERSED)
    }

    /// Key letters in hints: accent and bold.
    pub fn key(&self) -> Style {
        self.accent().add_modifier(Modifier::BOLD)
    }

    /// Healthy things (`runtime ok`).
    pub fn ok(&self) -> Style {
        self.pick(Style::default().fg(Color::Green), Style::default())
    }

    /// Things that deserve a look (the waiting banner, the border of a window that asks).
    pub fn warn(&self) -> Style {
        self.pick(
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            self.bold(),
        )
    }

    /// Needs the human or failed: red and bold.
    pub fn danger(&self) -> Style {
        self.pick(
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            self.bold(),
        )
    }

    /// The selected row: a highlighted background, reverse video without colours.
    pub fn selected(&self) -> Style {
        self.pick(
            Style::default().bg(Color::Indexed(238)),
            Style::default().add_modifier(Modifier::REVERSED),
        )
    }

    /// The style of a status symbol.
    pub fn mark(&self, mark: Mark) -> Style {
        match mark {
            Mark::Working => self.ok(),
            Mark::Needs => self.danger(),
            Mark::Review => self.pick(Style::default().fg(Color::Yellow), Style::default()),
            Mark::Done => self.dim(),
            Mark::Waiting => Style::default(),
        }
    }
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
