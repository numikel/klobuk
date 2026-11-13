//! Everything the TUI shows, loaded in one pass, plus the pure helpers the views share (display-width
//! truncation, ages, money, list navigation). No view does any IO: what it needs is in the [`Snapshot`],
//! and what the snapshot cannot hold (the comments of one issue, the screen of one pane) arrives
//! through an `Action` the event loop executes.

use std::collections::BTreeMap;
use std::path::Path;

use asf_core::config_edit::{editable_keys, lookup};
use asf_core::*;
use asf_store::serve_pulse::{self, LoopState, ServeMode};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::text::Span;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

use super::app::Action;
use super::overlays::InputTarget;
use super::theme::Mark;
use crate::ctx::{Ctx, CONFIG_FILE, FACTORY_DIR};
use crate::output::AppError;

/// The event the merge queue appends when a review round starts (`commands/merge.rs`); its payload
/// carries the `round` number.
pub const REVIEW_STARTED: &str = "merge.review_started";

/// How many events the Log view keeps (the newest).
const LOG_LIMIT: usize = 200;

/// Failed runtime probes in a row before the header says the runtime is down. The probe runs every
/// couple of seconds on a machine that may be loaded; one miss is not an outage.
pub const RUNTIME_DOWN_AFTER: u32 = 3;

/// What the header knows about the runtime, carried from one snapshot to the next.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeState {
    pub ok: Option<bool>,
    pub failures: u32,
    pub reason: Option<String>,
}

impl RuntimeState {
    /// The state after one refresh. `probe` is `None` when this refresh did not ask the runtime (the
    /// store is read more often than the runtime is probed): nothing is learned, the state stays.
    /// A failed probe keeps the last known state until [`RUNTIME_DOWN_AFTER`] of them follow each other;
    /// an answer resets the count.
    pub fn observe(&self, probe: Option<Result<(), String>>) -> RuntimeState {
        match probe {
            None => self.clone(),
            Some(Ok(())) => RuntimeState {
                ok: Some(true),
                failures: 0,
                reason: None,
            },
            Some(Err(reason)) => {
                let failures = self.failures.saturating_add(1);
                RuntimeState {
                    ok: if failures >= RUNTIME_DOWN_AFTER {
                        Some(false)
                    } else {
                        self.ok
                    },
                    failures,
                    reason: Some(reason),
                }
            }
        }
    }
}

/// Everything the TUI shows, loaded in one pass ([`load_snapshot`]).
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub repo_name: String,
    pub branch: String,
    /// `None` = no probe has answered yet, `Some(true)` = up, `Some(false)` = [`RUNTIME_DOWN_AFTER`]
    /// probes in a row failed. One slow or failed probe does not change it ([`RuntimeState::observe`]).
    pub runtime_ok: Option<bool>,
    /// Probes that failed in a row, ending with the latest one.
    pub runtime_failures: u32,
    /// The error of the latest failed probe; `None` while the runtime answers.
    pub runtime_reason: Option<String>,
    pub max_parallel: usize,
    pub issues: Vec<Issue>,
    pub claims: Vec<Claim>,
    /// Active runs only.
    pub runs: Vec<Run>,
    /// Every run of every issue, finished ones included: what the Board sums into the cost of an epic.
    pub all_runs: Vec<Run>,
    pub agents: BTreeMap<IssueId, AgentInfo>,
    /// Unread comments addressed to the human (peeked).
    pub inbox: Vec<Comment>,
    /// Every unanswered QUESTION to the human on a live issue, read or not, oldest first.
    pub questions: Vec<Comment>,
    /// Merge items that have not reached a terminal state.
    pub queue: Vec<MergeItem>,
    /// Newest first.
    pub events: Vec<Event>,
    /// One row per editable config key, read from `.factory/config.toml`.
    pub config: Vec<ConfigRow>,
    /// When the snapshot was taken, RFC 3339 UTC (the clock of the ages).
    pub now: String,
    /// Seconds east of UTC of the local time zone, for "today" (0 when it cannot be read).
    pub utc_offset_secs: i32,
    /// The newest run of every live issue whose newest run `failed`; `runs` holds active ones only.
    pub failed_runs: Vec<Run>,
    /// The newest BLOCKER comment of every live issue (it carries the worker screen of a blocked run).
    pub blockers: Vec<Comment>,
    /// Every dependency edge (issue, depends on).
    pub deps: Vec<Dep>,
    /// `[review] model`: what a reviewer run is shown with.
    pub review_model: String,
    /// `[review] required`: every merge waits for a reviewer.
    pub review_required: bool,
    /// `[review] on_labels`: an issue with one of these labels waits for a reviewer too.
    pub review_on_labels: Vec<String>,
    /// `[merge] verify_main_after`: the gates run on main after each merge. Off, nothing ever checks main.
    pub verify_main_after: bool,
    /// The latest run of every issue, finished ones included, ordered by issue id.
    // Read by the view tasks that follow (asf-39 .. asf-44): the snapshot is their contract.
    #[allow(dead_code)]
    pub last_runs: Vec<LastRun>,
    /// Merge items merged today (local date), newest first.
    // Read by the view tasks that follow (asf-39 .. asf-44): the snapshot is their contract.
    #[allow(dead_code)]
    pub merged_today: Vec<MergeItem>,
    /// Review issue id to the issue it reviews and the round the merge queue started for it.
    pub review_of: BTreeMap<IssueId, ReviewLink>,
    /// Everything that needs the human, oldest first: the one list behind the header count, the banner,
    /// the Now view and the Inbox. Derived from the fields above by [`Snapshot::derive`].
    pub needs_you: Vec<Need>,
    /// Comments and dependency status of the issue selected on the Board; loaded on demand
    /// (`Action::LoadDetail`), `None` until then.
    pub detail: Option<IssueDetail>,
    /// The last lines of one worker pane; loaded on demand (`Action::Peek`), `None` until then.
    pub pane_preview: Option<PanePreview>,
    /// What the pulse of `asf serve` says about the loop; `None` in a snapshot nobody loaded (the header
    /// and the banners then say nothing about the loop).
    pub serve: Option<LoopInfo>,
}

