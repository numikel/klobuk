use super::*;

#[test]
fn every_needs_you_source_has_its_row_with_label_first_line_age_and_key() {
    let a = App::new(busy_desk());
    let t = render_to_text(&a, 120, 30);
    assert!(t.contains("NEEDS YOU (8)"), "{t}");
    let expected = [
        (
            "asf-1",
            "question",
            "Allow Bash(rm -rf target)?",
            "10 min",
            "a answer",
        ),
        (
            "asf-8",
            "question",
            "waiting for your answer",
            "5 min",
            "a answer",
        ),
        (
            "asf-2",
            "question",
            "Keep the old --scope flag as an alias?",
            "4 min",
            "a answer",
        ),
        ("asf-3", "failed", "gate `test` failed", "40 min", "v verify"),
        ("asf-4", "stalled", "no output for 10 minutes", "1 h", "a answer"),
        (
            "asf-5",
            "over_budget",
            "no exit reason recorded",
            "2 h",
            "x abort",
        ),
        (
            "asf-6",
            "merge failed",
            "gate `test` after rebase (2 tests)",
            "12 min",
            "r retry",
        ),
        ("asf-7", "merge blocked", "no error recorded", "<1 min", "r retry"),
    ];
    for (issue, label, text, age, key) in expected {
        let row = row_of(&t, issue);
        for part in [label, text, age, key] {
            assert!(row.contains(part), "{issue}: {part:?} missing in {row:?}");
        }
        assert!(
            row.trim_end().ends_with(key),
            "{issue}: the key closes the row: {row:?}"
        );
        assert!(
            !row.contains("second line") && !row.contains("more output"),
            "{issue}: only the first line: {row:?}"
        );
    }
}

#[test]
fn needs_you_is_ordered_oldest_first_and_the_screen_agrees() {
    let s = busy_desk();
    let order: Vec<String> = needs(&s).iter().map(|n| n.issue.to_string()).collect();
    assert_eq!(
        order,
        ["asf-5", "asf-4", "asf-3", "asf-6", "asf-1", "asf-8", "asf-2", "asf-7"]
    );
    let t = render_to_text(&App::new(s), 120, 30);
    let at = |issue: &str| {
        t.find(&format!("■ {issue} "))
            .unwrap_or_else(|| panic!("{issue}:\n{t}"))
    };
    let positions: Vec<usize> = order.iter().map(|i| at(i)).collect();
    assert!(positions.windows(2).all(|w| w[0] < w[1]), "{t}");
    assert!(
        line_of(&t, 0).contains("8 waiting"),
        "the header counts the same rows:\n{t}"
    );
}

#[test]
fn a_blocked_stalled_or_over_budget_run_is_listed_once_in_needs_you_and_not_in_running() {
    let s = busy_desk();
    assert!(now::running(&s).is_empty());
    let t = render_to_text(&App::new(s), 120, 30);
    assert!(t.contains("RUNNING (0)"), "{t}");
    assert_eq!(t.matches("asf-5 ").count(), 1, "{t}");
}

#[test]
fn empty_sections_say_what_to_do_next_in_the_dim_style() {
    let a = App::new(desk(vec![]));
    let t = render_to_text(&a, 120, 30);
    assert!(t.contains("NEEDS YOU (0)") && t.contains("RUNNING (0)"), "{t}");
    assert!(t.contains("Nothing needs you."), "{t}");
    assert!(
        t.contains("No worker is running. Press Tab for the Board to start one."),
        "{t}"
    );
    for text in ["Nothing needs you.", "No worker is running."] {
        let (_, m) = cell_style(&a, text, text);
        assert!(m.contains(Modifier::DIM), "{text} is dim");
    }
    assert!(t.contains("UP NEXT (0)") && t.contains("DONE TODAY (0)"), "{t}");
}

