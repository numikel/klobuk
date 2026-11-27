mod common;

use std::sync::{Arc, Barrier};
use std::thread;

use common::Repo;

fn body(s: &str) -> String {
    s.lines().skip(1).collect::<Vec<_>>().join("\n")
}

fn seed(r: &Repo) -> (String, String) {
    let a = r.create_issue("a", &["--ready", "--scope", "src/a.rs"]);
    let b = r.create_issue("b", &["--dep", &a, "--label", "risky"]);
    r.ok(&[
        "--lane", "human", "issue", "comment", &a, "--type", "DECISION", "--body", "go",
    ]);
    (a, b)
}

#[test]
fn export_default_path_and_determinism() {
    let r = Repo::initialised();
    seed(&r);
    let res = r.ok(&["export"]);
    let path = r.path().join(".factory/issues.jsonl");
    common::assert_same_path(res["path"].as_str().unwrap(), &path);
    assert_eq!(res["records"], 5, "meta + 2 issues + 1 dep + 1 comment");
    let first = std::fs::read_to_string(&path).unwrap();
    r.ok(&["export"]);
    let second = std::fs::read_to_string(&path).unwrap();
    assert_eq!(body(&first), body(&second));
    assert!(!first.contains('\r'));
    assert!(!first.starts_with('\u{feff}'));
    let custom = r.path().join("dump.jsonl");
    r.ok(&["export", "--out", custom.to_str().unwrap()]);
    assert_eq!(body(&std::fs::read_to_string(&custom).unwrap()), body(&first));
}

#[test]
fn import_into_fresh_repo_then_export_equal() {
    let src = Repo::initialised();
    seed(&src);
    r_export(&src);
    let dump = src.path().join(".factory/issues.jsonl");

    let dst = Repo::initialised();
    let report = dst.ok(&["import", dump.to_str().unwrap()]);
    assert_eq!(report["issues_created"], 2);
    assert_eq!(report["deps"], 1);
    assert_eq!(report["comments"], 1);
    r_export(&dst);
    let a = std::fs::read_to_string(&dump).unwrap();
    let b = std::fs::read_to_string(dst.path().join(".factory/issues.jsonl")).unwrap();
    assert_eq!(body(&a), body(&b));
    assert_eq!(dst.ok(&["repo", "status"])["next_seq"], 3);

    let again = dst.ok(&["import", dump.to_str().unwrap()]);
    assert_eq!(again["issues_created"], 0);
    assert_eq!(again["skipped"], 2);

    let replaced = dst.ok(&["import", dump.to_str().unwrap(), "--mode", "replace"]);
    assert_eq!(replaced["issues_created"], 2);
    dst.klobuk()
        .args(["import", dump.to_str().unwrap(), "--mode", "bogus"])
        .output()
        .map(|o| assert_eq!(o.status.code(), Some(2)))
        .unwrap();
}

fn r_export(r: &Repo) {
    r.ok(&["export"]);
}

#[test]
fn import_missing_file_and_garbage() {
    let r = Repo::initialised();
    let (code, ecode, _) = r.err(&["import", "nope.jsonl"]);
    assert_eq!((code, ecode.as_str()), (1, "io"));
    let bad = r.path().join("bad.jsonl");
    std::fs::write(&bad, "{not json}\n").unwrap();
    let (code, ecode, _) = r.err(&["import", bad.to_str().unwrap()]);
    assert_eq!((code, ecode.as_str()), (1, "validation"));
}

