//! The Log view. Tests of it live here and nowhere else, so the task that rebuilds the view edits this
//! file only.

use super::*;
use crate::tui::snapshot::{text_width, LastRun};
use crate::tui::views::log::{self, Sym};

fn ev(seq: u64, ts: &str, kind: &str, issue: Option<&str>, payload: serde_json::Value) -> Event {
    Event {
        seq,
        ts: ts.into(),
        kind: kind.into(),
        issue_id: issue.map(id),
        run_id: issue.map(|i| format!("run-{i}")),
        actor: None,
        payload,
    }
}

/// A snapshot at [`NOW`] holding `events` (newest first, as the loader delivers them) and the issues
/// `asf-23` .. `asf-30`.
fn log_desk(events: Vec<Event>) -> Snapshot {
    let mut s = desk(vec![
        issue("asf-24", "Strip env", Status::InProgress),
        issue("asf-25", "Merge me", Status::Validating),
        issue("asf-29", "Reviewer approval survives retry", Status::InProgress),
    ]);
    s.events = events;
    s
}

fn last_run(i: &str, model: Option<&str>) -> LastRun {
    LastRun {
        issue: id(i),
        run_id: format!("run-{i}"),
        state: RunState::Working,
        cost: 0.0,
        budget: None,
        tool_calls: 0,
        started_at: "2026-11-06T11:00:00Z".into(),
        finished_at: None,
        model: model.map(Into::into),
        pane: None,
        session_name: None,
    }
}

/// `glyph sentence` of every shown event, newest first.
fn lines(s: &Snapshot) -> Vec<String> {
    log::entries(s)
        .iter()
        .map(|e| match e.sym {
            Sym::None => e.sentence(),
            sym => format!("{} {}", sym.glyph(), e.sentence()),
        })
        .collect()
}

fn gate(name: &str, ok: bool) -> GateResult {
    GateResult {
        name: name.into(),
        ok,
        exit_code: Some(i32::from(!ok)),
        duration_ms: 1,
        tail: String::new(),
        attempts: 1,
    }
}

fn log_app(s: Snapshot) -> App {
    let mut a = App::new(s);
    a.view = View::Log;
    a
}