#[test]
fn running_rows_show_state_title_cost_over_budget_and_elapsed_reviewers_the_model_and_round() {
    let a = App::new(working_desk());
    let t = render_to_text(&a, 120, 30);
    assert!(t.contains("RUNNING (2)") && t.contains("NEEDS YOU (0)"), "{t}");
    let w = row_of(&t, "asf-24");
    for part in [
        "● asf-24",
        "working",
        "Strip worker env vars from cli_export tests",
        "$0.50 / $10",
        "6 min",
    ] {
        assert!(w.contains(part), "{part:?} missing in {w:?}");
    }
    assert!(w.contains("$0.50 / $10 "), "a whole budget has no cents: {w:?}");
    let r = row_of(&t, "asf-31");
    for part in ["◆ asf-31", "review", "opus · round 1", "$0.21 / $10 ", "1 min"] {
        assert!(r.contains(part), "{part:?} missing in {r:?}");
    }
    assert!(
        !r.contains("working") && !r.contains("Review asf-24"),
        "a reviewer shows `review` and the model, not the run state or the title: {r:?}"
    );
    // a budget with cents keeps them, a run without one shows only the cost
    let mut s = working_desk();
    s.runs[0].budget_usd = Some(2.5);
    s.runs[1].budget_usd = None;
    let t = render_to_text(&App::new(s), 120, 30);
    assert!(row_of(&t, "asf-24").contains("$0.50 / $2.50"), "{t}");
    let r = row_of(&t, "asf-31");
    assert!(r.contains("$0.21") && !r.contains(" / "), "{r:?}");
}

#[test]
fn up_next_counts_ready_and_backlog_names_the_first_ids_and_the_shared_dependency() {
    let mut issues = vec![issue("asf-24", "Running", Status::InProgress)];
    for n in 40..46 {
        issues.push(issue(&format!("asf-{n}"), "Later", Status::Backlog));
    }
    let mut s = desk(issues);
    s.deps = (40..46).map(|n| dep(&format!("asf-{n}"), "asf-24")).collect();
    let t = render_to_text(&App::new(s.clone()), 120, 30);
    assert!(
        t.contains("UP NEXT (6)   asf-40, asf-41, asf-42 … waiting for asf-24"),
        "{t}"
    );
    // a ready issue that waits for nothing breaks the sharing; epics do not count
    s.issues.push(issue("asf-50", "Go", Status::Ready));
    let mut epic = issue("asf-51", "Epic", Status::Backlog);
    epic.kind = IssueType::Epic;
    s.issues.push(epic);
    let next = now::up_next(&s);
    assert_eq!(next.count, 7);
    assert_eq!(
        next.ids,
        [id("asf-50"), id("asf-40"), id("asf-41")],
        "ready first"
    );
    assert_eq!(next.waits_for, None);
    // two dependencies shared by everyone are not "one dependency"
    let mut two = desk(vec![
        issue("asf-1", "a", Status::InProgress),
        issue("asf-2", "b", Status::InProgress),
        issue("asf-3", "c", Status::Backlog),
    ]);
    two.deps = vec![dep("asf-3", "asf-1"), dep("asf-3", "asf-2")];
    assert_eq!(now::up_next(&two).waits_for, None);
    // a finished dependency waits for nothing
    two.issues[0].status = Status::Done;
    assert_eq!(now::up_next(&two).waits_for, Some(id("asf-2")));
}

#[test]
fn done_today_lists_ids_closed_on_the_local_date_newest_first_and_truncates_with_an_ellipsis() {
    let closed = |n: u32, at: &str| {
        let mut i = issue(&format!("asf-{n}"), "done", Status::Done);
        i.closed_at = Some(at.into());
        i
    };
    let mut s = desk(vec![
        closed(20, "2026-11-06T09:00:00Z"),
        closed(22, "2026-11-06T11:00:00Z"),
        closed(21, "2026-11-06T10:00:00Z"),
        closed(19, "2026-11-05T09:00:00Z"),
        closed(18, "2026-11-05T23:30:00Z"),
    ]);
    let mut review = closed(23, "2026-11-06T11:30:00Z");
    review.kind = IssueType::Review;
    s.issues.push(review);
    let ids: Vec<String> = now::done_today(&s).iter().map(ToString::to_string).collect();
    assert_eq!(
        ids,
        ["asf-22", "asf-21", "asf-20"],
        "UTC date; reviews are internal"
    );
    let t = render_to_text(&App::new(s.clone()), 120, 30);
    assert!(
        t.contains("DONE TODAY (3)   ✓ asf-22  ✓ asf-21  ✓ asf-20") && !t.contains("asf-20 …"),
        "{t}"
    );
    // one hour east of UTC, 23:30Z on the 5th is already the 6th
    s.utc_offset_secs = 3600;
    let ids: Vec<String> = now::done_today(&s).iter().map(ToString::to_string).collect();
    assert_eq!(ids, ["asf-22", "asf-21", "asf-20", "asf-18"]);
    // many ids do not fit one line: cut with … and the count stays whole
    for n in 100..140 {
        s.issues
            .push(closed(n, &format!("2026-11-06T11:{:02}:00Z", n - 100)));
    }
    let t = render_to_text(&App::new(s), 80, 24);
    let line = t.lines().find(|l| l.contains("DONE TODAY")).unwrap();
    assert!(
        line.contains("DONE TODAY (44)") && line.trim_end().ends_with('…'),
        "{line:?}"
    );
    assert!(line.chars().count() <= 80, "{line:?}");
}

