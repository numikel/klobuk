//! The one table of key bindings. The footer hints and the help view are both generated from it, so
//! neither can drift from the other. Pure data: `app.rs` implements the behaviour, this file only
//! describes it (a test in `tests.rs` pins the descriptions to the behaviour for the keys it can drive).

use super::app::{Mode, View};

/// Where in the help view a binding is listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Global,
    Navigation,
    Board,
    Agents,
    Inbox,
    Queue,
    Config,
    AnswerOverlay,
}

impl Group {
    pub const ALL: [Group; 8] = [
        Group::Global,
        Group::Navigation,
        Group::Board,
        Group::Agents,
        Group::Inbox,
        Group::Queue,
        Group::Config,
        Group::AnswerOverlay,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Group::Global => "Global",
            Group::Navigation => "Navigation",
            Group::Board => "Board",
            Group::Agents => "Agents",
            Group::Inbox => "Inbox",
            Group::Queue => "Queue",
            Group::Config => "Config",
            Group::AnswerOverlay => "Answer overlay",
        }
    }
}

/// What the keyboard is currently talking to (a [`Mode`] without its payload).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeKind {
    Normal,
    Input,
    Confirm,
    /// The answer overlay while it waits for a digit / key.
    Answer,
    /// The answer overlay while a free-text reply is typed.
    AnswerText,
    /// The full-comment modal of the Inbox.
    Comment,
    Help,
}

impl ModeKind {
    #[cfg(test)]
    pub const ALL: [ModeKind; 7] = [
        ModeKind::Normal,
        ModeKind::Input,
        ModeKind::Confirm,
        ModeKind::Answer,
        ModeKind::AnswerText,
        ModeKind::Comment,
        ModeKind::Help,
    ];
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

/// Whether and where a binding is advertised in the footer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// Only in the help view.
    None,
    /// In the footer while it applies (view-specific hints come before global ones).
    Hint,
    /// Always the last hints of the footer, in table order (`? help`, `q quit`).
    Tail,
}

#[derive(Debug, Clone, Copy)]
pub struct Binding {
    /// As shown to the user: `Enter`, `j / Down`, `Ctrl+C`.
    pub key: &'static str,
    /// The word the footer shows next to the key.
    pub label: &'static str,
    /// The help view's explanation, in plain words.
    pub help: &'static str,
    pub group: Group,
    /// Views the binding applies in; empty = every view.
    pub views: &'static [View],
    pub modes: &'static [ModeKind],
    pub slot: Slot,
}

impl Binding {
    pub fn applies(&self, view: View, mode: ModeKind) -> bool {
        self.modes.contains(&mode) && (self.views.is_empty() || self.views.contains(&view))
    }
}

const fn b(
    key: &'static str,
    label: &'static str,
    help: &'static str,
    group: Group,
    views: &'static [View],
    modes: &'static [ModeKind],
    slot: Slot,
) -> Binding {
    Binding {
        key,
        label,
        help,
        group,
        views,
        modes,
        slot,
    }
}

use Group::*;
use ModeKind as M;
use Slot::{Hint, Tail};

const ANY: &[View] = &[];
const NORMAL: &[ModeKind] = &[M::Normal];
const BOARD: &[View] = &[View::Board];
const AGENTS: &[View] = &[View::Agents];
const INBOX: &[View] = &[View::Inbox];
const QUEUE: &[View] = &[View::Queue];
const CONFIG: &[View] = &[View::Config];

