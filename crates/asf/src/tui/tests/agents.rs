//! The Agents view. Tests of it live here and nowhere else, so the task that rebuilds the view edits
//! this file only.

use super::*;
use crate::tui::snapshot::text_width;
use crate::tui::views::agents;

/// The three rows of the approved mockup, at [`NOW`] (12:00:00), pushed in an order that is not the
/// display order: asf-27 asks (since 11:56), asf-29 works, asf-30 reviews asf-28 (round 1).
fn agents_desk() -> Snapshot {
    let mut review = issue("asf-30", "Review asf-28: reload config", Status::InProgress);
    review.kind = IssueType::Review;
    review.parent_id = Some(id("asf-28"));
    let mut s = desk(vec![
        issue("asf-27", "Add `asf issue scope add/rm`", Status::InProgress),
        issue("asf-29", "Reviewer approval survives retry", Status::InProgress),
        issue("asf-28", "asf serve: reload config", Status::InProgress),
        review,
    ]);
    let mut reviewer = run_at("asf-30", RunState::Working, "2026-11-06T11:59:00Z");
    reviewer.cost_usd = 0.21;
    reviewer.last_seen_at = Some("2026-11-06T11:59:57Z".into());
    let mut worker = run_at("asf-29", RunState::Working, "2026-11-06T11:51:00Z");
    worker.cost_usd = 0.84;
    worker.last_seen_at = Some("2026-11-06T11:59:48Z".into());
    let mut asking = run_at("asf-27", RunState::Blocked, "2026-11-06T11:46:00Z");
    asking.cost_usd = 1.12;
    asking.last_seen_at = Some("2026-11-06T11:59:50Z".into());
    s.runs = vec![reviewer, worker, asking];
    s.events
        .push(state_event(1, "asf-27", "blocked", "2026-11-06T11:56:00Z"));
    s.events.push(Event {
        seq: 2,
        ts: "2026-11-06T11:59:00Z".into(),
        kind: "merge.review_started".into(),
        issue_id: Some(id("asf-28")),
        run_id: None,
        actor: None,
        payload: serde_json::json!({ "after_comment": 3, "round": 1 }),
    });
    s
}

fn agents_app(s: Snapshot) -> App {
    let mut a = App::new(s);
    a.view = View::Agents;
    a
}

/// The table row of `issue`: the screen line with its role next to it (the banner also names the issue).
fn agents_row(text: &str, issue: &str) -> String {
    text.lines()
        .find(|l| l.contains(&format!("{issue} ")) && (l.contains("worker") || l.contains("reviewer")))
        .unwrap_or_else(|| panic!("no agents row for {issue}:\n{text}"))
        .to_string()
}

