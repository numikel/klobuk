//! Tests of the TUI, split by what they test. The builders every topic needs live here; each topic
//! file starts with `use super::*;`. A view's own tests live in the file named after it
//! (`board.rs`, `agents.rs`, `inbox.rs`, `queue.rs`, `log.rs`, `config.rs`, `now_view.rs`), so a view
//! task edits its own file and no other. The modules are named like the views, so none of them is
//! imported here by its short name: tests use the full path `crate::tui::views::<view>`.

use std::collections::BTreeMap;
use std::time::Duration;

use asf_core::*;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier};

use super::app::{Action, App, View, STATUS_TTL};
use super::bindings::{all, footer_hints, Group, ModeKind};
use super::chrome::{render_to_buffer, render_to_text, MIN_HEIGHT, MIN_WIDTH};
use super::overlays::{InputTarget, Mode};
use super::snapshot::{
    age, compute_needs, config_rows, unanswered_questions, Act, Need, NeedSource, Snapshot,
};
use super::theme::no_color_requested;
use super::theme::Theme;
use super::views::now;
use super::{
    bg_outcome, config_set_status, config_unset_status, keys_status, parse_usd, post_answer, BgKind,
};

/// Selection and row queries the older tests were written against; the views own the state now.
impl App {
    fn selected(&self, view: View) -> usize {
        let s = &self.snap;
        match view {
            View::Now => now::selected(s, &self.now),
            View::Board => super::views::board::selected(s, &self.board),
            View::Agents => super::views::agents::selected(s, &self.agents),
            View::Inbox => super::views::inbox::selected(s, &self.inbox),
            View::Queue => super::views::queue::selected(s, &self.queue),
            View::Log => super::views::log::selected(s, &self.log),
            View::Config => super::views::config::selected(s, &self.config),
        }
    }

    fn rows(&self, view: View) -> usize {
        let s = &self.snap;
        match view {
            View::Now => now::rows(s),
            View::Board => super::views::board::rows(s),
            View::Agents => super::views::agents::rows(s),
            View::Inbox => super::views::inbox::rows(s),
            View::Queue => super::views::queue::rows(s),
            View::Log => super::views::log::rows(s),
            View::Config => super::views::config::rows(s),
        }
    }

    fn selected_issue(&self) -> Option<IssueId> {
        let s = &self.snap;
        match self.view {
            View::Now => now::selected_row(s, &self.now).and_then(|r| r.issue().cloned()),
            View::Board => super::views::board::selected_issue(s, &self.board),
            View::Agents => super::views::agents::selected_issue(s, &self.agents),
            View::Inbox => super::views::inbox::selected_issue(s, &self.inbox),
            View::Queue => super::views::queue::selected_issue(s, &self.queue),
            View::Log | View::Config => None,
        }
    }

    fn selected_now_row(&self) -> Option<now::NowRow> {
        now::selected_row(&self.snap, &self.now)
    }
}

/// What is waiting for the human in `snap`, oldest first.
fn needs(snap: &Snapshot) -> Vec<Need> {
    compute_needs(snap)
}

mod agents;
mod answer;
mod background;
mod board;
mod config;
mod controls;
mod frame;
mod help;
mod inbox;
mod log;
mod loop_state;
// `now` is the imported view, so the module that tests the one-row-per-issue rule has another name
#[path = "now.rs"]
mod now_rows;
mod now_view;
mod queue;
mod render;
mod snapshot;
mod state;
mod status;
mod waiting;

fn id(s: &str) -> IssueId {
    s.parse().unwrap()
}

fn issue(i: &str, title: &str, status: Status) -> Issue {
    Issue {
        id: id(i),
        uid: format!("u{i}"),
        title: title.into(),
        body: String::new(),
        kind: IssueType::Task,
        status,
        priority: 2,
        parent_id: None,
        file_scope: vec!["src/**".into()],
        acceptance: vec![Acceptance {
            text: "works".into(),
            verify: None,
        }],
        estimate: None,
        labels: vec![],
        source: None,
        created_at: "2026-11-02T10:00:00Z".into(),
        updated_at: "2026-11-02T10:00:00Z".into(),
        closed_at: None,
    }
}

fn run(i: &str, state: RunState, cost: f64) -> Run {
    Run {
        id: format!("run-{i}"),
        issue_id: id(i),
        attempt: 1,
        runtime: "fake".into(),
        agent_name: Some(i.into()),
        workspace_id: Some("f:w1".into()),
        pane_id: Some("f:p1".into()),
        claude_session_id: None,
        transcript_path: None,
        worktree: format!(".worktrees/{i}"),
        branch: format!("feat/{i}"),
        base_sha: "base".into(),
        state,
        budget_usd: Some(2.0),
        cost_usd: cost,
        input_tokens: 0,
        output_tokens: 0,
        turns: 0,
        tool_calls: 7,
        exit_reason: None,
        report_path: None,
        started_at: "2026-11-02T10:00:00Z".into(),
        last_seen_at: None,
        finished_at: None,
        herdr_revision: None,
    }
}

