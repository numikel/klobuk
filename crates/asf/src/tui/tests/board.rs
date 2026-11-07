//! The Board view: its columns, cards, selection, keys and detail panel. Tests of the Board live here
//! and nowhere else, so the task that rebuilds the view edits this file only.

use super::*;
use crate::tui::views::board;

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
fn board_columns_follow_status_all_without_cancelled() {
    let expected: Vec<Status> = Status::ALL
        .iter()
        .copied()
        .filter(|s| *s != Status::Cancelled)
        .collect();
    assert_eq!(board::columns(), expected);
    let cols = board::columns();
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

#[test]
fn cancelled_issues_are_not_rows_so_the_selection_never_runs_past_the_last_card() {
    let mut s = snapshot();
    // the loader reads cancelled issues too; the Board does not list them
    for n in 90..93 {
        s.issues
            .push(issue(&format!("asf-{n}"), "Dropped", Status::Cancelled));
    }
    let mut a = board_app(s);
    let cards = board::ordered(&a.snap);
    assert!(cards.len() >= 2 && cards.len() < a.snap.issues.len());
    let last = cards.len() - 1;
    let (second_to_last, final_card) = (cards[last - 1].id.clone(), cards[last].id.clone());

    assert_eq!(board::rows(&a.snap), cards.len(), "rows are the cards on screen");
    a.handle_key(key('G'));
    a.handle_key(key('j')); // already at the bottom: nothing moves
    assert_eq!(a.board.selected, last, "G stops at the last card");
    assert_eq!(board::selected_issue(&a.snap, &a.board), Some(final_card));
    assert_eq!(
        a.handle_key(key('k')),
        Action::LoadDetail(second_to_last.clone()),
        "one k moves to the second-to-last card"
    );
    assert_eq!(board::selected_issue(&a.snap, &a.board), Some(second_to_last));
}

#[test]
fn the_board_asks_for_the_detail_of_the_issue_it_lands_on() {
    let mut a = app(); // the Board, asf-4 (backlog) first
    assert_eq!(a.handle_key(key('j')), Action::LoadDetail(id("asf-3")));
    assert_eq!(a.handle_key(key('G')), Action::LoadDetail(id("asf-1")));
    assert_eq!(
        a.handle_key(key('G')),
        Action::None,
        "already there: nothing to load"
    );
    assert_eq!(a.handle_key(code(KeyCode::Esc)), Action::None);
    assert_eq!(
        a.handle_key(code(KeyCode::Enter)),
        Action::LoadDetail(id("asf-1")),
        "opening the panel loads what it shows"
    );
    // the event loop also asks after a view switch or a refresh: wanted until the detail is there
    assert_eq!(a.detail_wanted(), Some(id("asf-1")));
    a.snap.detail = Some(crate::tui::snapshot::IssueDetail {
        issue: id("asf-1"),
        comments: vec![],
        deps: vec![],
    });
    assert_eq!(a.detail_wanted(), None);
    a.handle_key(key('2'));
    assert_eq!(a.detail_wanted(), None, "only the Board shows a detail");
}

#[test]
fn escape_in_normal_mode_closes_the_board_detail_panel() {
    let mut a = app();
    assert!(a.board.detail);
    assert!(render_to_text(&a, 100, 30).contains("┌ detail "));
    assert_eq!(a.handle_key(code(KeyCode::Esc)), Action::None);
    assert!(!a.board.detail);
    assert!(!render_to_text(&a, 100, 30).contains("┌ detail "));
    a.handle_key(code(KeyCode::Enter));
    assert!(a.board.detail, "Enter still toggles it back");
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
