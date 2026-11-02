mod common;

use std::path::Path;

use asf_core::*;
use asf_runtime::FakeRuntime;
use asf_store::SqliteStore;
use common::{git, Repo};

struct F {
    repo: Repo,
    issue: String,
}

fn setup() -> F {
    let repo = Repo::initialised();
    let cfg = repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("runtime = \"herdr\"", "runtime = \"fake\"");
    std::fs::write(&cfg, s).unwrap();
    std::fs::create_dir_all(repo.path().join("src")).unwrap();
    std::fs::write(repo.path().join("src/lib.rs"), "pub fn a() {}\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "files"]);
    let issue = repo.create_issue("feature", &["--ready", "--scope", "src/**"]);
    F { repo, issue }
}

fn store(f: &F) -> SqliteStore {
    SqliteStore::open(&f.repo.path().join(".factory/factory.db")).unwrap()
}

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

/// Starts a worker on the fake runtime, commits `files` in the worktree, runs the finish ritual and `watch`.
fn finished_run(f: &F, files: &[(&str, &str)]) {
    f.repo.ok(&["run", "start", &f.issue]);
    let wt = f.repo.path().join(".worktrees").join(&f.issue);
    for (rel, content) in files {
        commit_in(&wt, rel, content);
    }
    let report = wt.join(".factory").join("runs").join(&f.issue).join("report.md");
    std::fs::write(&report, "# done\n").unwrap();
    f.repo.ok(&[
        "--lane",
        &f.issue,
        "issue",
        "finish",
        &f.issue,
        "--report",
        report.to_str().unwrap(),
    ]);
    let res = f.repo.ok(&["run", "watch", "--once"]);
    assert_eq!(res["transitions"][0]["to"], "succeeded", "{res}");
}

fn main_has(f: &F, rel: &str) -> bool {
    let out = std::process::Command::new("git")
        .args(["ls-tree", "-r", "--name-only", "main"])
        .current_dir(f.repo.path())
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).lines().any(|l| l == rel)
}

#[test]
fn tick_merges_queued_item_closes_issue_and_workspace() {
    let f = setup();
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    let q = f.repo.ok(&["merge", "list"]);
    assert_eq!(q[0]["state"], "queued");
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["processed"], 1);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    assert!(main_has(&f, "src/greet.rs"));
    assert_eq!(f.repo.ok(&["issue", "show", &f.issue])["issue"]["status"], "done");
    assert!(
        store(&f).active_claims().unwrap().is_empty(),
        "claim released after merge"
    );
    let log = FakeRuntime::new(f.repo.path().join(".factory/fake-runtime.json")).log();
    assert!(log.iter().any(|e| e.op == "close_workspace"));
    let cs = store(&f).comments(&f.issue.parse().unwrap()).unwrap();
    assert!(cs
        .iter()
        .any(|c| c.kind == CommentType::Fyi && c.body.contains("merged")));
    let item = &store(&f).merge_queue().unwrap()[0];
    assert_eq!(item.state, MergeState::Merged);
    assert!(item.head_sha.is_some() && item.rebased_onto.is_some());
    // nothing left to do
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["processed"], 0);
}

#[test]
fn tick_rebases_onto_moved_main_before_merging() {
    let f = setup();
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    commit_in(f.repo.path(), "README.txt", "main moved\n");
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    assert!(main_has(&f, "src/greet.rs") && main_has(&f, "README.txt"));
}

#[test]
fn tick_blocks_on_rebase_conflict_and_posts_blocker() {
    let f = setup();
    finished_run(&f, &[("src/lib.rs", "pub fn a() { /* worker */ }\n")]);
    commit_in(f.repo.path(), "src/lib.rs", "pub fn a() { /* main */ }\n");
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "blocked", "{res}");
    let item = &store(&f).merge_queue().unwrap()[0];
    assert_eq!(item.state, MergeState::Blocked);
    assert!(item.last_error.as_deref().unwrap().contains("src/lib.rs"));
    assert_eq!(
        f.repo.ok(&["issue", "show", &f.issue])["issue"]["status"],
        "blocked"
    );
    let cs = store(&f).comments(&f.issue.parse().unwrap()).unwrap();
    assert!(cs.iter().any(|c| c.kind == CommentType::Blocker
        && c.body.contains("src/lib.rs")
        && c.body.contains("rebase")));
    // worktree left clean (rebase aborted), branch intact
    let st = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(f.repo.path().join(".worktrees").join(&f.issue))
        .output()
        .unwrap();
    assert!(st.stdout.is_empty());
    // retry re-queues (human resolved by moving main back, simulated by resetting main)
    git(f.repo.path(), &["reset", "-q", "--hard", "HEAD~1"]);
    f.repo.ok(&["merge", "retry", &f.issue]);
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
}

