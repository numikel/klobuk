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

// ---- a dialog with several questions (asf-124) ----------------------------------------------------
// Captures of Claude Code 2.1.325 (AskUserQuestion, two questions). A digit answers the open tab and
// moves to the next one by itself, the last digit lands on the review page, Enter there submits.

const TABS_FIRST: &str = "←  ☐ First  ☐ Second  ✔ Submit  →\nPick A or B?\n❯ 1. A\n     Wybór A\n  2. B\n     Wybór B\n  3. Type something.\n──────────\n  4. Chat about this\nEnter to select · Tab/Arrow keys to navigate · Esc to cancel";
const TABS_SECOND: &str = "←  ☒ First  ☐ Second  ✔ Submit  →\nPick X or Y?\n❯ 1. X\n     Wybór X\n  2. Y\n     Wybór Y\n  3. Type something.\n──────────\n  4. Chat about this\nEnter to select · Tab/Arrow keys to navigate · Esc to cancel";
const TABS_REVIEW: &str = "←  ☒ First  ☒ Second  ✔ Submit  →\nReview your answers\n ● Pick A or B?\n   → A\n ● Pick X or Y?\n   → Y\nReady to submit your answers?\n❯ 1. Submit answers\n  2. Cancel";

fn keys_of(a: Action) -> Vec<String> {
    match a {
        Action::Keys(i, keys) => {
            assert_eq!(i, id("asf-2"));
            keys
        }
        other => panic!("expected keys, got {other:?}"),
    }
}

fn waiting(a: &App) -> bool {
    matches!(a.mode(), Mode::Answer { walk, .. } if walk.wait.is_some())
}

#[test]
fn a_two_question_dialog_is_answered_tab_by_tab_and_klobuk_sends_the_submit_step() {
    let mut a = open_overlay();
    a.set_screen(TABS_FIRST.into());
    let t = render_to_text(&a, 120, 30);
    assert!(t.contains("question 1 of 2"), "{t}");
    assert!(t.contains("Pick A or B?") && t.contains("1-4 choose"), "{t}");
    assert!(t.contains("k raw keys"), "{t}");

    let mut sent = keys_of(a.handle_key(key('1')));
    assert!(
        matches!(a.mode(), Mode::Answer { .. }),
        "stays open for the next question"
    );
    assert!(waiting(&a));
    // whatever view is behind the overlay (the Board asks for no pane by itself)
    a.view = View::Board;
    assert_eq!(
        a.peek_wanted(),
        Some(id("asf-2")),
        "the pane is read again until it moves on"
    );
    // the pane has not moved yet: a digit now would land on the tab that is going away
    assert_eq!(a.handle_key(key('2')), Action::None);
    assert_eq!(a.handle_key(code(KeyCode::Enter)), Action::None);

    a.set_screen(TABS_FIRST.into()); // a peek that still shows the old tab does not end the wait
    assert!(waiting(&a));
    // a half-drawn frame differs from the old one but still has tab 1 open: the wait goes on
    a.set_screen(TABS_FIRST.replace("Pick A or B?", "Pick A or B? (redrawing)"));
    assert!(waiting(&a));
    assert_eq!(a.handle_key(key('2')), Action::None);
    a.set_screen(TABS_SECOND.into());
    assert!(!waiting(&a));
    assert_eq!(a.peek_wanted(), None, "nothing to wait for any more");
    let t = render_to_text(&a, 120, 30);
    assert!(
        t.contains("question 2 of 2") && t.contains("picked First: 1 A"),
        "{t}"
    );
    assert!(t.contains("Pick X or Y?"), "{t}");

    sent.extend(keys_of(a.handle_key(key('2'))));
    assert!(
        matches!(a.mode(), Mode::Normal),
        "the last answer carries the Submit step and closes"
    );
    // Claude Code moves on by itself after a digit, so there is no Right between the keys
    assert_eq!(sent, ["1", "2", "submit"]);
}

#[test]
fn enter_on_a_tab_answers_the_highlighted_option_and_the_last_one_submits() {
    let mut a = open_overlay();
    a.set_screen(TABS_FIRST.into());
    assert_eq!(keys_of(a.handle_key(code(KeyCode::Enter))), ["enter"]);
    a.set_screen(TABS_SECOND.into());
    // the cursor moves to option 2 first
    assert_eq!(keys_of(a.handle_key(code(KeyCode::Down))), ["down"]);
    assert!(waiting(&a) && matches!(a.mode(), Mode::Answer { .. }));
    a.set_screen(
        TABS_SECOND
            .replace("❯ 1. X", "  1. X")
            .replace("  2. Y", "❯ 2. Y"),
    );
    assert!(!waiting(&a), "a moved cursor is a new screen");
    assert_eq!(keys_of(a.handle_key(code(KeyCode::Enter))), ["enter", "submit"]);
    assert!(matches!(a.mode(), Mode::Normal));
}

