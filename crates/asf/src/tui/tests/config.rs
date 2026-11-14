use super::*;
use asf_core::config_edit::{fields, FieldKind};

/// The footer for a key a running `asf serve` reloads, and for one only `asf plan` reads.
const TICK_NOTE: &str =
    "Saved to config.toml · comments kept · a running `asf serve` picks this up on its next tick";
const PLAN_NOTE: &str =
    "Saved to config.toml · comments kept · the next `asf plan` uses this; `asf serve` never does";

/// An app on the Config view whose snapshot holds the config text `toml`.
fn view(toml: &str) -> App {
    let mut s = snapshot();
    s.config = config_rows(toml);
    let mut a = board_app(s);
    a.handle_key(key('7'));
    a
}

/// The Config view with the selection moved to the field of `wanted`.
fn at(toml: &str, wanted: &str) -> App {
    config_app(toml, wanted)
}

/// The screen line that holds `label`.
fn row(text: &str, label: &str) -> String {
    text.lines()
        .find(|l| l.contains(label))
        .unwrap_or_else(|| panic!("no row {label:?}:\n{text}"))
        .to_string()
}

fn col(line: &str, needle: &str) -> usize {
    let byte = line
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in {line:?}"));
    line[..byte].chars().count()
}

/// Models set, budgets and flow away from their defaults: the mockup's sample data.
const BUDGETS: &str = "[planner]\nmodel = \"opus\"\nbudget_usd = 5.0\n[worker]\nbudget_usd = 10.0\nmodel = \"sonnet\"\n[scheduler]\nmax_parallel = 2\n[merge]\nverify_main_after = true\n[review]\nrequired = true\nmodel = \"opus\"\n";

#[test]
fn the_view_has_one_row_per_field_and_the_old_names_are_kept() {
    let a = view(Config::default_toml());
    assert_eq!(a.view, View::Config);
    assert_eq!(a.rows(View::Config), fields().len());
    assert_eq!(a.rows(View::Config), asf_core::config_edit::editable_keys().len());
}

#[test]
fn it_lists_the_groups_with_labels_instead_of_dotted_keys_at_120_columns() {
    let a = view(BUDGETS);
    let t = render_to_text(&a, 120, 40);
    for title in ["MODELS", "BUDGETS", "FLOW", "NOTIFICATIONS"] {
        assert!(t.contains(&format!(" {title}")), "{title}:\n{t}");
    }
    for f in fields() {
        assert!(t.contains(f.label), "label {}:\n{t}", f.label);
        assert!(t.contains(f.help), "help {}:\n{t}", f.help);
        assert!(!t.contains(f.key), "no dotted key {}:\n{t}", f.key);
    }
    // the first group carries its hint on the right
    assert!(
        row(&t, " MODELS")
            .trim_end()
            .ends_with("who thinks, who types, who checks"),
        "{t}"
    );
    assert!(!row(&t, " BUDGETS").contains("who thinks"));
    // groups appear in the mockup's order, a blank line after each title and each group
    let at = |s: &str| t.lines().position(|l| l.contains(s)).unwrap();
    assert!(
        at(" MODELS") < at(" BUDGETS") && at(" BUDGETS") < at(" FLOW") && at(" FLOW") < at(" NOTIFICATIONS")
    );
    assert_eq!(t.lines().nth(at(" BUDGETS") - 1).unwrap().trim(), "");
    assert_eq!(t.lines().nth(at(" BUDGETS") + 1).unwrap().trim(), "");
    // label, value and help sit in columns
    let planning = row(&t, "Planning  ");
    let workers = row(&t, "Workers  ");
    assert_eq!(
        col(&workers, "sonnet"),
        col(&planning, "opus") - 2,
        "value column (the selected row has `‹ `)"
    );
    assert_eq!(col(&row(&t, "Per issue"), "$10.00"), col(&workers, "sonnet"));
    assert_eq!(
        col(&row(&t, "Per issue"), "stops the worker"),
        col(&workers, "writes the code")
    );
}

