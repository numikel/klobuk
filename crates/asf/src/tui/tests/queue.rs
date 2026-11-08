//! The Queue view. Tests of it live here and nowhere else, so the task that rebuilds the view edits
//! this file only.

use super::*;
use crate::tui::snapshot::{text_width, REVIEW_STARTED};
use crate::tui::views::queue::{failed_tests, stages, Stage};

fn gate(name: &str, ok: bool, tail: &str) -> GateResult {
    GateResult {
        name: name.into(),
        ok,
        exit_code: Some(if ok { 0 } else { 101 }),
        duration_ms: 10,
        tail: tail.into(),
        attempts: 1,
    }
}

fn queue_item(n: i64, i: &str, state: MergeState, updated: &str) -> MergeItem {
    let mut m = merge_item(n, i, state, None, updated);
    m.attempts = 1;
    m
}

/// The mockup: asf-28 in review (round 1, `opus`), asf-25 failed in the `test` gate, three merges today.
fn queue_desk() -> Snapshot {
    let mut review = issue("asf-30", "Review asf-28", Status::InProgress);
    review.kind = IssueType::Review;
    review.parent_id = Some(id("asf-28"));
    let mut s = desk(vec![
        issue("asf-28", "Twenty-eight", Status::InReview),
        issue("asf-25", "Twenty-five", Status::InReview),
        issue("asf-24", "Twenty-four", Status::Done),
        issue("asf-22", "Twenty-two", Status::Done),
        issue("asf-21", "Twenty-one", Status::Done),
        review,
    ]);
    let mut reviewing = queue_item(1, "asf-28", MergeState::Reviewing, "2026-11-06T11:59:00Z");
    reviewing.gate_results = vec![
        gate("fmt", true, ""),
        gate("clippy", true, ""),
        gate("test", true, ""),
    ];
    let mut failed = queue_item(2, "asf-25", MergeState::Failed, "2026-11-06T11:48:00Z");
    failed.last_error = Some(
        "gate `test` failed (exit Some(101)) after rebase:\ntest start_retry_once ... FAILED\ntest start_gives_up_after_second_timeout ... FAILED\n\ntest result: FAILED. 10 passed; 2 failed"
            .into(),
    );
    failed.gate_results = vec![
        gate("fmt", true, ""),
        gate(
            "test",
            false,
            "test start_retry_once ... FAILED\ntest start_gives_up_after_second_timeout ... FAILED\n\nfailures:\n    start_retry_once\n    start_gives_up_after_second_timeout\n\ntest result: FAILED. 10 passed; 2 failed",
        ),
    ];
    s.queue = vec![reviewing, failed];
    let merged = |n: i64, i: &str, sha: &str, at: &str| {
        let mut m = queue_item(n, i, MergeState::Merged, at);
        m.head_sha = Some(sha.into());
        m
    };
    s.merged_today = vec![
        merged(3, "asf-24", "9c1e4aa0000", "2026-11-06T11:42:00Z"),
        merged(4, "asf-22", "f2b84da0000", "2026-11-06T08:00:00Z"),
        merged(5, "asf-21", "2d355650000", "2026-11-06T08:00:00Z"),
    ];
    s.runs
        .push(run_at("asf-30", RunState::Working, "2026-11-06T11:59:00Z"));
    s.events.push(Event {
        seq: 9,
        ts: "2026-11-06T11:59:00Z".into(),
        kind: REVIEW_STARTED.into(),
        issue_id: Some(id("asf-28")),
        run_id: None,
        actor: None,
        payload: serde_json::json!({ "round": 1 }),
    });
    // the merge of asf-24 was reviewed: its reviewer approved
    s.events.push(Event {
        seq: 5,
        ts: "2026-11-06T11:30:00Z".into(),
        kind: "merge.review_approved".into(),
        issue_id: Some(id("asf-24")),
        run_id: None,
        actor: None,
        payload: serde_json::json!({}),
    });
    s.derive();
    s
}

fn queue_app(s: Snapshot) -> App {
    let mut a = App::new(s);
    a.view = View::Queue;
    a
}

fn row_with<'a>(text: &'a str, needle: &str) -> &'a str {
    text.lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no line with {needle:?}:\n{text}"))
}

/// The screen line of the queue item `id` (the waiting banner names it too, but has no branch).
fn item_row<'a>(text: &'a str, id: &str) -> &'a str {
    text.lines()
        .find(|l| l.contains(&format!("feat/{id}")) && l.contains(id))
        .unwrap_or_else(|| {
            panic!(
                "no row for {id}:
{text}"
            )
        })
}

