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
    finished_run_of(f, &f.issue, files);
}

/// `finished_run` for any ready issue of the repo.
fn finished_run_of(f: &F, issue: &str, files: &[(&str, &str)]) {
    f.repo.ok(&["run", "start", issue]);
    let wt = f.repo.path().join(".worktrees").join(issue);
    for (rel, content) in files {
        commit_in(&wt, rel, content);
    }
    let report = wt.join(".factory").join("runs").join(issue).join("report.md");
    std::fs::write(&report, "# done\n").unwrap();
    f.repo.ok(&[
        "--lane",
        issue,
        "issue",
        "finish",
        issue,
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
    lose_the_worker(&f); // an idle worker would be sent back to resolve it (see below)
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
    // this test breaks the gate after the verification, so the queue has to run it again
    let mut s = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("reuse_verification = true", "reuse_verification = false");
    s.push_str("\n[[gate]]\nname = \"flaky\"\ncmd =[\"python\", \"-c\", \"import os,sys; sys.exit(0 if os.path.exists('PASS') else 7)\"]\n");
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
    lose_the_worker(&f); // an idle worker would be sent back to fix the gate (see below)

    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "failed", "{res}");
    let item = &store(&f).merge_queue().unwrap()[0];
    assert!(item.gate_results.iter().any(|g| g.name == "flaky" && !g.ok));
    assert!(item.last_error.as_deref().unwrap().contains("flaky"));
    assert_eq!(
        f.repo.ok(&["issue", "show", &f.issue])["issue"]["status"],
        "validating",
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

/// Two verified items in the queue and one gate that passes only where a `PASS` file exists: every
/// worktree has it, the repo root does not. The gate logs its working directory to a file outside the repo.
struct Red {
    f: F,
    second: String,
    log: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

fn red_setup(verify_main_after: bool) -> Red {
    let f = setup();
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("gate.py");
    std::fs::write(
        &script,
        "import os, sys\n\
         open(sys.argv[1], 'a').write(os.getcwd() + '\\n')\n\
         if os.path.exists('PASS'):\n    sys.exit(0)\n\
         print('PASS marker missing in', os.getcwd())\n\
         sys.exit(7)\n",
    )
    .unwrap();
    let log = dir.path().join("gate.log");
    let fwd = |p: &Path| p.to_str().unwrap().replace('\\', "/");
    let cfg = f.repo.path().join(".factory/config.toml");
    // these tests count the gate runs of the queue itself
    let mut s = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("reuse_verification = true", "reuse_verification = false");
    if verify_main_after {
        assert!(
            s.contains("verify_main_after = false"),
            "template carries the flag"
        );
        s = s.replace("verify_main_after = false", "verify_main_after = true");
    }
    s.push_str(&format!(
        "\n[[gate]]\nname = \"rootonly\"\ncmd = ['python', '{}', '{}']\n",
        fwd(&script),
        fwd(&log)
    ));
    std::fs::write(&cfg, s).unwrap();
    git(f.repo.path(), &["add", ".factory/config.toml"]);
    git(f.repo.path(), &["commit", "-q", "-m", "gate"]);
    let exclude = f.repo.path().join(".git/info/exclude");
    let mut ex = std::fs::read_to_string(&exclude).unwrap_or_default();
    ex.push_str("PASS\n");
    std::fs::write(&exclude, ex).unwrap();

    let second = f.repo.create_issue("second", &["--ready", "--scope", "docs/**"]);
    for (id, rel) in [(f.issue.clone(), "src/greet.rs"), (second.clone(), "docs/b.md")] {
        f.repo.ok(&["run", "start", &id]);
        let wt = f.repo.path().join(".worktrees").join(&id);
        commit_in(&wt, rel, "x\n");
        std::fs::write(wt.join("PASS"), "").unwrap();
        let report = wt.join(".factory").join("runs").join(&id).join("report.md");
        std::fs::write(&report, "# done\n").unwrap();
        f.repo.ok(&[
            "--lane",
            &id,
            "issue",
            "finish",
            &id,
            "--report",
            report.to_str().unwrap(),
        ]);
        let res = f.repo.ok(&["run", "watch", "--once"]);
        assert_eq!(res["transitions"][0]["to"], "succeeded", "{res}");
    }
    let q = store(&f).merge_queue().unwrap();
    assert_eq!(q.len(), 2, "both verified items are queued: {q:?}");
    Red {
        f,
        second,
        log,
        _dir: dir,
    }
}

fn gate_dirs(r: &Red) -> Vec<String> {
    std::fs::read_to_string(&r.log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn verify_main_after_marks_a_red_main_and_halts_the_tick() {
    let r = red_setup(true);
    let f = &r.f;
    let before = gate_dirs(&r).len();
    let res = f.repo.ok(&["merge", "tick"]);
    assert_eq!(res["processed"], 1, "tick stops after the red main: {res}");
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    assert_eq!(
        res["items"][0]["last_error"], "main red after merge: gate `rootonly` failed (exit 7)",
        "{res}"
    );
    assert!(main_has(f, "src/greet.rs"), "no automatic revert");

    let q = store(f).merge_queue().unwrap();
    assert_eq!(q[0].state, MergeState::Merged);
    assert!(q[0].last_error.is_some());
    assert_eq!(q[1].state, MergeState::Queued, "the other item is left alone");
    assert_eq!(q[1].last_error, None);
    assert_eq!(f.repo.ok(&["issue", "show", &f.issue])["issue"]["status"], "done");
    assert_eq!(
        f.repo.ok(&["issue", "show", &r.second])["issue"]["status"],
        "validating"
    );

    let events = store(f).events_since(0, 10_000).unwrap();
    let red: Vec<_> = events.iter().filter(|e| e.kind == "merge.main_red").collect();
    assert_eq!(red.len(), 1, "{events:?}");
    assert_eq!(
        red[0].issue_id.as_ref().map(|i| i.to_string()),
        Some(f.issue.clone())
    );
    assert_eq!(red[0].payload["gate"], "rootonly");
    assert_eq!(red[0].payload["exit_code"], 7);

    let head = git_head(f);
    assert_eq!(red[0].payload["head"], head.as_str());
    let blockers: Vec<String> = store(f)
        .comments(&f.issue.parse().unwrap())
        .unwrap()
        .into_iter()
        .filter(|c| c.kind == CommentType::Blocker)
        .map(|c| c.body)
        .collect();
    assert_eq!(blockers.len(), 1, "{blockers:?}");
    assert!(blockers[0].contains(&head), "commit SHA: {}", blockers[0]);
    assert!(
        blockers[0].contains("PASS marker missing"),
        "gate output: {}",
        blockers[0]
    );

    // this tick ran the first item's gate in its worktree, then once in the repo root; the second item never started
    let dirs = gate_dirs(&r);
    assert_eq!(dirs.len(), before + 2, "{dirs:?}");
    assert!(dirs[before].contains(".worktrees"), "{dirs:?}");
    assert!(!dirs[before + 1].contains(".worktrees"), "root run: {dirs:?}");
}

#[test]
fn verify_main_after_is_off_by_default_and_runs_no_root_gate() {
    let r = red_setup(false);
    let f = &r.f;
    let before = gate_dirs(&r).len();
    let res = f.repo.ok(&["merge", "tick"]);
    assert_eq!(res["processed"], 2, "{res}");
    for item in res["items"].as_array().unwrap() {
        assert_eq!(item["state"], "merged", "{res}");
        assert!(item["last_error"].is_null(), "{res}");
    }
    let events = store(f).events_since(0, 10_000).unwrap();
    assert!(events.iter().all(|e| e.kind != "merge.main_red"), "{events:?}");
    let dirs = gate_dirs(&r);
    assert_eq!(dirs.len(), before + 2, "one worktree gate run per item: {dirs:?}");
    assert!(
        dirs[before..].iter().all(|d| d.contains(".worktrees")),
        "no gate ran in the repo root: {dirs:?}"
    );
}

fn git_head(f: &F) -> String {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "main"])
        .current_dir(f.repo.path())
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
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

// ---- gate stages and the reviewer gate ----------------------------------------------------------

/// Replaces `from` with `to` in the root config and commits it, so main stays clean for the fast-forward.
fn set_config(f: &F, from: &str, to: &str) {
    let cfg = f.repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg).unwrap();
    assert!(s.contains(from), "config template carries `{from}`");
    std::fs::write(&cfg, s.replacen(from, to, 1)).unwrap();
    git(f.repo.path(), &["add", ".factory/config.toml"]);
    git(f.repo.path(), &["commit", "-q", "-m", "config"]);
}

fn require_review(f: &F) {
    set_config(f, "[review]\nrequired = false", "[review]\nrequired = true");
}

fn start_agents(f: &F) -> usize {
    FakeRuntime::new(f.repo.path().join(".factory/fake-runtime.json"))
        .log()
        .iter()
        .filter(|e| e.op == "start_agent")
        .count()
}

/// The review issues of `issue`, oldest first (a verdict closes one, the next round opens a new one).
fn reviews_of(f: &F, issue: &str) -> Vec<Issue> {
    store(f)
        .list_issues(&IssueFilter {
            kind: Some(IssueType::Review),
            parent: Some(issue.parse().unwrap()),
            ..IssueFilter::default()
        })
        .unwrap()
}

/// The newest review issue of `issue`: the one of the round in progress.
fn review_of(f: &F, issue: &str) -> String {
    reviews_of(f, issue)
        .last()
        .expect("a review issue exists")
        .id
        .to_string()
}

fn review_id(f: &F) -> String {
    review_of(f, &f.issue)
}

/// Active runs per `asf run plan`: (workers, reviewers).
fn running(f: &F) -> (u64, u64) {
    let plan = f.repo.ok(&["run", "plan"]);
    (
        plan["running"].as_u64().unwrap(),
        plan["reviewers_running"].as_u64().unwrap(),
    )
}

fn comment_count(f: &F) -> usize {
    store(f).comments(&f.issue.parse().unwrap()).unwrap().len()
}

fn status_of(f: &F, id: &str) -> String {
    f.repo.ok(&["issue", "show", id])["issue"]["status"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Posts a DECISION on the reviewed issue as the reviewer's lane (author `agent:<review-id>`).
fn decide_as(f: &F, lane: &str, body: &str) {
    f.repo.ok(&[
        "--lane", lane, "issue", "comment", &f.issue, "--type", "DECISION", "--to", "human", "--body", body,
    ]);
}

/// A finished, verified item with `[review] required = true`, ticked once into `reviewing`.
fn reviewing() -> F {
    let f = setup();
    require_review(&f);
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "reviewing", "{res}");
    f
}

#[test]
fn gates_run_in_the_merge_queue_only_when_their_when_lists_merge_or_main() {
    let f = setup();
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("gate.py");
    std::fs::write(
        &script,
        "import os, sys\nopen(sys.argv[1], 'a').write(sys.argv[2] + ' ' + os.getcwd() + '\\n')\n",
    )
    .unwrap();
    let log = dir.path().join("gate.log");
    let fwd = |p: &Path| p.to_str().unwrap().replace('\\', "/");
    let mut gates = String::new();
    for (name, when) in [("v", "[\"verify\"]"), ("m", "[\"merge\"]"), ("r", "[\"main\"]")] {
        gates.push_str(&format!(
            "\n[[gate]]\nname = \"{name}\"\nwhen = {when}\ncmd = ['python', '{}', '{}', '{name}']\n",
            fwd(&script),
            fwd(&log)
        ));
    }
    let cfg = f.repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("verify_main_after = false", "verify_main_after = true");
    std::fs::write(&cfg, format!("{s}{gates}")).unwrap();
    git(f.repo.path(), &["add", ".factory/config.toml"]);
    git(f.repo.path(), &["commit", "-q", "-m", "gates"]);

    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    let lines = |log: &Path| -> Vec<String> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    };
    let before = lines(&log).len();
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    assert!(res["items"][0]["last_error"].is_null(), "{res}");

    let ran = lines(&log)[before..].to_vec();
    assert_eq!(ran.len(), 2, "only the merge and the main gate ran: {ran:?}");
    assert!(
        ran[0].starts_with("m ") && ran[0].contains(".worktrees"),
        "merge gate in the worktree: {ran:?}"
    );
    assert!(
        ran[1].starts_with("r ") && !ran[1].contains(".worktrees"),
        "main gate in the root: {ran:?}"
    );
    let item = &store(&f).merge_queue().unwrap()[0];
    let recorded: Vec<&str> = item.gate_results.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(recorded, ["m"], "the item records the gates it ran");
}

#[test]
fn review_required_enters_reviewing_and_starts_the_reviewer_exactly_once() {
    let f = setup();
    require_review(&f);
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    let agents = start_agents(&f);

    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["processed"], 1, "{res}");
    assert_eq!(res["items"][0]["state"], "reviewing", "{res}");
    assert_eq!(start_agents(&f), agents + 1, "start_review ran on the first tick");
    assert!(!main_has(&f, "src/greet.rs"), "nothing merged before the verdict");
    assert_eq!(status_of(&f, &f.issue), "validating");
    let rid = review_id(&f);
    assert_eq!(status_of(&f, &rid), "in-progress");
    assert_eq!(store(&f).merge_queue().unwrap()[0].state, MergeState::Reviewing);

    // later ticks neither start another reviewer nor write anything
    let comments = comment_count(&f);
    for _ in 0..2 {
        let res = f.repo.ok(&["merge", "tick", "--once"]);
        assert_eq!(res["processed"], 0, "{res}");
    }
    assert_eq!(start_agents(&f), agents + 1);
    assert_eq!(comment_count(&f), comments, "no new comments while waiting");
    assert_eq!(store(&f).merge_queue().unwrap()[0].state, MergeState::Reviewing);
}

#[test]
fn an_item_without_a_review_requirement_merges_without_a_reviewer() {
    let f = setup();
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    let agents = start_agents(&f);
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    assert_eq!(start_agents(&f), agents, "no reviewer without [review] required");
}

#[test]
fn a_review_label_forces_the_review_even_when_not_required() {
    let f = setup();
    set_config(&f, "on_labels = []", "on_labels = [\"risky\"]");
    f.repo.ok(&["issue", "edit", &f.issue, "--label", "risky"]);
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "reviewing", "{res}");
}

#[test]
fn an_approve_decision_merges_and_closes_the_issue_and_the_review_issue() {
    let f = reviewing();
    let rid = review_id(&f);
    decide_as(&f, &rid, "approve");
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    assert!(main_has(&f, "src/greet.rs"));
    assert_eq!(status_of(&f, &f.issue), "done");
    assert_eq!(status_of(&f, &rid), "done");
    assert!(store(&f).active_claims().unwrap().is_empty(), "claims released");
    assert!(
        !f.repo.path().join(".worktrees").join(&rid).exists(),
        "the review worktree is removed by the default [merge] clean_after"
    );
}

#[test]
fn a_changes_decision_fails_the_item_blocks_the_issue_and_posts_a_blocker() {
    let f = reviewing();
    let rid = review_id(&f);
    lose_the_worker(&f); // a reachable worker is sent the findings instead (see the tests at the end)
    decide_as(&f, &rid, "changes:\n- src/greet.rs: hello() has no test");
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "failed", "{res}");
    let err = res["items"][0]["last_error"].as_str().unwrap();
    assert!(err.contains("src/greet.rs: hello() has no test"), "{err}");
    assert_eq!(status_of(&f, &f.issue), "blocked");
    assert!(!main_has(&f, "src/greet.rs"));
    let blockers: Vec<String> = store(&f)
        .comments(&f.issue.parse().unwrap())
        .unwrap()
        .into_iter()
        .filter(|c| c.kind == CommentType::Blocker && c.to_lane == "human")
        .map(|c| c.body)
        .collect();
    assert_eq!(blockers.len(), 1, "{blockers:?}");
    assert!(blockers[0].contains("hello() has no test"), "{}", blockers[0]);

    // the next tick leaves the failed item alone
    assert_eq!(f.repo.ok(&["merge", "tick", "--once"])["processed"], 0);
    // a retry puts the issue back under validation and starts a fresh round
    f.repo.ok(&["merge", "retry", &f.issue]);
    assert_eq!(status_of(&f, &f.issue), "validating");
}

#[test]
fn the_third_changes_round_asks_the_human_instead_of_starting_another_reviewer() {
    let f = reviewing();
    let rid = review_id(&f);
    lose_the_worker(&f); // the operator drives the rounds by hand here, with `merge retry`
    for round in 1..=2 {
        decide_as(&f, &rid, &format!("changes:\n- round {round} finding"));
        let res = f.repo.ok(&["merge", "tick", "--once"]);
        assert_eq!(res["items"][0]["state"], "failed", "round {round}: {res}");
        f.repo.ok(&["merge", "retry", &f.issue]);
        let res = f.repo.ok(&["merge", "tick", "--once"]);
        if round == 1 {
            // the first verdict is history: the new round waits for its own
            assert_eq!(res["items"][0]["state"], "reviewing", "{res}");
            assert_eq!(f.repo.ok(&["merge", "tick", "--once"])["processed"], 0, "{res}");
        }
    }
    // two `changes` rounds are on record: the retry above asked the human instead of reviewing again
    let agents = start_agents(&f);
    let item = &store(&f).merge_queue().unwrap()[0];
    assert_eq!(item.state, MergeState::Blocked, "{item:?}");
    assert!(
        item.last_error.as_deref().unwrap().contains("max_rounds"),
        "{item:?}"
    );
    assert_eq!(status_of(&f, &f.issue), "blocked");
    let blockers: Vec<String> = store(&f)
        .comments(&f.issue.parse().unwrap())
        .unwrap()
        .into_iter()
        .filter(|c| c.kind == CommentType::Blocker)
        .map(|c| c.body)
        .collect();
    let last = blockers.last().unwrap();
    assert!(last.contains("decide") && last.contains("max_rounds"), "{last}");
    assert_eq!(f.repo.ok(&["merge", "tick", "--once"])["processed"], 0);
    assert_eq!(start_agents(&f), agents, "no reviewer started");
}

#[test]
fn no_verdict_keeps_the_item_reviewing_without_a_word() {
    let f = reviewing();
    let rid = review_id(&f);
    let comments = comment_count(&f);
    // a DECISION from anyone but the reviewer, and a reviewer comment that is not a verdict, are not verdicts
    decide_as(&f, "human", "approve");
    decide_as(&f, &rid, "I am still looking at this");
    let before = comment_count(&f);
    assert_eq!(before, comments + 2);
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["processed"], 0, "{res}");
    assert_eq!(comment_count(&f), before, "the orchestrator stayed silent");
    assert_eq!(store(&f).merge_queue().unwrap()[0].state, MergeState::Reviewing);
    assert!(!main_has(&f, "src/greet.rs"));
}