fn comment(i: &str, kind: CommentType, body: &str) -> Comment {
    Comment {
        id: 1,
        issue_id: id(i),
        author: "orchestrator".into(),
        kind,
        to_lane: "human".into(),
        body: body.into(),
        reply_to: None,
        resolved_at: None,
        created_at: "2026-11-02T10:05:00Z".into(),
    }
}

/// Four issues across columns, one running worker, one inbox item, one queued merge, two events.
fn snapshot() -> Snapshot {
    let mut agents = BTreeMap::new();
    agents.insert(
        id("asf-2"),
        AgentInfo {
            state: AgentState::Working,
            revision: 3,
            ..AgentInfo::default()
        },
    );
    Snapshot {
        repo_name: "hello".into(),
        branch: "main".into(),
        runtime_ok: Some(true),
        max_parallel: 3,
        issues: vec![
            issue("asf-1", "Scaffold crate", Status::Done),
            issue("asf-2", "Add greeting", Status::InProgress),
            issue("asf-3", "Write docs", Status::Ready),
            issue("asf-4", "Release", Status::Backlog),
        ],
        config: config_rows(Config::default_toml()),
        questions: vec![],
        claims: vec![Claim {
            issue_id: id("asf-2"),
            worker: "asf-2".into(),
            run_id: Some("run-asf-2".into()),
            worktree: Some(".worktrees/asf-2".into()),
            branch: Some("feat/asf-2".into()),
            claimed_at: "2026-11-02T10:01:00Z".into(),
            note: None,
        }],
        runs: vec![run("asf-2", RunState::Working, 1.25)],
        agents,
        inbox: vec![comment(
            "asf-2",
            CommentType::Blocker,
            "Worker is waiting for input.\nScreen: Allow rm?",
        )],
        queue: vec![MergeItem {
            id: 1,
            issue_id: id("asf-1"),
            run_id: None,
            branch: "feat/asf-1".into(),
            state: MergeState::Queued,
            attempts: 0,
            head_sha: None,
            rebased_onto: None,
            pr_url: None,
            gate_results: vec![],
            last_error: None,
            enqueued_at: "2026-11-02T10:02:00Z".into(),
            updated_at: "2026-11-02T10:02:00Z".into(),
        }],
        events: vec![
            Event {
                seq: 1,
                ts: "2026-11-02T10:00:00Z".into(),
                kind: "issue.created".into(),
                issue_id: Some(id("asf-1")),
                run_id: None,
                actor: None,
                payload: serde_json::json!({}),
            },
            Event {
                seq: 2,
                ts: "2026-11-02T10:05:00Z".into(),
                kind: "run.state".into(),
                issue_id: Some(id("asf-2")),
                run_id: Some("run-asf-2".into()),
                actor: None,
                payload: serde_json::json!({ "to": "working" }),
            },
        ],
        now: "2026-11-02T10:30:00Z".into(),
        utc_offset_secs: 0,
        failed_runs: vec![],
        blockers: vec![],
        deps: vec![],
        review_model: "opus".into(),
        ..Snapshot::default()
    }
}

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn code(k: KeyCode) -> KeyEvent {
    KeyEvent::new(k, KeyModifiers::NONE)
}

/// An app on the Board: the older tests were written when the Board was the first view.
fn board_app(s: Snapshot) -> App {
    let mut a = App::new(s);
    a.view = View::Board;
    a
}

fn app() -> App {
    board_app(snapshot())
}

fn select_config_key(a: &mut App, wanted: &str) {
    a.handle_key(key('7'));
    let idx = asf_core::config_edit::editable_keys()
        .iter()
        .position(|(k, _)| *k == wanted)
        .unwrap();
    for _ in 0..idx {
        a.handle_key(key('j'));
    }
}

fn config_dir(toml: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".factory")).unwrap();
    let path = dir.path().join(".factory/config.toml");
    std::fs::write(&path, toml).unwrap();
    (dir, path)
}

/// asf-2's run is blocked on a prompt; the pane shows a numbered question.
fn blocked_snapshot() -> Snapshot {
    let mut s = snapshot();
    s.runs[0].state = RunState::Blocked;
    s
}

fn question(n: i64, i: &str, body: &str, at: &str) -> Comment {
    let mut c = comment(i, CommentType::Question, body);
    c.id = n;
    c.created_at = at.into();
    c
}

