use super::*;

#[test]
fn input_and_confirm_modes_render_prompts_and_small_sizes_do_not_panic() {
    let mut a = app();
    a.handle_key(key('j'));
    a.handle_key(key('j'));
    a.handle_key(key('r'));
    for c in "hi".chars() {
        a.handle_key(key(c));
    }
    let t = render_to_text(&a, 100, 30);
    assert!(t.contains("reply to asf-2") && t.contains("hi"), "{t}");
    a.handle_key(code(KeyCode::Esc));
    a.handle_key(key('x'));
    let t = render_to_text(&a, 100, 30);
    assert!(t.contains("abort asf-2?"), "{t}");
    let _ = render_to_text(&a, 60, 15);
    let _ = render_to_text(&a, 20, 5);
}

#[test]
fn status_message_shows_in_footer() {
    let mut a = app();
    a.status = Some("started asf-3".into());
    let t = render_to_text(&a, 100, 30);
    assert!(t.contains("started asf-3"), "{t}");
}
