//! The Board view: issues grouped under their epic, the counts line, the filter, the keys and the
//! detail panel. Tests of the Board live here and nowhere else, so the task that changes the view edits
//! this file only.

use super::*;
use crate::tui::snapshot::{text_width, IssueDetail, LastRun};
use crate::tui::views::board;

fn epic(i: &str, title: &str, status: Status) -> Issue {
    let mut e = issue(i, title, status);
    e.kind = IssueType::Epic;
    e
}

fn kid(i: &str, title: &str, status: Status, parent: &str) -> Issue {
    let mut k = issue(i, title, status);
    k.parent_id = Some(id(parent));
    k
}

/// The approved mockup's data: an open epic with ten children (one asks, one has a failed merge, one
/// validates, one works), two finished epics and a loose ready task.
fn round4() -> Snapshot {
    let mut issues = vec![epic(
        "asf-23",
        "Dogfooding round 4: no operator steps",
        Status::Backlog,
    )];
    issues.extend([
        kid("asf-24", "Strip worker env vars (canary)", Status::Done, "asf-23"),
        kid(
            "asf-25",
            "Retry Worker::start once on timeout",
            Status::InProgress,
            "asf-23",
        ),
        kid(
            "asf-26",
            "Watch re-verifies fixed runs",
            Status::Backlog,
            "asf-23",
        ),
        kid(
            "asf-27",
            "Add `asf issue scope add/rm`",
            Status::InProgress,
            "asf-23",
        ),
        kid(
            "asf-28",
            "asf serve: reload config each tick",
            Status::Validating,
            "asf-23",
        ),
        kid(
            "asf-29",
            "Reviewer approval survives retry",
            Status::InProgress,
            "asf-23",
        ),
        kid("asf-30", "TUI: waiting agents obvious", Status::Backlog, "asf-23"),
        kid(
            "asf-31",
            "TUI: audit defects, help view",
            Status::Backlog,
            "asf-23",
        ),
        kid("asf-32", "TUI: Now view", Status::Backlog, "asf-23"),
        kid("asf-33", "Readable session names", Status::Backlog, "asf-23"),
        epic(
            "asf-15",
            "Dogfooding round 3: review gate, questions",
            Status::Done,
        ),
        kid("asf-16", "Review gate", Status::Done, "asf-15"),
        kid("asf-17", "Questions", Status::Done, "asf-15"),
        epic("asf-10", "Dogfooding round 2: unattended loop", Status::Done),
        kid("asf-11", "Unattended loop", Status::Done, "asf-10"),
        issue("asf-90", "Loose task", Status::Ready),
    ]);
    let mut s = desk(issues);
    s.deps = vec![
        dep("asf-26", "asf-25"),
        dep("asf-27", "asf-24"),
        dep("asf-30", "asf-24"),
        dep("asf-31", "asf-30"),
        dep("asf-32", "asf-24"),
        dep("asf-32", "asf-31"),
        dep("asf-33", "asf-26"),
    ];
    let mut asking = run_at("asf-27", RunState::Blocked, "2026-11-06T11:46:00Z");
    asking.cost_usd = 1.12;
    asking.tool_calls = 21;
    s.runs = vec![
        asking,
        run_at("asf-29", RunState::Working, "2026-11-06T11:50:00Z"),
    ];
    s.events
        .push(state_event(1, "asf-27", "blocked", "2026-11-06T11:56:00Z"));
    s.queue.push(merge_item(
        1,
        "asf-25",
        MergeState::Failed,
        Some("gate failed"),
        "2026-11-06T11:40:00Z",
    ));
    s.last_runs.push(LastRun {
        issue: id("asf-27"),
        run_id: "run-asf-27".into(),
        state: RunState::Blocked,
        cost: 1.12,
        budget: Some(10.0),
        tool_calls: 21,
        started_at: "2026-11-06T11:46:00Z".into(),
        finished_at: None,
        model: Some("sonnet".into()),
        pane: None,
        session_name: None,
    });
    s
}

/// An app on the Board with the mockup's data and `select` selected.
fn on(select: &str) -> App {
    let mut a = board_app(round4());
    board::select(&mut a.board, &a.snap, &id(select));
    a
}

/// The screen line of the list row of `i` (a status symbol, then the id).
fn row(text: &str, i: &str) -> String {
    text.lines()
        .find(|l| {
            ["●", "■", "◆", "✓", "○"]
                .iter()
                .any(|g| l.contains(&format!("{g} {i}  ")))
        })
        .unwrap_or_else(|| panic!("no row for {i}:\n{text}"))
        .to_string()
}

fn line_with(text: &str, needle: &str) -> String {
    text.lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no line with {needle:?}:\n{text}"))
        .to_string()
}

fn press(a: &mut App, keys: &[char]) {
    for c in keys {
        a.handle_key(key(*c));
    }
}

/// Types `text` into a Board state the way the keyboard does, ending in the typing mode.
fn type_filter(snap: &Snapshot, state: &mut board::State, text: &str) {
    board::handle_key(key('/'), snap, state);
    for c in text.chars() {
        board::handle_key(key(c), snap, state);
    }
}

// ---- the counts line ---------------------------------------------------------------------------

#[test]
fn the_first_line_counts_tasks_per_status_and_hints_at_the_filter() {
    for width in [100, 120] {
        let t = render_to_text(&board_app(round4()), width, 30);
        let line = line_with(&t, "in-progress 3");
        assert_eq!(
            line.trim(),
            "backlog 5 · ready 1 · in-progress 3 · validating 1 · done 4     / filter",
            "{width}:\n{t}"
        );
    }
}

#[test]
fn blocked_and_in_review_appear_in_the_counts_only_when_not_zero() {
    let quiet = render_to_text(&board_app(round4()), 100, 30);
    assert!(
        !quiet.contains("blocked 0") && !quiet.contains("in-review 0"),
        "{quiet}"
    );
    let mut s = round4();
    s.issues.push(issue("asf-91", "Stuck", Status::Blocked));
    s.issues.push(issue("asf-92", "Under review", Status::InReview));
    s.issues
        .push(issue("asf-93", "Under review too", Status::InReview));
    let t = render_to_text(&board_app(s), 100, 30);
    let line = line_with(&t, "in-progress 3");
    assert!(
        line.contains("in-progress 3 · blocked 1 · in-review 2 · validating 1 · done 4"),
        "{line}"
    );
}

#[test]
fn an_open_epic_does_not_raise_the_backlog_count() {
    // an epic that sits in backlog is a group header, never an issue to count
    let mut s = desk(vec![epic("asf-1", "Round", Status::Backlog)]);
    s.issues.push(kid("asf-2", "Child", Status::Backlog, "asf-1"));
    let t = render_to_text(&board_app(s), 100, 30);
    assert!(t.contains("backlog 1 ·"), "only the child counts:\n{t}");
    let lone = render_to_text(
        &board_app(desk(vec![epic("asf-1", "Round", Status::Backlog)])),
        100,
        30,
    );
    assert!(lone.contains("backlog 0 ·"), "{lone}");
    // not a row either
    assert!(!lone.contains("○ asf-1"), "{lone}");
}

#[test]
fn cancelled_issues_are_neither_counted_nor_listed() {
    let mut s = round4();
    s.issues.push(issue("asf-94", "Dropped idea", Status::Cancelled));
    let t = render_to_text(&board_app(s), 100, 30);
    assert!(!t.contains("asf-94") && !t.contains("cancelled"), "{t}");
}

// ---- groups --------------------------------------------------------------------------------------

#[test]
fn issues_are_listed_under_their_epic_with_a_progress_bar_and_finished_epics_are_folded() {
    for width in [100, 120] {
        let t = render_to_text(&board_app(round4()), width, 30);
        let at = |n: &str| {
            t.find(n)
                .unwrap_or_else(|| panic!("{n:?} missing at {width}:\n{t}"))
        };
        assert!(
            at("asf-23") < at("asf-15") && at("asf-15") < at("asf-10") && at("asf-10") < at("No epic"),
            "open epics first, newest id first, then finished ones, No epic last:\n{t}"
        );
        let open = line_with(&t, "▾ asf-23");
        assert!(open.contains("Dogfooding round 4: no operator steps"), "{open}");
        assert!(open.contains("▓░░░░░░░░░  1/10") && !open.contains('✓'), "{open}");
        for (epic, text) in [("asf-15", "2/2 ✓"), ("asf-10", "1/1 ✓")] {
            let folded = line_with(&t, &format!("▸ {epic}"));
            assert!(
                folded.contains("▓▓▓▓▓▓▓▓▓▓") && folded.contains(text),
                "{width}: {folded}"
            );
        }
        for hidden in ["asf-16", "asf-17", "asf-11"] {
            assert!(!t.contains(hidden), "{hidden} sits in a folded epic:\n{t}");
        }
        assert!(line_with(&t, "No epic").contains('▾'));
        assert!(t.contains("asf-90"), "{t}");
    }
}

