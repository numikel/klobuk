//! The frame around every view (header, tabs, rules, hints), the answer overlay and the theme, at the
//! two widths the views are laid out for.

use super::*;
use crate::tui::bindings::Binding;
use crate::tui::theme::Mark;
use crate::tui::views::{agents, board, config, inbox, log, queue};

const WIDTHS: [u16; 2] = [100, 120];

#[test]
fn every_view_sits_between_the_header_and_tabs_above_and_a_rule_and_the_hints_below() {
    for width in WIDTHS {
        for view in View::ALL {
            let mut a = app();
            a.view = view;
            let t = render_to_text(&a, width, 30);
            let lines: Vec<&str> = t.lines().collect();
            assert_eq!(lines.len(), 30);
            assert!(
                lines[0].contains("klobuk · hello · main") && lines[0].contains("runtime ok"),
                "{view:?}@{width} header: {:?}",
                lines[0]
            );
            assert!(
                lines[1].contains(&format!("[{}] {}", view as usize + 1, view.title())),
                "{view:?}@{width} tabs: {:?}",
                lines[1]
            );
            // the top chrome ends with a rule: under the tabs, or under the banner when there is one
            let banner = view != View::Now && !a.snap.needs_you.is_empty();
            let top = if banner { 3 } else { 2 };
            if banner {
                assert!(lines[2].contains('■'), "{view:?}@{width} banner: {:?}", lines[2]);
            }
            assert_eq!(
                lines[top],
                "─".repeat(width as usize),
                "{view:?}@{width}: a full-width rule above the view"
            );
            assert_eq!(
                lines[28],
                "─".repeat(width as usize),
                "{view:?}@{width}: a full-width rule above the hints"
            );
            assert!(
                lines[29].trim_end().ends_with("? help  q quit"),
                "{view:?}@{width} hints: {:?}",
                lines[29]
            );
        }
    }
}

#[test]
fn without_a_banner_the_rule_follows_the_tabs_directly_and_the_view_starts_below_it() {
    for view in View::ALL {
        let mut a = App::new(Snapshot::default());
        a.view = view;
        let t = render_to_text(&a, 100, 30);
        let lines: Vec<&str> = t.lines().collect();
        assert!(lines[1].contains("[1] Now"), "{view:?}: {:?}", lines[1]);
        assert_eq!(lines[2], "─".repeat(100), "{view:?}: rule under the tabs");
        assert_ne!(
            lines[3],
            "─".repeat(100),
            "{view:?}: the view starts under the rule"
        );
    }
    let buf = render_to_buffer(&App::new(Snapshot::default()), 100, 30);
    assert!(
        buf[(0, 2)].modifier.contains(Modifier::DIM),
        "the top rule is secondary"
    );
}

#[test]
fn the_rule_and_the_hints_are_dim_and_accent() {
    let a = app();
    let buf = render_to_buffer(&a, 100, 30);
    assert!(
        buf[(0, 28)].modifier.contains(Modifier::DIM),
        "the rule is secondary"
    );
    // with a banner (a question waits) the rule sits one row lower, and is just as secondary
    let waiting = board_app(blocked_snapshot());
    let t = render_to_text(&waiting, 100, 30);
    assert!(t.lines().nth(2).unwrap().contains('■'), "{t}");
    assert_eq!(t.lines().nth(3).unwrap(), "─".repeat(100), "{t}");
    let buf = render_to_buffer(&waiting, 100, 30);
    assert!(
        buf[(0, 3)].modifier.contains(Modifier::DIM),
        "the rule under the banner is secondary too"
    );
    let (fg, m) = cell_style(&a, "? help", "?");
    assert_eq!((fg, m.contains(Modifier::BOLD)), (Color::Cyan, true));
}