#[test]
fn a_dirty_main_parks_an_approved_item_in_reviewing_until_it_is_clean() {
    let f = reviewing();
    let rid = review_id(&f);
    decide_as(&f, &rid, "approve");
    std::fs::write(f.repo.path().join("src/lib.rs"), "dirty\n").unwrap();
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["processed"], 0, "parked, nothing to report: {res}");
    let item = &store(&f).merge_queue().unwrap()[0];
    assert_eq!(item.state, MergeState::Reviewing, "{item:?}");
    assert!(!main_has(&f, "src/greet.rs"));
    assert_eq!(status_of(&f, &f.issue), "validating");

    git(f.repo.path(), &["checkout", "--", "src/lib.rs"]);
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    assert_eq!(status_of(&f, &rid), "done");
}

// ---- review approval survives a retry on an identical patch -------------------------------------

/// Events of one kind for the issue, oldest first.
fn events_of(f: &F, kind: &str) -> Vec<Event> {
    store(f)
        .events_since(0, 1000)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == kind && e.issue_id.as_ref().map(ToString::to_string) == Some(f.issue.clone()))
        .collect()
}

/// How many `issue.status` events the issue `id` has in the log.
fn status_events_of(f: &F, id: &str) -> usize {
    store(f)
        .events_since(0, 10_000)
        .unwrap()
        .into_iter()
        .filter(|e| {
            e.kind == "issue.status" && e.issue_id.as_ref().map(ToString::to_string).as_deref() == Some(id)
        })
        .count()
}

/// The `closed_at` stamp of issue `id`.
fn closed_at_of(f: &F, id: &str) -> Option<String> {
    store(f)
        .get_issue(&id.parse().unwrap())
        .unwrap()
        .unwrap()
        .closed_at
}

/// An item approved by its reviewer whose fast-forward then could not happen because main moved on in
/// between: back to `queued` (no failure, no error), so the next tick rebases and gates it again.
fn approved_but_main_moved() -> F {
    let f = reviewing();
    let rid = review_id(&f);
    decide_as(&f, &rid, "approve");
    commit_in(f.repo.path(), "README.txt", "main moved\n");
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "queued", "{res}");
    assert!(res["items"][0]["last_error"].is_null(), "{res}");
    assert!(!main_has(&f, "src/greet.rs"), "nothing merged on the stale base");
    assert_eq!(status_of(&f, &f.issue), "validating", "the issue is not blocked");
    let requeued = events_of(&f, "merge.requeued");
    assert_eq!(requeued.len(), 1, "{requeued:?}");
    assert_eq!(requeued[0].payload["reason"], "main_moved");
    f
}

#[test]
fn an_approval_is_recorded_with_a_patch_id_fingerprint() {
    let f = approved_but_main_moved();
    let approved = events_of(&f, "merge.review_approved");
    assert_eq!(approved.len(), 1, "{approved:?}");
    let fp = approved[0].payload["patch_id"].as_str().unwrap();
    assert_eq!(fp.len(), 40, "a git patch-id is 40 hex digits: {fp}");
    assert!(fp.chars().all(|c| c.is_ascii_hexdigit()), "{fp}");
}

#[test]
fn an_identical_patch_after_a_rebase_onto_moved_main_skips_the_review() {
    let f = approved_but_main_moved();
    let agents = start_agents(&f);
    let rounds = events_of(&f, "merge.review_started").len();
    assert_eq!(rounds, 1);

    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    assert!(main_has(&f, "src/greet.rs") && main_has(&f, "README.txt"));
    assert_eq!(status_of(&f, &f.issue), "done");
    assert_eq!(
        events_of(&f, "merge.review_started").len(),
        rounds,
        "no new review round"
    );
    assert_eq!(start_agents(&f), agents, "no reviewer started");
}

