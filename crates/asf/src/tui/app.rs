//! `App`: the current view, the state of each of the seven views, the overlay stack and the status
//! message, plus the dispatch of keys and drawing to them. Pure: no IO, no terminal, so every
//! transition is unit-testable. What a key wants done outside the app is an [`Action`].

use std::cell::Cell;
use std::time::Duration;

use asf_core::*;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::Frame;

use super::overlays::{self, is_chord, is_ctrl_c, InputTarget, Mode};
use super::snapshot::{IssueDetail, PanePreview, Snapshot};
use super::theme::Theme;
use super::views::{agents, board, config, inbox, log, now, queue};

/// The questions the services answer (`serve.rs` shares the rule with `asf issue finish`).
pub use super::snapshot::unanswered_questions;

/// How long a status message stays in the footer when no key is pressed.
pub const STATUS_TTL: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum View {
    /// The operator's desk: what needs the human, what runs, what is next. The first view.
    Now,
    Board,
    Agents,
    Inbox,
    Queue,
    Log,
    Config,
}

impl View {
    pub const ALL: [View; 7] = [
        View::Now,
        View::Board,
        View::Agents,
        View::Inbox,
        View::Queue,
        View::Log,
        View::Config,
    ];

    pub fn title(&self) -> &'static str {
        match self {
            View::Now => "Now",
            View::Board => "Board",
            View::Agents => "Agents",
            View::Inbox => "Inbox",
            View::Queue => "Queue",
            View::Log => "Log",
            View::Config => "Config",
        }
    }

    pub fn parse(s: &str) -> Option<View> {
        View::ALL
            .iter()
            .copied()
            .find(|v| v.title().eq_ignore_ascii_case(s))
    }
}

/// What a key asks for. The first group needs IO and goes back to the event loop (`tui/mod.rs`); the
/// second only changes the app (a window, a message, the view) and never leaves [`App::handle_key`].
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    None,
    Quit,
    Refresh,
    Tick,
    Start(IssueId),
    Focus(IssueId),
    Reply(IssueId, String),
    Keys(IssueId, Vec<String>),
    Enqueue(IssueId),
    Abort(IssueId),
    /// Mark the human's unread comments on one issue as read.
    MarkRead(IssueId),
    /// Mark the whole human inbox as read (confirmed with `y` first).
    MarkAllRead,
    /// Write `value` to the dotted config `key` (`.factory/config.toml`).
    SetConfig(String, String),
    /// Remove the optional config `key` from `.factory/config.toml`.
    UnsetConfig(String),
    /// Read the last lines of the worker's pane: the answer overlay shows them, `Snapshot::pane_preview`
    /// keeps them.
    Peek(IssueId),
    /// Post an ANSWER comment from the human that replies to QUESTION comment `.1` on the issue.
    Answer(IssueId, i64, String),
    /// Rerun verification of the issue's run in the background (`asf run verify <id>`).
    Verify(IssueId),
    /// Re-queue the issue's failed or blocked merge item in the background (`asf merge retry <id>`).
    Retry(IssueId),
    /// Load the comments and dependency status of an issue into `Snapshot::detail`.
    LoadDetail(IssueId),

    // ---- handled by the app itself ----
    /// Show a message in the footer.
    Status(String),
    /// Open the answer overlay for the issue's worker (then `Peek` reads its pane).
    OpenAnswer(IssueId),
    /// Open the one-line text input.
    OpenInput {
        prompt: String,
        buf: String,
        target: InputTarget,
    },
    /// Ask `what` in the footer; `y` runs `then`, any other key cancels.
    Confirm {
        what: String,
        then: Box<Action>,
    },
    /// Open one comment in full.
    OpenComment(Comment),
    /// Switch to the Board with the issue selected.
    OpenOnBoard(IssueId),
}

impl Action {
    /// Whether the action starts a background child process (only one runs at a time).
    fn is_background(&self) -> bool {
        matches!(
            self,
            Action::Tick | Action::Start(_) | Action::Verify(_) | Action::Retry(_)
        )
    }
}

static NO_OVERLAY: Mode = Mode::Normal;