#[test]
fn the_answer_overlay_is_a_centred_accent_box_with_the_worker_screen_and_the_mockup_hints() {
    for width in WIDTHS {
        let mut a = open_overlay();
        a.set_screen(
            "☐ Scope alias\n\nKeep the old --scope flag as an alias?\n\n❯ 1. Yes, keep it\n     `issue edit --scope` keeps replacing the whole list.\n  2. No, remove it\n  3. Type something.\n  4. Chat about this"
                .into(),
        );
        let t = render_to_text(&a, width, 30);
        assert!(
            t.contains("┌ answer asf-2 · Add greeting · worker pane, live "),
            "{width}:\n{t}"
        );
        for needle in [
            "Keep the old --scope flag as an alias?",
            "❯ 1. Yes, keep it",
            "4. Chat about this",
            "1-4 choose   Enter send the highlighted one   ↑/↓ move   t type a reply   Esc close, send nothing",
        ] {
            assert!(t.contains(needle), "{width}: {needle:?}:\n{t}");
        }
        // accent border, bold title, key letters accent + bold, words dim
        let (fg, _) = cell_style(&a, "┌ answer", "┌ answer");
        assert_eq!(fg, Color::Cyan, "accent border");
        let (fg, m) = cell_style(&a, "1-4 choose", "Enter");
        assert_eq!((fg, m.contains(Modifier::BOLD)), (Color::Cyan, true));
        assert!(cell_style(&a, "1-4 choose", "move").1.contains(Modifier::DIM));
        // centred: the same margin on both sides of the box, and the chrome rows stay visible
        let top = t.lines().find(|l| l.contains("┌ answer")).unwrap();
        // measured from the corners: the view behind the box may print text left and right of it
        let chars: Vec<char> = top.chars().collect();
        let left = chars.iter().position(|c| *c == '┌').unwrap();
        let box_w = chars.iter().rposition(|c| *c == '┐').unwrap() + 1 - left;
        assert_eq!(left * 2 + box_w, width as usize, "{width}: {top:?}");
        assert!(t.lines().last().unwrap().contains("answering asf-2"));
        assert!(t.lines().next().unwrap().contains("klobuk · hello"));
    }
}

#[test]
fn a_narrow_answer_overlay_shortens_its_hints_instead_of_cutting_them() {
    let mut a = open_overlay();
    a.set_screen("Do you want to proceed?\n 1. Yes\n 2. No".into());
    let t = render_to_text(&a, 80, 24);
    assert!(
        t.contains("1-2 choose   Enter send   ↑/↓ move   t type   Esc close"),
        "{t}"
    );
}

#[test]
fn the_number_of_choices_in_the_hint_follows_the_screen() {
    let mut a = open_overlay();
    a.set_screen("pick one\n 1. a\n 2. b\n 3. c".into());
    assert!(render_to_text(&a, 100, 24).contains("1-3 choose"));
    a.set_screen("no numbers here".into());
    assert!(render_to_text(&a, 100, 24).contains("1-9 choose"));
}

#[test]
fn every_style_comes_from_the_theme_and_no_color_leaves_only_modifiers() {
    let on = Theme::new(true);
    let off = Theme::new(false);
    // the styles the brief names
    assert!(on.bold().add_modifier.contains(Modifier::BOLD));
    assert!(on.dim().add_modifier.contains(Modifier::DIM));
    assert_eq!(on.key().fg, Some(Color::Cyan));
    assert!(on.key().add_modifier.contains(Modifier::BOLD));
    assert_eq!(on.ok().fg, Some(Color::Green));
    assert_eq!(on.warn().fg, Some(Color::Yellow));
    assert_eq!(on.danger().fg, Some(Color::Red));
    assert!(on.danger().add_modifier.contains(Modifier::BOLD));
    assert!(on.selected().bg.is_some(), "a highlighted background");
    assert_eq!(on.mark(Mark::Working).fg, Some(Color::Green));
    assert_eq!(on.mark(Mark::Review).fg, Some(Color::Yellow));
    assert_eq!(on.mark(Mark::Waiting).fg, None);
    // without colours: no foreground, no background anywhere, reverse video marks the selection
    for style in [
        off.bold(),
        off.dim(),
        off.accent(),
        off.key(),
        off.ok(),
        off.warn(),
        off.danger(),
        off.selected(),
        off.mark(Mark::Working),
        off.mark(Mark::Needs),
        off.mark(Mark::Review),
        off.mark(Mark::Done),
    ] {
        assert_eq!((style.fg, style.bg), (None, None), "{style:?}");
    }
    assert!(off.selected().add_modifier.contains(Modifier::REVERSED));
    assert!(
        off.danger().add_modifier.contains(Modifier::BOLD),
        "bold still tells ■ apart"
    );
}

