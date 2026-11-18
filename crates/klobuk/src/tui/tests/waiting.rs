use super::*;

#[test]
fn header_counts_questions_plus_blocked_runs_only_when_positive() {
    let t = render_to_text(&app(), 120, 30);
    assert!(!t.contains("waiting"), "nothing waits, nothing is said:\n{t}");

    let mut s = blocked_snapshot();
    s.questions = vec![question(7, "asf-3", "which crate?", "2026-11-02T10:06:00Z")];
    let a = board_app(s);
    let t = render_to_text(&a, 120, 30);
    assert!(line_of(&t, 0).contains("2 waiting"), "header:\n{t}");
    let (fg, m) = cell_style(&a, "klobuk ·", "2 waiting");
    assert_eq!(
        fg,
        Color::Red,
        "the waiting count is the red ■ of the needs-you mark"
    );
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
    assert_eq!(
        line_of(&t, 2),
        "─".repeat(120),
        "no banner row: the rule follows the tab bar:\n{t}"
    );
    assert!(
        line_of(&t, 3).contains("backlog"),
        "content starts right under that rule:\n{t}"
    );
    assert!(!t.contains("is waiting for your answer") && !t.contains(" asks:"));
}

#[test]
fn answered_question_is_not_waiting() {
    let q = question(7, "asf-3", "which crate?", "2026-11-02T10:06:00Z");
    let mut ans = comment("asf-3", CommentType::Answer, "the core one");
    ans.id = 9;
    ans.reply_to = Some(7);
    assert!(unanswered_questions(&[q.clone(), ans]).is_empty());
    assert_eq!(unanswered_questions(std::slice::from_ref(&q)), vec![7]);
    let mut other = q;
    other.to_lane = "orchestrator".into();
    assert!(unanswered_questions(&[other]).is_empty());
}