#[test]
fn the_progress_bar_fills_a_cell_per_tenth_and_is_full_only_when_all_are_done() {
    let mut s = desk(vec![epic("asf-1", "Round", Status::Backlog)]);
    for n in 2..=31 {
        let status = if n <= 30 { Status::Done } else { Status::Backlog };
        s.issues.push(kid(&format!("asf-{n}"), "Child", status, "asf-1"));
    }
    let t = render_to_text(&board_app(s), 100, 30);
    // 29 of 30 done: nine cells, not ten, because the epic is not finished
    assert!(line_with(&t, "▾ asf-1").contains("▓▓▓▓▓▓▓▓▓░  29/30"), "{t}");
}

#[test]
fn a_review_is_listed_under_the_epic_of_the_task_it_reviews_but_does_not_count_for_the_bar() {
    let mut s = desk(vec![
        epic("asf-1", "Round", Status::Backlog),
        kid("asf-2", "Work", Status::InProgress, "asf-1"),
    ]);
    let mut review = issue("asf-3", "Review asf-2", Status::InProgress);
    review.kind = IssueType::Review;
    review.parent_id = Some(id("asf-2"));
    s.issues.push(review);
    let t = render_to_text(&board_app(s), 100, 30);
    assert!(!t.contains("No epic"), "{t}");
    assert!(line_with(&t, "▾ asf-1").contains("0/1"), "{t}");
    row(&t, "asf-3");
}

#[test]
fn rows_show_symbol_id_state_label_title_and_the_first_unmet_dependency() {
    for width in [100, 120] {
        let t = render_to_text(&board_app(round4()), width, 30);
        for (i, want) in [
            ("asf-24", "✓ asf-24  done         Strip worker env vars (canary)"),
            (
                "asf-25",
                "■ asf-25  merge failed Retry Worker::start once on timeout",
            ),
            ("asf-27", "■ asf-27  asking       Add `asf issue scope add/rm`"),
            (
                "asf-28",
                "◆ asf-28  validating   asf serve: reload config each tick",
            ),
            (
                "asf-29",
                "● asf-29  in-progress  Reviewer approval survives retry",
            ),
            ("asf-26", "○ asf-26  backlog      Watch re-verifies fixed runs"),
            ("asf-90", "○ asf-90  ready        Loose task"),
        ] {
            let r = row(&t, i);
            assert!(r.contains(want), "{width}: {r:?} lacks {want:?}");
        }
        assert!(row(&t, "asf-26").contains("← asf-25"), "{t}");
        assert!(row(&t, "asf-33").contains("← asf-26"), "{t}");
        assert!(row(&t, "asf-31").contains("← asf-30"), "{t}");
        // asf-30's only dependency is done; asf-32's first one is done, so the arrow names the second
        assert!(!row(&t, "asf-30").contains('←'), "{}", row(&t, "asf-30"));
        assert!(row(&t, "asf-32").contains("← asf-31"), "{}", row(&t, "asf-32"));
        assert!(!row(&t, "asf-24").contains('←'));
    }
}

#[test]
fn rows_go_by_id_with_backlog_and_ready_after_all_the_others() {
    let t = render_to_text(&board_app(round4()), 100, 30);
    let order = [
        "asf-24", "asf-25", "asf-27", "asf-28", "asf-29", "asf-26", "asf-30", "asf-31", "asf-32", "asf-33",
    ]
    .map(|i| t.find(&row(&t, i)).unwrap_or_else(|| panic!("{i}:\n{t}")));
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}:\n{t}");
    let ids: Vec<String> = board::ordered(&round4())
        .iter()
        .map(|i| i.id.to_string())
        .collect();
    assert_eq!(
        ids,
        [
            "asf-24", "asf-25", "asf-27", "asf-28", "asf-29", "asf-26", "asf-30", "asf-31", "asf-32",
            "asf-33", "asf-16", "asf-17", "asf-11", "asf-90"
        ]
    );
}

#[test]
fn asking_and_merge_failed_come_from_the_run_and_the_merge_item_not_the_status() {
    let mut s = round4();
    s.queue[0].state = MergeState::Blocked;
    let t = render_to_text(&board_app(s), 100, 30);
    assert!(
        row(&t, "asf-25").contains("merge failed"),
        "a blocked merge reads the same:\n{t}"
    );
    let mut s = round4();
    s.queue[0].state = MergeState::Queued;
    let t = render_to_text(&board_app(s), 100, 30);
    assert!(row(&t, "asf-25").contains("in-progress"), "{t}");
}

#[test]
fn a_selected_row_has_a_marker_a_highlighted_background_and_reverse_video_without_colours() {
    let a = on("asf-27");
    let t = render_to_text(&a, 100, 30);
    assert!(row(&t, "asf-27").contains("▸ ■ asf-27"), "{}", row(&t, "asf-27"));
    assert!(!row(&t, "asf-25").contains('▸'));
    let (w, h) = (100, 30);
    let y = t.lines().position(|l| l.contains("▸ ■ asf-27")).unwrap() as u16;
    let buf = render_to_buffer(&a, w, h);
    assert_ne!(
        buf[(40, y)].bg,
        Color::Reset,
        "a highlighted background across the row"
    );
    let mut plain = on("asf-27");
    plain.theme = Theme::new(false);
    let buf = render_to_buffer(&plain, w, h);
    assert!(
        buf[(40, y)].modifier.contains(Modifier::REVERSED),
        "reverse video"
    );
    // a header is highlighted too
    let header = on("asf-15");
    let t = render_to_text(&header, w, h);
    let y = t.lines().position(|l| l.contains("asf-15")).unwrap() as u16;
    assert_ne!(render_to_buffer(&header, w, h)[(40, y)].bg, Color::Reset);
}

#[test]
fn symbols_carry_their_colours_and_ids_are_bold() {
    let a = board_app(round4());
    assert_eq!(cell_style(&a, "asf-29  in-progress", "●").0, Color::Green);
    let (fg, m) = cell_style(&a, "asf-27  asking", "■");
    assert_eq!(fg, Color::Red);
    assert!(m.contains(Modifier::BOLD));
    assert_eq!(cell_style(&a, "asf-25  merge failed", "■").0, Color::Red);
    assert_eq!(cell_style(&a, "asf-28  validating", "◆").0, Color::Yellow);
    assert_eq!(cell_style(&a, "asf-26  backlog", "○").0, Color::Reset);
    let mut f = board_app(round4());
    board::select(&mut f.board, &f.snap, &id("asf-16")); // unfolds the finished epic
    assert!(cell_style(&f, "asf-16  done", "✓").1.contains(Modifier::DIM));
    assert!(cell_style(&a, "asf-26  backlog", "asf-26")
        .1
        .contains(Modifier::BOLD));
    // a finished epic is dim, an open one is not
    assert!(cell_style(&a, "asf-15", "Dogfooding round 3")
        .1
        .contains(Modifier::DIM));
    assert!(!cell_style(&a, "▾ asf-23", "Dogfooding round 4")
        .1
        .contains(Modifier::DIM));
}

#[test]
fn a_long_title_is_cut_with_an_ellipsis_and_the_dependency_stays_visible() {
    for width in [100, 120] {
        let mut s = round4();
        s.issues.iter_mut().find(|i| i.id == id("asf-26")).unwrap().title =
            "A very long title that runs on and on about everything a reader may want to know".repeat(2);
        let t = render_to_text(&board_app(s), width, 30);
        let r = row(&t, "asf-26");
        assert!(r.contains('…'), "{width}: {r:?}");
        assert!(r.contains("← asf-25"), "{width}: {r:?}");
        for l in t.lines() {
            assert!(text_width(l) <= width as usize, "{width}: {l:?}");
        }
    }
}

#[test]
fn a_long_epic_title_keeps_its_bar_and_count_on_screen() {
    let mut s = round4();
    s.issues[0].title =
        "An epic whose title is far too long to fit next to its progress bar at any width".into();
    for width in [100, 120] {
        let t = render_to_text(&board_app(s.clone()), width, 30);
        let h = line_with(&t, "▾ asf-23");
        assert!(
            h.contains('…') && h.contains("▓░░░░░░░░░  1/10"),
            "{width}: {h:?}"
        );
    }
}