#[test]
fn the_selected_row_has_a_highlighted_background_in_every_list_view() {
    let a = app();
    let buf = render_to_buffer(&a, 120, 30);
    // the Board: the first card is selected
    let has_bg = (0..120).any(|x| (0..30).any(|y| buf[(x, y)].bg != Color::Reset));
    assert!(has_bg, "a selected card is highlighted");
    let mut plain = app();
    plain.theme = Theme::new(false);
    let buf = render_to_buffer(&plain, 120, 30);
    let reversed = (0..120).any(|x| (0..30).any(|y| buf[(x, y)].modifier.contains(Modifier::REVERSED)));
    assert!(reversed, "reverse video without colours");
}

#[test]
fn every_view_renders_without_data_at_every_size_and_four_of_them_say_what_to_do() {
    for (w, h) in [(80u16, 20u16), (100, 30), (120, 30)] {
        for view in View::ALL {
            let mut a = App::new(Snapshot::default());
            a.view = view;
            let t = render_to_text(&a, w, h);
            assert!(t.lines().count() == h as usize, "{view:?}@{w}x{h}");
        }
    }
    for (view, sentence) in [
        (View::Agents, "No worker is running."),
        (View::Inbox, "Nothing needs you and nothing new to read."),
        (View::Queue, "Queue is empty."),
        (View::Log, "Nothing has happened yet."),
    ] {
        let mut a = App::new(Snapshot::default());
        a.view = view;
        let t = render_to_text(&a, 100, 30);
        assert!(t.contains(sentence), "{view:?}:\n{t}");
        assert!(
            cell_style(&a, sentence, sentence).1.contains(Modifier::DIM),
            "{view:?}: the sentence is secondary text"
        );
    }
}

#[test]
fn the_agents_view_says_why_the_runtime_is_down() {
    let mut s = desk(live(&["asf-1"]));
    s.runs = vec![run_at("asf-1", RunState::Working, "2026-11-06T11:00:00Z")];
    s.runtime_ok = Some(false);
    s.runtime_failures = 3;
    s.runtime_reason = Some("io: herdr status exited 1: socket busy".into());
    let mut a = App::new(s);
    a.view = View::Agents;
    let t = render_to_text(&a, 120, 30);
    assert!(t.contains("runtime DOWN"), "{t}");
    assert!(
        t.contains("The runtime is down, so the pane cannot be read."),
        "{t}"
    );
    assert!(
        t.contains("Last error: io: herdr status exited 1: socket busy"),
        "{t}"
    );
}

fn review_issue(i: &str) -> Issue {
    let mut r = issue(i, "Review it", Status::InProgress);
    r.kind = IssueType::Review;
    r
}

/// The header line of `snap` at width 120.
fn header_of(snap: Snapshot) -> String {
    render_to_text(&App::new(snap), 120, 30)
        .lines()
        .next()
        .unwrap()
        .to_string()
}

#[test]
fn the_header_counts_workers_like_the_scheduler_not_every_open_run() {
    // The owner's screenshot: slot limit 1, one worker, two reviewers and a stalled run, and the header
    // said `workers 4/1`. Reviewers have their own limit and a stalled run holds no slot.
    let mut s = desk(vec![
        issue("asf-1", "Worker", Status::InProgress),
        issue("asf-2", "Stalled", Status::InProgress),
        review_issue("asf-3"),
        review_issue("asf-4"),
    ]);
    s.max_parallel = 1;
    s.runs = vec![
        run_at("asf-1", RunState::Working, "2026-11-06T11:00:00Z"),
        run_at("asf-2", RunState::Stalled, "2026-11-06T11:01:00Z"),
        run_at("asf-3", RunState::Working, "2026-11-06T11:02:00Z"),
        run_at("asf-4", RunState::Starting, "2026-11-06T11:03:00Z"),
    ];
    let header = header_of(s.clone());
    assert!(header.contains("workers 1/1"), "{header}");
    assert!(header.contains("reviewers 2"), "{header}");
    assert!(!header.contains("workers 4"), "{header}");
    // the same number the scheduler reads for `klobuk run plan`
    let c = scheduler::count_active_runs(&s.issues, &s.runs);
    assert_eq!((c.workers, c.reviewers), (1, 2));
}

