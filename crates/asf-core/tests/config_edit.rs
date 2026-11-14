use asf_core::config::{CleanKind, GateWhen};
use asf_core::config_edit::{
    applies, apply_note, editable_keys, fields, lookup, set_key, unset_key, Applies, ConfigGroup, FieldKind,
};
use asf_core::{Config, ConfigError};

const COMMENTED: &str = "\
# top comment
[repo]
prefix = \"asf\" # id prefix
default_branch = \"main\"

# scheduling knobs
[scheduler]
max_parallel = 3 # concurrent workers
hotspots = []

[merge]
target = \"local\"
# trailing note in merge
";

#[test]
fn set_existing_key_keeps_comments_and_order() {
    let out = set_key(COMMENTED, "scheduler.max_parallel", "5").unwrap();
    let before: Vec<&str> = COMMENTED.lines().collect();
    let after: Vec<&str> = out.lines().collect();
    assert_eq!(before.len(), after.len(), "no line added or removed:\n{out}");
    for (b, a) in before.iter().zip(&after) {
        if b.starts_with("max_parallel") {
            assert_eq!(*a, "max_parallel = 5 # concurrent workers");
        } else {
            assert_eq!(b, a, "unrelated line changed");
        }
    }
    assert_eq!(Config::from_toml(&out).unwrap().scheduler.max_parallel, 5);
}

#[test]
fn set_key_on_default_template_changes_only_that_line() {
    let t = Config::default_toml();
    let out = set_key(t, "merge.verify_main_after", "true").unwrap();
    let changed: Vec<(&str, &str)> = t.lines().zip(out.lines()).filter(|(b, a)| b != a).collect();
    assert_eq!(changed.len(), 1, "{changed:?}");
    assert!(changed[0].1.starts_with("verify_main_after = true"));
    assert!(changed[0].1.contains("# run the gates"), "inline comment lost");
    assert!(Config::from_toml(&out).unwrap().merge.verify_main_after);
}

#[test]
fn invalid_value_is_refused_and_input_unchanged() {
    let input = COMMENTED.to_string();
    let e = set_key(&input, "scheduler.max_parallel", "0").unwrap_err();
    assert!(matches!(e, ConfigError::Invalid(_)), "{e}");
    assert_eq!(input, COMMENTED, "caller's text must not change");
}

#[test]
fn wrong_type_is_refused() {
    let e = set_key(COMMENTED, "scheduler.max_parallel", "many").unwrap_err();
    assert!(
        matches!(e, ConfigError::Parse(_) | ConfigError::Invalid(_)),
        "{e}"
    );
}

#[test]
fn missing_section_is_created() {
    let input = "[repo]\nprefix = \"asf\"\n";
    let out = set_key(input, "review.model", "sonnet").unwrap();
    assert!(out.contains("[review]"), "{out}");
    assert!(out.starts_with(input), "existing content must stay first:\n{out}");
    let c = Config::from_toml(&out).unwrap();
    assert_eq!(c.review.model, "sonnet");
    assert_eq!(c.review.max_rounds, 2, "other review defaults survive");
}

#[test]
fn missing_section_created_in_empty_document() {
    let out = set_key("", "planner.budget_usd", "2.5").unwrap();
    assert!(out.contains("[planner]"), "{out}");
    assert_eq!(Config::from_toml(&out).unwrap().planner.budget_usd, 2.5);
}

#[test]
fn key_in_header_less_parent_table_is_added_under_it() {
    // `[merge.github]` exists but `[merge]` does not.
    let input = "[merge.github]\nmethod = \"squash\"\n";
    let out = set_key(input, "merge.target", "github").unwrap();
    let c = Config::from_toml(&out).unwrap();
    assert_eq!(c.merge.target, "github");
    assert_eq!(c.merge.github.method, "squash", "sibling table survives");
}

#[test]
fn value_falls_back_to_plain_string() {
    let out = set_key("", "planner.model", "claude-haiku-5-5").unwrap();
    assert_eq!(Config::from_toml(&out).unwrap().planner.model, "claude-haiku-5-5");
    let out = set_key("", "worker.model", "\"sonnet\"").unwrap();
    assert_eq!(
        Config::from_toml(&out).unwrap().worker.model.as_deref(),
        Some("sonnet"),
        "an explicitly quoted string is taken as TOML"
    );
}

