//! The Queue view. Tests of it live here and nowhere else, so the task that rebuilds the view edits
//! this file only.

use super::*;

#[test]
fn queue_view_renders_its_rows() {
    let mut a = app();
    a.handle_key(key('5'));
    let t = render_to_text(&a, 100, 30);
    assert!(t.contains("feat/asf-1") && t.contains("queued"), "{t}");
    assert_eq!(a.rows(View::Queue), 1);
}

#[test]
fn queue_view_selects_its_own_issue() {
    let mut a = app();
    a.handle_key(key('5'));
    assert_eq!(a.selected_issue().unwrap().as_str(), "asf-1");
}
