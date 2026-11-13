mod common;

use common::Repo;
use serde_json::Value;

const NEXT_TICK: &str = "a running `asf serve` picks this up on its next tick";
const PLAN_ONLY: &str = "the next `asf plan` uses this; `asf serve` never does";

fn config_path(r: &Repo) -> std::path::PathBuf {
    r.path().join(".factory/config.toml")
}

fn config_bytes(r: &Repo) -> Vec<u8> {
    std::fs::read(config_path(r)).unwrap()
}

fn entry<'a>(rows: &'a [Value], key: &str) -> &'a Value {
    rows.iter()
        .find(|row| row["key"] == key)
        .unwrap_or_else(|| panic!("no {key} in {rows:?}"))
}

#[test]
fn show_lists_the_twenty_editable_keys_with_current_values() {
    let r = Repo::initialised();
    let res = r.ok(&["config", "show"]);
    let rows = res.as_array().unwrap_or_else(|| panic!("{res}"));
    assert_eq!(rows.len(), 20, "{res}");
    assert_eq!(entry(rows, "gate.max_rounds")["value"], 2);
    assert_eq!(entry(rows, "gate.slow_tests")["value"], Value::Null);
    assert_eq!(entry(rows, "gate.slow_after_s")["value"], Value::Null);
    assert_eq!(entry(rows, "serve.interval_s")["value"], 10);
    assert_eq!(entry(rows, "serve.max_ticks")["value"], Value::Null);
    assert_eq!(
        entry(rows, "merge.clean_after")["value"],
        serde_json::json!(["worktree", "review_worktrees"]),
        "{res}"
    );
    assert_eq!(entry(rows, "worker.remote_control")["value"], false);
    assert_eq!(entry(rows, "notify.on_new")["value"], true);
    assert_eq!(entry(rows, "notify.after_minutes")["value"], 15);
    assert_eq!(entry(rows, "notify.every_minutes")["value"], 30);

    assert_eq!(entry(rows, "scheduler.max_parallel")["value"], 3);
    assert_eq!(entry(rows, "review.required")["value"], false);
    assert_eq!(entry(rows, "review.model")["value"], "opus");
    assert_eq!(entry(rows, "merge.target")["value"], "local");
    assert_eq!(entry(rows, "worker.budget_usd")["value"], 5.0);
    assert_eq!(entry(rows, "worker.model")["value"], Value::Null);
    for row in rows {
        assert!(
            !row["description"].as_str().unwrap_or_default().is_empty(),
            "{row}"
        );
    }

    let human = r.asf().args(["config", "show"]).output().unwrap();
    assert_eq!(human.status.code(), Some(0));
    let text = String::from_utf8_lossy(&human.stdout);
    for key in ["planner.model", "review.max_rounds", "merge.verify_main_after"] {
        assert!(text.contains(key), "{key} missing in {text}");
    }
}

#[test]
fn set_changes_a_value_and_keeps_comments() {
    let r = Repo::initialised();
    let before = String::from_utf8(config_bytes(&r)).unwrap();
    assert!(before.contains("max_parallel = 3"), "{before}");

    let res = r.ok(&["config", "set", "scheduler.max_parallel", "5"]);
    assert_eq!(res["key"], "scheduler.max_parallel", "{res}");
    assert_eq!(res["old"], 3, "{res}");
    assert_eq!(res["new"], 5, "{res}");

    let after = String::from_utf8(config_bytes(&r)).unwrap();
    assert!(after.contains("max_parallel = 5"), "{after}");
    assert!(after.contains("# concurrent workers"), "{after}");
    assert!(
        after.starts_with("# auto-software-factory configuration"),
        "{after}"
    );
    assert_eq!(
        after.replace("max_parallel = 5", "max_parallel = 3"),
        before,
        "only the value may differ"
    );
}