/// The loop as [`serve_pulse::judge`] sees it, with the mode its restart command is shown in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopInfo {
    pub state: LoopState,
    pub mode: ServeMode,
}

impl Snapshot {
    /// What `stuck` waits for, in a few words: the oldest thing that needs the human.
    fn stuck_waiting(&self) -> Option<String> {
        let n = self.needs_you.first()?;
        Some(match n.source {
            NeedSource::Question(_) | NeedSource::BlockedRun => format!("{} waits for your answer", n.issue),
            _ => format!("{} {}", n.issue, n.label),
        })
    }

    /// The one-line notice for a loop that is not running (`None` while it runs or when unknown): the
    /// banner under the tabs and the first NEEDS YOU row of the Now view.
    pub fn loop_notice(&self) -> Option<String> {
        let info = self.serve.as_ref()?;
        info.state.banner(info.mode, self.stuck_waiting().as_deref())
    }

    /// The notice with what it means and what to do, for the window `Enter` opens.
    pub fn loop_explanation(&self) -> Option<String> {
        let info = self.serve.as_ref()?;
        info.state.explain(info.mode, self.stuck_waiting().as_deref())
    }

    /// Recomputes the fields that are a pure function of the others (today: `needs_you`).
    pub fn derive(&mut self) {
        self.needs_you = compute_needs(self);
        self.fill_review_links();
    }

    /// Adds the link of every review issue with a parent and the round of the newest
    /// `merge.review_started` event (the loader reads the round from all events; here only the
    /// events the snapshot holds are known, which is all a snapshot built by hand has).
    fn fill_review_links(&mut self) {
        for issue in self.issues.iter().filter(|i| i.kind == IssueType::Review) {
            let Some(parent) = issue.parent_id.clone() else {
                continue;
            };
            let link = self.review_of.entry(issue.id.clone()).or_insert(ReviewLink {
                reviews: parent.clone(),
                round: None,
            });
            if link.round.is_none() {
                link.round = self
                    .events
                    .iter()
                    .find(|e| e.kind == REVIEW_STARTED && e.issue_id.as_ref() == Some(&parent))
                    .and_then(|e| e.payload["round"].as_i64());
            }
        }
    }

    pub fn issue(&self, id: &IssueId) -> Option<&Issue> {
        self.issues.iter().find(|i| &i.id == id)
    }

    /// The runtime health to hand to the next [`load_snapshot`].
    pub fn runtime_state(&self) -> RuntimeState {
        RuntimeState {
            ok: self.runtime_ok,
            failures: self.runtime_failures,
            reason: self.runtime_reason.clone(),
        }
    }

