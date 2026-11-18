//! The steps of a queue item (`views/steps.rs`, the Queue view's open item) and the gate run in flight
//! in the Now, Agents and Queue views and in its own window.

use super::*;
use crate::tui::snapshot::GateInfo;
use crate::tui::views::steps::{duration, item_steps, StepKind, StepMark};
use crate::worker::{GateLive, LiveGate};
use asf_core::config::GateWhen;

fn event(seq: u64, at: &str, kind: &str, issue: &str, payload: serde_json::Value) -> Event {
    Event {
        seq,
        ts: at.into(),
        kind: kind.into(),
        issue_id: Some(id(issue)),
        run_id: None,
        actor: None,
        payload,
    }
}

/// The queue's move of merge item `n` into `to`.
fn moved(seq: u64, at: &str, issue: &str, n: i64, to: &str) -> Event {
    event(
        seq,
        at,
        "merge.state",
        issue,
        serde_json::json!({ "merge_id": n, "to": to }),
    )
}

fn gate_cfg(name: &str, when: &[GateWhen]) -> GateInfo {
    GateInfo {
        name: name.into(),
        when: when.to_vec(),
    }
}

fn live_gate(name: &str, ok: bool, ms: u64, tail: &str) -> LiveGate {
    LiveGate {
        name: name.into(),
        ok,
        duration_ms: ms,
        tail: tail.into(),
    }
}

/// The gate run in flight: `issue`'s `kind` run, on `gate` since `gate_since`, the earlier gates over.
fn run_live(
    kind: &str,
    issue: &str,
    gate: &str,
    gate_since: &str,
    finished: Vec<LiveGate>,
    tail: &str,
) -> GateLive {
    GateLive {
        kind: kind.into(),
        issue: id(issue),
        gates: vec!["fmt".into(), "clippy".into(), "test".into()],
        started_at: "2026-11-06T11:46:01Z".into(),
        gate: gate.into(),
        gate_started_at: gate_since.into(),
        finished,
        tail: tail.into(),
        updated_at: NOW.into(),
    }
}

/// asf-134 on its second attempt, in the `test` gate for 6 min 12 s; its first attempt failed in `test`
/// after 9 min. The config has a fourth gate that only runs in the verification.
fn gating_desk() -> Snapshot {
    let mut s = desk(vec![issue("asf-134", "One thirty-four", Status::InReview)]);
    let mut item = merge_item(7, "asf-134", MergeState::Gating, None, "2026-11-06T11:46:01Z");
    item.attempts = 2;
    item.rebased_onto = Some("c1b3a61f00".into());
    s.queue = vec![item];
    s.gates = vec![
        gate_cfg("fmt", &GateWhen::ALL),
        gate_cfg("clippy", &GateWhen::ALL),
        gate_cfg("test", &GateWhen::ALL),
        gate_cfg("docs", &[GateWhen::Verify]),
    ];
    s.verify_main_after = true;
    s.events = vec![
        moved(11, "2026-11-06T11:46:01Z", "asf-134", 7, "gating"),
        moved(10, "2026-11-06T11:46:00Z", "asf-134", 7, "rebasing"),
        moved(9, "2026-11-06T11:45:00Z", "asf-134", 7, "queued"),
        event(
            8,
            "2026-11-06T11:44:30Z",
            "run.fix_sent_back",
            "asf-134",
            serde_json::json!({ "kind": "gate", "headline": "after the rebase onto abc1234 the gate `test` failed" }),
        ),
        moved(3, "2026-11-06T11:39:00Z", "asf-134", 7, "failed"),
        moved(2, "2026-11-06T11:30:00Z", "asf-134", 7, "gating"),
        moved(1, "2026-11-06T11:29:59Z", "asf-134", 7, "rebasing"),
    ];
    s.gate_live = Some(run_live(
        "merge",
        "asf-134",
        "test",
        "2026-11-06T11:53:48Z",
        vec![
            live_gate("fmt", true, 1000, "fmt ok"),
            live_gate("clippy", true, 1200, "clippy ok"),
        ],
        "compiling asf-core\nrunning 12 tests\ntest a::b ... ok",
    ));
    s.derive();
    s
}