// ---- cost ----------------------------------------------------------------------------------------

/// Epic asf-1: a done task that cost 30 (one review of 4.2) and an open task whose aborted run cost 10;
/// epic asf-10 has not run anything.
fn costed() -> Snapshot {
    let mut review = issue("asf-4", "Review asf-2", Status::Done);
    review.kind = IssueType::Review;
    review.parent_id = Some(id("asf-2"));
    let mut s = desk(vec![
        epic("asf-1", "Costly round", Status::Backlog),
        kid("asf-2", "Done work", Status::Done, "asf-1"),
        kid("asf-3", "Open work", Status::InProgress, "asf-1"),
        review,
        epic("asf-10", "Cheap round", Status::Backlog),
        kid("asf-11", "Idle", Status::Backlog, "asf-10"),
    ]);
    s.all_runs = vec![
        run("asf-2", RunState::Succeeded, 30.0),
        run("asf-3", RunState::Aborted, 10.0),
        run("asf-4", RunState::Succeeded, 4.2),
    ];
    s
}

/// The list part of the screen line with `needle`: from 120 columns the detail panel sits to the right of
/// a `│`, and it is not part of the header.
fn list_line(text: &str, needle: &str) -> String {
    let line = line_with(text, needle);
    line.split('│').next().unwrap_or("").trim_end().to_string()
}

#[test]
fn the_epic_header_ends_with_the_epics_total_cost_after_the_bar_and_the_count() {
    for width in [100, 120] {
        let t = render_to_text(&board_app(costed()), width, 30);
        let h = list_line(&t, "▾ asf-1 ");
        // task runs (aborted ones too) and the review of a task: 30 + 10 + 4.2
        assert!(h.trim_end().ends_with("$44.2"), "{width}: {h:?}");
        assert!(
            h.find("1/2").unwrap() < h.find("$44.2").unwrap(),
            "{width}: {h:?}"
        );
        let cheap = list_line(&t, "▾ asf-10");
        assert!(cheap.trim_end().ends_with("$0.0"), "{width}: {cheap:?}");
        // the cost column lines up whatever the counts are
        assert_eq!(
            text_width(h.trim_end()),
            text_width(cheap.trim_end()),
            "{width}:\n{h:?}\n{cheap:?}"
        );
    }
}

#[test]
fn a_long_epic_title_gives_way_to_the_cost() {
    let mut s = costed();
    s.issues[0].title =
        "An epic whose title is far too long to fit next to its progress bar at any width".into();
    for width in [100, 120] {
        let t = render_to_text(&board_app(s.clone()), width, 30);
        let h = list_line(&t, "▾ asf-1 ");
        assert!(
            h.contains('…') && h.contains("1/2") && h.trim_end().ends_with("$44.2"),
            "{width}: {h:?}"
        );
    }
}

#[test]
fn a_folded_epic_header_shows_its_cost_too() {
    let mut s = costed();
    for i in &mut s.issues {
        i.status = Status::Done;
    }
    let t = render_to_text(&board_app(s), 100, 30);
    let h = list_line(&t, "▸ asf-1 ");
    assert!(h.contains("2/2 ✓") && h.trim_end().ends_with("$44.2"), "{h:?}");
}

#[test]
fn the_epic_detail_breaks_the_cost_down_into_tasks_reviews_and_total() {
    let mut a = board_app(costed());
    board::select(&mut a.board, &a.snap, &id("asf-1"));
    let t = render_to_text(&a, 120, 30);
    let field = |label: &str, text: &str| format!("{label:<12}{text}");
    for needle in [
        "Cost",
        &field("Tasks", "$40.00 · 1/2 done"),
        &field("Reviews", "$4.20 · 1 run"),
        &field("Total", "$44.20"),
    ] {
        assert!(t.contains(needle), "missing {needle:?}:\n{t}");
    }
    // the same breakdown on the full-screen detail below 120 columns
    a.handle_key(code(KeyCode::Enter));
    let t = render_to_text(&a, 100, 30);
    assert!(t.contains(&field("Total", "$44.20")), "{t}");
}

#[test]
fn an_epic_that_ran_nothing_shows_zero_and_a_task_has_no_cost_block() {
    let mut a = board_app(costed());
    board::select(&mut a.board, &a.snap, &id("asf-10"));
    let t = render_to_text(&a, 120, 30);
    assert!(t.contains(&format!("{:<12}{}", "Total", "$0.00")), "{t}");
    assert!(
        t.contains(&format!("{:<12}{}", "Reviews", "$0.00 · 0 runs")),
        "{t}"
    );
    board::select(&mut a.board, &a.snap, &id("asf-2"));
    let t = render_to_text(&a, 120, 30);
    assert!(!t.contains("Reviews  ") && !t.contains("$44.20"), "{t}");
}

// ---- keys ----------------------------------------------------------------------------------------

#[test]
fn j_and_k_move_over_every_visible_row_headers_included_and_clamp() {
    let mut a = board_app(round4());
    assert_eq!(
        a.selected_issue(),
        Some(id("asf-23")),
        "the first row is the first header"
    );
    a.handle_key(key('j'));
    assert_eq!(a.selected_issue(), Some(id("asf-24")));
    a.handle_key(code(KeyCode::Down));
    assert_eq!(a.selected_issue(), Some(id("asf-25")));
    a.handle_key(code(KeyCode::Up));
    a.handle_key(key('k'));
    a.handle_key(key('k'));
    assert_eq!(a.selected_issue(), Some(id("asf-23")), "clamped at the first row");
    a.handle_key(key('G'));
    assert_eq!(a.selected_issue(), Some(id("asf-90")), "G is the last row");
    a.handle_key(key('j'));
    assert_eq!(a.selected_issue(), Some(id("asf-90")), "clamped at the last row");
    a.handle_key(key('g'));
    assert_eq!(a.selected_issue(), Some(id("asf-23")));
    // from the last row of the open group the next row is the next header
    board::select(&mut a.board, &a.snap, &id("asf-33"));
    a.handle_key(key('j'));
    assert_eq!(a.selected_issue(), Some(id("asf-15")));
    a.handle_key(key('j'));
    assert_eq!(a.selected_issue(), Some(id("asf-10")));
    a.handle_key(key('j'));
    assert_eq!(a.selected_issue(), None, "the No epic header is not an issue");
}

#[test]
fn h_folds_the_group_and_l_unfolds_it() {
    let mut a = on("asf-27");
    a.handle_key(key('h'));
    assert_eq!(
        a.selected_issue(),
        Some(id("asf-23")),
        "the selection goes to the header"
    );
    let t = render_to_text(&a, 100, 30);
    assert!(t.contains("▸ asf-23") && !t.contains("asf-24"), "{t}");
    a.handle_key(key('l'));
    let t = render_to_text(&a, 100, 30);
    assert!(t.contains("▾ asf-23") && t.contains("asf-24"), "{t}");
    assert_eq!(
        a.selected_issue(),
        Some(id("asf-23")),
        "l keeps the header selected"
    );
    // a finished epic opens with l and closes again with h
    board::select(&mut a.board, &a.snap, &id("asf-33"));
    a.handle_key(key('j'));
    assert_eq!(a.selected_issue(), Some(id("asf-15")));
    a.handle_key(key('l'));
    let t = render_to_text(&a, 100, 30);
    assert!(
        t.contains("▾ asf-15") && t.contains("asf-16") && t.contains("asf-17"),
        "{t}"
    );
    a.handle_key(key('h'));
    assert!(!render_to_text(&a, 100, 30).contains("asf-16"));
    // h on a header that is already folded, and l on a row, change nothing
    let before = a.board.selected;
    a.handle_key(key('h'));
    assert_eq!(a.board.selected, before);
}

#[test]
fn rows_counts_the_rows_of_the_default_layout() {
    assert_eq!(
        board::rows(&round4()),
        15,
        "3 epic headers + 10 rows + No epic header + 1 row"
    );
    let mut a = board_app(round4());
    assert_eq!(a.rows(View::Board), 15);
    a.handle_key(key('G'));
    assert_eq!(a.selected(View::Board), 14);
    assert_eq!(board::rows(&Snapshot::default()), 0);
}