fn words(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn row_with(text: &str, needle: &str) -> String {
    words(
        text.lines()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no row with {needle:?}:\n{text}")),
    )
}

#[test]
fn a_run_goes_from_started_to_verified_in_plain_sentences() {
    let mut s = log_desk(vec![
        ev(
            5,
            "2026-11-06T11:40:00Z",
            "run.state",
            Some("asf-24"),
            serde_json::json!({"from":"verifying","to":"succeeded","reason":"verified"}),
        ),
        ev(
            4,
            "2026-11-06T11:39:00Z",
            "run.state",
            Some("asf-24"),
            serde_json::json!({"from":"working","to":"finishing"}),
        ),
        ev(
            3,
            "2026-11-06T11:30:00Z",
            "run.state",
            Some("asf-24"),
            serde_json::json!({"from":"working","to":"blocked"}),
        ),
        ev(
            2,
            "2026-11-06T11:00:02Z",
            "run.state",
            Some("asf-24"),
            serde_json::json!({"from":"starting","to":"working"}),
        ),
        ev(
            1,
            "2026-11-06T11:00:01Z",
            "run.created",
            Some("asf-24"),
            serde_json::json!({"attempt":1,"runtime":"herdr"}),
        ),
    ]);
    s.last_runs = vec![last_run("asf-24", Some("sonnet"))];
    assert_eq!(
        lines(&s),
        [
            "✓ run verified · queued for merge",
            "● worker finished and handed over its report",
            "■ worker asked a question and is waiting",
            "● worker started · sonnet · asf-24-strip-env",
        ]
    );
}

#[test]
fn a_worker_that_could_not_start_is_not_reported_as_asking_a_question() {
    let s = log_desk(vec![
        ev(
            2,
            "2026-11-06T11:00:30Z",
            "run.state",
            Some("asf-24"),
            serde_json::json!({
                "from":"working","to":"blocked",
                "reason":"the agent did not come up\nscreen: login required"
            }),
        ),
        ev(
            1,
            "2026-11-06T11:00:10Z",
            "run.state",
            Some("asf-24"),
            serde_json::json!({"from":"starting","to":"blocked","reason":"start timed out"}),
        ),
    ]);
    assert_eq!(
        lines(&s),
        [
            "■ worker could not start · the agent did not come up",
            "■ worker could not start · start timed out",
        ]
    );
}

#[test]
fn a_verified_run_has_no_counts_because_no_event_carries_them() {
    // `run.state` is written as {from, to, reason} only (asf-store `runs.rs`); the operator dropped
    // the counts from the contract (#429), so the sentence never invents them
    let s = log_desk(vec![ev(
        1,
        NOW,
        "run.state",
        Some("asf-24"),
        serde_json::json!({"from":"verifying","to":"succeeded","reason":"verified"}),
    )]);
    assert_eq!(lines(&s), ["✓ run verified · queued for merge"]);
}

#[test]
fn a_failed_verification_shows_the_first_line_of_the_reason() {
    let s = log_desk(vec![ev(
        1,
        NOW,
        "run.state",
        Some("asf-24"),
        serde_json::json!({"to":"failed","reason":"gate `test` failed (exit Some(1)):\nmore output"}),
    )]);
    assert_eq!(
        lines(&s),
        ["✗ verification failed · gate `test` failed (exit Some(1)):"]
    );
    assert_eq!(log::entries(&s)[0].sym, Sym::Failed);
}

#[test]
fn a_start_retry_replaces_the_first_started_line() {
    let s = log_desk(vec![
        ev(
            3,
            NOW,
            "run.state",
            Some("asf-25"),
            serde_json::json!({"from":"starting","to":"working"}),
        ),
        ev(
            2,
            NOW,
            "run.start_retry",
            Some("asf-25"),
            serde_json::json!({"reason":"timeout"}),
        ),
        ev(
            1,
            NOW,
            "run.created",
            Some("asf-25"),
            serde_json::json!({"attempt":1}),
        ),
    ]);
    assert_eq!(
        lines(&s),
        ["● worker started · second attempt after a start timeout"]
    );
}

#[test]
fn passing_gates_after_the_rebase_are_one_line_with_the_gate_names() {
    let mut s = log_desk(vec![
        ev(
            3,
            NOW,
            "merge.state",
            Some("asf-25"),
            serde_json::json!({"merge_id":4,"from":"gating","to":"reviewing"}),
        ),
        ev(
            2,
            NOW,
            "merge.state",
            Some("asf-25"),
            serde_json::json!({"merge_id":4,"from":"rebasing","to":"gating"}),
        ),
        ev(
            1,
            NOW,
            "merge.state",
            Some("asf-25"),
            serde_json::json!({"merge_id":4,"from":"queued","to":"rebasing"}),
        ),
    ]);
    let mut item = merge_item(4, "asf-25", MergeState::Reviewing, None, NOW);
    item.gate_results = vec![gate("fmt", true), gate("clippy", true), gate("test", true)];
    s.queue = vec![item];
    assert_eq!(lines(&s), ["✓ gates passed after rebase · fmt, clippy, test"]);
}

#[test]
fn a_failing_gate_after_the_rebase_is_one_failure_line() {
    let mut s = log_desk(vec![
        ev(
            3,
            NOW,
            "merge.state",
            Some("asf-25"),
            serde_json::json!({"merge_id":4,"from":"gating","to":"failed"}),
        ),
        ev(
            2,
            NOW,
            "merge.state",
            Some("asf-25"),
            serde_json::json!({"merge_id":4,"from":"rebasing","to":"gating"}),
        ),
        ev(
            1,
            NOW,
            "merge.state",
            Some("asf-25"),
            serde_json::json!({"merge_id":4,"from":"queued","to":"rebasing"}),
        ),
    ]);
    let mut item = merge_item(4, "asf-25", MergeState::Failed, Some("gate `test` failed"), NOW);
    item.gate_results = vec![gate("fmt", true), gate("test", false)];
    s.queue = vec![item];
    assert_eq!(lines(&s), ["✗ merge failed · gate test after rebase"]);
}

#[test]
fn a_merge_that_failed_for_another_reason_says_why() {
    let mut s = log_desk(vec![ev(
        1,
        NOW,
        "merge.state",
        Some("asf-25"),
        serde_json::json!({"merge_id":4,"from":"rebasing","to":"failed"}),
    )]);
    s.queue = vec![merge_item(
        4,
        "asf-25",
        MergeState::Failed,
        Some("rebase conflict in src/lib.rs\nmore"),
        NOW,
    )];
    assert_eq!(lines(&s), ["✗ merge failed · rebase conflict in src/lib.rs"]);
}

#[test]
fn the_review_has_a_start_and_a_verdict() {
    let s = log_desk(vec![
        ev(
            4,
            NOW,
            "merge.sent_back",
            Some("asf-24"),
            serde_json::json!({"round":1,"file":"x"}),
        ),
        ev(
            3,
            NOW,
            "merge.sent_back",
            Some("asf-25"),
            serde_json::json!({"round":1,"file":"x"}),
        ),
        ev(
            2,
            NOW,
            "merge.review_approved",
            Some("asf-24"),
            serde_json::json!({"patch_id":"p"}),
        ),
        ev(
            1,
            NOW,
            "merge.review_started",
            Some("asf-24"),
            serde_json::json!({"after_comment":3,"round":2}),
        ),
    ]);
    assert_eq!(
        lines(&s),
        [
            "◆ review verdict: changes",
            "◆ review verdict: changes",
            "◆ review verdict: approve",
            "◆ review started · opus · round 2",
        ]
    );
}

/// A merge.state event as the queue writes it.
fn merge_state(seq: u64, merge_id: i64, from: &str, to: &str) -> Event {
    ev(
        seq,
        NOW,
        "merge.state",
        Some("asf-24"),
        serde_json::json!({"merge_id":merge_id,"from":from,"to":to}),
    )
}

#[test]
fn a_changes_verdict_that_is_not_sent_back_still_shows_as_a_verdict() {
    // `request_changes` fails the item without `merge.sent_back` when the worker is still running or
    // `review.max_rounds` is reached: the failed state is then the only trace of the verdict
    let mut s = log_desk(vec![
        merge_state(3, 1, "reviewing", "failed"),
        ev(
            2,
            NOW,
            "merge.review_started",
            Some("asf-24"),
            serde_json::json!({"after_comment":3,"round":1}),
        ),
    ]);
    s.queue = vec![merge_item(
        1,
        "asf-24",
        MergeState::Failed,
        Some("review asks for changes on feat/asf-24:\n- one\n- two"),
        NOW,
    )];
    assert_eq!(
        lines(&s),
        ["◆ review verdict: changes", "◆ review started · opus · round 1"]
    );
}

#[test]
fn a_changes_verdict_that_was_sent_back_is_one_line_not_two() {
    // the failed state is written first and `merge.sent_back` right after it
    let s = log_desk(vec![
        ev(
            4,
            NOW,
            "merge.sent_back",
            Some("asf-24"),
            serde_json::json!({"round":1,"file":"x"}),
        ),
        merge_state(3, 1, "reviewing", "failed"),
        ev(
            2,
            NOW,
            "run.state",
            Some("asf-24"),
            serde_json::json!({"from":"succeeded","to":"working","reason":"sent back"}),
        ),
    ]);
    let verdicts = lines(&s)
        .into_iter()
        .filter(|l| l.contains("review verdict"))
        .count();
    assert_eq!(verdicts, 1, "{:?}", lines(&s));
}

#[test]
fn a_failure_sent_back_to_the_worker_is_a_sentence_not_a_review_verdict() {
    // a gate or a conflict goes back under its own event kind: the Log must not call it a review
    let s = log_desk(vec![
        ev(
            2,
            NOW,
            "run.fix_sent_back",
            Some("asf-24"),
            serde_json::json!({
                "kind":"conflict","round":2,"file":"x","head":"abc",
                "headline":"the rebase of feat/asf-24 onto main conflicts"
            }),
        ),
        ev(
            1,
            NOW,
            "run.fix_sent_back",
            Some("asf-24"),
            serde_json::json!({
                "kind":"gate","round":1,"file":"x","head":"abc",
                "headline":"after the rebase onto abc the gate `clippy` failed"
            }),
        ),
    ]);
    let lines = lines(&s);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines[1].ends_with(
            "after the rebase onto abc the gate `clippy` failed, sent back to the worker (round 1)"
        ),
        "{lines:?}"
    );
    assert!(
        lines[0]
            .ends_with("the rebase of feat/asf-24 onto main conflicts, sent back to the worker (round 2)"),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .all(|l| !l.contains("review verdict") && !l.contains("run.fix_sent_back")),
        "{lines:?}"
    );

    // an event without a headline still reads as a sentence
    let s = log_desk(vec![ev(
        1,
        NOW,
        "run.fix_sent_back",
        Some("asf-24"),
        serde_json::json!({"kind":"gate","round":1}),
    )]);
    let lines = self::lines(&s);
    assert!(
        lines[0].ends_with("sent back to the worker to fix a gate (round 1)"),
        "{lines:?}"
    );
}