/// Colour and modifiers of the first cell of `needle`, searched only in screen rows containing `row`.
fn cell_style(app: &App, row: &str, needle: &str) -> (Color, Modifier) {
    let (w, h) = (120, 30);
    let buf = render_to_buffer(app, w, h);
    for y in 0..h {
        let line: String = (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect();
        if !line.contains(row) {
            continue;
        }
        if let Some(byte) = line.find(needle) {
            let x = line[..byte].chars().count() as u16;
            let c = &buf[(x, y)];
            return (c.fg, c.modifier);
        }
    }
    panic!("{needle:?} not on screen:\n{}", render_to_text(app, w, h));
}

fn line_of(text: &str, n: usize) -> String {
    text.lines().nth(n).unwrap_or("").to_string()
}

fn open_overlay() -> App {
    let mut a = board_app(blocked_snapshot());
    a.handle_key(key('3'));
    assert_eq!(a.handle_key(key('a')), Action::Peek(id("asf-2")));
    a
}

fn config_app(toml: &str, wanted: &str) -> App {
    let mut s = snapshot();
    s.config = config_rows(toml);
    let mut a = board_app(s);
    select_config_key(&mut a, wanted);
    a
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn mods(c: char, m: KeyModifiers) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), m)
}

fn footer_line(a: &App, width: u16) -> String {
    let t = render_to_text(a, width, 30);
    t.lines().last().unwrap_or("").to_string()
}

fn inbox_app() -> App {
    let mut a = app();
    a.handle_key(key('4'));
    a
}

fn ctrl(c: char) -> KeyEvent {
    mods(c, KeyModifiers::CONTROL)
}

fn typing_app() -> App {
    let mut a = app();
    a.handle_key(key('j'));
    a.handle_key(key('j'));
    a.handle_key(key('r'));
    a
}

fn buf_of(a: &App) -> String {
    match &a.mode() {
        Mode::Input { buf, .. } => buf.clone(),
        other => panic!("not in input mode: {other:?}"),
    }
}

const NOW: &str = "2026-11-06T12:00:00Z";

fn dep(i: &str, on: &str) -> Dep {
    Dep {
        issue_id: id(i),
        depends_on: id(on),
    }
}

fn merge_item(n: i64, i: &str, state: MergeState, error: Option<&str>, at: &str) -> MergeItem {
    MergeItem {
        id: n,
        issue_id: id(i),
        run_id: None,
        branch: format!("feat/{i}"),
        state,
        attempts: 1,
        head_sha: None,
        rebased_onto: None,
        pr_url: None,
        gate_results: vec![],
        last_error: error.map(Into::into),
        enqueued_at: at.into(),
        updated_at: at.into(),
    }
}

fn run_at(i: &str, state: RunState, started: &str) -> Run {
    let mut r = run(i, state, 0.0);
    r.budget_usd = Some(10.0);
    r.started_at = started.into();
    r
}

fn state_event(seq: u64, i: &str, to: &str, at: &str) -> Event {
    Event {
        seq,
        ts: at.into(),
        kind: "run.state".into(),
        issue_id: Some(id(i)),
        run_id: Some(format!("run-{i}")),
        actor: None,
        payload: serde_json::json!({ "to": to }),
    }
}

/// A snapshot taken at [`NOW`] holding `issues` and nothing else.
fn desk(issues: Vec<Issue>) -> Snapshot {
    Snapshot {
        repo_name: "auto-software-factory".into(),
        branch: "main".into(),
        runtime_ok: Some(true),
        max_parallel: 2,
        issues,
        now: NOW.into(),
        review_model: "opus".into(),
        ..Snapshot::default()
    }
}

fn live(ids: &[&str]) -> Vec<Issue> {
    ids.iter()
        .map(|i| issue(i, &format!("Title of {i}"), Status::InProgress))
        .collect()
}

