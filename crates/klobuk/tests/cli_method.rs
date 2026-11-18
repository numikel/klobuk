mod common;

use std::path::{Path, PathBuf};

use common::Repo;

fn shipped(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../methodologies")
        .join(name)
}

fn write_fixture(dir: &Path, manifest: &str, prompt: bool) -> String {
    std::fs::create_dir_all(dir.join("prompts")).unwrap();
    std::fs::write(dir.join("manifest.toml"), manifest).unwrap();
    if prompt {
        std::fs::write(dir.join("prompts/go.md"), "# go\n").unwrap();
    }
    dir.to_string_lossy().to_string()
}

const FIXTURE: &str = r#"
[methodology]
name = "mine"
version = "1.0.0"
description = "test methodology"

[[stage]]
id = "go"
title = "Go"
kind = "prompt"
prompt = "prompts/go.md"
outputs = ["plan.md"]
parser = "builtin:md-tasks"
"#;

#[test]
fn list_shows_shipped_methodologies_and_marks_the_active_one() {
    let r = Repo::initialised();
    let res = r.ok(&["method", "list"]);
    let rows = res.as_array().unwrap();
    let names: Vec<&str> = rows.iter().map(|m| m["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["klobuk-default", "spec-kit", "superpowers"], "{res}");
    for m in rows {
        assert_eq!(m["source"], "builtin", "{m}");
        assert!(m["stages"].as_u64().unwrap() >= 1, "{m}");
        assert_eq!(m["valid"], true, "{m}");
    }
    assert_eq!(rows[0]["active"], true, "{res}");
    assert_eq!(rows[1]["active"], false, "{res}");

    let human = r.klobuk().args(["method", "list"]).output().unwrap();
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(
        text.contains("klobuk-default") && text.contains("spec-kit"),
        "{text}"
    );
}

#[test]
fn list_includes_repo_local_methodologies() {
    let r = Repo::initialised();
    let dir = r.path().join(".factory/methodologies/mine");
    write_fixture(&dir, FIXTURE, true);
    let res = r.ok(&["method", "list"]);
    let mine = res
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == "mine")
        .unwrap_or_else(|| panic!("{res}"));
    assert_eq!(mine["source"], ".factory/methodologies/mine", "{mine}");
    assert_eq!(mine["valid"], true, "{mine}");
    assert_eq!(mine["active"], false, "{mine}");
}

#[test]
fn validate_accepts_the_shipped_manifests_outside_any_repo() {
    let r = Repo::new();
    for name in ["klobuk-default", "spec-kit", "superpowers"] {
        let dir = shipped(name);
        let res = r.ok(&["method", "validate", dir.to_str().unwrap()]);
        assert_eq!(res["name"], name, "{res}");
        assert_eq!(res["valid"], true, "{res}");
        assert_eq!(res["problems"].as_array().unwrap().len(), 0, "{res}");
    }
}

#[test]
fn validate_lists_problems_and_fails() {
    let r = Repo::new();
    let dir = r.path().join("broken");
    let path = write_fixture(&dir, FIXTURE, false);
    let (code, ecode, msg) = r.err(&["method", "validate", &path]);
    assert_eq!((code, ecode.as_str()), (1, "validation"), "{msg}");
    assert!(msg.contains("prompts/go.md"), "{msg}");

    let missing = r.path().join("nothing-here");
    let (code, ecode, _) = r.err(&["method", "validate", missing.to_str().unwrap()]);
    assert_eq!((code, ecode.as_str()), (1, "not_found"));
}

#[test]
fn method_commands_are_allowed_inside_a_worker_run() {
    let r = Repo::initialised();
    let out = r
        .klobuk()
        .env("KLOBUK_RUN", "01RUN")
        .args(["method", "list", "--json"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// `asf-default` was the name of the shipped methodology before the rename: a config that still
/// says it works, resolves to `klobuk-default` and prints one deprecation line on stderr.
#[test]
fn a_config_naming_the_old_default_methodology_is_accepted_with_one_notice() {
    let r = Repo::initialised();
    let cfg = r.path().join(".factory/config.toml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    let old = text.replace("name = \"klobuk-default\"", "name = \"asf-default\"");
    assert_ne!(old, text, "the template names the default methodology");
    std::fs::write(&cfg, old).unwrap();

    let out = r.klobuk().args(["method", "list", "--json"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["result"][0]["name"], "klobuk-default", "{v}");
    assert_eq!(v["result"][0]["active"], true, "{v}");
    let err = String::from_utf8_lossy(&out.stderr);
    let notices: Vec<&str> = err.lines().filter(|l| l.contains("asf-default")).collect();
    assert_eq!(notices.len(), 1, "{err}");
    assert!(notices[0].contains("klobuk-default") && notices[0].contains("deprecated"), "{err}");
}
