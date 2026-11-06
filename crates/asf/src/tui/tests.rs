use std::collections::BTreeMap;

use asf_core::*;
use std::time::Duration;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::app::{board_columns, config_rows, Action, App, Mode, Snapshot, View, STATUS_TTL};
use super::keys::{footer_hints, ModeKind, BINDINGS};
use super::view::{render_to_buffer, render_to_text};
use super::{bg_outcome, config_set_status, config_unset_status, keys_status, BgKind};

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

// ---------- Task 1: state ----------

#[test]
fn number_keys_switch_views_and_q_quits() {
    let mut a = App::new(snapshot());
    assert_eq!(a.view, View::Now, "the first view is the default");
    for (digit, view) in ['1', '2', '3', '4', '5', '6', '7'].into_iter().zip(View::ALL) {
        assert_eq!(a.handle_key(key(digit)), Action::None);
        assert_eq!(a.view, view, "key {digit}");
    }
    assert_eq!(
        View::ALL,
        [
            View::Now,
            View::Board,
            View::Agents,
            View::Inbox,
            View::Queue,
            View::Log,
            View::Config
        ]
    );
    assert_eq!(a.handle_key(key('8')), Action::None);
    assert_eq!(a.view, View::Config, "there is no eighth view");
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
    // asf-30: bare y/n no longer reach the worker from normal mode.
    assert_eq!(a.handle_key(key('y')), Action::None);
    assert_eq!(a.handle_key(key('n')), Action::None);
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
    a.handle_key(key('3'));
    assert_eq!(a.selected_issue().unwrap().as_str(), "asf-2");
    a.handle_key(key('4'));
    assert_eq!(a.selected_issue().unwrap().as_str(), "asf-2");
    // Enter only opens the comment now; marking read is `m` / `M`.
    assert_eq!(a.handle_key(code(KeyCode::Enter)), Action::None);
    assert!(matches!(a.mode, Mode::Comment { .. }));
    a.handle_key(code(KeyCode::Esc));
    a.handle_key(key('5'));
    assert_eq!(a.selected_issue().unwrap().as_str(), "asf-1");
    a.handle_key(key('6'));
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
    let text = render_to_text(&a, 120, 30);
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
        "validating",
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
    a.handle_key(key('3'));
    let t = render_to_text(&a, 100, 30);
    assert!(
        t.contains("asf-2") && t.contains("$1.25") && t.contains("working") && t.contains("f:p1"),
        "{t}"
    );
    a.handle_key(key('4'));
    let t = render_to_text(&a, 100, 30);
    assert!(
        t.contains("BLOCKER") && t.contains("Worker is waiting for input."),
        "{t}"
    );
    a.handle_key(key('5'));
    let t = render_to_text(&a, 100, 30);
    assert!(t.contains("feat/asf-1") && t.contains("queued"), "{t}");
    a.handle_key(key('6'));
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

// ---------- asf-22: validating column and Config view ----------

#[test]
fn board_columns_follow_status_all_without_cancelled() {
    let expected: Vec<Status> = Status::ALL
        .iter()
        .copied()
        .filter(|s| *s != Status::Cancelled)
        .collect();
    assert_eq!(board_columns(), expected);
    let cols = board_columns();
    let pos = |s: Status| cols.iter().position(|c| *c == s).unwrap();
    assert_eq!(pos(Status::Validating), pos(Status::InReview) + 1);
    assert_eq!(pos(Status::Done), pos(Status::Validating) + 1);
}

#[test]
fn board_renders_a_validating_column_between_in_review_and_done() {
    let mut s = snapshot();
    s.issues.push(issue("asf-5", "Verify gates", Status::Validating));
    let a = board_app(s);
    let text = render_to_text(&a, 120, 30);
    let title_row = text
        .lines()
        .find(|l| l.contains("in-review") && l.contains("validating") && l.contains("done"))
        .unwrap_or_else(|| panic!("no column title row with all three:\n{text}"));
    let at = |needle: &str| title_row.find(needle).unwrap();
    assert!(
        at("in-review") < at("validating") && at("validating") < at("done"),
        "{title_row}"
    );
    assert!(text.contains("asf-5"), "validating issue missing:\n{text}");
    assert_eq!(a.rows(View::Board), 5, "board_issues must include validating");
}

#[test]
fn key_six_opens_config_view_listing_every_editable_key_with_its_value() {
    let mut a = app();
    assert_eq!(a.handle_key(key('7')), Action::None);
    assert_eq!(a.view, View::Config);
    assert_eq!(a.rows(View::Config), asf_core::config_edit::editable_keys().len());
    let text = render_to_text(&a, 100, 30);
    for (k, _) in asf_core::config_edit::editable_keys() {
        assert!(text.contains(k), "missing key {k} in:\n{text}");
    }
    // current values come from the config text: defaults are worker.budget_usd = 5.0, merge.target = local
    assert!(text.contains("local"), "{text}");
    assert!(
        text.contains("(unset)"),
        "worker.model is unset by default:\n{text}"
    );
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

#[test]
fn enter_opens_prefilled_input_and_second_enter_returns_the_edit() {
    let mut a = app();
    select_config_key(&mut a, "scheduler.max_parallel");
    assert_eq!(a.handle_key(code(KeyCode::Enter)), Action::None);
    match &a.mode {
        Mode::Input { prompt, buf, .. } => {
            assert!(prompt.contains("scheduler.max_parallel"), "{prompt}");
            assert_eq!(buf, "3", "prefilled with the current value");
        }
        other => panic!("expected input mode, got {other:?}"),
    }
    a.handle_key(code(KeyCode::Backspace));
    a.handle_key(key('5'));
    assert_eq!(
        a.handle_key(code(KeyCode::Enter)),
        Action::SetConfig("scheduler.max_parallel".into(), "5".into())
    );
    assert!(matches!(a.mode, Mode::Normal));
}

#[test]
fn esc_in_config_input_cancels_without_an_action() {
    let mut a = app();
    select_config_key(&mut a, "merge.target");
    a.handle_key(code(KeyCode::Enter));
    a.handle_key(key('x'));
    assert_eq!(a.handle_key(code(KeyCode::Esc)), Action::None);
    assert!(matches!(a.mode, Mode::Normal));
    assert_eq!(a.view, View::Config);
}

#[test]
fn enter_on_a_view_without_rows_does_not_open_input() {
    let mut a = board_app(Snapshot::default());
    a.handle_key(key('7'));
    assert_eq!(a.handle_key(code(KeyCode::Enter)), Action::None);
    assert!(matches!(a.mode, Mode::Normal));
}

fn config_dir(toml: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".factory")).unwrap();
    let path = dir.path().join(".factory/config.toml");
    std::fs::write(&path, toml).unwrap();
    (dir, path)
}

#[test]
fn confirmed_edit_is_written_to_config_toml_and_reported() {
    let (dir, path) = config_dir(Config::default_toml());
    let msg = config_set_status(dir.path(), "scheduler.max_parallel", "5");
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(after.contains("max_parallel = 5"), "file not updated:\n{after}");
    assert!(
        after.contains("# concurrent workers"),
        "inline comment must survive:\n{after}"
    );
    assert!(!msg.starts_with("error"), "{msg}");
    assert!(
        msg.contains("scheduler.max_parallel") && msg.contains('5'),
        "{msg}"
    );
}

#[test]
fn invalid_value_leaves_the_file_unchanged_and_status_line_shows_the_error() {
    let (dir, path) = config_dir(Config::default_toml());
    let before = std::fs::read(&path).unwrap();
    let msg = config_set_status(dir.path(), "scheduler.max_parallel", "0");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "file must be byte-identical"
    );
    assert!(msg.starts_with("error"), "{msg}");
    let msg2 = config_set_status(dir.path(), "merge.target", "svn");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(msg2.starts_with("error"), "{msg2}");
    let mut a = app();
    a.view = View::Config;
    a.status = Some(msg);
    let text = render_to_text(&a, 120, 30);
    assert!(text.contains("error") && text.contains("max_parallel"), "{text}");
}

#[test]
fn config_rows_read_values_from_the_toml_text() {
    let rows = config_rows("[scheduler]\nmax_parallel = 7\n[merge]\ntarget = \"github\"\n");
    let get = |k: &str| rows.iter().find(|r| r.key == k).unwrap();
    assert_eq!(get("scheduler.max_parallel").raw, "7");
    assert_eq!(get("merge.target").raw, "github");
    assert_eq!(get("worker.model").raw, "");
    assert_eq!(get("worker.model").value, "(unset)");
    assert_eq!(rows.len(), asf_core::config_edit::editable_keys().len());
}

// ---------- asf-30: waiting agents ----------

use ratatui::style::{Color, Modifier};

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

#[test]
fn header_counts_questions_plus_blocked_runs_only_when_positive() {
    let t = render_to_text(&app(), 120, 30);
    assert!(!t.contains("waiting"), "nothing waits, nothing is said:\n{t}");

    let mut s = blocked_snapshot();
    s.questions = vec![question(7, "asf-3", "which crate?", "2026-11-02T10:06:00Z")];
    let a = board_app(s);
    let t = render_to_text(&a, 120, 30);
    assert!(line_of(&t, 0).contains("2 waiting"), "header:\n{t}");
    let (fg, m) = cell_style(&a, "asf ·", "2 waiting");
    assert_eq!(fg, Color::Cyan, "accent colour");
    assert!(m.contains(Modifier::BOLD));
}

#[test]
fn banner_names_a_blocked_run_directly_under_the_tab_bar() {
    let a = board_app(blocked_snapshot());
    let t = render_to_text(&a, 120, 30);
    assert!(line_of(&t, 1).contains("[2] Board"), "tab bar is line 1:\n{t}");
    assert_eq!(
        line_of(&t, 2).trim(),
        "■ asf-2 is waiting for your answer · a answer · f focus pane"
    );
    let (fg, m) = cell_style(&a, "is waiting", "■ asf-2 is waiting");
    assert_eq!(fg, Color::Yellow);
    assert!(m.contains(Modifier::BOLD));
}