#[test]
fn scalars_are_typed() {
    let out = set_key("", "review.required", "true").unwrap();
    assert!(Config::from_toml(&out).unwrap().review.required);
    let out = set_key("", "worker.budget_usd", "7").unwrap();
    assert_eq!(Config::from_toml(&out).unwrap().worker.budget_usd, 7.0);
    let out = set_key("", "review.max_rounds", "4").unwrap();
    assert_eq!(Config::from_toml(&out).unwrap().review.max_rounds, 4);
}

#[test]
fn value_with_trailing_text_is_a_string_never_a_comment() {
    // Not a TOML value, so it is taken as a plain string: the wrong type for a number...
    assert!(set_key(COMMENTED, "scheduler.max_parallel", "7 # sneaky").is_err());
    // ...and for a string key it is quoted, so it cannot comment out anything.
    let out = set_key(COMMENTED, "merge.target", "github # sneaky").unwrap_err();
    assert!(matches!(out, ConfigError::Invalid(_)), "{out}");
    let out = set_key("", "planner.model", "haiku # x").unwrap();
    assert!(out.contains("\"haiku # x\""), "{out}");
    assert_eq!(Config::from_toml(&out).unwrap().planner.model, "haiku # x");
}

#[test]
fn unknown_or_non_editable_key_is_refused() {
    for key in ["nope.key", "repo.prefix", "planer.model", "review", "", "a.b.c"] {
        let e = set_key(COMMENTED, key, "x").unwrap_err();
        assert!(matches!(e, ConfigError::Invalid(_)), "{key}: {e}");
    }
}

#[test]
fn unparsable_document_is_a_parse_error() {
    let e = set_key("[repo\n", "review.model", "opus").unwrap_err();
    assert!(matches!(e, ConfigError::Parse(_)), "{e}");
}

#[test]
fn editable_keys_are_exactly_the_twenty_one_named_in_the_briefs() {
    // In the order of the Config view: models, budgets, flow, notifications.
    let keys: Vec<&str> = editable_keys().iter().map(|(k, _)| *k).collect();
    assert_eq!(
        keys,
        [
            "planner.model",
            "worker.model",
            "review.model",
            "planner.budget_usd",
            "worker.budget_usd",
            "scheduler.max_parallel",
            "review.required",
            "review.max_rounds",
            "gate.max_rounds",
            "merge.target",
            "merge.verify_main_after",
            "merge.reuse_verification",
            "merge.clean_after",
            "worker.remote_control",
            "serve.interval_s",
            "serve.max_ticks",
            "gate.slow_tests",
            "gate.slow_after_s",
            "notify.on_new",
            "notify.after_minutes",
            "notify.every_minutes",
        ]
    );
    assert!(editable_keys().iter().all(|(_, d)| !d.trim().is_empty()));
}

#[test]
fn fields_describe_every_editable_key_in_the_same_order() {
    let described: Vec<&str> = fields().iter().map(|f| f.key).collect();
    let keys: Vec<&str> = editable_keys().iter().map(|(k, _)| *k).collect();
    assert_eq!(described, keys);
    for f in fields() {
        assert!(!f.label.trim().is_empty(), "{}: label", f.key);
        assert!(!f.help.trim().is_empty(), "{}: help", f.key);
    }
    let labels: std::collections::HashSet<_> = fields().iter().map(|f| f.label).collect();
    assert_eq!(labels.len(), fields().len(), "labels are unique");
}

