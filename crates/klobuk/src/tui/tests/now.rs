//! NEEDS YOU keeps one row per issue and shows the one thing the human can do now (asf-96).

use super::*;

const REVIEW_ERROR: &str = "review asks for changes on feat/asf-65:\n- handle the empty queue";
const GATE_ERROR: &str = "gate `test` failed (exit Some(101)) after rebase:\nthread panicked";

/// asf-65 as on the 2026-11-08 screenshot: a merge item that failed 15 h ago (the reviewer asked for
/// changes) and a run that failed 12 h ago (the worker's fix did not pass the gates).
fn failed_merge_and_failed_run() -> Snapshot {
    let mut s = desk(live(&["asf-65"]));
    s.queue.push(merge_item(
        1,
        "asf-65",
        MergeState::Failed,
        Some(REVIEW_ERROR),
        "2026-11-05T21:00:00Z",
    ));
    let mut failed = run_at("asf-65", RunState::Failed, "2026-11-05T22:00:00Z");
    failed.finished_at = Some("2026-11-06T00:00:00Z".into());
    failed.exit_reason = Some("gate `test` failed (exit None)".into());
    s.failed_runs.push(failed);
    s
}

fn need_rows(s: &Snapshot) -> Vec<(String, NeedSource, Act)> {
    needs(s)
        .into_iter()
        .map(|n| (n.issue.to_string(), n.source, n.act))
        .collect()
}

#[test]
fn an_issue_with_a_failed_merge_item_and_a_failed_run_shows_one_row_with_verify() {
    let s = failed_merge_and_failed_run();
    let t = render_to_text(&App::new(s.clone()), 120, 30);
    assert!(t.contains("NEEDS YOU (1)"), "{t}");
    assert_eq!(t.matches("asf-65 ").count(), 1, "one row for the issue:\n{t}");
    let row = row_of(&t, "asf-65");
    assert!(
        row.contains("failed") && row.contains("gate `test` failed"),
        "{row:?}"
    );
    assert!(row.trim_end().ends_with("v verify"), "{row:?}");
    assert!(
        !row.contains("r retry") && !row.contains("merge failed"),
        "{row:?}"
    );
    assert!(
        line_of(&t, 0).contains("1 waiting"),
        "the header counts rows:\n{t}"
    );
}

#[test]
fn the_row_is_as_old_as_the_oldest_thing_that_waits_on_the_issue() {
    let s = failed_merge_and_failed_run();
    let row = needs(&s).remove(0);
    assert_eq!(
        row.since, "2026-11-05T21:00:00Z",
        "the merge item is the older one"
    );
    let t = render_to_text(&App::new(s), 120, 30);
    assert!(row_of(&t, "asf-65").contains("15 h"), "{t}");
}

#[test]
fn once_the_run_is_verified_the_merge_item_is_what_is_left_and_the_row_says_retry() {
    let mut s = failed_merge_and_failed_run();
    s.failed_runs.clear(); // `v` verified the run: it succeeded and is no longer a failed one
    let t = render_to_text(&App::new(s.clone()), 120, 30);
    assert!(t.contains("NEEDS YOU (1)"), "{t}");
    let row = row_of(&t, "asf-65");
    assert!(
        row.contains("merge failed") && row.contains("review asks for changes"),
        "{row:?}"
    );
    assert!(row.trim_end().ends_with("r retry"), "{row:?}");
    let mut a = App::new(s);
    a.view = View::Now;
    assert_eq!(a.handle_key(key('r')), Action::Retry(id("asf-65")));
}

#[test]
fn the_key_on_the_single_row_is_the_one_that_works_for_it() {
    let mut a = App::new(failed_merge_and_failed_run());
    a.view = View::Now;
    assert_eq!(a.handle_key(key('v')), Action::Verify(id("asf-65")));
    // `r` is refused on the run row, and the status line says which key works
    assert_eq!(a.handle_key(key('r')), Action::None);
    let status = a.status.clone().unwrap_or_default();
    assert!(status.contains("this row takes v verify"), "{status}");
}

#[test]
fn a_blocked_run_beats_a_question_and_a_question_beats_a_failed_run_and_a_failed_run_beats_a_merge() {
    // blocked run + question: the prompt is the thing to answer first
    let mut s = desk(live(&["asf-1"]));
    s.runs
        .push(run_at("asf-1", RunState::Blocked, "2026-11-06T11:30:00Z"));
    s.events
        .push(state_event(1, "asf-1", "blocked", "2026-11-06T11:50:00Z"));
    s.questions
        .push(question(3, "asf-1", "Keep the flag?", "2026-11-06T11:40:00Z"));
    assert_eq!(
        need_rows(&s),
        [("asf-1".into(), NeedSource::BlockedRun, Act::Answer)]
    );
    assert_eq!(needs(&s)[0].since, "2026-11-06T11:40:00Z", "aged by the question");

    // question + failed run
    let mut s = desk(live(&["asf-2"]));
    s.questions
        .push(question(4, "asf-2", "Which crate?", "2026-11-06T11:00:00Z"));
    let mut failed = run_at("asf-2", RunState::Failed, "2026-11-06T09:00:00Z");
    failed.finished_at = Some("2026-11-06T10:00:00Z".into());
    s.failed_runs.push(failed);
    assert_eq!(
        need_rows(&s),
        [("asf-2".into(), NeedSource::Question(4), Act::Answer)]
    );
    assert_eq!(needs(&s)[0].since, "2026-11-06T10:00:00Z", "aged by the run");

    // question + failed merge item (no run at all)
    let mut s = desk(live(&["asf-3"]));
    s.questions
        .push(question(5, "asf-3", "Rebase or merge?", "2026-11-06T11:00:00Z"));
    s.queue.push(merge_item(
        1,
        "asf-3",
        MergeState::Blocked,
        None,
        "2026-11-06T08:00:00Z",
    ));
    assert_eq!(
        need_rows(&s),
        [("asf-3".into(), NeedSource::Question(5), Act::Answer)]
    );
    assert_eq!(needs(&s)[0].since, "2026-11-06T08:00:00Z");
}