/// Every binding, in help-view order. The footer takes the `Hint` ones that apply, view-specific first.
pub const BINDINGS: &[Binding] = &[
    // ---- Global ----
    b("?", "help", "open this key reference", Global, ANY, NORMAL, Tail),
    b("q", "quit", "quit the TUI", Global, ANY, NORMAL, Tail),
    b(
        "Ctrl+C",
        "quit",
        "quit from anywhere, also while typing",
        Global,
        ANY,
        &[
            M::Normal,
            M::Input,
            M::Confirm,
            M::Answer,
            M::AnswerText,
            M::Comment,
            M::Help,
        ],
        Slot::None,
    ),
    b(
        "1 - 6",
        "views",
        "jump to a view: Board, Agents, Inbox, Queue, Log, Config",
        Global,
        ANY,
        NORMAL,
        Slot::None,
    ),
    b(
        "t",
        "tick",
        "run one tick in the background (watch, merge queue, promote)",
        Global,
        ANY,
        NORMAL,
        Hint,
    ),
    b(
        "R",
        "refresh",
        "reload everything from the store now",
        Global,
        ANY,
        NORMAL,
        Hint,
    ),
    b(
        "Tab / Shift+Tab",
        "views",
        "next / previous view",
        Global,
        ANY,
        NORMAL,
        Hint,
    ),
    b(
        "Enter",
        "send",
        "send the typed text or save the value",
        Global,
        ANY,
        &[M::Input],
        Hint,
    ),
    b(
        "Esc",
        "cancel",
        "leave the input without sending",
        Global,
        ANY,
        &[M::Input],
        Hint,
    ),
    b(
        "Ctrl+U",
        "clear",
        "clear the whole line",
        Global,
        ANY,
        &[M::Input, M::AnswerText],
        Hint,
    ),
    b(
        "Ctrl+W",
        "del word",
        "delete the last word",
        Global,
        ANY,
        &[M::Input, M::AnswerText],
        Hint,
    ),
    b(
        "y",
        "yes",
        "confirm the question in the footer",
        Global,
        ANY,
        &[M::Confirm],
        Hint,
    ),
    b(
        "n / any other key",
        "cancel",
        "cancel the question in the footer",
        Global,
        ANY,
        &[M::Confirm],
        Hint,
    ),
    b(
        "j / k",
        "scroll",
        "scroll the key reference",
        Global,
        ANY,
        &[M::Help],
        Hint,
    ),
    b(
        "Esc / ? / q",
        "close",
        "close the key reference",
        Global,
        ANY,
        &[M::Help],
        Hint,
    ),
    // ---- Navigation ----
    b(
        "j / Down",
        "down",
        "move the selection down",
        Navigation,
        ANY,
        NORMAL,
        Slot::None,
    ),
    b(
        "k / Up",
        "up",
        "move the selection up",
        Navigation,
        ANY,
        NORMAL,
        Slot::None,
    ),
    b(
        "g",
        "first",
        "jump to the first row",
        Navigation,
        ANY,
        NORMAL,
        Slot::None,
    ),
    b(
        "G",
        "last",
        "jump to the last row",
        Navigation,
        ANY,
        NORMAL,
        Slot::None,
    ),
    // ---- Board ----
    b(
        "Enter",
        "detail",
        "show or hide the detail panel",
        Board,
        BOARD,
        NORMAL,
        Hint,
    ),
    b(
        "Esc",
        "close detail",
        "hide the detail panel",
        Board,
        BOARD,
        NORMAL,
        Slot::None,
    ),
    b(
        "s",
        "start",
        "start a worker on the selected ready issue (background)",
        Board,
        BOARD,
        NORMAL,
        Hint,
    ),
    b(
        "a",
        "answer",
        "answer a worker that waits for you (blocked runs only)",
        Board,
        BOARD,
        NORMAL,
        Hint,
    ),
    b(
        "m",
        "merge",
        "put the selected issue in the merge queue",
        Board,
        BOARD,
        NORMAL,
        Hint,
    ),
    b(
        "f",
        "focus",
        "focus the worker pane in Herdr",
        Board,
        BOARD,
        NORMAL,
        Slot::None,
    ),
    b(
        "r",
        "reply",
        "type a reply to the selected issue's worker",
        Board,
        BOARD,
        NORMAL,
        Slot::None,
    ),
    b(
        "x",
        "abort",
        "abort the selected issue's worker (asks first)",
        Board,
        BOARD,
        NORMAL,
        Slot::None,
    ),
    // ---- Agents ----
    b(
        "f",
        "focus",
        "focus the worker pane in Herdr",
        Agents,
        AGENTS,
        NORMAL,
        Hint,
    ),
    b(
        "a",
        "answer",
        "answer a worker that waits for you (blocked runs only)",
        Agents,
        AGENTS,
        NORMAL,
        Hint,
    ),
    b(
        "r",
        "reply",
        "type a reply to the selected worker",
        Agents,
        AGENTS,
        NORMAL,
        Hint,
    ),
    b(
        "x",
        "abort",
        "abort the selected worker (asks first)",
        Agents,
        AGENTS,
        NORMAL,
        Hint,
    ),
    b(
        "m",
        "merge",
        "put the selected issue in the merge queue",
        Agents,
        AGENTS,
        NORMAL,
        Slot::None,
    ),
    // ---- Inbox ----
    b(
        "Enter",
        "open",
        "open the selected comment in full",
        Inbox,
        INBOX,
        NORMAL,
        Hint,
    ),
    b(
        "m",
        "mark read",
        "mark this issue's messages as read",
        Inbox,
        INBOX,
        NORMAL,
        Hint,
    ),
    b(
        "M",
        "mark all",
        "mark every message read (asks first)",
        Inbox,
        INBOX,
        NORMAL,
        Hint,
    ),
    b(
        "a",
        "answer",
        "answer a worker that waits for you (blocked runs only)",
        Inbox,
        INBOX,
        NORMAL,
        Hint,
    ),
    b(
        "r",
        "reply",
        "type a reply to the issue's worker",
        Inbox,
        INBOX,
        NORMAL,
        Hint,
    ),
    b(
        "j / k",
        "scroll",
        "scroll the open comment",
        Inbox,
        INBOX,
        &[M::Comment],
        Hint,
    ),
    b(
        "Esc",
        "close",
        "close the open comment (nothing is marked read)",
        Inbox,
        INBOX,
        &[M::Comment],
        Hint,
    ),
    // ---- Queue ----
    b(
        "f",
        "focus",
        "focus the worker pane in Herdr",
        Queue,
        QUEUE,
        NORMAL,
        Hint,
    ),
    b(
        "r",
        "reply",
        "type a reply to the issue's worker",
        Queue,
        QUEUE,
        NORMAL,
        Hint,
    ),
    b(
        "x",
        "abort",
        "abort the issue's worker (asks first)",
        Queue,
        QUEUE,
        NORMAL,
        Hint,
    ),
    // ---- Config ----
    b(
        "Enter",
        "edit",
        "edit the selected key (saved to .factory/config.toml)",
        Config,
        CONFIG,
        NORMAL,
        Hint,
    ),
    b(
        "d",
        "unset",
        "remove an optional key such as worker.model (asks first)",
        Config,
        CONFIG,
        NORMAL,
        Hint,
    ),
    // ---- Answer overlay ----
    b(
        "1 - 9",
        "send",
        "send that number key to the worker",
        AnswerOverlay,
        ANY,
        &[M::Answer],
        Hint,
    ),
    b(
        "Enter / Up / Down",
        "send",
        "send that key to the worker",
        AnswerOverlay,
        ANY,
        &[M::Answer],
        Hint,
    ),
    b(
        "y / n",
        "send",
        "send y or n to the worker",
        AnswerOverlay,
        ANY,
        &[M::Answer],
        Hint,
    ),
    b(
        "t",
        "type",
        "type a free-text reply instead",
        AnswerOverlay,
        ANY,
        &[M::Answer],
        Hint,
    ),
    b(
        "Esc",
        "close",
        "close the overlay without sending anything",
        AnswerOverlay,
        ANY,
        &[M::Answer],
        Hint,
    ),
    b(
        "Enter",
        "send",
        "send the typed reply",
        AnswerOverlay,
        ANY,
        &[M::AnswerText],
        Hint,
    ),
    b(
        "Esc",
        "close",
        "close the overlay without sending",
        AnswerOverlay,
        ANY,
        &[M::AnswerText],
        Hint,
    ),
];