    /// Worker and reviewer runs that hold a slot, counted by the scheduler's own function, so the
    /// header can never disagree with `asf run plan`. `runs` is every run that has not ended, which
    /// is more than that: stalled and over-budget runs hold no slot.
    pub fn run_counts(&self) -> scheduler::RunCounts {
        scheduler::count_active_runs(&self.issues, &self.runs)
    }

    /// The active run of `id`.
    pub fn run_for(&self, id: &IssueId) -> Option<&Run> {
        self.runs.iter().find(|r| &r.issue_id == id)
    }

    /// The latest run of `id`, active or finished.
    #[allow(dead_code)] // for the view tasks that follow
    pub fn last_run(&self, id: &IssueId) -> Option<&LastRun> {
        self.last_runs.iter().find(|r| &r.issue == id)
    }

    /// The pane preview of `id`, when that is the pane that was read last.
    #[allow(dead_code)] // for the view tasks that follow
    pub fn preview_of(&self, id: &IssueId) -> Option<&str> {
        self.pane_preview
            .as_ref()
            .filter(|p| &p.issue == id)
            .map(|p| p.text.as_str())
    }

    /// The Board detail of `id`, when that is the issue it was loaded for.
    pub fn detail_of(&self, id: &IssueId) -> Option<&IssueDetail> {
        self.detail.as_ref().filter(|d| &d.issue == id)
    }
}

/// The latest run of one issue, flattened to what a row needs.
#[allow(dead_code)] // read by the view tasks that follow
#[derive(Debug, Clone, PartialEq)]
pub struct LastRun {
    pub issue: IssueId,
    pub run_id: String,
    pub state: RunState,
    pub cost: f64,
    pub budget: Option<f64>,
    pub tool_calls: i64,
    pub started_at: String,
    pub finished_at: Option<String>,
    /// `[review] model` for a reviewer, `[worker] model` otherwise (`None` = the Claude default).
    pub model: Option<String>,
    pub pane: Option<String>,
    /// Claude Code's `--name` of the session. The store does not keep it, so it is `None` for now.
    pub session_name: Option<String>,
}

/// A review issue's link to the issue it reviews.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewLink {
    /// The reviewed issue (the review issue's parent).
    pub reviews: IssueId,
    /// The round from the newest `merge.review_started` event of the reviewed issue.
    pub round: Option<i64>,
}

/// Comments and dependency status of one issue.
#[allow(dead_code)] // read by the view tasks that follow
#[derive(Debug, Clone, PartialEq)]
pub struct IssueDetail {
    pub issue: IssueId,
    pub comments: Vec<Comment>,
    /// What the issue depends on, with the status of each.
    pub deps: Vec<(IssueId, Status)>,
}

/// The last lines of a worker pane.
#[allow(dead_code)] // read by the view tasks that follow
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanePreview {
    pub issue: IssueId,
    pub text: String,
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
                Some(root) => match lookup(root, key) {
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

// ---------------------------------------------------------------------------------------------
// NEEDS YOU
// ---------------------------------------------------------------------------------------------

/// The one key that acts on a NEEDS YOU row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    /// `a`: the answer overlay (blocked or stalled worker) or a typed answer (QUESTION comment).
    Answer,
    /// `v`: rerun verification of a failed run.
    Verify,
    /// `r`: re-queue the merge item.
    Retry,
    /// `x`: abort an over-budget worker (after a confirmation).
    Abort,
}

impl Act {
    pub fn key(self) -> char {
        match self {
            Act::Answer => 'a',
            Act::Verify => 'v',
            Act::Retry => 'r',
            Act::Abort => 'x',
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Act::Answer => "answer",
            Act::Verify => "verify",
            Act::Retry => "retry",
            Act::Abort => "abort",
        }
    }

    /// `a answer`: how the key is printed at the end of a row.
    pub fn hint(self) -> String {
        format!("{} {}", self.key(), self.label())
    }
}

/// Where a NEEDS YOU row comes from (its kind).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NeedSource {
    /// A run in `blocked`: the agent sits on a prompt.
    BlockedRun,
    /// An unanswered QUESTION comment to the human (its comment id).
    Question(i64),
    /// A run in `failed`, `stalled` or `over_budget`.
    Run(RunState),
    /// A merge item in `failed` or `blocked`.
    Merge(MergeState),
}