#[test]
fn tick_fails_on_gate_and_retry_requeues() {
    let f = setup();
    let cfg = f.repo.path().join(".factory/config.toml");
    let mut s = std::fs::read_to_string(&cfg).unwrap();
    s.push_str("\n[[gate]]\nname = \"flaky\"\ncmd = [\"python\", \"-c\", \"import os,sys; sys.exit(0 if os.path.exists('PASS') else 7)\"]\n");
    std::fs::write(&cfg, s).unwrap();
    git(f.repo.path(), &["add", ".factory/config.toml"]);
    git(f.repo.path(), &["commit", "-q", "-m", "gate"]);
    // the worker's own verify also runs gates: make PASS exist in the worktree first, then remove it on main's side
    f.repo.ok(&["run", "start", &f.issue]);
    let wt = f.repo.path().join(".worktrees").join(&f.issue);
    commit_in(&wt, "src/greet.rs", "");
    std::fs::write(wt.join("PASS"), "").unwrap();
    let exclude = f.repo.path().join(".git/info/exclude");
    let mut ex = std::fs::read_to_string(&exclude).unwrap();
    ex.push_str("PASS\n");
    std::fs::write(&exclude, ex).unwrap();
    let report = wt.join(".factory").join("runs").join(&f.issue).join("report.md");
    std::fs::write(&report, "# done\n").unwrap();
    f.repo.ok(&[
        "--lane",
        &f.issue,
        "issue",
        "finish",
        &f.issue,
        "--report",
        report.to_str().unwrap(),
    ]);
    assert_eq!(
        f.repo.ok(&["run", "watch", "--once"])["transitions"][0]["to"],
        "succeeded"
    );
    std::fs::remove_file(wt.join("PASS")).unwrap();

    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "failed", "{res}");
    let item = &store(&f).merge_queue().unwrap()[0];
    assert!(item.gate_results.iter().any(|g| g.name == "flaky" && !g.ok));
    assert!(item.last_error.as_deref().unwrap().contains("flaky"));
    assert_eq!(
        f.repo.ok(&["issue", "show", &f.issue])["issue"]["status"],
        "in-review",
        "issue stays in review"
    );
    std::fs::write(wt.join("PASS"), "").unwrap();
    f.repo.ok(&["merge", "retry", &f.issue]);
    assert_eq!(store(&f).merge_queue().unwrap()[0].state, MergeState::Queued);
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    let (code, ecode, _) = f.repo.err(&["merge", "retry", &f.issue]);
    assert_eq!((code, ecode.as_str()), (3, "conflict"));
}

#[test]
fn tick_refuses_dirty_main_and_skip_removes_from_queue() {
    let f = setup();
    finished_run(&f, &[("src/greet.rs", "")]);
    std::fs::write(f.repo.path().join("src/lib.rs"), "dirty\n").unwrap();
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "failed", "{res}");
    assert!(res["items"][0]["last_error"]
        .as_str()
        .unwrap()
        .contains("uncommitted"));
    git(f.repo.path(), &["checkout", "--", "src/lib.rs"]);
    let res = f.repo.ok(&["merge", "skip", &f.issue]);
    assert_eq!(res["state"], "skipped");
    assert_eq!(f.repo.ok(&["merge", "tick", "--once"])["processed"], 0);
    // a skipped issue can be enqueued again by hand
    f.repo.ok(&["merge", "enqueue", &f.issue]);
    assert_eq!(f.repo.ok(&["merge", "list"]).as_array().unwrap().len(), 1);
}

