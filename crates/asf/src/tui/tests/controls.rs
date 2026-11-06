use super::*;

#[test]
fn ctrl_c_quits_from_every_mode() {
    let mut input = app();
    for c in ['j', 'j', 'r'] {
        input.handle_key(key(c));
    }
    let mut confirm = app();
    for c in ['j', 'j', 'x'] {
        confirm.handle_key(key(c));
    }
    let mut text = open_overlay();
    text.handle_key(key('t'));
    let mut comment = inbox_app();
    comment.handle_key(code(KeyCode::Enter));
    let mut help = app();
    help.handle_key(key('?'));
    let modes = [
        ("normal", app()),
        ("input", input),
        ("confirm", confirm),
        ("answer", open_overlay()),
        ("answer text", text),
        ("comment", comment),
        ("help", help),
    ];
    for (name, mut a) in modes {
        assert_eq!(a.handle_key(ctrl('c')), Action::Quit, "{name}");
    }
}

#[test]
fn ctrl_w_deletes_the_last_word_and_ctrl_u_clears_the_line() {
    let mut a = typing_app();
    for c in "go on  now".chars() {
        a.handle_key(key(c));
    }
    a.handle_key(ctrl('w'));
    assert_eq!(buf_of(&a), "go on  ");
    a.handle_key(ctrl('w'));
    assert_eq!(buf_of(&a), "go ");
    a.handle_key(ctrl('w'));
    assert_eq!(buf_of(&a), "");
    a.handle_key(ctrl('w')); // nothing to delete: no panic
    for c in "abc".chars() {
        a.handle_key(key(c));
    }
    a.handle_key(ctrl('u'));
    assert_eq!(buf_of(&a), "");
    // the same keys edit the free-text reply of the answer overlay
    let mut o = open_overlay();
    o.handle_key(key('t'));
    for c in "use 2 please".chars() {
        o.handle_key(key(c));
    }
    o.handle_key(ctrl('w'));
    assert_eq!(
        o.handle_key(code(KeyCode::Enter)),
        Action::Reply(id("asf-2"), "use 2".into())
    );
    let mut o = open_overlay();
    o.handle_key(key('t'));
    o.handle_key(key('x'));
    o.handle_key(ctrl('u'));
    assert_eq!(
        o.handle_key(code(KeyCode::Enter)),
        Action::None,
        "empty text is not sent"
    );
}

#[test]
fn modifier_chords_never_insert_a_character_or_trigger_a_binding() {
    let mut a = typing_app();
    a.handle_key(key('a'));
    for k in [
        mods('x', KeyModifiers::CONTROL),
        mods('x', KeyModifiers::ALT),
        mods('x', KeyModifiers::CONTROL | KeyModifiers::SHIFT),
        mods('b', KeyModifiers::ALT | KeyModifiers::SHIFT),
    ] {
        assert_eq!(a.handle_key(k), Action::None);
    }
    assert_eq!(buf_of(&a), "a", "chords left the line alone");
    // Shift alone is just a capital letter
    a.handle_key(mods('B', KeyModifiers::SHIFT));
    assert_eq!(buf_of(&a), "aB");
    // Windows reports AltGr (Polish ą ę ł) as Ctrl+Alt together; that is text, not a chord
    a.handle_key(mods('ą', KeyModifiers::CONTROL | KeyModifiers::ALT));
    assert_eq!(buf_of(&a), "aBą");
    // in normal mode a chord is no binding: Alt+q does not quit, Ctrl+j does not move
    let mut n = app();
    assert_eq!(n.handle_key(mods('q', KeyModifiers::ALT)), Action::None);
    assert_eq!(n.handle_key(ctrl('t')), Action::None);
    n.handle_key(ctrl('j'));
    assert_eq!(n.selected_issue().unwrap().as_str(), "asf-4");
    let mut o = open_overlay();
    assert_eq!(o.handle_key(ctrl('y')), Action::None);
    assert!(matches!(o.mode(), Mode::Answer { .. }));
}