#[test]
fn banner_shows_the_first_line_of_an_unanswered_question() {
    let mut s = snapshot();
    s.questions = vec![question(
        7,
        "asf-3",
        "Which crate owns the parser?\nsecond line is not shown",
        "2026-11-02T10:06:00Z",
    )];
    let t = render_to_text(&board_app(s), 120, 30);
    assert_eq!(
        line_of(&t, 2).trim(),
        "■ asf-3 asks: Which crate owns the parser?"
    );
    assert!(!t.contains("second line is not shown"), "{t}");
}

#[test]
fn banner_with_several_waiters_shows_the_oldest_and_a_count() {
    let mut s = blocked_snapshot(); // run started 10:00
    s.questions = vec![
        question(8, "asf-3", "newer question", "2026-11-02T10:09:00Z"),
        question(7, "asf-4", "older question", "2026-11-02T10:06:00Z"),
    ];
    let t = render_to_text(&board_app(s), 120, 30);
    assert_eq!(
        line_of(&t, 2).trim(),
        "■ asf-2 is waiting for your answer · a answer · f focus pane (+2 more)"
    );
    let mut s = snapshot();
    s.questions = vec![
        question(8, "asf-3", "newer question", "2026-11-02T10:09:00Z"),
        question(7, "asf-4", "older question", "2026-11-02T10:06:00Z"),
    ];
    let t = render_to_text(&board_app(s), 120, 30);
    assert_eq!(line_of(&t, 2).trim(), "■ asf-4 asks: older question (+1 more)");
}

#[test]
fn long_question_is_truncated_but_keeps_the_more_suffix_inside_the_width() {
    let mut s = snapshot();
    s.questions = vec![
        question(
            7,
            "asf-4",
            &"very long question ".repeat(10),
            "2026-11-02T10:06:00Z",
        ),
        question(8, "asf-3", "other", "2026-11-02T10:07:00Z"),
    ];
    let t = render_to_text(&board_app(s), 80, 20);
    let l = line_of(&t, 2);
    assert!(l.chars().count() <= 80, "{l:?}");
    assert!(
        l.starts_with("■ asf-4 asks: very long") && l.contains('…'),
        "{l:?}"
    );
    assert!(l.trim_end().ends_with("(+1 more)"), "{l:?}");
}

#[test]
fn banner_is_visible_in_every_view_but_now() {
    let mut a = board_app(blocked_snapshot());
    for c in '2'..='7' {
        a.handle_key(key(c));
        let t = render_to_text(&a, 120, 30);
        assert!(
            line_of(&t, 2).starts_with("■ asf-2 is waiting"),
            "view {:?}:\n{t}",
            a.view
        );
    }
}

#[test]
fn no_banner_and_no_reserved_line_when_nothing_waits() {
    let t = render_to_text(&app(), 120, 30);
    assert!(
        line_of(&t, 2).contains("backlog"),
        "content starts right under the tab bar:\n{t}"
    );
    assert!(!t.contains("is waiting for your answer") && !t.contains(" asks:"));
}

#[test]
fn answered_question_is_not_waiting() {
    let q = question(7, "asf-3", "which crate?", "2026-11-02T10:06:00Z");
    let mut ans = comment("asf-3", CommentType::Answer, "the core one");
    ans.id = 9;
    ans.reply_to = Some(7);
    assert!(super::app::unanswered_questions(&[q.clone(), ans]).is_empty());
    assert_eq!(
        super::app::unanswered_questions(std::slice::from_ref(&q)),
        vec![7]
    );
    let mut other = q;
    other.to_lane = "orchestrator".into();
    assert!(super::app::unanswered_questions(&[other]).is_empty());
}

#[test]
fn board_marks_a_blocked_run_with_a_red_bold_question_marker() {
    let a = board_app(blocked_snapshot());
    let t = render_to_text(&a, 120, 30);
    assert!(t.contains("■ ? asf-2") && !t.contains('●'), "{t}");
    let (fg, m) = cell_style(&a, "■ ? asf-2", "■ ? asf-2");
    assert_eq!(fg, Color::Red);
    assert!(m.contains(Modifier::BOLD));
    let t = render_to_text(&app(), 120, 30);
    assert!(
        t.contains("● asf-2") && !t.contains('■'),
        "a working run is a green dot:\n{t}"
    );
}

#[test]
fn agents_row_of_a_blocked_run_starts_with_a_red_bold_square() {
    let mut a = board_app(blocked_snapshot());
    a.handle_key(key('3'));
    let t = render_to_text(&a, 120, 30);
    let row = t
        .lines()
        .find(|l| l.contains("asf-2") && l.contains("blocked") && l.contains('│'))
        .unwrap_or_else(|| panic!("no agents row:\n{t}"));
    assert!(row.trim_start_matches([' ', '│']).starts_with('■'), "{row:?}");
    let (fg, m) = cell_style(&a, "blocked", "■");
    assert_eq!(fg, Color::Red);
    assert!(m.contains(Modifier::BOLD));
    let mut w = board_app(snapshot());
    w.handle_key(key('3'));
    assert!(!render_to_text(&w, 120, 30).contains('■'));
}

// ---------- answer overlay ----------

fn open_overlay() -> App {
    let mut a = board_app(blocked_snapshot());
    a.handle_key(key('3'));
    assert_eq!(a.handle_key(key('a')), Action::Peek(id("asf-2")));
    a
}

#[test]
fn a_opens_the_overlay_only_for_a_blocked_run() {
    let mut w = board_app(snapshot());
    w.handle_key(key('3')); // asf-2 is working
    assert_eq!(w.handle_key(key('a')), Action::None);
    assert!(matches!(w.mode, Mode::Normal));
    assert!(
        w.status.as_deref().unwrap_or("").contains("no blocked run"),
        "{:?}",
        w.status
    );
    // an issue without a run (the board starts on asf-4, backlog)
    let mut b = board_app(blocked_snapshot());
    assert_eq!(b.handle_key(key('a')), Action::None);
    assert!(matches!(b.mode, Mode::Normal));
    // a blocked run, from Board, Agents and Inbox
    for (view, steps) in [('2', 2), ('3', 0), ('4', 0)] {
        let mut a = board_app(blocked_snapshot());
        a.handle_key(key(view));
        for _ in 0..steps {
            a.handle_key(key('j'));
        }
        assert_eq!(a.handle_key(key('a')), Action::Peek(id("asf-2")), "view {view}");
        assert!(matches!(a.mode, Mode::Answer { .. }));
    }
}

#[test]
fn overlay_renders_title_screen_and_hints() {
    let mut a = open_overlay();
    a.set_screen("Do you want to proceed?\n 1. Yes\n 2. No".into());
    let t = render_to_text(&a, 100, 30);
    for needle in [
        "answer asf-2",
        "Do you want to proceed?",
        "1. Yes",
        "2. No",
        "Esc",
    ] {
        assert!(t.contains(needle), "missing {needle:?}:\n{t}");
    }
    // tiny terminals must not panic
    let _ = render_to_text(&a, 20, 5);
}

#[test]
fn overlay_hint_line_and_typed_reply_stay_visible_in_short_panes() {
    for height in [24u16, 20] {
        let mut a = open_overlay();
        a.set_screen("Do you want to proceed?\n 1. Yes\n 2. No".into());
        let keys = render_to_text(&a, 100, height);
        assert!(
            keys.contains("1-9 / Enter / Up / Down send keys") && keys.contains("t type text"),
            "key hints hidden at height {height}:\n{keys}"
        );
        a.handle_key(key('t'));
        for c in "go ahead".chars() {
            a.handle_key(key(c));
        }
        let typed = render_to_text(&a, 100, height);
        assert!(
            typed.contains("reply: go ahead"),
            "typed reply hidden at height {height}:\n{typed}"
        );
        // the footer keeps its own row under the popup
        let last = typed.lines().last().unwrap_or("");
        assert!(
            last.contains("answering asf-2"),
            "footer at height {height}: {last:?}"
        );
    }
}

#[test]
fn digit_in_the_overlay_sends_keys_and_does_not_switch_views() {
    let mut a = open_overlay();
    assert_eq!(a.view, View::Agents);
    assert_eq!(
        a.handle_key(key('3')),
        Action::Keys(id("asf-2"), vec!["3".into()])
    );
    assert_eq!(a.view, View::Agents, "digit is an answer, not a view switch");
    assert!(matches!(a.mode, Mode::Normal), "modal closes after sending");
}

#[test]
fn enter_up_down_y_n_are_sent_as_keys() {
    for (k, name) in [
        (code(KeyCode::Enter), "enter"),
        (code(KeyCode::Up), "up"),
        (code(KeyCode::Down), "down"),
        (key('y'), "y"),
        (key('n'), "n"),
    ] {
        let mut a = open_overlay();
        assert_eq!(
            a.handle_key(k),
            Action::Keys(id("asf-2"), vec![name.into()]),
            "{name}"
        );
        assert!(matches!(a.mode, Mode::Normal));
    }
}

#[test]
fn esc_closes_the_overlay_and_sends_nothing() {
    let mut a = open_overlay();
    assert_eq!(a.handle_key(code(KeyCode::Esc)), Action::None);
    assert!(matches!(a.mode, Mode::Normal));
    assert_eq!(a.view, View::Agents);
}

#[test]
fn no_other_binding_is_active_while_the_overlay_is_open() {
    let mut a = open_overlay();
    for c in ['q', 's', 'x', 'f', 'm', 'r', 'R', 'j', 'k', 'a', '0', 'd'] {
        assert_eq!(a.handle_key(key(c)), Action::None, "{c}");
        assert!(
            matches!(a.mode, Mode::Answer { .. }),
            "{c} must not close or change the modal"
        );
        assert_eq!(a.view, View::Agents);
    }
}

