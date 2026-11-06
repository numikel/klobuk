use super::*;

#[test]
fn a_second_background_request_answers_busy_and_runs_nothing() {
    let mut a = app();
    a.busy = Some("tick".into());
    assert_eq!(a.handle_key(key('t')), Action::None);
    assert_eq!(a.status.as_deref(), Some("busy: tick"));
    a.handle_key(key('j')); // asf-3 is ready
    assert_eq!(a.handle_key(key('s')), Action::None);
    assert_eq!(a.status.as_deref(), Some("busy: tick"));
    a.busy = None;
    assert_eq!(a.handle_key(key('s')), Action::Start(id("asf-3")));
}

#[test]
fn the_header_shows_a_spinner_and_label_while_busy_and_every_key_still_works() {
    let mut a = app();
    a.busy = Some("starting asf-25".into());
    a.advance(ms(0));
    let h = render_to_text(&a, 100, 30).lines().next().unwrap().to_string();
    assert!(h.contains("⠋ starting asf-25…"), "{h:?}");
    a.advance(ms(100));
    let h = render_to_text(&a, 100, 30).lines().next().unwrap().to_string();
    assert!(
        h.contains("⠙ starting asf-25…"),
        "the frame advances with the clock: {h:?}"
    );
    a.handle_key(key('3'));
    assert_eq!(a.view, View::Agents, "views still switch while busy");
    a.busy = None;
    let h = render_to_text(&a, 100, 30).lines().next().unwrap().to_string();
    assert!(!h.contains('⠙') && !h.contains('…'), "{h:?}");
}

#[test]
fn child_output_becomes_a_check_or_a_cross_with_the_envelope_message() {
    let tick = r#"{"ok":true,"result":{"promoted":["asf-3"],"closed_epics":[],"watch":{"checked":2,"transitions":[{"x":1}]},"merge":{"items":[{"id":1},{"id":2}]},"started":[],"start_errors":[]}}"#;
    assert_eq!(
        bg_outcome(&BgKind::Tick, tick, "", Some(0)),
        "✓ tick: 1 transition(s), 2 merge item(s), 1 promoted, 0 epic(s) closed"
    );
    let start = r#"{"ok":true,"result":{"run":{"issue_id":"asf-25"},"workspace_id":"w1","pane_id":"p1","trusted":false}}"#;
    assert_eq!(
        bg_outcome(&BgKind::Start(id("asf-25")), start, "", Some(0)),
        "✓ started asf-25 in w1 (pane p1)"
    );
    let err = r#"{"ok":false,"error":{"code":"conflict","message":"asf-25 is already claimed"}}"#;
    assert_eq!(
        bg_outcome(&BgKind::Start(id("asf-25")), err, "", Some(3)),
        "✗ asf-25 is already claimed"
    );
    // a crash prints no envelope: say what we know
    assert_eq!(
        bg_outcome(&BgKind::Tick, "", "boom\nmore", Some(101)),
        "✗ tick exited with 101: boom"
    );
    assert_eq!(bg_outcome(&BgKind::Tick, "", "", None), "✗ tick was terminated");
}