fn queue_app(s: Snapshot) -> App {
    let mut a = App::new(s);
    a.view = View::Queue;
    a
}

/// The screen line that has every one of `needles`.
fn line_with<'a>(text: &'a str, needles: &[&str]) -> &'a str {
    text.lines()
        .find(|l| needles.iter().all(|n| l.contains(n)))
        .unwrap_or_else(|| panic!("no line with {needles:?}:\n{text}"))
}

/// Whether some screen line has every one of `needles`.
fn present(text: &str, needles: &[&str]) -> bool {
    text.lines().any(|l| needles.iter().all(|n| l.contains(n)))
}

fn open_queue(s: Snapshot) -> (App, String) {
    let mut a = queue_app(s);
    a.handle_key(key('l'));
    let t = render_to_text(&a, 120, 40);
    (a, t)
}

#[test]
fn durations_are_written_the_way_the_rows_do_and_a_whole_minute_drops_its_seconds() {
    assert_eq!(duration(1), "1 s");
    assert_eq!(duration(372), "6 min 12 s");
    assert_eq!(duration(540), "9 min");
    assert_eq!(duration(7500), "2 h 5 min");
    assert_eq!(duration(7200), "2 h");
    assert_eq!(duration(-5), "0 s");
}

#[test]
fn an_open_item_lists_the_rebase_every_configured_gate_the_merge_and_main_with_marks_and_times() {
    let (_, t) = open_queue(gating_desk());
    assert!(
        line_with(&t, &["rebase onto main (c1b3a61)", "✓", "1 s"])
            .trim_end()
            .ends_with("1 s"),
        "{t}"
    );
    assert!(line_with(&t, &["✓ gate fmt"]).trim_end().ends_with("1 s"), "{t}");
    assert!(
        line_with(&t, &["✓ gate clippy"]).trim_end().ends_with("1 s"),
        "{t}"
    );
    // the running gate counts up from the moment its gate started: 11:53:48 to 12:00:00
    let running = line_with(&t, &["● gate test"]);
    assert!(
        running.contains("running") && running.trim_end().ends_with("6 min 12 s"),
        "{t}"
    );
    assert!(line_with(&t, &["○ merge"]).trim_end().ends_with("merge"), "{t}");
    assert!(
        line_with(&t, &["○ gates on main"]).contains("gates on main"),
        "{t}"
    );
    // the name of the gate is the config's, and a gate this stage does not run says why
    assert!(
        present(&t, &["– gate docs", "gate skipped: when-condition"]),
        "{t}"
    );
    assert!(!t.contains("cargo"), "nothing is hard-coded to a tool: {t}");
    // the attempt and its age, once
    assert!(t.contains("attempt 2 · 14 min"), "{t}");
}

#[test]
fn the_first_gate_that_has_not_started_waits_and_the_unselected_rows_stay_one_line() {
    let mut s = gating_desk();
    s.queue.push(merge_item(
        8,
        "asf-135",
        MergeState::Queued,
        None,
        "2026-11-06T11:50:00Z",
    ));
    s.issues.push(issue("asf-135", "Another", Status::InReview));
    s.derive();
    let (_, t) = open_queue(s);
    assert!(t.contains("✓ gate fmt"), "{t}");
    // the second item is not selected: its row is the only line it has
    let others: Vec<&str> = t.lines().filter(|l| l.contains("asf-135")).collect();
    assert_eq!(others.len(), 1, "its row and nothing else: {others:?}");
    assert!(
        !t.contains("waiting for the gate slot (asf-134"),
        "a queued item folded away does not say it: {t}"
    );
}

