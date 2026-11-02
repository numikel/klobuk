use std::collections::BTreeMap;

use asf_core::*;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::app::{Action, App, Mode, Snapshot, View};
use super::view::render_to_text;

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
    }
}

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn code(k: KeyCode) -> KeyEvent {
    KeyEvent::new(k, KeyModifiers::NONE)
}

fn app() -> App {
    App::new(snapshot())
}

// ---------- Task 1: state ----------

#[test]
fn number_keys_switch_views_and_q_quits() {
    let mut a = app();
    assert_eq!(a.view, View::Board);
    assert_eq!(a.handle_key(key('2')), Action::None);
    assert_eq!(a.view, View::Agents);
    a.handle_key(key('3'));
    assert_eq!(a.view, View::Inbox);
    a.handle_key(key('4'));
    assert_eq!(a.view, View::Queue);
    a.handle_key(key('5'));
    assert_eq!(a.view, View::Log);
    a.handle_key(key('1'));
    assert_eq!(a.view, View::Board);
    assert_eq!(a.handle_key(key('q')), Action::Quit);
    assert_eq!(a.handle_key(key('R')), Action::Refresh);
    assert_eq!(a.handle_key(key('t')), Action::Tick);
}

#[test]
fn board_selection_moves_in_status_order_and_clamps() {
    let mut a = app();
    // board order = status order: done(asf-1), in-progress(asf-2), ready(asf-3), backlog(asf-4)? No: columns are
    // backlog, ready, in-progress, blocked, in-review, done → asf-4, asf-3, asf-2, asf-1
    assert_eq!(a.selected_issue().unwrap().as_str(), "asf-4");
    a.handle_key(key('j'));
    assert_eq!(a.selected_issue().unwrap().as_str(), "asf-3");
    a.handle_key(code(KeyCode::Down));
    a.handle_key(key('j'));
    assert_eq!(a.selected_issue().unwrap().as_str(), "asf-1");
    a.handle_key(key('j'));
    assert_eq!(
        a.selected_issue().unwrap().as_str(),
        "asf-1",
        "clamped at the end"
    );
    a.handle_key(key('k'));
    a.handle_key(code(KeyCode::Up));
    a.handle_key(key('k'));
    a.handle_key(key('k'));
    assert_eq!(
        a.selected_issue().unwrap().as_str(),
        "asf-4",
        "clamped at the start"
    );
    assert_eq!(a.rows(View::Board), 4);
    assert_eq!(a.rows(View::Agents), 1);
    assert_eq!(a.rows(View::Inbox), 1);
    assert_eq!(a.rows(View::Queue), 1);
    assert_eq!(a.rows(View::Log), 2);
}

#[test]
fn start_only_for_ready_issues() {
    let mut a = app();
    a.handle_key(key('j')); // asf-3 ready
    assert_eq!(a.handle_key(key('s')), Action::Start(id("asf-3")));
    a.handle_key(key('j')); // asf-2 in-progress
    assert_eq!(a.handle_key(key('s')), Action::None);
    assert!(
        a.status.as_deref().unwrap_or("").contains("ready"),
        "{:?}",
        a.status
    );
    assert_eq!(a.handle_key(key('m')), Action::Enqueue(id("asf-2")));
    assert_eq!(a.handle_key(key('f')), Action::Focus(id("asf-2")));
    assert_eq!(
        a.handle_key(key('y')),
        Action::Keys(id("asf-2"), vec!["y".into()])
    );
    assert_eq!(
        a.handle_key(key('n')),
        Action::Keys(id("asf-2"), vec!["n".into()])
    );
}

#[test]
fn reply_input_mode_types_and_sends_or_cancels() {
    let mut a = app();
    a.handle_key(key('j'));
    a.handle_key(key('j')); // asf-2
    assert_eq!(a.handle_key(key('r')), Action::None);
    assert!(matches!(a.mode, Mode::Input { .. }));
    for c in "go on".chars() {
        a.handle_key(key(c));
    }
    a.handle_key(code(KeyCode::Backspace));
    a.handle_key(key('n'));
    // keys like 'q' are text while typing
    a.handle_key(key('q'));
    a.handle_key(code(KeyCode::Backspace));
    assert_eq!(
        a.handle_key(code(KeyCode::Enter)),
        Action::Reply(id("asf-2"), "go on".into())
    );
    assert!(matches!(a.mode, Mode::Normal));
    a.handle_key(key('r'));
    a.handle_key(key('x'));
    assert_eq!(a.handle_key(code(KeyCode::Esc)), Action::None);
    assert!(matches!(a.mode, Mode::Normal));
    // empty reply is not sent
    a.handle_key(key('r'));
    assert_eq!(a.handle_key(code(KeyCode::Enter)), Action::None);
    assert!(matches!(a.mode, Mode::Normal));
}