pub struct App {
    pub view: View,
    /// The seven view states.
    pub now: now::State,
    pub board: board::State,
    pub agents: agents::State,
    pub inbox: inbox::State,
    pub queue: queue::State,
    pub log: log::State,
    pub config: config::State,
    /// Open windows, the top one last; the keyboard talks to the top one.
    overlays: Vec<Mode>,
    /// Shown in the footer until [`STATUS_TTL`] passes (see [`App::advance`]) or the next key press.
    pub status: Option<String>,
    pub snap: Snapshot,
    /// Label of the background action that is running (`tick`, `starting asf-25`); the header
    /// spins next to it. The event loop sets and clears it; `handle_key` refuses a second request.
    pub busy: Option<String>,
    /// Time since the TUI started, as supplied by the event loop; the app never reads a clock.
    pub clock: Duration,
    /// The message the expiry timer is running for, and when it first appeared.
    status_stamp: Option<(String, Duration)>,
    /// Largest useful scroll of the open window, written by the renderer (it alone knows the wrapped
    /// height), read when scrolling so `j` cannot run past the end.
    pub(crate) scroll_max: Cell<u16>,
    /// The colours (or the lack of them); the entry points build it from `NO_COLOR`.
    pub theme: Theme,
    /// The detail in the snapshot was carried over a store refresh and may be out of date: a comment
    /// or a dependency status can have changed under the selection. Cleared by [`App::set_detail`].
    detail_stale: bool,
    /// The pane preview in the snapshot was carried over a store refresh; [`App::peek_wanted`] asks
    /// for it again. Cleared by [`App::set_pane_preview`].
    pane_stale: bool,
}

impl App {
    /// A fresh app on the Now view, in colour (the entry points apply `NO_COLOR`).
    pub fn new(mut snap: Snapshot) -> App {
        snap.derive();
        App {
            view: View::Now,
            now: now::State::default(),
            board: board::State::default(),
            agents: agents::State::default(),
            inbox: inbox::State::default(),
            queue: queue::State::default(),
            log: log::State::default(),
            config: config::State::default(),
            overlays: Vec::new(),
            status: None,
            snap,
            busy: None,
            clock: Duration::ZERO,
            status_stamp: None,
            scroll_max: Cell::new(u16::MAX),
            theme: Theme::default(),
            detail_stale: false,
            pane_stale: false,
        }
    }

    /// The window the keyboard talks to, or [`Mode::Normal`] when none is open.
    pub fn mode(&self) -> &Mode {
        self.overlays.last().unwrap_or(&NO_OVERLAY)
    }

    /// Feeds the clock: expires a status message that has been on screen for [`STATUS_TTL`].
    /// A message is stamped the first time it is seen here, so setting `status` needs no clock.
    pub fn advance(&mut self, now: Duration) {
        self.clock = now;
        // A key the answer overlay waits on that the pane never reflects (the dialog moved on without
        // it, or the key did nothing): give up and say so, so the overlay takes keys again.
        if let Some(Mode::Answer { walk, .. }) = self.overlays.last_mut() {
            if let Some(wait) = &mut walk.wait {
                let since = *wait.since.get_or_insert(now);
                if now.saturating_sub(since) >= overlays::WAIT_TTL {
                    walk.wait = None;
                    self.status = Some(
                        "the pane did not change after the key; check it, or press k for key mode".into(),
                    );
                }
            }
        }
        let Some(text) = &self.status else {
            self.status_stamp = None;
            return;
        };
        match &self.status_stamp {
            Some((stamped, since)) if stamped == text => {
                if now.saturating_sub(*since) >= STATUS_TTL {
                    self.status = None;
                    self.status_stamp = None;
                }
            }
            _ => self.status_stamp = Some((text.clone(), now)),
        }
    }

