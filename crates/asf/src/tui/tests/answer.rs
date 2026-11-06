use super::*;

#[test]
fn a_opens_the_overlay_only_for_a_blocked_run() {
    let mut w = board_app(snapshot());
    w.handle_key(key('3')); // asf-2 is working
    assert_eq!(w.handle_key(key('a')), Action::None);
    assert!(matches!(w.mode(), Mode::Normal));
    assert!(
        w.status.as_deref().unwrap_or("").contains("no blocked run"),
        "{:?}",
        w.status
    );
    // an issue without a run (the board starts on asf-4, backlog)
    let mut b = board_app(blocked_snapshot());
    assert_eq!(b.handle_key(key('a')), Action::None);
    assert!(matches!(b.mode(), Mode::Normal));
    // a blocked run, from Board, Agents and Inbox
    for (view, steps) in [('2', 2), ('3', 0), ('4', 0)] {
        let mut a = board_app(blocked_snapshot());
        a.handle_key(key(view));
        for _ in 0..steps {
            a.handle_key(key('j'));
        }
        assert_eq!(a.handle_key(key('a')), Action::Peek(id("asf-2")), "view {view}");
        assert!(matches!(a.mode(), Mode::Answer { .. }));
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
            keys.contains("1-2 choose   Enter send the highlighted one   ↑/↓ move   t type a reply")
                && keys.contains("Esc close, send nothing"),
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
    assert!(matches!(a.mode(), Mode::Normal), "modal closes after sending");
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
        assert!(matches!(a.mode(), Mode::Normal));
    }
}

#[test]
fn esc_closes_the_overlay_and_sends_nothing() {
    let mut a = open_overlay();
    assert_eq!(a.handle_key(code(KeyCode::Esc)), Action::None);
    assert!(matches!(a.mode(), Mode::Normal));
    assert_eq!(a.view, View::Agents);
}

#[test]
fn no_other_binding_is_active_while_the_overlay_is_open() {
    let mut a = open_overlay();
    for c in ['q', 's', 'x', 'f', 'm', 'r', 'R', 'j', 'k', 'a', '0', 'd'] {
        assert_eq!(a.handle_key(key(c)), Action::None, "{c}");
        assert!(
            matches!(a.mode(), Mode::Answer { .. }),
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
    assert!(matches!(a.mode(), Mode::Normal));
    // empty text is not sent; Esc closes without sending
    let mut a = open_overlay();
    a.handle_key(key('t'));
    assert_eq!(a.handle_key(code(KeyCode::Enter)), Action::None);
    a.handle_key(key('x'));
    assert_eq!(a.handle_key(code(KeyCode::Esc)), Action::None);
    assert!(matches!(a.mode(), Mode::Normal));
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