#[test]
fn set_prints_old_and_new_values_and_says_a_running_serve_picks_them_up() {
    let r = Repo::initialised();
    let out = r
        .asf()
        .args(["config", "set", "merge.target", "github"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("merge.target"), "{text}");
    assert!(text.contains("local") && text.contains("github"), "{text}");
    assert!(text.contains(NEXT_TICK), "{text}");
    assert!(
        !text.contains("restart"),
        "a reloaded key needs no restart: {text}"
    );

    let res = r.ok(&["config", "set", "merge.target", "local"]);
    assert_eq!(
        (res["old"].as_str(), res["new"].as_str()),
        (Some("github"), Some("local"))
    );
    assert_eq!(res["note"], NEXT_TICK, "{res}");
}

#[test]
fn every_key_a_serve_tick_reloads_says_so_and_never_hints_at_a_restart() {
    let r = Repo::initialised();
    for (key, value) in [
        ("worker.model", "sonnet"),
        ("worker.budget_usd", "4"),
        ("review.model", "haiku"),
        ("review.required", "true"),
        ("review.max_rounds", "3"),
        ("scheduler.max_parallel", "2"),
        ("merge.verify_main_after", "true"),
        ("merge.clean_after", "[\"worktree\", \"branch\"]"),
        ("worker.remote_control", "true"),
        ("notify.on_new", "false"),
        ("notify.after_minutes", "10"),
        ("notify.every_minutes", "0"),
    ] {
        let res = r.ok(&["config", "set", key, value]);
        assert_eq!(res["note"], NEXT_TICK, "{key}: {res}");
    }
}

#[test]
fn a_planner_key_says_only_asf_plan_reads_it() {
    let r = Repo::initialised();
    let res = r.ok(&["config", "set", "planner.budget_usd", "2"]);
    assert_eq!(res["note"], PLAN_ONLY, "{res}");
    let out = r
        .asf()
        .args(["config", "set", "planner.model", "sonnet"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains(PLAN_ONLY), "{text}");
    assert!(!text.contains("restart"), "{text}");
}

#[test]
fn set_writes_utf8_without_bom_and_with_lf_endings() {
    let r = Repo::initialised();
    let crlf = format!(
        "\u{feff}{}",
        String::from_utf8(config_bytes(&r)).unwrap().replace('\n', "\r\n")
    );
    std::fs::write(config_path(&r), crlf).unwrap();

    r.ok(&["config", "set", "review.required", "true"]);

    let bytes = config_bytes(&r);
    assert!(!bytes.starts_with(&[0xEF, 0xBB, 0xBF]), "BOM written");
    assert!(!bytes.contains(&b'\r'), "CR written");
    assert!(String::from_utf8(bytes).unwrap().contains("required = true"));
}

#[test]
fn set_adds_a_value_for_a_key_that_is_not_in_the_file() {
    let r = Repo::initialised();
    let res = r.ok(&["config", "set", "worker.model", "sonnet"]);
    assert_eq!(res["old"], Value::Null, "{res}");
    assert_eq!(res["new"], "sonnet", "{res}");
    let shown = r.ok(&["config", "show"]);
    assert_eq!(
        entry(shown.as_array().unwrap(), "worker.model")["value"],
        "sonnet"
    );
}

#[test]
fn unknown_key_exits_1_and_leaves_the_file_untouched() {
    let r = Repo::initialised();
    let before = config_bytes(&r);
    let (code, ecode, msg) = r.err(&["config", "set", "repo.prefix", "zzz"]);
    assert_eq!((code, ecode.as_str()), (1, "validation"), "{msg}");
    assert!(msg.contains("repo.prefix"), "{msg}");
    assert_eq!(config_bytes(&r), before);

    let out = r
        .asf()
        .args(["config", "set", "nope.nothing", "1"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("error:"));
    assert_eq!(config_bytes(&r), before);
}

#[test]
fn invalid_value_exits_1_and_leaves_the_file_untouched() {
    let r = Repo::initialised();
    let before = config_bytes(&r);
    for (key, value) in [
        ("scheduler.max_parallel", "0"),
        ("scheduler.max_parallel", "many"),
        ("review.required", "maybe"),
        ("merge.target", "mars"),
    ] {
        let (code, ecode, msg) = r.err(&["config", "set", key, value]);
        assert_eq!((code, ecode.as_str()), (1, "validation"), "{key}={value}: {msg}");
        assert_eq!(config_bytes(&r), before, "{key}={value} changed the file");
    }
}

#[test]
fn set_without_a_value_is_a_usage_error() {
    let r = Repo::initialised();
    let before = config_bytes(&r);
    let (code, ecode, _) = r.err(&["config", "set", "scheduler.max_parallel"]);
    assert_eq!((code, ecode.as_str()), (2, "usage"));
    assert_eq!(config_bytes(&r), before);
}

#[test]
fn without_a_repository_config_commands_fail_with_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let r = Repo { dir };
    let (code, ecode, _) = r.err(&["config", "show"]);
    assert_eq!((code, ecode.as_str()), (1, "not_found"));
}

#[test]
fn set_is_refused_inside_a_worker_run_but_show_is_allowed() {
    let r = Repo::initialised();
    let before = config_bytes(&r);

    let out = r
        .asf()
        .env("ASF_RUN", "01RUN")
        .args(["config", "set", "scheduler.max_parallel", "9", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], false, "{v}");
    assert_eq!(v["error"]["code"], "forbidden", "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("refused inside a worker run"),
        "{v}"
    );
    assert_eq!(config_bytes(&r), before);

    let shown = r
        .asf()
        .env("ASF_RUN", "01RUN")
        .args(["config", "show", "--json"])
        .output()
        .unwrap();
    assert_eq!(shown.status.code(), Some(0));
}

#[test]
fn set_changes_the_reminder_minutes_and_keeps_the_commented_command() {
    let r = Repo::initialised();
    let res = r.ok(&["config", "set", "notify.after_minutes", "5"]);
    assert_eq!(res["old"], 15, "{res}");
    assert_eq!(res["new"], 5, "{res}");
    r.ok(&["config", "set", "notify.every_minutes", "0"]);

    let after = String::from_utf8(config_bytes(&r)).unwrap();
    assert!(after.contains("after_minutes = 5"), "{after}");
    assert!(after.contains("every_minutes = 0"), "{after}");
    assert!(
        after.contains("# command = [\"pwsh\", \"-File\", \"notify.ps1\"]"),
        "the command stays a comment: {after}"
    );
    let rows = r.ok(&["config", "show"]);
    let rows = rows.as_array().unwrap();
    assert_eq!(entry(rows, "notify.after_minutes")["value"], 5);
    assert_eq!(entry(rows, "notify.every_minutes")["value"], 0);
}

#[test]
fn set_switches_the_immediate_notification_off_without_touching_the_reminders() {
    let r = Repo::initialised();
    let res = r.ok(&["config", "set", "notify.on_new", "false"]);
    assert_eq!(res["old"], true, "{res}");
    assert_eq!(res["new"], false, "{res}");

    let after = String::from_utf8(config_bytes(&r)).unwrap();
    assert!(after.contains("on_new = false"), "{after}");
    assert!(after.contains("after_minutes = 15"), "{after}");
    assert!(
        after.contains("notify at once"),
        "the comment on the line stays: {after}"
    );
    let rows = r.ok(&["config", "show"]);
    let rows = rows.as_array().unwrap();
    assert_eq!(entry(rows, "notify.on_new")["value"], false);
    assert_eq!(entry(rows, "notify.after_minutes")["value"], 15);

    let (code, _, _) = r.err(&["config", "set", "notify.on_new", "maybe"]);
    assert_eq!(code, 1, "only a boolean is accepted");
}

/// `gate.*` keys live in the `[[gate]]` named `test`, an array of tables: `show` and `set` must read
/// the value there (not report `(unset)` for a key that is set) and refuse a config without it.
#[test]
fn gate_keys_are_set_read_and_unset_on_the_gate_named_test() {
    let r = Repo::initialised();
    let (code, ecode, msg) = r.err(&["config", "set", "gate.slow_tests", "3"]);
    assert_eq!((code, ecode.as_str()), (1, "validation"), "{msg}");
    assert!(msg.contains("[[gate]]"), "{msg}");

    let path = config_path(&r);
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str(
        "\n[[gate]]\nname = \"fmt\"\ncmd = [\"true\"]\n\n[[gate]]\nname = \"test\"\ncmd = [\"true\"]\n",
    );
    std::fs::write(&path, text).unwrap();

    let res = r.ok(&["config", "set", "gate.slow_tests", "3"]);
    assert_eq!(
        (res["old"].clone(), res["new"].clone()),
        (Value::Null, serde_json::json!(3)),
        "{res}"
    );
    r.ok(&["config", "set", "gate.slow_after_s", "300"]);
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(
        after.contains("slow_tests = 3") && after.contains("slow_after_s = 300"),
        "{after}"
    );
    let gates = asf_core::Config::from_toml(&after).unwrap().gates;
    assert_eq!(gates[0].slow_tests, None, "the fmt gate is untouched: {after}");
    assert_eq!(
        (gates[1].slow_tests, gates[1].slow_after_s),
        (Some(3), Some(300)),
        "{after}"
    );

    let rows = r.ok(&["config", "show"]);
    let rows = rows.as_array().unwrap();
    assert_eq!(entry(rows, "gate.slow_tests")["value"], 3);
    assert_eq!(entry(rows, "gate.slow_after_s")["value"], 300);

    let (code, _, _) = r.err(&["config", "set", "gate.slow_tests", "0"]);
    assert_eq!(code, 1, "0 is refused");
}

#[test]
fn a_negative_reminder_delay_and_the_command_list_are_refused() {
    let r = Repo::initialised();
    let before = config_bytes(&r);
    for bad in ["soon", "1.5"] {
        let (code, _, _) = r.err(&["config", "set", "notify.after_minutes", bad]);
        assert_eq!(code, 1, "{bad}");
    }
    let (code, _, message) = r.err(&["config", "set", "notify.command", "[\"x\"]"]);
    assert_eq!(code, 1);
    assert!(message.contains("notify.command"), "{message}");
    assert_eq!(config_bytes(&r), before, "a refused edit leaves the file alone");
}

#[test]
fn set_changes_clean_after_as_a_list_and_refuses_an_unknown_kind() {
    let r = Repo::initialised();
    let res = r.ok(&[
        "config",
        "set",
        "merge.clean_after",
        "[\"worktree\", \"branch\", \"review_worktrees\"]",
    ]);
    assert_eq!(
        res["old"],
        serde_json::json!(["worktree", "review_worktrees"]),
        "{res}"
    );
    assert_eq!(
        res["new"],
        serde_json::json!(["worktree", "branch", "review_worktrees"]),
        "{res}"
    );
    let after = String::from_utf8(config_bytes(&r)).unwrap();
    assert!(
        after.contains("clean_after = [\"worktree\", \"branch\", \"review_worktrees\"]"),
        "{after}"
    );
    assert!(
        after.contains("git branch -d"),
        "the comment on the line stays: {after}"
    );

    r.ok(&["config", "set", "merge.clean_after", "[]"]);
    let rows = r.ok(&["config", "show"]);
    assert_eq!(
        entry(rows.as_array().unwrap(), "merge.clean_after")["value"],
        serde_json::json!([])
    );

    let before = config_bytes(&r);
    for bad in ["[\"remote_branch\"]", "worktree"] {
        let (code, ecode, _) = r.err(&["config", "set", "merge.clean_after", bad]);
        assert_eq!((code, ecode.as_str()), (1, "validation"), "{bad}");
        assert_eq!(config_bytes(&r), before, "{bad} changed the file");
    }
}