#[test]
fn auto_export_rewrites_file_after_mutations_and_can_be_disabled() {
    let r = Repo::initialised();
    let path = r.path().join(".factory/issues.jsonl");
    assert!(!path.exists());
    let a = r.create_issue("a", &["--ready"]);
    assert!(path.exists(), "auto_export after create");
    let after_create = std::fs::read_to_string(&path).unwrap();
    assert!(after_create.contains(r#""title":"a""#));
    r.ok(&["--lane", &a, "issue", "claim", &a]);
    r.ok(&[
        "--lane", &a, "issue", "comment", &a, "--type", "STATUS", "--body", "started",
    ]);
    let after_comment = std::fs::read_to_string(&path).unwrap();
    assert!(after_comment.contains(r#""body":"started""#));
    assert!(after_comment.contains(r#""status":"in-progress""#));
    // read-only commands never touch the file
    let before = std::fs::read_to_string(&path).unwrap();
    r.ok(&["issue", "list"]);
    r.ok(&["--lane", &a, "issue", "inbox"]);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

    let cfg = r.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("auto_export = true", "auto_export = false");
    std::fs::write(&cfg, s).unwrap();
    r.create_issue("b", &[]);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        before,
        "disabled auto_export leaves the file alone"
    );
}

#[test]
fn doctor_reports_checks() {
    let r = Repo::initialised();
    let res = r.ok(&["repo", "doctor"]);
    assert_eq!(res["ok"], true);
    let names: Vec<&str> = res["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    for n in ["git", "repo_root", "config", "database", "herdr", "claude"] {
        assert!(names.contains(&n), "missing check {n}: {names:?}");
    }
    let db = res["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "database")
        .unwrap();
    assert_eq!(db["ok"], true);
    assert!(db["detail"].as_str().unwrap().contains("schema 2"));
    let herdr = res["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "herdr")
        .unwrap();
    assert_eq!(herdr["required"], false);

    let plain = Repo::new();
    let (code, ecode, _) = plain.err(&["repo", "doctor"]);
    assert_eq!((code, ecode.as_str()), (1, "not_found"));
}

fn parallel<F>(n: usize, f: F) -> Vec<(i32, String)>
where
    F: Fn(usize) -> std::process::Command + Send + Sync + 'static,
{
    let f = Arc::new(f);
    let barrier = Arc::new(Barrier::new(n));
    let handles: Vec<_> = (0..n)
        .map(|i| {
            let f = Arc::clone(&f);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let mut cmd = f(i);
                barrier.wait();
                let out = cmd.output().unwrap();
                (
                    out.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&out.stdout).to_string(),
                )
            })
        })
        .collect();
    handles.into_iter().map(|h| h.join().unwrap()).collect()
}

#[test]
fn parallel_comments_no_lock_errors() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--ready"]);
    let root = r.path().to_path_buf();
    let results = parallel(8, move |i| {
        let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_klobuk"));
        c.env_remove("KLOBUK_RUN")
            .env_remove("KLOBUK_ISSUE")
            .env_remove("KLOBUK_LANE");
        c.current_dir(&root).args([
            "--lane", "human", "issue", "comment", "asf-1", "--type", "FYI", "--body",
        ]);
        c.arg(format!("note {i}"));
        c.arg("--json");
        c
    });
    let failures: Vec<_> = results.iter().filter(|(code, _)| *code != 0).collect();
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(
        r.ok(&["issue", "show", &a, "--dossier"])["comments"]
            .as_array()
            .unwrap()
            .len(),
        8
    );
}

/// Exports that run at the same time each write a temp file of their own: all succeed, the file
/// they leave is one complete snapshot and no temp file stays behind.
#[test]
fn parallel_exports_all_succeed_and_leave_one_complete_file() {
    let r = Repo::initialised();
    seed(&r);
    let root = r.path().to_path_buf();
    let results = parallel(8, move |_| {
        let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_klobuk"));
        c.env_remove("KLOBUK_RUN")
            .env_remove("KLOBUK_ISSUE")
            .env_remove("KLOBUK_LANE");
        c.current_dir(&root).args(["export", "--json"]);
        c
    });
    let failures: Vec<_> = results.iter().filter(|(code, _)| *code != 0).collect();
    assert!(failures.is_empty(), "{failures:?}");
    let factory = r.path().join(".factory");
    let text = std::fs::read_to_string(factory.join("issues.jsonl")).unwrap();
    assert!(text.ends_with('\n'), "complete file, no cut-off last line");
    let records: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect();
    assert_eq!(records.len(), 5, "meta + 2 issues + 1 dep + 1 comment");
    let leftovers: Vec<_> = std::fs::read_dir(&factory)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("issues.jsonl.") || n.ends_with(".tmp") || n.ends_with(".old"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn parallel_claims_exactly_one_winner() {
    let r = Repo::initialised();
    r.create_issue("a", &["--ready"]);
    let root = r.path().to_path_buf();
    let results = parallel(8, move |i| {
        let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_klobuk"));
        c.env_remove("KLOBUK_RUN")
            .env_remove("KLOBUK_ISSUE")
            .env_remove("KLOBUK_LANE");
        c.current_dir(&root)
            .args(["issue", "claim", "asf-1", "--worker"])
            .arg(format!("w{i}"))
            .arg("--json");
        c
    });
    let wins = results.iter().filter(|(code, _)| *code == 0).count();
    let conflicts = results
        .iter()
        .filter(|(code, out)| *code == 3 && out.contains("\"conflict\""))
        .count();
    assert_eq!(wins, 1, "{results:?}");
    assert_eq!(conflicts, 7, "{results:?}");
}

#[test]
fn export_refuses_symlink_destination() {
    let r = Repo::initialised();
    r.create_issue("a", &[]);
    let victim = r.path().join("victim.txt");
    std::fs::write(&victim, "precious").unwrap();
    let link = r.path().join("out.jsonl");
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file(&victim, &link).is_ok();
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&victim, &link).is_ok();
    if !made {
        eprintln!("skipping: cannot create symlinks on this machine");
        return;
    }
    let (code, ecode, msg) = r.err(&["export", "--out", link.to_str().unwrap()]);
    assert_eq!((code, ecode.as_str()), (1, "validation"));
    assert!(msg.contains("symlink"), "{msg}");
    assert_eq!(
        std::fs::read_to_string(&victim).unwrap(),
        "precious",
        "symlink target must be untouched"
    );
    // a stale temp file from a crashed run is replaced, a symlinked temp file is refused
    let out = r.path().join("fresh.jsonl");
    std::fs::write(out.with_extension("jsonl.tmp"), "stale").unwrap();
    r.ok(&["export", "--out", out.to_str().unwrap()]);
    assert!(out.exists() && !out.with_extension("jsonl.tmp").exists());
}

/// Runs `klobuk <args> --json` as the user whose home is `home`; returns the envelope and exit code.
fn json_as(r: &Repo, home: &std::path::Path, args: &[&str]) -> (serde_json::Value, i32) {
    let out = r
        .klobuk()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .args(args)
        .arg("--json")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("not json: {e}\n{stdout}"));
    (v, out.status.code().unwrap_or(-1))
}

const FAKE_KEY: &str = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123";

#[test]
fn export_redacts_home_paths_and_warns_without_printing_values() {
    let r = Repo::initialised();
    let home_dir = tempfile::Builder::new().prefix("klobuk-home-").tempdir().unwrap();
    let home = home_dir.path().join("alice");
    let home_text = home.to_string_lossy().into_owned();
    let a = r.create_issue("a", &["--ready"]);
    let body = format!(
        r"crashed in {home_text}/src/main.rs, also C:\Users\bob\notes.txt and /home/carol/x; kept D:\work\keep.txt; contact dev@corp.io (not t@example.com, 7+me@users.noreply.github.com, t@x) key {FAKE_KEY}"
    );
    let (v, code) = json_as(
        &r,
        &home,
        &[
            "--lane", "human", "issue", "comment", &a, "--type", "FYI", "--body", &body,
        ],
    );
    assert_eq!((code, &v["ok"]), (0, &serde_json::Value::Bool(true)), "{v}");

    // the automatic export after the comment redacts too, and never warns or fails
    let path = r.path().join(".factory/issues.jsonl");
    let auto = std::fs::read_to_string(&path).unwrap();
    let comment: serde_json::Value = serde_json::from_str(auto.lines().last().unwrap()).unwrap();
    let exported = comment["body"].as_str().unwrap();
    assert!(
        exported.starts_with(r"crashed in ~/src/main.rs, also ~\notes.txt and ~/x; kept D:\work\keep.txt;"),
        "{exported}"
    );
    for gone in ["alice", "bob", "carol"] {
        assert!(!auto.contains(gone), "{gone} still in the export");
    }

    // the store keeps the original text
    let shown = r.ok(&["issue", "show", &a, "--dossier"]);
    let stored = shown["comments"][0]["body"].as_str().unwrap();
    assert!(
        stored.contains(&home_text) && stored.contains(r"C:\Users\bob"),
        "{stored}"
    );

    // explicit export: same file, warnings by record and kind, exit 0
    let (v, code) = json_as(&r, &home, &["export"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(
        v["result"]["warnings"],
        serde_json::json!([
            {"issue": a, "comment": 1, "field": "body", "kind": "email"},
            {"issue": a, "comment": 1, "field": "body", "kind": "api-key"},
        ])
    );
    let printed = v.to_string();
    for value in ["corp.io", FAKE_KEY, "sk-ant"] {
        assert!(!printed.contains(value), "{value} printed: {printed}");
    }
    assert_eq!(
        std::fs::read_to_string(&path).unwrap().lines().last(),
        auto.lines().last()
    );

    // human output names them too
    let out = r
        .klobuk()
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .arg("export")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(
        text.contains(&format!("warning: {a} comment 1 body: email")),
        "{text}"
    );
    assert!(!text.contains("corp.io") && !text.contains(FAKE_KEY), "{text}");

    // --strict: exit 1 with warnings (file still written), the message lists kinds, not values
    let (v, code) = json_as(&r, &home, &["export", "--strict"]);
    assert_eq!(code, 1, "{v}");
    let msg = v["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("comment 1 body: api-key") && !msg.contains("corp.io") && !msg.contains(FAKE_KEY),
        "{msg}"
    );

    // --strict without findings: exit 0
    let clean = Repo::initialised();
    clean.create_issue("clean", &["--ready"]);
    let (v, code) = json_as(&clean, &home, &["export", "--strict"]);
    assert_eq!(
        (code, &v["result"]["warnings"]),
        (0, &serde_json::json!([])),
        "{v}"
    );
}

#[test]
fn redacted_exports_are_deterministic_and_do_not_bring_paths_back_on_import() {
    let r = Repo::initialised();
    let a = r.create_issue(r"fix C:\Users\bob\src", &["--ready"]);
    r.ok(&[
        "--lane",
        "human",
        "issue",
        "comment",
        &a,
        "--type",
        "FYI",
        "--body",
        "see /Users/dan/x and /Users/dan/y",
    ]);
    let first = r.path().join("one.jsonl");
    let second = r.path().join("two.jsonl");
    r.ok(&["export", "--out", first.to_str().unwrap()]);
    r.ok(&["export", "--out", second.to_str().unwrap()]);
    let (one, two) = (
        std::fs::read_to_string(&first).unwrap(),
        std::fs::read_to_string(&second).unwrap(),
    );
    assert_eq!(body(&one), body(&two));
    assert!(
        one.contains(r#""title":"fix ~\\src""#) && one.contains("see ~/x and ~/y"),
        "{one}"
    );

    let dst = Repo::initialised();
    dst.ok(&["import", first.to_str().unwrap()]);
    let shown = dst.ok(&["issue", "show", "asf-1", "--dossier"]);
    assert_eq!(
        shown["comments"][0]["body"], "see ~/x and ~/y",
        "the original paths are gone for good"
    );
}

/// `plan import <file>` stores the plan's absolute path as the issue source; it is a home path too.
#[test]
fn export_redacts_the_source_of_an_imported_plan() {
    let r = Repo::initialised();
    let home_dir = tempfile::Builder::new().prefix("klobuk-home-").tempdir().unwrap();
    let home = home_dir.path().join("alice");
    std::fs::create_dir_all(&home).unwrap();
    let plan = home.join("plan.md");
    std::fs::write(&plan, "# Plan\n\n### Task 1: Do it\n\n- [ ] done\n").unwrap();
    let (v, code) = json_as(&r, &home, &["plan", "import", plan.to_str().unwrap()]);
    assert_eq!(code, 0, "{v}");
    let exported = std::fs::read_to_string(r.path().join(".factory/issues.jsonl")).unwrap();
    assert!(exported.contains(r#""source":"~/plan.md#"#), "{exported}");
    assert!(!exported.contains("alice"), "{exported}");
    let shown = r.ok(&["issue", "show", "asf-2"]);
    assert!(
        shown["issue"]["source"].as_str().unwrap().contains("alice"),
        "the store keeps the path"
    );
}