#[test]
fn a_whitespace_only_change_gets_a_different_fingerprint_and_a_new_review_round() {
    let f = approved_but_main_moved();
    let wt = f.repo.path().join(".worktrees").join(&f.issue);
    // the same added line, only indented: a whitespace-stripping patch-id would call this patch identical
    commit_in(&wt, "src/greet.rs", "    pub fn hello() {}\n");

    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "reviewing", "{res}");
    assert!(
        !main_has(&f, "src/greet.rs"),
        "nothing merged without a fresh approval"
    );
    assert_eq!(
        events_of(&f, "merge.review_started").len(),
        2,
        "a second round began"
    );

    let rid = review_id(&f);
    decide_as(&f, &rid, "approve");
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    let ids: Vec<String> = events_of(&f, "merge.review_approved")
        .iter()
        .map(|e| e.payload["patch_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert_ne!(ids[0], ids[1], "indentation alone changes the fingerprint");
}

#[test]
fn a_changed_patch_after_the_retry_starts_a_new_review_round() {
    let f = approved_but_main_moved();
    let agents = start_agents(&f);
    let wt = f.repo.path().join(".worktrees").join(&f.issue);
    commit_in(&wt, "src/extra.rs", "pub fn extra() {}\n");

    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "reviewing", "{res}");
    assert!(
        !main_has(&f, "src/greet.rs"),
        "nothing merged without a fresh approval"
    );
    assert_eq!(
        events_of(&f, "merge.review_started").len(),
        2,
        "a second round began"
    );
    assert_eq!(events_of(&f, "merge.review_approved").len(), 1);
    assert!(start_agents(&f) >= agents);
    // the old approval belongs to the old round: the new one waits for its own verdict
    assert_eq!(f.repo.ok(&["merge", "tick", "--once"])["processed"], 0);

    // the fresh approval fingerprints the new patch and merges it
    let rid = review_id(&f);
    decide_as(&f, &rid, "approve");
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    assert!(main_has(&f, "src/extra.rs"));
    let ids: Vec<String> = events_of(&f, "merge.review_approved")
        .iter()
        .map(|e| e.payload["patch_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1], "the two patches have different fingerprints");
}

// ---- reviewers have a limit of their own; a verdict closes its review at once -------------------

/// `[review] max_parallel = n`, committed so main stays clean.
fn review_limit(f: &F, n: u32) {
    set_config(
        f,
        "required = true",
        &format!("required = true\nmax_parallel = {n}"),
    );
}

fn close_workspaces(f: &F) -> usize {
    FakeRuntime::new(f.repo.path().join(".factory/fake-runtime.json"))
        .log()
        .iter()
        .filter(|e| e.op == "close_workspace")
        .count()
}

#[test]
fn a_full_reviewer_limit_leaves_the_next_verified_item_queued_until_a_reviewer_is_free() {
    let f = setup();
    require_review(&f);
    review_limit(&f, 1);
    let second = f.repo.create_issue("docs", &["--ready", "--scope", "docs/**"]);
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    finished_run_of(&f, &second, &[("docs/a.md", "# a\n")]);
    let second_id: IssueId = second.parse().unwrap();
    let comments_before = store(&f).comments(&second_id).unwrap().len();
    let agents = start_agents(&f);

    // FIFO: the first item gets the only reviewer, the second one is left exactly as it was
    let res = f.repo.ok(&["merge", "tick"]);
    assert_eq!(res["processed"], 1, "{res}");
    assert_eq!(res["items"][0]["issue_id"], f.issue.as_str(), "{res}");
    assert_eq!(res["items"][0]["state"], "reviewing", "{res}");
    assert_eq!(start_agents(&f), agents + 1);
    let q = store(&f).merge_queue().unwrap();
    assert_eq!(q[0].state, MergeState::Reviewing);
    assert_eq!(q[1].state, MergeState::Queued, "waits, does not fail: {:?}", q[1]);
    assert_eq!(q[1].attempts, 0, "not even rebased yet");
    assert!(q[1].last_error.is_none(), "{:?}", q[1]);
    assert_eq!(
        store(&f).comments(&second_id).unwrap().len(),
        comments_before,
        "the wait is silent"
    );
    assert_eq!(running(&f), (0, 1));

    // while the reviewer is busy later ticks change nothing
    assert_eq!(f.repo.ok(&["merge", "tick"])["processed"], 0);
    assert_eq!(start_agents(&f), agents + 1);
    assert_eq!(store(&f).merge_queue().unwrap()[1].state, MergeState::Queued);

    // the verdict frees the reviewer in the same tick: the first item merges, the second one's review starts
    decide_as(&f, &review_id(&f), "approve");
    let res = f.repo.ok(&["merge", "tick"]);
    let states: Vec<&str> = res["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, ["merged", "reviewing"], "{res}");
    assert!(main_has(&f, "src/greet.rs"));
    assert_eq!(start_agents(&f), agents + 2);
    assert_eq!(running(&f), (0, 1), "one reviewer at a time");
    assert_eq!(status_of(&f, &review_of(&f, &second)), "in-progress");
}

#[test]
fn a_reviewer_limit_of_two_runs_two_reviewers_at_once() {
    let f = setup();
    require_review(&f);
    let second = f.repo.create_issue("docs", &["--ready", "--scope", "docs/**"]);
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    finished_run_of(&f, &second, &[("docs/a.md", "# a\n")]);
    let res = f.repo.ok(&["merge", "tick"]);
    assert_eq!(res["processed"], 2, "the default limit is 2: {res}");
    assert_eq!(running(&f), (0, 2));
}

#[test]
fn a_running_reviewer_does_not_take_a_worker_slot() {
    let f = reviewing();
    set_config(&f, "max_parallel = 3 ", "max_parallel = 1 ");
    let other = f.repo.create_issue("other", &["--ready", "--scope", "docs/**"]);
    assert_eq!(running(&f), (0, 1), "a reviewer, no worker");
    let plan = f.repo.ok(&["run", "plan"]);
    assert_eq!(plan["capacity"], 1);
    let slots: Vec<&str> = plan["slots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["issue"].as_str().unwrap())
        .collect();
    assert_eq!(slots, [other.as_str()], "the only worker slot is free: {plan}");
}

#[test]
fn an_approve_verdict_closes_the_review_issue_its_claim_and_run_even_when_the_merge_is_parked() {
    let f = reviewing();
    let rid = review_id(&f);
    assert_eq!(running(&f), (0, 1));
    decide_as(&f, &rid, "approve");
    std::fs::write(f.repo.path().join("src/lib.rs"), "dirty\n").unwrap();
    let closed = close_workspaces(&f);
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["processed"], 0, "parked behind the dirty main: {res}");
    assert_eq!(store(&f).merge_queue().unwrap()[0].state, MergeState::Reviewing);
    assert_eq!(
        status_of(&f, &rid),
        "done",
        "closed as soon as the verdict was read"
    );
    assert!(
        !store(&f)
            .active_claims()
            .unwrap()
            .iter()
            .any(|c| c.issue_id.to_string() == rid),
        "the review claim is released"
    );
    let run = store(&f)
        .runs(Some(&rid.parse().unwrap()), false)
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(run.state, RunState::Succeeded);
    assert_eq!(
        close_workspaces(&f),
        closed + 1,
        "the reviewer's workspace is closed"
    );
    assert_eq!(running(&f), (0, 0));

    // the parked item is looked at again; a review that is already closed is left alone: no new
    // `issue.status` event and no moved `closed_at`
    let status_events = status_events_of(&f, &rid);
    let closed_at = closed_at_of(&f, &rid);
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["processed"], 0, "{res}");
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["processed"], 0, "{res}");
    assert_eq!(
        status_events_of(&f, &rid),
        status_events,
        "parked ticks add no issue.status event"
    );
    git(f.repo.path(), &["checkout", "--", "src/lib.rs"]);
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    assert_eq!(status_of(&f, &rid), "done");
    assert_eq!(
        status_events_of(&f, &rid),
        status_events,
        "the merge adds no issue.status event either"
    );
    assert_eq!(closed_at_of(&f, &rid), closed_at, "closed_at stays put");
}

#[test]
fn a_changes_verdict_closes_the_review_at_once_and_the_next_round_gets_a_fresh_review() {
    let f = reviewing();
    let rid = review_id(&f);
    lose_the_worker(&f);
    decide_as(&f, &rid, "changes:\n- src/greet.rs: hello() has no test");
    let closed = close_workspaces(&f);
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "failed", "{res}");
    assert_eq!(status_of(&f, &rid), "done");
    assert!(
        !store(&f)
            .active_claims()
            .unwrap()
            .iter()
            .any(|c| c.issue_id.to_string() == rid),
        "the review claim is released"
    );
    assert_eq!(close_workspaces(&f), closed + 1);
    assert_eq!(running(&f), (0, 0), "no reviewer left holding a slot");

    // a retry opens the next round with a new review issue and a new reviewer
    let agents = start_agents(&f);
    f.repo.ok(&["merge", "retry", &f.issue]);
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "reviewing", "{res}");
    assert_eq!(start_agents(&f), agents + 1);
    let next = review_id(&f);
    assert_ne!(next, rid, "a new review issue for the new round");
    assert_eq!(status_of(&f, &next), "in-progress");
    assert_eq!(running(&f), (0, 1));
}

// ---- a `changes` verdict sends the findings back to the idle worker ------------------------------

/// The latest run of the reviewed issue.
fn worker_run(f: &F) -> Run {
    store(f)
        .runs(Some(&f.issue.parse().unwrap()), false)
        .unwrap()
        .pop()
        .expect("the issue has a run")
}

/// Closes the worker's runtime workspace: its agent can no longer be reached.
fn lose_the_worker(f: &F) {
    lose_the_worker_of(f, &f.issue);
}

/// `lose_the_worker` for any issue of the repo.
fn lose_the_worker_of(f: &F, issue: &str) {
    let ws = store(f)
        .runs(Some(&issue.parse().unwrap()), false)
        .unwrap()
        .pop()
        .expect("the issue has a run")
        .workspace_id
        .expect("the worker has a workspace");
    FakeRuntime::new(f.repo.path().join(".factory/fake-runtime.json"))
        .close_workspace(&ws)
        .unwrap();
}

/// Prompts that point a worker at a review file, oldest first.
fn send_back_prompts(f: &F) -> Vec<String> {
    FakeRuntime::new(f.repo.path().join(".factory/fake-runtime.json"))
        .log()
        .into_iter()
        .filter(|e| e.op == "prompt")
        .filter_map(|e| e.args["text"].as_str().map(str::to_string))
        .filter(|t| t.contains("review-"))
        .collect()
}

fn review_file(f: &F, round: u32) -> std::path::PathBuf {
    f.repo
        .path()
        .join(".worktrees")
        .join(&f.issue)
        .join(".factory/runs")
        .join(&f.issue)
        .join(format!("review-{round}.md"))
}

fn comments_of(f: &F, kind: CommentType) -> Vec<Comment> {
    store(f)
        .comments(&f.issue.parse().unwrap())
        .unwrap()
        .into_iter()
        .filter(|c| c.kind == kind)
        .collect()
}

/// The worker commits `rel`, writes its report and finishes again; `run watch` then verifies the run.
fn finish_again(f: &F, rel: &str, content: &str) -> serde_json::Value {
    let issue = &f.issue;
    let wt = f.repo.path().join(".worktrees").join(issue);
    commit_in(&wt, rel, content);
    let report = wt.join(".factory/runs").join(issue).join("report.md");
    std::fs::write(&report, "# done again\n").unwrap();
    f.repo.ok(&[
        "--lane",
        issue,
        "issue",
        "finish",
        issue,
        "--report",
        report.to_str().unwrap(),
    ]);
    f.repo.ok(&["run", "watch", "--once"])
}

#[test]
fn a_changes_verdict_sends_the_findings_to_a_reachable_worker_instead_of_a_blocker() {
    let f = reviewing();
    let rid = review_id(&f);
    assert!(send_back_prompts(&f).is_empty());
    decide_as(&f, &rid, "changes:\n- src/greet.rs: hello() has no \"test\"");
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "failed", "{res}");

    // the findings are in a file in the worker's worktree: data from the reviewer, then what to do
    let text = std::fs::read_to_string(review_file(&f, 1)).expect("review-1.md is written");
    assert!(!text.contains('\r'), "LF line endings:\n{text}");
    assert!(
        text.contains("- src/greet.rs: hello() has no \"test\""),
        "the finding is listed:\n{text}"
    );
    assert!(text.contains("data"), "says the findings are data:\n{text}");
    assert!(text.contains("test-first"), "{text}");
    assert!(
        text.contains("issue finish") && text.contains(&f.issue),
        "tells the worker to finish again:\n{text}"
    );

    // the worker gets exactly one line that points at the file; the findings are not inline
    let prompts = send_back_prompts(&f);
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(!prompts[0].contains('\n'), "one line: {:?}", prompts[0]);
    assert!(
        prompts[0].contains(&format!(".factory/runs/{}/review-1.md", f.issue)),
        "{prompts:?}"
    );
    assert!(!prompts[0].contains("hello()"), "no findings inline: {prompts:?}");

    // no BLOCKER for the human, a STATUS for everyone, and the issue is being worked on again
    assert!(
        comments_of(&f, CommentType::Blocker).is_empty(),
        "{:?}",
        comments_of(&f, CommentType::Blocker)
    );
    let status: Vec<String> = comments_of(&f, CommentType::Status)
        .into_iter()
        .filter(|c| c.author == "orchestrator" && c.to_lane == "all")
        .map(|c| c.body)
        .collect();
    assert_eq!(status.len(), 1, "{status:?}");
    assert!(
        status[0].contains("review round 1 asked for changes") && status[0].contains("sent to the worker"),
        "{status:?}"
    );
    assert_eq!(status_of(&f, &f.issue), "in-progress");
    assert_eq!(worker_run(&f).state, RunState::Working);
    assert_eq!(
        running(&f),
        (1, 0),
        "the worker holds its slot again, no reviewer is left"
    );
    assert!(!main_has(&f, "src/greet.rs"));

    // the failed item is left alone until the worker is done
    assert_eq!(f.repo.ok(&["merge", "tick", "--once"])["processed"], 0);
}

#[test]
fn a_worker_that_finishes_again_gets_its_item_queued_and_the_next_review_round_starts() {
    let f = reviewing();
    let rid = review_id(&f);
    decide_as(&f, &rid, "changes:\n- src/greet.rs: hello() has no test");
    f.repo.ok(&["merge", "tick", "--once"]);
    let item_id = store(&f).merge_queue().unwrap()[0].id;

    let res = finish_again(&f, "src/greet_test.rs", "// covers hello()\n");
    assert_eq!(res["transitions"][0]["to"], "succeeded", "{res}");
    assert_eq!(status_of(&f, &f.issue), "validating");
    let queue = store(&f).merge_queue().unwrap();
    assert_eq!(queue.len(), 1, "the item is reused, not duplicated: {queue:?}");
    assert_eq!(queue[0].id, item_id);
    assert_eq!(queue[0].state, MergeState::Queued, "{queue:?}");
    assert!(queue[0].last_error.is_none(), "{queue:?}");

    // no operator command: the next tick rebases, gates and starts round 2
    let agents = start_agents(&f);
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "reviewing", "{res}");
    assert_eq!(res["items"][0]["id"], item_id);
    assert_eq!(start_agents(&f), agents + 1, "a reviewer for round 2");
    let next = review_id(&f);
    assert_ne!(next, rid, "round 2 has its own review issue");

    decide_as(&f, &next, "approve");
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    assert!(main_has(&f, "src/greet_test.rs"), "the fix was merged");
    assert_eq!(status_of(&f, &f.issue), "done");
}

#[test]
fn a_run_reopened_after_a_rebase_verifies_against_the_rebased_base_and_lists_only_the_workers_files() {
    let f = setup();
    require_review(&f);
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    let stale_base = worker_run(&f).base_sha;

    // main moves on with a file far outside the issue's scope, then the queue rebases the branch onto it
    commit_in(f.repo.path(), "README.md", "# moved on\n");
    commit_in(f.repo.path(), "docs/notes.txt", "also moved\n");
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "reviewing", "{res}");
    let wt = f.repo.path().join(".worktrees").join(&f.issue);
    assert_ne!(
        git_out(&wt, &["merge-base", "main", "HEAD"]),
        stale_base,
        "the branch now sits on a newer main than the run's base_sha"
    );

    // the reviewer asks for changes, the worker is sent back, fixes and finishes again
    decide_as(
        &f,
        &review_id(&f),
        "changes:\n- src/greet.rs: hello() has no test",
    );
    f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(worker_run(&f).state, RunState::Working);
    let res = finish_again(&f, "src/greet_test.rs", "// covers hello()\n");
    assert_eq!(res["transitions"][0]["to"], "succeeded", "{res}");
    assert_eq!(
        res["transitions"][0]["detail"]
            .as_str()
            .unwrap()
            .split(' ')
            .next(),
        Some("2"),
        "only the worker's two files are counted, not main's: {res}"
    );
    let run = worker_run(&f);
    assert_eq!(run.state, RunState::Succeeded);
    assert_eq!(run.exit_reason.as_deref(), Some("verified"));
    assert_eq!(status_of(&f, &f.issue), "validating");
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn a_changes_verdict_with_an_unreachable_worker_keeps_the_blocker_and_the_retry_hint() {
    let f = reviewing();
    let rid = review_id(&f);
    lose_the_worker(&f);
    decide_as(&f, &rid, "changes:\n- src/greet.rs: hello() has no test");
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "failed", "{res}");
    assert_eq!(status_of(&f, &f.issue), "blocked");
    assert!(send_back_prompts(&f).is_empty(), "nothing could be delivered");
    let blockers = comments_of(&f, CommentType::Blocker);
    assert_eq!(blockers.len(), 1, "{blockers:?}");
    assert_eq!(blockers[0].to_lane, "human");
    assert!(
        blockers[0].body.contains("hello() has no test"),
        "{}",
        blockers[0].body
    );
    assert!(
        blockers[0].body.contains(&format!("asf merge retry {}", f.issue)),
        "{}",
        blockers[0].body
    );
    assert_eq!(
        worker_run(&f).state,
        RunState::Succeeded,
        "the run is left as it was"
    );
}