/// One thing that needs the human.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Need {
    pub issue: IssueId,
    pub source: NeedSource,
    /// `question`, `failed`, `stalled`, `over_budget`, `merge failed` or `merge blocked`.
    pub label: String,
    /// One line of text: the question, the screen line of the prompt, the exit reason, the error.
    pub text: String,
    /// RFC 3339 UTC time the thing started waiting; the age and the order come from it.
    pub since: String,
    pub act: Act,
}

/// The first non-empty line of `s`, trimmed.
pub fn first_line(s: &str) -> Option<&str> {
    s.lines().map(str::trim).find(|l| !l.is_empty())
}

/// The first non-empty line of the worker screen quoted in a BLOCKER body (the fenced block after
/// `Screen:`), or `None` when the body quotes no screen.
pub fn screen_line(body: &str) -> Option<String> {
    let mut lines = body.lines().skip_while(|l| l.trim() != "Screen:");
    lines.next()?; // the `Screen:` line itself
    let mut fenced = false;
    for line in lines {
        if line.trim_start().starts_with("```") {
            if fenced {
                return None;
            }
            fenced = true;
        } else if fenced && !line.trim().is_empty() {
            return Some(line.trim().to_string());
        }
    }
    None
}

/// When `run` entered its current state: the newest `run.state` event that moved it there, else the
/// time the run finished or started.
fn state_since(snap: &Snapshot, run: &Run) -> String {
    snap.events
        .iter()
        .find(|e| {
            e.kind == "run.state"
                && e.run_id.as_deref() == Some(run.id.as_str())
                && e.payload["to"].as_str() == Some(run.state.as_str())
        })
        .map(|e| e.ts.clone())
        .or_else(|| run.finished_at.clone())
        .unwrap_or_else(|| run.started_at.clone())
}

/// How actionable a source is now (lower first): a blocked run is a live prompt, a QUESTION is a
/// person waiting for a reply, a failed/stalled/over-budget run is the cause of what follows, and a
/// merge item is only the consequence. (`asf serve` ranks its alerts alike, except that it puts a
/// QUESTION before a blocked run.) A blocked *issue* has no row of its own: no key acts on it.
fn actionability(source: &NeedSource) -> u8 {
    match source {
        NeedSource::BlockedRun => 0,
        NeedSource::Question(_) => 1,
        NeedSource::Run(_) => 2,
        NeedSource::Merge(_) => 3,
    }
}

/// Whether the worker of `id` is working right now (it has an active run). A failed merge item of
/// such an issue is not waiting for the human: a reviewer or a post-rebase gate sent the worker back
/// to fix it, and `asf merge retry` refuses the issue until that run ends. `asf serve` applies the
/// same guard before it reminds (`commands/remind.rs`).
pub fn is_being_fixed(snap: &Snapshot, id: &IssueId) -> bool {
    snap.runs.iter().any(|r| &r.issue_id == id)
}