#[test]
fn fields_carry_the_labels_and_groups_of_the_view() {
    let get = |key: &str| fields().iter().find(|f| f.key == key).unwrap();
    for (key, label, group) in [
        ("planner.model", "Planning", ConfigGroup::Models),
        ("worker.model", "Workers", ConfigGroup::Models),
        ("review.model", "Reviewer", ConfigGroup::Models),
        ("planner.budget_usd", "Planning step", ConfigGroup::Budgets),
        ("worker.budget_usd", "Per issue", ConfigGroup::Budgets),
        ("scheduler.max_parallel", "Workers at once", ConfigGroup::Flow),
        ("review.required", "Review before merge", ConfigGroup::Flow),
        ("review.max_rounds", "Review rounds", ConfigGroup::Flow),
        ("gate.max_rounds", "Gate fix rounds", ConfigGroup::Flow),
        ("merge.target", "Merge target", ConfigGroup::Flow),
        ("merge.verify_main_after", "Gates on main", ConfigGroup::Flow),
        (
            "merge.reuse_verification",
            "Reuse verification",
            ConfigGroup::Flow,
        ),
        ("merge.clean_after", "Clean up after merge", ConfigGroup::Flow),
        ("worker.remote_control", "Remote control", ConfigGroup::Flow),
        ("serve.interval_s", "Loop interval (s)", ConfigGroup::Flow),
        ("serve.max_ticks", "Loop tick limit", ConfigGroup::Flow),
        ("gate.slow_tests", "Slow tests to stop", ConfigGroup::Flow),
        ("gate.slow_after_s", "Slow test wait (s)", ConfigGroup::Flow),
    ] {
        assert_eq!((get(key).label, get(key).group), (label, group), "{key}");
    }
    // the keys the mockup does not show still get a group of their own
    for key in ["notify.on_new", "notify.after_minutes", "notify.every_minutes"] {
        assert_eq!(get(key).group, ConfigGroup::Notifications, "{key}");
    }
    assert_eq!(
        [
            ConfigGroup::Models.title(),
            ConfigGroup::Budgets.title(),
            ConfigGroup::Flow.title(),
            ConfigGroup::Notifications.title(),
        ],
        ["Models", "Budgets", "Flow", "Notifications"]
    );
}

#[test]
fn field_kinds_are_choice_bool_number_or_text() {
    let get = |key: &str| fields().iter().find(|f| f.key == key).unwrap();
    for key in ["planner.model", "worker.model", "review.model"] {
        assert_eq!(
            get(key).kind,
            FieldKind::Choice(&["opus", "sonnet", "haiku"]),
            "{key}"
        );
    }
    assert_eq!(get("merge.target").kind, FieldKind::Choice(&["local", "github"]));
    assert_eq!(
        get("merge.clean_after").kind,
        FieldKind::Text,
        "a list is typed with Enter"
    );
    for key in [
        "review.required",
        "merge.verify_main_after",
        "merge.reuse_verification",
        "worker.remote_control",
        "notify.on_new",
    ] {
        assert_eq!(get(key).kind, FieldKind::Bool, "{key}");
    }
    for key in [
        "planner.budget_usd",
        "worker.budget_usd",
        "scheduler.max_parallel",
        "review.max_rounds",
        "gate.max_rounds",
        "serve.interval_s",
        "serve.max_ticks",
        "gate.slow_tests",
        "gate.slow_after_s",
        "notify.after_minutes",
        "notify.every_minutes",
    ] {
        assert_eq!(get(key).kind, FieldKind::Number, "{key}");
    }
}

#[test]
fn field_defaults_are_read_from_config_default() {
    let get = |key: &str| fields().iter().find(|f| f.key == key).unwrap();
    assert_eq!(get("planner.budget_usd").default_value().as_deref(), Some("1.0"));
    assert_eq!(get("worker.budget_usd").default_value().as_deref(), Some("5.0"));
    assert_eq!(
        get("scheduler.max_parallel").default_value().as_deref(),
        Some("3")
    );
    assert_eq!(get("review.required").default_value().as_deref(), Some("false"));
    assert_eq!(get("review.model").default_value().as_deref(), Some("opus"));
    assert_eq!(get("merge.target").default_value().as_deref(), Some("local"));
    assert_eq!(
        get("merge.clean_after").default_value().as_deref(),
        Some("[\"worktree\",\"review_worktrees\"]")
    );
    assert_eq!(get("notify.on_new").default_value().as_deref(), Some("true"));
    assert_eq!(
        get("merge.reuse_verification").default_value().as_deref(),
        Some("true")
    );
    assert_eq!(
        get("merge.verify_main_after").default_value().as_deref(),
        Some("false")
    );
    assert_eq!(
        get("worker.remote_control").default_value().as_deref(),
        Some("false")
    );
    assert_eq!(get("serve.interval_s").default_value().as_deref(), Some("10"));
    assert_eq!(get("gate.max_rounds").default_value().as_deref(), Some("2"));
    // an optional key that is absent by default has no default value to write
    assert_eq!(get("worker.model").default_value(), None);
    assert_eq!(get("serve.max_ticks").default_value(), None);
    assert_eq!(get("gate.slow_tests").default_value(), None);
    assert_eq!(get("gate.slow_after_s").default_value(), None);
}