#[test]
fn the_round_that_reaches_max_rounds_asks_the_human_and_sends_nothing_to_the_worker() {
    let f = reviewing();
    let rid = review_id(&f);
    decide_as(&f, &rid, "changes:\n- round 1 finding");
    f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(send_back_prompts(&f).len(), 1, "round 1 goes back to the worker");
    finish_again(&f, "src/greet_test.rs", "// round 1 fix\n");
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "reviewing", "{res}");

    // two `changes` rounds are on record now and [review] max_rounds is 2: the human decides
    let rid = review_id(&f);
    decide_as(&f, &rid, "changes:\n- round 2 finding");
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "failed", "{res}");
    assert_eq!(
        send_back_prompts(&f).len(),
        1,
        "nothing is sent for the last round"
    );
    assert!(!review_file(&f, 2).exists(), "no review-2.md");
    assert_eq!(status_of(&f, &f.issue), "blocked");
    let last = comments_of(&f, CommentType::Blocker).pop().expect("a BLOCKER");
    assert_eq!(last.to_lane, "human");
    assert!(last.body.contains("round 2 finding"), "{}", last.body);
    assert!(
        last.body.contains(&format!("asf merge retry {}", f.issue)),
        "{}",
        last.body
    );
    assert_eq!(worker_run(&f).state, RunState::Succeeded);
}

#[test]
fn merge_retry_is_refused_while_the_worker_is_fixing_the_findings() {
    let f = reviewing();
    let rid = review_id(&f);
    decide_as(&f, &rid, "changes:\n- src/greet.rs: hello() has no test");
    f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(worker_run(&f).state, RunState::Working);
    let (code, error, message) = f.repo.err(&["merge", "retry", &f.issue]);
    assert_eq!((code, error.as_str()), (3, "conflict"), "{message}");
    assert_eq!(
        store(&f).merge_queue().unwrap()[0].state,
        MergeState::Failed,
        "the item was not requeued behind the worker's back"
    );
}

// ---- a reviewer that cannot start does not burn the merge item ----------------------------------

/// A 1 ms startup window, so the re-read after a start timeout finds `unknown` at once and gives up.
/// Committed so main stays clean; call it before the worker's run is set up.
fn short_start_window(f: &F) {
    set_config(f, "auto_trust = true", "start_timeout_ms = 1\nauto_trust = true");
}

/// The next `n` agent starts time out and the agent never comes up. Call it after `finished_run`,
/// which starts the worker's own agent first.
fn failing_reviewer_starts(f: &F, n: usize) {
    script_starts(f, vec![serde_json::json!({"error": "timeout"}); n]);
}

fn script_starts(f: &F, entries: Vec<serde_json::Value>) {
    FakeRuntime::new(f.repo.path().join(".factory/fake-runtime.json"))
        .script(serde_json::json!({ "start_agent": entries }))
        .unwrap();
}

fn item_state(f: &F) -> MergeState {
    store(f).merge_queue().unwrap()[0].state
}

/// A gate that appends one line to a log outside the repo per run, with `reuse_verification` off so
/// that every gate run of the queue itself shows. `runs` counts the lines: the worker's verification
/// counts too, so take a baseline after `finished_run`.
struct CountingGate {
    log: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

impl CountingGate {
    fn runs(&self) -> usize {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .count()
    }
}

fn counting_gate(f: &F) -> CountingGate {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("gate.py");
    std::fs::write(&script, "import sys\nopen(sys.argv[1], 'a').write('run\\n')\n").unwrap();
    let log = dir.path().join("gate.log");
    let fwd = |p: &Path| p.to_str().unwrap().replace('\\', "/");
    let cfg = f.repo.path().join(".factory/config.toml");
    let mut s = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("reuse_verification = true", "reuse_verification = false");
    s.push_str(&format!(
        "\n[[gate]]\nname = \"counted\"\ncmd = ['python', '{}', '{}']\n",
        fwd(&script),
        fwd(&log)
    ));
    std::fs::write(&cfg, s).unwrap();
    git(f.repo.path(), &["add", ".factory/config.toml"]);
    git(f.repo.path(), &["commit", "-q", "-m", "counted gate"]);
    CountingGate { log, _dir: dir }
}

/// `require_review` + a short start window + a counting gate + a finished run, with the first
/// reviewer start scripted to fail. Returns the fixture and the gate runs seen so far.
fn failed_reviewer_start_setup() -> (F, CountingGate) {
    let f = setup();
    require_review(&f);
    short_start_window(&f);
    let gate = counting_gate(&f);
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    failing_reviewer_starts(&f, 1);
    (f, gate)
}

#[test]
fn a_failed_reviewer_start_keeps_the_item_in_review_and_the_gates_run_once() {
    let (f, gate) = failed_reviewer_start_setup();
    let (verified, agents) = (gate.runs(), start_agents(&f));

    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "reviewing", "{res}");
    assert_eq!(start_agents(&f), agents + 1, "the first reviewer did not come up");
    assert_eq!(gate.runs(), verified + 1, "the queue gated the branch once");

    // the next tick starts a fresh reviewer; neither the branch nor main moved, so no gate run
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "reviewing", "{res}");
    assert_eq!(start_agents(&f), agents + 2, "a fresh reviewer");
    assert_eq!(status_of(&f, &review_id(&f)), "in-progress");
    assert_eq!(gate.runs(), verified + 1, "the gates were not repeated");
    assert_eq!(
        store(&f).merge_queue().unwrap()[0].attempts,
        1,
        "one pass of the queue"
    );
    assert!(events_of(&f, "merge.requeued").is_empty());
    assert_eq!(events_of(&f, "merge.review_started").len(), 2);
    assert_eq!(
        item_last_error(&f),
        None,
        "the failure is cleared once a reviewer is up"
    );
}

#[test]
fn a_branch_that_moved_after_a_failed_reviewer_start_is_gated_again() {
    let (f, gate) = failed_reviewer_start_setup();
    let agents = start_agents(&f);
    assert_eq!(
        f.repo.ok(&["merge", "tick", "--once"])["items"][0]["state"],
        "reviewing"
    );
    let gated = gate.runs();
    commit_in(
        &f.repo.path().join(".worktrees").join(&f.issue),
        "src/extra.rs",
        "pub fn x() {}\n",
    );

    // the gates passed on a head that is gone: back to the queue, no reviewer yet
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "queued", "{res}");
    assert_eq!(start_agents(&f), agents + 1, "no reviewer on stale gates");
    let requeued = events_of(&f, "merge.requeued");
    assert_eq!(requeued.len(), 1);
    assert_eq!(requeued[0].payload["reason"], "branch_moved");
    assert_eq!(gate.runs(), gated);

    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "reviewing", "{res}");
    assert_eq!(gate.runs(), gated + 1, "the new head is gated");
    assert_eq!(start_agents(&f), agents + 2);
}

#[test]
fn a_main_that_moved_after_a_failed_reviewer_start_is_gated_again() {
    let (f, gate) = failed_reviewer_start_setup();
    let agents = start_agents(&f);
    assert_eq!(
        f.repo.ok(&["merge", "tick", "--once"])["items"][0]["state"],
        "reviewing"
    );
    let gated = gate.runs();
    commit_in(f.repo.path(), "README.txt", "main moved\n");

    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "queued", "{res}");
    assert_eq!(start_agents(&f), agents + 1, "no reviewer on stale gates");
    assert_eq!(events_of(&f, "merge.requeued")[0].payload["reason"], "main_moved");
    assert_eq!(gate.runs(), gated);

    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "reviewing", "{res}");
    assert_eq!(gate.runs(), gated + 1, "the rebased branch is gated");
    assert_eq!(start_agents(&f), agents + 2);
}

fn item_last_error(f: &F) -> Option<String> {
    store(f).merge_queue().unwrap()[0].last_error.clone()
}

#[test]
fn a_reviewer_that_cannot_start_closes_its_review_and_leaves_the_item_for_the_next_tick() {
    let f = setup();
    require_review(&f);
    short_start_window(&f);
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    failing_reviewer_starts(&f, 1);
    let agents = start_agents(&f);

    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "reviewing", "{res}");
    assert_eq!(start_agents(&f), agents + 1);
    let err = res["items"][0]["last_error"].as_str().expect("the cause is kept");
    assert!(err.contains("reviewer") && err.contains("1 of 2"), "{err}");
    assert!(
        comments_of(&f, CommentType::Blocker).is_empty(),
        "no BLOCKER on the first failure"
    );
    assert_eq!(events_of(&f, "merge.review_start_failed").len(), 1);

    // the failed reviewer is closed: review issue, run, claim, workspace, slot
    let rid = review_id(&f);
    assert_eq!(status_of(&f, &rid), "cancelled");
    let run = store(&f)
        .runs(Some(&rid.parse().unwrap()), false)
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(run.state, RunState::Failed, "{run:?}");
    assert!(run.finished_at.is_some());
    assert!(store(&f)
        .active_claims()
        .unwrap()
        .iter()
        .all(|c| c.issue_id.to_string() != rid));
    assert_eq!(running(&f), (0, 0));
    assert_eq!(
        status_of(&f, &f.issue),
        "validating",
        "the reviewed issue is untouched"
    );

    // the next tick starts a fresh reviewer on a new review issue
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "reviewing", "{res}");
    assert_eq!(start_agents(&f), agents + 2);
    let next = review_id(&f);
    assert_ne!(next, rid);
    assert_eq!(status_of(&f, &next), "in-progress");
    assert_eq!(running(&f), (0, 1));
    assert!(comments_of(&f, CommentType::Blocker).is_empty());
}

#[test]
fn two_failed_reviewer_starts_in_a_row_fail_the_item_with_a_blocker() {
    let f = setup();
    require_review(&f);
    short_start_window(&f);
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    failing_reviewer_starts(&f, 2);
    let agents = start_agents(&f);

    assert_eq!(
        f.repo.ok(&["merge", "tick", "--once"])["items"][0]["state"],
        "reviewing"
    );
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "failed", "{res}");
    assert_eq!(start_agents(&f), agents + 2, "two starts, then it gives up");
    let err = res["items"][0]["last_error"].as_str().unwrap();
    assert!(err.contains("cannot start the reviewer"), "{err}");
    let blockers = comments_of(&f, CommentType::Blocker);
    assert_eq!(blockers.len(), 1, "{blockers:?}");
    assert_eq!(blockers[0].to_lane, "human");
    assert!(blockers[0].body.contains("merge retry"), "{}", blockers[0].body);

    for r in reviews_of(&f, &f.issue) {
        assert_eq!(status_of(&f, r.id.as_str()), "cancelled", "{}", r.id);
    }
    assert_eq!(reviews_of(&f, &f.issue).len(), 2);
    assert_eq!(running(&f), (0, 0));
    assert_eq!(
        f.repo.ok(&["merge", "tick", "--once"])["processed"],
        0,
        "a failed item waits for the human"
    );
}

#[test]
fn a_human_retry_starts_the_count_of_failed_reviewer_starts_over() {
    let f = setup();
    require_review(&f);
    short_start_window(&f);
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    failing_reviewer_starts(&f, 2);
    f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(
        f.repo.ok(&["merge", "tick", "--once"])["items"][0]["state"],
        "failed"
    );
    assert_eq!(comments_of(&f, CommentType::Blocker).len(), 1);

    // the human fixed the cause and retries; the first start afterwards fails again
    f.repo.ok(&["merge", "retry", &f.issue]);
    failing_reviewer_starts(&f, 1);
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(
        res["items"][0]["state"], "reviewing",
        "one failed start after a retry is the first of the new count, not the third: {res}"
    );
    let err = res["items"][0]["last_error"].as_str().unwrap();
    assert!(err.contains("1 of 2"), "{err}");
    assert_eq!(
        comments_of(&f, CommentType::Blocker).len(),
        1,
        "no second BLOCKER"
    );

    // and the count still ends: a second failed start after the retry fails the item again
    failing_reviewer_starts(&f, 1);
    assert_eq!(
        f.repo.ok(&["merge", "tick", "--once"])["items"][0]["state"],
        "failed"
    );
    assert_eq!(comments_of(&f, CommentType::Blocker).len(), 2);
}