#[test]
fn ages_are_short() {
    assert_eq!(age(NOW, "2026-11-06T11:59:30Z"), "<1 min");
    assert_eq!(age(NOW, "2026-11-06T11:59:00Z"), "1 min");
    assert_eq!(age(NOW, "2026-11-06T11:00:01Z"), "59 min");
    assert_eq!(age(NOW, "2026-11-06T09:00:00Z"), "3 h");
    assert_eq!(age(NOW, "2026-11-04T12:00:00Z"), "2 d");
    assert_eq!(
        age(NOW, "2026-11-06T12:00:30Z"),
        "<1 min",
        "the future is not negative"
    );
    assert_eq!(age("", NOW), "-");
    assert_eq!(age(NOW, "yesterday"), "-");
}

#[test]
fn the_blocker_screen_comes_from_the_fenced_block_after_screen() {
    let mut s = busy_desk();
    let first = |s: &Snapshot, issue: &str| needs(s).into_iter().find(|n| n.issue == id(issue)).unwrap().text;
    assert_eq!(first(&s, "asf-1"), "Allow Bash(rm -rf target)?");
    // the newest BLOCKER wins, and one without a screen falls back
    let mut newer = comment(
        "asf-1",
        CommentType::Blocker,
        "Over budget.\nNo screen in this one.",
    );
    newer.id = 9;
    s.blockers.push(newer);
    assert_eq!(first(&s, "asf-1"), "waiting for your answer");
    // the fixture of the older tests has no fence either
    let mut plain = snapshot();
    plain.runs[0].state = RunState::Blocked;
    plain.blockers = plain.inbox.clone();
    assert_eq!(first(&plain, "asf-2"), "waiting for your answer");
}

#[test]
fn symbols_differ_per_state_and_carry_their_colours() {
    let a = App::new(full_desk());
    // Now: ■ red bold, ● green, ◆ yellow
    let (fg, m) = cell_style(&a, "asf-5 ", "■");
    assert_eq!(fg, Color::Red);
    assert!(m.contains(Modifier::BOLD));
    assert_eq!(cell_style(&a, "asf-24 ", "●").0, Color::Green);
    assert_eq!(cell_style(&a, "asf-31 ", "◆").0, Color::Yellow);
    // Board: the same symbols, plus ✓ dim and ○ in the default colour (a roomier snapshot, so every
    // card fits on screen)
    let mut s = working_desk();
    s.issues.push(issue("asf-5", "Over budget", Status::InProgress));
    s.runs
        .push(run_at("asf-5", RunState::OverBudget, "2026-11-06T10:00:00Z"));
    s.issues.push(issue("asf-60", "Validating", Status::Validating));
    s.issues.push(issue("asf-61", "Waits", Status::Ready));
    s.issues.push(issue("asf-62", "Shipped", Status::Done));
    let mut b = board_app(s);
    assert_eq!(cell_style(&b, "asf-24", "●").0, Color::Green);
    assert_eq!(cell_style(&b, "asf-31", "◆").0, Color::Yellow);
    assert_eq!(cell_style(&b, "asf-60", "◆").0, Color::Yellow);
    assert_eq!(cell_style(&b, "asf-5   in-progress", "■").0, Color::Red);
    let (_, m) = cell_style(&b, "asf-62", "✓");
    assert!(m.contains(Modifier::DIM), "done is dim");
    assert_eq!(cell_style(&b, "asf-61", "○").0, Color::Reset);
    // Agents: a working run, a reviewer and a run that needs you each have their own symbol
    b.handle_key(key('3'));
    let t = render_to_text(&b, 120, 30);
    for (issue, glyph) in [("asf-24", '●'), ("asf-31", '◆'), ("asf-5", '■')] {
        let row = t
            .lines()
            .find(|l| l.contains(&format!("{issue} ")) && (l.contains("worker") || l.contains("reviewer")))
            .unwrap_or_else(|| panic!("no agents row for {issue}:\n{t}"));
        assert!(row.contains(glyph), "{issue}: {row:?}");
    }
}

