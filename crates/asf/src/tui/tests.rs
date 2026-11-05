use std::collections::BTreeMap;

use asf_core::*;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::app::{board_columns, config_rows, Action, App, Mode, Snapshot, View};
use super::view::{render_to_buffer, render_to_text};
use super::{config_set_status, config_unset_status, keys_status};

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
    a.handle_key(key('6'));
    assert_eq!(a.view, View::Config);
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
    let a = App::new(s);
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
    assert_eq!(a.handle_key(key('6')), Action::None);
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
    a.handle_key(key('6'));
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
    let mut a = App::new(Snapshot::default());
    a.handle_key(key('6'));
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
    let a = App::new(s);
    let t = render_to_text(&a, 120, 30);
    assert!(line_of(&t, 0).contains("2 waiting"), "header:\n{t}");
    let (fg, m) = cell_style(&a, "asf ·", "2 waiting");
    assert_eq!(fg, Color::Cyan, "accent colour");
    assert!(m.contains(Modifier::BOLD));
}

#[test]
fn banner_names_a_blocked_run_directly_under_the_tab_bar() {
    let a = App::new(blocked_snapshot());
    let t = render_to_text(&a, 120, 30);
    assert!(line_of(&t, 1).contains("[1] Board"), "tab bar is line 1:\n{t}");
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
    let t = render_to_text(&App::new(s), 120, 30);
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
    let t = render_to_text(&App::new(s), 120, 30);
    assert_eq!(
        line_of(&t, 2).trim(),
        "■ asf-2 is waiting for your answer · a answer · f focus pane (+2 more)"
    );
    let mut s = snapshot();
    s.questions = vec![
        question(8, "asf-3", "newer question", "2026-11-02T10:09:00Z"),
        question(7, "asf-4", "older question", "2026-11-02T10:06:00Z"),
    ];
    let t = render_to_text(&App::new(s), 120, 30);
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
    let t = render_to_text(&App::new(s), 50, 20);
    let l = line_of(&t, 2);
    assert!(l.chars().count() <= 50, "{l:?}");
    assert!(
        l.starts_with("■ asf-4 asks: very long") && l.contains('…'),
        "{l:?}"
    );
    assert!(l.trim_end().ends_with("(+1 more)"), "{l:?}");
}

#[test]
fn banner_is_visible_in_every_view() {
    let mut a = App::new(blocked_snapshot());
    for c in '1'..='6' {
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
    let a = App::new(blocked_snapshot());
    let t = render_to_text(&a, 120, 30);
    assert!(t.contains("■ ? asf-2") && !t.contains('▶'), "{t}");
    let (fg, m) = cell_style(&a, "■ ? asf-2", "■ ? asf-2");
    assert_eq!(fg, Color::Red);
    assert!(m.contains(Modifier::BOLD));
    let t = render_to_text(&app(), 120, 30);
    assert!(
        t.contains("▶ asf-2") && !t.contains('■'),
        "a working run keeps its marker:\n{t}"
    );
}

#[test]
fn agents_row_of_a_blocked_run_starts_with_a_red_bold_square() {
    let mut a = App::new(blocked_snapshot());
    a.handle_key(key('2'));
    let t = render_to_text(&a, 120, 30);
    let row = t
        .lines()
        .find(|l| l.contains("asf-2") && l.contains("blocked") && l.contains('│'))
        .unwrap_or_else(|| panic!("no agents row:\n{t}"));
    assert!(row.trim_start_matches([' ', '│']).starts_with('■'), "{row:?}");
    let (fg, m) = cell_style(&a, "blocked", "■");
    assert_eq!(fg, Color::Red);
    assert!(m.contains(Modifier::BOLD));
    let mut w = App::new(snapshot());
    w.handle_key(key('2'));
    assert!(!render_to_text(&w, 120, 30).contains('■'));
}

// ---------- answer overlay ----------

fn open_overlay() -> App {
    let mut a = App::new(blocked_snapshot());
    a.handle_key(key('2'));
    assert_eq!(a.handle_key(key('a')), Action::Peek(id("asf-2")));
    a
}

#[test]
fn a_opens_the_overlay_only_for_a_blocked_run() {
    let mut w = App::new(snapshot());
    w.handle_key(key('2')); // asf-2 is working
    assert_eq!(w.handle_key(key('a')), Action::None);
    assert!(matches!(w.mode, Mode::Normal));
    assert!(
        w.status.as_deref().unwrap_or("").contains("no blocked run"),
        "{:?}",
        w.status
    );
    // an issue without a run (the board starts on asf-4, backlog)
    let mut b = App::new(blocked_snapshot());
    assert_eq!(b.handle_key(key('a')), Action::None);
    assert!(matches!(b.mode, Mode::Normal));
    // a blocked run, from Board, Agents and Inbox
    for (view, steps) in [('1', 2), ('2', 0), ('3', 0)] {
        let mut a = App::new(blocked_snapshot());
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
    let mut a = App::new(s);
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
