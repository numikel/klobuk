//! The loop's pulse in the TUI: the header segment, the banner under the tabs and the first NEEDS YOU
//! row of the Now view.

use super::*;
use crate::tui::snapshot::LoopInfo;
use klobuk_store::serve_pulse::{LoopState, ServeMode};

fn with_loop(mut s: Snapshot, state: LoopState, mode: ServeMode) -> Snapshot {
    s.serve = Some(LoopInfo { state, mode });
    s
}

fn running(mode: ServeMode, age_s: i64) -> LoopState {
    LoopState::Running { mode, age_s }
}

fn stopped(reason: Option<&str>, age_s: i64) -> LoopState {
    LoopState::Stopped {
        reason: reason.map(Into::into),
        age_s,
        ticks: 633,
        max_ticks: Some(633),
    }
}

fn app_on(view: View, s: Snapshot) -> App {
    let mut a = App::new(s);
    a.view = view;
    a
}

fn header(a: &App) -> String {
    line_of(&render_to_text(a, 100, 30), 0)
}

#[test]
fn the_header_shows_a_fresh_loop_dim_with_its_age() {
    let s = with_loop(snapshot(), running(ServeMode::Dispatch, 12), ServeMode::Dispatch);
    let a = app_on(View::Board, s);
    assert!(
        header(&a).contains("runtime ok · loop ok 12 s · workers"),
        "{}",
        header(&a)
    );
    let (_, m) = cell_style(&a, "klobuk ·", "loop ok 12 s");
    assert!(m.contains(Modifier::DIM), "{m:?}");
}

#[test]
fn the_header_says_observe_when_the_loop_runs_without_dispatch() {
    let s = with_loop(snapshot(), running(ServeMode::Observe, 3), ServeMode::Observe);
    assert!(header(&app_on(View::Board, s)).contains("loop observe"));
}

#[test]
fn the_header_shouts_when_the_loop_stopped_and_names_how_long_ago() {
    let s = with_loop(
        snapshot(),
        stopped(Some("max_ticks"), 14 * 60),
        ServeMode::Dispatch,
    );
    let a = app_on(View::Board, s);
    assert!(header(&a).contains("LOOP STOPPED 14 min"), "{}", header(&a));
    let (_, m) = cell_style(&a, "klobuk ·", "LOOP STOPPED");
    assert!(m.contains(Modifier::BOLD), "bold in colour and without: {m:?}");
    let never = with_loop(snapshot(), LoopState::NeverStarted, ServeMode::Dispatch);
    assert!(header(&app_on(View::Board, never)).contains("loop not running"));
}

#[test]
fn a_snapshot_nobody_loaded_says_nothing_about_the_loop() {
    let a = app_on(View::Board, snapshot());
    assert!(!header(&a).contains("loop"), "{}", header(&a));
    assert!(!render_to_text(&a, 100, 30).contains("the loop"));
}

#[test]
fn every_view_but_now_carries_the_banner_with_the_reason_and_the_fix() {
    let s = with_loop(
        snapshot(),
        stopped(Some("max_ticks"), 14 * 60),
        ServeMode::Dispatch,
    );
    let want = "the loop stopped 14 min ago: max_ticks (633) — start it again: klobuk serve --dispatch";
    for view in View::ALL.iter().copied().filter(|v| *v != View::Now) {
        let a = app_on(view, s.clone());
        let line = line_of(&render_to_text(&a, 120, 30), 2);
        assert!(line.contains(want), "{view:?}: {line:?}");
        assert!(
            line.contains('■'),
            "{view:?}: the banner carries the attention mark"
        );
    }
    // a loop that never ran
    let never = with_loop(snapshot(), LoopState::NeverStarted, ServeMode::Dispatch);
    let line = line_of(&render_to_text(&app_on(View::Board, never), 120, 30), 2);
    assert!(
        line.contains("the loop is not running — start it: klobuk serve --dispatch"),
        "{line:?}"
    );
}

#[test]
fn a_running_loop_has_no_banner() {
    let s = with_loop(snapshot(), running(ServeMode::Dispatch, 5), ServeMode::Dispatch);
    for view in View::ALL {
        let text = render_to_text(&app_on(view, s.clone()), 120, 30);
        assert!(
            !text.contains("the loop is") && !text.contains("the loop stopped"),
            "{view:?}:\n{text}"
        );
    }
}