#[test]
fn a_digit_beyond_the_options_and_the_free_text_options_never_submit() {
    let mut a = open_overlay();
    a.set_screen(TABS_SECOND.into());
    assert_eq!(a.handle_key(key('9')), Action::None, "no option 9");
    assert_eq!(
        a.handle_key(key('y')),
        Action::None,
        "y/n mean nothing on a question"
    );
    // option 3 opens a text box instead of answering: only the digit goes out, no Submit
    assert_eq!(keys_of(a.handle_key(key('3'))), ["3"]);
    assert!(matches!(a.mode(), Mode::Answer { .. }));
    assert!(
        matches!(a.mode(), Mode::Answer { walk, .. } if walk.picked.is_empty()),
        "nothing was picked"
    );
}

#[test]
fn a_one_question_dialog_keeps_the_one_key_flow() {
    // the header chip of a single question has no Submit tab behind it
    let single = "☐ Scope alias\n\nKeep the old --scope flag as an alias?\n\n❯ 1. Yes, keep it\n  2. No, remove it\n  3. Type something.\n  4. Chat about this";
    for (k, expected) in [
        (key('1'), "1"),
        (code(KeyCode::Enter), "enter"),
        (code(KeyCode::Down), "down"),
    ] {
        let mut a = open_overlay();
        a.set_screen(single.into());
        let t = render_to_text(&a, 120, 30);
        assert!(!t.contains("question 1 of"), "{t}");
        assert_eq!(keys_of(a.handle_key(k)), [expected]);
        assert!(
            matches!(a.mode(), Mode::Normal),
            "closes after the key, as before"
        );
        assert!(!waiting(&a));
    }
}

#[test]
fn the_review_page_takes_one_key_like_any_dialog() {
    let mut a = open_overlay();
    a.set_screen(TABS_REVIEW.into());
    let t = render_to_text(&a, 120, 30);
    assert!(t.contains("review page"), "{t}");
    assert_eq!(keys_of(a.handle_key(code(KeyCode::Enter))), ["enter"]);
    assert!(matches!(a.mode(), Mode::Normal));
}

#[test]
fn an_unknown_dialog_shape_says_so_and_offers_key_mode_and_a_typed_reply() {
    for shown in [
        "The agent is thinking about something else",
        "(cannot read the pane: x)",
    ] {
        let mut a = open_overlay();
        a.set_screen(shown.into());
        let t = render_to_text(&a, 120, 30);
        assert!(t.contains("not a known dialog shape"), "{t}");
        assert!(t.contains("k key mode") && t.contains("t type a reply"), "{t}");
        // raw keys as before
        assert_eq!(keys_of(a.handle_key(key('2'))), ["2"]);
        assert!(matches!(a.mode(), Mode::Normal));
        // and the typed reply as before
        let mut a = open_overlay();
        a.set_screen(shown.into());
        a.handle_key(key('t'));
        for c in "ok".chars() {
            a.handle_key(key(c));
        }
        assert_eq!(
            a.handle_key(code(KeyCode::Enter)),
            Action::Reply(id("asf-2"), "ok".into())
        );
    }
    // a pane that has not arrived yet is not an unknown shape
    let a = open_overlay();
    let t = render_to_text(&a, 120, 30);
    assert!(
        t.contains("reading the pane") && !t.contains("not a known dialog shape"),
        "{t}"
    );
}

#[test]
fn key_mode_sends_every_key_to_the_pane_and_stays_open() {
    let mut a = open_overlay();
    a.set_screen(TABS_FIRST.into());
    assert_eq!(a.handle_key(key('k')), Action::None);
    let t = render_to_text(&a, 120, 30);
    assert!(t.contains("key mode") && t.contains("←/→/Tab"), "{t}");
    for (k, name) in [
        (code(KeyCode::Right), "right"),
        (code(KeyCode::Left), "left"),
        (code(KeyCode::Tab), "tab"),
        (key('2'), "2"),
        (code(KeyCode::Enter), "enter"),
    ] {
        assert_eq!(keys_of(a.handle_key(k)), [name], "{name}");
        assert!(
            matches!(a.mode(), Mode::Answer { .. }),
            "{name} keeps key mode open"
        );
    }
    // k again goes back to the walk: a digit is a tab answer again
    a.handle_key(key('k'));
    a.set_screen(TABS_FIRST.into());
    assert_eq!(keys_of(a.handle_key(key('1'))), ["1"]);
    assert!(waiting(&a), "a tab answer waits for the next question");
    assert_eq!(a.handle_key(code(KeyCode::Esc)), Action::None);
    assert!(matches!(a.mode(), Mode::Normal));
}