#[test]
fn only_a_ready_issue_starts_and_the_other_keys_act_on_the_selected_issue() {
    let mut a = on("asf-90");
    assert_eq!(a.handle_key(key('s')), Action::Start(id("asf-90")));
    assert_eq!(a.handle_key(key('m')), Action::Enqueue(id("asf-90")));
    assert_eq!(a.handle_key(key('f')), Action::Focus(id("asf-90")));
    board::select(&mut a.board, &a.snap, &id("asf-29"));
    assert_eq!(a.handle_key(key('s')), Action::None);
    assert!(
        a.status.as_deref().unwrap_or("").contains("ready"),
        "{:?}",
        a.status
    );
    assert_eq!(a.handle_key(key('m')), Action::Enqueue(id("asf-29")));
    assert_eq!(a.handle_key(key('f')), Action::Focus(id("asf-29")));
    assert_eq!(a.handle_key(key('a')), Action::None);
    assert!(
        a.status.as_deref().unwrap_or("").contains("no blocked run"),
        "{:?}",
        a.status
    );
    board::select(&mut a.board, &a.snap, &id("asf-27"));
    assert_eq!(
        a.handle_key(key('a')),
        Action::Peek(id("asf-27")),
        "the answer overlay opens"
    );
    assert!(matches!(a.mode(), Mode::Answer { .. }));
    // bare y/n never reach the worker from the list
    let mut b = on("asf-29");
    assert_eq!(b.handle_key(key('y')), Action::None);
    assert_eq!(b.handle_key(key('n')), Action::None);
    // x aborts after asking, r replies
    assert!(matches!(b.handle_key(key('x')), Action::None));
    assert!(matches!(b.mode(), Mode::Confirm { .. }));
}

#[test]
fn an_epic_header_cannot_be_merged_or_started() {
    let mut a = on("asf-23");
    assert_eq!(a.handle_key(key('m')), Action::None);
    assert!(
        a.status.as_deref().unwrap_or("").contains("epic"),
        "{:?}",
        a.status
    );
    assert_eq!(a.handle_key(key('s')), Action::None);
    assert!(a.status.is_some());
    // the No epic header is no issue at all
    let mut n = board_app(round4());
    n.handle_key(key('G'));
    assert_eq!(n.selected_issue(), Some(id("asf-90")));
    n.handle_key(key('k'));
    assert_eq!(n.selected_issue(), None);
    assert_eq!(n.handle_key(key('f')), Action::None);
    assert_eq!(n.handle_key(key('m')), Action::None);
}

#[test]
fn the_board_asks_for_the_detail_of_the_issue_it_lands_on() {
    let snap = round4();
    let mut st = board::State::default();
    assert_eq!(
        board::handle_key(key('j'), &snap, &mut st),
        Action::LoadDetail(id("asf-24"))
    );
    assert_eq!(
        board::handle_key(key('j'), &snap, &mut st),
        Action::LoadDetail(id("asf-25"))
    );
    assert_eq!(
        board::handle_key(key('G'), &snap, &mut st),
        Action::LoadDetail(id("asf-90"))
    );
    assert_eq!(
        board::handle_key(key('G'), &snap, &mut st),
        Action::None,
        "already there"
    );
    assert_eq!(
        board::handle_key(key('k'), &snap, &mut st),
        Action::None,
        "the No epic header has no detail to load"
    );
    assert_eq!(
        board::handle_key(key('j'), &snap, &mut st),
        Action::LoadDetail(id("asf-90"))
    );
    // an epic header has a detail of its own
    assert_eq!(
        board::handle_key(key('g'), &snap, &mut st),
        Action::LoadDetail(id("asf-23"))
    );
    // folding from a row moves the selection to the header: that is a new selection too
    board::handle_key(key('j'), &snap, &mut st);
    assert_eq!(
        board::handle_key(key('h'), &snap, &mut st),
        Action::LoadDetail(id("asf-23"))
    );
}

#[test]
fn the_event_loop_keeps_asking_until_the_detail_of_the_selection_is_there() {
    let mut a = on("asf-27");
    assert_eq!(a.detail_wanted(), Some(id("asf-27")));
    a.snap.detail = Some(IssueDetail {
        issue: id("asf-27"),
        comments: vec![],
        deps: vec![],
    });
    assert_eq!(a.detail_wanted(), None);
    a.handle_key(key('3'));
    assert_eq!(a.detail_wanted(), None, "only the Board shows a detail");
    assert!(a.board.detail, "the panel is wanted whenever the Board is open");
}

#[test]
fn a_shrinking_snapshot_keeps_the_selection_in_range() {
    let mut a = app();
    press(&mut a, &['j', 'j', 'j', 'j']);
    assert_eq!(a.selected_issue(), Some(id("asf-4")));
    let mut s = snapshot();
    s.issues.truncate(1);
    a.replace(s);
    assert_eq!(a.selected_issue().unwrap().as_str(), "asf-1");
    assert_eq!(a.rows(View::Board), 2, "the No epic header and the one issue");
}

#[test]
fn cancelled_issues_and_epics_are_not_rows_so_the_selection_never_runs_past_the_last_one() {
    let mut s = snapshot();
    for n in 90..93 {
        s.issues
            .push(issue(&format!("asf-{n}"), "Dropped", Status::Cancelled));
    }
    s.issues.push(epic("asf-95", "Round", Status::Backlog));
    let mut a = board_app(s);
    let listed = board::ordered(&a.snap);
    assert_eq!(listed.len(), 4, "no cancelled issue, no epic");
    assert!(listed
        .iter()
        .all(|i| i.kind != IssueType::Epic && i.status != Status::Cancelled));
    a.handle_key(key('G'));
    a.handle_key(key('j'));
    assert_eq!(a.selected_issue(), Some(id("asf-4")));
    assert_eq!(
        a.rows(View::Board),
        6,
        "the empty epic's header, No epic's header and four rows"
    );
}

// ---- the filter ----------------------------------------------------------------------------------

#[test]
fn the_filter_matches_id_title_or_status_and_escape_clears_it() {
    let snap = round4();
    let mut st = board::State::default();
    // status
    type_filter(&snap, &mut st, "validating");
    assert!(st.typing());
    let mut a = board_app(snap.clone());
    a.board = st.clone();
    let t = render_to_text(&a, 100, 30);
    assert!(t.contains("asf-28") && t.contains("▾ asf-23"), "{t}");
    for gone in ["asf-27", "asf-29", "asf-24", "asf-15", "No epic", "asf-90"] {
        assert!(!t.contains(gone), "{gone} does not match:\n{t}");
    }
    assert!(
        t.contains("/ validating"),
        "the first line shows the filter:\n{t}"
    );
    assert!(
        t.contains("in-progress 3"),
        "the counts do not follow the filter:\n{t}"
    );
    // Enter keeps the filter and ends the typing
    board::handle_key(code(KeyCode::Enter), &snap, &mut st);
    assert!(!st.typing());
    assert_eq!(
        board::selected_issue(&snap, &st),
        Some(id("asf-28")),
        "the first match is selected"
    );
    // Esc clears it
    board::handle_key(code(KeyCode::Esc), &snap, &mut st);
    a.board = st.clone();
    let t = render_to_text(&a, 100, 30);
    assert!(
        t.contains("asf-27") && t.contains("No epic") && t.contains("/ filter"),
        "{t}"
    );

    // id (case does not matter) and title
    for (needle, shown, hidden) in [
        ("ASF-3", "asf-31", "asf-29"),
        ("Loose", "asf-90", "asf-24"),
        ("ready", "asf-90", "asf-29"),
        ("session", "asf-33", "asf-32"),
    ] {
        let mut st = board::State::default();
        type_filter(&snap, &mut st, needle);
        a.board = st;
        let t = render_to_text(&a, 100, 30);
        assert!(
            t.contains(shown) && !t.contains(&format!(" {hidden} ")),
            "{needle}:\n{t}"
        );
    }
}

#[test]
fn typing_edits_the_filter_with_backspace_and_ctrl_u_and_escape_leaves_typing_and_clears() {
    let snap = round4();
    let mut st = board::State::default();
    type_filter(&snap, &mut st, "valx");
    board::handle_key(code(KeyCode::Backspace), &snap, &mut st);
    assert_eq!(st.filter(), "val");
    board::handle_key(ctrl('u'), &snap, &mut st);
    assert_eq!(st.filter(), "");
    assert!(st.typing(), "still typing");
    board::handle_key(key('q'), &snap, &mut st);
    board::handle_key(key('1'), &snap, &mut st);
    board::handle_key(key('R'), &snap, &mut st);
    assert_eq!(st.filter(), "q1R", "letters and digits are text while typing");
    board::handle_key(code(KeyCode::Esc), &snap, &mut st);
    assert!(
        !st.typing() && st.filter().is_empty(),
        "Esc clears and ends the typing"
    );
}