/// Every NEEDS YOU source at once, with ages from under a minute to 2 h.
fn busy_desk() -> Snapshot {
    let mut s = desk(live(&[
        "asf-1", "asf-2", "asf-3", "asf-4", "asf-5", "asf-6", "asf-7", "asf-8",
    ]));
    // blocked since 11:50 on a prompt that the BLOCKER quotes
    s.runs
        .push(run_at("asf-1", RunState::Blocked, "2026-11-06T11:30:00Z"));
    s.events
        .push(state_event(1, "asf-1", "blocked", "2026-11-06T11:50:00Z"));
    let mut screen = comment(
        "asf-1",
        CommentType::Blocker,
        "Worker for asf-1 is waiting for input.\n\nScreen:\n```\n\n  Allow Bash(rm -rf target)?\n 1. Yes\n 2. No\n```\nAnswer with `asf run keys asf-1 y`.",
    );
    screen.id = 4;
    s.blockers.push(screen);
    // blocked since 11:55, no BLOCKER comment to quote
    s.runs
        .push(run_at("asf-8", RunState::Blocked, "2026-11-06T11:40:00Z"));
    s.events
        .push(state_event(2, "asf-8", "blocked", "2026-11-06T11:55:00Z"));
    // an unanswered question since 11:56
    s.questions.push(question(
        7,
        "asf-2",
        "\nKeep the old --scope flag as an alias?\nsecond line",
        "2026-11-06T11:56:00Z",
    ));
    // a failed run is not active, so it comes from `failed_runs`
    let mut failed = run_at("asf-3", RunState::Failed, "2026-11-06T11:00:00Z");
    failed.finished_at = Some("2026-11-06T11:20:00Z".into());
    failed.exit_reason = Some("gate `test` failed\nsecond line".into());
    s.failed_runs.push(failed);
    // stalled since 11:00; over budget with no event at all (falls back to the start, 10:00)
    let mut stalled = run_at("asf-4", RunState::Stalled, "2026-11-06T10:30:00Z");
    stalled.exit_reason = Some("no output for 10 minutes".into());
    s.runs.push(stalled);
    s.events
        .push(state_event(3, "asf-4", "stalled", "2026-11-06T11:00:00Z"));
    s.runs
        .push(run_at("asf-5", RunState::OverBudget, "2026-11-06T10:00:00Z"));
    s.queue.push(merge_item(
        1,
        "asf-6",
        MergeState::Failed,
        Some("gate `test` after rebase (2 tests)\nmore output"),
        "2026-11-06T11:48:00Z",
    ));
    s.queue.push(merge_item(
        2,
        "asf-7",
        MergeState::Blocked,
        None,
        "2026-11-06T11:59:40Z",
    ));
    // a healthy queue item waits for nobody
    s.queue
        .push(merge_item(3, "asf-2", MergeState::Queued, None, NOW));
    s
}

/// A worker (6 min), a reviewer of it (1 min) and the round the merge queue started.
fn working_desk() -> Snapshot {
    let mut review = issue("asf-31", "Review asf-24: strip env vars", Status::InProgress);
    review.kind = IssueType::Review;
    review.parent_id = Some(id("asf-24"));
    let mut s = desk(vec![
        issue(
            "asf-24",
            "Strip worker env vars from cli_export tests",
            Status::InProgress,
        ),
        review,
    ]);
    let mut worker = run_at("asf-24", RunState::Working, "2026-11-06T11:54:00Z");
    worker.cost_usd = 0.5;
    let mut reviewer = run_at("asf-31", RunState::Working, "2026-11-06T11:59:00Z");
    reviewer.cost_usd = 0.21;
    s.runs = vec![worker, reviewer];
    s.events.push(Event {
        seq: 9,
        ts: "2026-11-06T11:59:00Z".into(),
        kind: "merge.review_started".into(),
        issue_id: Some(id("asf-24")),
        run_id: None,
        actor: None,
        payload: serde_json::json!({ "after_comment": 3, "round": 1 }),
    });
    s
}

/// `busy_desk` plus the running rows of `working_desk`.
fn full_desk() -> Snapshot {
    let mut s = busy_desk();
    let w = working_desk();
    s.issues.extend(w.issues);
    s.runs.extend(w.runs);
    s.events.extend(w.events);
    s
}

/// The screen line of the NEEDS YOU / RUNNING row for `id` (it holds a status symbol before the id).
fn row_of(text: &str, id: &str) -> String {
    text.lines()
        .find(|l| ["■", "●", "◆"].iter().any(|g| l.contains(&format!("{g} {id} "))))
        .unwrap_or_else(|| panic!("no row for {id}:\n{text}"))
        .to_string()
}

/// Moves the selection to the NEEDS YOU row of `issue`.
fn select_need(a: &mut App, issue: &str) {
    a.handle_key(key('g'));
    for _ in 0..a.rows(View::Now) {
        if matches!(a.selected_now_row(), Some(now::NowRow::Need(n)) if n.issue == id(issue)) {
            return;
        }
        a.handle_key(key('j'));
    }
    panic!("no NEEDS YOU row for {issue}");
}

/// The states each command behind a NEEDS YOU key accepts, copied from the commands themselves:
/// `Worker::verify_issue` (`worker.rs`), `Worker::abort` and the `reply`/`keys` unblock path.
fn accepted_by_the_command(act: Act, state: RunState) -> bool {
    match act {
        Act::Verify => matches!(
            state,
            RunState::Failed | RunState::Finishing | RunState::Verifying
        ),
        Act::Abort => !state.is_terminal(),
        Act::Answer => matches!(state, RunState::Blocked | RunState::Stalled),
        Act::Retry => false, // merge items, not runs
    }
}
