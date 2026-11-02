//! App state and key handling. Pure: no IO, no terminal, so every transition is unit-testable.

use std::collections::BTreeMap;

use asf_core::*;
use ratatui::crossterm::event::{KeyCode, KeyEvent};

/// Everything the TUI shows, loaded in one pass (`tui::load_snapshot`).
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub repo_name: String,
    pub branch: String,
    /// `None` = runtime not probed, `Some(false)` = unhealthy.
    pub runtime_ok: Option<bool>,
    pub max_parallel: usize,
    pub issues: Vec<Issue>,
    pub claims: Vec<Claim>,
    /// Active runs only.
    pub runs: Vec<Run>,
    pub agents: BTreeMap<IssueId, AgentInfo>,
    /// Unread comments addressed to the human (peeked).
    pub inbox: Vec<Comment>,
    pub queue: Vec<MergeItem>,
    /// Newest first.
    pub events: Vec<Event>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum View {
    Board,
    Agents,
    Inbox,
    Queue,
    Log,
}

impl View {
    pub const ALL: [View; 5] = [View::Board, View::Agents, View::Inbox, View::Queue, View::Log];

    pub fn title(&self) -> &'static str {
        match self {
            View::Board => "Board",
            View::Agents => "Agents",
            View::Inbox => "Inbox",
            View::Queue => "Queue",
            View::Log => "Log",
        }
    }

    pub fn parse(s: &str) -> Option<View> {
        View::ALL
            .iter()
            .copied()
            .find(|v| v.title().eq_ignore_ascii_case(s))
    }
}

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
    MarkRead(IssueId),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Normal,
    Input {
        prompt: String,
        buf: String,
        target: IssueId,
    },
    Confirm {
        what: String,
        action: Box<Action>,
    },
}

/// Board columns, left to right (cancelled is hidden).
pub const BOARD_COLUMNS: [Status; 6] = [
    Status::Backlog,
    Status::Ready,
    Status::InProgress,
    Status::Blocked,
    Status::InReview,
    Status::Done,
];

pub struct App {
    pub view: View,
    pub mode: Mode,
    sel: BTreeMap<View, usize>,
    pub detail: bool,
    pub status: Option<String>,
    pub snap: Snapshot,
}

impl App {
    pub fn new(snap: Snapshot) -> App {
        App {
            view: View::Board,
            mode: Mode::Normal,
            sel: BTreeMap::new(),
            detail: true,
            status: None,
            snap,
        }
    }

    /// Swaps the data and keeps every selection inside the new bounds.
    pub fn replace(&mut self, snap: Snapshot) {
        self.snap = snap;
        for v in View::ALL {
            let n = self.rows(v);
            let s = self.sel.entry(v).or_insert(0);
            if n == 0 {
                *s = 0;
            } else if *s >= n {
                *s = n - 1;
            }
        }
    }

    /// Issues in board order: column by column, by sequence number inside a column.
    pub fn board_issues(&self) -> Vec<&Issue> {
        let mut out = Vec::new();
        for st in BOARD_COLUMNS {
            let mut col: Vec<&Issue> = self.snap.issues.iter().filter(|i| i.status == st).collect();
            col.sort_by(|a, b| a.id.cmp(&b.id));
            out.extend(col);
        }
        out
    }

    pub fn rows(&self, view: View) -> usize {
        match view {
            View::Board => self.board_issues().len(),
            View::Agents => self.snap.runs.len(),
            View::Inbox => self.snap.inbox.len(),
            View::Queue => self.snap.queue.len(),
            View::Log => self.snap.events.len(),
        }
    }

    pub fn selected(&self, view: View) -> usize {
        self.sel
            .get(&view)
            .copied()
            .unwrap_or(0)
            .min(self.rows(view).saturating_sub(1))
    }