#[test]
fn a_selected_choice_shows_the_current_one_in_brackets_and_the_others_after_it() {
    let a = at(BUDGETS, "planner.model");
    let t = render_to_text(&a, 120, 40);
    let planning = row(&t, "Planning  ");
    assert!(planning.contains("▸ Planning"), "{planning}");
    assert!(
        planning.contains("‹ opus ›   fable   sonnet   haiku"),
        "{planning}"
    );
    // the others are secondary text, the current one is not
    let (_, m) = cell_style(&a, "Planning  ", "sonnet");
    assert!(m.contains(Modifier::DIM), "the other choices are dim");
    let (_, m) = cell_style(&a, "Planning  ", "opus");
    assert!(!m.contains(Modifier::DIM));
    // a row that is not selected shows only its value
    let workers = row(&t, "Workers  ");
    assert!(workers.contains("sonnet") && !workers.contains('‹'), "{workers}");
    assert!(!workers.contains("haiku"), "{workers}");
}

#[test]
fn a_model_outside_the_list_is_kept_and_shown_as_the_current_choice() {
    let a = at("[planner]\nmodel = \"claude-haiku-5-5\"\n", "planner.model");
    let t = render_to_text(&a, 120, 40);
    let planning = row(&t, "Planning  ");
    // the value cell is 44 columns wide: a long full id leaves room for the first choices only (the
    // tail is clipped with `…`; h / l still reach every choice), a short custom name for all four
    assert!(
        planning.contains("‹ claude-haiku-5-5 ›   fable   opus   sonne"),
        "{planning}"
    );
    let short = at("[planner]\nmodel = \"my-model\"\n", "planner.model");
    let t = render_to_text(&short, 120, 40);
    assert!(
        row(&t, "Planning  ").contains("‹ my-model ›   fable   opus   sonnet   haiku"),
        "{t}"
    );
    let b = view("[planner]\nmodel = \"claude-haiku-5-5\"\n");
    let t = render_to_text(&b, 120, 40);
    assert!(row(&t, "Planning  ").contains("claude-haiku-5-5"), "{t}");
}

#[test]
fn a_value_that_differs_from_the_default_says_so_in_dim_text() {
    let a = view(BUDGETS);
    let t = render_to_text(&a, 120, 40);
    let step = row(&t, "Planning step");
    assert!(step.contains("$5.00") && step.contains("default $1.00"), "{step}");
    let issue = row(&t, "Per issue");
    assert!(
        issue.contains("$10.00") && issue.contains("default $5.00"),
        "{issue}"
    );
    assert!(row(&t, "Workers at once").contains("2") && row(&t, "Workers at once").contains("default 3"));
    assert!(row(&t, "Gates on main").contains("[x] on") && row(&t, "Gates on main").contains("default off"));
    assert!(row(&t, "Workers  ").contains("default (unset)"), "{t}");
    let (_, m) = cell_style(&a, "Per issue", "default $5.00");
    assert!(m.contains(Modifier::DIM));
    // at the default the note is absent
    let d = view(Config::default_toml());
    let t = render_to_text(&d, 120, 40);
    assert!(!row(&t, "Per issue").contains("default"), "{t}");
    assert!(row(&t, "Per issue").contains("$5.00"), "{t}");
}

#[test]
fn on_off_is_drawn_as_a_checkbox_and_a_short_choice_lists_its_options() {
    let a = view("[review]\nrequired = true\n");
    let t = render_to_text(&a, 120, 40);
    assert!(row(&t, "Review before merge").contains("[x] on"), "{t}");
    assert!(row(&t, "Gates on main").contains("[ ] off"), "{t}");
    assert!(row(&t, "Merge target").contains("local") && row(&t, "Merge target").contains("local · github"));
    assert!(row(&t, "Review rounds").contains('2'));
}

#[test]
fn an_unset_optional_key_and_an_unreadable_file_are_marked() {
    let a = view(Config::default_toml());
    assert!(row(&render_to_text(&a, 120, 40), "Workers  ").contains("(unset)"));
    let b = view("[scheduler");
    let t = render_to_text(&b, 120, 40);
    assert!(row(&t, "Workers at once").contains("(unreadable)"), "{t}");
}

