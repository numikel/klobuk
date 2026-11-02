mod common;

use assert_cmd::prelude::*;
use common::Repo;

#[test]
fn create_prints_sequential_ids_and_defaults() {
    let r = Repo::initialised();
    let a = r.ok(&["issue", "create", "--title", "first"]);
    assert_eq!(a["id"], "asf-1");
    assert_eq!(a["status"], "backlog");
    assert_eq!(a["type"], "task");
    assert_eq!(a["priority"], 2);
    let b = r.ok(&[
        "issue",
        "create",
        "--title",
        "second",
        "--type",
        "epic",
        "--priority",
        "1",
        "--label",
        "risky",
        "--label",
        "ui",
        "--scope",
        "src/**",
        "--scope",
        "Cargo.toml",
        "--accept",
        "builds",
        "--accept",
        "tests pass",
        "--estimate",
        "M",
        "--ready",
    ]);
    assert_eq!(b["id"], "asf-2");
    assert_eq!(b["type"], "epic");
    assert_eq!(b["status"], "ready");
    assert_eq!(b["labels"], serde_json::json!(["risky", "ui"]));
    assert_eq!(b["file_scope"], serde_json::json!(["src/**", "Cargo.toml"]));
    assert_eq!(b["acceptance"][1]["text"], "tests pass");
    assert_eq!(b["estimate"], "M");
}

#[test]
fn create_human_output_is_just_the_id_line() {
    let r = Repo::initialised();
    let out = r
        .asf()
        .args(["issue", "create", "--title", "x"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "asf-1");
}

#[test]
fn create_body_from_file_keeps_utf8() {
    let r = Repo::initialised();
    let body = r.path().join("body.md");
    std::fs::write(&body, "Zażółć gęślą jaźń\nline 2\n").unwrap();
    let id = r.create_issue("utf", &["--body-file", body.to_str().unwrap()]);
    let shown = r.ok(&["issue", "show", &id]);
    assert_eq!(shown["issue"]["body"], "Zażółć gęślą jaźń\nline 2\n");
}

#[test]
fn create_validations() {
    let r = Repo::initialised();
    let (code, ecode, _) = r.err(&["issue", "create", "--title", "   "]);
    assert_eq!((code, ecode.as_str()), (1, "validation"));
    let (code, ecode, msg) = r.err(&["issue", "create", "--title", "x", "--dep", "asf-9"]);
    assert_eq!((code, ecode.as_str()), (1, "validation"));
    assert!(msg.contains("asf-9"), "{msg}");
    let (_, ecode, _) = r.err(&["issue", "create", "--title", "x", "--scope", "src/["]);
    assert_eq!(ecode, "validation");
    r.create_issue("a", &[]);
    let (_, ecode, msg) = r.err(&["issue", "create", "--title", "x", "--dep", "asf-1", "--ready"]);
    assert_eq!(ecode, "validation");
    assert!(msg.contains("--ready"), "{msg}");
    r.asf()
        .args(["issue", "create", "--title", "x", "--type", "bogus"])
        .assert()
        .code(2);
    r.asf()
        .args(["issue", "create", "--title", "x", "--dep", "not-an-id!"])
        .assert()
        .code(2);
    assert_eq!(
        r.ok(&["repo", "status"])["next_seq"],
        2,
        "failed creates must not burn ids"
    );
}

#[test]
fn list_filters_and_ready_flag() {
    let r = Repo::initialised();
    r.create_issue("a", &["--ready"]);
    r.create_issue("b", &["--label", "risky"]);
    r.create_issue("c", &["--type", "review", "--ready"]);
    let all = r.ok(&["issue", "list"]);
    assert_eq!(all.as_array().unwrap().len(), 3);
    assert_eq!(r.ok(&["issue", "list", "--ready"]).as_array().unwrap().len(), 2);
    assert_eq!(
        r.ok(&["issue", "list", "--status", "backlog"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(r.ok(&["issue", "list", "--label", "risky"])[0]["title"], "b");
    assert_eq!(r.ok(&["issue", "list", "--type", "review"])[0]["id"], "asf-3");
    let human = r.asf().args(["issue", "list"]).output().unwrap();
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(
        text.contains("asf-1") && text.contains("ready") && text.contains("a"),
        "{text}"
    );
}

#[test]
fn show_and_dossier() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--ready"]);
    let b = r.create_issue("b", &["--dep", &a]);
    r.ok(&[
        "--lane",
        &a,
        "issue",
        "comment",
        &a,
        "--type",
        "HANDOFF",
        "--to",
        &b,
        "--body",
        "api in src/api.rs",
    ]);
    let plain = r.ok(&["issue", "show", &b]);
    assert_eq!(plain["issue"]["id"], "asf-2");
    assert_eq!(plain["deps"][0]["id"], "asf-1");
    assert_eq!(plain["deps"][0]["status"], "ready");
    assert!(plain.get("comments").is_none() || plain["comments"].as_array().unwrap().is_empty());
    let dossier = r.ok(&["issue", "show", &b, "--dossier"]);
    assert_eq!(dossier["deps"][0]["handoffs"][0]["body"], "api in src/api.rs");
    assert_eq!(dossier["comments"].as_array().unwrap().len(), 0);
    assert_eq!(dossier["dependents"].as_array().unwrap().len(), 0);
    let (code, ecode, _) = r.err(&["issue", "show", "asf-9"]);
    assert_eq!((code, ecode.as_str()), (1, "not_found"));
}

#[test]
fn edit_patches_only_given_fields() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--estimate", "S", "--scope", "x.rs"]);
    let e = r.ok(&["issue", "edit", &a, "--title", "renamed", "--priority", "1"]);
    assert_eq!(e["title"], "renamed");
    assert_eq!(e["priority"], 1);
    assert_eq!(e["estimate"], "S");
    assert_eq!(e["file_scope"], serde_json::json!(["x.rs"]));
    let e = r.ok(&[
        "issue",
        "edit",
        &a,
        "--scope",
        "y.rs",
        "--scope",
        "z.rs",
        "--clear-estimate",
    ]);
    assert_eq!(e["file_scope"], serde_json::json!(["y.rs", "z.rs"]));
    assert!(e["estimate"].is_null());
}

#[test]
fn status_transitions_and_terminal_conflict() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &[]);
    assert_eq!(r.ok(&["issue", "status", &a, "ready"])["status"], "ready");
    assert_eq!(r.ok(&["issue", "status", &a, "done"])["status"], "done");
    let (code, ecode, _) = r.err(&["issue", "status", &a, "ready"]);
    assert_eq!((code, ecode.as_str()), (3, "conflict"));
    r.asf().args(["issue", "status", &a, "nonsense"]).assert().code(2);
}

#[test]
fn deps_add_rm_cycle_and_ready_promotion() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &[]);
    let b = r.create_issue("b", &[]);
    r.ok(&["issue", "dep", "add", &b, &a]);
    let (code, ecode, _) = r.err(&["issue", "dep", "add", &a, &b]);
    assert_eq!((code, ecode.as_str()), (1, "validation"));
    let promoted = r.ok(&["issue", "ready"]);
    assert_eq!(promoted, serde_json::json!(["asf-1"]));
    r.ok(&["issue", "status", &a, "done"]);
    assert_eq!(r.ok(&["issue", "ready"]), serde_json::json!(["asf-2"]));
    r.ok(&["issue", "dep", "rm", &b, &a]);
    let (_, ecode, _) = r.err(&["issue", "dep", "rm", &b, &a]);
    assert_eq!(ecode, "not_found");
}
