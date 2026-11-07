//! The one table of key bindings. The footer hints and the help view are both generated from it, so
//! neither can drift from the other. Pure data: the views and `app.rs` implement the behaviour, this
//! file only describes it (a test pins the descriptions to the behaviour for the keys it can drive).
//!
//! The table is assembled from the global bindings here, those of the overlays, and one `BINDINGS`
//! constant per view: a view task edits its own constant and nothing else.

use super::app::View;
use super::overlays;
use super::views::{agents, board, config, inbox, log, now, queue};

/// Where in the help view a binding is listed (the headings of the page, in order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Everywhere,
    NeedsYou,
    Board,
    Inbox,
    Queue,
    Config,
    /// Keys of an open window (typing, a question, a modal): in the footer while it is open, not on the
    /// help page, which describes the keys of the views.
    Window,
}

impl Group {
    /// The groups of the help page, in order.
    pub const PAGE: [Group; 6] = [
        Group::Everywhere,
        Group::NeedsYou,
        Group::Board,
        Group::Inbox,
        Group::Queue,
        Group::Config,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Group::Everywhere => "EVERYWHERE",
            Group::NeedsYou => "WHEN AN AGENT NEEDS YOU",
            Group::Board => "BOARD",
            Group::Inbox => "INBOX",
            Group::Queue => "QUEUE",
            Group::Config => "CONFIG",
            Group::Window => "WINDOWS",
        }
    }
}

/// What the keyboard is currently talking to (the top overlay without its payload).
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
    /// As shown to the user: `Enter`, `j / k`, `Ctrl+C`.
    pub key: &'static str,
    /// The word the footer shows next to the key.
    pub label: &'static str,
    /// The help view's explanation, in plain words (`\n` starts a continuation line).
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

/// One table row; the argument order is the order the fields are written in a `BINDINGS` constant.
pub const fn b(
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

/// Help texts of the keys several views share. A binding that repeats another one's key and text is
/// listed once on the help page, so the views use these instead of writing the sentence again.
pub const HELP_MOVE: &str = "move down / up (arrows work too)";
pub const HELP_OPEN: &str = "open what is selected";
pub const HELP_ANSWER: &str = "answer: shows the agent's question,\nthen 1-4, Enter or t to reply";
pub const HELP_FOCUS: &str = "focus the agent's own Herdr pane";
pub const HELP_REPLY: &str = "send the agent a message";
pub const HELP_VERIFY: &str = "check a failed run again";
pub const HELP_ABORT: &str = "stop the agent (asks first)";

/// Every view.
pub const ANY: &[View] = &[];
/// The keyboard talks to the views (no overlay is open).
pub const NORMAL: &[ModeKind] = &[ModeKind::Normal];
/// Every mode.
pub const EVERY_MODE: &[ModeKind] = &[
    ModeKind::Normal,
    ModeKind::Input,
    ModeKind::Confirm,
    ModeKind::Answer,
    ModeKind::AnswerText,
    ModeKind::Comment,
    ModeKind::Help,
];

use Group::{Everywhere, Window};
use ModeKind as M;
use Slot::{Hint, Tail};

/// The bindings of every view and of the windows: handled by `app.rs` before a view sees the key.
pub const GLOBAL: &[Binding] = &[
    b(
        "1-7",
        "views",
        "jump to a view",
        Everywhere,
        ANY,
        NORMAL,
        Slot::None,
    ),
    b(
        "Tab",
        "next view",
        "next view (Shift+Tab: previous)",
        Everywhere,
        ANY,
        NORMAL,
        Hint,
    ),
    b("j / k", "move", HELP_MOVE, Everywhere, ANY, NORMAL, Slot::None),
    b(
        "g / G",
        "first / last",
        "first / last row",
        Everywhere,
        ANY,
        NORMAL,
        Slot::None,
    ),
    b("Enter", "open", HELP_OPEN, Everywhere, ANY, NORMAL, Slot::None),
    b(
        "Esc",
        "back",
        "go back, close a window",
        Everywhere,
        ANY,
        NORMAL,
        Slot::None,
    ),
    b(
        "t",
        "tick",
        "run one factory tick now",
        Everywhere,
        ANY,
        NORMAL,
        Hint,
    ),
    b(
        "R",
        "refresh",
        "reload everything from the store now",
        Everywhere,
        ANY,
        NORMAL,
        Hint,
    ),
    b("?", "help", "this page", Everywhere, ANY, NORMAL, Tail),
    b(
        "q",
        "quit",
        "quit (Ctrl+C works too)",
        Everywhere,
        ANY,
        NORMAL,
        Tail,
    ),
    b(
        "Ctrl+C",
        "quit",
        "quit from anywhere, also while typing",
        Window,
        ANY,
        EVERY_MODE,
        Slot::None,
    ),
    // ---- typing a line ----
    b(
        "Enter",
        "send",
        "send the typed text or save the value",
        Window,
        ANY,
        &[M::Input],
        Hint,
    ),
    b(
        "Esc",
        "cancel",
        "leave the input without sending",
        Window,
        ANY,
        &[M::Input],
        Hint,
    ),
    b(
        "Ctrl+U",
        "clear",
        "clear the whole line",
        Window,
        ANY,
        &[M::Input, M::AnswerText],
        Hint,
    ),
    b(
        "Ctrl+W",
        "del word",
        "delete the last word",
        Window,
        ANY,
        &[M::Input, M::AnswerText],
        Hint,
    ),
    // ---- a question in the footer ----
    b(
        "y",
        "yes",
        "confirm the question in the footer",
        Window,
        ANY,
        &[M::Confirm],
        Hint,
    ),
    b(
        "n / any other key",
        "cancel",
        "cancel the question in the footer",
        Window,
        ANY,
        &[M::Confirm],
        Hint,
    ),
];

/// Every binding, in help order: global, windows, then the views in tab order.
pub fn all() -> impl Iterator<Item = &'static Binding> {
    [
        GLOBAL,
        overlays::BINDINGS,
        now::BINDINGS,
        board::BINDINGS,
        agents::BINDINGS,
        inbox::BINDINGS,
        queue::BINDINGS,
        log::BINDINGS,
        config::BINDINGS,
    ]
    .into_iter()
    .flatten()
}