/// What needs the human: one row per issue, oldest first. Candidates are blocked runs, unanswered
/// QUESTIONs to the human, runs in `failed`/`stalled`/`over_budget`, and merge items in
/// `failed`/`blocked` of a live issue whose worker is not running. When several wait on one issue the
/// row is the most actionable of them ([`actionability`]) and its age is the oldest one's.
pub fn compute_needs(snap: &Snapshot) -> Vec<Need> {
    let mut out = Vec::new();
    for run in snap.runs.iter().chain(&snap.failed_runs) {
        let reason = || {
            run.exit_reason
                .as_deref()
                .and_then(first_line)
                .unwrap_or("no exit reason recorded")
                .to_string()
        };
        match run.state {
            RunState::Blocked => {
                let screen = snap
                    .blockers
                    .iter()
                    .filter(|c| c.issue_id == run.issue_id)
                    .max_by_key(|c| c.id)
                    .and_then(|c| screen_line(&c.body));
                out.push(Need {
                    issue: run.issue_id.clone(),
                    source: NeedSource::BlockedRun,
                    label: "question".into(),
                    text: screen.unwrap_or_else(|| "waiting for your answer".into()),
                    since: state_since(snap, run),
                    act: Act::Answer,
                });
            }
            // The key on a row is one that the command behind it accepts for that state:
            // `asf run verify` refuses a worker that is still active (stalled, over budget),
            // a reply moves a stalled run back to working, and `asf run abort` takes any
            // run that is not finished.
            RunState::Failed | RunState::Stalled | RunState::OverBudget => out.push(Need {
                issue: run.issue_id.clone(),
                source: NeedSource::Run(run.state),
                label: run.state.to_string(),
                text: reason(),
                since: state_since(snap, run),
                act: match run.state {
                    RunState::Failed => Act::Verify,
                    RunState::Stalled => Act::Answer,
                    _ => Act::Abort,
                },
            }),
            _ => {}
        }
    }
    for q in &snap.questions {
        out.push(Need {
            issue: q.issue_id.clone(),
            source: NeedSource::Question(q.id),
            label: "question".into(),
            text: first_line(&q.body).unwrap_or("(empty question)").to_string(),
            since: q.created_at.clone(),
            act: Act::Answer,
        });
    }
    for m in &snap.queue {
        let label = match m.state {
            MergeState::Failed => "merge failed",
            MergeState::Blocked => "merge blocked",
            _ => continue,
        };
        // the same guard as `asf serve`: a closed issue, or one whose worker is fixing it, waits for nobody
        let live = snap.issue(&m.issue_id).is_some_and(|i| !i.status.is_terminal());
        if !live || is_being_fixed(snap, &m.issue_id) {
            continue;
        }
        out.push(Need {
            issue: m.issue_id.clone(),
            source: NeedSource::Merge(m.state),
            label: label.into(),
            text: m
                .last_error
                .as_deref()
                .and_then(first_line)
                .unwrap_or("no error recorded")
                .to_string(),
            since: m.updated_at.clone(),
            act: Act::Retry,
        });
    }
    // One row per issue: the most actionable candidate, aged by the oldest one. A tie keeps the first
    // candidate, which is the older QUESTION (the questions are sorted by time).
    let mut rows: Vec<Need> = Vec::new();
    for need in out {
        let Some(row) = rows.iter_mut().find(|r| r.issue == need.issue) else {
            rows.push(need);
            continue;
        };
        let oldest = row.since.clone().min(need.since.clone());
        if actionability(&need.source) < actionability(&row.source) {
            *row = need;
        }
        row.since = oldest;
    }
    // RFC 3339 UTC strings sort by time; the issue id keeps ties stable.
    rows.sort_by(|a, b| (&a.since, &a.issue).cmp(&(&b.since, &b.issue)));
    rows
}

// ---------------------------------------------------------------------------------------------
// Time, text and list helpers shared by the views
// ---------------------------------------------------------------------------------------------

pub fn parse_time(ts: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(ts, &Rfc3339).ok()
}

/// The zone of the snapshot (`utc_offset_secs`), UTC when it is out of range.
pub fn offset_of(snap: &Snapshot) -> UtcOffset {
    UtcOffset::from_whole_seconds(snap.utc_offset_secs).unwrap_or(UtcOffset::UTC)
}

/// How long ago `since` was, as the rows print it: `<1 min`, `4 min`, `3 h`, `2 d`. `-` when either
/// time does not parse.
pub fn age(now: &str, since: &str) -> String {
    let (Some(now), Some(since)) = (parse_time(now), parse_time(since)) else {
        return "-".into();
    };
    let secs = (now - since).whole_seconds().max(0);
    match secs {
        0..=59 => "<1 min".into(),
        60..=3599 => format!("{} min", secs / 60),
        3600..=86399 => format!("{} h", secs / 3600),
        _ => format!("{} d", secs / 86400),
    }
}

/// `$0.50 / $10`: cost with cents, budget without them when it is a whole number.
pub fn money(cost: f64, budget: Option<f64>) -> String {
    match budget {
        Some(b) if b.fract() == 0.0 => format!("${cost:.2} / ${b:.0}"),
        Some(b) => format!("${cost:.2} / ${b:.2}"),
        None => format!("${cost:.2}"),
    }
}

/// Columns `s` takes on the screen (wide characters count twice).
pub fn text_width(s: &str) -> usize {
    Span::raw(s).width()
}

fn char_width(c: char) -> usize {
    Span::raw(c.to_string()).width()
}

/// The first line of `s`, cut to `n` columns with a trailing `…` when it does not fit.
pub fn trunc(s: &str, n: usize) -> String {
    let s = s.lines().next().unwrap_or("");
    if text_width(s) <= n {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = char_width(c);
        if used + w + 1 > n {
            break;
        }
        out.push(c);
        used += w;
    }
    if n > 0 {
        out.push('…');
    }
    out
}

