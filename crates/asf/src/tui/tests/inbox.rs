//! The Inbox view. Tests of it live here and nowhere else, so the task that rebuilds the view edits
//! this file only.

use super::*;

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