#[test]
fn a_reviewer_that_started_resets_the_count_of_failed_starts() {
    let f = setup();
    require_review(&f);
    short_start_window(&f);
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    // fails, starts, then fails again in the next round: two failures, but not in a row
    script_starts(
        &f,
        vec![
            serde_json::json!({"error": "timeout"}),
            serde_json::json!({"state": "idle"}),
            serde_json::json!({"error": "timeout"}),
        ],
    );
    assert_eq!(
        f.repo.ok(&["merge", "tick", "--once"])["items"][0]["state"],
        "reviewing"
    );
    assert_eq!(
        f.repo.ok(&["merge", "tick", "--once"])["items"][0]["state"],
        "reviewing"
    );
    assert_eq!(
        events_of(&f, "merge.review_started").len(),
        2,
        "the second start came up"
    );

    lose_the_worker(&f);
    decide_as(
        &f,
        &review_id(&f),
        "changes:\n- src/greet.rs: hello() has no test",
    );
    assert_eq!(
        f.repo.ok(&["merge", "tick", "--once"])["items"][0]["state"],
        "failed"
    );
    f.repo.ok(&["merge", "retry", &f.issue]);

    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(
        res["items"][0]["state"], "reviewing",
        "the earlier failure does not count: {res}"
    );
    assert_eq!(item_state(&f), MergeState::Reviewing);
}

// ---- a gate failure on a test outside the branch is rerun once and filed as flaky ------------------

const FLAKY_TEST: &str = "remind::flow_tests::waits";
const FLAKY_FILE: &str = "crates/foo/src/remind.rs";

/// A queued item whose merge gate `test` prints a `cargo test` failure for one test on its first
/// `fail_times` runs and passes afterwards. `counter` holds how many times the gate has run.
struct Flaky {
    f: F,
    issue: String,
    counter: std::path::PathBuf,
    /// `<hold>.started` appears when the rerun begins, the rerun ends once `<hold>.go` exists.
    hold: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

fn flaky_setup(scope: &str, worker_file: &str, test: &str, fail_times: u32) -> Flaky {
    flaky_setup_with(scope, worker_file, test, fail_times, 0, false)
}

/// `flaky_setup` where a failing run also writes the `Running` line and then `noise` more lines to
/// stderr, after the failures list on stdout, as `cargo test --workspace` does: with enough noise the
/// 40-line tail of the gate holds no test name.
fn flaky_setup_noisy(scope: &str, worker_file: &str, test: &str, fail_times: u32, noise: u32) -> Flaky {
    flaky_setup_with(scope, worker_file, test, fail_times, noise, false)
}

/// The general form. With `hold` the second run of the gate (the rerun) writes `<hold>.started` and
/// waits for `<hold>.go` before it answers, so a test can act while the rerun is in flight.
fn flaky_setup_with(
    scope: &str,
    worker_file: &str,
    test: &str,
    fail_times: u32,
    noise: u32,
    hold: bool,
) -> Flaky {
    let f = setup();
    commit_in(f.repo.path(), "crates/foo/src/main.rs", "fn main() {}\n");
    commit_in(f.repo.path(), FLAKY_FILE, "// holds the flaky test\n");
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("gate.py");
    let lines = [
        "import sys",
        "counter, fail_times, test, noise = sys.argv[1], int(sys.argv[2]), sys.argv[3], int(sys.argv[4])",
        "try:",
        "    n = int(open(counter).read())",
        "except OSError:",
        "    n = 0",
        "open(counter, 'w').write(str(n + 1))",
        "hold = sys.argv[5]",
        "if hold != '-' and n == 1:",
        "    import os, time",
        "    open(hold + '.started', 'w').write('x')",
        "    for _ in range(300):",
        "        if os.path.exists(hold + '.go'):",
        "            break",
        "        time.sleep(0.1)",
        "if n >= fail_times:",
        "    print('all tests passed')",
        "    sys.exit(0)",
        "running = '     Running unittests src/main.rs (target/debug/deps/foo-0123456789abcdef.exe)'",
        "if noise == 0:",
        "    print(running)",
        "print('running 1 test')",
        "print('test ' + test + ' ... FAILED')",
        "print('')",
        "print('failures:')",
        "print('')",
        "print('---- ' + test + ' stdout ----')",
        "print('thread panicked: the reminder was late')",
        "print('')",
        "print('failures:')",
        "print('    ' + test)",
        "print('')",
        "print('test result: FAILED. 0 passed; 1 failed')",
        "sys.stdout.flush()",
        "if noise > 0:",
        "    sys.stderr.write(running + '\\n')",
        "    for i in range(noise):",
        "        sys.stderr.write('   Compiling crate%d v0.1.0\\n' % i)",
        "sys.exit(101)",
    ];
    std::fs::write(&script, lines.join("\n")).unwrap();
    let counter = dir.path().join("count");
    let hold_path = dir.path().join("hold");
    let fwd = |p: &Path| p.to_str().unwrap().replace('\\', "/");
    let hold_arg = if hold { fwd(&hold_path) } else { "-".to_string() };
    let cfg = f.repo.path().join(".factory/config.toml");
    let mut s = std::fs::read_to_string(&cfg).unwrap();
    s.push_str(&format!(
        "\n[[gate]]\nname = \"test\"\nwhen = [\"merge\"]\ncmd = ['python', '{}', '{}', '{fail_times}', '{test}', '{noise}', '{hold_arg}']\n",
        fwd(&script),
        fwd(&counter)
    ));
    std::fs::write(&cfg, s).unwrap();
    git(f.repo.path(), &["add", ".factory/config.toml"]);
    git(f.repo.path(), &["commit", "-q", "-m", "gate"]);
    let issue = f
        .repo
        .create_issue("flaky feature", &["--ready", "--scope", scope]);
    finished_run_of(&f, &issue, &[(worker_file, "x\n")]);
    Flaky {
        f,
        issue,
        counter,
        hold: hold_path,
        _dir: dir,
    }
}

/// How many times the gate has run.
fn gate_runs(fl: &Flaky) -> u32 {
    std::fs::read_to_string(&fl.counter)
        .map(|s| s.trim().parse().unwrap())
        .unwrap_or(0)
}

fn flaky_issues(f: &F) -> Vec<Issue> {
    store(f)
        .list_issues(&IssueFilter::default())
        .unwrap()
        .into_iter()
        .filter(|i| i.title.starts_with("Flaky test: "))
        .collect()
}

fn foreign_rerun_events(f: &F) -> Vec<Event> {
    store(f)
        .events_since(0, 10_000)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "merge.gate_rerun_foreign")
        .collect()
}

#[test]
fn a_gate_that_fails_on_a_test_outside_the_branch_is_rerun_once_and_filed_as_flaky() {
    let fl = flaky_setup("src/**", "src/greet.rs", FLAKY_TEST, 1);
    let f = &fl.f;
    let main_before = git_head(f);
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    assert_eq!(
        gate_runs(&fl),
        2,
        "the gate ran, failed on a foreign test and ran again"
    );
    let item = &store(f).merge_queue().unwrap()[0];
    assert_eq!(item.attempts, 1, "the rerun is not an attempt");
    assert!(item.gate_results.iter().all(|g| g.ok), "{:?}", item.gate_results);

    let issues = flaky_issues(f);
    assert_eq!(issues.len(), 1, "{issues:?}");
    let i = &issues[0];
    assert_eq!(i.title, format!("Flaky test: {FLAKY_TEST}"));
    assert_eq!((i.kind, i.priority), (IssueType::Task, 2));
    assert_eq!(i.labels, ["flaky"]);
    assert_eq!(i.file_scope, [FLAKY_FILE]);
    assert!(i.body.contains(&format!("feat/{}", fl.issue)), "{}", i.body);
    assert!(i.body.contains(&main_before), "main sha in the body: {}", i.body);
    assert!(
        i.body.contains("the reminder was late"),
        "gate tail excerpt: {}",
        i.body
    );

    let events = foreign_rerun_events(f);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].payload["tests"], serde_json::json!([FLAKY_TEST]));
    assert_eq!(events[0].payload["gate"], "test");
}

#[test]
fn an_open_issue_with_the_same_title_is_not_filed_again_but_a_closed_one_is() {
    let fl = flaky_setup("src/**", "src/greet.rs", FLAKY_TEST, 1);
    let f = &fl.f;
    let title = format!("Flaky test: {FLAKY_TEST}");
    f.repo.create_issue(&title, &[]);
    f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(flaky_issues(f).len(), 1, "the open one is enough");

    // a closed issue does not stand in the way: the next flake is a new report
    let again = flaky_setup("src/**", "src/greet.rs", FLAKY_TEST, 1);
    let g = &again.f;
    let closed = g.repo.create_issue(&title, &[]);
    g.repo.ok(&["issue", "status", &closed, "done"]);
    g.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(flaky_issues(g).len(), 2, "the closed one and the new report");
}

#[test]
fn a_rerun_that_fails_again_is_an_ordinary_failure_and_files_nothing() {
    let fl = flaky_setup("src/**", "src/greet.rs", FLAKY_TEST, 2);
    let f = &fl.f;
    lose_the_worker_of(f, &fl.issue); // so the failure ends in the BLOCKER this test reads
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "failed", "{res}");
    assert_eq!(gate_runs(&fl), 2, "one rerun, not more");
    assert_eq!(foreign_rerun_events(f).len(), 1);
    assert!(flaky_issues(f).is_empty());
    let item = &store(f).merge_queue().unwrap()[0];
    assert_eq!(item.attempts, 1);
    assert!(item.last_error.as_deref().unwrap().contains("gate `test` failed"));
    let cs = store(f).comments(&fl.issue.parse().unwrap()).unwrap();
    assert!(cs
        .iter()
        .any(|c| c.kind == CommentType::Blocker && c.body.contains("gate `test` failed")));
}

/// A failure deep in `cargo test --workspace`: the failures list is on stdout and 60 stderr lines
/// follow it, so the 40-line tail of the gate names no test. The item must still be recognised.
#[test]
fn a_foreign_failure_is_found_even_when_the_stderr_after_it_cuts_the_tail() {
    let fl = flaky_setup_noisy("src/**", "src/greet.rs", FLAKY_TEST, 1, 60);
    let f = &fl.f;
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    assert_eq!(gate_runs(&fl), 2);
    assert_eq!(flaky_issues(f).len(), 1);
    assert_eq!(foreign_rerun_events(f).len(), 1);
}

/// Runs `merge tick --once` in a child process and, while the rerun of the gate is in flight, skips
/// the item as the operator would. The rerun then answers (green when `fail_times` is 1, red when 2).
fn skip_while_the_rerun_runs(fail_times: u32) -> Flaky {
    let fl = flaky_setup_with("src/**", "src/greet.rs", FLAKY_TEST, fail_times, 0, true);
    let f = &fl.f;
    let mut cmd = f.repo.asf();
    cmd.args(["merge", "tick", "--once", "--json"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let tick = cmd.spawn().unwrap();
    let started = std::path::PathBuf::from(format!("{}.started", fl.hold.display()));
    for _ in 0..600 {
        if started.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(started.exists(), "the rerun never began");
    f.repo.ok(&["merge", "skip", &fl.issue]);
    std::fs::write(format!("{}.go", fl.hold.display()), "x").unwrap();
    let out = tick.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "tick: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(gate_runs(&fl), 2, "the gate ran and was rerun");
    fl
}

#[test]
fn an_item_skipped_while_a_green_rerun_runs_stays_skipped_and_is_not_merged() {
    let fl = skip_while_the_rerun_runs(1);
    let f = &fl.f;
    assert_eq!(item_state(f), MergeState::Skipped);
    assert!(
        !main_has(f, "src/greet.rs"),
        "the skipped branch must not reach main"
    );
}

#[test]
fn an_item_skipped_while_a_red_rerun_runs_stays_skipped_without_a_blocker() {
    let fl = skip_while_the_rerun_runs(2);
    let f = &fl.f;
    assert_eq!(item_state(f), MergeState::Skipped);
    let cs = store(f).comments(&fl.issue.parse().unwrap()).unwrap();
    assert!(
        cs.iter().all(|c| c.kind != CommentType::Blocker),
        "no BLOCKER for an item the operator skipped: {cs:?}"
    );
}

#[test]
fn a_noisy_failure_in_a_file_the_branch_changed_is_not_rerun() {
    let fl = flaky_setup_noisy("crates/**", FLAKY_FILE, FLAKY_TEST, 1, 60);
    let f = &fl.f;
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "failed", "{res}");
    assert_eq!(gate_runs(&fl), 1);
    assert!(foreign_rerun_events(f).is_empty());
}

#[test]
fn a_failing_test_in_a_file_the_branch_changed_is_not_rerun() {
    let fl = flaky_setup("crates/**", FLAKY_FILE, FLAKY_TEST, 1);
    let f = &fl.f;
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "failed", "{res}");
    assert_eq!(gate_runs(&fl), 1, "the worker's own failure is not rerun");
    assert!(foreign_rerun_events(f).is_empty());
    assert!(flaky_issues(f).is_empty());
}

#[test]
fn a_failing_test_that_maps_to_no_file_counts_as_the_branchs_own() {
    let fl = flaky_setup("src/**", "src/greet.rs", "ghost::tests::unknown", 1);
    let f = &fl.f;
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "failed", "{res}");
    assert_eq!(gate_runs(&fl), 1);
    assert!(foreign_rerun_events(f).is_empty());
    assert!(flaky_issues(f).is_empty());
}