#[test]
fn every_field_default_and_every_choice_is_accepted_by_set_key() {
    for f in fields() {
        if let Some(default) = f.default_value() {
            set_key("", f.key, &default).unwrap_or_else(|e| panic!("{}: default {default}: {e}", f.key));
        }
        if let FieldKind::Choice(choices) = f.kind {
            for c in choices {
                set_key("", f.key, c).unwrap_or_else(|e| panic!("{}: {c}: {e}", f.key));
            }
        }
    }
}

#[test]
fn every_editable_key_accepts_its_default_value() {
    // Guards against a key whose path drifted from the Config struct.
    for (key, sample) in [
        ("planner.model", "claude-haiku-5-5"),
        ("planner.budget_usd", "1.0"),
        ("worker.model", "sonnet"),
        ("worker.budget_usd", "5.0"),
        ("review.required", "false"),
        ("review.model", "opus"),
        ("review.max_rounds", "2"),
        ("scheduler.max_parallel", "3"),
        ("merge.target", "local"),
        ("merge.verify_main_after", "false"),
        ("merge.reuse_verification", "true"),
        ("merge.clean_after", "[\"worktree\", \"review_worktrees\"]"),
        ("worker.remote_control", "false"),
        ("notify.on_new", "true"),
        ("notify.after_minutes", "15"),
        ("notify.every_minutes", "30"),
    ] {
        set_key("", key, sample).unwrap_or_else(|e| panic!("{key}: {e}"));
    }
}

#[test]
fn review_defaults_and_overrides() {
    let c = Config::from_toml("").unwrap();
    assert_eq!(c.review.model, "opus");
    assert_eq!(c.review.max_rounds, 2);
    let c = Config::from_toml("[review]\nmodel = \"sonnet\"\nmax_rounds = 3\n").unwrap();
    assert_eq!(c.review.model, "sonnet");
    assert_eq!(c.review.max_rounds, 3);
    assert!(!c.review.required, "other review fields keep their defaults");
}

#[test]
fn gate_when_parses_and_defaults_to_all_stages() {
    let c = Config::from_toml(
        "[[gate]]\nname = \"fmt\"\ncmd = [\"cargo\", \"fmt\"]\nwhen = [\"verify\"]\n\
         [[gate]]\nname = \"test\"\ncmd = [\"cargo\", \"test\"]\n",
    )
    .unwrap();
    assert_eq!(c.gates[0].when, vec![GateWhen::Verify]);
    assert_eq!(
        c.gates[1].when,
        vec![GateWhen::Verify, GateWhen::Merge, GateWhen::Main]
    );
}

#[test]
fn gate_when_rejects_unknown_stage_and_empty_list() {
    let base = "[[gate]]\nname = \"x\"\ncmd = [\"true\"]\n";
    assert!(Config::from_toml(&format!("{base}when = [\"later\"]\n")).is_err());
    let e = Config::from_toml(&format!("{base}when = []\n")).unwrap_err();
    assert!(matches!(e, ConfigError::Invalid(_)), "{e}");
}

#[test]
fn editing_preserves_gate_tables() {
    let input = "[[gate]]\nname = \"fmt\"\ncmd = [\"cargo\", \"fmt\"]\nwhen = [\"verify\"]\n";
    let out = set_key(input, "review.required", "true").unwrap();
    assert!(out.starts_with(input), "{out}");
    let c = Config::from_toml(&out).unwrap();
    assert_eq!(c.gates[0].when, vec![GateWhen::Verify]);
    assert!(c.review.required);
}

// ---------- asf-30: unset_key ----------

const WITH_MODELS: &str = "\
# top comment
[worker]
budget_usd = 5.0
# which model the workers run on
model = \"sonnet\" # chosen in round 3
permission_mode = \"acceptEdits\"

# reviewer
[review]
required = true
model = \"sonnet\"
max_rounds = 2 # rounds
";

