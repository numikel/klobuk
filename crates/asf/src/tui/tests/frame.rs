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
                lines[0].contains("asf · hello · main") && lines[0].contains("runtime ok"),
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
        assert!(t.lines().next().unwrap().contains("asf · hello"));
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
        (View::Queue, "The merge queue is empty."),
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