#[test]
fn no_color_removes_every_colour_but_keeps_bold_dim_and_reverse() {
    let mut a = App::new(full_desk());
    let coloured = render_to_buffer(&a, 120, 30);
    let has_colour = |b: &ratatui::buffer::Buffer| {
        b.area
            .positions()
            .any(|p| b[p].fg != Color::Reset || b[p].bg != Color::Reset)
    };
    assert!(has_colour(&coloured), "the control render is in colour");
    a.theme = Theme::new(false);
    let plain = render_to_buffer(&a, 120, 30);
    assert!(!has_colour(&plain), "{}", render_to_text(&a, 120, 30));
    let modifier_at = |needle: &str| cell_style(&a, needle, needle).1;
    assert!(modifier_at("NEEDS YOU").contains(Modifier::BOLD));
    assert!(
        modifier_at("■").contains(Modifier::BOLD),
        "the red square stays bold"
    );
    assert!(
        modifier_at("▸").contains(Modifier::REVERSED),
        "the selected row stays reversed"
    );
    // the same on every view and with a modal open
    for c in ['2', '3', '4', '5', '6', '7', '?'] {
        a.handle_key(key(c));
        assert!(!has_colour(&render_to_buffer(&a, 120, 30)), "view {:?}", a.view);
    }
    // the entry points read NO_COLOR with the no-color.org rule: set and non-empty
    assert!(!no_color_requested(None));
    assert!(!no_color_requested(Some(std::ffi::OsStr::new(""))));
    assert!(no_color_requested(Some(std::ffi::OsStr::new("1"))));
}

#[test]
fn below_80_columns_or_20_rows_one_centred_line_replaces_every_view() {
    for (w, h) in [(79u16, 24u16), (80, 19), (100, 5), (60, 10)] {
        for view in View::ALL {
            let mut a = App::new(full_desk());
            a.view = view;
            let t = render_to_text(&a, w, h);
            let lines: Vec<(usize, &str)> = t
                .lines()
                .enumerate()
                .filter(|(_, l)| !l.trim().is_empty())
                .collect();
            let want = format!("Terminal too small (need {MIN_WIDTH}x{MIN_HEIGHT}, have {w}x{h}).");
            assert_eq!(lines.len(), 1, "{view:?} at {w}x{h}:\n{t}");
            assert_eq!(lines[0].1.trim(), want);
            let lead = lines[0].1.len() - lines[0].1.trim_start().len();
            assert!(
                lead.abs_diff((w as usize - want.len()) / 2) <= 1,
                "centred across: {lead}"
            );
            assert!(
                lines[0].0.abs_diff(h as usize / 2) <= 1,
                "centred down: row {}",
                lines[0].0
            );
        }
    }
    // a modal does not survive it either
    let mut a = App::new(full_desk());
    a.handle_key(key('?'));
    let lines = render_to_text(&a, 79, 24)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    assert_eq!(lines, 1);
    // exactly 80x20 is large enough
    let t = render_to_text(&App::new(full_desk()), 80, 20);
    assert!(t.contains("NEEDS YOU") && !t.contains("too small"), "{t}");
}

