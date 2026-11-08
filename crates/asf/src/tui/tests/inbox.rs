//! The Inbox view. Tests of it live here and nowhere else, so the task that rebuilds the view edits
//! this file only.

use super::*;
use crate::tui::snapshot::text_width;
use crate::tui::views::inbox;

fn at(mut c: Comment, id: i64, ts: &str) -> Comment {
    c.id = id;
    c.created_at = ts.into();
    c
}

/// The merge queue's BLOCKER body after a gate failed on the rebased branch (`commands/merge.rs`).
const MERGE_BODY: &str = "gate `test` failed (exit Some(101)) after rebase:\n\
test start_retry_once ... FAILED\n\
test start_gives_up_after_second_timeout ... FAILED\n\
\n\
failures:\n\
\x20\x20\x20\x20start_retry_once\n\
\x20\x20\x20\x20start_gives_up_after_second_timeout\n\
\n\
test result: FAILED. 8 passed; 2 failed; 0 ignored\n\
\n\
Fix and `asf merge retry asf-25`.";

const SCREEN_BODY: &str = "Worker for asf-27 is waiting for input.\n\nScreen:\n```\nKeep the old --scope flag as an alias?\n 1. Yes\n 2. No\n```\nAnswer with `asf run keys asf-27 y`.";

/// The verification BLOCKER of a run (`worker.rs`): the gate that failed, its output, the way out.
const VERIFY_BODY: &str = "Verification of asf-3 failed:\n\
gate `clippy` failed (exit Some(101)):\n\
warning: unused variable: `x`\n\
error: could not compile `asf` (lib) due to 1 previous error\n\
\n\
Fix in the worktree (.worktrees/asf-3) and re-run `asf issue finish`, or `asf run abort asf-3`.";

/// The desk of the approved mockup: a failed merge (12 min) and a blocked worker (4 min) need the
/// human, four comments are plain information, and both BLOCKERs are unread too (6 unread in all).
fn mockup_desk() -> Snapshot {
    let mut s = desk(vec![
        issue("asf-24", "Strip env", Status::Done),
        issue("asf-25", "Retry once", Status::InProgress),
        issue("asf-27", "Scope alias", Status::InProgress),
        issue("asf-28", "Wire up", Status::Done),
        issue("asf-29", "Fingerprint", Status::Done),
    ]);
    s.queue.push(merge_item(
        1,
        "asf-25",
        MergeState::Failed,
        Some(MERGE_BODY),
        "2026-11-06T11:48:00Z",
    ));
    s.runs
        .push(run_at("asf-27", RunState::Blocked, "2026-11-06T11:30:00Z"));
    s.events
        .push(state_event(1, "asf-27", "blocked", "2026-11-06T11:56:00Z"));
    let merged = at(
        comment(
            "asf-24",
            CommentType::Fyi,
            "feat/asf-24 merged into main at 9c1e4aa0f3d2b1c4a5e6f708192a3b4c5d6e7f80 (asf-24 done)",
        ),
        1,
        "2026-11-06T11:42:00Z",
    );
    let review = at(
        comment(
            "asf-24",
            CommentType::Decision,
            "review: approve (opus, round 1)\nno findings",
        ),
        2,
        "2026-11-06T11:39:00Z",
    );
    let status = at(
        comment(
            "asf-28",
            CommentType::Status,
            "finished; report saved to .factory/runs/asf-28/report.md",
        ),
        3,
        "2026-11-06T11:35:00Z",
    );
    let decision = at(
        comment(
            "asf-29",
            CommentType::Decision,
            "fingerprint uses `git patch-id --stable`, stored in the merge event",
        ),
        4,
        "2026-11-06T11:29:00Z",
    );
    let merge_blocker = at(
        comment("asf-25", CommentType::Blocker, MERGE_BODY),
        5,
        "2026-11-06T11:48:00Z",
    );
    let screen_blocker = at(
        comment("asf-27", CommentType::Blocker, SCREEN_BODY),
        6,
        "2026-11-06T11:56:00Z",
    );
    s.blockers = vec![merge_blocker.clone(), screen_blocker.clone()];
    s.inbox = vec![merged, review, status, decision, merge_blocker, screen_blocker];
    s.derive();
    s
}