#[test]
fn a_review_that_failed_for_another_reason_is_a_failed_merge() {
    let mut s = log_desk(vec![merge_state(2, 1, "reviewing", "failed")]);
    s.queue = vec![merge_item(
        1,
        "asf-24",
        MergeState::Failed,
        Some("worktree .worktrees/asf-24 does not exist"),
        NOW,
    )];
    assert_eq!(
        lines(&s),
        ["✗ merge failed · worktree .worktrees/asf-24 does not exist"]
    );
}

#[test]
fn a_reviewers_own_run_and_decision_comment_repeat_the_review_lines() {
    let mut s = log_desk(vec![
        ev(
            3,
            NOW,
            "comment.added",
            Some("asf-24"),
            serde_json::json!({"comment_id":9,"type":"DECISION","to":"human"}),
        ),
        ev(
            2,
            NOW,
            "run.state",
            Some("asf-30"),
            serde_json::json!({"to":"finishing"}),
        ),
        ev(
            1,
            NOW,
            "run.created",
            Some("asf-30"),
            serde_json::json!({"attempt":1}),
        ),
    ]);
    s.events[0].actor = Some("agent:asf-30".into());
    let mut review = issue("asf-30", "Review asf-24", Status::InProgress);
    review.kind = IssueType::Review;
    review.parent_id = Some(id("asf-24"));
    s.issues.push(review);
    assert_eq!(lines(&s), Vec::<String>::new());
}