#[test]
fn j_and_k_walk_the_needs_you_rows_and_then_the_running_rows() {
    let mut a = App::new(full_desk());
    let needs = needs(&a.snap).len();
    assert_eq!(needs, 8);
    assert_eq!(a.rows(View::Now), 10);
    assert_eq!(a.selected(View::Now), 0);
    for _ in 0..needs {
        a.handle_key(key('j'));
    }
    assert!(
        matches!(a.selected_now_row(), Some(now::NowRow::Running(r)) if r.issue == id("asf-24")),
        "{:?}",
        a.selected_now_row()
    );
    a.handle_key(key('G'));
    assert!(matches!(a.selected_now_row(), Some(now::NowRow::Running(r)) if r.issue == id("asf-31")));
    a.handle_key(key('j'));
    assert_eq!(a.selected(View::Now), 9, "clamped at the last row");
    a.handle_key(key('g'));
    assert!(matches!(a.selected_now_row(), Some(now::NowRow::Need(n)) if n.issue == id("asf-5")));
    // the selected row carries the marker and no other row does
    let t = render_to_text(&a, 120, 30);
    assert_eq!(t.matches('▸').count(), 1, "{t}");
    assert!(row_of(&t, "asf-5").contains("▸ ■ asf-5"), "{t}");
    // a refresh that drops rows keeps the selection inside
    a.handle_key(key('G'));
    a.replace(busy_desk());
    assert_eq!(a.selected(View::Now), 7);
    a.replace(desk(vec![]));
    assert_eq!(a.selected(View::Now), 0);
    assert_eq!(a.selected_now_row(), None);
}

#[test]
fn enter_switches_to_the_board_with_that_issue_selected() {
    let mut a = App::new(full_desk());
    for _ in 0..3 {
        a.handle_key(key('j'));
    }
    assert!(matches!(a.selected_now_row(), Some(now::NowRow::Need(n)) if n.issue == id("asf-6")));
    assert_eq!(a.handle_key(code(KeyCode::Enter)), Action::None);
    assert_eq!(a.view, View::Board);
    assert_eq!(a.selected_issue(), Some(id("asf-6")));
    // a running row too
    let mut b = App::new(full_desk());
    b.handle_key(key('G'));
    b.handle_key(code(KeyCode::Enter));
    assert_eq!(b.view, View::Board);
    assert_eq!(b.selected_issue(), Some(id("asf-31")));
    // nothing selected: Enter does nothing
    let mut e = App::new(desk(vec![]));
    assert_eq!(e.handle_key(code(KeyCode::Enter)), Action::None);
    assert_eq!(e.view, View::Now);
}

#[test]
fn f_focuses_the_pane_of_the_selected_row() {
    let mut a = App::new(full_desk());
    assert_eq!(a.handle_key(key('f')), Action::Focus(id("asf-5")));
    a.handle_key(key('G'));
    assert_eq!(a.handle_key(key('f')), Action::Focus(id("asf-31")));
    assert_eq!(App::new(desk(vec![])).handle_key(key('f')), Action::None);
}

#[test]
fn a_answers_a_blocked_worker_with_the_overlay_and_a_question_with_a_typed_answer() {
    let mut a = App::new(busy_desk());
    select_need(&mut a, "asf-1");
    assert_eq!(a.handle_key(key('a')), Action::Peek(id("asf-1")));
    assert!(matches!(&a.mode(), Mode::Answer { id: i, .. } if *i == id("asf-1")));
    a.handle_key(code(KeyCode::Esc));

    select_need(&mut a, "asf-2");
    assert_eq!(a.handle_key(key('a')), Action::None);
    match &a.mode() {
        Mode::Input { prompt, target, .. } => {
            assert_eq!(prompt, "answer asf-2 (#7)");
            assert_eq!(*target, InputTarget::Answer(id("asf-2"), 7));
        }
        other => panic!("expected the answer input, got {other:?}"),
    }
    for c in "keep it".chars() {
        a.handle_key(key(c));
    }
    assert_eq!(
        a.handle_key(code(KeyCode::Enter)),
        Action::Answer(id("asf-2"), 7, "keep it".into())
    );
    assert!(matches!(a.mode(), Mode::Normal));
    // an empty answer sends nothing; Esc cancels
    select_need(&mut a, "asf-2");
    a.handle_key(key('a'));
    assert_eq!(a.handle_key(code(KeyCode::Enter)), Action::None);
    select_need(&mut a, "asf-2");
    a.handle_key(key('a'));
    a.handle_key(key('x'));
    assert_eq!(a.handle_key(code(KeyCode::Esc)), Action::None);
    assert!(matches!(a.mode(), Mode::Normal));

    // a row that is not a question says which key it takes
    select_need(&mut a, "asf-3");
    assert_eq!(a.handle_key(key('a')), Action::None);
    assert!(matches!(a.mode(), Mode::Normal));
    assert!(
        a.status.as_deref().unwrap_or("").contains("v verify"),
        "{:?}",
        a.status
    );
}

