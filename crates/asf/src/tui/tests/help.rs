use super::*;

#[test]
fn footer_fits_100_columns_in_every_view_and_ends_with_help_and_quit() {
    let mut a = app();
    let mut seen = Vec::new();
    for c in '1'..='7' {
        a.handle_key(key(c));
        let hints = footer_hints(a.view, ModeKind::Normal);
        assert!(hints.len() <= 7, "{:?}: {} hints", a.view, hints.len());
        let f = footer_line(&a, 100);
        assert!(f.trim_end().ends_with("? help  q quit"), "{:?}: {f:?}", a.view);
        for h in &hints {
            assert!(
                f.contains(&format!("{} {}", h.key, h.label)),
                "{:?} lacks {}: {f:?}",
                a.view,
                h.key
            );
        }
        seen.push(f);
    }
    let (board, config) = (&seen[1], &seen[6]);
    assert_ne!(board, config, "Board and Config show different hints");
    assert!(board.contains("s start") && !board.contains("d unset"), "{board}");
    assert!(
        config.contains("d unset") && !config.contains("s start"),
        "{config}"
    );
}

#[test]
fn no_view_and_mode_combination_advertises_more_than_seven_hints() {
    for view in View::ALL {
        for kind in ModeKind::ALL {
            let n = footer_hints(view, kind).len();
            assert!(n <= 7, "{view:?}/{kind:?}: {n} hints");
        }
    }
}

#[test]
fn footer_keys_are_bold_and_their_actions_dim() {
    let a = app();
    let (fg, key_mod) = cell_style(&a, "? help", "?");
    assert!(key_mod.contains(Modifier::BOLD));
    assert_eq!(fg, Color::Cyan, "key letters are accent + bold");
    let (_, label_mod) = cell_style(&a, "? help", "help");
    assert!(label_mod.contains(Modifier::DIM), "the actions are dim");
}

#[test]
fn every_mode_has_a_footer_that_fits_100_columns() {
    let mut comment = inbox_app();
    comment.handle_key(code(KeyCode::Enter));
    let mut help = app();
    help.handle_key(key('?'));
    let mut confirm = app();
    for c in ['j', 'j', 'x'] {
        confirm.handle_key(key(c));
    }
    let f = footer_line(&comment, 100);
    assert!(f.contains("Esc close") && f.contains("j / k scroll"), "{f:?}");
    for (name, a) in [
        ("comment", comment),
        ("help", help),
        ("confirm", confirm),
        ("input", typing_app()),
    ] {
        let f = footer_line(&a, 100);
        assert!(!f.trim().is_empty(), "{name}: {f:?}");
        assert!(!f.contains("q quit"), "{name}: q is not quit here: {f:?}");
    }
    let f = footer_line(&typing_app(), 100);
    assert!(
        f.contains("reply to asf-2") && f.contains("Ctrl+W del word"),
        "{f:?}"
    );
}

#[test]
fn question_mark_opens_the_help_view_and_nothing_else_is_bound_meanwhile() {
    let mut a = app();
    a.handle_key(key('3'));
    assert_eq!(a.handle_key(key('?')), Action::None);
    assert!(matches!(a.mode(), Mode::Help { .. }));
    for k in [
        key('s'),
        key('t'),
        key('R'),
        key('m'),
        key('x'),
        key('f'),
        key('r'),
        key('a'),
        key('d'),
        key('1'),
        key('6'),
        key('M'),
        code(KeyCode::Enter),
        code(KeyCode::Tab),
        code(KeyCode::BackTab),
    ] {
        assert_eq!(a.handle_key(k), Action::None, "{k:?}");
        assert!(
            matches!(a.mode(), Mode::Help { .. }),
            "{k:?} must not leave the help view"
        );
        assert_eq!(a.view, View::Agents, "{k:?} must not change the view");
    }
    for closer in [code(KeyCode::Esc), key('?'), key('q')] {
        let mut b = app();
        b.handle_key(key('?'));
        assert_eq!(
            b.handle_key(closer),
            Action::None,
            "{closer:?} closes; q does not quit here"
        );
        assert!(matches!(b.mode(), Mode::Normal), "{closer:?}");
    }
}