#[test]
fn what_waits_for_your_answer_outranks_the_loop_banner() {
    let mut s = with_loop(blocked_snapshot(), stopped(None, 600), ServeMode::Dispatch);
    s.derive();
    let line = line_of(&render_to_text(&app_on(View::Board, s), 120, 30), 2);
    assert!(line.contains("asf-2 is waiting for your answer"), "{line:?}");
    assert!(!line.contains("the loop"), "{line:?}");
}

#[test]
fn a_stuck_loop_names_what_it_waits_for() {
    let mut s = blocked_snapshot();
    s.serve = Some(LoopInfo {
        state: stopped(Some("stuck"), 60),
        mode: ServeMode::Dispatch,
    });
    s.derive();
    assert_eq!(
        s.loop_notice().as_deref(),
        Some("the loop stopped: stuck (asf-2 waits for your answer)")
    );
}

#[test]
fn the_now_view_lists_the_stopped_loop_as_the_first_needs_you_row() {
    let s = with_loop(
        busy_desk(),
        stopped(Some("max_ticks"), 14 * 60),
        ServeMode::Dispatch,
    );
    let a = app_on(View::Now, s);
    let text = render_to_text(&a, 140, 40);
    let lines: Vec<&str> = text.lines().collect();
    let head = lines.iter().position(|l| l.contains("NEEDS YOU (")).unwrap();
    let count = a.snap.needs_you.len() + 1;
    assert!(
        lines[head].contains(&format!("NEEDS YOU ({count})")),
        "the needs plus the loop: {}",
        lines[head]
    );
    let first = lines[head + 1];
    assert!(first.contains("loop") && first.contains("stopped"), "{first:?}");
    assert!(
        first.contains("max_ticks (633)") && first.contains("Enter explain"),
        "{first:?}"
    );
    assert!(
        first.contains("▸"),
        "the loop row is the selected one at first: {first:?}"
    );
    assert!(
        !text.contains("start it again: klobuk serve --dispatch\n\n"),
        "no banner row on the Now view"
    );
}

#[test]
fn the_selection_walks_the_loop_row_first_and_keys_do_not_leak_to_issue_actions() {
    let s = with_loop(busy_desk(), stopped(None, 900), ServeMode::Dispatch);
    let mut a = app_on(View::Now, s);
    let needs = a.snap.needs_you.len();
    assert_eq!(a.rows(View::Now), needs + 1 + now::running(&a.snap).len());
    assert!(matches!(a.selected_now_row(), Some(now::NowRow::Loop)));
    assert_eq!(a.selected_issue(), None);
    // a, f, v, r, x on the loop row only explain themselves
    for k in ['a', 'f', 'v', 'r', 'x'] {
        a.status = None;
        assert_eq!(a.handle_key(key(k)), Action::None, "{k}");
        let msg = a.status.clone().unwrap_or_default();
        assert!(msg.contains("loop"), "{k}: {msg:?}");
    }
    a.handle_key(key('j'));
    assert!(
        matches!(a.selected_now_row(), Some(now::NowRow::Need(_))),
        "the next row is the first need"
    );
}

#[test]
fn enter_on_the_loop_row_opens_the_explanation() {
    let s = with_loop(
        snapshot(),
        stopped(Some("max_ticks"), 14 * 60),
        ServeMode::Dispatch,
    );
    let mut a = app_on(View::Now, s);
    assert_eq!(a.handle_key(code(KeyCode::Enter)), Action::None);
    let Mode::Comment { comment: c, .. } = a.mode().clone() else {
        panic!("expected a window, got {:?}", a.mode());
    };
    assert!(
        c.body.starts_with("the loop stopped 14 min ago: max_ticks (633)"),
        "{}",
        c.body
    );
    assert!(
        c.body.contains("Start it again: klobuk serve --dispatch"),
        "{}",
        c.body
    );
    assert_eq!(c.author, "klobuk serve");
}

#[test]
fn a_running_loop_adds_no_row_to_the_now_view() {
    let plain = full_desk();
    let with = with_loop(
        plain.clone(),
        running(ServeMode::Dispatch, 4),
        ServeMode::Dispatch,
    );
    assert_eq!(now::rows(&with), now::rows(&plain));
    let text = render_to_text(&app_on(View::Now, with), 140, 40);
    assert!(!text.contains("Enter explain"));
}