#[test]
fn a_filter_ignores_folds_and_shows_the_matches_of_a_finished_epic() {
    let snap = round4();
    let mut st = board::State::default();
    type_filter(&snap, &mut st, "Questions");
    let mut a = board_app(snap);
    a.board = st;
    let t = render_to_text(&a, 100, 30);
    assert!(t.contains("asf-17") && t.contains("asf-15"), "{t}");
    assert!(!t.contains("asf-16"), "{t}");
}

#[test]
fn a_filter_that_matches_nothing_says_so() {
    let snap = round4();
    let mut st = board::State::default();
    type_filter(&snap, &mut st, "zzz");
    let mut a = board_app(snap);
    a.board = st;
    let t = render_to_text(&a, 100, 30);
    assert!(t.contains("Nothing matches") && t.contains("Esc"), "{t}");
    assert_eq!(a.selected_issue(), None);
}

#[test]
fn the_filter_works_from_the_app_for_letters_the_global_keys_do_not_take() {
    let mut a = board_app(round4());
    a.handle_key(key('/'));
    press(&mut a, &['v', 'a', 'l', 'i', 'd']);
    let t = render_to_text(&a, 100, 30);
    assert!(
        t.contains("/ valid") && t.contains("asf-28") && !t.contains("asf-27"),
        "{t}"
    );
    a.handle_key(code(KeyCode::Esc));
    assert!(render_to_text(&a, 100, 30).contains("asf-27"));
}

// ---- the detail ----------------------------------------------------------------------------------

fn detailed() -> App {
    let mut a = on("asf-27");
    let s = &mut a.snap;
    let i = s.issues.iter_mut().find(|i| i.id == id("asf-27")).unwrap();
    i.title = "Add `asf issue scope add/rm` and scope-request protocol".into();
    i.estimate = Some("L".into());
    i.file_scope = (1..=7).map(|n| format!("crates/asf/src/file{n}.rs")).collect();
    i.acceptance = ["scope add/rm keep the rest of the list", "gates pass"]
        .map(|text| Acceptance {
            text: text.into(),
            verify: None,
        })
        .to_vec();
    s.runs[0].cost_usd = 1.12;
    let ask = question(
        5,
        "asf-27",
        "Keep the old --scope flag as an alias?",
        "2026-11-06T11:41:00Z",
    );
    let old = comment("asf-27", CommentType::Fyi, "an older note");
    s.events.push(Event {
        seq: 9,
        ts: "2026-11-06T11:27:00Z".into(),
        kind: "run.created".into(),
        issue_id: Some(id("asf-27")),
        run_id: Some("run-asf-27".into()),
        actor: None,
        payload: serde_json::json!({}),
    });
    s.detail = Some(IssueDetail {
        issue: id("asf-27"),
        comments: vec![old, ask],
        deps: vec![(id("asf-24"), Status::Done)],
    });
    a
}

#[test]
fn at_120_columns_the_detail_panel_shows_the_whole_issue_next_to_the_list() {
    let a = detailed();
    let t = render_to_text(&a, 120, 30);
    for needle in [
        "asf-27 · task · P2 · L",
        "Add `asf issue scope add/rm` and",
        "scope-request protocol",
        "■ asking since 4 min",
        "sonnet · $1.12 / $10 · 21 calls · 14 min",
        "Depends on  ✓ asf-24",
        "Scope       crates/asf/src/file1.rs",
        "crates/asf/src/file2.rs",
        "+5 more",
        "Acceptance",
        "☐ scope add/rm keep the rest of the list",
        "☐ gates pass",
        "Latest",
        "11:56 run blocked",
        "11:41 QUESTION Keep the old --scope flag",
    ] {
        assert!(t.contains(needle), "missing {needle:?}:\n{t}");
    }
    // only the two newest entries: the run start (11:27) and the older note are left out
    assert!(!t.contains("an older note") && !t.contains("run started"), "{t}");
    assert!(!t.contains("file3.rs"), "only the first two globs:\n{t}");
    // the list is still there, to the left of the panel, and the panel is bordered
    let r = row(&t, "asf-27");
    assert!(r.contains("asking") && r.contains('│'), "{r:?}");
    assert!(t.contains('┌') && t.contains('┘'));
    // about 40% of the width: the border column sits between 60% and 65% of it
    let top = t.lines().find(|l| l.contains('┌')).unwrap();
    let at = top.chars().position(|c| c == '┌').unwrap();
    assert!((70..=76).contains(&at), "{at}: {top:?}");
}

#[test]
fn the_panel_for_an_issue_without_run_scope_or_dependencies_still_has_every_heading() {
    let mut a = on("asf-90");
    a.snap
        .issues
        .iter_mut()
        .find(|i| i.id == id("asf-90"))
        .unwrap()
        .file_scope
        .clear();
    let t = render_to_text(&a, 120, 30);
    for needle in [
        "asf-90 · task · P2",
        "Depends on  none",
        "Scope       (none)",
        "Acceptance",
        "Latest",
    ] {
        assert!(t.contains(needle), "{needle:?}:\n{t}");
    }
    assert!(!t.contains("since"), "no run line without a run:\n{t}");
}

#[test]
fn the_panel_shows_a_header_epic_and_the_loose_group_without_failing() {
    let t = render_to_text(&on("asf-23"), 120, 30);
    assert!(t.contains("asf-23 · epic · P2"), "{t}");
    let mut n = board_app(round4());
    n.handle_key(key('G'));
    n.handle_key(key('k'));
    let t = render_to_text(&n, 120, 30);
    assert!(t.contains("No epic"), "{t}");
}

#[test]
fn below_120_columns_enter_opens_the_detail_full_screen_and_escape_returns() {
    let mut a = detailed();
    for width in [100, 119] {
        let t = render_to_text(&a, width, 30);
        assert!(
            !t.contains("Acceptance") && !t.contains("Latest"),
            "no panel at {width}:\n{t}"
        );
    }
    assert_eq!(
        a.handle_key(code(KeyCode::Enter)),
        Action::None,
        "the detail is loaded already"
    );
    let t = render_to_text(&a, 100, 30);
    for needle in [
        "asf-27 · task · P2 · L",
        "Depends on",
        "Scope",
        "Acceptance",
        "☐ gates pass",
        "Latest",
        "Esc",
    ] {
        assert!(t.contains(needle), "full screen lacks {needle:?}:\n{t}");
    }
    assert!(!t.contains("asf-28"), "the list is gone:\n{t}");
    assert_eq!(a.handle_key(code(KeyCode::Esc)), Action::None);
    let t = render_to_text(&a, 100, 30);
    assert!(
        t.contains("asf-28") && !t.contains("Acceptance"),
        "back on the list:\n{t}"
    );
    // the keys of the list still act on the selection from the full screen
    a.handle_key(code(KeyCode::Enter));
    assert_eq!(a.handle_key(key('f')), Action::Focus(id("asf-27")));
}

#[test]
fn enter_asks_for_the_detail_when_the_snapshot_does_not_hold_it() {
    let mut a = on("asf-90");
    assert_eq!(
        a.handle_key(code(KeyCode::Enter)),
        Action::LoadDetail(id("asf-90"))
    );
}

#[test]
fn escape_on_the_list_does_nothing() {
    let mut a = on("asf-27");
    assert_eq!(a.handle_key(code(KeyCode::Esc)), Action::None);
    assert!(a.board.detail);
    assert!(render_to_text(&a, 100, 30).contains("asf-27"));
}

// ---- empty, small, chrome ---------------------------------------------------------------------------

#[test]
fn an_empty_board_says_what_will_appear_and_what_to_do() {
    for width in [100, 120] {
        let a = board_app(Snapshot::default());
        let t = render_to_text(&a, width, 30);
        assert!(
            t.contains("No issues yet. Create one with `asf issue create`; it shows up here under its epic."),
            "{t}"
        );
        assert!(
            t.contains("backlog 0 · ready 0 · in-progress 0 · validating 0 · done 0"),
            "{t}"
        );
        assert!(cell_style(&a, "No issues yet", "No issues yet")
            .1
            .contains(Modifier::DIM));
    }
}

#[test]
fn the_board_fits_the_smallest_terminal_without_failing() {
    for (w, h) in [(80u16, 20u16), (100, 24), (120, 20), (160, 40)] {
        let a = board_app(round4());
        let t = render_to_text(&a, w, h);
        assert_eq!(t.lines().count(), h as usize);
        for l in t.lines() {
            assert!(text_width(l) <= w as usize, "{w}x{h}: {l:?}");
        }
        let mut full = board_app(round4());
        full.handle_key(code(KeyCode::Enter));
        render_to_text(&full, w, h);
    }
}