/// Byte offsets of `needles` in `line` must grow: the cells come in column order.
fn in_order(line: &str, needles: &[&str]) {
    let mut at = 0;
    for n in needles {
        let p = line[at..]
            .find(n)
            .unwrap_or_else(|| panic!("{n:?} missing or out of order in {line:?}"));
        at += p + n.len();
    }
}

#[test]
fn the_queue_lists_every_item_with_its_five_stages_and_the_age() {
    let t = render_to_text(&queue_app(queue_desk()), 120, 40);
    assert!(t.contains("IN THE QUEUE (2)"), "{t}");
    in_order(
        row_with(&t, "IN THE QUEUE"),
        &["rebase", "gates", "review", "merge", "main gates"],
    );
    let running = item_row(&t, "asf-28");
    in_order(
        running,
        &[
            "feat/asf-28",
            "✓ done",
            "✓ 3 of 3",
            "⠋ opus · round 1",
            "○",
            "○",
            "1 min",
        ],
    );
    assert!(running.contains("▸"), "the first item is selected: {running}");
    let failed = item_row(&t, "asf-25");
    in_order(
        failed,
        &["feat/asf-25", "✓ done", "✗ test", "·", "·", "·", "12 min"],
    );
    assert!(!failed.contains("▸"), "{failed}");
}

#[test]
fn a_failed_item_gets_three_dim_lines_the_error_with_the_attempt_the_tests_and_the_keys() {
    let a = queue_app(queue_desk());
    let t = render_to_text(&a, 120, 40);
    let lines: Vec<&str> = t.lines().collect();
    let at = lines.iter().position(|l| l.contains("feat/asf-25")).unwrap();
    assert!(
        lines[at + 1].contains("gate `test` failed") && lines[at + 1].contains("· attempt 1"),
        "{t}"
    );
    assert!(
        lines[at + 2].contains("2 tests failed: start_retry_once, start_gives_up_after_second_timeout"),
        "{t}"
    );
    assert_eq!(
        lines[at + 3].trim(),
        "r retry after a fix · v verify the run again · Enter full gate output"
    );
    // all three are secondary text, the key letters excepted
    assert!(cell_style(&a, "· attempt 1", "gate `test` failed")
        .1
        .contains(Modifier::DIM));
    assert!(cell_style(&a, "2 tests failed", "2 tests failed")
        .1
        .contains(Modifier::DIM));
    let key = cell_style(&a, "r retry after a fix", "r");
    assert!(key.1.contains(Modifier::BOLD), "key letters are bold");
    // the healthy item has no such lines
    let run = lines.iter().position(|l| l.contains("feat/asf-28")).unwrap();
    assert!(!lines[run + 1].contains("attempt"), "{t}");
}

#[test]
fn the_failed_stage_is_in_the_danger_style_and_never_by_colour_alone() {
    let a = queue_app(queue_desk());
    let (fg, m) = cell_style(&a, "feat/asf-25", "✗ test");
    assert_eq!(fg, Color::Red);
    assert!(m.contains(Modifier::BOLD));
    let mut plain = queue_app(queue_desk());
    plain.theme = Theme::new(false);
    let t = render_to_text(&plain, 120, 40);
    assert!(
        item_row(&t, "asf-25").contains("✗ test"),
        "the glyph carries the meaning"
    );
    let (fg, m) = cell_style(&plain, "feat/asf-25", "✗ test");
    assert_eq!(fg, Color::Reset);
    assert!(m.contains(Modifier::BOLD));
}

#[test]
fn merged_today_lists_sha_review_outcome_main_state_and_age() {
    let t = render_to_text(&queue_app(queue_desk()), 120, 40);
    assert!(t.contains("MERGED TODAY (3)"), "{t}");
    let one = row_with(&t, "✓ asf-24");
    in_order(
        one,
        &["9c1e4aa", "review approve", "gates on main green", "18 min ago"],
    );
    assert!(!one.contains("9c1e4aa0"), "a short sha: {one}");
    let two = row_with(&t, "✓ asf-22");
    in_order(
        two,
        &[
            "f2b84da",
            "no review (review was off)",
            "gates on main green",
            "4 h ago",
        ],
    );
    // a merged item that carries an error is a red main
    let mut s = queue_desk();
    s.merged_today[0].last_error = Some("main red after merge: gate `test` failed (exit 101)".into());
    let t = render_to_text(&queue_app(s), 120, 40);
    assert!(row_with(&t, "✓ asf-24").contains("main red"), "{t}");
    assert!(!row_with(&t, "✓ asf-24").contains("gates on main green"));
}

