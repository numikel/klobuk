use asf_core::config::GateWhen;
use asf_core::config_edit::{editable_keys, set_key};
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
fn editable_keys_are_exactly_the_ten_named_in_the_brief() {
    let keys: Vec<&str> = editable_keys().iter().map(|(k, _)| *k).collect();
    assert_eq!(
        keys,
        [
            "planner.model",
            "planner.budget_usd",
            "worker.model",
            "worker.budget_usd",
            "review.required",
            "review.model",
            "review.max_rounds",
            "scheduler.max_parallel",
            "merge.target",
            "merge.verify_main_after",
        ]
    );
    assert!(editable_keys().iter().all(|(_, d)| !d.trim().is_empty()));
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