#[test]
fn a_snapshot_without_config_rows_shows_the_defaults() {
    let mut a = board_app(Snapshot::default());
    a.handle_key(key('7'));
    let t = render_to_text(&a, 120, 40);
    assert!(row(&t, "Workers at once").contains('3') && !row(&t, "Workers at once").contains("default"));
}

#[test]
fn the_note_says_where_the_value_is_saved_and_when_it_applies() {
    // the first row (Planning) is selected: only `asf plan` reads it
    let t = render_to_text(&view(BUDGETS), 120, 30);
    assert!(t.contains(PLAN_NOTE), "{t}");
    let t = render_to_text(&view(BUDGETS), 120, 40);
    let note_at = t.lines().position(|l| l.contains(PLAN_NOTE)).unwrap();
    let last_row = t.lines().position(|l| l.contains("Repeat reminder")).unwrap();
    assert_eq!(
        note_at,
        last_row + 2,
        "one blank line between the list and the note:\n{t}"
    );
    let (_, m) = cell_style(&view(BUDGETS), PLAN_NOTE, "Saved to");
    assert!(m.contains(Modifier::DIM));
}

#[test]
fn the_note_follows_the_selected_row() {
    let t = render_to_text(&at(BUDGETS, "worker.budget_usd"), 120, 40);
    assert!(
        t.contains(TICK_NOTE) && !t.contains("`asf serve` never does"),
        "{t}"
    );
    let t = render_to_text(&at(BUDGETS, "planner.budget_usd"), 120, 40);
    assert!(t.contains(PLAN_NOTE) && !t.contains("picks this up"), "{t}");
    // never a restart hint: no key of the view needs one
    for f in fields() {
        let t = render_to_text(&at(BUDGETS, f.key), 120, 40);
        assert!(!t.contains("restart"), "{}:\n{t}", f.key);
        assert!(
            t.contains(asf_core::config_edit::apply_note(f.key)),
            "{}:\n{t}",
            f.key
        );
    }
}

#[test]
fn the_note_fits_100_columns_for_every_key() {
    for f in fields() {
        let t = render_to_text(&at(BUDGETS, f.key), 100, 40);
        assert!(
            t.contains(asf_core::config_edit::apply_note(f.key)),
            "{} is cut at 100 columns:\n{t}",
            f.key
        );
    }
}

#[test]
fn it_fits_100_columns_and_truncates_with_an_ellipsis() {
    let a = at("[planner]\nmodel = \"claude-haiku-5-5\"\n", "planner.model");
    let t = render_to_text(&a, 100, 40);
    for l in t.lines() {
        assert!(l.chars().count() <= 100, "{l:?}");
    }
    let planning = row(&t, "Planning  ");
    assert!(
        planning.contains('…'),
        "the long help or choices are cut: {planning}"
    );
    assert!(planning.contains("‹ claude-haiku-5-5 ›"), "{planning}");
    for label in ["Review before merge", "Gates on main", "Per issue"] {
        assert!(t.contains(label), "{label}:\n{t}");
    }
    assert!(t.contains(PLAN_NOTE), "{t}");
}

#[test]
fn the_selected_row_has_a_highlighted_background_or_reverse_video() {
    let a = at(BUDGETS, "worker.budget_usd");
    let buf = render_to_buffer(&a, 120, 40);
    let y = (0..40u16)
        .find(|y| {
            (0..120u16)
                .map(|x| buf[(x, *y)].symbol().to_string())
                .collect::<String>()
                .contains("Per issue")
        })
        .unwrap();
    for x in [5u16, 30, 119] {
        assert_eq!(buf[(x, y)].bg, Color::Indexed(238), "x={x}");
    }
    let other = y - 1;
    assert_ne!(buf[(5, other)].bg, Color::Indexed(238));
    let mut plain = at(BUDGETS, "worker.budget_usd");
    plain.theme = Theme::new(false);
    let buf = render_to_buffer(&plain, 120, 40);
    assert!(buf[(5, y)].modifier.contains(Modifier::REVERSED));
    assert!(
        row(&render_to_text(&plain, 120, 40), "Per issue").contains('▸'),
        "the marker is not colour"
    );
}