#[test]
fn merged_today_is_omitted_when_nothing_merged_and_the_queue_still_renders() {
    let mut s = queue_desk();
    s.merged_today.clear();
    let t = render_to_text(&queue_app(s), 120, 40);
    assert!(
        t.contains("IN THE QUEUE (2)") && !t.contains("MERGED TODAY"),
        "{t}"
    );
}

#[test]
fn an_empty_queue_says_what_arrives_here_and_when() {
    let a = queue_app(desk(vec![]));
    let t = render_to_text(&a, 100, 30);
    assert!(
        t.contains("Queue is empty. A branch arrives here after its worker's run is verified."),
        "{t}"
    );
    assert!(!t.contains("IN THE QUEUE") && !t.contains("MERGED TODAY"), "{t}");
    assert!(cell_style(&a, "Queue is empty.", "Queue is empty.")
        .1
        .contains(Modifier::DIM));
    // merges of today still show under the sentence
    let mut s = queue_desk();
    s.queue.clear();
    let t = render_to_text(&queue_app(s), 120, 40);
    assert!(
        t.contains("Queue is empty.") && t.contains("MERGED TODAY (3)"),
        "{t}"
    );
}

#[test]
fn no_line_is_wider_than_the_screen_at_100_columns_and_the_cells_are_cut_with_an_ellipsis() {
    let mut s = queue_desk();
    s.queue[0].branch = "feat/asf-28-a-branch-with-a-very-long-descriptive-name".into();
    s.queue[1].last_error = Some(format!("gate `test` failed after rebase: {}", "x".repeat(200)));
    for w in [100u16, 120] {
        let t = render_to_text(&queue_app(s.clone()), w, 40);
        for l in t.lines() {
            assert!(text_width(l) <= w as usize, "{w}: {l:?}");
        }
        let row = item_row(&t, "asf-28");
        in_order(
            row,
            &["feat/asf-28", "✓ done", "✓ 3 of 3", "⠋ opus · round 1", "1 min"],
        );
    }
    let t = render_to_text(&queue_app(s), 100, 40);
    assert!(item_row(&t, "asf-28").contains('…'), "{t}");
    assert!(row_with(&t, "· attempt 1").contains('…'), "{t}");
}

#[test]
fn the_selected_row_is_highlighted_and_reversed_without_colours() {
    let mut a = queue_app(queue_desk());
    let buf = render_to_buffer(&a, 120, 40);
    let line = |y: u16| -> String { (0..120).map(|x| buf[(x, y)].symbol().to_string()).collect() };
    let y = (0..40).find(|y| line(*y).contains("feat/asf-28")).unwrap();
    assert!(buf[(60, y)].bg != Color::Reset, "highlighted background");
    let y2 = (0..40).find(|y| line(*y).contains("feat/asf-25")).unwrap();
    assert_eq!(buf[(60, y2)].bg, Color::Reset);
    a.theme = Theme::new(false);
    let buf = render_to_buffer(&a, 120, 40);
    assert!(buf[(60, y)].modifier.contains(Modifier::REVERSED));
}

#[test]
fn the_selection_moves_with_j_and_k_and_the_window_scrolls_to_keep_it_on_screen() {
    let mut a = queue_app(queue_desk());
    assert_eq!(a.selected_issue().unwrap().as_str(), "asf-28");
    a.handle_key(key('j'));
    assert_eq!(a.selected_issue().unwrap().as_str(), "asf-25");
    a.handle_key(key('j'));
    assert_eq!(a.selected_issue().unwrap().as_str(), "asf-25", "clamped");
    a.handle_key(key('k'));
    assert_eq!(a.selected_issue().unwrap().as_str(), "asf-28");
    // a screen too short for everything still shows the selected item with its detail lines
    a.handle_key(key('j'));
    let t = render_to_text(&a, 100, 20);
    assert!(
        t.contains("■ asf-25") && t.contains("Enter full gate output"),
        "{t}"
    );
}