// ---- [merge] clean_after: the clean-up after a merge -------------------------------------------

const CLEAN_DEFAULT: &str = "clean_after = [\"worktree\", \"review_worktrees\"]";

fn set_clean_after(f: &F, list: &str) {
    set_config(f, CLEAN_DEFAULT, &format!("clean_after = {list}"));
}

fn wt_dir(f: &F, id: &str) -> std::path::PathBuf {
    f.repo.path().join(".worktrees").join(id)
}

fn branch_listed(f: &F, id: &str) -> bool {
    let out = std::process::Command::new("git")
        .args(["branch", "--list", &format!("feat/{id}")])
        .current_dir(f.repo.path())
        .output()
        .unwrap();
    !String::from_utf8_lossy(&out.stdout).trim().is_empty()
}

/// `(kind, target)` of every entry of `res.cleanup.<which>`.
fn cleanup(res: &serde_json::Value, which: &str) -> Vec<(String, String)> {
    res["cleanup"][which]
        .as_array()
        .unwrap_or_else(|| panic!("no cleanup.{which} in {res}"))
        .iter()
        .map(|e| {
            (
                e["kind"].as_str().unwrap().to_string(),
                e["target"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn the_default_clean_up_removes_the_worktree_keeps_the_branch_and_reports_it() {
    let f = setup();
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    assert!(!wt_dir(&f, &f.issue).exists(), "the worktree is gone");
    assert!(branch_listed(&f, &f.issue), "the branch stays by default");
    assert_eq!(
        cleanup(&res, "cleaned"),
        [("worktree".to_string(), format!(".worktrees/{}", f.issue))],
        "{res}"
    );
    assert!(cleanup(&res, "refused").is_empty(), "{res}");
    assert_eq!(res["cleanup"]["cleaned"][0]["issue"], f.issue.as_str(), "{res}");
    let events = events_of(&f, "merge.cleaned");
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].payload["kind"], "worktree");
}

#[test]
fn an_empty_clean_after_keeps_everything_as_before() {
    let f = setup();
    set_clean_after(&f, "[]");
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    assert!(wt_dir(&f, &f.issue).is_dir() && branch_listed(&f, &f.issue));
    assert!(
        cleanup(&res, "cleaned").is_empty() && cleanup(&res, "refused").is_empty(),
        "{res}"
    );
    assert!(events_of(&f, "merge.cleaned").is_empty());
}

#[test]
fn branch_in_the_list_deletes_the_branch_after_the_worktree_is_gone() {
    let f = setup();
    set_clean_after(&f, "[\"worktree\", \"branch\", \"review_worktrees\"]");
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    assert!(!wt_dir(&f, &f.issue).exists());
    assert!(!branch_listed(&f, &f.issue), "git branch -d removed it");
    assert!(main_has(&f, "src/greet.rs"), "the work is on main");
    assert_eq!(
        cleanup(&res, "cleaned"),
        [
            ("worktree".to_string(), format!(".worktrees/{}", f.issue)),
            ("branch".to_string(), format!("feat/{}", f.issue)),
        ],
        "{res}"
    );
}

#[test]
fn branch_alone_is_refused_while_a_worktree_still_holds_it_and_the_merge_stands() {
    let f = setup();
    set_clean_after(&f, "[\"branch\"]");
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(
        res["items"][0]["state"], "merged",
        "a refusal never fails the merge: {res}"
    );
    assert!(res["items"][0]["last_error"].is_null(), "{res}");
    assert!(branch_listed(&f, &f.issue) && wt_dir(&f, &f.issue).is_dir());
    let refused = &res["cleanup"]["refused"];
    assert_eq!(refused.as_array().unwrap().len(), 1, "{res}");
    assert_eq!(refused[0]["kind"], "branch", "{res}");
    assert!(!refused[0]["reason"].as_str().unwrap().is_empty(), "{res}");
    assert_eq!(events_of(&f, "merge.clean_refused").len(), 1);
}

#[test]
fn a_dirty_worktree_is_refused_reported_and_left_for_lane_clean() {
    let f = setup();
    set_clean_after(&f, "[\"worktree\", \"branch\"]");
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    let wt = wt_dir(&f, &f.issue);
    std::fs::write(wt.join("scratch.txt"), "wip\n").unwrap();
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    assert!(
        wt.join("scratch.txt").exists(),
        "uncommitted work is never touched"
    );
    assert!(
        branch_listed(&f, &f.issue),
        "the branch cannot go while its worktree stays"
    );
    assert!(cleanup(&res, "cleaned").is_empty(), "{res}");
    let refused = res["cleanup"]["refused"].as_array().unwrap();
    assert_eq!(refused[0]["kind"], "worktree", "{res}");
    assert!(
        refused[0]["reason"].as_str().unwrap().contains("scratch.txt"),
        "{res}"
    );
    let events = events_of(&f, "merge.clean_refused");
    assert!(
        events.iter().any(|e| e.payload["kind"] == "worktree"),
        "{events:?}"
    );
}

#[test]
fn review_worktrees_go_with_the_merge_and_only_with_review_worktrees_in_the_list() {
    let f = reviewing();
    let rid = review_id(&f);
    assert!(
        wt_dir(&f, &rid).is_dir(),
        "the reviewer works in a detached worktree"
    );
    decide_as(&f, &rid, "approve");
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    assert!(!wt_dir(&f, &rid).exists(), "review worktree removed");
    assert!(!wt_dir(&f, &f.issue).exists(), "worktree removed");
    let cleaned = cleanup(&res, "cleaned");
    assert!(
        cleaned.contains(&("review_worktrees".to_string(), format!(".worktrees/{rid}"))),
        "{res}"
    );
    assert!(
        cleaned.contains(&("worktree".to_string(), format!(".worktrees/{}", f.issue))),
        "{res}"
    );

    // the config is committed before the worker starts, so main does not move while the review runs
    let f = setup();
    require_review(&f);
    set_clean_after(&f, "[\"worktree\"]");
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    f.repo.ok(&["merge", "tick", "--once"]);
    let rid = review_id(&f);
    decide_as(&f, &rid, "approve");
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    assert!(wt_dir(&f, &rid).is_dir(), "not listed, so it stays");
    assert!(!wt_dir(&f, &f.issue).exists());
}

#[test]
fn a_review_worktree_with_loose_files_stays() {
    let f = reviewing();
    let rid = review_id(&f);
    std::fs::write(wt_dir(&f, &rid).join("notes.txt"), "x\n").unwrap();
    decide_as(&f, &rid, "approve");
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    assert!(wt_dir(&f, &rid).join("notes.txt").exists());
    let refused = res["cleanup"]["refused"].as_array().unwrap();
    assert!(refused.iter().any(|e| e["kind"] == "review_worktrees"), "{res}");
}

#[test]
fn a_red_main_cleans_nothing_and_a_green_main_cleans_after_the_check() {
    let r = red_setup(true);
    let res = r.f.repo.ok(&["merge", "tick"]);
    assert_eq!(res["processed"], 1, "{res}");
    assert!(res["items"][0]["last_error"].is_string(), "main is red: {res}");
    assert!(wt_dir(&r.f, &r.f.issue).is_dir(), "a red main cleans nothing");
    assert!(cleanup(&res, "cleaned").is_empty(), "{res}");
    assert!(events_of(&r.f, "merge.cleaned").is_empty());

    let r = red_setup(true);
    std::fs::write(r.f.repo.path().join("PASS"), "").unwrap(); // the root gate passes now
    let res = r.f.repo.ok(&["merge", "tick"]);
    assert_eq!(res["processed"], 2, "{res}");
    for id in [&r.f.issue, &r.second] {
        assert!(!wt_dir(&r.f, id).exists(), "{id} cleaned after its green main");
    }
    assert_eq!(cleanup(&res, "cleaned").len(), 2, "{res}");
}

// ---- a gate failure or a rebase conflict goes back to the idle worker -----------------------------

/// Fails with a lint message once main's `README.txt` is in the worktree (so only after the rebase
/// onto the moved main), unless the worker added `src/fixed.rs`.
const LINT_FAILS: &str = "import os, sys\nif os.path.exists('README.txt') and not os.path.exists('src/fixed.rs'):\n    print('error: lint found a problem in src/greet.rs')\n    sys.exit(7)\n";

/// Hangs once main's `README.txt` is in the worktree: the gate's own `timeout_s` ends it, with no exit code.
const LINT_HANGS: &str = "import os, time\nif os.path.exists('README.txt'):\n    time.sleep(60)\n";

/// The linker cannot open a test executable that exists (and is recreated on every run): the lock
/// that `run_gates` reruns and then reports as it is.
const LINT_LOCKED: &str = "import sys\np = sys.argv[1] + '/locked.exe'\nopen(p, 'w').write('x')\nprint('LINK : fatal error LNK1104: cannot open file \"%s\"' % p)\nsys.exit(101)\n";

/// A verified item whose branch is behind a main that moved on, and a merge-stage gate `lint` (it
/// does not run in the worker's own verification) that runs `body`. `gate_fields` are more keys of the gate.
struct Lint {
    f: F,
    _dir: tempfile::TempDir,
}

fn lint_setup(body: &str, gate_fields: &str) -> Lint {
    let f = setup();
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("gate.py");
    std::fs::write(&script, body).unwrap();
    let fwd = |p: &Path| p.to_str().unwrap().replace('\\', "/");
    let cfg = f.repo.path().join(".factory/config.toml");
    let mut s = std::fs::read_to_string(&cfg).unwrap();
    s.push_str(&format!(
        "\n[[gate]]\nname = \"lint\"\nwhen = [\"merge\"]\n{gate_fields}cmd = ['python', '{}', '{}']\n",
        fwd(&script),
        fwd(dir.path())
    ));
    std::fs::write(&cfg, s).unwrap();
    git(f.repo.path(), &["add", ".factory/config.toml"]);
    git(f.repo.path(), &["commit", "-q", "-m", "gate"]);
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    commit_in(f.repo.path(), "README.txt", "main moved\n");
    Lint { f, _dir: dir }
}

/// `[merge] gate_max_rounds = n`, committed so main stays clean.
fn gate_rounds(f: &F, n: u32) {
    set_config(
        f,
        "[merge]\ntarget = \"local\"",
        &format!("[merge]\ngate_max_rounds = {n}\ntarget = \"local\""),
    );
}

/// Prompts the worker got that contain `needle`, oldest first.
fn prompts_with(f: &F, needle: &str) -> Vec<String> {
    FakeRuntime::new(f.repo.path().join(".factory/fake-runtime.json"))
        .log()
        .into_iter()
        .filter(|e| e.op == "prompt")
        .filter_map(|e| e.args["text"].as_str().map(str::to_string))
        .filter(|t| t.contains(needle))
        .collect()
}

/// `.factory/runs/<id>/<name>` in the worker's worktree.
fn run_file(f: &F, name: &str) -> std::path::PathBuf {
    f.repo
        .path()
        .join(".worktrees")
        .join(&f.issue)
        .join(".factory/runs")
        .join(&f.issue)
        .join(name)
}

/// STATUS comments of the orchestrator addressed to everyone.
fn orchestrator_status(f: &F) -> Vec<String> {
    comments_of(f, CommentType::Status)
        .into_iter()
        .filter(|c| c.author == "orchestrator" && c.to_lane == "all")
        .map(|c| c.body)
        .collect()
}

#[test]
fn a_gate_that_fails_only_after_the_rebase_goes_back_to_the_idle_worker_instead_of_a_blocker() {
    let l = lint_setup(LINT_FAILS, "");
    let f = &l.f;
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "failed", "{res}");

    // the gate tail and a short explanation are in a file the worker reads: data, then what to do
    let text = std::fs::read_to_string(run_file(f, "gate-1.md")).expect("gate-1.md is written");
    assert!(!text.contains('\r'), "LF line endings:\n{text}");
    assert!(
        text.contains("error: lint found a problem in src/greet.rs"),
        "the gate tail is quoted:\n{text}"
    );
    assert!(
        text.contains("after the rebase onto") && text.contains("the gate `lint` failed"),
        "{text}"
    );
    assert!(text.contains("Fix it on what is checked out"), "{text}");
    assert!(text.contains("data"), "says the evidence is data:\n{text}");
    assert!(
        text.contains("issue finish") && text.contains(&f.issue),
        "tells the worker to finish again:\n{text}"
    );

    // one line that points at the file, the tail is not inline
    let prompts = prompts_with(f, "gate-");
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(!prompts[0].contains('\n'), "one line: {:?}", prompts[0]);
    assert!(
        prompts[0].contains(&format!(".factory/runs/{}/gate-1.md", f.issue)),
        "{prompts:?}"
    );
    assert!(!prompts[0].contains("lint found a problem"), "{prompts:?}");

    // no BLOCKER, a STATUS for everyone, the run reopened so the item leaves NEEDS YOU
    assert!(
        comments_of(f, CommentType::Blocker).is_empty(),
        "{:?}",
        comments_of(f, CommentType::Blocker)
    );
    let status = orchestrator_status(f);
    assert_eq!(status.len(), 1, "{status:?}");
    assert!(
        status[0].contains("the gate `lint` failed") && status[0].contains("sent to the worker"),
        "{status:?}"
    );
    assert_eq!(status_of(f, &f.issue), "in-progress");
    assert_eq!(worker_run(f).state, RunState::Working);
    assert_eq!(running(f), (1, 0), "the worker holds its slot again");
    let item = &store(f).merge_queue().unwrap()[0];
    assert!(
        item.last_error.as_deref().unwrap().contains("gate `lint` failed"),
        "{item:?}"
    );
    assert!(!main_has(f, "src/greet.rs"));
    // the failed item is left alone until the worker is done, and a retry by hand is refused
    assert_eq!(f.repo.ok(&["merge", "tick", "--once"])["processed"], 0);
    let (code, error, _) = f.repo.err(&["merge", "retry", &f.issue]);
    assert_eq!((code, error.as_str()), (3, "conflict"));

    // the worker fixes it and finishes again: the normal verify -> re-queue path runs
    let res = finish_again(f, "src/fixed.rs", "\n");
    assert_eq!(res["transitions"][0]["to"], "succeeded", "{res}");
    assert_eq!(store(f).merge_queue().unwrap()[0].state, MergeState::Queued);
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    assert!(main_has(f, "src/fixed.rs"));
}

#[test]
fn a_gate_that_timed_out_is_not_sent_to_the_worker() {
    let l = lint_setup(LINT_HANGS, "timeout_s = 1\n");
    let f = &l.f;
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "failed", "{res}");
    let item = &store(f).merge_queue().unwrap()[0];
    assert_eq!(
        item.gate_results.iter().find(|g| !g.ok).map(|g| g.exit_code),
        Some(None),
        "a timeout has no exit code: {item:?}"
    );
    assert!(prompts_with(f, "gate-").is_empty());
    assert!(!run_file(f, "gate-1.md").exists());
    assert_eq!(
        worker_run(f).state,
        RunState::Succeeded,
        "the run is left as it was"
    );
    let blockers = comments_of(f, CommentType::Blocker);
    assert_eq!(blockers.len(), 1, "{blockers:?}");
    assert!(
        blockers[0].body.contains(&format!("asf merge retry {}", f.issue)),
        "{}",
        blockers[0].body
    );
    assert!(
        !blockers[0].body.contains("not sent back"),
        "it was never a candidate: {}",
        blockers[0].body
    );
}

#[test]
fn a_gate_that_only_hit_a_linker_lock_is_not_sent_to_the_worker() {
    // the gate is rerun on the lock first (about twenty seconds of pauses), then reported as it is
    let l = lint_setup(LINT_LOCKED, "");
    let f = &l.f;
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "failed", "{res}");
    let item = &store(f).merge_queue().unwrap()[0];
    let bad = item.gate_results.iter().find(|g| !g.ok).expect("a failed gate");
    assert!(bad.tail.contains("LNK1104"), "{bad:?}");
    assert!(prompts_with(f, "gate-").is_empty());
    assert!(!run_file(f, "gate-1.md").exists());
    assert_eq!(worker_run(f).state, RunState::Succeeded);
    let blockers = comments_of(f, CommentType::Blocker);
    assert_eq!(blockers.len(), 1, "{blockers:?}");
    assert!(blockers[0].body.contains("LNK1104"), "{}", blockers[0].body);
    assert!(
        blockers[0].body.contains(&format!("asf merge retry {}", f.issue)),
        "{}",
        blockers[0].body
    );
}