#[test]
fn a_long_list_scrolls_to_keep_the_selection_on_screen() {
    let mut s = desk(vec![epic("asf-1", "Round", Status::Backlog)]);
    for n in 2..=60 {
        s.issues
            .push(kid(&format!("asf-{n}"), "Child", Status::Backlog, "asf-1"));
    }
    let mut a = board_app(s);
    a.handle_key(key('G'));
    let t = render_to_text(&a, 100, 24);
    assert!(
        t.contains("▸ ○ asf-60"),
        "the last row is on screen and selected:\n{t}"
    );
    assert!(!t.contains(" asf-2 "), "{t}");
    a.handle_key(key('g'));
    assert!(render_to_text(&a, 100, 24).contains("▾ asf-1"));
}

#[test]
fn the_footer_names_the_keys_of_the_board_and_the_help_page_lists_them() {
    for width in [100, 120] {
        let footer = footer_line(&board_app(round4()), width);
        for hint in [
            "h / l fold or column",
            "s start",
            "a answer",
            "Enter full details",
            "b layout",
            "? help  q quit",
        ] {
            assert!(footer.contains(hint), "{width}: {hint:?} in {footer:?}");
        }
        // `/ filter` is not in the footer: the first line of the Board says it
        let top = line_with(&render_to_text(&board_app(round4()), width, 30), "/ filter");
        assert!(top.contains("backlog 5"), "{top:?}");
    }
    assert!(footer_hints(View::Board, ModeKind::Normal).len() <= 7);
    let mut a = board_app(round4());
    a.handle_key(key('?'));
    let t = render_to_text(&a, 120, 60);
    for key in ["h / l", "/", "Enter", "b"] {
        assert!(t.contains(key), "{key:?} on the help page:\n{t}");
    }
    assert!(t.contains("fold") && t.contains("filter"), "{t}");
    assert!(
        t.contains("switch between the epic list") && t.contains("a kanban with a column per status"),
        "{t}"
    );
    assert!(t.contains("kanban: move to the next column"), "{t}");
    let keys: Vec<&str> = board::BINDINGS.iter().map(|b| b.key).collect();
    for k in ["h / l", "/", "s", "m", "a", "f", "Enter", "b"] {
        assert!(keys.contains(&k), "{k:?} in {keys:?}");
    }
}

// ---- the kanban layout -------------------------------------------------------------------------------

/// An app on the Board in the kanban layout with the mockup's data and `select` selected.
fn kanban_on(select: &str) -> App {
    let mut a = on(select);
    a.handle_key(key('b'));
    assert_eq!(a.board.layout, board::BoardLayout::Kanban);
    a
}

/// Whether a card of `i` is on screen (a status symbol, a blank, the id; the selected one has `▸` before).
fn has_card(text: &str, i: &str) -> bool {
    text.lines().any(|l| {
        ["●", "■", "◆", "✓", "○"]
            .iter()
            .any(|g| l.contains(&format!("{g} {i}")))
    })
}

#[test]
fn the_default_layout_is_the_epic_list_and_b_switches_to_the_kanban_and_back() {
    let mut a = board_app(round4());
    assert_eq!(a.board.layout, board::BoardLayout::Epics);
    for width in [100, 120] {
        let t = render_to_text(&a, width, 30);
        assert!(t.contains("▾ asf-23") && t.contains("No epic"), "{width}:\n{t}");
    }
    assert_eq!(a.handle_key(key('b')), Action::LoadDetail(id("asf-24")));
    assert_eq!(a.board.layout, board::BoardLayout::Kanban);
    for width in [100, 120] {
        let t = render_to_text(&a, width, 30);
        assert!(!t.contains("▾ asf-23") && !t.contains("No epic"), "{width}:\n{t}");
        assert!(t.contains("in-review") && t.contains("blocked"), "{width}:\n{t}");
    }
    a.handle_key(key('b'));
    assert_eq!(a.board.layout, board::BoardLayout::Epics);
    assert!(render_to_text(&a, 100, 30).contains("▾ asf-23"));
}

#[test]
fn the_kanban_has_a_column_per_status_with_its_count_and_never_a_card_for_an_epic() {
    let a = kanban_on("asf-27");
    let t = render_to_text(&a, 100, 30);
    let header = line_with(&t, "blocked 0");
    for want in [
        "backlog 5",
        "ready 1",
        "in-progress 3",
        "blocked 0",
        "in-review 0",
        "validating 1",
        "done 4",
    ] {
        assert!(header.contains(want), "{want:?} in {header:?}");
    }
    let at = |w: &str| header.find(w).unwrap();
    assert!(
        at("backlog") < at("ready")
            && at("ready") < at("in-progress")
            && at("in-progress") < at("blocked")
            && at("blocked") < at("in-review")
            && at("in-review") < at("validating")
            && at("validating") < at("done"),
        "{header:?}"
    );
    // tasks and reviews are cards, epics are not (they are neither cards nor counted)
    for card in [
        "asf-24", "asf-25", "asf-26", "asf-27", "asf-28", "asf-29", "asf-30", "asf-90",
    ] {
        assert!(has_card(&t, card), "{card}:\n{t}");
    }
    for epic in ["asf-23", "asf-15", "asf-10"] {
        assert!(!t.contains(epic), "{epic} is an epic:\n{t}");
    }
    // cancelled issues are not cards either
    let mut s = round4();
    s.issues.push(issue("asf-94", "Dropped idea", Status::Cancelled));
    assert!(!render_to_text(&board_app(s), 100, 30).contains("asf-94"));
}

#[test]
fn a_card_shows_the_symbol_the_id_and_the_title_cut_with_an_ellipsis() {
    let mut s = round4();
    s.issues.iter_mut().find(|i| i.id == id("asf-26")).unwrap().title =
        "A very long title that runs on and on about everything a reader may want to know".into();
    let mut a = board_app(s);
    a.handle_key(key('b'));
    let t = render_to_text(&a, 100, 30);
    assert!(has_card(&t, "asf-26"), "{t}");
    // the id line is followed by two lines of the title; the rest is cut
    let at = t.lines().position(|l| l.contains("asf-26")).unwrap();
    let [first, second]: [String; 2] = [1, 2].map(|n| t.lines().nth(at + n).unwrap().to_string());
    assert!(first.contains("A very long"), "{first:?}");
    assert!(second.contains('…'), "{second:?}");
    // symbols and ids are styled like the rows of the epic list
    assert_eq!(cell_style(&a, "asf-29", "●").0, Color::Green);
    assert_eq!(cell_style(&a, "asf-27", "■").0, Color::Red);
    assert_eq!(cell_style(&a, "asf-28", "◆").0, Color::Yellow);
    assert!(cell_style(&a, "asf-29", "asf-29").1.contains(Modifier::BOLD));
}

#[test]
fn the_selected_issue_stays_selected_when_the_layout_changes() {
    for select in ["asf-27", "asf-90", "asf-33", "asf-28"] {
        let mut a = on(select);
        a.handle_key(key('b'));
        assert_eq!(a.selected_issue(), Some(id(select)), "{select} in the kanban");
        let t = render_to_text(&a, 100, 30);
        assert!(t.contains('▸') && has_card(&t, select), "{t}");
        a.handle_key(key('b'));
        assert_eq!(a.selected_issue(), Some(id(select)), "{select} back in the list");
        let t = render_to_text(&a, 100, 30);
        assert!(row(&t, select).contains('▸'), "{}", row(&t, select));
    }
    // a card picked in the kanban opens its epic in the list: asf-16 sits in a finished, folded epic
    let mut a = kanban_on("asf-27");
    a.handle_key(key('l'));
    a.handle_key(key('l'));
    a.handle_key(key('j'));
    a.handle_key(key('j'));
    assert_eq!(a.selected_issue(), Some(id("asf-16")), "done runs 24, 17, 16, 11");
    a.handle_key(key('b'));
    assert_eq!(a.board.layout, board::BoardLayout::Epics);
    assert_eq!(a.selected_issue(), Some(id("asf-16")));
    let t = render_to_text(&a, 100, 30);
    assert!(t.contains("▾ asf-15") && row(&t, "asf-16").contains('▸'), "{t}");
}

#[test]
fn an_epic_header_stands_for_its_first_card_and_the_no_epic_header_keeps_the_cursor() {
    let mut a = board_app(round4());
    assert_eq!(a.selected_issue(), Some(id("asf-23")), "the first header");
    a.handle_key(key('b'));
    assert_eq!(
        a.selected_issue(),
        Some(id("asf-24")),
        "its first child in row order"
    );
    // the No epic header is nothing: the kanban keeps its cursor
    let mut n = board_app(round4());
    n.handle_key(key('G'));
    n.handle_key(key('k'));
    assert_eq!(n.selected_issue(), None);
    n.handle_key(key('b'));
    assert!(n.selected_issue().is_some(), "the kanban always rests on a card");
}