#[test]
fn help_page_is_the_mockup_title_two_columns_of_groups_the_legend_and_its_own_hints() {
    let mut a = app();
    a.handle_key(key('?'));
    let t = render_to_text(&a, 100, 60);
    let wide = render_to_text(&a, 120, 60); // long explanations stay on one line here
    let lines: Vec<&str> = t.lines().collect();
    assert!(
        lines[0].starts_with(" Keys · everything the TUI understands, in plain words")
            && lines[0].ends_with("Esc close"),
        "{:?}",
        lines[0]
    );
    assert!(lines[1].starts_with("────────") && lines[1].chars().count() == 100);
    // the legend, a rule and the hints close the page
    let n = lines.len();
    assert_eq!(
        lines[n - 3].trim(),
        "SYMBOLS   ● working   ■ needs you   ◆ being checked   ✓ done   ○ waiting"
    );
    assert!(lines[n - 2].starts_with("────────"));
    assert_eq!(
        lines[n - 1].trim(),
        "j/k scroll   Esc close   ? close",
        "the mockup's hint row"
    );
    // every group of the page that has keys today, in two columns: the second column starts in the
    // middle of the screen (QUEUE returns when the Queue view binds keys of its own)
    let heading_at = |h: &str| lines.iter().position(|l| l.contains(h));
    for h in [
        "EVERYWHERE",
        "WHEN AN AGENT NEEDS YOU",
        "BOARD",
        "INBOX",
        "CONFIG",
    ] {
        assert!(heading_at(h).is_some(), "no heading {h}:\n{t}");
    }
    assert!(
        lines.iter().any(|l| l
            .chars()
            .skip(50)
            .collect::<String>()
            .starts_with(|c: char| c.is_ascii_uppercase())),
        "a second column starts at x=50:\n{t}"
    );
    // every binding of the views is on the page: its key and the first line of its explanation
    for b in all().filter(|b| b.modes.contains(&ModeKind::Normal) && b.group != Group::Window) {
        assert!(wide.contains(b.key), "missing key {:?}:\n{wide}", b.key);
        let first = b.help.lines().next().unwrap();
        assert!(wide.contains(first), "missing help {first:?}:\n{wide}");
    }
    assert!(t.contains("answer: shows the agent's question,"));
    assert!(t.contains("then 1-4, Enter or t to reply"));
    // on a short screen the tail is reached by scrolling
    let short = render_to_text(&a, 80, 20);
    assert!(!short.contains("back to the default"), "{short}");
    for _ in 0..200 {
        a.handle_key(key('j'));
    }
    let end = render_to_text(&a, 80, 20);
    assert!(end.contains("back to the default"), "scrolled to the end:\n{end}");
    a.handle_key(key('g'));
    assert!(render_to_text(&a, 80, 20).contains("EVERYWHERE"));
}

#[test]
fn when_an_agent_needs_you_lists_a_f_r_v_x_in_the_mockup_order_and_r_says_both_things() {
    use crate::tui::bindings::help_sections;
    let sections = help_sections();
    let needs = sections
        .iter()
        .find(|s| s.title == "WHEN AN AGENT NEEDS YOU")
        .expect("the group is on the page");
    let keys: Vec<&str> = needs.items.iter().map(|(k, _)| *k).collect();
    assert_eq!(keys, ["a", "f", "r", "v", "x"]);
    // `r` writes to the agent in four views and retries a merge in Now: one row says both
    let r = &needs.items[2].1;
    assert!(r.starts_with("send the agent a message"), "{r:?}");
    assert!(r.contains("Now") && r.contains("merge"), "{r:?}");
    // the QUEUE heading must not promise a key the Queue view does not have (there `r` replies)
    assert!(
        sections.iter().all(|s| s.title != "QUEUE"),
        "{:?}",
        sections.iter().map(|s| s.title).collect::<Vec<_>>()
    );
    // and the rendered page shows the rows in that order
    let mut a = app();
    a.handle_key(key('?'));
    let t = render_to_text(&a, 120, 60);
    let at = |needle: &str| {
        t.find(needle)
            .unwrap_or_else(|| panic!("{needle:?} missing:\n{t}"))
    };
    let order = [
        at("answer: shows"),
        at("focus the agent's own"),
        at("send the agent a message"),
        at("check a failed run again"),
        at("stop the agent (asks first)"),
    ];
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}:\n{t}");
}

#[test]
fn the_page_lists_a_binding_shared_by_several_views_once() {
    let mut a = app();
    a.handle_key(key('?'));
    let t = render_to_text(&a, 120, 60);
    assert_eq!(t.matches("focus the agent's own Herdr pane").count(), 1, "{t}");
    assert_eq!(t.matches("stop the agent (asks first)").count(), 1, "{t}");
}

#[test]
fn tab_and_shift_tab_cycle_the_views_and_wrap() {
    let mut a = App::new(snapshot());
    for expected in [
        View::Board,
        View::Agents,
        View::Inbox,
        View::Queue,
        View::Log,
        View::Config,
        View::Now,
    ] {
        assert_eq!(a.handle_key(code(KeyCode::Tab)), Action::None);
        assert_eq!(a.view, expected);
    }
    let back = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(a.handle_key(back), Action::None);
    assert_eq!(a.view, View::Config, "Shift+Tab from the first view wraps");
    a.handle_key(back);
    assert_eq!(a.view, View::Log);
    // some terminals send Tab + SHIFT instead of BackTab
    a.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT));
    assert_eq!(a.view, View::Queue);
}