#[test]
fn the_header_counts_each_state_the_scheduler_counts_and_no_other() {
    for (state, counted) in [
        (RunState::Starting, true),
        (RunState::Working, true),
        (RunState::Blocked, true),
        (RunState::Finishing, true),
        (RunState::Verifying, true),
        (RunState::Stalled, false),
        (RunState::OverBudget, false),
    ] {
        let mut s = desk(live(&["asf-1"]));
        s.runs = vec![run_at("asf-1", state, "2026-11-06T11:00:00Z")];
        let header = header_of(s);
        let want = format!("workers {}/2", u8::from(counted));
        assert!(header.contains(&want), "{state}: {header}");
        assert!(!header.contains("reviewers"), "{state}: {header}");
    }
}

#[test]
fn a_reopened_run_is_one_worker_and_the_header_agrees_with_the_running_section() {
    // asf-1 came back from a review (`fixing after review`): its first run is over, the second works.
    // One worker, one slot, one RUNNING row; the two reviewers are RUNNING rows too, so the rows are
    // `workers + reviewers` while nobody is blocked.
    let mut s = desk(vec![
        issue("asf-1", "Reopened", Status::InProgress),
        issue("asf-2", "Plain", Status::InProgress),
        review_issue("asf-3"),
        review_issue("asf-4"),
    ]);
    let mut again = run_at("asf-1", RunState::Working, "2026-11-06T11:10:00Z");
    again.attempt = 2;
    s.runs = vec![
        run_at("asf-2", RunState::Working, "2026-11-06T11:00:00Z"),
        again,
        run_at("asf-3", RunState::Working, "2026-11-06T11:02:00Z"),
        run_at("asf-4", RunState::Verifying, "2026-11-06T11:03:00Z"),
    ];
    s.queue = vec![merge_item(
        1,
        "asf-1",
        MergeState::Failed,
        Some("review asks for changes"),
        "2026-11-06T11:09:00Z",
    )];
    let rows = now::running(&s).len();
    let header = header_of(s);
    assert!(header.contains("workers 2/2"), "{header}");
    assert!(header.contains("reviewers 2"), "{header}");
    assert_eq!(rows, 4, "RUNNING lists 2 workers + 2 reviewers");
}

#[test]
fn a_blocked_worker_holds_a_slot_but_is_listed_under_needs_you_not_running() {
    let mut s = desk(live(&["asf-1", "asf-2"]));
    s.runs = vec![
        run_at("asf-1", RunState::Blocked, "2026-11-06T11:00:00Z"),
        run_at("asf-2", RunState::Working, "2026-11-06T11:01:00Z"),
    ];
    assert_eq!(now::running(&s).len(), 1, "the blocked run is a NEEDS YOU row");
    let header = header_of(s);
    assert!(
        header.contains("workers 2/2"),
        "the scheduler counts it: {header}"
    );
}

#[test]
fn a_peek_keeps_the_pane_in_the_snapshot_for_the_views() {
    let mut a = open_overlay();
    a.set_pane_preview(id("asf-2"), "line 1\nline 2".into());
    assert_eq!(a.snap.preview_of(&id("asf-2")), Some("line 1\nline 2"));
    assert_eq!(a.snap.preview_of(&id("asf-3")), None);
}

#[test]
fn every_view_module_registers_bindings_for_its_own_view_only() {
    let own = |table: &'static [Binding], view: View| {
        for b in table {
            assert!(
                b.views.is_empty() || b.views == [view],
                "{view:?}: {:?} lists {:?}",
                b.key,
                b.views
            );
        }
    };
    own(now::BINDINGS, View::Now);
    own(board::BINDINGS, View::Board);
    own(agents::BINDINGS, View::Agents);
    own(inbox::BINDINGS, View::Inbox);
    own(queue::BINDINGS, View::Queue);
    own(log::BINDINGS, View::Log);
    own(config::BINDINGS, View::Config);
    // and the table the footer and the help page read is the sum of them
    let total = crate::tui::bindings::GLOBAL.len()
        + crate::tui::overlays::BINDINGS.len()
        + [
            now::BINDINGS.len(),
            board::BINDINGS.len(),
            agents::BINDINGS.len(),
            inbox::BINDINGS.len(),
            queue::BINDINGS.len(),
            log::BINDINGS.len(),
            config::BINDINGS.len(),
        ]
        .iter()
        .sum::<usize>();
    assert_eq!(all().count(), total);
}