#[test]
fn unset_removes_only_that_key_and_keeps_other_comments() {
    let out = unset_key(WITH_MODELS, "worker.model").unwrap();
    assert!(!out.contains("model = \"sonnet\" # chosen"), "{out}");
    let kept: Vec<&str> = WITH_MODELS
        .lines()
        .filter(|l| !l.starts_with("model = \"sonnet\" # chosen") && *l != "# which model the workers run on")
        .collect();
    let after: Vec<&str> = out.lines().collect();
    assert_eq!(
        kept, after,
        "only the key line (and its own leading comment) may go:\n{out}"
    );
    assert!(out.contains("# top comment") && out.contains("# reviewer") && out.contains("# rounds"));
    assert_eq!(Config::from_toml(&out).unwrap().worker.model, None);
}

#[test]
fn unset_review_model_falls_back_to_the_default() {
    let out = unset_key(WITH_MODELS, "review.model").unwrap();
    let c = Config::from_toml(&out).unwrap();
    assert_eq!(c.review.model, "opus");
    assert!(c.review.required, "sibling keys survive");
    assert_eq!(c.worker.model.as_deref(), Some("sonnet"));
}

#[test]
fn unset_of_an_absent_key_or_section_is_a_noop() {
    for (text, key) in [
        ("", "worker.model"),
        ("[repo]\nprefix = \"asf\"\n", "review.model"),
        (Config::default_toml(), "worker.model"),
    ] {
        let out = unset_key(text, key).unwrap();
        assert_eq!(out, text, "{key}");
        Config::from_toml(&out).unwrap();
    }
}

#[test]
fn unset_refuses_required_unknown_and_malformed() {
    for key in [
        "scheduler.max_parallel",
        "merge.target",
        "planner.model",
        "worker.budget_usd",
        "review.required",
        "repo.prefix",
        "nope.key",
        "worker",
        "",
    ] {
        let e = unset_key(WITH_MODELS, key).unwrap_err();
        assert!(matches!(e, ConfigError::Invalid(_)), "{key}: {e}");
    }
    let e = unset_key("[worker\n", "worker.model").unwrap_err();
    assert!(matches!(e, ConfigError::Parse(_)), "{e}");
}

#[test]
fn unset_output_always_parses() {
    for key in ["worker.model", "review.model"] {
        let out = unset_key(WITH_MODELS, key).unwrap();
        Config::from_toml(&out).unwrap();
        // and set_key can bring it back
        let back = set_key(&out, key, "haiku").unwrap();
        Config::from_toml(&back).unwrap();
    }
}

#[test]
fn a_key_the_serve_loop_reloads_says_it_applies_on_the_next_tick() {
    // each key is read from `ctx.config`, which `asf serve` replaces at the start of every tick
    for key in [
        "worker.model",
        "worker.budget_usd",
        "review.model",
        "review.required",
        "review.max_rounds",
        "gate.max_rounds",
        "scheduler.max_parallel",
        "merge.target",
        "merge.verify_main_after",
        "merge.reuse_verification",
        "merge.clean_after",
        "worker.remote_control",
        "gate.slow_tests",
        "gate.slow_after_s",
        "notify.on_new",
        "notify.after_minutes",
        "notify.every_minutes",
    ] {
        assert_eq!(applies(key), Applies::NextTick, "{key}");
        assert_eq!(
            apply_note(key),
            "a running `asf serve` picks this up on its next tick",
            "{key}"
        );
    }
}

#[test]
fn the_two_gate_run_keys_say_what_they_cost_and_when_they_save_it() {
    let describe = |key: &str| editable_keys().iter().find(|(k, _)| *k == key).unwrap().1;
    let main = describe("merge.verify_main_after");
    assert!(main.contains("one more full gate run per merge"), "{main}");
    assert!(main.contains("redundant after a fast-forward"), "{main}");
    let reuse = describe("merge.reuse_verification");
    assert!(reuse.contains("one more full gate run"), "{reuse}");
    assert!(reuse.contains("rebase changed nothing"), "{reuse}");
}

#[test]
fn reuse_verification_is_set_and_read_back_like_any_flow_toggle() {
    let out = set_key(
        "[merge]\nreuse_verification = true\n",
        "merge.reuse_verification",
        "false",
    )
    .unwrap();
    assert!(!Config::from_toml(&out).unwrap().merge.reuse_verification);
    let from_nothing = set_key("", "merge.reuse_verification", "false").unwrap();
    assert!(!Config::from_toml(&from_nothing).unwrap().merge.reuse_verification);
}