    /// Swaps the data. What was loaded on demand (the Board detail, the pane preview) stays on screen
    /// until the event loop loads it again, so the panel does not blink; a carried-over detail is marked
    /// stale and [`App::detail_wanted`] asks for it again. The views clamp their selections when they
    /// read them.
    pub fn replace(&mut self, mut snap: Snapshot) {
        if snap.detail.is_none() {
            snap.detail = self.snap.detail.take();
            self.detail_stale = snap.detail.is_some();
        }
        if snap.pane_preview.is_none() {
            snap.pane_preview = self.snap.pane_preview.take();
            self.pane_stale = snap.pane_preview.is_some();
        }
        snap.derive();
        self.snap = snap;
    }

    /// Draws the active view into `area` (the rows the chrome leaves).
    pub fn render_view(&self, frame: &mut Frame, area: Rect) {
        let (snap, t) = (&self.snap, &self.theme);
        match self.view {
            View::Now => now::render(frame, area, snap, &self.now, t),
            View::Board => board::render(frame, area, snap, &self.board, t),
            View::Agents => agents::render(frame, area, snap, &self.agents, t),
            View::Inbox => inbox::render(frame, area, snap, &self.inbox, t),
            View::Queue => queue::render(frame, area, snap, &self.queue, t),
            View::Log => log::render(frame, area, snap, &self.log, t),
            View::Config => config::render(frame, area, snap, &self.config, t),
        }
    }

    /// The issue whose Board detail should be loaded: the selected one, while the Board's panel is open
    /// and the snapshot holds the detail of another issue or one from before the last store refresh.
    /// The event loop turns it into a load.
    pub fn detail_wanted(&self) -> Option<IssueId> {
        if self.view != View::Board || !self.board.detail {
            return None;
        }
        board::selected_issue(&self.snap, &self.board)
            .filter(|id| self.detail_stale || self.snap.detail_of(id).is_none())
    }

    /// The issue whose pane the Agents view wants read: the selected run's, while that view is open and
    /// the snapshot holds another pane or one from before the last store refresh. Never while the
    /// runtime is down (the read would fail and be asked for again on every pass). The event loop
    /// turns it into a read.
    pub fn peek_wanted(&self) -> Option<IssueId> {
        if self.snap.runtime_ok == Some(false) {
            return None;
        }
        // The answer overlay sent a key and waits for the pane to move on: read it again on every pass
        // until it does (or the wait runs out, see `advance`), whatever view is behind the overlay.
        if let Some(Mode::Answer { id, walk, .. }) = self.overlays.last() {
            if walk.wait.is_some() {
                return Some(id.clone());
            }
        }
        if self.view != View::Agents {
            return None;
        }
        agents::selected_issue(&self.snap, &self.agents)
            .filter(|id| self.pane_stale || self.snap.preview_of(id).is_none())
    }

    /// Keeps a freshly loaded Board detail in the snapshot.
    pub fn set_detail(&mut self, detail: IssueDetail) {
        self.snap.detail = Some(detail);
        self.detail_stale = false;
    }

    /// The reload of the Board detail failed: say so, and keep what is shown until the next store
    /// refresh asks again (instead of retrying on every pass of the event loop).
    pub fn detail_failed(&mut self, message: String) {
        self.status = Some(format!("detail failed: {message}"));
        self.detail_stale = false;
    }

    /// Feeds the pane text read for the open answer overlay; anything else ignores it. A key the overlay
    /// is waiting on (the next question of a tabbed dialog) is settled by the first text that shows it.
    pub fn set_screen(&mut self, text: String) {
        self.feed_answer(None, text);
    }

    /// Keeps the last lines read from `issue`'s pane for the views (`Snapshot::pane_preview`), and shows
    /// them in the answer overlay when it is open for the same issue.
    pub fn set_pane_preview(&mut self, issue: IssueId, text: String) {
        self.feed_answer(Some(&issue), text.clone());
        self.snap.pane_preview = Some(PanePreview { issue, text });
        self.pane_stale = false;
    }

    fn feed_answer(&mut self, issue: Option<&IssueId>, text: String) {
        if let Some(Mode::Answer { id, screen, walk, .. }) = self.overlays.last_mut() {
            if issue.is_none_or(|i| i == id) {
                overlays::take_screen(screen, walk, text);
            }
        }
    }