#[test]
fn toggling_asks_for_the_detail_only_when_the_selection_changes() {
    let snap = round4();
    let mut st = board::State::default();
    assert_eq!(
        board::handle_key(key('b'), &snap, &mut st),
        Action::LoadDetail(id("asf-24")),
        "the epic header gave way to its first card"
    );
    assert_eq!(board::handle_key(key('b'), &snap, &mut st), Action::None);
    let mut a = on("asf-27");
    assert_eq!(a.handle_key(key('b')), Action::None);
}

#[test]
fn j_and_k_move_inside_a_column_and_h_and_l_between_columns_skipping_empty_ones() {
    let mut a = kanban_on("asf-25");
    let go = |a: &mut App, k: KeyEvent| {
        a.handle_key(k);
        a.selected_issue().unwrap().to_string()
    };
    // in-progress holds asf-25, 27, 29
    assert_eq!(go(&mut a, key('j')), "asf-27");
    assert_eq!(go(&mut a, key('j')), "asf-29");
    assert_eq!(go(&mut a, key('j')), "asf-29", "clamped at the last card");
    assert_eq!(go(&mut a, code(KeyCode::Up)), "asf-27");
    assert_eq!(go(&mut a, key('k')), "asf-25");
    assert_eq!(go(&mut a, key('k')), "asf-25", "clamped at the first card");
    assert_eq!(go(&mut a, key('G')), "asf-29");
    assert_eq!(go(&mut a, key('g')), "asf-25");
    // blocked and in-review are empty: l goes to validating, then done
    assert_eq!(go(&mut a, key('l')), "asf-28");
    assert_eq!(
        go(&mut a, code(KeyCode::Right)),
        "asf-24",
        "done: the newest card first"
    );
    assert_eq!(go(&mut a, key('l')), "asf-24", "no column to the right");
    assert_eq!(go(&mut a, key('h')), "asf-28");
    assert_eq!(go(&mut a, key('h')), "asf-25");
    assert_eq!(go(&mut a, code(KeyCode::Left)), "asf-90", "ready");
    assert_eq!(go(&mut a, key('h')), "asf-26", "backlog");
    assert_eq!(go(&mut a, key('h')), "asf-26", "no column to the left");
    // the row is kept when the next column has it
    assert_eq!(go(&mut a, key('j')), "asf-30");
    assert_eq!(go(&mut a, key('l')), "asf-90", "ready has one card only");
    // Home and End are g and G
    a.handle_key(key('h'));
    assert_eq!(go(&mut a, code(KeyCode::End)), "asf-33");
    assert_eq!(go(&mut a, code(KeyCode::Home)), "asf-26");
}

#[test]
fn moving_in_the_kanban_asks_for_the_detail_of_the_card_it_lands_on() {
    let snap = round4();
    let mut st = board::State::default();
    board::select(&mut st, &snap, &id("asf-25"));
    board::handle_key(key('b'), &snap, &mut st);
    assert_eq!(
        board::handle_key(key('j'), &snap, &mut st),
        Action::LoadDetail(id("asf-27"))
    );
    assert_eq!(
        board::handle_key(key('l'), &snap, &mut st),
        Action::LoadDetail(id("asf-28"))
    );
    assert_eq!(
        board::handle_key(key('l'), &snap, &mut st),
        Action::LoadDetail(id("asf-24"))
    );
    assert_eq!(
        board::handle_key(key('l'), &snap, &mut st),
        Action::None,
        "already there"
    );
}

#[test]
fn the_selected_card_has_a_marker_and_a_highlight_and_reverse_video_without_colours() {
    let a = kanban_on("asf-27");
    let t = render_to_text(&a, 100, 30);
    let y = t
        .lines()
        .position(|l| l.contains("▸■ asf-27"))
        .expect("the marker") as u16;
    let x = t
        .lines()
        .nth(y as usize)
        .unwrap()
        .chars()
        .position(|c| c == '▸')
        .unwrap() as u16;
    assert_eq!(t.matches('▸').count(), 1, "one selected card:\n{t}");
    let buf = render_to_buffer(&a, 100, 30);
    for line in 0..3 {
        assert_ne!(buf[(x + 3, y + line)].bg, Color::Reset, "line {line} of the card");
    }
    assert_eq!(
        buf[(x + 3, y + 3)].bg,
        Color::Reset,
        "the gap below the card is not"
    );
    let mut plain = kanban_on("asf-27");
    plain.theme = Theme::new(false);
    let buf = render_to_buffer(&plain, 100, 30);
    assert!(buf[(x + 3, y + 1)].modifier.contains(Modifier::REVERSED));
}

#[test]
fn the_kanban_fits_the_smallest_terminal_and_cuts_titles() {
    for (w, h) in [(80u16, 20u16), (100, 24), (120, 20), (160, 40)] {
        let mut s = round4();
        s.issues.iter_mut().find(|i| i.id == id("asf-26")).unwrap().title =
            "A very long title that runs on and on about everything a reader may want to know".repeat(2);
        let mut a = board_app(s);
        a.handle_key(key('b'));
        let t = render_to_text(&a, w, h);
        assert_eq!(t.lines().count(), h as usize, "{w}x{h}");
        for l in t.lines() {
            assert!(text_width(l) <= w as usize, "{w}x{h}: {l:?}");
        }
        assert!(t.contains('…'), "{w}x{h}:\n{t}");
        let mut full = board_app(round4());
        full.handle_key(key('b'));
        full.handle_key(code(KeyCode::Enter));
        render_to_text(&full, w, h);
    }
}

#[test]
fn the_kanban_keeps_the_detail_panel_at_120_columns_and_not_below() {
    let mut a = kanban_on("asf-27");
    let t = render_to_text(&a, 120, 30);
    for needle in [
        "asf-27 · task · P2",
        "Depends on",
        "Scope",
        "Acceptance",
        "☐ works",
        "Latest",
    ] {
        assert!(t.contains(needle), "{needle:?}:\n{t}");
    }
    assert!(t.contains('┌') && t.contains('┘'));
    assert!(
        has_card(&t, "asf-27") && has_card(&t, "asf-29"),
        "the columns are still there:\n{t}"
    );
    // below 120 there is no panel; Enter opens the whole detail, Esc comes back to the kanban
    for width in [100, 119] {
        let t = render_to_text(&a, width, 30);
        assert!(!t.contains("Acceptance"), "no panel at {width}:\n{t}");
    }
    assert_eq!(
        a.handle_key(code(KeyCode::Enter)),
        Action::LoadDetail(id("asf-27"))
    );
    let t = render_to_text(&a, 100, 30);
    assert!(t.contains("Acceptance") && t.contains("Esc"), "{t}");
    assert!(!has_card(&t, "asf-29"), "the columns are gone:\n{t}");
    assert_eq!(a.handle_key(code(KeyCode::Esc)), Action::None);
    let t = render_to_text(&a, 100, 30);
    assert!(has_card(&t, "asf-29") && !t.contains("Acceptance"), "{t}");
    assert_eq!(a.board.layout, board::BoardLayout::Kanban);
}

#[test]
fn the_action_keys_act_on_the_selected_card_like_in_the_list() {
    let mut a = kanban_on("asf-90");
    assert_eq!(a.handle_key(key('s')), Action::Start(id("asf-90")));
    assert_eq!(a.handle_key(key('m')), Action::Enqueue(id("asf-90")));
    assert_eq!(a.handle_key(key('f')), Action::Focus(id("asf-90")));
    let mut a = kanban_on("asf-29");
    assert_eq!(a.handle_key(key('s')), Action::None);
    assert!(
        a.status.as_deref().unwrap_or("").contains("ready"),
        "{:?}",
        a.status
    );
    assert_eq!(a.handle_key(key('m')), Action::Enqueue(id("asf-29")));
    assert_eq!(a.handle_key(key('f')), Action::Focus(id("asf-29")));
    assert_eq!(a.handle_key(key('a')), Action::None);
    assert!(
        a.status.as_deref().unwrap_or("").contains("no blocked run"),
        "{:?}",
        a.status
    );
    assert!(matches!(a.handle_key(key('x')), Action::None));
    assert!(matches!(a.mode(), Mode::Confirm { .. }));
    let mut a = kanban_on("asf-27");
    assert_eq!(a.handle_key(key('a')), Action::Peek(id("asf-27")));
    assert!(matches!(a.mode(), Mode::Answer { .. }));
}

