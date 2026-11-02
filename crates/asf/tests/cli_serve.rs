mod common;

use std::path::Path;

use common::{git, Repo};

fn commit_in(dir: &Path, rel: &str, content: &str) {
    if let Some(p) = Path::new(rel).parent() {
        std::fs::create_dir_all(dir.join(p)).unwrap();
    }
    std::fs::write(dir.join(rel), content).unwrap();
    git(dir, &["add", rel]);
    git(
        dir,
        &[
            "-c",
            "user.name=w",
            "-c",
            "user.email=w@x",
            "commit",
            "-q",
            "-m",
            &format!("add {rel}"),
        ],
    );
}

#[test]
fn serve_once_watches_merges_and_promotes() {
    let repo = Repo::initialised();
    let cfg = repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("runtime = \"herdr\"", "runtime = \"fake\"");
    std::fs::write(&cfg, s).unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "cfg"]);
    let a = repo.create_issue("first", &["--ready", "--scope", "src/**"]);
    let b = repo.create_issue("second", &["--dep", &a, "--scope", "docs/**"]);
    assert_eq!(repo.ok(&["issue", "show", &b])["issue"]["status"], "backlog");

    repo.ok(&["run", "start", &a]);
    let wt = repo.path().join(".worktrees").join(&a);
    commit_in(&wt, "src/x.rs", "");
    let report = wt.join(".factory").join("runs").join(&a).join("report.md");
    std::fs::write(&report, "# done\n").unwrap();
    repo.ok(&[
        "--lane",
        &a,
        "issue",
        "finish",
        &a,
        "--report",
        report.to_str().unwrap(),
    ]);

    let res = repo.ok(&["serve", "--once"]);
    assert_eq!(res["watch"]["transitions"][0]["to"], "succeeded", "{res}");
    assert_eq!(res["merge"]["processed"], 1, "{res}");
    assert_eq!(res["merge"]["items"][0]["state"], "merged", "{res}");
    assert_eq!(res["promoted"], serde_json::json!(["asf-2"]), "{res}");
    assert_eq!(repo.ok(&["issue", "show", &a])["issue"]["status"], "done");
    assert_eq!(repo.ok(&["issue", "show", &b])["issue"]["status"], "ready");

    let human = repo.asf().args(["serve", "--once"]).output().unwrap();
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("tick"), "{text}");
}

#[test]
fn serve_interval_is_validated() {
    let repo = Repo::initialised();
    let out = repo
        .asf()
        .args(["serve", "--once", "--interval", "0"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn serve_dispatch_starts_ready_slots_from_the_plan() {
    let repo = Repo::initialised();
    let cfg = repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("runtime = \"herdr\"", "runtime = \"fake\"")
        .replace("max_parallel = 3", "max_parallel = 2");
    std::fs::write(&cfg, s).unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "cfg"]);
    let a = repo.create_issue("a", &["--ready", "--scope", "src/a.rs"]);
    let b = repo.create_issue("b", &["--ready", "--scope", "src/b.rs"]);
    let c = repo.create_issue("c", &["--ready", "--scope", "src/c.rs"]);
    let d = repo.create_issue("d", &["--dep", &a, "--scope", "docs/**"]);

    let res = repo.ok(&["serve", "--once", "--dispatch"]);
    assert_eq!(res["started"], serde_json::json!([a, b]), "capacity 2: {res}");
    assert_eq!(res["start_errors"].as_array().unwrap().len(), 0, "{res}");
    assert_eq!(
        repo.ok(&["issue", "show", &c])["issue"]["status"],
        "ready",
        "third waits for a slot"
    );
    assert_eq!(repo.ok(&["issue", "show", &d])["issue"]["status"], "backlog");
    assert_eq!(repo.ok(&["run", "list"]).as_array().unwrap().len(), 2);
    // without --dispatch nothing is started
    let res = repo.ok(&["serve", "--once"]);
    assert!(
        res.get("started").is_none() || res["started"].as_array().unwrap().is_empty(),
        "{res}"
    );
    assert_eq!(repo.ok(&["run", "list"]).as_array().unwrap().len(), 2);
    // a second dispatch tick is idempotent while the slots are full
    let res = repo.ok(&["serve", "--once", "--dispatch"]);
    assert_eq!(res["started"].as_array().unwrap().len(), 0, "{res}");
}