/// `s` cut with [`trunc`], then padded with blanks to exactly `n` columns.
pub fn fit(s: &str, n: usize) -> String {
    let cut = trunc(s, n);
    let pad = n.saturating_sub(text_width(&cut));
    format!("{cut}{}", " ".repeat(pad))
}

/// Greedy word wrap to `width` columns, so a scroll range is known exactly. Blank lines survive,
/// leading indentation is kept, and a word longer than a line is split.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for raw in text.lines() {
        if raw.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let indent = (raw.len() - raw.trim_start().len()).min(width / 2);
        let mut cur = " ".repeat(indent);
        let mut cur_len = indent;
        let mut fresh = true; // no word on this line yet
        for word in raw.split_whitespace() {
            let chars: Vec<char> = word.chars().collect();
            for piece in chars.chunks(width) {
                let pl = piece.len();
                if !fresh && cur_len + 1 + pl > width {
                    out.push(std::mem::replace(&mut cur, " ".repeat(indent)));
                    cur_len = indent;
                    fresh = true;
                }
                if fresh && cur_len + pl > width {
                    cur.clear(); // a full-width piece leaves no room for the indent
                    cur_len = 0;
                }
                if !fresh {
                    cur.push(' ');
                    cur_len += 1;
                }
                cur.extend(piece);
                cur_len += pl;
                fresh = false;
            }
        }
        if !fresh {
            out.push(cur);
        }
    }
    out
}

/// `a` on a row of an issue: the answer overlay for a blocked run; anything else says why not.
pub fn answer_blocked(snap: &Snapshot, id: IssueId) -> Action {
    if snap.run_for(&id).is_some_and(|r| r.state == RunState::Blocked) {
        Action::OpenAnswer(id)
    } else {
        Action::Status(format!("{id} has no blocked run to answer"))
    }
}

/// `x` on a row of an issue: abort its active run, after a confirmation.
pub fn abort_run(snap: &Snapshot, id: IssueId) -> Action {
    if snap.run_for(&id).is_some() {
        Action::Confirm {
            what: format!("abort {id}?"),
            then: Box::new(Action::Abort(id)),
        }
    } else {
        Action::Status(format!("{id} has no active run"))
    }
}

/// `r` on a row of an issue: the input line that sends the issue's worker a message.
pub fn reply_input(id: IssueId) -> Action {
    Action::OpenInput {
        prompt: format!("reply to {id}"),
        buf: String::new(),
        target: InputTarget::Reply(id),
    }
}

/// Whether a run in `state` waits for the human or has gone wrong (the red `■`).
pub fn run_needs_you(state: RunState) -> bool {
    matches!(
        state,
        RunState::Blocked | RunState::Stalled | RunState::OverBudget | RunState::Failed
    )
}

/// The status symbol of a card or row for `issue`: a run decides first, then a failed or blocked
/// merge item, then the issue's own status.
pub fn issue_mark(snap: &Snapshot, issue: &Issue) -> Mark {
    if let Some(r) = snap.run_for(&issue.id) {
        return if run_needs_you(r.state) {
            Mark::Needs
        } else if issue.kind == IssueType::Review {
            Mark::Review
        } else {
            Mark::Working
        };
    }
    let failed_run = snap.failed_runs.iter().any(|r| r.issue_id == issue.id);
    let failed_merge = snap
        .queue
        .iter()
        .any(|m| m.issue_id == issue.id && matches!(m.state, MergeState::Failed | MergeState::Blocked));
    match issue.status {
        _ if failed_run || failed_merge => Mark::Needs,
        Status::Validating | Status::InReview => Mark::Review,
        Status::Done => Mark::Done,
        _ => Mark::Waiting,
    }
}