#[test]
fn a_long_list_scrolls_to_keep_the_selected_row_and_the_note_visible() {
    let mut a = view(Config::default_toml());
    let t = render_to_text(&a, 100, 20);
    assert!(t.contains("Planning") && t.contains(PLAN_NOTE), "{t}");
    assert!(!t.contains("Repeat reminder"), "does not fit 14 rows:\n{t}");
    a.handle_key(key('G'));
    let t = render_to_text(&a, 100, 20);
    assert!(row(&t, "Repeat reminder").contains('▸'), "{t}");
    assert!(t.contains(TICK_NOTE) && !t.contains("Planning  "), "{t}");
    a.handle_key(key('k'));
    let t = render_to_text(&a, 100, 20);
    assert!(
        row(&t, "First reminder").contains('▸') && t.contains("Repeat reminder"),
        "{t}"
    );
    a.handle_key(key('g'));
    let t = render_to_text(&a, 100, 20);
    assert!(
        t.contains(" MODELS") && row(&t, "Planning  ").contains('▸'),
        "{t}"
    );
}

#[test]
fn j_and_k_move_the_selection_and_stop_at_the_ends() {
    let mut a = view(Config::default_toml());
    assert_eq!(a.selected(View::Config), 0);
    a.handle_key(key('k'));
    assert_eq!(a.selected(View::Config), 0);
    a.handle_key(key('j'));
    a.handle_key(code(KeyCode::Down));
    assert_eq!(a.selected(View::Config), 2);
    a.handle_key(key('G'));
    a.handle_key(key('j'));
    assert_eq!(a.selected(View::Config), fields().len() - 1);
}

#[test]
fn l_and_h_step_through_the_choices_and_write_at_once() {
    let mut a = at("[planner]\nmodel = \"opus\"\n", "planner.model");
    assert_eq!(
        a.handle_key(key('l')),
        Action::SetConfig("planner.model".into(), "sonnet".into())
    );
    assert_eq!(
        a.handle_key(code(KeyCode::Right)),
        Action::SetConfig("planner.model".into(), "sonnet".into())
    );
    assert_eq!(
        a.handle_key(key('h')),
        Action::SetConfig("planner.model".into(), "fable".into()),
        "fable, the strongest, comes before opus"
    );
    // before the first comes the last
    let mut first = at("[planner]\nmodel = \"fable\"\n", "planner.model");
    assert_eq!(
        first.handle_key(key('h')),
        Action::SetConfig("planner.model".into(), "haiku".into())
    );
    // from the last, next wraps to the first
    let mut b = at("[planner]\nmodel = \"haiku\"\n", "planner.model");
    assert_eq!(
        b.handle_key(key('l')),
        Action::SetConfig("planner.model".into(), "fable".into())
    );
    // no overlay opens: the write is the action
    assert!(matches!(b.mode(), Mode::Normal));
    let mut m = at("[merge]\ntarget = \"local\"\n", "merge.target");
    assert_eq!(
        m.handle_key(key('l')),
        Action::SetConfig("merge.target".into(), "github".into())
    );
    assert_eq!(
        m.handle_key(key('h')),
        Action::SetConfig("merge.target".into(), "github".into()),
        "two choices: previous and next meet"
    );
}

#[test]
fn a_value_outside_the_list_steps_into_the_list_from_either_side() {
    let toml = "[planner]\nmodel = \"claude-haiku-5-5\"\n";
    let mut a = at(toml, "planner.model");
    assert_eq!(
        a.handle_key(key('l')),
        Action::SetConfig("planner.model".into(), "fable".into())
    );
    assert_eq!(
        a.handle_key(key('h')),
        Action::SetConfig("planner.model".into(), "haiku".into())
    );
    // an unset optional key too
    let mut w = at(Config::default_toml(), "worker.model");
    assert_eq!(
        w.handle_key(key('l')),
        Action::SetConfig("worker.model".into(), "fable".into())
    );
}

