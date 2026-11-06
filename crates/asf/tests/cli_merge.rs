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
    let mut s = std::fs::read_to_string(&cfg).unwrap();
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
        f.repo.path().join(".worktrees").join(&rid).is_dir(),
        "the review worktree is left for lane clean"
    );
}

#[test]
fn a_changes_decision_fails_the_item_blocks_the_issue_and_posts_a_blocker() {
    let f = reviewing();
    let rid = review_id(&f);
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