#[test]
fn t_switches_to_text_input_that_is_sent_as_a_reply() {
    let mut a = open_overlay();
    assert_eq!(a.handle_key(key('t')), Action::None);
    for c in "use 2".chars() {
        assert_eq!(a.handle_key(key(c)), Action::None, "typed {c}");
    }
    a.handle_key(code(KeyCode::Backspace));
    a.handle_key(key('3'));
    a.set_screen("refreshed screen".into()); // a late peek must not drop the typed text
    let t = render_to_text(&a, 100, 30);
    assert!(t.contains("use 3"), "{t}");
    assert_eq!(
        a.handle_key(code(KeyCode::Enter)),
        Action::Reply(id("asf-2"), "use 3".into())
    );
    assert!(matches!(a.mode, Mode::Normal));
    // empty text is not sent; Esc closes without sending
    let mut a = open_overlay();
    a.handle_key(key('t'));
    assert_eq!(a.handle_key(code(KeyCode::Enter)), Action::None);
    a.handle_key(key('x'));
    assert_eq!(a.handle_key(code(KeyCode::Esc)), Action::None);
    assert!(matches!(a.mode, Mode::Normal));
}

#[test]
fn sent_keys_are_reported_in_the_status_line() {
    assert_eq!(
        keys_status(&id("asf-27"), &["2".into(), "enter".into()]),
        "sent 2 enter to asf-27"
    );
}

#[test]
fn footer_lists_a_answer_and_no_longer_y_n_keys() {
    let t = render_to_text(&app(), 200, 30);
    assert!(t.contains("a answer") && !t.contains("y/n keys"), "{t}");
}

// ---------- d = unset an optional config key ----------

fn config_app(toml: &str, wanted: &str) -> App {
    let mut s = snapshot();
    s.config = config_rows(toml);
    let mut a = board_app(s);
    select_config_key(&mut a, wanted);
    a
}

#[test]
fn d_asks_for_confirmation_and_y_returns_the_unset_action() {
    for key_name in ["worker.model", "review.model"] {
        let mut a = config_app(
            "[worker]\nmodel = \"sonnet\"\n[review]\nmodel = \"sonnet\"\n",
            key_name,
        );
        assert_eq!(a.handle_key(key('d')), Action::None);
        match &a.mode {
            Mode::Confirm { what, .. } => {
                assert!(what.contains(&format!("unset {key_name}?")), "{what}")
            }
            other => panic!("expected confirm, got {other:?}"),
        }
        let t = render_to_text(&a, 120, 30);
        assert!(t.contains(&format!("unset {key_name}?")), "{t}");
        assert_eq!(a.handle_key(key('y')), Action::UnsetConfig(key_name.into()));
        assert!(matches!(a.mode, Mode::Normal));
    }
}

#[test]
fn any_other_key_cancels_the_unset() {
    for cancel in [
        key('n'),
        key('q'),
        code(KeyCode::Enter),
        code(KeyCode::Esc),
        key('d'),
    ] {
        let mut a = config_app("[worker]\nmodel = \"sonnet\"\n", "worker.model");
        a.handle_key(key('d'));
        assert_eq!(a.handle_key(cancel), Action::None);
        assert!(matches!(a.mode, Mode::Normal));
    }
}

#[test]
fn d_on_a_required_key_or_an_already_unset_one_only_sets_a_status() {
    let mut a = config_app(Config::default_toml(), "scheduler.max_parallel");
    assert_eq!(a.handle_key(key('d')), Action::None);
    assert!(matches!(a.mode, Mode::Normal));
    assert!(
        a.status.as_deref().unwrap_or("").contains("cannot be unset"),
        "{:?}",
        a.status
    );
    let mut a = config_app(Config::default_toml(), "worker.model"); // unset by default
    assert_eq!(a.handle_key(key('d')), Action::None);
    assert!(matches!(a.mode, Mode::Normal));
    assert!(
        a.status.as_deref().unwrap_or("").contains("already unset"),
        "{:?}",
        a.status
    );
    // d outside the Config view does nothing
    let mut b = app();
    assert_eq!(b.handle_key(key('d')), Action::None);
    assert!(matches!(b.mode, Mode::Normal) && b.status.is_none());
}

#[test]
fn confirmed_unset_removes_the_line_from_config_toml_and_reports() {
    let (dir, path) = config_dir("# keep me\n[worker]\nbudget_usd = 5.0\nmodel = \"sonnet\"\n");
    let msg = config_unset_status(dir.path(), "worker.model");
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after, "# keep me\n[worker]\nbudget_usd = 5.0\n");
    assert!(
        !msg.starts_with("error") && msg.contains("worker.model") && msg.contains("(unset)"),
        "{msg}"
    );
}

#[test]
fn refused_unset_leaves_the_file_byte_identical() {
    let (dir, path) = config_dir(Config::default_toml());
    let before = std::fs::read(&path).unwrap();
    let msg = config_unset_status(dir.path(), "scheduler.max_parallel");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(msg.starts_with("error"), "{msg}");
}

// ---------- asf-31: status expiry ----------

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

#[test]
fn status_expires_after_four_seconds_and_the_hints_come_back() {
    let mut a = app();
    a.advance(ms(1_000));
    a.status = Some("started asf-3".into());
    assert!(footer_line(&a, 100).contains("started asf-3"));
    a.advance(ms(1_000)); // the first tick after the message stamps it
    a.advance(ms(1_000) + STATUS_TTL - ms(1));
    assert!(a.status.is_some(), "still within the 4 s");
    a.advance(ms(1_000) + STATUS_TTL);
    assert!(a.status.is_none());
    let f = footer_line(&a, 100);
    assert!(f.contains("? help") && !f.contains("started asf-3"), "{f:?}");
}

#[test]
fn status_goes_away_on_the_next_key_press_but_a_key_may_set_a_new_one() {
    let mut a = app();
    a.status = Some("sent to asf-2".into());
    a.handle_key(key('j'));
    assert!(a.status.is_none());
    a.handle_key(key('j')); // asf-2, in progress
    a.handle_key(key('s'));
    assert!(
        a.status.as_deref().unwrap_or("").contains("only ready"),
        "{:?}",
        a.status
    );
    a.handle_key(key('k'));
    assert!(a.status.is_none(), "cleared again by the following key");
}

// ---------- asf-31: Inbox open / mark read ----------

fn inbox_app() -> App {
    let mut a = app();
    a.handle_key(key('4'));
    a
}

#[test]
fn enter_opens_the_comment_modal_and_changes_nothing_unread() {
    let mut a = inbox_app();
    assert_eq!(a.handle_key(code(KeyCode::Enter)), Action::None);
    assert!(matches!(a.mode, Mode::Comment { .. }));
    assert_eq!(a.rows(View::Inbox), 1, "unread count is untouched");
    let t = render_to_text(&a, 100, 30);
    assert!(
        t.contains("#1 asf-2 [orchestrator] BLOCKER -> human"),
        "header line:\n{t}"
    );
    assert!(
        t.contains("Worker is waiting for input.") && t.contains("Screen: Allow rm?"),
        "{t}"
    );
    assert_eq!(a.handle_key(code(KeyCode::Esc)), Action::None);
    assert!(matches!(a.mode, Mode::Normal));
    assert_eq!(a.rows(View::Inbox), 1);
}