#[test]
fn h_and_l_on_a_row_that_is_not_a_choice_only_say_what_the_key_does() {
    let mut a = at(Config::default_toml(), "review.required");
    assert_eq!(a.handle_key(key('l')), Action::None);
    assert!(
        a.status.as_deref().unwrap_or("").contains("Space"),
        "{:?}",
        a.status
    );
    let mut n = at(Config::default_toml(), "scheduler.max_parallel");
    assert_eq!(n.handle_key(key('h')), Action::None);
    assert!(
        n.status.as_deref().unwrap_or("").contains("Enter"),
        "{:?}",
        n.status
    );
}

#[test]
fn space_toggles_an_on_off_setting() {
    let mut a = at("[review]\nrequired = false\n", "review.required");
    assert_eq!(
        a.handle_key(key(' ')),
        Action::SetConfig("review.required".into(), "true".into())
    );
    let mut b = at("[review]\nrequired = true\n", "review.required");
    assert_eq!(
        b.handle_key(key(' ')),
        Action::SetConfig("review.required".into(), "false".into())
    );
    let mut c = at(Config::default_toml(), "merge.verify_main_after");
    assert_eq!(
        c.handle_key(key(' ')),
        Action::SetConfig("merge.verify_main_after".into(), "true".into())
    );
    let mut n = at(Config::default_toml(), "scheduler.max_parallel");
    assert_eq!(n.handle_key(key(' ')), Action::None);
    assert!(
        n.status.as_deref().unwrap_or("").contains("Enter"),
        "{:?}",
        n.status
    );
}

#[test]
fn enter_opens_the_input_prefilled_and_the_second_enter_returns_the_edit() {
    let mut a = at(Config::default_toml(), "scheduler.max_parallel");
    assert_eq!(a.handle_key(code(KeyCode::Enter)), Action::None);
    match &a.mode() {
        Mode::Input { prompt, buf, target } => {
            assert!(prompt.contains("Workers at once"), "{prompt}");
            assert_eq!(buf, "3", "prefilled with the current value");
            assert_eq!(*target, InputTarget::Config("scheduler.max_parallel".into()));
        }
        other => panic!("expected input mode, got {other:?}"),
    }
    a.handle_key(code(KeyCode::Backspace));
    a.handle_key(key('5'));
    assert_eq!(
        a.handle_key(code(KeyCode::Enter)),
        Action::SetConfig("scheduler.max_parallel".into(), "5".into())
    );
    assert!(matches!(a.mode(), Mode::Normal));
}

#[test]
fn enter_on_a_choice_types_a_custom_model_and_on_an_unset_one_starts_empty() {
    let mut a = at("[planner]\nmodel = \"opus\"\n", "planner.model");
    a.handle_key(code(KeyCode::Enter));
    assert_eq!(buf_of(&a), "opus");
    for _ in 0..4 {
        a.handle_key(code(KeyCode::Backspace));
    }
    for c in "claude-opus-5-5".chars() {
        a.handle_key(key(c));
    }
    assert_eq!(
        a.handle_key(code(KeyCode::Enter)),
        Action::SetConfig("planner.model".into(), "claude-opus-5-5".into())
    );
    let mut u = at(Config::default_toml(), "worker.model");
    u.handle_key(code(KeyCode::Enter));
    assert_eq!(buf_of(&u), "");
}

#[test]
fn enter_on_an_on_off_row_points_to_space() {
    let mut a = at(Config::default_toml(), "review.required");
    assert_eq!(a.handle_key(code(KeyCode::Enter)), Action::None);
    assert!(matches!(a.mode(), Mode::Normal));
    assert!(
        a.status.as_deref().unwrap_or("").contains("Space"),
        "{:?}",
        a.status
    );
}

#[test]
fn esc_in_the_input_cancels_without_an_action() {
    let mut a = at(Config::default_toml(), "scheduler.max_parallel");
    a.handle_key(code(KeyCode::Enter));
    a.handle_key(key('x'));
    assert_eq!(a.handle_key(code(KeyCode::Esc)), Action::None);
    assert!(matches!(a.mode(), Mode::Normal));
    assert_eq!(a.view, View::Config);
}