#[test]
fn a_key_only_asf_plan_reads_promises_nothing_of_serve() {
    for key in ["planner.model", "planner.budget_usd"] {
        assert_eq!(applies(key), Applies::NextPlan, "{key}");
        let note = apply_note(key);
        assert_eq!(
            note, "the next `asf plan` uses this; `asf serve` never does",
            "{key}"
        );
        assert!(!note.contains("restart"), "{note}");
    }
}

#[test]
fn a_key_nobody_classified_gets_the_restart_hint() {
    assert_eq!(applies("repo.prefix"), Applies::Restart);
    assert_eq!(apply_note("repo.prefix"), "restart `asf serve` to apply");
    assert_eq!(apply_note("nope"), "restart `asf serve` to apply");
}

#[test]
fn every_editable_key_has_a_note() {
    for (key, _) in editable_keys() {
        assert!(!apply_note(key).is_empty(), "{key}");
    }
}

#[test]
fn clean_after_is_set_as_a_list_keeping_the_comment_on_the_line() {
    let t = Config::default_toml();
    let out = set_key(t, "merge.clean_after", "[\"worktree\", \"branch\"]").unwrap();
    let changed: Vec<(&str, &str)> = t.lines().zip(out.lines()).filter(|(b, a)| b != a).collect();
    assert_eq!(changed.len(), 1, "{changed:?}");
    assert!(
        changed[0]
            .1
            .starts_with("clean_after = [\"worktree\", \"branch\"]"),
        "{changed:?}"
    );
    assert!(changed[0].1.contains('#'), "inline comment lost: {changed:?}");
    assert_eq!(
        Config::from_toml(&out).unwrap().merge.clean_after,
        [CleanKind::Worktree, CleanKind::Branch]
    );
    let none = set_key(t, "merge.clean_after", "[]").unwrap();
    assert!(Config::from_toml(&none).unwrap().merge.clean_after.is_empty());
}

#[test]
fn clean_after_refuses_a_kind_that_does_not_exist_and_a_bare_word() {
    for bad in ["[\"remote_branch\"]", "worktree", "[\"branch\", 3]"] {
        let e = set_key(Config::default_toml(), "merge.clean_after", bad).unwrap_err();
        assert!(matches!(e, ConfigError::Parse(_)), "{bad}: {e}");
    }
}

#[test]
fn serve_keys_set_validate_and_the_tick_limit_can_be_unset() {
    let out = set_key("", "serve.interval_s", "5").unwrap();
    assert_eq!(Config::from_toml(&out).unwrap().serve.interval_s, 5);
    assert!(
        set_key("", "serve.interval_s", "0").is_err(),
        "an interval of 0 is refused"
    );
    let out = set_key("", "serve.max_ticks", "600").unwrap();
    assert_eq!(Config::from_toml(&out).unwrap().serve.max_ticks, Some(600));
    assert!(set_key("", "serve.max_ticks", "0").is_err());
    let back = asf_core::config_edit::unset_key(&out, "serve.max_ticks").unwrap();
    assert_eq!(Config::from_toml(&back).unwrap().serve.max_ticks, None);
    assert!(asf_core::config_edit::unset_key(&out, "serve.interval_s").is_err());
    assert_eq!(
        asf_core::config_edit::applies("serve.interval_s"),
        asf_core::config_edit::Applies::NextTick,
        "the loop takes [serve] from the refreshed config every tick"
    );
}

