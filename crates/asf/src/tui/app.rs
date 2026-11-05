//! App state and key handling. Pure: no IO, no terminal, so every transition is unit-testable.

use std::collections::BTreeMap;

use asf_core::config_edit::editable_keys;
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
    /// Every unanswered QUESTION to the human on a live issue, read or not, oldest first.
    pub questions: Vec<Comment>,
    pub queue: Vec<MergeItem>,
    /// Newest first.
    pub events: Vec<Event>,
    /// One row per editable config key, read from `.factory/config.toml`.
    pub config: Vec<ConfigRow>,
}

/// An editable config key with its current value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigRow {
    pub key: String,
    pub description: String,
    /// What the Config view shows (`(unset)` for an optional key that is absent).
    pub value: String,
    /// What the input prompt is prefilled with (empty for an unset key).
    pub raw: String,
}

/// Rows for every [`editable_keys`] entry, with values taken from the TOML text `toml`.
/// A file that does not parse yields `(unreadable)` rows so the view never shows stale defaults.
pub fn config_rows(toml: &str) -> Vec<ConfigRow> {
    let root = Config::from_toml(toml)
        .ok()
        .and_then(|c| serde_json::to_value(&c).ok());
    editable_keys()
        .iter()
        .map(|(key, description)| {
            let (value, raw) = match &root {
                None => ("(unreadable)".to_string(), String::new()),
                Some(root) => match key.split('.').try_fold(root, |node, seg| node.get(seg)) {
                    None | Some(serde_json::Value::Null) => ("(unset)".to_string(), String::new()),
                    Some(serde_json::Value::String(s)) => (s.clone(), s.clone()),
                    Some(other) => (other.to_string(), other.to_string()),
                },
            };
            ConfigRow {
                key: (*key).to_string(),
                description: (*description).to_string(),
                value,
                raw,
            }
        })
        .collect()
}

/// Ids of the QUESTION comments to the human among `comments` that nobody has answered. A question
/// counts as answered when an ANSWER comment replies to it (`--reply-to`) or `resolved_at` is set;
/// the same rule `asf issue finish` applies.
pub fn unanswered_questions(comments: &[Comment]) -> Vec<i64> {
    comments
        .iter()
        .filter(|q| q.kind == CommentType::Question && q.to_lane == LANE_HUMAN && q.resolved_at.is_none())
        .filter(|q| {
            !comments
                .iter()
                .any(|a| a.kind == CommentType::Answer && a.reply_to == Some(q.id))
        })
        .map(|q| q.id)
        .collect()
}

/// Something that needs the human before it can go on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Waiting {
    /// A run in `blocked`: the agent sits on a prompt.
    Run { issue: IssueId, since: String },
    /// An unanswered QUESTION to the human.
    Question {
        issue: IssueId,
        body: String,
        since: String,
    },
}