#[test]
fn a_refused_value_is_shown_under_its_row_until_the_next_key() {
    let mut a = at(Config::default_toml(), "scheduler.max_parallel");
    a.config.fail(
        "scheduler.max_parallel",
        "invalid config: scheduler.max_parallel must be >= 1",
    );
    let t = render_to_text(&a, 120, 40);
    let at_row = t.lines().position(|l| l.contains("Workers at once")).unwrap();
    let under = t.lines().nth(at_row + 1).unwrap();
    assert!(under.contains("error: ") && under.contains("must be >= 1"), "{t}");
    assert!(
        col(under, "error:") == col(&row(&t, "Workers at once"), "3"),
        "the error starts at the value column:\n{t}"
    );
    // the other rows keep their place below it
    assert!(t.lines().nth(at_row + 2).unwrap().contains("Review before merge"));
    // an error belongs to the row of its own key, whichever row is selected
    let mut b = at(Config::default_toml(), "scheduler.max_parallel");
    b.config.fail("merge.target", "boom");
    let t = render_to_text(&b, 120, 40);
    let target_at = t.lines().position(|l| l.contains("Merge target")).unwrap();
    assert!(
        t.lines().nth(target_at + 1).unwrap().contains("error: boom"),
        "{t}"
    );
    assert_eq!(t.matches("boom").count(), 1);
    // any key retires it
    a.handle_key(key('j'));
    assert!(!render_to_text(&a, 120, 40).contains("must be >= 1"));
    // a long message is cut to the screen
    let mut c = at(Config::default_toml(), "scheduler.max_parallel");
    c.config.fail("scheduler.max_parallel", &"x".repeat(200));
    let t = render_to_text(&c, 100, 40);
    assert!(t.lines().all(|l| l.chars().count() <= 100) && t.contains('…'));
}

#[test]
fn the_event_loop_hands_a_refused_edit_to_the_view_and_nothing_else() {
    use crate::tui::note_config_result;
    let (dir, _) = config_dir(Config::default_toml());
    let set = Action::SetConfig("scheduler.max_parallel".into(), "0".into());
    let msg = config_set_status(dir.path(), "scheduler.max_parallel", "0");
    let mut a = at(Config::default_toml(), "scheduler.max_parallel");
    note_config_result(&mut a, &set, &msg);
    let t = render_to_text(&a, 120, 40);
    assert!(
        t.contains("error: invalid config: scheduler.max_parallel must be >= 1"),
        "{t}"
    );
    assert!(!t.contains("error: error"), "the prefix is not doubled:\n{t}");
    // an unset that the file refuses is shown the same way
    let unset = Action::UnsetConfig("scheduler.max_parallel".into());
    let msg = config_unset_status(dir.path(), "scheduler.max_parallel");
    let mut u = at(Config::default_toml(), "scheduler.max_parallel");
    note_config_result(&mut u, &unset, &msg);
    assert!(render_to_text(&u, 120, 40).contains("cannot be unset"));
    // a saved value, or another action, leaves nothing under the row
    let mut b = at(Config::default_toml(), "scheduler.max_parallel");
    note_config_result(
        &mut b,
        &set,
        "scheduler.max_parallel: 3 -> 5 (applies on the next tick)",
    );
    note_config_result(&mut b, &Action::Tick, "error: not a config edit");
    let t = render_to_text(&b, 120, 40);
    assert!(!t.contains("error"), "{t}");
}

#[test]
fn a_saved_edit_says_a_running_serve_picks_it_up_on_its_next_tick() {
    let (dir, _) = config_dir(Config::default_toml());
    let msg = config_set_status(dir.path(), "scheduler.max_parallel", "5");
    assert_eq!(
        msg,
        "scheduler.max_parallel: 3 -> 5 (a running `asf serve` picks this up on its next tick)"
    );
    assert!(!msg.contains("restart"), "{msg}");
}