#[test]
fn enqueue_requires_known_issue_and_list_is_fifo() {
    let f = setup();
    let (code, ecode, _) = f.repo.err(&["merge", "enqueue", "asf-9"]);
    assert_eq!((code, ecode.as_str()), (1, "not_found"));
    let b = f.repo.create_issue("second", &["--ready"]);
    f.repo.ok(&["merge", "enqueue", &b, "--force"]);
    f.repo.ok(&["merge", "enqueue", &f.issue, "--force"]);
    let l = f.repo.ok(&["merge", "list"]);
    assert_eq!(l[0]["issue_id"], "asf-2");
    assert_eq!(l[1]["issue_id"], "asf-1");
    let human = f.repo.asf().args(["merge", "list"]).output().unwrap();
    assert!(String::from_utf8_lossy(&human.stdout).contains("queued"));
}

#[test]
fn enqueue_refuses_an_issue_without_a_verified_run_unless_forced() {
    let f = setup();
    let (code, ecode, msg) = f.repo.err(&["merge", "enqueue", &f.issue]);
    assert_eq!((code, ecode.as_str()), (3, "conflict"), "{msg}");
    assert!(msg.contains("--force"), "{msg}");
    f.repo.ok(&["merge", "enqueue", &f.issue, "--force"]);
    assert_eq!(f.repo.ok(&["merge", "list"]).as_array().unwrap().len(), 1);
}

#[test]
fn enqueue_accepts_a_verified_run_without_force() {
    let f = setup();
    finished_run(&f, &[("src/lib2.rs", "pub fn b() {}\n")]);
    // verification already queued it; skipping and enqueueing again by hand must work without --force
    f.repo.ok(&["merge", "skip", &f.issue]);
    f.repo.ok(&["merge", "enqueue", &f.issue]);
    assert_eq!(f.repo.ok(&["merge", "list"]).as_array().unwrap().len(), 1);
}

#[test]
fn worker_context_cannot_drive_the_orchestrator() {
    let f = setup();
    let worker = |args: &[&str]| {
        let out = f
            .repo
            .asf()
            .env("ASF_RUN", "01RUN")
            .env("ASF_ISSUE", &f.issue)
            .args(args)
            .arg("--json")
            .output()
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
        (out.status.code().unwrap(), v)
    };
    for args in [
        vec!["merge", "enqueue", f.issue.as_str(), "--force"],
        vec!["merge", "tick", "--once"],
        vec!["run", "start", f.issue.as_str()],
        vec!["serve", "--once"],
        vec!["issue", "status", f.issue.as_str(), "done"],
        vec!["issue", "release", f.issue.as_str()],
    ] {
        let (code, v) = worker(&args);
        assert_eq!(v["ok"], false, "{args:?}: {v}");
        assert_eq!(v["error"]["code"], "forbidden", "{args:?}: {v}");
        assert_eq!(code, 1, "{args:?}");
    }
    let (code, v) = worker(&["issue", "show", &f.issue]);
    assert_eq!((code, v["ok"].as_bool()), (0, Some(true)), "{v}");
    let (code, v) = worker(&[
        "issue", "comment", &f.issue, "--type", "STATUS", "--body", "working",
    ]);
    assert_eq!((code, v["ok"].as_bool()), (0, Some(true)), "{v}");
    assert!(f.repo.ok(&["merge", "list"]).as_array().unwrap().is_empty());
}

#[test]
fn tick_blocks_when_the_branch_gained_out_of_scope_commits_after_verification() {
    let f = setup();
    finished_run(&f, &[("src/lib2.rs", "pub fn b() {}\n")]);
    let wt = f.repo.path().join(".worktrees").join(&f.issue);
    commit_in(&wt, "docs/sneaky.md", "# not in scope\n");
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "blocked", "{res}");
    assert!(res["items"][0]["last_error"]
        .as_str()
        .unwrap()
        .contains("docs/sneaky.md"));
    assert!(!main_has(&f, "docs/sneaky.md"));
    assert!(!main_has(&f, "src/lib2.rs"));
    let cs = store(&f).comments(&f.issue.parse().unwrap()).unwrap();
    assert!(cs
        .iter()
        .any(|c| c.kind == CommentType::Blocker && c.body.contains("docs/sneaky.md")));
}