/// The most context hints a footer shows; with `? help` and `q quit` that makes 7.
pub const FOOTER_CONTEXT_MAX: usize = 5;

/// Hints for the footer of `view` in `mode`: at most [`FOOTER_CONTEXT_MAX`] contextual ones
/// (view-specific before global), then the `Tail` ones.
pub fn footer_hints(view: View, mode: ModeKind) -> Vec<&'static Binding> {
    let applies = |slot: Slot| {
        BINDINGS
            .iter()
            .filter(move |x| x.slot == slot && x.applies(view, mode))
    };
    let mut context: Vec<&Binding> = applies(Hint).collect();
    context.sort_by_key(|x| x.views.is_empty()); // stable: table order survives inside each class
    context.truncate(FOOTER_CONTEXT_MAX);
    context.extend(applies(Tail));
    context
}

/// One line of the help view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelpLine {
    Heading(&'static str),
    Item { key: &'static str, help: &'static str },
    Blank,
}

/// The whole help view, grouped in [`Group::ALL`] order.
pub fn help_lines() -> Vec<HelpLine> {
    let mut out = Vec::new();
    for g in Group::ALL {
        if !out.is_empty() {
            out.push(HelpLine::Blank);
        }
        out.push(HelpLine::Heading(g.title()));
        out.extend(BINDINGS.iter().filter(|x| x.group == g).map(|x| HelpLine::Item {
            key: x.key,
            help: x.help,
        }));
    }
    out
}