#[test]
fn a_saved_planner_edit_says_serve_never_uses_it() {
    let (dir, _) = config_dir(Config::default_toml());
    let msg = config_set_status(dir.path(), "planner.budget_usd", "2");
    assert!(
        msg.ends_with("(the next `asf plan` uses this; `asf serve` never does)"),
        "{msg}"
    );
    assert!(!msg.contains("restart") && !msg.contains("next tick"), "{msg}");
    // an unset says the same as a set of the same key
    let msg = config_unset_status(dir.path(), "worker.model");
    assert!(msg.ends_with("picks this up on its next tick)"), "{msg}");
}

#[test]
fn d_puts_a_key_back_to_its_default_after_a_yes() {
    let mut a = at(BUDGETS, "planner.budget_usd");
    assert_eq!(a.handle_key(key('d')), Action::None);
    match &a.mode() {
        Mode::Confirm { what, .. } => {
            assert!(what.contains("Planning step") && what.contains("$1.00"), "{what}")
        }
        other => panic!("expected confirm, got {other:?}"),
    }
    let t = render_to_text(&a, 120, 30);
    assert!(footer_text(&t).contains("Planning step"), "{t}");
    assert_eq!(
        a.handle_key(key('y')),
        Action::SetConfig("planner.budget_usd".into(), "1.0".into())
    );
    assert!(matches!(a.mode(), Mode::Normal));
}

#[test]
fn d_removes_an_optional_key_with_unset() {
    for key_name in ["worker.model", "review.model"] {
        let mut a = config_app(
            "[worker]\nmodel = \"sonnet\"\n[review]\nmodel = \"sonnet\"\n",
            key_name,
        );
        assert_eq!(a.handle_key(key('d')), Action::None);
        assert!(matches!(a.mode(), Mode::Confirm { .. }), "{key_name}");
        assert_eq!(a.handle_key(key('y')), Action::UnsetConfig(key_name.into()));
        assert!(matches!(a.mode(), Mode::Normal));
    }
}

#[test]
fn any_other_key_cancels_the_return_to_default() {
    for cancel in [
        key('n'),
        key('q'),
        code(KeyCode::Enter),
        code(KeyCode::Esc),
        key('d'),
    ] {
        let mut a = config_app("[worker]\nmodel = \"sonnet\"\n", "worker.model");
        a.handle_key(key('d'));
        assert_eq!(a.handle_key(cancel), Action::None);
        assert!(matches!(a.mode(), Mode::Normal));
    }
}

#[test]
fn d_on_a_key_that_is_already_at_its_default_only_says_so() {
    for wanted in [
        "scheduler.max_parallel",
        "worker.model",
        "review.model",
        "merge.target",
    ] {
        let mut a = at(Config::default_toml(), wanted);
        assert_eq!(a.handle_key(key('d')), Action::None, "{wanted}");
        assert!(matches!(a.mode(), Mode::Normal), "{wanted}");
        assert!(
            a.status.as_deref().unwrap_or("").contains("already"),
            "{wanted}: {:?}",
            a.status
        );
    }
    // d outside the Config view does nothing
    let mut b = app();
    assert_eq!(b.handle_key(key('d')), Action::None);
    assert!(matches!(b.mode(), Mode::Normal) && b.status.is_none());
}

#[test]
fn every_field_kind_has_a_key_that_edits_it() {
    for f in fields() {
        let mut a = at("[planner]\nmodel = \"opus\"\n", f.key);
        let action = match f.kind {
            FieldKind::Choice(_) => a.handle_key(key('l')),
            FieldKind::Bool => a.handle_key(key(' ')),
            FieldKind::Number | FieldKind::Text => {
                a.handle_key(code(KeyCode::Enter));
                if f.default_value().is_none() {
                    // an optional number starts empty (`serve.max_ticks`): there is nothing to confirm
                    a.handle_key(key('5'));
                }
                a.handle_key(code(KeyCode::Enter))
            }
        };
        assert!(
            matches!(action, Action::SetConfig(ref k, _) if k == f.key),
            "{}: {action:?}",
            f.key
        );
    }
}