#[test]
fn each_stage_is_derived_from_the_state_the_gate_results_and_the_review_events() {
    use Stage::*;
    let s = queue_desk();
    let item = |state: MergeState| {
        let mut m = queue_item(9, "asf-26", state, "2026-11-06T11:00:00Z");
        m.gate_results = vec![gate("fmt", true, ""), gate("test", true, "")];
        m
    };
    let done = || Done("done".into());
    let st = |m: &MergeItem| stages(&s, m);
    // review is off and no label asks for one: it shows `–` before it would have run
    assert_eq!(
        st(&item(MergeState::Queued)),
        [Pending, Pending, NotNeeded, Pending, Pending]
    );
    assert_eq!(
        st(&item(MergeState::Rebasing)),
        [Running("rebasing".into()), Pending, NotNeeded, Pending, Pending]
    );
    assert_eq!(
        st(&item(MergeState::Gating)),
        [done(), Running("running".into()), NotNeeded, Pending, Pending]
    );
    // `[review] required` makes it a stage to come
    let mut required = s.clone();
    required.review_required = true;
    assert_eq!(
        stages(&required, &item(MergeState::Gating)),
        [done(), Running("running".into()), Pending, Pending, Pending]
    );
    // so does a label from `[review] on_labels`
    let mut labelled = s.clone();
    labelled.review_on_labels = vec!["risky".into()];
    labelled.issues.push({
        let mut i = issue("asf-26", "Labelled", Status::InReview);
        i.labels = vec!["risky".into()];
        i
    });
    assert_eq!(stages(&labelled, &item(MergeState::Queued))[2], Pending);
    assert_eq!(
        st(&item(MergeState::Queued))[2],
        NotNeeded,
        "the label is not on the plain snapshot"
    );
    let gates_done = || Done("2 of 2".into());
    assert_eq!(
        st(&item(MergeState::Merging)),
        [
            done(),
            gates_done(),
            NotNeeded,
            Running("merging".into()),
            Pending
        ],
        "no reviewer ever: review is off for this item"
    );
    assert_eq!(
        st(&item(MergeState::PrOpen)),
        [
            done(),
            gates_done(),
            NotNeeded,
            Running("PR open".into()),
            Pending
        ]
    );
    // an item whose branch was reviewed shows the review as done
    let mut reviewed = s.clone();
    reviewed.events.push(Event {
        seq: 20,
        ts: "2026-11-06T11:00:00Z".into(),
        kind: "merge.review_approved".into(),
        issue_id: Some(id("asf-26")),
        run_id: None,
        actor: None,
        payload: serde_json::json!({}),
    });
    assert_eq!(
        stages(&reviewed, &item(MergeState::Merging)),
        [done(), gates_done(), done(), Running("merging".into()), Pending]
    );
    // the reviewer is running: model and round
    assert_eq!(
        stages(&s, &{
            let mut m = item(MergeState::Reviewing);
            m.issue_id = id("asf-28");
            m
        })[2],
        Running("opus · round 1".into())
    );
}

#[test]
fn a_failure_marks_its_stage_and_dots_the_stages_after_it() {
    use Stage::*;
    let s = queue_desk();
    let done = || Done("done".into());
    let failed_with = |state: MergeState, error: &str, gates: Vec<GateResult>| {
        let mut m = queue_item(9, "asf-26", state, "2026-11-06T11:00:00Z");
        m.last_error = Some(error.into());
        m.gate_results = gates;
        stages(&s, &m)
    };
    // the failed gate names itself
    assert_eq!(
        stages(&s, &s.queue[1]),
        [done(), Failed("test".into()), After, After, After]
    );
    assert_eq!(
        failed_with(
            MergeState::Blocked,
            "rebase of feat/asf-26 onto main conflicts in: a.rs",
            vec![]
        ),
        [Failed("conflict".into()), After, After, After, After]
    );
    assert_eq!(
        failed_with(MergeState::Failed, "rebase failed: boom", vec![])[0],
        Failed("rebase".into())
    );
    assert_eq!(
        failed_with(
            MergeState::Blocked,
            "feat/asf-26 touches files outside scope [src/**]: x",
            vec![]
        ),
        [done(), Failed("scope".into()), After, After, After]
    );
    assert_eq!(
        failed_with(
            MergeState::Failed,
            "review asks for changes on feat/asf-26:\n- x",
            vec![gate("fmt", true, "")]
        ),
        [
            done(),
            Done("1 of 1".into()),
            Failed("changes".into()),
            After,
            After
        ]
    );
    assert_eq!(
        failed_with(
            MergeState::Failed,
            "fast-forward failed: not possible",
            vec![gate("fmt", true, "")]
        ),
        [
            done(),
            Done("1 of 1".into()),
            NotNeeded,
            Failed("merge".into()),
            After
        ]
    );
}