#[test]
fn several_questions_of_one_issue_make_one_row_for_the_oldest() {
    let mut s = desk(live(&["asf-1"]));
    s.questions
        .push(question(7, "asf-1", "First?", "2026-11-06T11:00:00Z"));
    s.questions
        .push(question(8, "asf-1", "Second?", "2026-11-06T11:30:00Z"));
    assert_eq!(
        need_rows(&s),
        [("asf-1".into(), NeedSource::Question(7), Act::Answer)]
    );
}

#[test]
fn different_issues_keep_their_own_rows_oldest_first() {
    let mut s = failed_merge_and_failed_run();
    s.issues
        .push(issue("asf-40", "Title of asf-40", Status::InProgress));
    s.queue.push(merge_item(
        2,
        "asf-40",
        MergeState::Failed,
        Some("rebase failed: boom"),
        "2026-11-06T09:00:00Z",
    ));
    let order: Vec<String> = needs(&s).iter().map(|n| n.issue.to_string()).collect();
    assert_eq!(order, ["asf-65", "asf-40"]);
}

#[test]
fn a_failed_merge_item_of_an_issue_with_an_active_run_is_not_listed_it_is_running() {
    for (error, label) in [
        (REVIEW_ERROR, "fixing after review"),
        (GATE_ERROR, "fixing after gate"),
    ] {
        let mut s = desk(live(&["asf-98"]));
        s.queue.push(merge_item(
            1,
            "asf-98",
            MergeState::Failed,
            Some(error),
            "2026-11-06T11:00:00Z",
        ));
        s.runs
            .push(run_at("asf-98", RunState::Working, "2026-11-06T11:10:00Z"));
        assert!(needs(&s).is_empty(), "{error}: the worker is fixing it");
        let t = render_to_text(&App::new(s), 120, 30);
        assert!(t.contains("NEEDS YOU (0)") && t.contains("RUNNING (1)"), "{t}");
        let row = row_of(&t, "asf-98");
        assert!(row.contains(label), "{label:?} missing in {row:?}");
        // the label fills the whole state column, so the gap before the title must still be there
        assert!(
            row.contains(&format!("{label:<19}  Title of asf-98")),
            "the title stays readable and apart from the state: {row:?}"
        );
        assert!(!row.contains("r retry"), "{row:?}");
    }
}

#[test]
fn a_blocked_merge_item_with_an_active_run_is_not_listed_either() {
    let mut s = desk(live(&["asf-9"]));
    s.queue.push(merge_item(
        1,
        "asf-9",
        MergeState::Blocked,
        None,
        "2026-11-06T11:00:00Z",
    ));
    s.runs
        .push(run_at("asf-9", RunState::Working, "2026-11-06T11:10:00Z"));
    assert!(needs(&s).is_empty());
    let t = render_to_text(&App::new(s), 120, 30);
    assert!(
        row_of(&t, "asf-9").contains("working"),
        "no cause is known, so plain {t}"
    );
}

#[test]
fn a_working_run_without_a_failed_merge_item_is_just_working() {
    let t = render_to_text(&App::new(working_desk()), 120, 30);
    let row = row_of(&t, "asf-24");
    assert!(row.contains("working") && !row.contains("fixing"), "{row:?}");
}

#[test]
fn a_run_that_is_finishing_after_a_send_back_keeps_its_own_state_word() {
    let mut s = desk(live(&["asf-98"]));
    s.queue.push(merge_item(
        1,
        "asf-98",
        MergeState::Failed,
        Some(REVIEW_ERROR),
        "2026-11-06T11:00:00Z",
    ));
    s.runs
        .push(run_at("asf-98", RunState::Verifying, "2026-11-06T11:10:00Z"));
    assert!(needs(&s).is_empty());
    let t = render_to_text(&App::new(s), 120, 30);
    assert!(row_of(&t, "asf-98").contains("verifying"), "{t}");
}

#[test]
fn a_failed_merge_item_of_a_closed_issue_waits_for_nobody() {
    let mut s = desk(vec![issue("asf-5", "Title of asf-5", Status::Done)]);
    s.queue.push(merge_item(
        1,
        "asf-5",
        MergeState::Failed,
        Some("boom"),
        "2026-11-06T11:00:00Z",
    ));
    assert!(needs(&s).is_empty());
}

#[test]
fn the_overdue_mark_follows_the_chosen_row_and_its_age() {
    let mut a = App::new(failed_merge_and_failed_run());
    a.snap.config.iter_mut().for_each(|r| {
        if r.key == "notify.after_minutes" {
            r.raw = "60".into();
        }
    });
    let t = render_to_text(&a, 120, 30);
    assert!(
        row_of(&t, "asf-65").contains("! 15 h"),
        "waited 15 h, past 60 min:\n{t}"
    );
}