#[test]
fn the_comment_modal_wraps_and_scrolls_with_j_and_k() {
    let mut s = snapshot();
    s.inbox[0].body = (1..=60)
        .map(|n| format!("line {n:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut a = board_app(s);
    a.handle_key(key('4'));
    a.handle_key(code(KeyCode::Enter));
    let top = render_to_text(&a, 100, 30);
    assert!(top.contains("line 10") && !top.contains("line 40"), "{top}");
    for _ in 0..30 {
        a.handle_key(key('j'));
    }
    let mid = render_to_text(&a, 100, 30);
    assert!(mid.contains("line 31") && !mid.contains("line 10"), "{mid}");
    for _ in 0..200 {
        a.handle_key(code(KeyCode::Down));
    }
    let end = render_to_text(&a, 100, 30);
    assert!(end.contains("line 60"), "clamped at the end:\n{end}");
    for _ in 0..200 {
        a.handle_key(key('k'));
    }
    assert!(render_to_text(&a, 100, 30).contains("line 10"));
    // a long single line is wrapped, not cut
    let mut s = snapshot();
    s.inbox[0].body = (1..=80).map(|n| format!("w{n:02}")).collect::<Vec<_>>().join(" ");
    let mut a = board_app(s);
    a.handle_key(key('4'));
    a.handle_key(code(KeyCode::Enter));
    let t = render_to_text(&a, 80, 30);
    // the list behind the modal shows the first words; only a wrapped modal can show the last ones
    for n in 1..=80 {
        assert!(t.contains(&format!("w{n:02}")), "w{n:02} was cut:\n{t}");
    }
}

#[test]
fn m_marks_the_selected_issues_conversation_read_and_enter_does_not() {
    let mut a = inbox_app();
    assert_eq!(a.handle_key(key('m')), Action::MarkRead(id("asf-2")));
    // on the Board `m` is still "merge"
    let mut b = app();
    b.handle_key(key('j'));
    b.handle_key(key('j'));
    assert_eq!(b.handle_key(key('m')), Action::Enqueue(id("asf-2")));
    // nothing selected, nothing to mark
    let mut e = board_app(Snapshot::default());
    e.handle_key(key('4'));
    assert_eq!(e.handle_key(key('m')), Action::None);
    assert_eq!(e.handle_key(key('M')), Action::None);
}

#[test]
fn capital_m_asks_before_marking_everything() {
    let mut a = inbox_app();
    assert_eq!(a.handle_key(key('M')), Action::None);
    match &a.mode {
        Mode::Confirm { what, .. } => assert_eq!(what, "mark all 1 read?"),
        other => panic!("expected confirm, got {other:?}"),
    }
    assert!(render_to_text(&a, 100, 30).contains("mark all 1 read?"));
    assert_eq!(a.handle_key(key('n')), Action::None);
    assert!(matches!(a.mode, Mode::Normal));
    a.handle_key(key('M'));
    assert_eq!(a.handle_key(key('y')), Action::MarkAllRead);
    assert!(matches!(a.mode, Mode::Normal));
}

// ---------- asf-31: background actions ----------

#[test]
fn a_second_background_request_answers_busy_and_runs_nothing() {
    let mut a = app();
    a.busy = Some("tick".into());
    assert_eq!(a.handle_key(key('t')), Action::None);
    assert_eq!(a.status.as_deref(), Some("busy: tick"));
    a.handle_key(key('j')); // asf-3 is ready
    assert_eq!(a.handle_key(key('s')), Action::None);
    assert_eq!(a.status.as_deref(), Some("busy: tick"));
    a.busy = None;
    assert_eq!(a.handle_key(key('s')), Action::Start(id("asf-3")));
}

#[test]
fn the_header_shows_a_spinner_and_label_while_busy_and_every_key_still_works() {
    let mut a = app();
    a.busy = Some("starting asf-25".into());
    a.advance(ms(0));
    let h = render_to_text(&a, 100, 30).lines().next().unwrap().to_string();
    assert!(h.contains("⠋ starting asf-25…"), "{h:?}");
    a.advance(ms(100));
    let h = render_to_text(&a, 100, 30).lines().next().unwrap().to_string();
    assert!(
        h.contains("⠙ starting asf-25…"),
        "the frame advances with the clock: {h:?}"
    );
    a.handle_key(key('3'));
    assert_eq!(a.view, View::Agents, "views still switch while busy");
    a.busy = None;
    let h = render_to_text(&a, 100, 30).lines().next().unwrap().to_string();
    assert!(!h.contains('⠙') && !h.contains('…'), "{h:?}");
}

#[test]
fn child_output_becomes_a_check_or_a_cross_with_the_envelope_message() {
    let tick = r#"{"ok":true,"result":{"promoted":["asf-3"],"closed_epics":[],"watch":{"checked":2,"transitions":[{"x":1}]},"merge":{"items":[{"id":1},{"id":2}]},"started":[],"start_errors":[]}}"#;
    assert_eq!(
        bg_outcome(&BgKind::Tick, tick, "", Some(0)),
        "✓ tick: 1 transition(s), 2 merge item(s), 1 promoted, 0 epic(s) closed"
    );
    let start = r#"{"ok":true,"result":{"run":{"issue_id":"asf-25"},"workspace_id":"w1","pane_id":"p1","trusted":false}}"#;
    assert_eq!(
        bg_outcome(&BgKind::Start(id("asf-25")), start, "", Some(0)),
        "✓ started asf-25 in w1 (pane p1)"
    );
    let err = r#"{"ok":false,"error":{"code":"conflict","message":"asf-25 is already claimed"}}"#;
    assert_eq!(
        bg_outcome(&BgKind::Start(id("asf-25")), err, "", Some(3)),
        "✗ asf-25 is already claimed"
    );
    // a crash prints no envelope: say what we know
    assert_eq!(
        bg_outcome(&BgKind::Tick, "", "boom\nmore", Some(101)),
        "✗ tick exited with 101: boom"
    );
    assert_eq!(bg_outcome(&BgKind::Tick, "", "", None), "✗ tick was terminated");
}

// ---------- asf-31: control keys ----------

fn ctrl(c: char) -> KeyEvent {
    mods(c, KeyModifiers::CONTROL)
}

#[test]
fn ctrl_c_quits_from_every_mode() {
    let mut input = app();
    for c in ['j', 'j', 'r'] {
        input.handle_key(key(c));
    }
    let mut confirm = app();
    for c in ['j', 'j', 'x'] {
        confirm.handle_key(key(c));
    }
    let mut text = open_overlay();
    text.handle_key(key('t'));
    let mut comment = inbox_app();
    comment.handle_key(code(KeyCode::Enter));
    let mut help = app();
    help.handle_key(key('?'));
    let modes = [
        ("normal", app()),
        ("input", input),
        ("confirm", confirm),
        ("answer", open_overlay()),
        ("answer text", text),
        ("comment", comment),
        ("help", help),
    ];
    for (name, mut a) in modes {
        assert_eq!(a.handle_key(ctrl('c')), Action::Quit, "{name}");
    }
}

fn typing_app() -> App {
    let mut a = app();
    a.handle_key(key('j'));
    a.handle_key(key('j'));
    a.handle_key(key('r'));
    a
}

fn buf_of(a: &App) -> String {
    match &a.mode {
        Mode::Input { buf, .. } => buf.clone(),
        other => panic!("not in input mode: {other:?}"),
    }
}

#[test]
fn ctrl_w_deletes_the_last_word_and_ctrl_u_clears_the_line() {
    let mut a = typing_app();
    for c in "go on  now".chars() {
        a.handle_key(key(c));
    }
    a.handle_key(ctrl('w'));
    assert_eq!(buf_of(&a), "go on  ");
    a.handle_key(ctrl('w'));
    assert_eq!(buf_of(&a), "go ");
    a.handle_key(ctrl('w'));
    assert_eq!(buf_of(&a), "");
    a.handle_key(ctrl('w')); // nothing to delete: no panic
    for c in "abc".chars() {
        a.handle_key(key(c));
    }
    a.handle_key(ctrl('u'));
    assert_eq!(buf_of(&a), "");
    // the same keys edit the free-text reply of the answer overlay
    let mut o = open_overlay();
    o.handle_key(key('t'));
    for c in "use 2 please".chars() {
        o.handle_key(key(c));
    }
    o.handle_key(ctrl('w'));
    assert_eq!(
        o.handle_key(code(KeyCode::Enter)),
        Action::Reply(id("asf-2"), "use 2".into())
    );
    let mut o = open_overlay();
    o.handle_key(key('t'));
    o.handle_key(key('x'));
    o.handle_key(ctrl('u'));
    assert_eq!(
        o.handle_key(code(KeyCode::Enter)),
        Action::None,
        "empty text is not sent"
    );
}

#[test]
fn modifier_chords_never_insert_a_character_or_trigger_a_binding() {
    let mut a = typing_app();
    a.handle_key(key('a'));
    for k in [
        mods('x', KeyModifiers::CONTROL),
        mods('x', KeyModifiers::ALT),
        mods('x', KeyModifiers::CONTROL | KeyModifiers::SHIFT),
        mods('b', KeyModifiers::ALT | KeyModifiers::SHIFT),
    ] {
        assert_eq!(a.handle_key(k), Action::None);
    }
    assert_eq!(buf_of(&a), "a", "chords left the line alone");
    // Shift alone is just a capital letter
    a.handle_key(mods('B', KeyModifiers::SHIFT));
    assert_eq!(buf_of(&a), "aB");
    // Windows reports AltGr (Polish ą ę ł) as Ctrl+Alt together; that is text, not a chord
    a.handle_key(mods('ą', KeyModifiers::CONTROL | KeyModifiers::ALT));
    assert_eq!(buf_of(&a), "aBą");
    // in normal mode a chord is no binding: Alt+q does not quit, Ctrl+j does not move
    let mut n = app();
    assert_eq!(n.handle_key(mods('q', KeyModifiers::ALT)), Action::None);
    assert_eq!(n.handle_key(ctrl('t')), Action::None);
    n.handle_key(ctrl('j'));
    assert_eq!(n.selected_issue().unwrap().as_str(), "asf-4");
    let mut o = open_overlay();
    assert_eq!(o.handle_key(ctrl('y')), Action::None);
    assert!(matches!(o.mode, Mode::Answer { .. }));
}

// ---------- asf-31: footer, help, navigation ----------

#[test]
fn footer_fits_100_columns_in_every_view_and_ends_with_help_and_quit() {
    let mut a = app();
    let mut seen = Vec::new();
    for c in '1'..='7' {
        a.handle_key(key(c));
        let hints = footer_hints(a.view, ModeKind::Normal);
        assert!(hints.len() <= 7, "{:?}: {} hints", a.view, hints.len());
        let f = footer_line(&a, 100);
        assert!(f.trim_end().ends_with("? help  q quit"), "{:?}: {f:?}", a.view);
        for h in &hints {
            assert!(
                f.contains(&format!("{} {}", h.key, h.label)),
                "{:?} lacks {}: {f:?}",
                a.view,
                h.key
            );
        }
        seen.push(f);
    }
    let (board, config) = (&seen[1], &seen[6]);
    assert_ne!(board, config, "Board and Config show different hints");
    assert!(board.contains("s start") && !board.contains("d unset"), "{board}");
    assert!(
        config.contains("d unset") && !config.contains("s start"),
        "{config}"
    );
}

#[test]
fn no_view_and_mode_combination_advertises_more_than_seven_hints() {
    for view in View::ALL {
        for kind in ModeKind::ALL {
            let n = footer_hints(view, kind).len();
            assert!(n <= 7, "{view:?}/{kind:?}: {n} hints");
        }
    }
}

#[test]
fn footer_keys_are_bold_and_their_actions_dim() {
    let a = app();
    let (_, key_mod) = cell_style(&a, "? help", "?");
    assert!(key_mod.contains(Modifier::BOLD));
    let (fg, _) = cell_style(&a, "? help", "help");
    assert_eq!(fg, Color::DarkGray);
}

#[test]
fn every_mode_has_a_footer_that_fits_100_columns() {
    let mut comment = inbox_app();
    comment.handle_key(code(KeyCode::Enter));
    let mut help = app();
    help.handle_key(key('?'));
    let mut confirm = app();
    for c in ['j', 'j', 'x'] {
        confirm.handle_key(key(c));
    }
    let f = footer_line(&comment, 100);
    assert!(f.contains("Esc close") && f.contains("j / k scroll"), "{f:?}");
    for (name, a) in [
        ("comment", comment),
        ("help", help),
        ("confirm", confirm),
        ("input", typing_app()),
    ] {
        let f = footer_line(&a, 100);
        assert!(!f.trim().is_empty(), "{name}: {f:?}");
        assert!(!f.contains("q quit"), "{name}: q is not quit here: {f:?}");
    }
    let f = footer_line(&typing_app(), 100);
    assert!(
        f.contains("reply to asf-2") && f.contains("Ctrl+W del word"),
        "{f:?}"
    );
}

#[test]
fn question_mark_opens_the_help_view_and_nothing_else_is_bound_meanwhile() {
    let mut a = app();
    a.handle_key(key('3'));
    assert_eq!(a.handle_key(key('?')), Action::None);
    assert!(matches!(a.mode, Mode::Help { .. }));
    for k in [
        key('s'),
        key('t'),
        key('R'),
        key('m'),
        key('x'),
        key('f'),
        key('r'),
        key('a'),
        key('d'),
        key('1'),
        key('6'),
        key('M'),
        code(KeyCode::Enter),
        code(KeyCode::Tab),
        code(KeyCode::BackTab),
    ] {
        assert_eq!(a.handle_key(k), Action::None, "{k:?}");
        assert!(
            matches!(a.mode, Mode::Help { .. }),
            "{k:?} must not leave the help view"
        );
        assert_eq!(a.view, View::Agents, "{k:?} must not change the view");
    }
    for closer in [code(KeyCode::Esc), key('?'), key('q')] {
        let mut b = app();
        b.handle_key(key('?'));
        assert_eq!(
            b.handle_key(closer),
            Action::None,
            "{closer:?} closes; q does not quit here"
        );
        assert!(matches!(b.mode, Mode::Normal), "{closer:?}");
    }
}

#[test]
fn help_lists_every_binding_in_plain_words_grouped_and_scrolls() {
    let mut a = app();
    a.handle_key(key('?'));
    let t = render_to_text(&a, 100, 120);
    let mut last = 0;
    for g in [
        "Global",
        "Navigation",
        "Now",
        "Board",
        "Agents",
        "Inbox",
        "Queue",
        "Config",
        "Answer overlay",
    ] {
        let at = t
            .find(&format!("\n {g}\n"))
            .unwrap_or_else(|| panic!("no group heading {g}:\n{t}"));
        assert!(at >= last, "groups are in order, {g}");
        last = at;
    }
    for b in BINDINGS {
        assert!(
            b.help.chars().count() <= 62,
            "keep help lines short: {:?}",
            b.help
        );
        assert!(t.contains(b.key), "missing key {:?}:\n{t}", b.key);
        assert!(t.contains(b.help), "missing help {:?}:\n{t}", b.help);
    }
    assert!(t.contains("mark this issue's messages as read"));
    for needle in [
        "answer a worker that waits",
        "remove an optional key",
        "send that number key",
    ] {
        assert!(t.contains(needle), "{needle}:\n{t}");
    }
    // on a short screen the tail is reached by scrolling
    let short = render_to_text(&a, 100, 20);
    assert!(!short.contains("send the typed reply"), "{short}");
    for _ in 0..200 {
        a.handle_key(key('j'));
    }
    let end = render_to_text(&a, 100, 20);
    assert!(
        end.contains("send the typed reply"),
        "scrolled to the end:\n{end}"
    );
    a.handle_key(key('g'));
    assert!(render_to_text(&a, 100, 20).contains("Global"));
}

#[test]
fn tab_and_shift_tab_cycle_the_views_and_wrap() {
    let mut a = App::new(snapshot());
    for expected in [
        View::Board,
        View::Agents,
        View::Inbox,
        View::Queue,
        View::Log,
        View::Config,
        View::Now,
    ] {
        assert_eq!(a.handle_key(code(KeyCode::Tab)), Action::None);
        assert_eq!(a.view, expected);
    }
    let back = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(a.handle_key(back), Action::None);
    assert_eq!(a.view, View::Config, "Shift+Tab from the first view wraps");
    a.handle_key(back);
    assert_eq!(a.view, View::Log);
    // some terminals send Tab + SHIFT instead of BackTab
    a.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT));
    assert_eq!(a.view, View::Queue);
}