#[test]
fn abort_requires_confirmation() {
    let mut a = app();
    a.handle_key(key('j'));
    a.handle_key(key('j')); // asf-2 (running)
    assert_eq!(a.handle_key(key('x')), Action::None);
    assert!(matches!(a.mode, Mode::Confirm { .. }));
    assert_eq!(a.handle_key(key('n')), Action::None);
    assert!(matches!(a.mode, Mode::Normal));
    a.handle_key(key('x'));
    assert_eq!(a.handle_key(key('y')), Action::Abort(id("asf-2")));
    // x on an issue without a run does nothing
    a.handle_key(key('k'));
    assert_eq!(a.handle_key(key('x')), Action::None);
    assert!(matches!(a.mode, Mode::Normal));
}

#[test]
fn agents_and_inbox_views_select_their_own_issue() {
    let mut a = app();
    a.handle_key(key('2'));
    assert_eq!(a.selected_issue().unwrap().as_str(), "asf-2");
    a.handle_key(key('3'));
    assert_eq!(a.selected_issue().unwrap().as_str(), "asf-2");
    assert_eq!(a.handle_key(code(KeyCode::Enter)), Action::MarkRead(id("asf-2")));
    a.handle_key(key('4'));
    assert_eq!(a.selected_issue().unwrap().as_str(), "asf-1");
    a.handle_key(key('5'));
    assert_eq!(a.selected_issue(), None);
}

#[test]
fn snapshot_replace_keeps_selection_in_range() {
    let mut a = app();
    a.handle_key(key('j'));
    a.handle_key(key('j'));
    a.handle_key(key('j')); // asf-1 (index 3)
    let mut s = snapshot();
    s.issues.truncate(1);
    a.replace(s);
    assert_eq!(a.selected_issue().unwrap().as_str(), "asf-1");
    assert_eq!(a.rows(View::Board), 1);
}

// ---------- Task 2: rendering ----------

#[test]
fn board_renders_header_columns_and_ids() {
    let a = app();
    let text = render_to_text(&a, 100, 30);
    for needle in [
        "hello",
        "main",
        "workers 1/3",
        "queue 1",
        "backlog",
        "ready",
        "in-progress",
        "blocked",
        "in-review",
        "done",
    ] {
        assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
    }
    for idn in ["asf-1", "asf-2", "asf-3", "asf-4"] {
        assert!(text.contains(idn), "missing {idn} in:\n{text}");
    }
    assert!(text.contains("Release"), "selected issue detail:\n{text}");
    assert!(text.contains("src/**"));
}

#[test]
fn agents_inbox_queue_log_render_their_rows() {
    let mut a = app();
    a.handle_key(key('2'));
    let t = render_to_text(&a, 100, 30);
    assert!(
        t.contains("asf-2") && t.contains("$1.25") && t.contains("working") && t.contains("f:p1"),
        "{t}"
    );
    a.handle_key(key('3'));
    let t = render_to_text(&a, 100, 30);
    assert!(
        t.contains("BLOCKER") && t.contains("Worker is waiting for input."),
        "{t}"
    );
    a.handle_key(key('4'));
    let t = render_to_text(&a, 100, 30);
    assert!(t.contains("feat/asf-1") && t.contains("queued"), "{t}");
    a.handle_key(key('5'));
    let t = render_to_text(&a, 100, 30);
    assert!(t.contains("run.state") && t.contains("issue.created"), "{t}");
}

#[test]
fn input_and_confirm_modes_render_prompts_and_small_sizes_do_not_panic() {
    let mut a = app();
    a.handle_key(key('j'));
    a.handle_key(key('j'));
    a.handle_key(key('r'));
    for c in "hi".chars() {
        a.handle_key(key(c));
    }
    let t = render_to_text(&a, 100, 30);
    assert!(t.contains("reply to asf-2") && t.contains("hi"), "{t}");
    a.handle_key(code(KeyCode::Esc));
    a.handle_key(key('x'));
    let t = render_to_text(&a, 100, 30);
    assert!(t.contains("abort asf-2?"), "{t}");
    let _ = render_to_text(&a, 60, 15);
    let _ = render_to_text(&a, 20, 5);
}

#[test]
fn status_message_shows_in_footer() {
    let mut a = app();
    a.status = Some("started asf-3".into());
    let t = render_to_text(&a, 100, 30);
    assert!(t.contains("started asf-3"), "{t}");
}