#[test]
fn a_merge_names_the_branch_and_the_short_sha_and_never_claims_main_is_green() {
    let mut s = log_desk(vec![ev(
        1,
        NOW,
        "merge.merged",
        Some("asf-24"),
        serde_json::json!({"head":"9c1e4aaf00112233","branch":"feat/asf-24","pr_url":null}),
    )]);
    s.branch = "main".into();
    assert_eq!(lines(&s), ["✓ merged into main · 9c1e4aa"]);
    // the main gates run after `merge.merged` is written and only a red run writes an event, so
    // there is no positive signal to say "green" from
    s.verify_main_after = true;
    assert_eq!(lines(&s), ["✓ merged into main · 9c1e4aa"]);
    s.events.insert(
        0,
        ev(
            2,
            NOW,
            "merge.main_red",
            Some("asf-24"),
            serde_json::json!({"head":"9c1e4aa","branch":"feat/asf-24","gate":"test","exit_code":1}),
        ),
    );
    assert_eq!(
        lines(&s),
        [
            "■ main is red after the merge · gate test",
            "✓ merged into main · 9c1e4aa"
        ]
    );
}

#[test]
fn a_plan_import_is_one_line_per_batch_with_no_issue_column() {
    let mut s = log_desk(vec![
        ev(
            4,
            "2026-11-06T11:00:03Z",
            "issue.created",
            Some("asf-26"),
            serde_json::json!({"title":"c"}),
        ),
        ev(
            3,
            "2026-11-06T11:00:02Z",
            "issue.created",
            Some("asf-25"),
            serde_json::json!({"title":"b"}),
        ),
        ev(
            2,
            "2026-11-06T11:00:01Z",
            "issue.created",
            Some("asf-24"),
            serde_json::json!({"title":"a"}),
        ),
        ev(
            1,
            "2026-11-06T11:00:00Z",
            "issue.created",
            Some("asf-23"),
            serde_json::json!({"title":"Plan"}),
        ),
    ]);
    let mut epic = issue("asf-23", "Plan", Status::Backlog);
    epic.kind = IssueType::Epic;
    epic.source = Some("plan.md#epic".into());
    s.issues.push(epic);
    for (n, i) in ["asf-24", "asf-25", "asf-26"].into_iter().enumerate() {
        let mut t = issue(i, "t", Status::Backlog);
        t.parent_id = Some(id("asf-23"));
        t.source = Some(format!("plan.md#task-{}", n + 1));
        s.issues.retain(|x| x.id != t.id);
        s.issues.push(t);
    }
    assert_eq!(lines(&s), ["plan imported: epic asf-23 with 3 issues"]);
    let e = &log::entries(&s)[0];
    assert_eq!((e.sym, e.issue.clone()), (Sym::None, None));
}

