use super::*;

#[test]
fn key_six_opens_config_view_listing_every_editable_key_with_its_value() {
    let mut a = app();
    assert_eq!(a.handle_key(key('7')), Action::None);
    assert_eq!(a.view, View::Config);
    assert_eq!(a.rows(View::Config), asf_core::config_edit::editable_keys().len());
    let text = render_to_text(&a, 100, 30);
    for (k, _) in asf_core::config_edit::editable_keys() {
        assert!(text.contains(k), "missing key {k} in:\n{text}");
    }
    // current values come from the config text: defaults are worker.budget_usd = 5.0, merge.target = local
    assert!(text.contains("local"), "{text}");
    assert!(
        text.contains("(unset)"),
        "worker.model is unset by default:\n{text}"
    );
}

#[test]
fn enter_opens_prefilled_input_and_second_enter_returns_the_edit() {
    let mut a = app();
    select_config_key(&mut a, "scheduler.max_parallel");
    assert_eq!(a.handle_key(code(KeyCode::Enter)), Action::None);
    match &a.mode() {
        Mode::Input { prompt, buf, .. } => {
            assert!(prompt.contains("scheduler.max_parallel"), "{prompt}");
            assert_eq!(buf, "3", "prefilled with the current value");
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
fn esc_in_config_input_cancels_without_an_action() {
    let mut a = app();
    select_config_key(&mut a, "merge.target");
    a.handle_key(code(KeyCode::Enter));
    a.handle_key(key('x'));
    assert_eq!(a.handle_key(code(KeyCode::Esc)), Action::None);
    assert!(matches!(a.mode(), Mode::Normal));
    assert_eq!(a.view, View::Config);
}

#[test]
fn enter_on_a_view_without_rows_does_not_open_input() {
    let mut a = board_app(Snapshot::default());
    a.handle_key(key('7'));
    assert_eq!(a.handle_key(code(KeyCode::Enter)), Action::None);
    assert!(matches!(a.mode(), Mode::Normal));
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
fn invalid_value_leaves_the_file_unchanged_and_status_line_shows_the_error() {
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
fn d_asks_for_confirmation_and_y_returns_the_unset_action() {
    for key_name in ["worker.model", "review.model"] {
        let mut a = config_app(
            "[worker]\nmodel = \"sonnet\"\n[review]\nmodel = \"sonnet\"\n",
            key_name,
        );
        assert_eq!(a.handle_key(key('d')), Action::None);
        match &a.mode() {
            Mode::Confirm { what, .. } => {
                assert!(what.contains(&format!("unset {key_name}?")), "{what}")
            }
            other => panic!("expected confirm, got {other:?}"),
        }
        let t = render_to_text(&a, 120, 30);
        assert!(t.contains(&format!("unset {key_name}?")), "{t}");
        assert_eq!(a.handle_key(key('y')), Action::UnsetConfig(key_name.into()));
        assert!(matches!(a.mode(), Mode::Normal));
    }
}

#[test]
fn any_other_key_cancels_the_unset() {
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
fn d_on_a_required_key_or_an_already_unset_one_only_sets_a_status() {
    let mut a = config_app(Config::default_toml(), "scheduler.max_parallel");
    assert_eq!(a.handle_key(key('d')), Action::None);
    assert!(matches!(a.mode(), Mode::Normal));
    assert!(
        a.status.as_deref().unwrap_or("").contains("cannot be unset"),
        "{:?}",
        a.status
    );
    let mut a = config_app(Config::default_toml(), "worker.model"); // unset by default
    assert_eq!(a.handle_key(key('d')), Action::None);
    assert!(matches!(a.mode(), Mode::Normal));
    assert!(
        a.status.as_deref().unwrap_or("").contains("already unset"),
        "{:?}",
        a.status
    );
    // d outside the Config view does nothing
    let mut b = app();
    assert_eq!(b.handle_key(key('d')), Action::None);
    assert!(matches!(b.mode(), Mode::Normal) && b.status.is_none());
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