#[test]
fn failed_test_names_come_from_libtest_lines_and_the_failures_section_without_repeats() {
    let out = "running 3 tests\ntest a::ok ... ok\ntest a::first ... FAILED\ntest b::second ... FAILED\n\nfailures:\n\n---- a::first stdout ----\nboom\n\nfailures:\n    a::first\n    b::second\n\ntest result: FAILED. 1 passed; 2 failed";
    assert_eq!(failed_tests(out), ["a::first", "b::second"]);
    assert!(failed_tests("error: could not compile `asf`").is_empty());
}

#[test]
fn enter_opens_the_gate_output_in_the_comment_modal() {
    let mut a = queue_app(queue_desk());
    a.handle_key(key('j'));
    match a.handle_key(code(KeyCode::Enter)) {
        Action::None => {}
        other => panic!("the app consumes OpenComment itself: {other:?}"),
    }
    match a.mode() {
        Mode::Comment { comment, .. } => {
            assert_eq!(comment.issue_id, id("asf-25"));
            assert!(comment.body.contains("gate `test` failed"), "{}", comment.body);
            assert!(comment.body.contains("test start_retry_once ... FAILED"));
            assert!(comment.body.contains("fmt") && comment.body.contains("ok"));
        }
        other => panic!("not the comment modal: {other:?}"),
    }
    // an item with no gate output yet says so instead of opening an empty window
    let mut s = queue_desk();
    s.queue[0].state = MergeState::Queued;
    s.queue[0].gate_results.clear();
    let mut a = queue_app(s);
    a.handle_key(code(KeyCode::Enter));
    assert!(matches!(a.mode(), Mode::Normal));
    assert!(
        a.status.as_deref().unwrap_or("").contains("asf-28"),
        "{:?}",
        a.status
    );
}

#[test]
fn r_retries_and_v_verifies_a_failed_or_blocked_item_and_both_refuse_any_other() {
    let mut a = queue_app(queue_desk());
    for c in ['r', 'v'] {
        a.handle_key(key(c));
        let status = a.status.clone().unwrap_or_default();
        assert!(status.contains("asf-28"), "{c} on a reviewing item: {status:?}");
        assert!(matches!(a.mode(), Mode::Normal), "never opens a reply");
    }
    a.handle_key(key('j'));
    assert_eq!(a.handle_key(key('r')), Action::Retry(id("asf-25")));
    assert_eq!(a.handle_key(key('v')), Action::Verify(id("asf-25")));
    let mut s = queue_desk();
    s.queue[1].state = MergeState::Blocked;
    let mut a = queue_app(s);
    a.handle_key(key('j'));
    assert_eq!(a.handle_key(key('r')), Action::Retry(id("asf-25")));
}

#[test]
fn f_focuses_the_reviewer_of_the_selected_item_and_says_so_when_there_is_none() {
    let mut a = queue_app(queue_desk());
    assert_eq!(a.handle_key(key('f')), Action::Focus(id("asf-30")));
    a.handle_key(key('j'));
    assert_eq!(a.handle_key(key('f')), Action::None);
    assert!(
        a.status.as_deref().unwrap_or("").contains("asf-25"),
        "{:?}",
        a.status
    );
}

#[test]
fn keys_the_view_does_not_list_do_nothing() {
    let mut a = queue_app(queue_desk());
    for c in ['x', 'a', 's', 'm'] {
        assert_eq!(a.handle_key(key(c)), Action::None, "{c}");
        assert!(a.status.is_none() && matches!(a.mode(), Mode::Normal), "{c}");
    }
}

#[test]
fn the_footer_and_the_help_page_list_the_queue_keys() {
    let a = queue_app(queue_desk());
    let f = footer_line(&a, 120);
    in_order(
        &f,
        &[
            "j / k move",
            "r retry",
            "v verify",
            "Enter gate output",
            "f focus reviewer",
            "? help",
            "q quit",
        ],
    );
    let hints = footer_hints(View::Queue, ModeKind::Normal);
    assert!(hints.len() <= 7, "{}", hints.len());
    let sections = crate::tui::bindings::help_sections();
    let queue = sections
        .iter()
        .find(|s| s.title == "QUEUE")
        .expect("a QUEUE heading");
    let keys: Vec<&str> = queue.items.iter().map(|(k, _)| *k).collect();
    assert_eq!(keys, ["r", "v", "Enter", "f"]);
}

#[test]
fn queue_view_selects_its_own_issue() {
    let mut a = app();
    a.handle_key(key('5'));
    assert_eq!(a.selected_issue().unwrap().as_str(), "asf-1");
    assert_eq!(a.rows(View::Queue), 1);
}