/// The most context hints a footer shows; with `? help` and `q quit` that makes 7.
pub const FOOTER_CONTEXT_MAX: usize = 5;

/// Hints for the footer of `view` in `mode`: at most [`FOOTER_CONTEXT_MAX`] contextual ones
/// (view-specific before global), then the `Tail` ones.
pub fn footer_hints(view: View, mode: ModeKind) -> Vec<&'static Binding> {
    let applies = |slot: Slot| all().filter(move |x| x.slot == slot && x.applies(view, mode));
    let mut context: Vec<&Binding> = applies(Hint).collect();
    context.sort_by_key(|x| x.views.is_empty()); // stable: table order survives inside each class
    context.truncate(FOOTER_CONTEXT_MAX);
    context.extend(applies(Tail));
    context
}

/// One heading of the help page with its rows: `(key, help)`. A binding that several views share is
/// listed once; a key that means different things in different views is one row, the meaning that
/// holds in the most views first and the others as continuation lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpSection {
    pub title: &'static str,
    pub items: Vec<(&'static str, String)>,
}

/// The help page, grouped in [`Group::PAGE`] order: every binding of the views (keyboard in `Normal`
/// mode), so the page cannot say anything the table does not. The rows of WHEN AN AGENT NEEDS YOU are
/// sorted by key, as the mockup lists them; the other groups keep table order.
pub fn help_sections() -> Vec<HelpSection> {
    Group::PAGE
        .iter()
        .map(|g| {
            // key -> [(how many views it holds in, what it does)], in order of first appearance
            let mut rows: Vec<(&'static str, Vec<(usize, &'static str)>)> = Vec::new();
            for x in all().filter(|x| x.group == *g && x.modes.contains(&ModeKind::Normal)) {
                let reach = if x.views.is_empty() {
                    usize::MAX
                } else {
                    x.views.len()
                };
                match rows.iter_mut().find(|(key, _)| *key == x.key) {
                    Some((_, meanings)) => {
                        // the same sentence in several views is one meaning that holds in all of them
                        match meanings.iter_mut().find(|(_, help)| *help == x.help) {
                            Some((seen, _)) => *seen = seen.saturating_add(reach),
                            None => meanings.push((reach, x.help)),
                        }
                    }
                    None => rows.push((x.key, vec![(reach, x.help)])),
                }
            }
            if *g == Group::NeedsYou {
                rows.sort_by_key(|(key, _)| *key);
            }
            let items = rows
                .into_iter()
                .map(|(key, mut meanings)| {
                    meanings.sort_by_key(|(reach, _)| std::cmp::Reverse(*reach)); // stable
                    let help: Vec<&str> = meanings.into_iter().map(|(_, help)| help).collect();
                    (key, help.join("\n"))
                })
                .collect();
            HelpSection {
                title: g.title(),
                items,
            }
        })
        .filter(|s| !s.items.is_empty())
        .collect()
}
