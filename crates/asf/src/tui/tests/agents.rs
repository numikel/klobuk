//! The Agents view. Tests of it live here and nowhere else, so the task that rebuilds the view edits
//! this file only.

use super::*;

#[test]
fn agents_view_renders_its_rows() {
    let mut a = app();
    a.handle_key(key('3'));
    let t = render_to_text(&a, 100, 30);
    assert!(
        t.contains("asf-2") && t.contains("$1.25") && t.contains("working") && t.contains("f:p1"),
        "{t}"
    );
    assert_eq!(a.rows(View::Agents), 1);
}

#[test]
fn agents_view_selects_its_own_issue() {
    let mut a = app();
    a.handle_key(key('3'));
    assert_eq!(a.selected_issue().unwrap().as_str(), "asf-2");
}

#[test]
fn agents_row_of_a_blocked_run_starts_with_a_red_bold_square() {
    let mut a = board_app(blocked_snapshot());
    a.handle_key(key('3'));
    let t = render_to_text(&a, 120, 30);
    let row = t
        .lines()
        .find(|l| l.contains("asf-2") && l.contains("blocked") && l.contains('│'))
        .unwrap_or_else(|| panic!("no agents row:\n{t}"));
    assert!(row.trim_start_matches([' ', '│']).starts_with('■'), "{row:?}");
    let (fg, m) = cell_style(&a, "blocked", "■");
    assert_eq!(fg, Color::Red);
    assert!(m.contains(Modifier::BOLD));
    let mut w = board_app(snapshot());
    w.handle_key(key('3'));
    assert!(!render_to_text(&w, 120, 30).contains('■'));
}
