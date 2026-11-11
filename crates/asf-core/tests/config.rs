use asf_core::config::CleanKind;
use asf_core::{Config, ConfigError};

#[test]
fn notify_defaults_are_fifteen_then_thirty_and_no_command() {
    let c = Config::from_toml("").unwrap();
    assert_eq!(c.notify.after_minutes, 15);
    assert_eq!(c.notify.every_minutes, 30);
    assert!(c.notify.command.is_empty());
}

#[test]
fn on_new_defaults_to_true_and_can_be_switched_off_on_its_own() {
    assert!(Config::from_toml("").unwrap().notify.on_new);
    assert!(
        Config::from_toml("[notify]\nafter_minutes = 0\n")
            .unwrap()
            .notify
            .on_new
    );
    let c = Config::from_toml("[notify]\non_new = false\n").unwrap();
    assert!(!c.notify.on_new);
    assert_eq!(c.notify.after_minutes, 15, "the reminders keep their defaults");
    assert!(Config::from_toml("[notify]\non_new = \"no\"\n").is_err());
}

#[test]
fn notify_section_overrides_and_keeps_the_other_defaults() {
    let c =
        Config::from_toml("[notify]\nafter_minutes = 5\ncommand = [\"pwsh\", \"-File\", \"notify.ps1\"]\n")
            .unwrap();
    assert_eq!(c.notify.after_minutes, 5);
    assert_eq!(c.notify.every_minutes, 30);
    assert_eq!(c.notify.command, ["pwsh", "-File", "notify.ps1"]);
}

#[test]
fn zero_minutes_are_allowed() {
    let c = Config::from_toml("[notify]\nafter_minutes = 0\nevery_minutes = 0\n").unwrap();
    assert_eq!(c.notify.after_minutes, 0);
    assert_eq!(c.notify.every_minutes, 0);
}

#[test]
fn a_negative_or_textual_delay_is_refused() {
    for bad in ["-1", "\"soon\"", "1.5"] {
        let e = Config::from_toml(&format!("[notify]\nafter_minutes = {bad}\n")).unwrap_err();
        assert!(matches!(e, ConfigError::Parse(_)), "{bad}: {e}");
    }
}

#[test]
fn a_command_without_a_program_is_refused() {
    let e = Config::from_toml("[notify]\ncommand = [\"\"]\n").unwrap_err();
    assert!(
        matches!(e, ConfigError::Invalid(ref m) if m.contains("notify.command")),
        "{e}"
    );
    assert!(
        Config::from_toml("[notify]\ncommand = []\n").is_ok(),
        "empty means no command"
    );
}

#[test]
fn the_template_documents_notify_and_still_parses_to_the_defaults() {
    let t = Config::default_toml();
    assert!(t.contains("[notify]"), "{t}");
    assert!(t.contains("on_new = true"), "{t}");
    assert!(t.contains("after_minutes = 15"), "{t}");
    assert!(t.contains("every_minutes = 30"), "{t}");
    assert!(
        t.contains("# command = [\"pwsh\", \"-File\", \"notify.ps1\"]"),
        "{t}"
    );
    for var in [
        "ASF_REMIND_ISSUE",
        "ASF_REMIND_KIND",
        "ASF_REMIND_AGE_MINUTES",
        "ASF_REMIND_TEXT",
    ] {
        assert!(t.contains(var), "{var} not documented");
    }
    assert_eq!(Config::from_toml(t).unwrap(), Config::default());
}

#[test]
fn clean_after_defaults_to_the_worktree_and_the_review_worktrees() {
    let want = [CleanKind::Worktree, CleanKind::ReviewWorktrees];
    assert_eq!(Config::default().merge.clean_after, want);
    assert_eq!(Config::from_toml("").unwrap().merge.clean_after, want);
    let c = Config::from_toml("[merge]\nverify_main_after = true\n").unwrap();
    assert_eq!(c.merge.clean_after, want, "another [merge] key keeps the default");
}

#[test]
fn clean_after_takes_any_subset_and_an_empty_list_keeps_everything() {
    let c = Config::from_toml("[merge]\nclean_after = [\"worktree\", \"branch\", \"review_worktrees\"]\n")
        .unwrap();
    assert_eq!(
        c.merge.clean_after,
        [CleanKind::Worktree, CleanKind::Branch, CleanKind::ReviewWorktrees]
    );
    let c = Config::from_toml("[merge]\nclean_after = [\"branch\"]\n").unwrap();
    assert_eq!(c.merge.clean_after, [CleanKind::Branch]);
    let c = Config::from_toml("[merge]\nclean_after = []\n").unwrap();
    assert!(c.merge.clean_after.is_empty(), "empty list: today's behaviour");
}

#[test]
fn clean_after_refuses_an_unknown_kind() {
    for bad in ["[\"remote_branch\"]", "[\"Worktree\"]", "\"worktree\"", "[1]"] {
        let e = Config::from_toml(&format!("[merge]\nclean_after = {bad}\n")).unwrap_err();
        assert!(matches!(e, ConfigError::Parse(_)), "{bad}: {e}");
    }
}

#[test]
fn the_template_documents_clean_after_and_still_parses_to_the_defaults() {
    let t = Config::default_toml();
    assert!(
        t.contains("clean_after = [\"worktree\", \"review_worktrees\"]"),
        "the default is spelled out: {t}"
    );
    for kind in ["worktree", "branch", "review_worktrees"] {
        assert!(t.contains(kind), "{kind} not documented");
    }
    assert_eq!(
        Config::from_toml(t).unwrap().merge.clean_after,
        Config::default().merge.clean_after
    );
}
