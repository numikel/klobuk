//! The demo behind the README pictures: a made-up small project ("notekeeper", issue ids `nk-N`) held in
//! a [`Snapshot`] built by hand, on a fixed clock. Nothing is read from a store, a config file, the
//! environment or the clock, so two runs draw the same pictures, and no picture can show real work.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use klobuk_core::config::GateWhen;
use klobuk_core::*;
use klobuk_store::serve_pulse::{LoopState, ServeMode};

use crate::tui::app::{App, View};
use crate::tui::chrome::render_to_buffer;
use crate::tui::snapshot::{config_rows, GateInfo, LoopInfo, Snapshot};
use crate::tui::theme::Theme;
use crate::tui::views::board::BoardLayout;
use crate::worker::{GateLive, LiveGate};

use super::svg;

/// The width of every picture, in cells: 110 x 8 px = 880 px, the width of a README column on GitHub.
pub const COLS: u16 = 110;

/// The moment the demo is drawn at; every age in the pictures is counted from here.
const NOW: &str = "2026-06-18T14:30:00Z";

fn id(n: i64) -> IssueId {
    IssueId::new("nk", n)
}

fn issue(n: i64, title: &str, status: Status) -> Issue {
    Issue {
        id: id(n),
        uid: format!("demo-{n}"),
        title: title.into(),
        body: String::new(),
        kind: IssueType::Task,
        status,
        priority: 2,
        parent_id: Some(id(1)),
        file_scope: Vec::new(),
        acceptance: Vec::new(),
        estimate: None,
        labels: Vec::new(),
        source: None,
        created_at: "2026-06-16T09:00:00Z".into(),
        updated_at: "2026-06-18T13:00:00Z".into(),
        closed_at: None,
    }
}

fn closed(mut issue: Issue, at: &str) -> Issue {
    issue.closed_at = Some(at.into());
    issue
}

fn run(n: i64, state: RunState, cost: f64, started: &str, finished: Option<&str>) -> Run {
    Run {
        id: format!("run-nk-{n}"),
        issue_id: id(n),
        attempt: 1,
        runtime: "herdr".into(),
        agent_name: Some(format!("nk-{n}")),
        workspace_id: Some(format!("w{n}")),
        pane_id: Some(format!("w{n}-1")),
        claude_session_id: None,
        transcript_path: None,
        worktree: format!(".worktrees/nk-{n}"),
        branch: format!("feat/nk-{n}"),
        base_sha: "9f3c2a1".into(),
        state,
        budget_usd: Some(5.0),
        cost_usd: cost,
        input_tokens: 0,
        output_tokens: 0,
        turns: 0,
        tool_calls: 0,
        exit_reason: None,
        report_path: None,
        started_at: started.into(),
        last_seen_at: None,
        finished_at: finished.map(Into::into),
        herdr_revision: None,
    }
}

fn merge_item(n: i64, issue: i64, state: MergeState, head: &str, enqueued: &str, updated: &str) -> MergeItem {
    MergeItem {
        id: n,
        issue_id: id(issue),
        run_id: Some(format!("run-nk-{issue}")),
        branch: format!("feat/nk-{issue}"),
        state,
        attempts: 1,
        head_sha: Some(head.into()),
        rebased_onto: Some("a07d3be".into()),
        pr_url: None,
        gate_results: Vec::new(),
        last_error: None,
        enqueued_at: enqueued.into(),
        updated_at: updated.into(),
    }
}

/// A `merge.state` or review event of the merge queue, with its sequence number.
fn event(seq: u64, ts: &str, kind: &str, issue: i64, payload: serde_json::Value) -> Event {
    Event {
        seq,
        ts: ts.into(),
        kind: kind.into(),
        issue_id: Some(id(issue)),
        run_id: None,
        actor: None,
        payload,
    }
}

fn moved(seq: u64, ts: &str, merge: i64, issue: i64, to: &str) -> Event {
    event(
        seq,
        ts,
        "merge.state",
        issue,
        serde_json::json!({ "merge_id": merge, "to": to }),
    )
}