const WITH_GATES: &str = "\
[[gate]]
name = \"fmt\"
cmd = [\"cargo\", \"fmt\"]

# the slow one
[[gate]]
name = \"test\"
cmd = [\"cargo\", \"test\"]
timeout_s = 1200 # twenty minutes
";

#[test]
fn gate_keys_land_in_the_gate_named_test_and_nowhere_else() {
    let out = set_key(WITH_GATES, "gate.slow_tests", "3").unwrap();
    let out = set_key(&out, "gate.slow_after_s", "300").unwrap();
    let gates = Config::from_toml(&out).unwrap().gates;
    assert_eq!(
        (gates[0].slow_tests, gates[0].slow_after_s),
        (None, None),
        "{out}"
    );
    assert_eq!(
        (gates[1].slow_tests, gates[1].slow_after_s),
        (Some(3), Some(300)),
        "{out}"
    );
    assert!(
        out.contains("# the slow one") && out.contains("# twenty minutes"),
        "{out}"
    );
    assert_eq!(gates.len(), 2, "no new [[gate]] table: {out}");
    // a second set replaces the value in place
    let again = set_key(&out, "gate.slow_tests", "5").unwrap();
    assert_eq!(Config::from_toml(&again).unwrap().gates[1].slow_tests, Some(5));
    assert_eq!(again.matches("slow_tests").count(), 1, "{again}");
}

#[test]
fn gate_keys_need_a_gate_named_test_and_a_positive_number() {
    let no_gate = set_key("", "gate.slow_tests", "3").unwrap_err();
    assert!(matches!(no_gate, ConfigError::Invalid(_)), "{no_gate}");
    assert!(no_gate.to_string().contains("[[gate]]"), "{no_gate}");
    let only_fmt = "[[gate]]\nname = \"fmt\"\ncmd = [\"cargo\", \"fmt\"]\n";
    assert!(set_key(only_fmt, "gate.slow_after_s", "60").is_err());
    for bad in ["0", "-1", "soon", "1.5"] {
        assert!(set_key(WITH_GATES, "gate.slow_tests", bad).is_err(), "{bad}");
        assert!(set_key(WITH_GATES, "gate.slow_after_s", bad).is_err(), "{bad}");
    }
}

#[test]
fn gate_keys_can_be_unset_and_unsetting_an_absent_one_is_a_noop() {
    let set = set_key(WITH_GATES, "gate.slow_tests", "3").unwrap();
    let back = unset_key(&set, "gate.slow_tests").unwrap();
    assert_eq!(Config::from_toml(&back).unwrap().gates[1].slow_tests, None);
    assert_eq!(back, WITH_GATES, "the edit is undone byte for byte");
    assert_eq!(unset_key(WITH_GATES, "gate.slow_after_s").unwrap(), WITH_GATES);
    assert_eq!(unset_key("", "gate.slow_tests").unwrap(), "", "no gate, no key");
}

#[test]
fn lookup_reads_gate_keys_from_the_gate_named_test() {
    let config = Config::from_toml(&set_key(WITH_GATES, "gate.slow_tests", "3").unwrap()).unwrap();
    let root = serde_json::to_value(&config).unwrap();
    assert_eq!(lookup(&root, "gate.slow_tests"), Some(&serde_json::json!(3)));
    assert_eq!(lookup(&root, "gate.slow_after_s"), Some(&serde_json::Value::Null));
    assert_eq!(lookup(&root, "serve.interval_s"), Some(&serde_json::json!(10)));
    assert_eq!(lookup(&root, "nope.nothing"), None);
    let none = serde_json::to_value(Config::default()).unwrap();
    assert_eq!(lookup(&none, "gate.slow_tests"), None, "no gate named test");
}

#[test]
fn gate_max_rounds_is_a_merge_setting_not_a_field_of_a_gate() {
    // `gate` is an array of tables, so the key cannot live in `[gate]`: it is stored in `[merge]`
    // and needs no `[[gate]]` at all
    let out = set_key("", "gate.max_rounds", "3").unwrap();
    assert_eq!(Config::from_toml(&out).unwrap().merge.gate_max_rounds, 3, "{out}");
    assert!(out.contains("gate_max_rounds = 3"), "{out}");
    // with gates in the file none of them is touched, and a second set replaces the value in place
    let out = set_key(WITH_GATES, "gate.max_rounds", "1").unwrap();
    let again = set_key(&out, "gate.max_rounds", "0").unwrap();
    let config = Config::from_toml(&again).unwrap();
    assert_eq!(config.merge.gate_max_rounds, 0, "0 never sends back: {again}");
    assert_eq!(config.gates.len(), 2, "{again}");
    assert_eq!(again.matches("gate_max_rounds").count(), 1, "{again}");
    assert!(again.contains("# the slow one"), "{again}");
    let root = serde_json::to_value(&config).unwrap();
    assert_eq!(lookup(&root, "gate.max_rounds"), Some(&serde_json::json!(0)));
    let defaults = serde_json::to_value(Config::default()).unwrap();
    assert_eq!(lookup(&defaults, "gate.max_rounds"), Some(&serde_json::json!(2)));
    for bad in ["-1", "many", "1.5"] {
        assert!(set_key("", "gate.max_rounds", bad).is_err(), "{bad}");
    }
    assert!(unset_key(&out, "gate.max_rounds").is_err(), "a required key");
}