#[test]
fn a_gate_failure_with_an_unreachable_worker_keeps_the_blocker_and_the_retry_hint() {
    let l = lint_setup(LINT_FAILS, "");
    let f = &l.f;
    lose_the_worker(f);
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "failed", "{res}");
    assert!(prompts_with(f, "gate-").is_empty(), "nothing could be delivered");
    assert_eq!(status_of(f, &f.issue), "validating");
    assert_eq!(worker_run(f).state, RunState::Succeeded);
    let blockers = comments_of(f, CommentType::Blocker);
    assert_eq!(blockers.len(), 1, "{blockers:?}");
    assert_eq!(blockers[0].to_lane, "human");
    assert!(
        blockers[0].body.contains("gate `lint` failed") && blockers[0].body.contains("lint found a problem"),
        "{}",
        blockers[0].body
    );
    assert!(
        blockers[0].body.contains("The worker was not sent back"),
        "{}",
        blockers[0].body
    );
    assert!(
        blockers[0].body.contains(&format!("asf merge retry {}", f.issue)),
        "{}",
        blockers[0].body
    );
}

#[test]
fn after_gate_max_rounds_send_backs_the_item_stays_failed_with_a_blocker_that_names_the_rounds() {
    let l = lint_setup(LINT_FAILS, "");
    let f = &l.f;
    gate_rounds(f, 1);
    f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(
        prompts_with(f, "gate-").len(),
        1,
        "round 1 goes back to the worker"
    );

    // the worker finishes without fixing the gate: the next failure is the one that asks the human
    let res = finish_again(f, "src/other.rs", "\n");
    assert_eq!(res["transitions"][0]["to"], "succeeded", "{res}");
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "failed", "{res}");
    assert_eq!(
        prompts_with(f, "gate-").len(),
        1,
        "nothing is sent for the last round"
    );
    assert!(!run_file(f, "gate-2.md").exists(), "no gate-2.md");
    assert_eq!(worker_run(f).state, RunState::Succeeded);
    let last = comments_of(f, CommentType::Blocker).pop().expect("a BLOCKER");
    assert_eq!(last.to_lane, "human");
    assert!(last.body.contains("gate `lint` failed"), "{}", last.body);
    assert!(
        last.body.contains("sent back 1 time(s)") && last.body.contains("gate.max_rounds is 1"),
        "names the rounds: {}",
        last.body
    );
    assert!(
        last.body.contains(&format!("asf merge retry {}", f.issue)),
        "{}",
        last.body
    );
}

#[test]
fn a_gate_max_rounds_of_zero_never_sends_back() {
    let l = lint_setup(LINT_FAILS, "");
    let f = &l.f;
    gate_rounds(f, 0);
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "failed", "{res}");
    assert!(prompts_with(f, "gate-").is_empty());
    let blockers = comments_of(f, CommentType::Blocker);
    assert_eq!(blockers.len(), 1, "{blockers:?}");
    assert!(
        blockers[0].body.contains("sent back 0 time(s)") && blockers[0].body.contains("gate.max_rounds is 0"),
        "{}",
        blockers[0].body
    );
}

#[test]
fn a_rebase_conflict_goes_to_the_idle_worker_and_the_queue_resolves_nothing() {
    let f = setup();
    finished_run(&f, &[("src/lib.rs", "pub fn a() { /* worker */ }\n")]);
    commit_in(f.repo.path(), "src/lib.rs", "pub fn a() { /* main */ }\n");
    let wt = f.repo.path().join(".worktrees").join(&f.issue);
    let before = git_out(&wt, &["rev-parse", "HEAD"]);
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "failed", "{res}");

    // the worktree is clean and the branch untouched: the queue aborted the rebase, it did not resolve it
    assert_eq!(git_out(&wt, &["rev-parse", "HEAD"]), before);
    assert_eq!(git_out(&wt, &["status", "--porcelain"]), "");
    let text = std::fs::read_to_string(run_file(&f, "conflict-1.md")).expect("conflict-1.md is written");
    assert!(text.contains("src/lib.rs"), "names the conflicting file:\n{text}");
    assert!(
        text.contains("Rebase onto main") && text.contains("intent of both sides"),
        "{text}"
    );
    assert!(text.contains("issue finish"), "{text}");
    let prompts = prompts_with(&f, "conflict-");
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(!prompts[0].contains('\n'), "one line: {:?}", prompts[0]);
    assert!(
        prompts[0].contains(&format!(".factory/runs/{}/conflict-1.md", f.issue)),
        "{prompts:?}"
    );
    assert!(events_of(&f, "merge.sent_back").is_empty());
    assert_eq!(events_of(&f, "run.fix_sent_back")[0].payload["kind"], "conflict");
    assert!(comments_of(&f, CommentType::Blocker).is_empty());
    let status = orchestrator_status(&f);
    assert_eq!(status.len(), 1, "{status:?}");
    assert!(
        status[0].contains("conflicts") && status[0].contains("sent to the worker"),
        "{status:?}"
    );
    assert_eq!(status_of(&f, &f.issue), "in-progress");
    assert_eq!(worker_run(&f).state, RunState::Working);

    // the worker resolves it (the branch's side wins here), finishes, and the queue merges the result
    git(&wt, &["rebase", "-X", "theirs", "main"]);
    let res = finish_again(&f, "src/extra.rs", "\n");
    assert_eq!(res["transitions"][0]["to"], "succeeded", "{res}");
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    assert!(main_has(&f, "src/extra.rs"));
}

#[test]
fn a_rebase_conflict_uses_the_same_rounds_as_the_gate_failures() {
    let f = setup();
    gate_rounds(&f, 1);
    finished_run(&f, &[("src/lib.rs", "pub fn a() { /* worker */ }\n")]);
    commit_in(f.repo.path(), "src/lib.rs", "pub fn a() { /* main */ }\n");
    f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(prompts_with(&f, "conflict-").len(), 1);

    // finished again without resolving the conflict: the second one is the human's
    let res = finish_again(&f, "src/extra.rs", "\n");
    assert_eq!(res["transitions"][0]["to"], "succeeded", "{res}");
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "blocked", "{res}");
    assert_eq!(prompts_with(&f, "conflict-").len(), 1);
    assert!(!run_file(&f, "conflict-2.md").exists());
    assert_eq!(status_of(&f, &f.issue), "blocked");
    let last = comments_of(&f, CommentType::Blocker).pop().expect("a BLOCKER");
    assert!(
        last.body.contains("src/lib.rs") && last.body.contains("sent back 1 time(s)"),
        "{}",
        last.body
    );
}

#[test]
fn a_rebase_conflict_with_an_unreachable_worker_blocks_as_before() {
    let f = setup();
    finished_run(&f, &[("src/lib.rs", "pub fn a() { /* worker */ }\n")]);
    lose_the_worker(&f);
    commit_in(f.repo.path(), "src/lib.rs", "pub fn a() { /* main */ }\n");
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "blocked", "{res}");
    assert!(prompts_with(&f, "conflict-").is_empty());
    assert_eq!(status_of(&f, &f.issue), "blocked");
    let blockers = comments_of(&f, CommentType::Blocker);
    assert_eq!(blockers.len(), 1, "{blockers:?}");
    assert!(
        blockers[0].body.contains("The worker was not sent back")
            && blockers[0].body.contains(&format!("asf merge retry {}", f.issue)),
        "{}",
        blockers[0].body
    );
}

/// The TUI Log shows `merge.sent_back` as "review verdict: changes", so a gate failure or a rebase
/// conflict sent back to the worker is recorded under its own event kind.
#[test]
fn a_gate_or_conflict_send_back_is_not_recorded_as_a_review_verdict() {
    let l = lint_setup(LINT_FAILS, "");
    let f = &l.f;
    f.repo.ok(&["merge", "tick", "--once"]);
    assert!(events_of(f, "merge.sent_back").is_empty());
    let sent = events_of(f, "run.fix_sent_back");
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].payload["kind"], "gate", "{:?}", sent[0].payload);
    assert_eq!(sent[0].payload["round"], 1);
    assert!(sent[0].payload["file"].as_str().unwrap().ends_with("gate-1.md"));
    assert!(sent[0].payload["head"].is_string());

    // the worker's own verification goes through the same path
    let g = setup();
    verify_gate(&g);
    g.repo.ok(&["run", "start", &g.issue]);
    finish_and_watch(&g);
    assert!(events_of(&g, "merge.sent_back").is_empty());
    assert_eq!(events_of(&g, "run.fix_sent_back").len(), 1);
}

/// A gate with no `when`: it runs in the worker's own verification too, and fails until `src/fixed.rs` exists.
fn verify_gate(f: &F) {
    let cfg = f.repo.path().join(".factory/config.toml");
    let mut s = std::fs::read_to_string(&cfg).unwrap();
    s.push_str(
        "\n[[gate]]\nname = \"needs-fix\"\ncmd = [\"python\", \"-c\", \"import os, sys; print('fix missing'); sys.exit(0 if os.path.exists('src/fixed.rs') else 2)\"]\n",
    );
    std::fs::write(&cfg, s).unwrap();
    git(f.repo.path(), &["add", ".factory/config.toml"]);
    git(f.repo.path(), &["commit", "-q", "-m", "gate"]);
}