/// The words of a screen line with every run of blanks collapsed, so a test reads cells, not padding.
fn words(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn agents_rows_follow_the_mockup_at_120_columns() {
    let a = agents_app(agents_desk());
    let t = render_to_text(&a, 120, 30);
    assert_eq!(
        words(&agents_row(&t, "asf-27")),
        "▸ ■ asf-27 worker asking Add `asf issue scope add/rm` $1.12 / $10 ▓░░░░░░░░░ 14 min asked 4 min ago",
        "{t}"
    );
    assert_eq!(
        words(&agents_row(&t, "asf-29")),
        "● asf-29 worker working Reviewer approval survives retry $0.84 / $10 ▓░░░░░░░░░ 9 min 12 s ago",
        "{t}"
    );
    assert_eq!(
        words(&agents_row(&t, "asf-30")),
        "◆ asf-30 reviewer reviewing asf serve: reload config · opus · r1 $0.21 / $10 ░░░░░░░░░░ 1 min 3 s ago",
        "{t}"
    );
    let header = t.lines().find(|l| l.contains("last activity")).unwrap();
    assert_eq!(
        words(header),
        "issue role state task cost / budget time last activity",
        "{t}"
    );
}

#[test]
fn agents_rows_fit_100_columns_and_cut_the_task_with_an_ellipsis() {
    let a = agents_app(agents_desk());
    let t = render_to_text(&a, 100, 30);
    for line in t.lines() {
        assert!(text_width(line) <= 100, "{line:?}");
    }
    let reviewer = agents_row(&t, "asf-30");
    assert!(reviewer.contains('…'), "{reviewer:?}");
    assert!(reviewer.trim_end().ends_with("3 s ago"), "{reviewer:?}");
    let asking = words(&agents_row(&t, "asf-27"));
    assert!(
        asking.starts_with("▸ ■ asf-27 worker asking Add `asf issue")
            && asking.ends_with("$1.12 / $10 ▓░░░░░░░░░ 14 min asked 4 min ago"),
        "{asking:?}"
    );
}

#[test]
fn agents_view_survives_the_smallest_screen() {
    let a = agents_app(agents_desk());
    let t = render_to_text(&a, 80, 20);
    assert_eq!(t.lines().count(), 20);
    assert!(agents_row(&t, "asf-27").contains("asking"), "{t}");
    for line in t.lines() {
        assert!(text_width(line) <= 80, "{line:?}");
    }
}

#[test]
fn rows_that_need_the_human_come_first_then_by_start_time() {
    let mut s = agents_desk();
    // the oldest run works; the youngest one is over budget
    let mut oldest = run_at("asf-29", RunState::Working, "2026-11-06T10:00:00Z");
    oldest.cost_usd = 0.84;
    s.runs.retain(|r| r.issue_id != id("asf-29"));
    s.runs.push(oldest);
    s.runs
        .push(run_at("asf-28", RunState::OverBudget, "2026-11-06T11:59:30Z"));
    let a = agents_app(s);
    let order: Vec<String> = (0..a.rows(View::Agents))
        .map(|i| {
            let mut b = agents_app(a.snap.clone());
            for _ in 0..i {
                b.handle_key(key('j'));
            }
            b.selected_issue().unwrap().to_string()
        })
        .collect();
    // asking (11:46) before over budget (11:59:30), then the working runs by start: 10:00, 11:59
    assert_eq!(order, ["asf-27", "asf-28", "asf-29", "asf-30"]);
    assert!(words(&agents_row(&render_to_text(&a, 120, 30), "asf-28")).contains("over_budget"));
}

#[test]
fn the_budget_bar_fills_one_cell_per_tenth_of_the_budget() {
    assert_eq!(agents::bar_cells(1.12, Some(10.0)), 1);
    assert_eq!(agents::bar_cells(0.84, Some(10.0)), 1);
    assert_eq!(agents::bar_cells(0.21, Some(10.0)), 0);
    assert_eq!(agents::bar_cells(5.0, Some(10.0)), 5);
    assert_eq!(agents::bar_cells(10.0, Some(10.0)), 10);
    assert_eq!(
        agents::bar_cells(25.0, Some(10.0)),
        10,
        "over budget stays 10 cells"
    );
    assert_eq!(agents::bar_cells(3.0, None), 0, "no budget, nothing to fill");
    assert_eq!(agents::bar_cells(3.0, Some(0.0)), 0);
}

#[test]
fn the_filled_cells_are_green_and_the_rest_is_dim() {
    let a = agents_app(agents_desk());
    let (fg, _) = cell_style(&a, "asf-27", "▓");
    assert_eq!(fg, Color::Green);
    let (_, m) = cell_style(&a, "asf-27", "░");
    assert!(m.contains(Modifier::DIM));
}

#[test]
fn symbols_carry_the_state_in_shape_and_colour() {
    let a = agents_app(agents_desk());
    let (fg, m) = cell_style(&a, "asking", "■");
    assert_eq!(fg, Color::Red);
    assert!(m.contains(Modifier::BOLD));
    assert_eq!(cell_style(&a, "working", "●").0, Color::Green);
    assert_eq!(cell_style(&a, "reviewing", "◆").0, Color::Yellow);
    let (_, m) = cell_style(&a, "working", "asf-29");
    assert!(m.contains(Modifier::BOLD), "issue ids are bold");
}

#[test]
fn the_selected_row_is_highlighted_and_reversed_without_colours() {
    let mut a = agents_app(agents_desk());
    let buf = render_to_buffer(&a, 120, 30);
    let y = (0..30u16)
        .find(|y| {
            let line: String = (0..120u16).map(|x| buf[(x, *y)].symbol().to_string()).collect();
            line.contains("asf-27 ") && line.contains("worker")
        })
        .unwrap();
    assert!(
        (0..120u16).any(|x| buf[(x, y)].bg != Color::Reset),
        "the selected row has a background"
    );
    a.theme = Theme::new(false);
    let plain = render_to_buffer(&a, 120, 30);
    assert!(
        (0..120u16).any(|x| plain[(x, y)].modifier.contains(Modifier::REVERSED)),
        "reverse video when NO_COLOR is set"
    );
}

#[test]
fn a_run_that_is_not_blocked_shows_its_last_seen_time() {
    let mut s = agents_desk();
    s.runs[1].last_seen_at = Some("2026-11-06T11:55:00Z".into()); // asf-29: 5 min
    s.runs[0].last_seen_at = None; // asf-30: never seen
    let t = render_to_text(&agents_app(s), 120, 30);
    assert!(words(&agents_row(&t, "asf-29")).ends_with("5 min ago"), "{t}");
    assert!(words(&agents_row(&t, "asf-30")).ends_with("1 min -"), "{t}");
}

#[test]
fn a_blocked_run_says_how_long_ago_it_asked() {
    let mut s = agents_desk();
    s.events.clear();
    s.events
        .push(state_event(1, "asf-27", "blocked", "2026-11-06T11:59:40Z"));
    let t = render_to_text(&agents_app(s), 120, 30);
    assert!(
        words(&agents_row(&t, "asf-27")).ends_with("asked <1 min ago"),
        "{t}"
    );
}

#[test]
fn the_reviewer_without_a_known_round_shows_task_and_model_only() {
    let mut s = agents_desk();
    s.events.retain(|e| e.kind != "merge.review_started");
    let t = render_to_text(&agents_app(s), 120, 30);
    let row = words(&agents_row(&t, "asf-30"));
    assert!(row.contains("asf serve: reload config · opus $0.21"), "{row}");
}

#[test]
fn a_failing_reviewer_keeps_its_problem_state_instead_of_reviewing() {
    let mut s = agents_desk();
    s.runs[0].state = RunState::Stalled; // asf-30
    let t = render_to_text(&agents_app(s), 120, 30);
    assert!(
        words(&agents_row(&t, "asf-30")).contains("reviewer stalled"),
        "{t}"
    );
}

#[test]
fn the_empty_view_says_what_to_do() {
    let a = agents_app(desk(vec![]));
    let t = render_to_text(&a, 120, 30);
    let sentence = "No worker is running. Start one from the Board with s, or run asf serve --dispatch.";
    assert!(t.contains(sentence), "{t}");
    assert!(cell_style(&a, sentence, sentence).1.contains(Modifier::DIM));
    assert!(!t.contains("pane preview"), "no table, no preview");
    assert_eq!(a.rows(View::Agents), 0);
    assert_eq!(a.selected_issue(), None);
}

// ---- the pane preview ----

fn pane_text(n: usize) -> String {
    (1..=n)
        .map(|i| format!("pane line {i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_preview_panel_is_titled_with_the_issue_and_the_session_name() {
    let mut a = agents_app(agents_desk());
    a.set_pane_preview(id("asf-27"), pane_text(3));
    let t = render_to_text(&a, 120, 30);
    assert!(
        t.contains("pane preview · asf-27 · asf-27-add-asf-issue-scope-add-rm · live"),
        "{t}"
    );
    assert!(t.contains("pane line 3"), "{t}");
    let top = t.lines().find(|l| l.contains("pane preview")).unwrap();
    assert!(top.contains('┌') || top.contains('╭'), "bordered panel: {top:?}");
}

#[test]
fn the_title_of_a_reviewer_uses_the_review_session_name() {
    let mut a = agents_app(agents_desk());
    a.handle_key(key('j'));
    a.handle_key(key('j'));
    assert_eq!(a.selected_issue(), Some(id("asf-30")));
    a.set_pane_preview(id("asf-30"), pane_text(2));
    let t = render_to_text(&a, 120, 30);
    assert!(
        t.contains("pane preview · asf-30 · review-asf-28-asf-serve-reload-config · live"),
        "{t}"
    );
}

#[test]
fn a_reviewer_without_a_review_link_falls_back_to_its_own_issue_for_the_session_name() {
    let mut a = agents_app(agents_desk());
    // `derive` rebuilds the links when the snapshot is loaded, so cut it afterwards
    a.snap.review_of.clear();
    a.handle_key(key('j'));
    a.handle_key(key('j'));
    assert_eq!(a.selected_issue(), Some(id("asf-30")));
    a.set_pane_preview(id("asf-30"), pane_text(2));
    let t = render_to_text(&a, 120, 30);
    assert!(
        t.contains("pane preview · asf-30 · review-asf-30-review-asf-28-reload-config · live"),
        "{t}"
    );
}

#[test]
fn session_names_follow_the_rule_of_the_worker() {
    assert_eq!(
        agents::session_name("asf-27", "Add asf issue scope add/rm and scope request"),
        "asf-27-add-asf-issue-scope-add-rm-and-scope-request"
    );
    assert_eq!(
        agents::session_name("review-asf-1", "Add Greeting"),
        "review-asf-1-add-greeting"
    );
    assert_eq!(
        agents::session_name("asf-1", "Zażółć gęślą jaźń 2"),
        "asf-1-za-g-l-ja-2"
    );
    assert_eq!(agents::session_name("asf-1", "!!!"), "asf-1");
    let long = agents::session_name("asf-123", &"word ".repeat(40));
    assert!(long.len() <= 60 && !long.ends_with('-'), "{long}");
}

#[test]
fn the_preview_shows_only_the_last_lines_that_fit() {
    let mut a = agents_app(agents_desk());
    a.set_pane_preview(id("asf-27"), format!("{}\n\n\n", pane_text(60)));
    let t = render_to_text(&a, 120, 30);
    assert!(t.contains("pane line 60"), "the newest line is shown:\n{t}");
    assert!(!t.contains("pane line 1 ") && !t.contains("pane line 2\n"), "{t}");
    assert!(!t.contains("pane line 10"), "{t}");
}

#[test]
fn a_long_pane_line_is_cut_and_control_characters_are_dropped() {
    let mut a = agents_app(agents_desk());
    a.set_pane_preview(
        id("asf-27"),
        format!("{}\n\u{1b}[31mred\u{1b}[0m", "x".repeat(300)),
    );
    let t = render_to_text(&a, 100, 30);
    for line in t.lines() {
        assert!(text_width(line) <= 100, "{line:?}");
    }
    assert!(t.contains('…'), "{t}");
    assert!(!t.contains('\u{1b}'), "no escape sequence reaches the screen");
}

#[test]
fn the_preview_waits_for_the_pane_of_the_selected_run() {
    let mut a = agents_app(agents_desk());
    a.set_pane_preview(id("asf-29"), pane_text(2)); // another run's pane
    let t = render_to_text(&a, 120, 30);
    assert!(t.contains("pane preview · asf-27"), "{t}");
    assert!(!t.contains("pane line 1"), "{t}");
    assert!(t.contains("No pane text has been read yet."), "{t}");
}

#[test]
fn when_the_runtime_is_down_the_preview_says_so_in_one_line() {
    let mut s = agents_desk();
    s.runtime_ok = Some(false);
    let mut a = agents_app(s);
    a.set_pane_preview(id("asf-27"), pane_text(2));
    let t = render_to_text(&a, 120, 30);
    assert!(
        t.contains("The runtime is down, so the pane cannot be read."),
        "{t}"
    );
    assert!(!t.contains("pane line 1"), "{t}");
}

#[test]
fn a_run_without_a_pane_says_so() {
    let mut s = agents_desk();
    s.runs[2].pane_id = None; // asf-27
    let t = render_to_text(&agents_app(s), 120, 30);
    assert!(t.contains("This run has no pane."), "{t}");
}

#[test]
fn a_long_list_scrolls_with_the_selection_and_leaves_room_for_the_preview() {
    let mut s = desk(
        (1..=14)
            .map(|n| issue(&format!("asf-{n}"), "Work", Status::InProgress))
            .collect(),
    );
    s.runs = (1..=14)
        .map(|n| {
            run_at(
                &format!("asf-{n}"),
                RunState::Working,
                &format!("2026-11-06T11:{:02}:00Z", 10 + n),
            )
        })
        .collect();
    let mut a = agents_app(s);
    a.set_pane_preview(id("asf-1"), pane_text(3));
    let t = render_to_text(&a, 100, 24);
    assert!(t.contains("pane preview · asf-1"), "{t}");
    assert!(t.contains("pane line 3"), "{t}");
    assert!(
        !t.contains("asf-14 worker"),
        "the last rows are scrolled out:\n{t}"
    );
    a.handle_key(key('G'));
    let t = render_to_text(&a, 100, 24);
    assert!(
        words(&agents_row(&t, "asf-14")).starts_with("▸ ● asf-14 worker"),
        "{t}"
    );
    assert!(t.contains("pane preview · asf-14"), "{t}");
}

// ---- keys ----

#[test]
fn moving_the_selection_asks_for_the_pane_of_the_new_row() {
    let mut a = agents_app(agents_desk());
    assert_eq!(a.handle_key(key('j')), Action::Peek(id("asf-29")));
    assert_eq!(a.selected_issue(), Some(id("asf-29")));
    assert_eq!(a.handle_key(key('G')), Action::Peek(id("asf-30")));
    assert_eq!(a.handle_key(key('G')), Action::None, "same row, nothing to read");
    assert_eq!(a.handle_key(key('g')), Action::Peek(id("asf-27")));
    assert_eq!(a.handle_key(code(KeyCode::Down)), Action::Peek(id("asf-29")));
    assert_eq!(a.handle_key(code(KeyCode::Up)), Action::Peek(id("asf-27")));
}

#[test]
fn a_answers_the_blocked_run_and_says_why_it_cannot_otherwise() {
    let mut a = agents_app(agents_desk());
    assert_eq!(
        a.handle_key(key('a')),
        Action::Peek(id("asf-27")),
        "opens the answer overlay"
    );
    assert!(matches!(a.mode(), Mode::Answer { .. }));
    let mut b = agents_app(agents_desk());
    b.handle_key(key('j'));
    assert_eq!(b.handle_key(key('a')), Action::None);
    assert_eq!(b.status.as_deref(), Some("asf-29 has no blocked run to answer"));
}

#[test]
fn f_focuses_the_pane_of_the_selected_run() {
    let mut a = agents_app(agents_desk());
    a.handle_key(key('j'));
    assert_eq!(a.handle_key(key('f')), Action::Focus(id("asf-29")));
}

#[test]
fn r_opens_the_reply_line_for_the_selected_run() {
    let mut a = agents_app(agents_desk());
    assert_eq!(a.handle_key(key('r')), Action::None);
    assert!(
        matches!(a.mode(), Mode::Input { target: InputTarget::Reply(i), .. } if *i == id("asf-27")),
        "{:?}",
        a.mode()
    );
}

#[test]
fn x_asks_before_it_aborts() {
    let mut a = agents_app(agents_desk());
    assert_eq!(a.handle_key(key('x')), Action::None);
    assert!(matches!(a.mode(), Mode::Confirm { what, .. } if what == "abort asf-27?"));
    assert_eq!(a.handle_key(key('y')), Action::Abort(id("asf-27")));
}

#[test]
fn enter_opens_the_issue_on_the_board() {
    let mut a = agents_app(agents_desk());
    a.handle_key(key('G'));
    assert_eq!(a.handle_key(code(KeyCode::Enter)), Action::None);
    assert_eq!(a.view, View::Board);
    assert_eq!(a.selected_issue(), Some(id("asf-30")));
}

#[test]
fn the_keys_do_nothing_when_no_worker_runs() {
    let mut a = agents_app(desk(vec![]));
    for c in ['a', 'f', 'r', 'x', 'j', 'k'] {
        assert_eq!(a.handle_key(key(c)), Action::None, "{c}");
    }
    assert_eq!(a.handle_key(code(KeyCode::Enter)), Action::None);
    assert_eq!(a.view, View::Agents);
    assert!(matches!(a.mode(), Mode::Normal));
}

#[test]
fn the_footer_lists_the_keys_of_the_mockup() {
    let a = agents_app(agents_desk());
    let footer = words(&footer_line(&a, 120));
    assert!(
        footer.contains("a answer f focus pane r reply x abort Enter open issue"),
        "{footer}"
    );
    assert!(footer.ends_with("? help q quit"), "{footer}");
}

#[test]
fn the_help_page_lists_every_key_of_the_view() {
    let help = crate::tui::bindings::help_sections();
    let keys: Vec<&str> = help
        .iter()
        .flat_map(|s| s.items.iter().map(|(k, _)| *k))
        .collect();
    for k in ["a", "f", "r", "x", "Enter"] {
        assert!(keys.contains(&k), "{k}: {keys:?}");
    }
}

#[test]
fn the_selection_stays_inside_the_rows_when_a_run_ends() {
    let mut a = agents_app(agents_desk());
    a.handle_key(key('G'));
    let mut s = agents_desk();
    s.runs.truncate(1); // only asf-30 is left
    a.replace(s);
    assert_eq!(a.selected(View::Agents), 0);
    assert_eq!(a.selected_issue(), Some(id("asf-30")));
}

// ---- reading the pane on the event loop's behalf ----

#[test]
fn the_loop_is_asked_to_read_the_pane_of_the_selected_run_while_the_view_is_open() {
    let mut a = agents_app(agents_desk());
    assert_eq!(a.peek_wanted(), Some(id("asf-27")), "nothing read yet");
    a.set_pane_preview(id("asf-27"), pane_text(2));
    assert_eq!(a.peek_wanted(), None, "read, nothing more to do");
    a.handle_key(key('j'));
    assert_eq!(a.peek_wanted(), Some(id("asf-29")), "another run is selected");
    a.view = View::Board;
    assert_eq!(a.peek_wanted(), None, "only while the Agents view is open");
}

#[test]
fn a_store_refresh_makes_the_pane_stale_but_keeps_it_on_screen() {
    let mut a = agents_app(agents_desk());
    a.set_pane_preview(id("asf-27"), pane_text(2));
    a.replace(agents_desk());
    assert_eq!(
        a.peek_wanted(),
        Some(id("asf-27")),
        "read it again after a refresh"
    );
    assert!(
        render_to_text(&a, 120, 30).contains("pane line 2"),
        "no blink while it loads"
    );
    a.set_pane_preview(id("asf-27"), pane_text(3));
    assert_eq!(a.peek_wanted(), None);
}

#[test]
fn nothing_is_read_when_the_runtime_is_down_or_no_worker_runs() {
    let mut s = agents_desk();
    s.runtime_ok = Some(false);
    assert_eq!(
        agents_app(s).peek_wanted(),
        None,
        "or the loop would retry on every pass"
    );
    assert_eq!(agents_app(desk(vec![])).peek_wanted(), None);
}