#[test]
fn a_answers_a_stalled_worker_with_the_overlay_because_a_reply_moves_it_back_to_working() {
    let mut a = App::new(busy_desk());
    select_need(&mut a, "asf-4");
    assert_eq!(a.handle_key(key('a')), Action::Peek(id("asf-4")));
    assert!(matches!(&a.mode(), Mode::Answer { id: i, .. } if *i == id("asf-4")));
    // the over-budget row does not take `a`: it says which key it does take
    a.handle_key(code(KeyCode::Esc));
    select_need(&mut a, "asf-5");
    assert_eq!(a.handle_key(key('a')), Action::None);
    assert!(matches!(a.mode(), Mode::Normal));
    assert!(
        a.status.as_deref().unwrap_or("").contains("x abort"),
        "{:?}",
        a.status
    );
}

#[test]
fn v_reruns_verification_of_a_failed_run_in_the_background_and_of_nothing_else() {
    let mut a = App::new(busy_desk());
    select_need(&mut a, "asf-3");
    assert_eq!(a.handle_key(key('v')), Action::Verify(id("asf-3")));
    // `asf run verify` refuses a stalled or over-budget run (worker still active), so `v` never
    // hands it one; the status names the key the row does take
    for (issue, key_hint) in [
        ("asf-4", "a answer"),
        ("asf-5", "x abort"),
        ("asf-1", "a answer"),
        ("asf-2", "a answer"),
        ("asf-6", "r retry"),
    ] {
        let mut a = App::new(busy_desk());
        select_need(&mut a, issue);
        assert_eq!(a.handle_key(key('v')), Action::None, "{issue}");
        let status = a.status.clone().unwrap_or_default();
        assert!(
            status.starts_with(&format!("{issue}: v verifies a failed run")),
            "{status}"
        );
        assert!(status.contains(key_hint), "{issue}: {status}");
        assert!(matches!(a.mode(), Mode::Normal));
    }
    let mut a = App::new(busy_desk());
    // one background action at a time, like tick and start
    select_need(&mut a, "asf-3");
    a.busy = Some("tick".into());
    assert_eq!(a.handle_key(key('v')), Action::None);
    assert_eq!(a.status.as_deref(), Some("busy: tick"));
}

#[test]
fn r_requeues_a_failed_or_blocked_merge_item_in_the_background_and_never_opens_a_reply() {
    for issue in ["asf-6", "asf-7"] {
        let mut a = App::new(busy_desk());
        select_need(&mut a, issue);
        assert_eq!(a.handle_key(key('r')), Action::Retry(id(issue)), "{issue}");
        assert!(matches!(a.mode(), Mode::Normal));
    }
    let mut a = App::new(busy_desk());
    for issue in ["asf-1", "asf-3", "asf-2"] {
        select_need(&mut a, issue);
        assert_eq!(a.handle_key(key('r')), Action::None, "{issue}");
        assert!(
            matches!(a.mode(), Mode::Normal),
            "{issue}: r is retry here, not reply"
        );
        assert!(
            a.status.as_deref().unwrap_or("").contains("r retries"),
            "{:?}",
            a.status
        );
    }
    select_need(&mut a, "asf-6");
    a.busy = Some("verifying asf-3".into());
    assert_eq!(a.handle_key(key('r')), Action::None);
    assert_eq!(a.status.as_deref(), Some("busy: verifying asf-3"));
}

#[test]
fn x_asks_before_aborting_an_over_budget_run_and_is_refused_on_every_other_row() {
    let mut a = App::new(busy_desk());
    select_need(&mut a, "asf-5");
    assert_eq!(a.handle_key(key('x')), Action::None);
    assert!(
        matches!(&a.mode(), Mode::Confirm { what, .. } if what == "abort asf-5?"),
        "{:?}",
        a.mode()
    );
    // anything but `y` cancels; `y` aborts
    assert_eq!(a.handle_key(key('n')), Action::None);
    select_need(&mut a, "asf-5");
    a.handle_key(key('x'));
    assert_eq!(a.handle_key(key('y')), Action::Abort(id("asf-5")));

    for issue in ["asf-1", "asf-2", "asf-3", "asf-4", "asf-6"] {
        let mut a = App::new(busy_desk());
        select_need(&mut a, issue);
        assert_eq!(a.handle_key(key('x')), Action::None, "{issue}");
        assert!(matches!(a.mode(), Mode::Normal), "{issue}: {:?}", a.mode());
        let status = a.status.clone().unwrap_or_default();
        assert!(
            status.starts_with(&format!("{issue}: x aborts an over-budget run")),
            "{status}"
        );
    }
    // a running worker is not a NEEDS YOU row
    let mut r = App::new(full_desk());
    r.handle_key(key('G'));
    assert_eq!(r.handle_key(key('x')), Action::None);
    assert!(matches!(r.mode(), Mode::Normal));
}