#[test]
fn a_queued_item_says_what_it_waits_for_and_how_many_wait_before_it() {
    let mut s = gating_desk();
    s.queue = vec![
        merge_item(7, "asf-134", MergeState::Gating, None, "2026-11-06T11:46:01Z"),
        merge_item(8, "asf-135", MergeState::Queued, None, "2026-11-06T11:50:00Z"),
        merge_item(9, "asf-136", MergeState::Queued, None, "2026-11-06T11:51:00Z"),
    ];
    s.gate_live = Some(run_live(
        "merge",
        "asf-134",
        "test",
        "2026-11-06T11:47:57Z",
        vec![],
        "",
    ));
    s.events.clear();
    s.derive();
    let mut a = queue_app(s);
    a.handle_key(key('G'));
    a.handle_key(key('l'));
    let t = render_to_text(&a, 120, 40);
    let wait = line_with(&t, &["rebase onto main", "waiting for the gate slot"]);
    assert!(
        wait.contains("(asf-134 is on gate test, 12 min 3 s)") && wait.contains("1 ahead"),
        "{wait}"
    );
}

#[test]
fn a_waiting_item_of_a_stopped_loop_says_so_instead_of_naming_a_holder() {
    let mut s = gating_desk();
    s.queue = vec![merge_item(
        8,
        "asf-135",
        MergeState::Queued,
        None,
        "2026-11-06T11:50:00Z",
    )];
    s.gate_live = None;
    s.events.clear();
    s.serve = Some(crate::tui::snapshot::LoopInfo {
        state: asf_store::serve_pulse::LoopState::NeverStarted,
        mode: asf_store::serve_pulse::ServeMode::Dispatch,
    });
    s.derive();
    let (_, t) = open_queue(s);
    assert!(
        t.contains("waiting for the gate slot (asf serve is not running)"),
        "{t}"
    );
}

#[test]
fn earlier_attempts_are_one_line_each_and_open_into_their_phases() {
    let (mut a, t) = open_queue(gating_desk());
    assert!(t.contains("▸ attempt 1: gate test failed after 9 min"), "{t}");
    assert!(
        !t.contains("attempt 1: ") || !t.contains("✗ gates"),
        "folded: {t}"
    );
    // the cursor goes down to the folded line first, Enter opens it
    a.handle_key(key('j'));
    a.handle_key(code(KeyCode::Enter));
    let t = render_to_text(&a, 120, 40);
    assert!(t.contains("▾ attempt 1: gate test failed after 9 min"), "{t}");
    assert!(present(&t, &["✓ rebase", "1 s"]), "{t}");
    assert!(present(&t, &["✗ gates", "9 min"]), "{t}");
    a.handle_key(code(KeyCode::Enter));
    assert!(!render_to_text(&a, 120, 40).contains("✗ gates"));
}

#[test]
fn j_and_k_walk_the_lines_of_the_open_item_before_they_leave_it_and_moving_on_folds_it() {
    let mut s = gating_desk();
    s.queue.push(merge_item(
        8,
        "asf-135",
        MergeState::Queued,
        None,
        "2026-11-06T11:50:00Z",
    ));
    s.issues.push(issue("asf-135", "Another", Status::InReview));
    s.derive();
    let mut a = queue_app(s);
    a.handle_key(key('l'));
    let steps = item_steps(&a.snap, &a.snap.queue[0]);
    let lines = steps.earlier.len() + steps.steps.len();
    for _ in 0..lines {
        a.handle_key(key('j'));
        assert_eq!(a.selected_issue().unwrap().as_str(), "asf-134");
    }
    a.handle_key(key('j'));
    assert_eq!(
        a.selected_issue().unwrap().as_str(),
        "asf-135",
        "past the last line, the next item"
    );
    assert!(!a.queue.expanded, "the item that was left folds");
    a.handle_key(key('k'));
    assert_eq!(a.selected_issue().unwrap().as_str(), "asf-134");
    assert!(
        !render_to_text(&a, 120, 40).contains("✓ gate fmt"),
        "and stays folded"
    );
    // h folds, Enter on the row opens and folds again
    a.handle_key(code(KeyCode::Enter));
    assert!(a.queue.expanded);
    a.handle_key(code(KeyCode::Enter));
    assert!(!a.queue.expanded);
    a.handle_key(key('l'));
    a.handle_key(key('h'));
    assert!(!a.queue.expanded);
}