#[test]
fn the_filter_works_in_the_kanban_and_keeps_the_selection_when_it_is_cleared() {
    let mut a = kanban_on("asf-27");
    a.handle_key(key('/'));
    press(&mut a, &['v', 'a', 'l', 'i', 'd']);
    let t = render_to_text(&a, 100, 30);
    assert!(
        t.contains("/ valid") && has_card(&t, "asf-28") && !has_card(&t, "asf-27"),
        "{t}"
    );
    assert_eq!(
        a.selected_issue(),
        Some(id("asf-28")),
        "the first match is selected"
    );
    assert!(
        t.contains("validating 1") && t.contains("backlog 0"),
        "the columns count the matches:\n{t}"
    );
    // `b` is text while typing, not the layout key
    a.handle_key(key('b'));
    assert_eq!(a.board.layout, board::BoardLayout::Kanban);
    assert_eq!(a.board.filter(), "validb");
    a.handle_key(code(KeyCode::Backspace));
    a.handle_key(code(KeyCode::Enter));
    assert!(!a.board.typing());
    // the filter survives a switch of the layout
    a.handle_key(key('b'));
    let t = render_to_text(&a, 100, 30);
    assert!(t.contains("asf-28") && !t.contains("asf-27"), "{t}");
    a.handle_key(key('b'));
    // Esc clears it and asf-28 stays selected
    a.handle_key(code(KeyCode::Esc));
    assert!(a.board.filter().is_empty());
    assert_eq!(a.selected_issue(), Some(id("asf-28")));
    let t = render_to_text(&a, 100, 30);
    assert!(has_card(&t, "asf-27") && t.contains("▸◆ asf-28"), "{t}");
    // nothing matches
    a.handle_key(key('/'));
    press(&mut a, &['z', 'z', 'z']);
    a.handle_key(code(KeyCode::Enter));
    let t = render_to_text(&a, 100, 30);
    assert!(t.contains("Nothing matches") && t.contains("blocked 0"), "{t}");
    assert_eq!(a.selected_issue(), None);
    assert_eq!(a.handle_key(key('s')), Action::None, "no card, no action");
}

#[test]
fn an_empty_kanban_shows_the_columns_and_says_what_to_do() {
    for width in [100, 120] {
        let mut a = board_app(Snapshot::default());
        a.handle_key(key('b'));
        let t = render_to_text(&a, width, 30);
        assert!(
            t.contains("No issues yet. Create one with `asf issue create`."),
            "{t}"
        );
        for column in ["backlog 0", "ready 0", "blocked 0", "validating 0", "done 0"] {
            assert!(t.contains(column), "{width}: {column}:\n{t}");
        }
        assert_eq!(a.selected_issue(), None);
        for k in ['j', 'k', 'h', 'l', 'g', 'G', 'b', 'b'] {
            a.handle_key(key(k)); // nothing to move over, and no panic
        }
    }
}

#[test]
fn the_done_column_shows_the_newest_cards_and_says_how_many_more() {
    let finished = || {
        let mut issues: Vec<Issue> = (1..=8)
            .map(|n| issue(&format!("asf-{n}"), &format!("Finished {n}"), Status::Done))
            .collect();
        issues.push(issue("asf-9", "Open work", Status::Backlog));
        desk(issues)
    };
    let mut a = board_app(finished());
    a.handle_key(key('b'));
    let t = render_to_text(&a, 100, 30);
    assert!(t.contains("done 8") && t.contains("+3 more"), "{t}");
    for newest in ["asf-8", "asf-7", "asf-6", "asf-5", "asf-4"] {
        assert!(has_card(&t, newest), "{newest}:\n{t}");
    }
    for older in ["asf-3", "asf-2", "asf-1"] {
        assert!(!has_card(&t, older), "{older}:\n{t}");
    }
    let order: Vec<usize> = ["asf-8", "asf-7", "asf-6"]
        .iter()
        .map(|i| t.lines().position(|l| l.contains(&format!("✓ {i}"))).unwrap())
        .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "newest first: {order:?}");
    // the keys stop at the fifth card
    a.handle_key(key('l'));
    assert_eq!(a.selected_issue(), Some(id("asf-8")));
    a.handle_key(key('G'));
    assert_eq!(a.selected_issue(), Some(id("asf-4")));
    // a filter lists every match, and a card older than the cap is reached by selecting it
    a.handle_key(key('/'));
    press(&mut a, &['d', 'o', 'n', 'e']);
    let t = render_to_text(&a, 100, 60); // tall enough for all eight cards
    assert!(!t.contains("more") && has_card(&t, "asf-1"), "{t}");
    let mut b = board_app(finished());
    b.handle_key(key('b'));
    board::select(&mut b.board, &b.snap, &id("asf-1"));
    assert_eq!(b.selected_issue(), Some(id("asf-1")));
    let t = render_to_text(&b, 100, 30);
    assert!(has_card(&t, "asf-1") && !t.contains("+3 more"), "{t}");
    // switching the layout closes it again
    b.handle_key(key('b'));
    b.handle_key(key('b'));
    assert_eq!(b.selected_issue(), Some(id("asf-1")));
}

#[test]
fn a_tall_column_scrolls_to_keep_the_selected_card_on_screen() {
    let issues: Vec<Issue> = (1..=30)
        .map(|n| issue(&format!("asf-{n}"), "Card", Status::Backlog))
        .collect();
    let mut a = board_app(desk(issues));
    a.handle_key(key('b'));
    let top = render_to_text(&a, 100, 24);
    assert!(top.contains("▸○ asf-1"), "{top}");
    assert!(top.contains("more") && !top.contains('↑'), "{top}");
    a.handle_key(key('G'));
    let t = render_to_text(&a, 100, 24);
    assert!(
        t.contains("▸○ asf-30"),
        "the last card is on screen and selected:\n{t}"
    );
    assert!(t.contains('↑') && !t.contains("more"), "{t}");
    for l in t.lines() {
        assert!(text_width(l) <= 100, "{l:?}");
    }
    a.handle_key(key('g'));
    assert!(render_to_text(&a, 100, 24).contains("▸○ asf-1"));
}

#[test]
fn the_layout_is_remembered_for_the_session_but_the_default_stays_the_epic_list() {
    let mut a = board_app(round4());
    a.handle_key(key('b'));
    // another view and back
    a.handle_key(key('3'));
    a.handle_key(key('2'));
    assert_eq!(a.view, View::Board);
    assert_eq!(a.board.layout, board::BoardLayout::Kanban);
    // a fresh snapshot keeps it too
    a.replace(round4());
    assert_eq!(a.board.layout, board::BoardLayout::Kanban);
    // OpenOnBoard from another view selects the card
    a.handle_key(key('3'));
    board::select(&mut a.board, &a.snap, &id("asf-33"));
    a.handle_key(key('2'));
    assert_eq!(a.selected_issue(), Some(id("asf-33")));
    // the state a `--once` run starts from is the epic list
    let fresh = App::new(round4());
    assert_eq!(fresh.board.layout, board::BoardLayout::Epics);
    let mut once = App::new(round4());
    once.view = View::Board;
    let t = render_to_text(&once, 100, 30);
    assert!(t.contains("▾ asf-23") && !t.contains("in-review"), "{t}");
}

#[test]
fn while_typing_the_filter_every_key_is_text_even_the_ones_bound_elsewhere() {
    let mut a = board_app(round4());
    a.handle_key(key('/'));
    for c in "asf-3".chars() {
        let action = a.handle_key(key(c));
        assert_ne!(action, Action::Quit);
        assert_eq!(a.view, View::Board, "{c:?} did not switch the view");
    }
    assert_eq!(a.board.filter(), "asf-3");
    let t = render_to_text(&a, 100, 30);
    assert!(
        t.contains("/ asf-3") && t.contains("asf-31") && !t.contains("asf-29"),
        "{t}"
    );
    // q, t, R and ? are text too; Tab does nothing until the typing ends
    for c in ['q', 't', 'R', '?'] {
        assert_ne!(a.handle_key(key(c)), Action::Quit, "{c:?}");
    }
    assert_eq!(a.board.filter(), "asf-3qtR?");
    assert!(matches!(a.mode(), Mode::Normal), "? did not open the help");
    assert_eq!(a.handle_key(code(KeyCode::Tab)), Action::None);
    assert_eq!(a.view, View::Board);
    // Esc clears the filter and gives the keys back to the app
    a.handle_key(code(KeyCode::Esc));
    assert!(a.board.filter().is_empty());
    assert_eq!(a.handle_key(key('q')), Action::Quit);
}
