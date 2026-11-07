//! The Log view. Tests of it live here and nowhere else, so the task that rebuilds the view edits this
//! file only.

use super::*;

#[test]
fn log_view_renders_its_rows() {
    let mut a = app();
    a.handle_key(key('6'));
    let t = render_to_text(&a, 100, 30);
    assert!(t.contains("run.state") && t.contains("issue.created"), "{t}");
    assert_eq!(a.rows(View::Log), 2);
}

#[test]
fn log_view_selects_no_issue() {
    let mut a = app();
    a.handle_key(key('6'));
    assert_eq!(a.selected_issue(), None);
}