#[test]
fn enter_on_a_finished_gate_opens_its_output_and_on_the_running_one_the_live_window() {
    let (mut a, _) = open_queue(gating_desk());
    // lines: attempt 1, rebase, fmt, clippy, test
    for _ in 0..3 {
        a.handle_key(key('j'));
    }
    a.handle_key(code(KeyCode::Enter)); // gate fmt, over
    match a.mode() {
        Mode::Comment { comment, .. } => {
            assert!(comment.body.starts_with("gate fmt: ok"), "{}", comment.body);
            assert!(
                comment.body.contains("fmt ok"),
                "its output is kept: {}",
                comment.body
            );
        }
        other => panic!("not the output window: {other:?}"),
    }
    a.handle_key(code(KeyCode::Esc));
    for _ in 0..2 {
        a.handle_key(key('j'));
    }
    a.handle_key(code(KeyCode::Enter)); // gate test, running
    assert!(matches!(a.mode(), Mode::Gate { .. }), "{:?}", a.mode());
    let t = render_to_text(&a, 120, 40);
    assert!(t.contains("gate test") && t.contains("running 6 min 12 s"), "{t}");
    assert!(t.contains("✓ fmt 1 s") && t.contains("✓ clippy 1 s"), "{t}");
    assert!(
        t.contains("running 12 tests") && t.contains("test a::b ... ok"),
        "{t}"
    );
}

#[test]
fn the_live_window_moves_with_the_snapshot_and_follows_the_newest_line_until_scrolled_up() {
    let (mut a, _) = open_queue(gating_desk());
    for _ in 0..5 {
        a.handle_key(key('j'));
    }
    a.handle_key(code(KeyCode::Enter));
    assert!(matches!(a.mode(), Mode::Gate { .. }));
    // 60 lines of output: a small window shows the newest ones
    let mut s = a.snap.clone();
    let tail: Vec<String> = (1..=60).map(|n| format!("line {n}")).collect();
    s.gate_live.as_mut().unwrap().tail = tail.join("\n");
    s.now = "2026-11-06T12:00:30Z".into();
    a.replace(s);
    let t = render_to_text(&a, 100, 24);
    assert!(t.contains("line 60") && !t.contains("line 1\n"), "{t}");
    assert!(t.contains("running 6 min 42 s"), "the timer moved: {t}");
    // k scrolls away from the newest line, G comes back
    for _ in 0..30 {
        a.handle_key(key('k'));
    }
    let t = render_to_text(&a, 100, 24);
    assert!(!t.contains("line 60"), "{t}");
    a.handle_key(key('G'));
    assert!(render_to_text(&a, 100, 24).contains("line 60"));
    // when the run is over the window says so rather than showing a gate that is gone
    let mut s = a.snap.clone();
    s.gate_live = None;
    a.replace(s);
    let t = render_to_text(&a, 100, 24);
    assert!(t.contains("No gate run is in flight"), "{t}");
    a.handle_key(code(KeyCode::Esc));
    assert!(matches!(a.mode(), Mode::Normal));
}

#[test]
fn the_steps_of_a_reused_run_a_skipped_review_and_a_waiting_review_say_why() {
    let mut s = gating_desk();
    s.gate_live = None;
    s.review_required = true;
    s.queue[0].state = MergeState::Merging;
    s.queue[0].gate_results = vec![GateResult {
        name: "fmt".into(),
        ok: true,
        exit_code: Some(0),
        duration_ms: 1000,
        tail: String::new(),
        attempts: 1,
    }];
    s.events.push(event(
        20,
        "2026-11-06T11:55:00Z",
        crate::commands::merge::GATES_REUSED,
        "asf-134",
        serde_json::json!({ "merge_id": 7, "attempt": 2, "head": "abc", "gates": ["fmt"] }),
    ));
    s.events.push(event(
        21,
        "2026-11-06T11:55:01Z",
        "merge.review_skipped",
        "asf-134",
        serde_json::json!({ "patch_id": "p" }),
    ));
    s.derive();
    let (_, t) = open_queue(s);
    assert!(
        present(&t, &["– gate fmt", "gates reused from verification"]),
        "{t}"
    );
    assert!(
        present(
            &t,
            &["– review", "review skipped: this patch was approved before"]
        ),
        "{t}"
    );
}