#[test]
fn g_and_capital_g_jump_to_the_first_and_last_row() {
    let mut a = app();
    a.handle_key(key('G'));
    assert_eq!(a.selected_issue().unwrap().as_str(), "asf-1");
    a.handle_key(key('g'));
    assert_eq!(a.selected_issue().unwrap().as_str(), "asf-4");
    let mut c = app();
    c.handle_key(key('7'));
    c.handle_key(key('G'));
    assert_eq!(c.selected(View::Config), c.rows(View::Config) - 1);
    c.handle_key(key('g'));
    assert_eq!(c.selected(View::Config), 0);
    let mut empty = board_app(Snapshot::default());
    empty.handle_key(key('G')); // no rows: no panic
    empty.handle_key(key('g'));
}

#[test]
fn escape_in_normal_mode_closes_the_board_detail_panel() {
    let mut a = app();
    assert!(a.detail);
    assert!(render_to_text(&a, 100, 30).contains("┌ detail "));
    assert_eq!(a.handle_key(code(KeyCode::Esc)), Action::None);
    assert!(!a.detail);
    assert!(!render_to_text(&a, 100, 30).contains("┌ detail "));
    a.handle_key(code(KeyCode::Enter));
    assert!(a.detail, "Enter still toggles it back");
}

// ---------- asf-31 review round 1 ----------

#[test]
fn a_status_set_again_by_a_key_gets_a_fresh_four_seconds() {
    let mut a = app();
    a.advance(ms(0));
    a.handle_key(key('j')); // asf-3, ready
    a.busy = Some("tick".into());
    a.handle_key(key('t'));
    assert_eq!(a.status.as_deref(), Some("busy: tick"));
    a.advance(ms(0)); // stamped at t = 0
    a.advance(ms(3_000));
    // the same text again, set by a key before the next advance()
    a.handle_key(key('t'));
    assert_eq!(a.status.as_deref(), Some("busy: tick"));
    a.advance(ms(3_000)); // stamps the new message at t = 3 s
    a.advance(ms(6_000));
    assert_eq!(
        a.status.as_deref(),
        Some("busy: tick"),
        "the first message's 4 s must not cut the second one short"
    );
    a.advance(ms(3_000) + STATUS_TTL);
    assert!(a.status.is_none(), "gone 4 s after the second press");
}

#[test]
fn a_key_is_inert_in_every_view_the_table_does_not_list_it_for() {
    for view in View::ALL {
        for c in ['s', 'm', 'f', 'r', 'x', 'a', 'd', 'M', 'v'] {
            let listed = BINDINGS
                .iter()
                .any(|b| b.key == c.to_string() && b.applies(view, ModeKind::Normal));
            if listed {
                continue;
            }
            let mut a = app();
            a.view = view;
            assert_eq!(a.handle_key(key(c)), Action::None, "{c} in {view:?}");
            assert!(
                matches!(a.mode, Mode::Normal),
                "{c} in {view:?} opened {:?}",
                a.mode
            );
            assert!(a.status.is_none(), "{c} in {view:?}: {:?}", a.status);
        }
    }
    // the cases the review named, spelled out
    let mut a = inbox_app();
    for c in ['s', 'f', 'x'] {
        assert_eq!(a.handle_key(key(c)), Action::None, "{c} in the Inbox");
    }
    assert_eq!(a.handle_key(key('m')), Action::MarkRead(id("asf-2")));
    let mut q = app();
    q.view = View::Queue;
    assert_eq!(q.handle_key(key('s')), Action::None);
    assert_eq!(q.handle_key(key('m')), Action::None);
    let mut g = app();
    g.view = View::Agents;
    assert_eq!(g.handle_key(key('s')), Action::None);
    assert_eq!(g.handle_key(key('m')), Action::Enqueue(id("asf-2")));
}

#[test]
fn the_table_lists_page_and_jump_keys_where_they_work() {
    let listed = |mode: ModeKind, word: &str| {
        BINDINGS
            .iter()
            .any(|b| b.modes.contains(&mode) && b.key.contains(word))
    };
    for word in ["Home", "End"] {
        assert!(listed(ModeKind::Normal, word), "{word} in normal mode");
    }
    for mode in [ModeKind::Help, ModeKind::Comment] {
        for word in ["PageUp", "PageDown", "Home", "End"] {
            assert!(listed(mode, word), "{word} in {mode:?}");
        }
    }
    let mut a = app();
    a.handle_key(key('?'));
    let t = render_to_text(&a, 100, 150);
    for word in ["PageUp", "PageDown", "Home", "End"] {
        assert!(t.contains(word), "help view lacks {word}");
    }
}