#[test]
fn g_and_capital_g_jump_to_the_first_and_last_row() {
    let mut a = app();
    a.handle_key(key('G'));
    assert_eq!(a.selected_issue().unwrap().as_str(), "asf-4");
    a.handle_key(key('g'));
    assert_eq!(a.selected(View::Board), 0, "the first row is the group header");
    let mut c = app();
    c.handle_key(key('7'));
    c.handle_key(key('G'));
    assert_eq!(c.selected(View::Config), c.rows(View::Config) - 1);
    c.handle_key(key('g'));
    assert_eq!(c.selected(View::Config), 0);
    let mut empty = board_app(Snapshot::default());
    empty.handle_key(key('G')); // no rows: no panic
    empty.handle_key(key('g'));
}

#[test]
fn a_status_set_again_by_a_key_gets_a_fresh_four_seconds() {
    let mut a = app();
    a.advance(ms(0));
    a.handle_key(key('j')); // asf-3, ready
    a.busy = Some("tick".into());
    a.handle_key(key('t'));
    assert_eq!(a.status.as_deref(), Some("busy: tick"));
    a.advance(ms(0)); // stamped at t = 0
    a.advance(ms(3_000));
    // the same text again, set by a key before the next advance()
    a.handle_key(key('t'));
    assert_eq!(a.status.as_deref(), Some("busy: tick"));
    a.advance(ms(3_000)); // stamps the new message at t = 3 s
    a.advance(ms(6_000));
    assert_eq!(
        a.status.as_deref(),
        Some("busy: tick"),
        "the first message's 4 s must not cut the second one short"
    );
    a.advance(ms(3_000) + STATUS_TTL);
    assert!(a.status.is_none(), "gone 4 s after the second press");
}

#[test]
fn a_key_is_inert_in_every_view_the_table_does_not_list_it_for() {
    for view in View::ALL {
        for c in ['s', 'm', 'f', 'r', 'x', 'a', 'd', 'M', 'v', 'b'] {
            let listed = all().any(|b| b.key == c.to_string() && b.applies(view, ModeKind::Normal));
            if listed {
                continue;
            }
            let mut a = app();
            a.view = view;
            assert_eq!(a.handle_key(key(c)), Action::None, "{c} in {view:?}");
            assert!(
                matches!(a.mode(), Mode::Normal),
                "{c} in {view:?} opened {:?}",
                a.mode()
            );
            assert!(a.status.is_none(), "{c} in {view:?}: {:?}", a.status);
        }
    }
    // the cases the review named, spelled out
    let mut a = inbox_app();
    for c in ['s', 'f', 'x'] {
        assert_eq!(a.handle_key(key(c)), Action::None, "{c} in the Inbox");
    }
    assert_eq!(a.handle_key(key('m')), Action::MarkRead(id("asf-2")));
    let mut q = app();
    q.view = View::Queue;
    assert_eq!(q.handle_key(key('s')), Action::None);
    assert_eq!(q.handle_key(key('m')), Action::None);
    let mut g = app();
    g.view = View::Agents;
    assert_eq!(g.handle_key(key('s')), Action::None);
    assert_eq!(g.handle_key(key('m')), Action::Enqueue(id("asf-2")));
}

#[test]
fn the_table_lists_page_and_jump_keys_where_they_work() {
    let listed = |mode: ModeKind, word: &str| all().any(|b| b.modes.contains(&mode) && b.key.contains(word));
    // Home / End are aliases of g / G in the views; the table advertises the letters.
    assert!(listed(ModeKind::Normal, "g / G"));
    for mode in [ModeKind::Help, ModeKind::Comment] {
        for word in ["PageUp", "PageDown", "Home", "End"] {
            assert!(listed(mode, word), "{word} in {mode:?}");
        }
    }
    // the keys of an open window are in the footer while it is open, not on the page of the views
    let mut a = app();
    a.handle_key(key('?'));
    let t = render_to_text(&a, 100, 60);
    assert!(t.contains("scroll") && !t.contains("PageUp"), "{t}");
}

#[test]
fn page_and_jump_keys_do_what_the_help_says() {
    let mut s = snapshot();
    s.inbox[0].body = (1..=80)
        .map(|n| format!("line {n:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut a = board_app(s);
    a.handle_key(key('4'));
    a.handle_key(code(KeyCode::Enter));
    render_to_text(&a, 100, 30); // publishes the scroll limit
    a.handle_key(code(KeyCode::PageDown));
    assert!(render_to_text(&a, 100, 30).contains("line 11"));
    a.handle_key(code(KeyCode::End));
    assert!(render_to_text(&a, 100, 30).contains("line 80"));
    a.handle_key(code(KeyCode::PageUp));
    assert!(!render_to_text(&a, 100, 30).contains("line 80"));
    a.handle_key(code(KeyCode::Home));
    assert!(render_to_text(&a, 100, 30).contains("line 10"));
    // Home / End move the selection in normal mode
    let mut b = app();
    b.handle_key(code(KeyCode::End));
    assert_eq!(b.selected_issue().unwrap().as_str(), "asf-4");
    b.handle_key(code(KeyCode::Home));
    assert_eq!(b.selected(View::Board), 0);
}