#[test]
fn the_review_step_names_the_round_the_model_and_the_reviewer_run_and_enter_shows_the_verdict() {
    let mut s = gating_desk();
    s.gate_live = None;
    s.queue[0].state = MergeState::Reviewing;
    s.queue[0].gate_results = vec![GateResult {
        name: "fmt".into(),
        ok: true,
        exit_code: Some(0),
        duration_ms: 1000,
        tail: String::new(),
        attempts: 1,
    }];
    let mut reviewer = issue("asf-137", "Review asf-134", Status::InProgress);
    reviewer.kind = IssueType::Review;
    reviewer.parent_id = Some(id("asf-134"));
    s.issues.push(reviewer);
    s.runs
        .push(run_at("asf-137", RunState::Working, "2026-11-06T11:58:00Z"));
    s.events
        .push(moved(30, "2026-11-06T11:57:00Z", "asf-134", 7, "reviewing"));
    s.events.push(event(
        31,
        "2026-11-06T11:57:01Z",
        "merge.review_started",
        "asf-134",
        serde_json::json!({ "round": 1 }),
    ));
    let mut verdict = comment("asf-134", CommentType::Decision, "verdict: approve - looks fine");
    verdict.author = "agent:asf-137".into();
    s.verdicts.insert(id("asf-134"), verdict);
    s.derive();
    let (mut a, t) = open_queue(s);
    let review = line_with(&t, &["● review"]);
    assert!(
        review.contains("round 1") && review.contains("opus") && review.contains("run-asf-137"),
        "{review}"
    );
    assert!(
        review.trim_end().ends_with("3 min"),
        "counted from the review start: {review}"
    );
    // the cursor walks to the review step: attempt 1, rebase, 3 gates + docs, review
    let steps = item_steps(&a.snap, &a.snap.queue[0]);
    let at = steps.earlier.len()
        + steps
            .steps
            .iter()
            .position(|s| s.kind == StepKind::Review)
            .unwrap();
    for _ in 0..=at {
        a.handle_key(key('j'));
    }
    a.handle_key(code(KeyCode::Enter));
    match a.mode() {
        Mode::Comment { comment, .. } => assert!(comment.body.contains("approve"), "{}", comment.body),
        other => panic!("no verdict window: {other:?}"),
    }
    a.handle_key(code(KeyCode::Esc));
    // f focuses the reviewer from any line
    assert_eq!(a.handle_key(key('f')), Action::Focus(id("asf-137")));
}

#[test]
fn gates_on_main_are_a_step_only_when_they_run_there() {
    let mut s = gating_desk();
    s.verify_main_after = false;
    s.derive();
    let steps = item_steps(&s, &s.queue[0]);
    assert!(!steps.steps.iter().any(|st| st.kind == StepKind::MainGates));
    s.verify_main_after = true;
    let steps = item_steps(&s, &s.queue[0]);
    assert_eq!(
        steps.steps.last().map(|st| (st.kind.clone(), st.mark)),
        Some((StepKind::MainGates, StepMark::Waiting))
    );
}

#[test]
fn a_failed_item_keeps_the_gates_it_passed_marks_the_one_that_failed_and_does_not_run_the_rest() {
    let mut s = gating_desk();
    s.gate_live = None;
    s.queue[0].state = MergeState::Failed;
    s.queue[0].last_error = Some("gate `clippy` failed (exit Some(101)) after rebase".into());
    s.queue[0].gate_results = vec![
        GateResult {
            name: "fmt".into(),
            ok: true,
            exit_code: Some(0),
            duration_ms: 1000,
            tail: String::new(),
            attempts: 1,
        },
        GateResult {
            name: "clippy".into(),
            ok: false,
            exit_code: Some(101),
            duration_ms: 42_000,
            tail: "error: boom".into(),
            attempts: 1,
        },
    ];
    s.derive();
    let (_, t) = open_queue(s);
    assert!(present(&t, &["✓ gate fmt"]));
    let bad = line_with(&t, &["✗ gate clippy"]);
    assert!(
        bad.contains("failed") && bad.trim_end().ends_with("42 s"),
        "{bad}"
    );
    assert!(present(&t, &["– gate test", "not run"]), "{t}");
}