/// The selection after a navigation key (`j`/`k`/arrows, `g`/`G`/Home/End) over `rows` rows, or `None`
/// when `key` does not navigate. `selected` is clamped first, so a stale index is never an error.
pub fn move_selection(key: &KeyEvent, selected: usize, rows: usize) -> Option<usize> {
    let last = rows.saturating_sub(1);
    let cur = selected.min(last);
    Some(match key.code {
        KeyCode::Char('j') | KeyCode::Down => (cur + 1).min(last),
        KeyCode::Char('k') | KeyCode::Up => cur.saturating_sub(1),
        KeyCode::Char('g') | KeyCode::Home => 0,
        KeyCode::Char('G') | KeyCode::End => last,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------------------------

/// `.factory/config.toml` text with the BOM dropped and CRLF turned into LF (as `asf config` reads it).
pub fn read_config_text(root: &Path) -> std::io::Result<String> {
    let raw = std::fs::read_to_string(root.join(FACTORY_DIR).join(CONFIG_FILE))?;
    Ok(raw.trim_start_matches('\u{feff}').replace("\r\n", "\n"))
}

/// Loads everything the TUI shows. `rt` adds live agent states and one health probe (both skipped when
/// `None`); `prev_runtime` is the health the last snapshot ended with, which a skipped or failed probe
/// leaves in place (see [`RuntimeState::observe`]).
///
/// The settings it carries (`max_parallel`, the review and merge switches, the models of the runs)
/// come from `.factory/config.toml` as it is now, not from the `Ctx` loaded when the TUI started, so an
/// edit made from the Config view, `asf config set` or an editor shows at the next refresh. A file that
/// is missing or does not parse leaves the settings of the `Ctx`.
pub fn load_snapshot(
    ctx: &Ctx,
    rt: Option<&dyn AgentRuntime>,
    prev_agents: &BTreeMap<IssueId, AgentInfo>,
    prev_runtime: &RuntimeState,
) -> Result<Snapshot, AppError> {
    let store = &ctx.store;
    let config_text = read_config_text(&ctx.root).ok();
    let live_config = config_text
        .as_deref()
        .and_then(|text| Config::from_toml(text).ok());
    let cfg = live_config.as_ref().unwrap_or(&ctx.config);
    let runs = store.runs(None, true)?;
    let mut agents = prev_agents.clone();
    let mut runtime = prev_runtime.observe(None);
    if let Some(rt) = rt {
        runtime = prev_runtime.observe(Some(rt.health().map(|_| ()).map_err(|e| e.to_string())));
        for r in &runs {
            if let (Some(pane), Some(name)) = (&r.pane_id, &r.agent_name) {
                let h = AgentHandle {
                    name: name.clone(),
                    pane_id: pane.clone(),
                    workspace_id: r.workspace_id.clone().unwrap_or_default(),
                };
                if let Ok(info) = rt.info(&h) {
                    agents.insert(r.issue_id.clone(), info);
                }
            }
        }
    }
    let mut events = store.events_since(0, usize::MAX)?;
    // The review round is read from every event, before the log is cut to its newest lines.
    let rounds: BTreeMap<IssueId, i64> = events
        .iter()
        .filter(|e| e.kind == REVIEW_STARTED)
        .filter_map(|e| Some((e.issue_id.clone()?, e.payload["round"].as_i64()?)))
        .collect(); // ordered by seq: the last (newest) round of an issue wins
    if events.len() > LOG_LIMIT {
        events = events.split_off(events.len() - LOG_LIMIT);
    }
    events.reverse();
    let issues = store.list_issues(&IssueFilter::default())?;
    // Unanswered questions are read from all comments, not the inbox: marking the inbox read must
    // not hide a question that still blocks `asf issue finish`. The newest BLOCKER of each live
    // issue rides along: it quotes the screen of a blocked worker.
    let mut questions = Vec::new();
    let mut blockers = Vec::new();
    for issue in issues
        .iter()
        .filter(|i| !matches!(i.status, Status::Done | Status::Cancelled))
    {
        let comments = store.comments(&issue.id)?;
        let open = unanswered_questions(&comments);
        blockers.extend(
            comments
                .iter()
                .filter(|c| c.kind == CommentType::Blocker)
                .max_by_key(|c| c.id)
                .cloned(),
        );
        questions.extend(comments.into_iter().filter(|c| open.contains(&c.id)));
    }
    questions.sort_by(|a, b| (&a.created_at, a.id).cmp(&(&b.created_at, b.id)));
    // `runs` is active-only, so a `failed` run is found among all runs: the newest run of a live
    // issue, when it failed and no run of the issue is active.
    let all_runs = store.runs(None, false)?;
    let mut newest: BTreeMap<IssueId, Run> = BTreeMap::new();
    for r in &all_runs {
        if newest.get(&r.issue_id).is_none_or(|n| r.attempt > n.attempt) {
            newest.insert(r.issue_id.clone(), r.clone());
        }
    }
    let failed_runs: Vec<Run> = issues
        .iter()
        .filter(|i| !matches!(i.status, Status::Done | Status::Cancelled))
        .filter_map(|i| newest.get(&i.id).cloned())
        .filter(|r| r.state == RunState::Failed)
        .collect();
    let is_review = |id: &IssueId| issues.iter().any(|i| &i.id == id && i.kind == IssueType::Review);
    let last_runs: Vec<LastRun> = newest
        .values()
        .map(|r| LastRun {
            issue: r.issue_id.clone(),
            run_id: r.id.clone(),
            state: r.state,
            cost: r.cost_usd,
            budget: r.budget_usd,
            tool_calls: r.tool_calls,
            started_at: r.started_at.clone(),
            finished_at: r.finished_at.clone(),
            model: if is_review(&r.issue_id) {
                Some(cfg.review.model.clone())
            } else {
                cfg.worker.model.clone()
            },
            pane: r.pane_id.clone(),
            session_name: None,
        })
        .collect();
    let review_of: BTreeMap<IssueId, ReviewLink> = issues
        .iter()
        .filter(|i| i.kind == IssueType::Review)
        .filter_map(|i| {
            let parent = i.parent_id.clone()?;
            let round = rounds.get(&parent).copied();
            Some((
                i.id.clone(),
                ReviewLink {
                    reviews: parent,
                    round,
                },
            ))
        })
        .collect();
    // `time`'s local-offset support is enabled workspace-wide by ratatui; UTC when it cannot be read.
    let utc_offset_secs = UtcOffset::current_local_offset().map_or(0, |o| o.whole_seconds());
    let now = now_rfc3339();
    let merge_all = store.merge_queue()?;
    let mut snap = Snapshot {
        repo_name: ctx
            .root
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default(),
        branch: cfg.repo.default_branch.clone(),
        runtime_ok: runtime.ok,
        runtime_failures: runtime.failures,
        runtime_reason: runtime.reason,
        max_parallel: cfg.scheduler.max_parallel,
        issues,
        claims: store.active_claims()?,
        runs,
        all_runs,
        agents,
        inbox: store.inbox(LANE_HUMAN, true, false)?,
        questions,
        queue: merge_all
            .iter()
            .filter(|m| !m.state.is_terminal())
            .cloned()
            .collect(),
        events,
        config: config_rows(config_text.as_deref().unwrap_or_default()),
        now,
        utc_offset_secs,
        failed_runs,
        blockers,
        deps: store.deps()?,
        review_model: cfg.review.model.clone(),
        review_required: cfg.review.required,
        review_on_labels: cfg.review.on_labels.clone(),
        verify_main_after: cfg.merge.verify_main_after,
        last_runs,
        merged_today: Vec::new(),
        review_of,
        needs_you: Vec::new(),
        detail: None,
        pane_preview: None,
        serve: None,
    };
    // age only: a process lookup is a child process on Windows and the snapshot refreshes every few seconds
    let pulse = serve_pulse::read(&ctx.root).ok().flatten();
    snap.serve = parse_time(&snap.now).map(|now| LoopInfo {
        state: serve_pulse::judge(pulse.as_ref(), now, None),
        mode: pulse.as_ref().map_or(ServeMode::Dispatch, |p| p.mode),
    });
    snap.merged_today = merged_today(&snap, merge_all);
    snap.derive();
    Ok(snap)
}

/// The merged items among `all` whose last update falls on the local date of the snapshot, newest first.
fn merged_today(snap: &Snapshot, all: Vec<MergeItem>) -> Vec<MergeItem> {
    let offset = offset_of(snap);
    let Some(today) = parse_time(&snap.now).map(|t| t.to_offset(offset).date()) else {
        return Vec::new();
    };
    let mut out: Vec<MergeItem> = all
        .into_iter()
        .filter(|m| m.state == MergeState::Merged)
        .filter(|m| parse_time(&m.updated_at).is_some_and(|t| t.to_offset(offset).date() == today))
        .collect();
    out.sort_by(|a, b| (&b.updated_at, b.id).cmp(&(&a.updated_at, a.id)));
    out
}

/// Loads the comments and the dependency status of one issue (`Action::LoadDetail`).
pub fn load_detail(ctx: &Ctx, id: &IssueId) -> Result<IssueDetail, AppError> {
    let store = &ctx.store;
    let mut deps = Vec::new();
    for d in store.deps()?.into_iter().filter(|d| &d.issue_id == id) {
        if let Some(issue) = store.get_issue(&d.depends_on)? {
            deps.push((issue.id, issue.status));
        }
    }
    Ok(IssueDetail {
        issue: id.clone(),
        comments: store.comments(id)?,
        deps,
    })
}