/// Eleven issues of a small command-line notes tool, two workers running, one question waiting, one
/// branch in the gates of the merge queue and one merged an hour ago.
pub fn snapshot() -> Snapshot {
    let epic = Issue {
        kind: IssueType::Epic,
        parent_id: None,
        ..issue(1, "Notes CLI 0.1", Status::InProgress)
    };
    let issues = vec![
        epic,
        closed(
            issue(2, "Set up the project", Status::Done),
            "2026-06-17T11:20:00Z",
        ),
        closed(
            issue(3, "Store notes in SQLite", Status::Done),
            "2026-06-17T16:40:00Z",
        ),
        closed(
            issue(4, "Add and list notes", Status::Done),
            "2026-06-18T13:58:00Z",
        ),
        issue(5, "Full-text note search", Status::InProgress),
        issue(6, "Export notes to Markdown", Status::InProgress),
        issue(7, "Tag and filter notes", Status::Validating),
        issue(8, "Sync through a shared folder", Status::Blocked),
        issue(9, "Colour output for lists", Status::Ready),
        issue(10, "Man page and completions", Status::Backlog),
        issue(11, "Publish to crates.io", Status::Backlog),
    ];
    let active = vec![
        run(5, RunState::Working, 1.84, "2026-06-18T14:05:00Z", None),
        run(6, RunState::Working, 0.62, "2026-06-18T14:19:00Z", None),
    ];
    let all_runs = [
        run(
            2,
            RunState::Succeeded,
            0.48,
            "2026-06-17T10:30:00Z",
            Some("2026-06-17T11:10:00Z"),
        ),
        run(
            3,
            RunState::Succeeded,
            1.31,
            "2026-06-17T15:50:00Z",
            Some("2026-06-17T16:30:00Z"),
        ),
        run(
            4,
            RunState::Succeeded,
            0.94,
            "2026-06-18T13:10:00Z",
            Some("2026-06-18T13:48:00Z"),
        ),
        run(
            7,
            RunState::Succeeded,
            2.07,
            "2026-06-18T13:20:00Z",
            Some("2026-06-18T14:22:00Z"),
        ),
    ]
    .into_iter()
    .chain(active.clone())
    .collect();

    let merged = merge_item(
        1,
        4,
        MergeState::Merged,
        "7be0f42",
        "2026-06-18T13:51:00Z",
        "2026-06-18T13:58:00Z",
    );
    let gating = merge_item(
        2,
        7,
        MergeState::Gating,
        "c4e91ab",
        "2026-06-18T14:24:30Z",
        "2026-06-18T14:24:52Z",
    );
    // newest first, as the store hands them over
    let events = vec![
        moved(8, "2026-06-18T14:24:52Z", 2, 7, "gating"),
        moved(7, "2026-06-18T14:24:40Z", 2, 7, "rebasing"),
        moved(6, "2026-06-18T13:58:00Z", 1, 4, "merged"),
        moved(5, "2026-06-18T13:57:30Z", 1, 4, "merging"),
        event(
            4,
            "2026-06-18T13:57:25Z",
            "merge.review_approved",
            4,
            serde_json::json!({ "round": 1 }),
        ),
        moved(3, "2026-06-18T13:53:40Z", 1, 4, "reviewing"),
        moved(2, "2026-06-18T13:51:20Z", 1, 4, "gating"),
        moved(1, "2026-06-18T13:51:10Z", 1, 4, "rebasing"),
    ];
    let gate = |name: &str, ok: bool, ms: u64| LiveGate {
        name: name.into(),
        ok,
        duration_ms: ms,
        tail: String::new(),
    };
    let gates = ["fmt", "clippy", "test"];
    let mut snap = Snapshot {
        repo_name: "notekeeper".into(),
        branch: "main".into(),
        runtime_ok: Some(true),
        max_parallel: 3,
        issues,
        runs: active,
        all_runs,
        questions: vec![Comment {
            id: 1,
            issue_id: id(8),
            author: "nk-8".into(),
            kind: CommentType::Question,
            to_lane: LANE_HUMAN.to_string(),
            body: "Should note sync use a shared folder or a git remote?".into(),
            reply_to: None,
            resolved_at: None,
            created_at: "2026-06-18T14:12:00Z".into(),
        }],
        queue: vec![gating],
        merged_today: vec![merged],
        events,
        config: config_rows(Config::default_toml()),
        now: NOW.into(),
        utc_offset_secs: 0,
        review_model: "opus".into(),
        review_required: true,
        verify_main_after: true,
        gates: gates
            .iter()
            .map(|name| GateInfo {
                name: (*name).into(),
                when: GateWhen::ALL.to_vec(),
            })
            .collect(),
        gate_live: Some(GateLive {
            kind: "merge".into(),
            issue: id(7),
            gates: gates.iter().map(|g| (*g).to_string()).collect(),
            started_at: "2026-06-18T14:24:52Z".into(),
            gate: "test".into(),
            gate_started_at: "2026-06-18T14:27:41Z".into(),
            finished: vec![gate("fmt", true, 3_000), gate("clippy", true, 41_000)],
            tail: String::new(),
            updated_at: NOW.into(),
        }),
        serve: Some(LoopInfo {
            state: LoopState::Running {
                mode: ServeMode::Dispatch,
                age_s: 4,
                waiting: Vec::new(),
            },
            mode: ServeMode::Dispatch,
        }),
        ..Snapshot::default()
    };
    snap.derive();
    snap
}

/// One README picture: the view it shows, how many rows tall it is, and the keys pressed to get the
/// view into the state worth showing (the same keys a person would press).
struct Shot {
    file: &'static str,
    title: &'static str,
    view: View,
    rows: u16,
    keys: &'static str,
}

const SHOTS: [Shot; 3] = [
    Shot {
        file: "board.svg",
        title: "The Board view of klobuk: a kanban of ten demo tasks, one of them waiting for an answer",
        view: View::Board,
        rows: 20,
        keys: "ll",
    },
    Shot {
        file: "now.svg",
        title: "The Now view of klobuk: one question for the human, two running workers and what comes next",
        view: View::Now,
        rows: 20,
        keys: "",
    },
    Shot {
        file: "queue.svg",
        title: "The Queue view of klobuk: a branch in the gates of the merge queue and one merged today",
        view: View::Queue,
        rows: 22,
        keys: "",
    },
];

fn app_for(shot: &Shot) -> App {
    let mut app = App::new(snapshot());
    // colours on whatever `NO_COLOR` says in the environment of the maintainer who regenerates them
    app.theme = Theme::new(true);
    app.view = shot.view;
    app.board.layout = BoardLayout::Kanban;
    // a still picture has no keys: the first queue item shows its steps (as `klobuk tui --once` does)
    app.queue.expanded = true;
    for c in shot.keys.chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app
}

/// The text of each picture's screen, for the tests that keep the demo telling its story.
pub fn frames() -> Vec<(&'static str, String)> {
    SHOTS
        .iter()
        .map(|s| {
            (
                s.file,
                crate::tui::chrome::render_to_text(&app_for(s), COLS, s.rows),
            )
        })
        .collect()
}

/// Every picture as `(file name, SVG text)`, in README order.
pub fn pictures() -> Vec<(&'static str, String)> {
    SHOTS
        .iter()
        .map(|s| {
            (
                s.file,
                svg::render(&render_to_buffer(&app_for(s), COLS, s.rows), s.title),
            )
        })
        .collect()
}