    pub fn selected_issue(&self) -> Option<&IssueId> {
        let i = self.selected(self.view);
        match self.view {
            View::Board => self.board_issues().get(i).map(|x| &x.id),
            View::Agents => self.snap.runs.get(i).map(|r| &r.issue_id),
            View::Inbox => self.snap.inbox.get(i).map(|c| &c.issue_id),
            View::Queue => self.snap.queue.get(i).map(|m| &m.issue_id),
            View::Log => None,
        }
    }

    pub fn issue(&self, id: &IssueId) -> Option<&Issue> {
        self.snap.issues.iter().find(|i| &i.id == id)
    }

    pub fn run_for(&self, id: &IssueId) -> Option<&Run> {
        self.snap.runs.iter().find(|r| &r.issue_id == id)
    }

    fn move_sel(&mut self, delta: i64) {
        let n = self.rows(self.view) as i64;
        if n == 0 {
            return;
        }
        let cur = self.selected(self.view) as i64;
        let next = (cur + delta).clamp(0, n - 1) as usize;
        self.sel.insert(self.view, next);
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Input {
                prompt,
                mut buf,
                target,
            } => {
                return match key.code {
                    KeyCode::Esc => Action::None,
                    KeyCode::Enter => {
                        if buf.trim().is_empty() {
                            Action::None
                        } else {
                            Action::Reply(target, buf.trim().to_string())
                        }
                    }
                    KeyCode::Backspace => {
                        buf.pop();
                        self.mode = Mode::Input { prompt, buf, target };
                        Action::None
                    }
                    KeyCode::Char(c) => {
                        buf.push(c);
                        self.mode = Mode::Input { prompt, buf, target };
                        Action::None
                    }
                    _ => {
                        self.mode = Mode::Input { prompt, buf, target };
                        Action::None
                    }
                };
            }
            Mode::Confirm { action, .. } => {
                return match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => *action,
                    _ => Action::None,
                };
            }
            Mode::Normal => {}
        }
        match key.code {
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Char('R') => Action::Refresh,
            KeyCode::Char('t') => Action::Tick,
            KeyCode::Char(c @ '1'..='5') => {
                self.view = View::ALL[(c as u8 - b'1') as usize];
                Action::None
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.move_sel(1);
                Action::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.move_sel(-1);
                Action::None
            }
            KeyCode::Enter => match (self.view, self.selected_issue().cloned()) {
                (View::Inbox, Some(id)) => Action::MarkRead(id),
                _ => {
                    self.detail = !self.detail;
                    Action::None
                }
            },
            KeyCode::Char('s') => match self.selected_issue().cloned() {
                Some(id) => match self.issue(&id).map(|i| i.status) {
                    Some(Status::Ready) => Action::Start(id),
                    Some(st) => {
                        self.status = Some(format!("{id} is {st}; only ready issues can be started"));
                        Action::None
                    }
                    None => Action::None,
                },
                None => Action::None,
            },
            KeyCode::Char('m') => self
                .selected_issue()
                .cloned()
                .map(Action::Enqueue)
                .unwrap_or(Action::None),
            KeyCode::Char('f') => self
                .selected_issue()
                .cloned()
                .map(Action::Focus)
                .unwrap_or(Action::None),
            KeyCode::Char(c @ ('y' | 'n')) => self
                .selected_issue()
                .cloned()
                .map(|id| Action::Keys(id, vec![c.to_string()]))
                .unwrap_or(Action::None),
            KeyCode::Char('r') => {
                if let Some(id) = self.selected_issue().cloned() {
                    self.mode = Mode::Input {
                        prompt: format!("reply to {id}"),
                        buf: String::new(),
                        target: id,
                    };
                }
                Action::None
            }
            KeyCode::Char('x') => {
                if let Some(id) = self.selected_issue().cloned() {
                    if self.run_for(&id).is_some() {
                        self.mode = Mode::Confirm {
                            what: format!("abort {id}?"),
                            action: Box::new(Action::Abort(id)),
                        };
                    } else {
                        self.status = Some(format!("{id} has no active run"));
                    }
                }
                Action::None
            }
            _ => Action::None,
        }
    }
}