    /// Next (`step == 1`) or previous (`step == -1`) view, wrapping around.
    fn cycle_view(&mut self, step: isize) {
        let n = View::ALL.len() as isize;
        let at = View::ALL.iter().position(|v| *v == self.view).unwrap_or(0) as isize;
        self.view = View::ALL[(at + step).rem_euclid(n) as usize];
    }

    /// A background action may only start when none runs; otherwise the status says which one does.
    fn busy_refusal(&mut self) -> bool {
        match &self.busy {
            Some(label) => {
                self.status = Some(format!("busy: {label}"));
                true
            }
            None => false,
        }
    }

    /// Carries out what a view or a window asked for inside the app and returns what is left for the
    /// event loop.
    fn dispatch(&mut self, action: Action) -> Action {
        match action {
            Action::Status(text) => {
                self.status = Some(text);
                Action::None
            }
            Action::OpenAnswer(id) => {
                self.overlays.push(Mode::Answer {
                    id: id.clone(),
                    screen: String::new(),
                    text: None,
                    walk: overlays::Walk::default(),
                });
                Action::Peek(id)
            }
            Action::OpenInput { prompt, buf, target } => {
                self.overlays.push(Mode::Input { prompt, buf, target });
                Action::None
            }
            Action::Confirm { what, then } => {
                self.overlays.push(Mode::Confirm { what, action: then });
                Action::None
            }
            Action::OpenComment(comment) => {
                self.scroll_max.set(u16::MAX);
                self.overlays.push(Mode::Comment { comment, scroll: 0 });
                Action::None
            }
            Action::OpenOnBoard(id) => {
                board::select(&mut self.board, &self.snap, &id);
                self.view = View::Board;
                Action::None
            }
            a if a.is_background() && self.busy_refusal() => Action::None,
            a => a,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        // Any key press retires the status message; a key that has news sets a new one below.
        self.status = None;
        self.status_stamp = None;
        if is_ctrl_c(&key) {
            return Action::Quit;
        }
        if let Some(top) = self.overlays.pop() {
            let (keep, action) = overlays::handle_key(top, key, self.scroll_max.get());
            self.overlays.extend(keep);
            return self.dispatch(action);
        }
        // The Board's and the Log's filters take every key as text, also the ones that are bound
        // elsewhere (q, t, 1-7); the Log's raw JSON window swallows keys like the other modals.
        let typing = match self.view {
            View::Board => self.board.typing(),
            View::Log => self.log.takes_every_key(),
            _ => false,
        };
        if typing {
            let action = self.view_key(key);
            return self.dispatch(action);
        }
        // Ctrl/Alt chords are not bindings (and never text); Ctrl+C was handled above.
        if is_chord(&key) {
            return Action::None;
        }
        match key.code {
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Char('?') => {
                self.scroll_max.set(u16::MAX);
                self.overlays.push(Mode::Help { scroll: 0 });
                Action::None
            }
            KeyCode::Char('R') => Action::Refresh,
            KeyCode::Char('t') => self.dispatch(Action::Tick),
            KeyCode::Char(c @ '1'..='7') => {
                self.view = View::ALL[(c as u8 - b'1') as usize];
                Action::None
            }
            KeyCode::Tab if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.cycle_view(1);
                Action::None
            }
            KeyCode::BackTab | KeyCode::Tab => {
                self.cycle_view(-1);
                Action::None
            }
            _ => {
                let action = self.view_key(key);
                self.dispatch(action)
            }
        }
    }

    /// Hands `key` to the active view.
    fn view_key(&mut self, key: KeyEvent) -> Action {
        let snap = &self.snap;
        match self.view {
            View::Now => now::handle_key(key, snap, &mut self.now),
            View::Board => board::handle_key(key, snap, &mut self.board),
            View::Agents => agents::handle_key(key, snap, &mut self.agents),
            View::Inbox => inbox::handle_key(key, snap, &mut self.inbox),
            View::Queue => queue::handle_key(key, snap, &mut self.queue),
            View::Log => log::handle_key(key, snap, &mut self.log),
            View::Config => config::handle_key(key, snap, &mut self.config),
        }
    }
}