/// The worker commits `src/greet.rs`, writes its report and finishes; one `run watch` verifies it.
fn finish_and_watch(f: &F) -> serde_json::Value {
    let wt = f.repo.path().join(".worktrees").join(&f.issue);
    commit_in(&wt, "src/greet.rs", "pub fn hello() {}\n");
    let report = run_file(f, "report.md");
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
    f.repo.ok(&["run", "watch", "--once"])
}

#[test]
fn a_gate_that_fails_in_the_workers_own_verification_goes_back_the_same_way() {
    let f = setup();
    verify_gate(&f);
    f.repo.ok(&["run", "start", &f.issue]);
    let res = finish_and_watch(&f);
    assert_eq!(res["transitions"][0]["to"], "working", "{res}");

    let text = std::fs::read_to_string(run_file(&f, "gate-1.md")).expect("gate-1.md is written");
    assert!(
        text.contains("fix missing") && text.contains("in your verification") && text.contains("`needs-fix`"),
        "{text}"
    );
    let prompts = prompts_with(&f, "gate-");
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(!prompts[0].contains('\n'), "one line: {:?}", prompts[0]);
    assert!(prompts[0].contains("gate-1.md"), "{prompts:?}");
    assert!(
        comments_of(&f, CommentType::Blocker).is_empty(),
        "{:?}",
        comments_of(&f, CommentType::Blocker)
    );
    let status = orchestrator_status(&f);
    assert_eq!(status.len(), 1, "{status:?}");
    assert!(
        status[0].contains("the gate `needs-fix` failed") && status[0].contains("sent to the worker"),
        "{status:?}"
    );
    assert_eq!(status_of(&f, &f.issue), "in-progress");
    assert_eq!(worker_run(&f).state, RunState::Working);
    assert!(store(&f).merge_queue().unwrap().is_empty());

    // fixed and finished again: verified and queued
    let res = finish_again(&f, "src/fixed.rs", "\n");
    assert_eq!(res["transitions"][0]["to"], "succeeded", "{res}");
    assert_eq!(status_of(&f, &f.issue), "validating");
    assert_eq!(store(&f).merge_queue().unwrap()[0].state, MergeState::Queued);
}

#[test]
fn a_verification_failure_beyond_the_rounds_or_with_a_lost_worker_keeps_the_blocker() {
    // no round left
    let f = setup();
    verify_gate(&f);
    gate_rounds(&f, 0);
    f.repo.ok(&["run", "start", &f.issue]);
    let res = finish_and_watch(&f);
    assert_eq!(res["transitions"][0]["to"], "failed", "{res}");
    assert!(prompts_with(&f, "gate-").is_empty());
    assert_eq!(status_of(&f, &f.issue), "blocked");
    let last = comments_of(&f, CommentType::Blocker).pop().expect("a BLOCKER");
    assert!(
        last.body.contains("gate `needs-fix` failed") && last.body.contains("gate.max_rounds is 0"),
        "{}",
        last.body
    );

    // a worker that cannot be reached
    let g = setup();
    verify_gate(&g);
    g.repo.ok(&["run", "start", &g.issue]);
    lose_the_worker(&g);
    let res = finish_and_watch(&g);
    assert_eq!(res["transitions"][0]["to"], "failed", "{res}");
    assert_eq!(status_of(&g, &g.issue), "blocked");
    let last = comments_of(&g, CommentType::Blocker).pop().expect("a BLOCKER");
    assert!(
        last.body.contains("The worker was not sent back"),
        "{}",
        last.body
    );
}

#[test]
fn a_failed_gate_next_to_other_verification_problems_stays_a_blocker() {
    // the report has open questions: that is the worker's to answer through QUESTION comments, not a
    // gate to fix, so the whole failure goes to the human as before
    let f = setup();
    verify_gate(&f);
    f.repo.ok(&["run", "start", &f.issue]);
    let wt = f.repo.path().join(".worktrees").join(&f.issue);
    commit_in(&wt, "src/greet.rs", "pub fn hello() {}\n");
    let report = run_file(&f, "report.md");
    std::fs::write(&report, "# done\n\n## Open questions\n\n- should it print?\n").unwrap();
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
    assert_eq!(res["transitions"][0]["to"], "failed", "{res}");
    assert!(prompts_with(&f, "gate-").is_empty());
    assert_eq!(status_of(&f, &f.issue), "blocked");
}

// ---- a sent-back worker that committed its fix is queued again without a finish --------------------

/// Scripts the next `info` answer of the fake runtime: the agent reports `state`.
fn agent_reports(f: &F, state: &str) {
    FakeRuntime::new(f.repo.path().join(".factory/fake-runtime.json"))
        .script(serde_json::json!({ "info": [{ "state": state }] }))
        .unwrap();
}

#[test]
fn an_idle_worker_with_a_fix_committed_after_a_send_back_is_verified_and_queued_without_a_finish() {
    let l = lint_setup(LINT_FAILS, "");
    let f = &l.f;
    f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(worker_run(f).state, RunState::Working);
    let wt = f.repo.path().join(".worktrees").join(&f.issue);
    let watch = |state: &str| {
        agent_reports(f, state);
        f.repo.ok(&["run", "watch", "--once"])
    };

    // idle but nothing was committed since it was sent back: it may still be thinking
    assert_eq!(watch("idle")["transitions"].as_array().unwrap().len(), 0);
    // a fix is committed but the agent is still working on it
    commit_in(&wt, "src/fixed.rs", "\n");
    assert_eq!(watch("working")["transitions"].as_array().unwrap().len(), 0);
    // idle with loose changes in the worktree: not done
    std::fs::write(wt.join("src/loose.rs"), "\n").unwrap();
    assert_eq!(watch("idle")["transitions"].as_array().unwrap().len(), 0);
    assert_eq!(status_of(f, &f.issue), "in-progress");
    std::fs::remove_file(wt.join("src/loose.rs")).unwrap();

    // idle, a new commit, a clean tree: that is a finish in everything but the command
    let res = watch("idle");
    assert_eq!(res["transitions"][0]["to"], "succeeded", "{res}");
    assert_eq!(status_of(f, &f.issue), "validating");
    assert_eq!(worker_run(f).state, RunState::Succeeded);
    let queue = store(f).merge_queue().unwrap();
    assert_eq!(queue.len(), 1, "the item is reused: {queue:?}");
    assert_eq!(queue[0].state, MergeState::Queued, "{queue:?}");
    let note: Vec<String> = orchestrator_status(f)
        .into_iter()
        .filter(|s| s.contains("without `asf issue finish`"))
        .collect();
    assert_eq!(note.len(), 1, "{note:?}");
    assert_eq!(events_of(f, "run.auto_finished").len(), 1);

    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
}

#[test]
fn a_sent_back_worker_with_an_open_question_is_not_finished_for_it() {
    // `asf issue finish` refuses while a QUESTION to the human is open; the automatic finish must too,
    // or the branch could merge with the question unanswered
    let l = lint_setup(LINT_FAILS, "");
    let f = &l.f;
    f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(worker_run(f).state, RunState::Working);
    let wt = f.repo.path().join(".worktrees").join(&f.issue);
    commit_in(&wt, "src/fixed.rs", "\n");
    let q = f.repo.ok(&[
        "--lane",
        &f.issue,
        "issue",
        "comment",
        &f.issue,
        "--type",
        "QUESTION",
        "--to",
        "human",
        "--body",
        "keep the old name?",
    ]);
    agent_reports(f, "idle");
    let res = f.repo.ok(&["run", "watch", "--once"]);
    assert_eq!(res["transitions"].as_array().unwrap().len(), 0, "{res}");
    assert_eq!(status_of(f, &f.issue), "in-progress");
    assert_eq!(worker_run(f).state, RunState::Working);
    assert!(events_of(f, "run.auto_finished").is_empty());

    // answered: the next tick finishes it
    f.repo.ok(&[
        "issue",
        "comment",
        &f.issue,
        "--type",
        "ANSWER",
        "--to",
        &f.issue,
        "--reply-to",
        &q["id"].as_i64().unwrap().to_string(),
        "--body",
        "yes",
    ]);
    agent_reports(f, "idle");
    let res = f.repo.ok(&["run", "watch", "--once"]);
    assert_eq!(res["transitions"][0]["to"], "succeeded", "{res}");
}

#[test]
fn a_worker_that_was_never_sent_back_is_not_finished_for_it() {
    let f = setup();
    f.repo.ok(&["run", "start", &f.issue]);
    let wt = f.repo.path().join(".worktrees").join(&f.issue);
    commit_in(&wt, "src/greet.rs", "pub fn hello() {}\n");
    agent_reports(&f, "idle");
    let res = f.repo.ok(&["run", "watch", "--once"]);
    assert_eq!(res["transitions"].as_array().unwrap().len(), 0, "{res}");
    assert_eq!(status_of(&f, &f.issue), "in-progress");
    assert!(events_of(&f, "run.auto_finished").is_empty());
}

// ---- [merge] reuse_verification: the queue does not gate a tree that the verification already gated ----

/// A gate named `counted` that appends one line to a log for every run, in every stage. Committed to
/// main, so a worker started afterwards carries it. Returns the log's directory (keep it alive) and the log.
fn counted_gate(f: &F) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("gate.py");
    std::fs::write(
        &script,
        "import os, sys\nopen(sys.argv[1], 'a').write(os.getcwd() + '\\n')\n",
    )
    .unwrap();
    let log = dir.path().join("gate.log");
    let fwd = |p: &Path| p.to_str().unwrap().replace('\\', "/");
    let cfg = f.repo.path().join(".factory/config.toml");
    let mut s = std::fs::read_to_string(&cfg).unwrap();
    s.push_str(&format!(
        "\n[[gate]]\nname = \"counted\"\ncmd = ['python', '{}', '{}']\n",
        fwd(&script),
        fwd(&log)
    ));
    std::fs::write(&cfg, s).unwrap();
    git(f.repo.path(), &["add", ".factory/config.toml"]);
    git(f.repo.path(), &["commit", "-q", "-m", "counted gate"]);
    (dir, log)
}

fn counted_runs(log: &Path) -> usize {
    std::fs::read_to_string(log).unwrap_or_default().lines().count()
}

fn worktree_head(f: &F) -> String {
    git_out(
        &f.repo.path().join(".worktrees").join(&f.issue),
        &["rev-parse", "HEAD"],
    )
}

#[test]
fn a_verified_item_whose_rebase_changed_nothing_skips_the_queue_gates() {
    let f = setup();
    let (_dir, log) = counted_gate(&f);
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    assert_eq!(counted_runs(&log), 1, "the verification ran the gate");
    let verified = events_of(&f, "run.verified");
    assert_eq!(verified.len(), 1, "{verified:?}");
    assert_eq!(verified[0].payload["head"], worktree_head(&f).as_str());

    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    assert_eq!(counted_runs(&log), 1, "the queue did not run the gate again");

    let item = &store(&f).merge_queue().unwrap()[0];
    let recorded: Vec<(&str, bool)> = item
        .gate_results
        .iter()
        .map(|g| (g.name.as_str(), g.ok))
        .collect();
    assert_eq!(
        recorded,
        [("counted", true)],
        "the verification's results are the item's"
    );
    let reused = events_of(&f, "merge.gates_reused");
    assert_eq!(reused.len(), 1, "{reused:?}");
    assert_eq!(reused[0].payload["merge_id"], item.id);
    assert_eq!(reused[0].payload["attempt"], item.attempts);
    assert_eq!(reused[0].payload["head"], verified[0].payload["head"]);
}

#[test]
fn a_real_rebase_runs_the_queue_gates_as_before() {
    let f = setup();
    let (_dir, log) = counted_gate(&f);
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    commit_in(f.repo.path(), "README.txt", "main moved\n");
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    assert_eq!(counted_runs(&log), 2, "verification and queue each ran the gate");
    assert!(events_of(&f, "merge.gates_reused").is_empty());
    assert_eq!(store(&f).merge_queue().unwrap()[0].gate_results.len(), 1);
}

#[test]
fn a_commit_after_verification_runs_the_queue_gates() {
    let f = setup();
    let (_dir, log) = counted_gate(&f);
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    let wt = f.repo.path().join(".worktrees").join(&f.issue);
    commit_in(&wt, "src/more.rs", "pub fn more() {}\n");
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    assert_eq!(counted_runs(&log), 2, "the unverified commit is gated");
    assert!(events_of(&f, "merge.gates_reused").is_empty());
}

#[test]
fn with_reuse_verification_off_the_queue_gates_every_item_as_before() {
    let f = setup();
    set_config(&f, "reuse_verification = true", "reuse_verification = false");
    let (_dir, log) = counted_gate(&f);
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    assert_eq!(counted_runs(&log), 2, "verification and queue each ran the gate");
    assert!(events_of(&f, "merge.gates_reused").is_empty());
}

#[test]
fn reused_gates_go_on_to_the_review_as_usual() {
    let f = setup();
    require_review(&f);
    let (_dir, log) = counted_gate(&f);
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "reviewing", "{res}");
    assert_eq!(counted_runs(&log), 1, "no queue gate run before the review");
    assert_eq!(events_of(&f, "merge.gates_reused").len(), 1);
    assert_eq!(reviews_of(&f, &f.issue).len(), 1, "the reviewer was started");
    assert_eq!(store(&f).merge_queue().unwrap()[0].gate_results.len(), 1);
}