fn view_of(s: Snapshot) -> App {
    let mut a = App::new(s);
    a.view = View::Inbox;
    a
}

fn mockup_app() -> App {
    view_of(mockup_desk())
}

fn lines_of(t: &str) -> Vec<&str> {
    t.lines().collect()
}

fn line_no(text: &str, needle: &str) -> usize {
    text.lines()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no row with {needle:?}:\n{text}"))
}

fn opened_comment_id(a: &App) -> i64 {
    match a.mode() {
        Mode::Comment { comment, .. } => comment.id,
        other => panic!("expected the comment modal, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------
// The layout of the mockup
// ---------------------------------------------------------------------------------------------

#[test]
fn the_default_view_is_the_mockup_needs_you_then_for_your_information() {
    for width in [100u16, 120] {
        let t = render_to_text(&mockup_app(), width, 30);
        let l = lines_of(&t);
        let head = l[line_no(&t, "NEEDS YOU (2)")];
        assert!(head.starts_with(" NEEDS YOU (2)"), "{width}: {head:?}");
        assert!(
            head.trim_end().ends_with("A show everything (6)"),
            "{width}: the toggle sits at the right end of the title row: {head:?}"
        );
        // the oldest thing first (the snapshot's order); the selection marker is on the first row
        let first = line_no(&t, "asf-25  BLOCKER");
        assert_eq!(
            l[first].trim_end(),
            " ▸ ■ asf-25  BLOCKER   12 min  Merge failed: gate `test` after rebase",
            "{width}"
        );
        let pad = " ".repeat(31);
        assert_eq!(
            l[first + 1].trim_end(),
            format!("{pad}2 tests failed: start_retry_once, start_gives_up_after_second_timeout"),
            "{width}"
        );
        assert_eq!(
            l[first + 2].trim_end(),
            format!("{pad}Enter full output · v verify again · r reply to the worker"),
            "{width}"
        );
        assert_eq!(l[first + 3].trim(), "", "{width}: a blank row between items");
        assert_eq!(
            l[first + 4].trim_end(),
            "   ■ asf-27  QUESTION   4 min  Keep the old --scope flag as an alias?",
            "{width}"
        );
        assert_eq!(
            l[first + 5].trim_end(),
            format!("{pad}asked in the worker pane · a answer"),
            "{width}"
        );
        // the information section: newest first, one line each, the BLOCKERs above are not repeated
        let info = line_no(&t, "FOR YOUR INFORMATION (4 unread)");
        assert!(info > first + 5, "{width}");
        assert!(
            l[info].starts_with(" FOR YOUR INFORMATION"),
            "{width}: {:?}",
            l[info]
        );
        assert_eq!(
            l[info + 2].trim_end(),
            "   ✓ asf-24  merged    18 min  feat/asf-24 → main at 9c1e4aa",
            "{width}"
        );
        assert_eq!(
            l[info + 3].trim_end(),
            "   ◆ asf-24  DECISION  21 min  review: approve (opus, round 1)",
            "{width}"
        );
        assert_eq!(
            l[info + 4].trim_end(),
            "   ● asf-28  STATUS    25 min  finished; report saved to .factory/runs/asf-28/report.md",
            "{width}"
        );
        assert_eq!(
            l[info + 5].trim_end(),
            "   ◆ asf-29  DECISION  31 min  fingerprint uses `git patch-id --stable`, stored in the merge event",
            "{width}"
        );
        assert!(!t.contains("Worker for asf-27 is waiting"), "{width}:\n{t}");
        // the footer is the mockup's
        assert_eq!(
            l[29].trim(),
            "j / k move  Enter open  a answer  m mark issue read  M mark all read  ? help  q quit",
            "{width}"
        );
    }
}

#[test]
fn rows_are_the_needs_you_items_followed_by_the_unread_information() {
    let a = mockup_app();
    assert_eq!(a.rows(View::Inbox), 6, "2 items that need you and 4 to read");
    assert_eq!(a.selected(View::Inbox), 0);
    assert_eq!(a.selected_issue().unwrap().as_str(), "asf-25");
}

#[test]
fn nothing_is_drawn_past_the_width_and_long_text_ends_in_an_ellipsis() {
    for (width, height) in [(80u16, 24u16), (100, 30), (120, 30)] {
        let t = render_to_text(&mockup_app(), width, height);
        for l in t.lines() {
            assert!(text_width(l) <= width as usize, "{width}: {l:?}");
        }
    }
    let t = render_to_text(&mockup_app(), 80, 30);
    let detail = t
        .lines()
        .find(|l| l.contains("2 tests failed"))
        .expect("the detail row");
    assert!(detail.trim_end().ends_with('…'), "{detail:?}");
    assert!(text_width(detail) <= 80, "{detail:?}");
}

#[test]
fn sixty_two_unread_comments_scroll_with_the_selection_in_both_layouts() {
    let mut s = mockup_desk();
    s.inbox = (1..=62)
        .map(|n| {
            at(
                comment("asf-28", CommentType::Status, &format!("progress note {n:02}")),
                n,
                &format!("2026-11-06T11:{:02}:00Z", n % 60),
            )
        })
        .collect();
    s.blockers.clear();
    s.derive();
    for everything in [false, true] {
        let mut a = view_of(s.clone());
        if everything {
            a.handle_key(key('A'));
        }
        a.handle_key(key('G'));
        let end = render_to_text(&a, 100, 30);
        // the selected (last) row is on screen and carries the marker; the title has scrolled away
        let marked = end
            .lines()
            .find(|l| l.contains("▸"))
            .unwrap_or_else(|| panic!("no selected row:\n{end}"));
        assert!(marked.contains("progress note"), "{everything}: {marked:?}");
        assert!(
            !end.contains("NEEDS YOU (") && !end.contains("EVERYTHING UNREAD ("),
            "{everything}:\n{end}"
        );
        a.handle_key(key('g'));
        let back = render_to_text(&a, 100, 30);
        assert!(
            back.contains("NEEDS YOU (2)") || back.contains("EVERYTHING UNREAD (62)"),
            "{everything}:\n{back}"
        );
    }
}

#[test]
fn an_empty_inbox_says_what_will_appear_and_is_secondary_text() {
    for width in [80u16, 100, 120] {
        let a = view_of(Snapshot::default());
        let t = render_to_text(&a, width, 30);
        assert!(
            t.contains("Nothing needs you and nothing new to read."),
            "{width}:\n{t}"
        );
        // the whole explanation is on screen at 100 columns and up; narrower, it ends in an ellipsis
        let line = t
            .lines()
            .find(|l| l.contains("Nothing needs you"))
            .expect("the empty line");
        assert!(text_width(line.trim_end()) <= width as usize, "{width}: {line:?}");
        if width >= 100 {
            assert!(
                line.contains("appear here."),
                "{width}: the line is cut: {line:?}"
            );
        } else {
            assert!(line.trim_end().ends_with('…'), "{width}: {line:?}");
        }
        assert!(
            !t.contains("NEEDS YOU") && !t.contains("FOR YOUR INFORMATION"),
            "{t}"
        );
        assert!(cell_style(&a, "Nothing needs you", "Nothing")
            .1
            .contains(Modifier::DIM));
    }
}

#[test]
fn a_section_without_rows_says_so() {
    // only information: the old fixture's single unread BLOCKER on a working issue
    let t = render_to_text(&inbox_app(), 100, 30);
    assert!(
        t.contains("NEEDS YOU (0)") && t.contains("Nothing needs you."),
        "{t}"
    );
    assert!(t.contains("FOR YOUR INFORMATION (1 unread)"), "{t}");
    // only needs: everything unread is already in NEEDS YOU
    let mut s = mockup_desk();
    s.inbox.clear();
    s.derive();
    let t = render_to_text(&view_of(s), 100, 30);
    assert!(t.contains("NEEDS YOU (2)"), "{t}");
    assert!(
        t.contains("FOR YOUR INFORMATION (0 unread)") && t.contains("Nothing new to read."),
        "{t}"
    );
}

#[test]
fn symbols_and_text_styles_follow_the_theme() {
    let a = mockup_app();
    // bold section titles and issue ids; the needs-you mark is red and bold
    assert!(cell_style(&a, "NEEDS YOU (2)", "NEEDS YOU")
        .1
        .contains(Modifier::BOLD));
    assert!(cell_style(&a, "FOR YOUR INFORMATION", "FOR YOUR")
        .1
        .contains(Modifier::BOLD));
    assert!(cell_style(&a, "asf-27  QUESTION", "asf-27")
        .1
        .contains(Modifier::BOLD));
    let (fg, m) = cell_style(&a, "asf-27  QUESTION", "■");
    assert_eq!(fg, Color::Red);
    assert!(m.contains(Modifier::BOLD));
    // information: ✓ dim, ◆ yellow, ● green
    assert!(cell_style(&a, "asf-24  merged", "✓").1.contains(Modifier::DIM));
    assert_eq!(cell_style(&a, "asf-24  DECISION", "◆").0, Color::Yellow);
    assert_eq!(cell_style(&a, "asf-28  STATUS", "●").0, Color::Green);
    // the hint line: what to do is dim, the key letters are accent + bold
    let (fg, m) = cell_style(&a, "asked in the worker pane", "a answer");
    assert_eq!((fg, m.contains(Modifier::BOLD)), (Color::Cyan, true));
    assert!(cell_style(&a, "asked in the worker pane", "answer")
        .1
        .contains(Modifier::DIM));
    assert!(cell_style(&a, "asked in the worker pane", "asked")
        .1
        .contains(Modifier::DIM));
    let (fg, m) = cell_style(&a, "full output", "Enter");
    assert_eq!((fg, m.contains(Modifier::BOLD)), (Color::Cyan, true));
    assert!(cell_style(&a, "full output", "verify again")
        .1
        .contains(Modifier::DIM));
}

#[test]
fn the_selected_item_has_a_highlighted_background_on_every_one_of_its_lines() {
    let a = mockup_app();
    let t = render_to_text(&a, 120, 30);
    let first = line_no(&t, "asf-25  BLOCKER");
    let buf = render_to_buffer(&a, 120, 30);
    for y in first..first + 3 {
        assert_ne!(buf[(60, y as u16)].bg, Color::Reset, "line {y} of the item");
    }
    assert_eq!(
        buf[(60, (first + 4) as u16)].bg,
        Color::Reset,
        "the next item is not selected"
    );
    let mut plain = mockup_app();
    plain.theme = Theme::new(false);
    let buf = render_to_buffer(&plain, 120, 30);
    assert!(buf[(60, first as u16)].modifier.contains(Modifier::REVERSED));
}

// ---------------------------------------------------------------------------------------------
// The two-line summary of a gate failure
// ---------------------------------------------------------------------------------------------

#[test]
fn a_merge_gate_failure_names_the_gate_and_the_failed_tests() {
    let s = inbox::gate_summary(MERGE_BODY, true).expect("a gate failure");
    assert_eq!(s.title, "Merge failed: gate `test` after rebase");
    assert_eq!(
        s.detail.as_deref(),
        Some("2 tests failed: start_retry_once, start_gives_up_after_second_timeout")
    );
}

#[test]
fn a_verification_failure_without_failed_tests_shows_the_first_error_line() {
    let s = inbox::gate_summary(VERIFY_BODY, false).expect("a gate failure");
    assert_eq!(s.title, "Verification failed: gate `clippy`");
    assert_eq!(
        s.detail.as_deref(),
        Some("error: could not compile `asf` (lib) due to 1 previous error")
    );
    // no line that looks like an error: the first line of the output
    let fmt = "gate `fmt` failed (exit Some(1)):\nDiff in src/lib.rs:3:\n-old\n+new\n\nFix and `asf merge retry asf-2`.";
    assert_eq!(
        inbox::gate_summary(fmt, true).unwrap().detail.as_deref(),
        Some("Diff in src/lib.rs:3:")
    );
    // no output at all: no second line
    let silent = "gate `x` failed (exit None):\n\nFix and `asf merge retry asf-2`.";
    assert_eq!(inbox::gate_summary(silent, true).unwrap().detail, None);
}

#[test]
fn failed_tests_come_from_the_list_under_failures_once_each() {
    let both = "gate `test` failed (exit Some(101)):\n\
failures:\n\
\n\
---- tests::a stdout ----\n\
thread 'tests::a' panicked at src/lib.rs:1:1\n\
\n\
failures:\n\
\x20\x20\x20\x20tests::a\n\
\x20\x20\x20\x20tests::b\n\
\n\
test result: FAILED. 0 passed; 2 failed";
    let s = inbox::gate_summary(both, false).unwrap();
    assert_eq!(s.detail.as_deref(), Some("2 tests failed: tests::a, tests::b"));
    // two test binaries in one tail: a name listed twice is still one failed test
    let twice = "gate `test` failed (exit Some(101)):\nfailures:\n    a\n    b\n\nfailures:\n    a\n";
    assert_eq!(
        inbox::gate_summary(twice, false).unwrap().detail.as_deref(),
        Some("2 tests failed: a, b")
    );
    let one = "gate `test` failed (exit Some(101)):\nfailures:\n    only_one\n";
    assert_eq!(
        inbox::gate_summary(one, false).unwrap().detail.as_deref(),
        Some("1 test failed: only_one")
    );
}

#[test]
fn a_blocker_that_is_not_a_gate_failure_gets_no_summary() {
    let rebase = "rebase of feat/asf-9 onto main conflicts in: src/a.rs\n\nThe rebase was aborted.";
    assert!(inbox::gate_summary(rebase, true).is_none());
    assert!(inbox::gate_summary("", false).is_none());
}

/// A merge that failed at the gate (BLOCKER at 11:20), was fixed and retried with `v`, and then
/// failed for another reason at 11:58 (`last_error` only, the queue posts no BLOCKER for it).
fn retried_merge(last_error: &str) -> Snapshot {
    let mut s = desk(live(&["asf-25"]));
    s.queue.push(merge_item(
        1,
        "asf-25",
        MergeState::Failed,
        Some(last_error),
        "2026-11-06T11:58:00Z",
    ));
    let old = at(
        comment("asf-25", CommentType::Blocker, MERGE_BODY),
        5,
        "2026-11-06T11:20:00Z",
    );
    s.blockers = vec![old.clone()];
    s.inbox = vec![];
    s.derive();
    s
}

#[test]
fn a_blocker_older_than_the_failure_is_not_its_summary_nor_what_enter_opens() {
    let mut a = view_of(retried_merge(
        "fast-forward of main to feat/asf-25 failed: main moved while the merge ran",
    ));
    let t = render_to_text(&a, 120, 30);
    assert!(
        t.contains("fast-forward of main to feat/asf-25 failed"),
        "the real error is on screen:\n{t}"
    );
    assert!(
        !t.contains("Merge failed: gate") && !t.contains("2 tests failed"),
        "the old gate output is not the summary:\n{t}"
    );
    assert!(
        !t.contains("Enter full output"),
        "no comment belongs to this failure, so no Enter hint:\n{t}"
    );
    // Enter opens nothing: the old BLOCKER is another failure
    let act = a.handle_key(code(KeyCode::Enter));
    assert!(
        !matches!(a.mode(), Mode::Comment { .. }),
        "{act:?} {:?}",
        a.mode()
    );
}

#[test]
fn a_gate_failure_is_summarised_from_the_error_the_queue_kept_before_any_blocker() {
    // the retry failed at the gate again, with other tests; the queue's error is the newest account
    let again = MERGE_BODY
        .replace("start_retry_once", "start_other")
        .replace("start_gives_up_after_second_timeout", "start_third");
    let a = view_of(retried_merge(&again));
    let t = render_to_text(&a, 120, 30);
    assert!(
        t.contains("2 tests failed: start_other, start_third"),
        "the new failure wins over the older BLOCKER:\n{t}"
    );
    assert!(!t.contains("start_retry_once"), "{t}");
}

#[test]
fn a_blocker_posted_with_the_failure_still_opens_with_enter() {
    // the mockup desk: BLOCKER and queue item carry the same minute
    let mut a = mockup_app();
    assert_eq!(a.handle_key(code(KeyCode::Enter)), Action::None);
    assert_eq!(opened_comment_id(&a), 5);
}

#[test]
fn a_failed_run_is_summarised_as_a_verification_failure_and_v_checks_it_again() {
    let mut s = desk(live(&["asf-3"]));
    let mut failed = run_at("asf-3", RunState::Failed, "2026-11-06T11:00:00Z");
    failed.finished_at = Some("2026-11-06T11:20:00Z".into());
    // `worker.rs` keeps the same text in the run's exit_reason as in the BLOCKER (`problems.join`)
    failed.exit_reason = Some(
        "gate `clippy` failed (exit Some(101)):\nwarning: unused variable: `x`\nerror: could not compile `asf` (lib) due to 1 previous error"
            .into(),
    );
    s.failed_runs.push(failed);
    let blocker = at(
        comment("asf-3", CommentType::Blocker, VERIFY_BODY),
        7,
        "2026-11-06T11:20:00Z",
    );
    s.blockers = vec![blocker.clone()];
    s.inbox = vec![blocker];
    s.derive();
    let mut a = view_of(s);
    let t = render_to_text(&a, 120, 30);
    let at_row = line_no(&t, "asf-3   BLOCKER");
    let l = lines_of(&t);
    assert!(
        l[at_row].contains("40 min") && l[at_row].contains("Verification failed: gate `clippy`"),
        "{t}"
    );
    assert!(
        l[at_row + 1].contains("error: could not compile `asf` (lib) due to 1 previous error"),
        "{t}"
    );
    assert!(
        l[at_row + 2].contains("Enter full output · v verify again · r reply to the worker"),
        "{t}"
    );
    assert!(
        !t.contains("FOR YOUR INFORMATION (1"),
        "its BLOCKER is not information too:\n{t}"
    );
    assert_eq!(a.handle_key(key('v')), Action::Verify(id("asf-3")));
}

// ---------------------------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------------------------

#[test]
fn enter_opens_the_full_comment_of_the_selected_needs_you_item() {
    let mut a = mockup_app();
    assert_eq!(a.handle_key(code(KeyCode::Enter)), Action::None);
    assert_eq!(opened_comment_id(&a), 5, "the merge BLOCKER with the full output");
    a.handle_key(code(KeyCode::Esc));
    a.handle_key(key('j'));
    a.handle_key(code(KeyCode::Enter));
    assert_eq!(
        opened_comment_id(&a),
        6,
        "the BLOCKER that quotes the worker screen"
    );
    a.handle_key(code(KeyCode::Esc));
    // an information row opens its own comment
    a.handle_key(key('j'));
    assert_eq!(a.selected_issue().unwrap().as_str(), "asf-24");
    a.handle_key(code(KeyCode::Enter));
    assert_eq!(opened_comment_id(&a), 1);
}

#[test]
fn j_and_k_walk_both_sections_and_g_jumps_to_the_ends() {
    let mut a = mockup_app();
    let seen: Vec<String> = (0..6)
        .map(|_| {
            let issue = a.selected_issue().unwrap().to_string();
            a.handle_key(key('j'));
            issue
        })
        .collect();
    assert_eq!(seen, ["asf-25", "asf-27", "asf-24", "asf-24", "asf-28", "asf-29"]);
    assert_eq!(a.selected(View::Inbox), 5, "stops on the last row");
    a.handle_key(key('g'));
    assert_eq!(a.selected(View::Inbox), 0);
    a.handle_key(key('G'));
    assert_eq!(a.selected_issue().unwrap().as_str(), "asf-29");
    a.handle_key(key('k'));
    assert_eq!(a.selected_issue().unwrap().as_str(), "asf-28");
}

#[test]
fn the_keys_act_on_the_selected_item() {
    let mut a = mockup_app();
    // a failed merge: v tries it again, r writes to the worker, m marks the issue read
    assert_eq!(a.handle_key(key('v')), Action::Retry(id("asf-25")));
    assert_eq!(a.handle_key(key('m')), Action::MarkRead(id("asf-25")));
    assert_eq!(a.handle_key(key('r')), Action::None);
    match a.mode() {
        Mode::Input { prompt, target, .. } => {
            assert_eq!(prompt, "reply to asf-25");
            assert!(matches!(target, InputTarget::Reply(i) if i == &id("asf-25")));
        }
        other => panic!("expected the reply input, got {other:?}"),
    }
    a.handle_key(code(KeyCode::Esc));
    // a is not for a merge; it says what the row takes instead of doing nothing
    a.handle_key(key('a'));
    assert!(matches!(a.mode(), Mode::Normal));
    let said = a.status.clone().unwrap_or_default();
    assert!(said.contains("asf-25") && said.contains("v verify"), "{said:?}");
    // a blocked worker: a opens the answer window and reads its pane; v has nothing to check
    a.handle_key(key('j'));
    assert_eq!(a.handle_key(key('a')), Action::Peek(id("asf-27")));
    assert!(matches!(a.mode(), Mode::Answer { .. }));
    a.handle_key(code(KeyCode::Esc));
    assert_eq!(a.handle_key(key('v')), Action::None);
    assert!(
        a.status.as_deref().unwrap_or("").contains("asf-27"),
        "{:?}",
        a.status
    );
    // an information row: m and r work on its issue, a says why not
    a.handle_key(key('j'));
    assert_eq!(a.handle_key(key('m')), Action::MarkRead(id("asf-24")));
    a.handle_key(key('a'));
    assert!(
        a.status.as_deref().unwrap_or("").contains("asf-24"),
        "{:?}",
        a.status
    );
    // M asks first, and counts everything unread, also what is already listed under NEEDS YOU
    assert_eq!(a.handle_key(key('M')), Action::None);
    match a.mode() {
        Mode::Confirm { what, .. } => assert_eq!(what, "mark all 6 read?"),
        other => panic!("expected a question, got {other:?}"),
    }
    assert_eq!(a.handle_key(key('y')), Action::MarkAllRead);
}

#[test]
fn a_answers_a_question_comment_with_a_typed_answer() {
    let mut s = desk(live(&["asf-2"]));
    let q = question(
        7,
        "asf-2",
        "Keep the old --scope flag as an alias?\nmore",
        "2026-11-06T11:56:00Z",
    );
    s.questions = vec![q.clone()];
    s.inbox = vec![q];
    s.derive();
    let mut a = view_of(s);
    let t = render_to_text(&a, 120, 30);
    let l = lines_of(&t);
    let at_row = line_no(&t, "asf-2   QUESTION");
    assert!(
        l[at_row].contains("4 min") && l[at_row].contains("Keep the old --scope flag as an alias?"),
        "{t}"
    );
    assert!(
        l[at_row + 1].contains("a answer") && l[at_row + 1].contains("Enter full question"),
        "{t}"
    );
    assert!(
        !t.contains("FOR YOUR INFORMATION (1"),
        "the question is not information too:\n{t}"
    );
    assert_eq!(a.handle_key(key('a')), Action::None);
    match a.mode() {
        Mode::Input { prompt, target, .. } => {
            assert_eq!(prompt, "answer asf-2 (#7)");
            assert!(matches!(target, InputTarget::Answer(i, 7) if i == &id("asf-2")));
        }
        other => panic!("expected the answer input, got {other:?}"),
    }
}

#[test]
fn an_item_without_a_comment_offers_no_enter_and_says_so() {
    let mut s = desk(live(&["asf-5"]));
    s.runs
        .push(run_at("asf-5", RunState::OverBudget, "2026-11-06T10:00:00Z"));
    s.derive();
    let mut a = view_of(s);
    let t = render_to_text(&a, 120, 30);
    let l = lines_of(&t);
    let hint = l[line_no(&t, "asf-5   BLOCKER") + 1];
    assert!(!hint.contains("Enter"), "{hint:?}");
    assert!(
        hint.contains("Now view") && hint.contains("r reply to the worker"),
        "{hint:?}"
    );
    assert_eq!(a.handle_key(code(KeyCode::Enter)), Action::None);
    assert!(matches!(a.mode(), Mode::Normal));
    assert!(
        a.status.as_deref().unwrap_or("").contains("asf-5"),
        "{:?}",
        a.status
    );
}

#[test]
fn a_stalled_worker_is_answered_from_here() {
    let mut s = desk(live(&["asf-4"]));
    let mut stalled = run_at("asf-4", RunState::Stalled, "2026-11-06T10:30:00Z");
    stalled.exit_reason = Some("no output for 10 minutes".into());
    s.runs.push(stalled);
    s.derive();
    let mut a = view_of(s);
    let t = render_to_text(&a, 120, 30);
    assert!(
        t.contains("a answer") && t.contains("r reply to the worker"),
        "{t}"
    );
    assert_eq!(a.handle_key(key('a')), Action::Peek(id("asf-4")));
}

// ---------------------------------------------------------------------------------------------
// A: everything unread, flat
// ---------------------------------------------------------------------------------------------

#[test]
fn capital_a_toggles_a_flat_chronological_list_of_everything_unread() {
    let mut a = mockup_app();
    assert_eq!(a.handle_key(key('A')), Action::None);
    let t = render_to_text(&a, 120, 30);
    let head = t
        .lines()
        .find(|l| l.contains("EVERYTHING UNREAD (6)"))
        .expect("title");
    assert!(head.trim_end().ends_with("A back to the summary"), "{head:?}");
    assert!(
        !t.contains("NEEDS YOU") && !t.contains("FOR YOUR INFORMATION"),
        "{t}"
    );
    // one line each, oldest first, the raw BLOCKERs included
    let order = [
        "asf-29  DECISION",
        "asf-28  STATUS",
        "asf-24  DECISION",
        "asf-24  merged",
        "asf-25  BLOCKER",
        "asf-27  BLOCKER",
    ]
    .map(|n| line_no(&t, n));
    assert_eq!(
        order,
        [
            order[0],
            order[0] + 1,
            order[0] + 2,
            order[0] + 3,
            order[0] + 4,
            order[0] + 5
        ]
    );
    assert!(
        t.contains("gate `test` failed (exit Some(101)) after rebase:"),
        "{t}"
    );
    assert!(t.contains("Worker for asf-27 is waiting for input."), "{t}");
    // the keys work on these rows too
    assert_eq!(a.handle_key(key('G')), Action::None);
    assert_eq!(a.selected_issue().unwrap().as_str(), "asf-27");
    a.handle_key(code(KeyCode::Enter));
    assert_eq!(opened_comment_id(&a), 6);
    a.handle_key(code(KeyCode::Esc));
    assert_eq!(
        a.handle_key(key('a')),
        Action::Peek(id("asf-27")),
        "the issue's open question"
    );
    a.handle_key(code(KeyCode::Esc));
    a.handle_key(key('k'));
    assert_eq!(a.handle_key(key('v')), Action::Retry(id("asf-25")));
    // and A goes back
    a.handle_key(key('A'));
    let t = render_to_text(&a, 120, 30);
    assert!(
        t.contains("NEEDS YOU (2)") && !t.contains("EVERYTHING UNREAD"),
        "{t}"
    );
    assert_eq!(a.selected(View::Inbox), 0, "back on the first row");
}

// ---------------------------------------------------------------------------------------------
// Behaviour that was already there
// ---------------------------------------------------------------------------------------------

#[test]
fn inbox_view_renders_its_rows() {
    let mut a = app();
    a.handle_key(key('4'));
    let t = render_to_text(&a, 100, 30);
    assert!(
        t.contains("BLOCKER") && t.contains("Worker is waiting for input."),
        "{t}"
    );
    assert_eq!(a.rows(View::Inbox), 1);
}

#[test]
fn inbox_view_selects_its_own_issue_and_enter_only_opens_the_comment() {
    let mut a = app();
    a.handle_key(key('4'));
    assert_eq!(a.selected_issue().unwrap().as_str(), "asf-2");
    // Enter only opens the comment now; marking read is `m` / `M`.
    assert_eq!(a.handle_key(code(KeyCode::Enter)), Action::None);
    assert!(matches!(a.mode(), Mode::Comment { .. }));
}

#[test]
fn enter_opens_the_comment_modal_and_changes_nothing_unread() {
    let mut a = inbox_app();
    assert_eq!(a.handle_key(code(KeyCode::Enter)), Action::None);
    assert!(matches!(a.mode(), Mode::Comment { .. }));
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
    assert!(matches!(a.mode(), Mode::Normal));
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
    match &a.mode() {
        Mode::Confirm { what, .. } => assert_eq!(what, "mark all 1 read?"),
        other => panic!("expected confirm, got {other:?}"),
    }
    assert!(render_to_text(&a, 100, 30).contains("mark all 1 read?"));
    assert_eq!(a.handle_key(key('n')), Action::None);
    assert!(matches!(a.mode(), Mode::Normal));
    a.handle_key(key('M'));
    assert_eq!(a.handle_key(key('y')), Action::MarkAllRead);
    assert!(matches!(a.mode(), Mode::Normal));
}