#[test]
fn page_and_jump_keys_do_what_the_help_says() {
    let mut s = snapshot();
    s.inbox[0].body = (1..=80)
        .map(|n| format!("line {n:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut a = board_app(s);
    a.handle_key(key('4'));
    a.handle_key(code(KeyCode::Enter));
    render_to_text(&a, 100, 30); // publishes the scroll limit
    a.handle_key(code(KeyCode::PageDown));
    assert!(render_to_text(&a, 100, 30).contains("line 11"));
    a.handle_key(code(KeyCode::End));
    assert!(render_to_text(&a, 100, 30).contains("line 80"));
    a.handle_key(code(KeyCode::PageUp));
    assert!(!render_to_text(&a, 100, 30).contains("line 80"));
    a.handle_key(code(KeyCode::Home));
    assert!(render_to_text(&a, 100, 30).contains("line 10"));
    // Home / End move the selection in normal mode
    let mut b = app();
    b.handle_key(code(KeyCode::End));
    assert_eq!(b.selected_issue().unwrap().as_str(), "asf-1");
    b.handle_key(code(KeyCode::Home));
    assert_eq!(b.selected_issue().unwrap().as_str(), "asf-4");
}

// ---------- asf-32: the Now view ----------

use super::now;
use super::post_answer;
use super::theme::{no_color_requested, strip_colors};
use super::view::{MIN_HEIGHT, MIN_WIDTH};

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

#[test]
fn every_needs_you_source_has_its_row_with_label_first_line_age_and_key() {
    let a = App::new(busy_desk());
    let t = render_to_text(&a, 120, 30);
    assert!(t.contains("NEEDS YOU (8)"), "{t}");
    let expected = [
        (
            "asf-1",
            "question",
            "Allow Bash(rm -rf target)?",
            "10 min",
            "a answer",
        ),
        (
            "asf-8",
            "question",
            "waiting for your answer",
            "5 min",
            "a answer",
        ),
        (
            "asf-2",
            "question",
            "Keep the old --scope flag as an alias?",
            "4 min",
            "a answer",
        ),
        ("asf-3", "failed", "gate `test` failed", "40 min", "v verify"),
        ("asf-4", "stalled", "no output for 10 minutes", "1 h", "v verify"),
        (
            "asf-5",
            "over_budget",
            "no exit reason recorded",
            "2 h",
            "v verify",
        ),
        (
            "asf-6",
            "merge failed",
            "gate `test` after rebase (2 tests)",
            "12 min",
            "r retry",
        ),
        ("asf-7", "merge blocked", "no error recorded", "<1 min", "r retry"),
    ];
    for (issue, label, text, age, key) in expected {
        let row = row_of(&t, issue);
        for part in [label, text, age, key] {
            assert!(row.contains(part), "{issue}: {part:?} missing in {row:?}");
        }
        assert!(
            row.trim_end().ends_with(key),
            "{issue}: the key closes the row: {row:?}"
        );
        assert!(
            !row.contains("second line") && !row.contains("more output"),
            "{issue}: only the first line: {row:?}"
        );
    }
}

#[test]
fn needs_you_is_ordered_oldest_first_and_the_screen_agrees() {
    let s = busy_desk();
    let order: Vec<String> = now::needs(&s).iter().map(|n| n.issue.to_string()).collect();
    assert_eq!(
        order,
        ["asf-5", "asf-4", "asf-3", "asf-6", "asf-1", "asf-8", "asf-2", "asf-7"]
    );
    let t = render_to_text(&App::new(s), 120, 30);
    let at = |issue: &str| {
        t.find(&format!("■ {issue} "))
            .unwrap_or_else(|| panic!("{issue}:\n{t}"))
    };
    let positions: Vec<usize> = order.iter().map(|i| at(i)).collect();
    assert!(positions.windows(2).all(|w| w[0] < w[1]), "{t}");
    assert!(
        line_of(&t, 0).contains("8 waiting"),
        "the header counts the same rows:\n{t}"
    );
}

#[test]
fn a_blocked_stalled_or_over_budget_run_is_listed_once_in_needs_you_and_not_in_running() {
    let s = busy_desk();
    assert!(now::running(&s).is_empty());
    let t = render_to_text(&App::new(s), 120, 30);
    assert!(t.contains("RUNNING (0)"), "{t}");
    assert_eq!(t.matches("asf-5 ").count(), 1, "{t}");
}

#[test]
fn empty_sections_say_what_to_do_next_in_the_dim_style() {
    let a = App::new(desk(vec![]));
    let t = render_to_text(&a, 120, 30);
    assert!(t.contains("NEEDS YOU (0)") && t.contains("RUNNING (0)"), "{t}");
    assert!(t.contains("Nothing needs you."), "{t}");
    assert!(
        t.contains("No worker is running. Press Tab for the Board to start one."),
        "{t}"
    );
    for text in ["Nothing needs you.", "No worker is running."] {
        let (_, m) = cell_style(&a, text, text);
        assert!(m.contains(Modifier::DIM), "{text} is dim");
    }
    assert!(t.contains("UP NEXT (0)") && t.contains("DONE TODAY (0)"), "{t}");
}

#[test]
fn running_rows_show_state_title_cost_over_budget_and_elapsed_reviewers_the_model_and_round() {
    let a = App::new(working_desk());
    let t = render_to_text(&a, 120, 30);
    assert!(t.contains("RUNNING (2)") && t.contains("NEEDS YOU (0)"), "{t}");
    let w = row_of(&t, "asf-24");
    for part in [
        "● asf-24",
        "working",
        "Strip worker env vars from cli_export tests",
        "$0.50 / $10",
        "6 min",
    ] {
        assert!(w.contains(part), "{part:?} missing in {w:?}");
    }
    let r = row_of(&t, "asf-31");
    for part in ["◆ asf-31", "review", "opus · round 1", "$0.21 / $10", "1 min"] {
        assert!(r.contains(part), "{part:?} missing in {r:?}");
    }
    assert!(
        !r.contains("working") && !r.contains("Review asf-24"),
        "a reviewer shows `review` and the model, not the run state or the title: {r:?}"
    );
    // a budget with cents keeps them, a run without one shows only the cost
    let mut s = working_desk();
    s.runs[0].budget_usd = Some(2.5);
    s.runs[1].budget_usd = None;
    let t = render_to_text(&App::new(s), 120, 30);
    assert!(row_of(&t, "asf-24").contains("$0.50 / $2.50"), "{t}");
    let r = row_of(&t, "asf-31");
    assert!(r.contains("$0.21") && !r.contains(" / "), "{r:?}");
}

#[test]
fn up_next_counts_ready_and_backlog_names_the_first_ids_and_the_shared_dependency() {
    let mut issues = vec![issue("asf-24", "Running", Status::InProgress)];
    for n in 40..46 {
        issues.push(issue(&format!("asf-{n}"), "Later", Status::Backlog));
    }
    let mut s = desk(issues);
    s.deps = (40..46).map(|n| dep(&format!("asf-{n}"), "asf-24")).collect();
    let t = render_to_text(&App::new(s.clone()), 120, 30);
    assert!(
        t.contains("UP NEXT (6)   asf-40, asf-41, asf-42 … waiting for asf-24"),
        "{t}"
    );
    // a ready issue that waits for nothing breaks the sharing; epics do not count
    s.issues.push(issue("asf-50", "Go", Status::Ready));
    let mut epic = issue("asf-51", "Epic", Status::Backlog);
    epic.kind = IssueType::Epic;
    s.issues.push(epic);
    let next = now::up_next(&s);
    assert_eq!(next.count, 7);
    assert_eq!(
        next.ids,
        [id("asf-50"), id("asf-40"), id("asf-41")],
        "ready first"
    );
    assert_eq!(next.waits_for, None);
    // two dependencies shared by everyone are not "one dependency"
    let mut two = desk(vec![
        issue("asf-1", "a", Status::InProgress),
        issue("asf-2", "b", Status::InProgress),
        issue("asf-3", "c", Status::Backlog),
    ]);
    two.deps = vec![dep("asf-3", "asf-1"), dep("asf-3", "asf-2")];
    assert_eq!(now::up_next(&two).waits_for, None);
    // a finished dependency waits for nothing
    two.issues[0].status = Status::Done;
    assert_eq!(now::up_next(&two).waits_for, Some(id("asf-2")));
}

#[test]
fn done_today_lists_ids_closed_on_the_local_date_newest_first_and_truncates_with_an_ellipsis() {
    let closed = |n: u32, at: &str| {
        let mut i = issue(&format!("asf-{n}"), "done", Status::Done);
        i.closed_at = Some(at.into());
        i
    };
    let mut s = desk(vec![
        closed(20, "2026-11-06T09:00:00Z"),
        closed(22, "2026-11-06T11:00:00Z"),
        closed(21, "2026-11-06T10:00:00Z"),
        closed(19, "2026-11-05T09:00:00Z"),
        closed(18, "2026-11-05T23:30:00Z"),
    ]);
    let mut review = closed(23, "2026-11-06T11:30:00Z");
    review.kind = IssueType::Review;
    s.issues.push(review);
    let ids: Vec<String> = now::done_today(&s).iter().map(ToString::to_string).collect();
    assert_eq!(
        ids,
        ["asf-22", "asf-21", "asf-20"],
        "UTC date; reviews are internal"
    );
    let t = render_to_text(&App::new(s.clone()), 120, 30);
    assert!(
        t.contains("DONE TODAY (3)   ✓ asf-22  ✓ asf-21  ✓ asf-20") && !t.contains("asf-20 …"),
        "{t}"
    );
    // one hour east of UTC, 23:30Z on the 5th is already the 6th
    s.utc_offset_secs = 3600;
    let ids: Vec<String> = now::done_today(&s).iter().map(ToString::to_string).collect();
    assert_eq!(ids, ["asf-22", "asf-21", "asf-20", "asf-18"]);
    // many ids do not fit one line: cut with … and the count stays whole
    for n in 100..140 {
        s.issues
            .push(closed(n, &format!("2026-11-06T11:{:02}:00Z", n - 100)));
    }
    let t = render_to_text(&App::new(s), 80, 24);
    let line = t.lines().find(|l| l.contains("DONE TODAY")).unwrap();
    assert!(
        line.contains("DONE TODAY (44)") && line.trim_end().ends_with('…'),
        "{line:?}"
    );
    assert!(line.chars().count() <= 80, "{line:?}");
}

#[test]
fn ages_are_short() {
    assert_eq!(now::age(NOW, "2026-11-06T11:59:30Z"), "<1 min");
    assert_eq!(now::age(NOW, "2026-11-06T11:59:00Z"), "1 min");
    assert_eq!(now::age(NOW, "2026-11-06T11:00:01Z"), "59 min");
    assert_eq!(now::age(NOW, "2026-11-06T09:00:00Z"), "3 h");
    assert_eq!(now::age(NOW, "2026-11-04T12:00:00Z"), "2 d");
    assert_eq!(
        now::age(NOW, "2026-11-06T12:00:30Z"),
        "<1 min",
        "the future is not negative"
    );
    assert_eq!(now::age("", NOW), "-");
    assert_eq!(now::age(NOW, "yesterday"), "-");
}

#[test]
fn the_blocker_screen_comes_from_the_fenced_block_after_screen() {
    let mut s = busy_desk();
    let first = |s: &Snapshot, issue: &str| {
        now::needs(s)
            .into_iter()
            .find(|n| n.issue == id(issue))
            .unwrap()
            .text
    };
    assert_eq!(first(&s, "asf-1"), "Allow Bash(rm -rf target)?");
    // the newest BLOCKER wins, and one without a screen falls back
    let mut newer = comment(
        "asf-1",
        CommentType::Blocker,
        "Over budget.\nNo screen in this one.",
    );
    newer.id = 9;
    s.blockers.push(newer);
    assert_eq!(first(&s, "asf-1"), "waiting for your answer");
    // the fixture of the older tests has no fence either
    let mut plain = snapshot();
    plain.runs[0].state = RunState::Blocked;
    plain.blockers = plain.inbox.clone();
    assert_eq!(first(&plain, "asf-2"), "waiting for your answer");
}

#[test]
fn symbols_differ_per_state_and_carry_their_colours() {
    let a = App::new(full_desk());
    // Now: ■ red bold, ● green, ◆ yellow
    let (fg, m) = cell_style(&a, "asf-5 ", "■");
    assert_eq!(fg, Color::Red);
    assert!(m.contains(Modifier::BOLD));
    assert_eq!(cell_style(&a, "asf-24 ", "●").0, Color::Green);
    assert_eq!(cell_style(&a, "asf-31 ", "◆").0, Color::Yellow);
    // Board: the same symbols, plus ✓ dim and ○ in the default colour (a roomier snapshot, so every
    // card fits on screen)
    let mut s = working_desk();
    s.issues.push(issue("asf-5", "Over budget", Status::InProgress));
    s.runs
        .push(run_at("asf-5", RunState::OverBudget, "2026-11-06T10:00:00Z"));
    s.issues.push(issue("asf-60", "Validating", Status::Validating));
    s.issues.push(issue("asf-61", "Waits", Status::Ready));
    s.issues.push(issue("asf-62", "Shipped", Status::Done));
    let mut b = board_app(s);
    assert_eq!(cell_style(&b, "asf-24", "●").0, Color::Green);
    assert_eq!(cell_style(&b, "asf-31", "◆").0, Color::Yellow);
    assert_eq!(cell_style(&b, "asf-60", "◆").0, Color::Yellow);
    assert_eq!(cell_style(&b, "asf-5", "■").0, Color::Red);
    let (_, m) = cell_style(&b, "asf-62", "✓");
    assert!(m.contains(Modifier::DIM), "done is dim");
    assert_eq!(cell_style(&b, "asf-61", "○").0, Color::Reset);
    // Agents: a working run, a reviewer and a run that needs you each have their own symbol
    b.handle_key(key('3'));
    let t = render_to_text(&b, 120, 30);
    for (issue, glyph) in [("asf-24", '●'), ("asf-31", '◆'), ("asf-5", '■')] {
        let row = t
            .lines()
            .find(|l| l.contains('│') && l.contains(&format!("{issue} ")))
            .unwrap_or_else(|| panic!("no agents row for {issue}:\n{t}"));
        assert!(row.contains(glyph), "{issue}: {row:?}");
    }
}

#[test]
fn no_color_removes_every_colour_but_keeps_bold_dim_and_reverse() {
    let mut a = App::new(full_desk());
    let coloured = render_to_buffer(&a, 120, 30);
    let has_colour = |b: &ratatui::buffer::Buffer| {
        b.area
            .positions()
            .any(|p| b[p].fg != Color::Reset || b[p].bg != Color::Reset)
    };
    assert!(has_colour(&coloured), "the control render is in colour");
    a.color = false;
    let plain = render_to_buffer(&a, 120, 30);
    assert!(!has_colour(&plain), "{}", render_to_text(&a, 120, 30));
    let modifier_at = |needle: &str| cell_style(&a, needle, needle).1;
    assert!(modifier_at("NEEDS YOU").contains(Modifier::BOLD));
    assert!(
        modifier_at("■").contains(Modifier::BOLD),
        "the red square stays bold"
    );
    assert!(
        modifier_at("▸").contains(Modifier::REVERSED),
        "the selected row stays reversed"
    );
    // the same on every view and with a modal open
    for c in ['2', '3', '4', '5', '6', '7', '?'] {
        a.handle_key(key(c));
        assert!(!has_colour(&render_to_buffer(&a, 120, 30)), "view {:?}", a.view);
    }
    // the entry points read NO_COLOR with the no-color.org rule: set and non-empty
    assert!(!no_color_requested(None));
    assert!(!no_color_requested(Some(std::ffi::OsStr::new(""))));
    assert!(no_color_requested(Some(std::ffi::OsStr::new("1"))));
    let mut buf = coloured;
    strip_colors(&mut buf);
    assert!(!has_colour(&buf));
}

#[test]
fn below_80_columns_or_20_rows_one_centred_line_replaces_every_view() {
    for (w, h) in [(79u16, 24u16), (80, 19), (100, 5), (60, 10)] {
        for view in View::ALL {
            let mut a = App::new(full_desk());
            a.view = view;
            let t = render_to_text(&a, w, h);
            let lines: Vec<(usize, &str)> = t
                .lines()
                .enumerate()
                .filter(|(_, l)| !l.trim().is_empty())
                .collect();
            let want = format!("Terminal too small (need {MIN_WIDTH}x{MIN_HEIGHT}, have {w}x{h}).");
            assert_eq!(lines.len(), 1, "{view:?} at {w}x{h}:\n{t}");
            assert_eq!(lines[0].1.trim(), want);
            let lead = lines[0].1.len() - lines[0].1.trim_start().len();
            assert!(
                lead.abs_diff((w as usize - want.len()) / 2) <= 1,
                "centred across: {lead}"
            );
            assert!(
                lines[0].0.abs_diff(h as usize / 2) <= 1,
                "centred down: row {}",
                lines[0].0
            );
        }
    }
    // a modal does not survive it either
    let mut a = App::new(full_desk());
    a.handle_key(key('?'));
    let lines = render_to_text(&a, 79, 24)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    assert_eq!(lines, 1);
    // exactly 80x20 is large enough
    let t = render_to_text(&App::new(full_desk()), 80, 20);
    assert!(t.contains("NEEDS YOU") && !t.contains("too small"), "{t}");
}

#[test]
fn j_and_k_walk_the_needs_you_rows_and_then_the_running_rows() {
    let mut a = App::new(full_desk());
    let needs = now::needs(&a.snap).len();
    assert_eq!(needs, 8);
    assert_eq!(a.rows(View::Now), 10);
    assert_eq!(a.selected(View::Now), 0);
    for _ in 0..needs {
        a.handle_key(key('j'));
    }
    assert!(
        matches!(a.selected_now_row(), Some(now::NowRow::Running(r)) if r.issue == id("asf-24")),
        "{:?}",
        a.selected_now_row()
    );
    a.handle_key(key('G'));
    assert!(matches!(a.selected_now_row(), Some(now::NowRow::Running(r)) if r.issue == id("asf-31")));
    a.handle_key(key('j'));
    assert_eq!(a.selected(View::Now), 9, "clamped at the last row");
    a.handle_key(key('g'));
    assert!(matches!(a.selected_now_row(), Some(now::NowRow::Need(n)) if n.issue == id("asf-5")));
    // the selected row carries the marker and no other row does
    let t = render_to_text(&a, 120, 30);
    assert_eq!(t.matches('▸').count(), 1, "{t}");
    assert!(row_of(&t, "asf-5").contains("▸ ■ asf-5"), "{t}");
    // a refresh that drops rows keeps the selection inside
    a.handle_key(key('G'));
    a.replace(busy_desk());
    assert_eq!(a.selected(View::Now), 7);
    a.replace(desk(vec![]));
    assert_eq!(a.selected(View::Now), 0);
    assert_eq!(a.selected_now_row(), None);
}

#[test]
fn enter_switches_to_the_board_with_that_issue_selected() {
    let mut a = App::new(full_desk());
    for _ in 0..3 {
        a.handle_key(key('j'));
    }
    assert!(matches!(a.selected_now_row(), Some(now::NowRow::Need(n)) if n.issue == id("asf-6")));
    assert_eq!(a.handle_key(code(KeyCode::Enter)), Action::None);
    assert_eq!(a.view, View::Board);
    assert_eq!(a.selected_issue(), Some(&id("asf-6")));
    // a running row too
    let mut b = App::new(full_desk());
    b.handle_key(key('G'));
    b.handle_key(code(KeyCode::Enter));
    assert_eq!(b.view, View::Board);
    assert_eq!(b.selected_issue(), Some(&id("asf-31")));
    // nothing selected: Enter does nothing
    let mut e = App::new(desk(vec![]));
    assert_eq!(e.handle_key(code(KeyCode::Enter)), Action::None);
    assert_eq!(e.view, View::Now);
}

#[test]
fn f_focuses_the_pane_of_the_selected_row() {
    let mut a = App::new(full_desk());
    assert_eq!(a.handle_key(key('f')), Action::Focus(id("asf-5")));
    a.handle_key(key('G'));
    assert_eq!(a.handle_key(key('f')), Action::Focus(id("asf-31")));
    assert_eq!(App::new(desk(vec![])).handle_key(key('f')), Action::None);
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

#[test]
fn a_answers_a_blocked_worker_with_the_overlay_and_a_question_with_a_typed_answer() {
    let mut a = App::new(busy_desk());
    select_need(&mut a, "asf-1");
    assert_eq!(a.handle_key(key('a')), Action::Peek(id("asf-1")));
    assert!(matches!(&a.mode, Mode::Answer { id: i, .. } if *i == id("asf-1")));
    a.handle_key(code(KeyCode::Esc));

    select_need(&mut a, "asf-2");
    assert_eq!(a.handle_key(key('a')), Action::None);
    match &a.mode {
        Mode::Input { prompt, target, .. } => {
            assert_eq!(prompt, "answer asf-2 (#7)");
            assert_eq!(*target, super::app::InputTarget::Answer(id("asf-2"), 7));
        }
        other => panic!("expected the answer input, got {other:?}"),
    }
    for c in "keep it".chars() {
        a.handle_key(key(c));
    }
    assert_eq!(
        a.handle_key(code(KeyCode::Enter)),
        Action::Answer(id("asf-2"), 7, "keep it".into())
    );
    assert!(matches!(a.mode, Mode::Normal));
    // an empty answer sends nothing; Esc cancels
    select_need(&mut a, "asf-2");
    a.handle_key(key('a'));
    assert_eq!(a.handle_key(code(KeyCode::Enter)), Action::None);
    select_need(&mut a, "asf-2");
    a.handle_key(key('a'));
    a.handle_key(key('x'));
    assert_eq!(a.handle_key(code(KeyCode::Esc)), Action::None);
    assert!(matches!(a.mode, Mode::Normal));

    // a row that is not a question says which key it takes
    select_need(&mut a, "asf-3");
    assert_eq!(a.handle_key(key('a')), Action::None);
    assert!(matches!(a.mode, Mode::Normal));
    assert!(
        a.status.as_deref().unwrap_or("").contains("v verify"),
        "{:?}",
        a.status
    );
}

#[test]
fn v_reruns_verification_of_a_failed_stalled_or_over_budget_run_in_the_background() {
    for issue in ["asf-3", "asf-4", "asf-5"] {
        let mut a = App::new(busy_desk());
        select_need(&mut a, issue);
        assert_eq!(a.handle_key(key('v')), Action::Verify(id(issue)), "{issue}");
    }
    let mut a = App::new(busy_desk());
    for issue in ["asf-1", "asf-2", "asf-6"] {
        select_need(&mut a, issue);
        assert_eq!(a.handle_key(key('v')), Action::None, "{issue}");
        let status = a.status.clone().unwrap_or_default();
        assert!(status.starts_with(&format!("{issue}: v verifies")), "{status}");
        assert!(matches!(a.mode, Mode::Normal));
    }
    // one background action at a time, like tick and start
    select_need(&mut a, "asf-3");
    a.busy = Some("tick".into());
    assert_eq!(a.handle_key(key('v')), Action::None);
    assert_eq!(a.status.as_deref(), Some("busy: tick"));
}

#[test]
fn r_requeues_a_failed_or_blocked_merge_item_in_the_background_and_never_opens_a_reply() {
    for issue in ["asf-6", "asf-7"] {
        let mut a = App::new(busy_desk());
        select_need(&mut a, issue);
        assert_eq!(a.handle_key(key('r')), Action::Retry(id(issue)), "{issue}");
        assert!(matches!(a.mode, Mode::Normal));
    }
    let mut a = App::new(busy_desk());
    for issue in ["asf-1", "asf-3", "asf-2"] {
        select_need(&mut a, issue);
        assert_eq!(a.handle_key(key('r')), Action::None, "{issue}");
        assert!(
            matches!(a.mode, Mode::Normal),
            "{issue}: r is retry here, not reply"
        );
        assert!(
            a.status.as_deref().unwrap_or("").contains("r retries"),
            "{:?}",
            a.status
        );
    }
    select_need(&mut a, "asf-6");
    a.busy = Some("verifying asf-3".into());
    assert_eq!(a.handle_key(key('r')), Action::None);
    assert_eq!(a.status.as_deref(), Some("busy: verifying asf-3"));
}

#[test]
fn the_background_children_of_v_and_r_run_the_cli_and_report_their_envelopes() {
    let root = std::path::Path::new("D:/repo");
    let args = |k: &BgKind| -> Vec<String> {
        k.args(root)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    };
    let verify = BgKind::Verify(id("asf-3"));
    let retry = BgKind::Retry(id("asf-6"));
    assert_eq!(
        args(&verify),
        ["--repo", "D:/repo", "run", "verify", "asf-3", "--json"]
    );
    assert_eq!(
        args(&retry),
        ["--repo", "D:/repo", "merge", "retry", "asf-6", "--json"]
    );
    assert_eq!(
        args(&BgKind::Start(id("asf-3")))[2..],
        ["run", "start", "asf-3", "--json"]
    );
    assert_eq!(verify.label(), "verifying asf-3");
    assert_eq!(retry.label(), "retrying asf-6");
    assert_eq!(
        bg_outcome(
            &verify,
            r#"{"ok":true,"result":{"run_id":"r","issue_id":"asf-3","from":"failed","to":"succeeded","detail":"gates ok\nmore"}}"#,
            "",
            Some(0)
        ),
        "✓ verify asf-3: failed -> succeeded: gates ok"
    );
    // the command itself succeeds when the verification it ran does not
    assert_eq!(
        bg_outcome(
            &verify,
            r#"{"ok":true,"result":{"run_id":"r","issue_id":"asf-3","from":"failed","to":"failed","detail":"gate test failed"}}"#,
            "",
            Some(0)
        ),
        "✗ verify asf-3: failed -> failed: gate test failed"
    );
    assert_eq!(
        bg_outcome(
            &retry,
            r#"{"ok":true,"result":{"id":4,"issue_id":"asf-6","state":"queued"}}"#,
            "",
            Some(0)
        ),
        "✓ asf-6 re-queued (#4)"
    );
    assert_eq!(
        bg_outcome(
            &retry,
            r##"{"ok":false,"error":{"code":"conflict","message":"#4 asf-6 is queued, only failed/blocked items can be retried"}}"##,
            "",
            Some(3)
        ),
        "✗ #4 asf-6 is queued, only failed/blocked items can be retried"
    );
    assert_eq!(
        bg_outcome(&verify, "", "boom\n", Some(1)),
        "✗ verify asf-3 exited with 1: boom"
    );
}

#[test]
fn an_answer_is_posted_as_the_humans_reply_and_stops_the_question_waiting() {
    let store = asf_store::SqliteStore::open_in_memory().unwrap();
    store.init_repo("D:/repo", "asf", "main").unwrap();
    let issue = store
        .create_issue(&NewIssue {
            title: "needs an answer".into(),
            ..NewIssue::default()
        })
        .unwrap();
    let q = store
        .add_comment(&NewComment {
            issue_id: issue.id.clone(),
            author: "agent:asf-1".into(),
            kind: CommentType::Question,
            to_lane: LANE_HUMAN.into(),
            body: "Keep the alias?".into(),
            reply_to: None,
        })
        .unwrap();
    assert_eq!(
        super::app::unanswered_questions(&store.comments(&issue.id).unwrap()),
        [q.id]
    );
    let status = post_answer(&store, &issue.id, q.id, "yes, keep it").unwrap();
    assert_eq!(status, format!("answered #{} on {}", q.id, issue.id));
    let comments = store.comments(&issue.id).unwrap();
    let answer = comments.last().unwrap();
    assert_eq!(
        (
            answer.kind,
            answer.author.as_str(),
            answer.reply_to,
            answer.body.as_str()
        ),
        (CommentType::Answer, "human", Some(q.id), "yes, keep it")
    );
    assert_eq!(answer.to_lane, issue.id.to_string());
    assert!(
        super::app::unanswered_questions(&comments).is_empty(),
        "the same rule `asf issue finish` applies"
    );
}

#[test]
fn the_banner_is_for_the_other_views_the_footer_and_help_know_the_new_view() {
    let mut a = App::new(busy_desk());
    let t = render_to_text(&a, 120, 30);
    assert!(
        !t.contains("is waiting for your answer") && !t.contains(" asks:"),
        "{t}"
    );
    assert!(
        line_of(&t, 1).contains("[1] Now") && line_of(&t, 1).contains("[7] Config"),
        "{t}"
    );
    assert!(
        line_of(&t, 2).contains("NEEDS YOU"),
        "content starts under the tab bar:\n{t}"
    );
    a.handle_key(key('2'));
    assert!(line_of(&render_to_text(&a, 120, 30), 2).starts_with("■ asf-1 is waiting"));
    // the tab bar marks the active view with reverse video
    a.handle_key(key('1'));
    let (_, m) = cell_style(&a, "[1] Now", "[1] Now");
    assert!(m.contains(Modifier::REVERSED));
    // the footer: the hints of the mockup, built from the table
    let f = footer_line(&a, 120);
    assert_eq!(
        f.trim(),
        "j / k move  a answer  f focus pane  Enter open issue  Tab next view  ? help  q quit"
    );
    // v and r are in the help view, in a group of their own
    a.handle_key(key('?'));
    let help = render_to_text(&a, 100, 150);
    for needle in [
        "\n Now\n",
        "rerun verification of a failed",
        "re-queue a failed or blocked merge item",
        "jump to a view: Now, Board, Agents, Inbox, Queue, Log, Config",
    ] {
        assert!(help.contains(needle), "{needle:?}:\n{help}");
    }
    assert!(BINDINGS.iter().any(|b| b.key == "1 - 7"));
}
