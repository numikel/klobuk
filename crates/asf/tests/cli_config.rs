mod common;

use common::Repo;
use serde_json::Value;

const RESTART: &str = "restart `asf serve` to apply";

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
fn show_lists_the_ten_editable_keys_with_current_values() {
    let r = Repo::initialised();
    let res = r.ok(&["config", "show"]);
    let rows = res.as_array().unwrap_or_else(|| panic!("{res}"));
    assert_eq!(rows.len(), 10, "{res}");

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
fn set_prints_old_and_new_values_and_the_restart_hint() {
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
    assert!(text.contains(RESTART), "{text}");

    let res = r.ok(&["config", "set", "merge.target", "local"]);
    assert_eq!(
        (res["old"].as_str(), res["new"].as_str()),
        (Some("github"), Some("local"))
    );
    assert!(res["note"].as_str().unwrap().contains(RESTART), "{res}");
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