#[test]
fn a_hand_made_issue_is_a_line_of_its_own() {
    let s = log_desk(vec![ev(
        1,
        NOW,
        "issue.created",
        Some("asf-24"),
        serde_json::json!({"title":"Strip env"}),
    )]);
    assert_eq!(lines(&s), ["○ issue created · Strip env"]);
}

#[test]
fn claims_and_status_comments_that_repeat_another_line_are_hidden() {
    let s = log_desk(vec![
        ev(
            4,
            NOW,
            "comment.added",
            Some("asf-24"),
            serde_json::json!({"comment_id":1,"type":"STATUS","to":"all"}),
        ),
        ev(
            3,
            NOW,
            "comment.added",
            Some("asf-24"),
            serde_json::json!({"comment_id":2,"type":"FYI","to":"all"}),
        ),
        ev(
            2,
            NOW,
            "claim.taken",
            Some("asf-24"),
            serde_json::json!({"worker":"asf-24"}),
        ),
        ev(1, NOW, "claim.released", Some("asf-24"), serde_json::json!({})),
    ]);
    assert!(lines(&s).is_empty());
}

#[test]
fn a_question_comment_is_not_hidden() {
    let mut s = log_desk(vec![ev(
        1,
        NOW,
        "comment.added",
        Some("asf-24"),
        serde_json::json!({"comment_id":2,"type":"QUESTION","to":"human"}),
    )]);
    s.events[0].actor = Some("agent:asf-24".into());
    assert_eq!(lines(&s), ["■ agent:asf-24 asked a question"]);
}