#[test]
fn the_hints_of_the_footer_are_the_ones_of_the_mockup() {
    let a = view(Config::default_toml());
    let f = footer_line(&a, 100);
    assert!(
        f.contains(
            "j / k move  h / l choose  Space toggle  Enter type a value  d back to default  ? help  q quit"
        ),
        "{f:?}"
    );
    assert!(f.chars().count() <= 100);
    assert_eq!(footer_hints(View::Config, ModeKind::Normal).len(), 7);
}

#[test]
fn the_help_page_explains_the_config_keys() {
    let sections = crate::tui::bindings::help_sections();
    let config = sections.iter().find(|s| s.title == "CONFIG").unwrap();
    let keys: Vec<&str> = config.items.iter().map(|(k, _)| *k).collect();
    for k in ["h / l", "Space", "Enter", "d"] {
        assert!(keys.contains(&k), "{k} in {keys:?}");
    }
}

#[test]
fn the_table_header_and_the_restart_hint_of_the_old_view_are_gone() {
    let t = render_to_text(&view(Config::default_toml()), 120, 30);
    assert!(!t.contains("description"), "{t}");
    assert!(!t.contains("restart"), "{t}");
}

#[test]
fn config_rows_read_values_from_the_toml_text() {
    let rows = config_rows("[scheduler]\nmax_parallel = 7\n[merge]\ntarget = \"github\"\n");
    let get = |k: &str| rows.iter().find(|r| r.key == k).unwrap();
    assert_eq!(get("scheduler.max_parallel").raw, "7");
    assert_eq!(get("merge.target").raw, "github");
    assert_eq!(get("worker.model").raw, "");
    assert_eq!(get("worker.model").value, "(unset)");
    assert_eq!(rows.len(), asf_core::config_edit::editable_keys().len());
}

#[test]
fn confirmed_edit_is_written_to_config_toml_and_reported() {
    let (dir, path) = config_dir(Config::default_toml());
    let msg = config_set_status(dir.path(), "scheduler.max_parallel", "5");
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(after.contains("max_parallel = 5"), "file not updated:\n{after}");
    assert!(
        after.contains("# concurrent workers"),
        "inline comment must survive:\n{after}"
    );
    assert!(!msg.starts_with("error"), "{msg}");
    assert!(
        msg.contains("scheduler.max_parallel") && msg.contains('5'),
        "{msg}"
    );
}

#[test]
fn invalid_value_leaves_the_file_unchanged_and_the_error_names_the_key() {
    let (dir, path) = config_dir(Config::default_toml());
    let before = std::fs::read(&path).unwrap();
    let msg = config_set_status(dir.path(), "scheduler.max_parallel", "0");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "file must be byte-identical"
    );
    assert!(msg.starts_with("error"), "{msg}");
    let msg2 = config_set_status(dir.path(), "merge.target", "svn");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(msg2.starts_with("error"), "{msg2}");
    let mut a = app();
    a.view = View::Config;
    a.status = Some(msg);
    let text = render_to_text(&a, 120, 30);
    assert!(text.contains("error") && text.contains("max_parallel"), "{text}");
}

#[test]
fn confirmed_unset_removes_the_line_from_config_toml_and_reports() {
    let (dir, path) = config_dir("# keep me\n[worker]\nbudget_usd = 5.0\nmodel = \"sonnet\"\n");
    let msg = config_unset_status(dir.path(), "worker.model");
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after, "# keep me\n[worker]\nbudget_usd = 5.0\n");
    assert!(
        !msg.starts_with("error") && msg.contains("worker.model") && msg.contains("(unset)"),
        "{msg}"
    );
}

#[test]
fn refused_unset_leaves_the_file_byte_identical() {
    let (dir, path) = config_dir(Config::default_toml());
    let before = std::fs::read(&path).unwrap();
    let msg = config_unset_status(dir.path(), "scheduler.max_parallel");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(msg.starts_with("error"), "{msg}");
}

/// The last line of a rendered frame (the footer).
fn footer_text(t: &str) -> String {
    t.lines().last().unwrap_or("").to_string()
}