// ---- the Now and Agents views ----

fn finishing_desk() -> Snapshot {
    let mut s = desk(live(&["asf-163", "asf-171", "asf-172", "asf-173"]));
    s.runs
        .push(run_at("asf-171", RunState::Finishing, "2026-11-06T11:00:00Z"));
    s.runs
        .push(run_at("asf-172", RunState::Finishing, "2026-11-06T11:10:00Z"));
    s.runs
        .push(run_at("asf-173", RunState::Working, "2026-11-06T11:20:00Z"));
    s.queue = vec![merge_item(
        5,
        "asf-163",
        MergeState::Gating,
        None,
        "2026-11-06T11:40:00Z",
    )];
    s.gate_live = Some(run_live(
        "merge",
        "asf-163",
        "test",
        "2026-11-06T11:48:00Z",
        vec![live_gate("fmt", true, 1000, "")],
        "x",
    ));
    s.derive();
    s
}

#[test]
fn a_run_in_finishing_says_who_holds_the_gate_slot_on_which_gate_for_how_long_and_how_many_are_ahead() {
    let mut a = App::new(finishing_desk());
    a.view = View::Now;
    let t = render_to_text(&a, 120, 30);
    let first = line_with(
        &t,
        &["waiting for the gate slot: asf-163 is on gate test, 12 min"],
    );
    assert!(!first.contains("ahead"), "asf-171 is first in line: {first}");
    let second = line_with(&t, &["waiting for the gate slot", "1 ahead"]);
    assert!(second.contains("asf-163 is on gate test"), "{second}");
    // the working run has no such line
    assert_eq!(t.matches("waiting for the gate slot").count(), 2, "{t}");
}

#[test]
fn a_run_in_verifying_shows_its_gate_with_a_live_timer_and_the_gates_that_are_over() {
    let mut s = finishing_desk();
    s.runs[0].state = RunState::Verifying;
    s.queue.clear();
    s.gate_live = Some(run_live(
        "verify",
        "asf-171",
        "clippy",
        "2026-11-06T11:58:30Z",
        vec![live_gate("fmt", true, 3000, "")],
        "x",
    ));
    s.derive();
    let mut a = App::new(s);
    a.view = View::Now;
    let t = render_to_text(&a, 120, 30);
    let row = line_with(&t, &["gate clippy running 1 min 30 s"]);
    assert!(row.contains("✓ fmt 3 s"), "{row}");
}

#[test]
fn enter_on_a_run_in_the_gate_queue_opens_the_live_output_in_the_now_and_agents_views() {
    let mut a = App::new(finishing_desk());
    a.view = View::Now;
    // the first RUNNING row is asf-171, finishing
    a.handle_key(code(KeyCode::Enter));
    assert!(matches!(a.mode(), Mode::Gate { .. }), "{:?}", a.mode());
    let t = render_to_text(&a, 120, 30);
    assert!(
        t.contains("asf-171 waits for the gate slot"),
        "the window says whose gate it is: {t}"
    );
    assert!(t.contains("gate test"), "{t}");
    a.handle_key(code(KeyCode::Esc));
    // a run that works keeps Enter for the Board
    a.handle_key(key('j'));
    a.handle_key(key('j'));
    a.handle_key(code(KeyCode::Enter));
    assert_eq!(a.view, View::Board);
    // the Agents view says the short form and opens the same window
    let mut a = App::new(finishing_desk());
    a.view = View::Agents;
    let t = render_to_text(&a, 140, 30);
    assert!(t.contains("waiting for the gate slot"), "{t}");
    a.handle_key(code(KeyCode::Enter));
    assert!(matches!(a.mode(), Mode::Gate { .. }));
}

#[test]
fn a_stale_file_is_not_a_gate_in_flight() {
    let l = run_live("merge", "asf-1", "test", "2026-11-06T11:00:00Z", vec![], "");
    assert!(l.is_fresh("2026-11-06T12:00:30Z"));
    assert!(
        !l.is_fresh("2026-11-06T12:05:00Z"),
        "a loop that stopped five minutes ago"
    );
    assert!(!l.is_fresh("not a time"));
}