#[test]
fn left_right_and_tab_are_not_keys_of_the_one_key_flow() {
    let mut a = open_overlay();
    a.set_screen("Do you want to proceed?\n 1. Yes\n 2. No".into());
    for k in [KeyCode::Left, KeyCode::Right, KeyCode::Tab] {
        assert_eq!(a.handle_key(code(k)), Action::None);
        assert!(matches!(a.mode(), Mode::Answer { .. }));
    }
}

#[test]
fn a_wait_the_pane_never_answers_runs_out_and_the_overlay_takes_keys_again() {
    use std::time::Duration;
    let mut a = open_overlay();
    a.set_screen(TABS_FIRST.into());
    keys_of(a.handle_key(key('1')));
    a.advance(Duration::from_secs(10));
    assert!(waiting(&a), "the wait starts when the app first sees it");
    a.advance(Duration::from_secs(12));
    assert!(waiting(&a));
    a.advance(Duration::from_secs(14));
    assert!(!waiting(&a));
    assert!(
        a.status.as_deref().unwrap_or("").contains("did not change"),
        "{:?}",
        a.status
    );
    assert_eq!(
        keys_of(a.handle_key(key('2'))),
        ["2"],
        "keys work again (the pane still shows tab 1)"
    );
}

#[test]
fn the_overlay_takes_the_pane_text_from_the_preview_feed_too() {
    let mut a = open_overlay();
    a.set_screen(TABS_FIRST.into());
    keys_of(a.handle_key(key('1')));
    // the event loop reads the pane for `peek_wanted` and stores it as the preview
    a.set_pane_preview(id("asf-9"), TABS_SECOND.into());
    assert!(waiting(&a), "another issue's pane is not the overlay's");
    a.set_pane_preview(id("asf-2"), TABS_SECOND.into());
    assert!(!waiting(&a));
    let t = render_to_text(&a, 120, 30);
    assert!(t.contains("question 2 of 2"), "{t}");
}

/// Types `text` in the open overlay and presses Enter.
fn type_reply(a: &mut App, text: &str) -> Action {
    assert_eq!(a.handle_key(key('t')), Action::None);
    for c in text.chars() {
        a.handle_key(key(c));
    }
    a.handle_key(code(KeyCode::Enter))
}

#[test]
fn a_typed_reply_on_a_tab_is_a_tab_answer_and_the_walk_goes_on() {
    let mut a = open_overlay();
    a.set_screen(TABS_FIRST.into());
    assert_eq!(type_reply(&mut a, "hi"), Action::Reply(id("asf-2"), "hi".into()));
    assert!(
        matches!(a.mode(), Mode::Answer { text: None, .. }),
        "the walk stays open for the next question"
    );
    assert!(waiting(&a));
    assert_eq!(
        a.handle_key(key('1')),
        Action::None,
        "the pane has not moved on yet"
    );
    a.set_screen(TABS_SECOND.into());
    assert!(!waiting(&a));
    let t = render_to_text(&a, 120, 30);
    assert!(
        t.contains("question 2 of 2") && t.contains("picked First: typed hi"),
        "{t}"
    );
}

#[test]
fn a_typed_reply_on_the_last_tab_closes_the_overlay_and_klobuk_submits() {
    let mut a = open_overlay();
    a.set_screen(TABS_SECOND.into());
    assert_eq!(
        type_reply(&mut a, "why"),
        Action::Reply(id("asf-2"), "why".into())
    );
    assert!(matches!(a.mode(), Mode::Normal));
}

#[test]
fn a_typed_reply_outside_a_tabbed_dialog_closes_the_overlay_as_before() {
    for shown in ["Do you want to proceed?\n 1. Yes\n 2. No", "", TABS_REVIEW] {
        let mut a = open_overlay();
        a.set_screen(shown.into());
        assert_eq!(type_reply(&mut a, "ok"), Action::Reply(id("asf-2"), "ok".into()));
        assert!(matches!(a.mode(), Mode::Normal), "{shown:?}");
    }
}

#[test]
fn a_typed_reply_survives_a_pane_refresh_during_a_walk() {
    let mut a = open_overlay();
    a.set_screen(TABS_FIRST.into());
    a.handle_key(key('t'));
    a.handle_key(key('x'));
    a.set_pane_preview(id("asf-2"), TABS_SECOND.into());
    assert_eq!(
        a.handle_key(code(KeyCode::Enter)),
        Action::Reply(id("asf-2"), "x".into())
    );
}