#[test]
fn an_unknown_kind_shows_the_kind_and_a_compact_payload() {
    let s = log_desk(vec![
        ev(
            2,
            NOW,
            "foo.bar",
            Some("asf-24"),
            serde_json::json!({"a":1,"b":"x"}),
        ),
        ev(1, NOW, "foo.empty", None, serde_json::json!({})),
    ]);
    assert_eq!(lines(&s), [r#"○ foo.bar {"a":1,"b":"x"}"#, "○ foo.empty"]);
}

/// The rows of the mockup, newest first.
fn mockup() -> Snapshot {
    let mut s = desk(vec![
        issue("asf-24", "Reviewer approval", Status::InProgress),
        issue("asf-25", "Merge me", Status::Validating),
        issue("asf-27", "Add `asf issue scope add/rm`", Status::InProgress),
        issue("asf-28", "Reload config", Status::InProgress),
        issue("asf-29", "Reviewer approval survives retry", Status::InProgress),
    ]);
    s.events = vec![
        ev(
            9,
            "2026-11-06T12:41:07Z",
            "run.state",
            Some("asf-27"),
            serde_json::json!({"from":"working","to":"blocked"}),
        ),
        ev(
            8,
            "2026-11-06T12:39:52Z",
            "merge.review_started",
            Some("asf-28"),
            serde_json::json!({"round":1}),
        ),
        ev(
            7,
            "2026-11-06T12:39:50Z",
            "merge.state",
            Some("asf-28"),
            serde_json::json!({"merge_id":2,"from":"rebasing","to":"gating"}),
        ),
        ev(
            6,
            "2026-11-06T12:37:11Z",
            "merge.state",
            Some("asf-25"),
            serde_json::json!({"merge_id":1,"from":"gating","to":"failed"}),
        ),
        ev(
            5,
            "2026-11-06T12:33:02Z",
            "run.created",
            Some("asf-29"),
            serde_json::json!({"attempt":1}),
        ),
        ev(
            4,
            "2026-11-06T12:08:03Z",
            "issue.created",
            Some("asf-24"),
            serde_json::json!({"title":"x"}),
        ),
    ];
    // the gating event of asf-25 sits between the failure and the start; keep the order newest first
    s.events.insert(
        4,
        ev(
            55,
            "2026-11-06T12:36:00Z",
            "merge.state",
            Some("asf-25"),
            serde_json::json!({"merge_id":1,"from":"rebasing","to":"gating"}),
        ),
    );
    let mut a = merge_item(1, "asf-25", MergeState::Failed, None, NOW);
    a.gate_results = vec![gate("test", false)];
    let mut b = merge_item(2, "asf-28", MergeState::Reviewing, None, NOW);
    b.gate_results = vec![gate("fmt", true), gate("clippy", true), gate("test", true)];
    s.queue = vec![a, b];
    s.last_runs = vec![last_run("asf-29", Some("sonnet"))];
    s
}

#[test]
fn the_rows_follow_the_mockup_at_120_columns() {
    let a = log_app(mockup());
    let t = render_to_text(&a, 120, 30);
    assert_eq!(
        row_with(&t, "12:41:07"),
        "▸ 12:41:07 asf-27 ■ worker asked a question and is waiting"
    );
    assert_eq!(
        row_with(&t, "12:39:52"),
        "12:39:52 asf-28 ◆ review started · opus · round 1"
    );
    assert_eq!(
        row_with(&t, "12:39:50"),
        "12:39:50 asf-28 ✓ gates passed after rebase · fmt, clippy, test"
    );
    assert_eq!(
        row_with(&t, "12:36:00"),
        "12:36:00 asf-25 ✗ merge failed · gate test after rebase"
    );
    assert_eq!(
        row_with(&t, "12:33:02"),
        "12:33:02 asf-29 ● worker started · sonnet · asf-29-reviewer-approval-survives-retry"
    );
    assert_eq!(
        t.lines().filter(|l| l.contains("✗ merge failed")).count(),
        1,
        "{t}"
    );
    let header = t.lines().find(|l| l.contains("showing")).unwrap();
    assert_eq!(
        words(header),
        "showing all issues · newest first · / filter by issue or text"
    );
}

#[test]
fn the_rows_fit_100_columns_and_cut_long_sentences_with_an_ellipsis() {
    let a = log_app(mockup());
    let t = render_to_text(&a, 100, 30);
    for line in t.lines() {
        assert!(text_width(line) <= 100, "{line:?}");
    }
    let started = row_with(&t, "12:33:02");
    assert!(
        started.starts_with("12:33:02 asf-29 ● worker started · sonnet"),
        "{started}"
    );
    let t = render_to_text(&a, 80, 20);
    for line in t.lines() {
        assert!(text_width(line) <= 80, "{line:?}");
    }
    assert!(t.contains('…'), "{t}");
}

#[test]
fn an_empty_log_says_what_will_appear_there() {
    let a = log_app(Snapshot::default());
    let t = render_to_text(&a, 100, 30);
    assert!(
        t.contains("Nothing has happened yet. Events appear here as the factory works."),
        "{t}"
    );
}

#[test]
fn the_footer_shows_the_five_keys_of_the_view_then_help_and_quit() {
    let a = log_app(mockup());
    assert_eq!(
        words(&footer_line(&a, 120)),
        "j / k move g / G newest / oldest / filter Enter open issue J raw event ? help q quit"
    );
}

#[test]
fn j_k_g_and_capital_g_move_between_newest_and_oldest() {
    let mut a = log_app(mockup());
    let last = log::rows(&a.snap) - 1;
    assert_eq!(a.selected(View::Log), 0);
    a.handle_key(key('G'));
    assert_eq!(a.selected(View::Log), last);
    a.handle_key(key('g'));
    assert_eq!(a.selected(View::Log), 0);
    a.handle_key(key('j'));
    a.handle_key(key('j'));
    a.handle_key(key('k'));
    assert_eq!(a.selected(View::Log), 1);
    let t = render_to_text(&a, 120, 30);
    assert!(row_with(&t, "12:39:52").starts_with("▸ 12:39:52"), "{t}");
}

#[test]
fn enter_opens_the_issue_of_the_selected_event_on_the_board() {
    let mut a = log_app(mockup());
    assert_eq!(a.handle_key(code(KeyCode::Enter)), Action::None);
    assert_eq!((a.view, a.selected_issue()), (View::Board, Some(id("asf-27"))));
}

#[test]
fn enter_on_an_event_without_an_issue_says_so() {
    let mut s = log_desk(vec![ev(1, NOW, "foo.empty", None, serde_json::json!({}))]);
    s.issues.clear();
    let mut a = log_app(s);
    a.handle_key(code(KeyCode::Enter));
    assert_eq!(a.view, View::Log);
    assert_eq!(a.status.as_deref(), Some("this event belongs to no issue"));
}

#[test]
fn slash_filters_by_issue_id_with_every_key_taken_as_text() {
    let mut a = log_app(mockup());
    a.handle_key(key('/'));
    // `a`, `q`, `t`, digits and `-` are bindings elsewhere; while typing they are text
    for c in "asf-2".chars() {
        assert_eq!(a.handle_key(key(c)), Action::None);
    }
    assert_eq!(a.view, View::Log);
    a.handle_key(key('5'));
    let t = render_to_text(&a, 120, 30);
    assert_eq!(
        words(t.lines().find(|l| l.contains("showing")).unwrap()),
        "showing asf-25▏ · 1 of 6 events · Esc clears"
    );
    assert!(row_with(&t, "12:36:00").contains("asf-25"), "{t}");
    assert!(!t.contains("12:41:07"), "{t}");
    // Enter keeps the filter and gives the keys back
    a.handle_key(code(KeyCode::Enter));
    assert_eq!(log::filter(&a.log), "asf-25");
    let t = render_to_text(&a, 120, 30);
    assert_eq!(
        words(t.lines().find(|l| l.contains("showing")).unwrap()),
        "showing asf-25 · 1 of 6 events · Esc clears"
    );
    assert_eq!(a.handle_key(key('q')), Action::Quit);
}

#[test]
fn slash_also_matches_the_text_of_the_sentence() {
    let mut a = log_app(mockup());
    a.handle_key(key('/'));
    for c in "ROUND".chars() {
        a.handle_key(key(c));
    }
    let t = render_to_text(&a, 120, 30);
    assert_eq!(
        words(t.lines().find(|l| l.contains("showing")).unwrap()),
        "showing ROUND▏ · 1 of 6 events · Esc clears"
    );
    assert!(t.contains("review started"), "{t}");
}

#[test]
fn backspace_and_ctrl_u_edit_the_filter_and_esc_clears_it() {
    let mut a = log_app(mockup());
    a.handle_key(key('/'));
    for c in "asf-27".chars() {
        a.handle_key(key(c));
    }
    a.handle_key(code(KeyCode::Backspace));
    assert_eq!(log::filter(&a.log), "asf-2");
    a.handle_key(ctrl('u'));
    assert_eq!(log::filter(&a.log), "");
    a.handle_key(key('x'));
    a.handle_key(code(KeyCode::Enter));
    assert_eq!(log::filter(&a.log), "x");
    assert!(!a.log.takes_every_key());
    a.handle_key(code(KeyCode::Esc));
    assert_eq!(log::filter(&a.log), "");
    let t = render_to_text(&a, 120, 30);
    assert!(t.contains("showing all issues"), "{t}");
}

#[test]
fn a_filter_nothing_matches_says_so() {
    let mut a = log_app(mockup());
    a.handle_key(key('/'));
    for c in "zzz".chars() {
        a.handle_key(key(c));
    }
    let t = render_to_text(&a, 120, 30);
    assert!(
        t.contains("Nothing matches the filter. Press Esc to clear it."),
        "{t}"
    );
    assert!(t.contains("0 of 6 events"), "{t}");
}

#[test]
fn capital_j_shows_the_raw_json_of_the_selected_event_in_a_modal() {
    let mut a = log_app(mockup());
    a.handle_key(key('J'));
    let t = render_to_text(&a, 120, 30);
    assert!(t.contains("\"kind\": \"run.state\""), "{t}");
    assert!(t.contains("\"to\": \"blocked\""), "{t}");
    assert!(t.contains("\"seq\": 9"), "{t}");
    // the keys do not move the list behind the modal
    a.handle_key(key('j'));
    assert_eq!(a.selected(View::Log), 0);
    a.handle_key(key('J'));
    let t = render_to_text(&a, 120, 30);
    assert!(!t.contains("\"kind\""), "{t}");
    a.handle_key(key('J'));
    a.handle_key(code(KeyCode::Esc));
    assert!(!render_to_text(&a, 120, 30).contains("\"kind\""));
}

#[test]
fn while_the_raw_window_is_open_every_key_goes_to_the_log() {
    let mut a = log_app(mockup());
    a.handle_key(key('J'));
    // q does not quit the TUI behind the window
    assert!(!matches!(a.handle_key(key('q')), Action::Quit));
    // a digit does not switch the view
    a.handle_key(key('2'));
    assert_eq!(a.view, View::Log);
    assert!(render_to_text(&a, 120, 30).contains("\"kind\""));
    // J (or Esc) closes it, and then the global keys work again
    a.handle_key(key('J'));
    a.handle_key(key('2'));
    assert_eq!(a.view, View::Board);
}

#[test]
fn the_selected_row_is_highlighted_and_reversed_without_colours() {
    let a = log_app(mockup());
    let buf = render_to_buffer(&a, 120, 30);
    let row = (0..30u16)
        .find(|y| {
            (0..120u16)
                .map(|x| buf[(x, *y)].symbol().to_string())
                .collect::<String>()
                .contains("12:41:07")
        })
        .unwrap();
    assert!(buf[(60, row)].bg != Color::Reset, "a highlighted background");
    let mut plain = log_app(mockup());
    plain.theme = Theme::new(false);
    let buf = render_to_buffer(&plain, 120, 30);
    assert!(buf[(60, row)].modifier.contains(Modifier::REVERSED));
}

#[test]
fn the_symbols_carry_the_meaning_and_details_are_dim() {
    let a = log_app(mockup());
    assert!(
        cell_style(&a, "12:39:52", "·").1.contains(Modifier::DIM),
        "details are secondary"
    );
    assert_eq!(
        cell_style(&a, "12:36:00", "✗").1,
        Modifier::BOLD | Modifier::empty()
    );
    assert_eq!(cell_style(&a, "12:33:02", "●").0, Color::Green);
}

#[test]
fn every_key_of_the_view_is_on_the_help_page() {
    let mut a = log_app(mockup());
    a.handle_key(key('?'));
    let t = render_to_text(&a, 120, 70);
    for text in [
        "newest / oldest",
        "filter the log by issue id or text",
        "raw JSON",
    ] {
        assert!(t.contains(text), "{text}:\n{t}");
    }
}
