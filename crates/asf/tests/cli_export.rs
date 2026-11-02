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
    assert_eq!(
        res["path"].as_str().unwrap().replace('\\', "/"),
        path.to_string_lossy().replace('\\', "/")
    );
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
    dst.asf()
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
    assert!(db["detail"].as_str().unwrap().contains("schema 1"));
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
        let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_asf"));
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

#[test]
fn parallel_claims_exactly_one_winner() {
    let r = Repo::initialised();
    r.create_issue("a", &["--ready"]);
    let root = r.path().to_path_buf();
    let results = parallel(8, move |i| {
        let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_asf"));
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