#[test]
fn the_key_on_a_needs_you_row_is_never_one_the_command_behind_it_refuses() {
    use RunState::*;
    let states = [
        Starting, Working, Blocked, Stalled, OverBudget, Finishing, Verifying, Succeeded, Failed, Aborted,
    ];
    let mut listed = Vec::new();
    for state in states {
        let mut s = desk(live(&["asf-1"]));
        let mut r = run_at("asf-1", state, "2026-11-06T11:00:00Z");
        r.exit_reason = Some("why".into());
        if state == Failed {
            s.failed_runs.push(r); // a failed run is not active
        } else {
            s.runs.push(r);
        }
        let Some(need) = needs(&s).into_iter().next() else {
            continue;
        };
        listed.push(state);
        assert!(
            accepted_by_the_command(need.act, state),
            "{state:?}: the row advertises `{}` but its command refuses that state",
            need.act.hint()
        );
        // and pressing it does something: an action, an overlay or a confirmation
        let mut a = App::new(s);
        let action = a.handle_key(key(need.act.key()));
        let did_something = action != Action::None || !matches!(a.mode(), Mode::Normal);
        assert!(did_something, "{state:?}: `{}` did nothing", need.act.hint());
        assert!(a.status.is_none(), "{state:?}: {:?}", a.status);
    }
    // the states that need the human, nothing more and nothing less
    assert_eq!(listed, [Blocked, Stalled, OverBudget, Failed]);
}

#[test]
fn the_keys_of_each_run_state_are_verify_for_failed_answer_for_stalled_abort_for_over_budget() {
    let acts: Vec<(String, Act)> = needs(&busy_desk())
        .into_iter()
        .filter(|n| matches!(n.source, NeedSource::Run(_)))
        .map(|n| (n.label, n.act))
        .collect();
    assert_eq!(
        acts,
        [
            ("over_budget".to_string(), Act::Abort),
            ("stalled".to_string(), Act::Answer),
            ("failed".to_string(), Act::Verify),
        ]
    );
}

#[test]
fn the_background_children_of_v_and_r_run_the_cli_and_report_their_envelopes() {
    let root = std::path::Path::new("D:/repo");
    let args = |k: &BgKind| -> Vec<String> {
        k.args(root)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    };
    let verify = BgKind::Verify(id("asf-3"));
    let retry = BgKind::Retry(id("asf-6"));
    assert_eq!(
        args(&verify),
        ["--repo", "D:/repo", "run", "verify", "asf-3", "--json"]
    );
    assert_eq!(
        args(&retry),
        ["--repo", "D:/repo", "merge", "retry", "asf-6", "--json"]
    );
    assert_eq!(
        args(&BgKind::Start(id("asf-3")))[2..],
        ["run", "start", "asf-3", "--json"]
    );
    assert_eq!(verify.label(), "verifying asf-3");
    assert_eq!(retry.label(), "retrying asf-6");
    assert_eq!(
        bg_outcome(
            &verify,
            r#"{"ok":true,"result":{"run_id":"r","issue_id":"asf-3","from":"failed","to":"succeeded","detail":"gates ok\nmore"}}"#,
            "",
            Some(0)
        ),
        "✓ verify asf-3: failed -> succeeded: gates ok"
    );
    // the command itself succeeds when the verification it ran does not
    assert_eq!(
        bg_outcome(
            &verify,
            r#"{"ok":true,"result":{"run_id":"r","issue_id":"asf-3","from":"failed","to":"failed","detail":"gate test failed"}}"#,
            "",
            Some(0)
        ),
        "✗ verify asf-3: failed -> failed: gate test failed"
    );
    assert_eq!(
        bg_outcome(
            &retry,
            r#"{"ok":true,"result":{"id":4,"issue_id":"asf-6","state":"queued"}}"#,
            "",
            Some(0)
        ),
        "✓ asf-6 re-queued (#4)"
    );
    assert_eq!(
        bg_outcome(
            &retry,
            r##"{"ok":false,"error":{"code":"conflict","message":"#4 asf-6 is queued, only failed/blocked items can be retried"}}"##,
            "",
            Some(3)
        ),
        "✗ #4 asf-6 is queued, only failed/blocked items can be retried"
    );
    assert_eq!(
        bg_outcome(&verify, "", "boom\n", Some(1)),
        "✗ verify asf-3 exited with 1: boom"
    );
}