impl Waiting {
    pub fn since(&self) -> &str {
        match self {
            Waiting::Run { since, .. } | Waiting::Question { since, .. } => since,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum View {
    Board,
    Agents,
    Inbox,
    Queue,
    Log,
    Config,
}

impl View {
    pub const ALL: [View; 6] = [
        View::Board,
        View::Agents,
        View::Inbox,
        View::Queue,
        View::Log,
        View::Config,
    ];

    pub fn title(&self) -> &'static str {
        match self {
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
    /// Write `value` to the dotted config `key` (`.factory/config.toml`).
    SetConfig(String, String),
    /// Remove the optional config `key` from `.factory/config.toml`.
    UnsetConfig(String),
    /// Read the worker's pane so the answer overlay can show what it asks.
    Peek(IssueId),
}

/// What the input line is typing for.
#[derive(Debug, Clone, PartialEq)]
pub enum InputTarget {
    Reply(IssueId),
    Config(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
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
}

/// Board columns, left to right: every status in `Status::ALL` order except `cancelled`, which is hidden.
pub fn board_columns() -> Vec<Status> {
    Status::ALL
        .iter()
        .copied()
        .filter(|s| *s != Status::Cancelled)
        .collect()
}

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
        for st in board_columns() {
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
            View::Config => self.snap.config.len(),
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
            View::Log | View::Config => None,
        }
    }

    pub fn issue(&self, id: &IssueId) -> Option<&Issue> {
        self.snap.issues.iter().find(|i| &i.id == id)
    }

    pub fn run_for(&self, id: &IssueId) -> Option<&Run> {
        self.snap.runs.iter().find(|r| &r.issue_id == id)
    }

    /// Blocked runs and unanswered questions, oldest first (RFC 3339 UTC strings sort by time; a run
    /// counts from its start because the store keeps no "blocked since").
    pub fn waiting(&self) -> Vec<Waiting> {
        let runs = self
            .snap
            .runs
            .iter()
            .filter(|r| r.state == RunState::Blocked)
            .map(|r| Waiting::Run {
                issue: r.issue_id.clone(),
                since: r.started_at.clone(),
            });
        let questions = self.snap.questions.iter().map(|c| Waiting::Question {
            issue: c.issue_id.clone(),
            body: c.body.clone(),
            since: c.created_at.clone(),
        });
        let mut all: Vec<Waiting> = runs.chain(questions).collect();
        all.sort_by(|a, b| a.since().cmp(b.since()));
        all
    }

    /// Feeds the pane text read for the open answer overlay; anything else ignores it.
    pub fn set_screen(&mut self, text: String) {
        if let Mode::Answer { screen, .. } = &mut self.mode {
            *screen = text;
        }
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

    /// Keys inside the answer overlay: nothing else is bound while it is open. Digits, Enter, Up,
    /// Down, `y` and `n` go to the worker pane as keys and close the overlay; `t` types a reply;
    /// Esc closes without sending.
    fn handle_answer_key(
        &mut self,
        key: KeyEvent,
        id: IssueId,
        screen: String,
        text: Option<String>,
    ) -> Action {
        let keys = |name: &str, id: IssueId| Action::Keys(id, vec![name.to_string()]);
        if let Some(mut buf) = text {
            return match key.code {
                KeyCode::Esc => Action::None,
                KeyCode::Enter if !buf.trim().is_empty() => Action::Reply(id, buf.trim().to_string()),
                other => {
                    match other {
                        KeyCode::Backspace => {
                            buf.pop();
                        }
                        KeyCode::Char(c) => buf.push(c),
                        _ => {}
                    }
                    self.mode = Mode::Answer {
                        id,
                        screen,
                        text: Some(buf),
                    };
                    Action::None
                }
            };
        }
        match key.code {
            KeyCode::Esc => Action::None,
            KeyCode::Char(c @ '1'..='9') => keys(&c.to_string(), id.clone()),
            KeyCode::Char(c @ ('y' | 'n')) => keys(&c.to_string(), id.clone()),
            KeyCode::Enter => keys("enter", id.clone()),
            KeyCode::Up => keys("up", id.clone()),
            KeyCode::Down => keys("down", id.clone()),
            KeyCode::Char('t') => {
                self.mode = Mode::Answer {
                    id,
                    screen,
                    text: Some(String::new()),
                };
                Action::None
            }
            _ => {
                self.mode = Mode::Answer {
                    id,
                    screen,
                    text: None,
                };
                Action::None
            }
        }
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
                            let text = buf.trim().to_string();
                            match target {
                                InputTarget::Reply(id) => Action::Reply(id, text),
                                InputTarget::Config(key) => Action::SetConfig(key, text),
                            }
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
            Mode::Answer { id, screen, text } => return self.handle_answer_key(key, id, screen, text),
            Mode::Normal => {}
        }
        match key.code {
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Char('R') => Action::Refresh,
            KeyCode::Char('t') => Action::Tick,
            KeyCode::Char(c @ '1'..='6') => {
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
                (View::Config, _) => {
                    if let Some(row) = self.snap.config.get(self.selected(View::Config)) {
                        self.mode = Mode::Input {
                            prompt: format!("set {}", row.key),
                            buf: row.raw.clone(),
                            target: InputTarget::Config(row.key.clone()),
                        };
                    }
                    Action::None
                }
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
            KeyCode::Char('a') if matches!(self.view, View::Board | View::Agents | View::Inbox) => {
                match self.selected_issue().cloned() {
                    Some(id) if self.run_for(&id).is_some_and(|r| r.state == RunState::Blocked) => {
                        self.mode = Mode::Answer {
                            id: id.clone(),
                            screen: String::new(),
                            text: None,
                        };
                        Action::Peek(id)
                    }
                    Some(id) => {
                        self.status = Some(format!("{id} has no blocked run to answer"));
                        Action::None
                    }
                    None => Action::None,
                }
            }
            KeyCode::Char('d') if self.view == View::Config => {
                let Some(row) = self.snap.config.get(self.selected(View::Config)) else {
                    return Action::None;
                };
                let key = row.key.clone();
                if !asf_core::config_edit::unsettable_keys().contains(&key.as_str()) {
                    self.status = Some(format!("{key} cannot be unset"));
                } else if row.raw.is_empty() {
                    self.status = Some(format!("{key} is already unset"));
                } else {
                    self.mode = Mode::Confirm {
                        what: format!("unset {key}?"),
                        action: Box::new(Action::UnsetConfig(key)),
                    };
                }
                Action::None
            }
            KeyCode::Char('r') => {
                if let Some(id) = self.selected_issue().cloned() {
                    self.mode = Mode::Input {
                        prompt: format!("reply to {id}"),
                        buf: String::new(),
                        target: InputTarget::Reply(id),
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