#[test]
fn an_answer_is_posted_as_the_humans_reply_and_stops_the_question_waiting() {
    let store = asf_store::SqliteStore::open_in_memory().unwrap();
    store.init_repo("D:/repo", "asf", "main").unwrap();
    let issue = store
        .create_issue(&NewIssue {
            title: "needs an answer".into(),
            ..NewIssue::default()
        })
        .unwrap();
    let q = store
        .add_comment(&NewComment {
            issue_id: issue.id.clone(),
            author: "agent:asf-1".into(),
            kind: CommentType::Question,
            to_lane: LANE_HUMAN.into(),
            body: "Keep the alias?".into(),
            reply_to: None,
        })
        .unwrap();
    assert_eq!(unanswered_questions(&store.comments(&issue.id).unwrap()), [q.id]);
    let status = post_answer(&store, &issue.id, q.id, "yes, keep it").unwrap();
    assert_eq!(status, format!("answered #{} on {}", q.id, issue.id));
    let comments = store.comments(&issue.id).unwrap();
    let answer = comments.last().unwrap();
    assert_eq!(
        (
            answer.kind,
            answer.author.as_str(),
            answer.reply_to,
            answer.body.as_str()
        ),
        (CommentType::Answer, "human", Some(q.id), "yes, keep it")
    );
    assert_eq!(answer.to_lane, issue.id.to_string());
    assert!(
        unanswered_questions(&comments).is_empty(),
        "the same rule `asf issue finish` applies"
    );
}

#[test]
fn the_banner_is_for_the_other_views_the_footer_and_help_know_the_new_view() {
    let mut a = App::new(busy_desk());
    let t = render_to_text(&a, 120, 30);
    assert!(
        !t.contains("is waiting for your answer") && !t.contains(" asks:"),
        "{t}"
    );
    assert!(
        line_of(&t, 1).contains("[1] Now") && line_of(&t, 1).contains("[7] Config"),
        "{t}"
    );
    assert_eq!(line_of(&t, 2), "─".repeat(120), "a rule under the tab bar:\n{t}");
    assert!(
        line_of(&t, 3).contains("NEEDS YOU"),
        "content starts under that rule:\n{t}"
    );
    a.handle_key(key('2'));
    // the oldest thing that needs you: asf-5, over budget since the run started
    let banner = line_of(&render_to_text(&a, 120, 30), 2);
    assert!(banner.starts_with("■ asf-5 over_budget: "), "{banner:?}");
    assert!(banner.trim_end().ends_with("(+7 more)"), "{banner:?}");
    // the tab bar marks the active view with reverse video
    a.handle_key(key('1'));
    let (_, m) = cell_style(&a, "[1] Now", "[1] Now");
    assert!(m.contains(Modifier::REVERSED));
    // the footer: the hints of the mockup, built from the table
    let f = footer_line(&a, 120);
    assert_eq!(
        f.trim(),
        "j / k move  a answer  f focus pane  Enter open issue  Tab next view  ? help  q quit"
    );
    // v, r and x are in the help view, in a group of their own
    a.handle_key(key('?'));
    let help = render_to_text(&a, 120, 60);
    assert!(
        !help.contains("stalled or over-budget"),
        "v is for failed runs only:\n{help}"
    );
    for needle in [
        "WHEN AN AGENT NEEDS YOU",
        "check a failed run again",
        "Now, on a merge row: try it again",
        "stop the agent (asks first)",
        "jump to a view",
    ] {
        assert!(help.contains(needle), "{needle:?}:\n{help}");
    }
    assert!(all().any(|b| b.key == "1-7"));
}
