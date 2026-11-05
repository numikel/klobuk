//! `[merge] target = "github"` with the fake GitHub and a local bare `origin`.

mod common;

use std::path::{Path, PathBuf};

use asf_core::github::GitHub;
use asf_core::*;
use asf_store::SqliteStore;
use asf_vcs::FakeGitHub;
use common::{git, Repo};

struct F {
    repo: Repo,
    bare: PathBuf,
    issue: String,
    _remote_dir: tempfile::TempDir,
}

fn git_out(cwd: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn setup() -> F {
    let repo = Repo::initialised();
    let cfg = repo.path().join(".factory/config.toml");
    let mut s = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("runtime = \"herdr\"", "runtime = \"fake\"")
        .replace("target = \"local\"", "target = \"github\"");
    s.push_str("\n[merge.github]\nrunner = \"fake\"\n");
    std::fs::write(&cfg, s).unwrap();
    std::fs::create_dir_all(repo.path().join("src")).unwrap();
    std::fs::write(repo.path().join("src/lib.rs"), "pub fn a() {}\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "files"]);
    let remote_dir = tempfile::tempdir().unwrap();
    let bare = remote_dir.path().join("origin.git");
    git_out(
        remote_dir.path(),
        &["init", "-q", "--bare", "-b", "main", "origin.git"],
    );
    git(repo.path(), &["remote", "add", "origin", bare.to_str().unwrap()]);
    git(repo.path(), &["push", "-q", "-u", "origin", "main"]);
    let issue = repo.create_issue("feature", &["--ready", "--scope", "src/**"]);
    F {
        repo,
        bare,
        issue,
        _remote_dir: remote_dir,
    }
}

fn store(f: &F) -> SqliteStore {
    SqliteStore::open(&f.repo.path().join(".factory/factory.db")).unwrap()
}

fn gh(f: &F) -> FakeGitHub {
    FakeGitHub::new(f.repo.path().join(".factory/fake-github.json"))
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

fn blockers(f: &F) -> Vec<String> {
    store(f)
        .comments(&f.issue.parse().unwrap())
        .unwrap()
        .into_iter()
        .filter(|c| c.kind == CommentType::Blocker)
        .map(|c| c.body)
        .collect()
}

#[test]
fn github_target_opens_a_pr_waits_for_checks_then_merges() {
    let f = setup();
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);

    // tick 1: rebase, gates, push, PR
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "pr_open", "{res}");
    let url = res["items"][0]["pr_url"].as_str().unwrap().to_string();
    assert!(url.ends_with("/pull/1"), "{url}");
    assert!(git_out(&f.bare, &["branch", "--list"]).contains(&format!("feat/{}", f.issue)));
    assert_eq!(
        f.repo.ok(&["issue", "show", &f.issue])["issue"]["status"],
        "in-review"
    );
    let cs = store(&f).comments(&f.issue.parse().unwrap()).unwrap();
    assert!(
        cs.iter()
            .any(|c| c.kind == CommentType::Fyi && c.body.contains(&url)),
        "{cs:?}"
    );

    // tick 2: checks pending → nothing changes, nothing reported
    gh(&f).script(serde_json::json!({ "checks": "pending" })).unwrap();
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["processed"], 0, "{res}");
    assert_eq!(f.repo.ok(&["merge", "list"])[0]["state"], "pr_open");

    // tick 3: checks passed → merge PR, pull main, close issue
    gh(&f).script(serde_json::json!({ "checks": "passed" })).unwrap();
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    assert_eq!(f.repo.ok(&["issue", "show", &f.issue])["issue"]["status"], "done");
    assert!(
        f.repo.path().join("src/greet.rs").exists(),
        "main checkout pulled the merge"
    );
    assert_eq!(
        git_out(&f.bare, &["rev-parse", "main"]),
        git_out(f.repo.path(), &["rev-parse", "main"])
    );
    let log = gh(&f).log();
    let merge = log.iter().find(|e| e.op == "pr_merge").expect("pr_merge called");
    assert_eq!(merge.args["method"], "merge", "default keeps worker commits");
    assert_eq!(merge.args["delete_branch"], false);
    assert!(store(&f).active_claims().unwrap().is_empty());
    // nothing left
    assert_eq!(f.repo.ok(&["merge", "tick", "--once"])["processed"], 0);
}

#[test]
fn verify_main_after_runs_the_gates_in_the_root_after_pull_ff_and_halts_the_tick() {
    let f = setup();
    // a gate that passes only where a `PASS` file exists: every worktree has it, the repo root does not
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("gate.py");
    std::fs::write(
        &script,
        "import os, sys\n\
         if os.path.exists('PASS'):\n    sys.exit(0)\n\
         print('PASS marker missing in', os.getcwd())\n\
         sys.exit(7)\n",
    )
    .unwrap();
    let cfg = f.repo.path().join(".factory/config.toml");
    let mut s = std::fs::read_to_string(&cfg).unwrap();
    assert!(
        s.contains("verify_main_after = false"),
        "template carries the flag"
    );
    s = s.replace("verify_main_after = false", "verify_main_after = true");
    s.push_str(&format!(
        "\n[[gate]]\nname = \"rootonly\"\ncmd = ['python', '{}']\n",
        script.to_str().unwrap().replace('\\', "/")
    ));
    std::fs::write(&cfg, s).unwrap();
    git(f.repo.path(), &["add", ".factory/config.toml"]);
    git(f.repo.path(), &["commit", "-q", "-m", "gate"]);
    git(f.repo.path(), &["push", "-q", "origin", "main"]);
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

    // ticks 1 and 2 (once): each queued item gets its PR
    for _ in 0..2 {
        let res = f.repo.ok(&["merge", "tick", "--once"]);
        assert_eq!(res["items"][0]["state"], "pr_open", "{res}");
    }
    assert!(store(&f)
        .merge_queue()
        .unwrap()
        .iter()
        .all(|m| m.state == MergeState::PrOpen));

    // tick 3: both PRs have passing checks; the first is merged, pulled and verified in the root, which is red
    gh(&f).script(serde_json::json!({ "checks": "passed" })).unwrap();
    let res = f.repo.ok(&["merge", "tick"]);
    assert_eq!(res["processed"], 1, "the tick stops at the red main: {res}");
    assert_eq!(res["items"][0]["issue_id"], f.issue, "{res}");
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    assert_eq!(
        res["items"][0]["last_error"], "main red after merge: gate `rootonly` failed (exit 7)",
        "{res}"
    );
    assert!(
        f.repo.path().join("src/greet.rs").exists(),
        "pull_ff landed; no automatic revert"
    );

    // the red main halted the tick: the other PR was neither polled nor merged
    let merges = gh(&f).log().into_iter().filter(|e| e.op == "pr_merge").count();
    assert_eq!(merges, 1, "only the first PR was merged");
    let q = store(&f).merge_queue().unwrap();
    assert_eq!(q[1].state, MergeState::PrOpen);
    assert_eq!(q[1].last_error, None);

    let events = store(&f).events_since(0, 10_000).unwrap();
    let red: Vec<_> = events.iter().filter(|e| e.kind == "merge.main_red").collect();
    assert_eq!(red.len(), 1, "{events:?}");
    assert_eq!(red[0].payload["gate"], "rootonly");
    let head = git_out(f.repo.path(), &["rev-parse", "main"]);
    assert_eq!(red[0].payload["head"], head.as_str());
    let b = blockers(&f);
    assert_eq!(b.len(), 1, "{b:?}");
    assert!(
        b[0].contains(&head) && b[0].contains("PASS marker missing"),
        "{b:?}"
    );
    assert_eq!(f.repo.ok(&["issue", "show", &f.issue])["issue"]["status"], "done");
}

#[test]
fn github_target_fails_on_failed_checks_and_retry_reuses_the_open_pr() {
    let f = setup();
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    f.repo.ok(&["merge", "tick", "--once"]);
    gh(&f).script(serde_json::json!({ "checks": "failed" })).unwrap();
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "failed", "{res}");
    let b = blockers(&f);
    assert!(
        b.iter().any(|m| m.contains("/pull/1") && m.contains("checks")),
        "{b:?}"
    );

    f.repo.ok(&["merge", "retry", &f.issue]);
    gh(&f).script(serde_json::json!({ "checks": "passed" })).unwrap();
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "pr_open", "{res}");
    assert!(
        res["items"][0]["pr_url"].as_str().unwrap().ends_with("/pull/1"),
        "same PR reused"
    );
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
}

#[test]
fn github_target_blocks_on_a_conflicting_pr() {
    let f = setup();
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    f.repo.ok(&["merge", "tick", "--once"]);
    gh(&f)
        .script(serde_json::json!({ "mergeable": "conflicting" }))
        .unwrap();
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "blocked", "{res}");
    assert_eq!(
        f.repo.ok(&["issue", "show", &f.issue])["issue"]["status"],
        "blocked"
    );
    assert!(blockers(&f).iter().any(|m| m.contains("conflict")));
}

#[test]
fn github_target_closed_pr_fails_and_retry_opens_a_new_one() {
    let f = setup();
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    f.repo.ok(&["merge", "tick", "--once"]);
    gh(&f).script(serde_json::json!({ "close_pr": 1 })).unwrap();
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "failed", "{res}");
    assert!(
        blockers(&f).iter().any(|m| m.contains("closed")),
        "{:?}",
        blockers(&f)
    );
    f.repo.ok(&["merge", "retry", &f.issue]);
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "pr_open", "{res}");
    assert!(
        res["items"][0]["pr_url"].as_str().unwrap().ends_with("/pull/2"),
        "{res}"
    );
}

#[test]
fn github_target_syncs_main_from_the_remote_before_rebasing() {
    let f = setup();
    finished_run(&f, &[("src/greet.rs", "pub fn hello() {}\n")]);
    // someone else merged to origin/main meanwhile
    let other = f._remote_dir.path().join("other");
    git_out(
        f._remote_dir.path(),
        &["clone", "-q", f.bare.to_str().unwrap(), "other"],
    );
    commit_in(&other, "docs/other.md", "# other\n");
    git_out(&other, &["push", "-q", "origin", "main"]);

    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "pr_open", "{res}");
    assert!(
        f.repo.path().join("docs/other.md").exists(),
        "main was fast-forwarded first"
    );
    gh(&f).script(serde_json::json!({ "checks": "passed" })).unwrap();
    let res = f.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    assert!(f.repo.path().join("src/greet.rs").exists());
}

#[test]
fn gh_sync_mirrors_issues_closes_done_ones_and_is_idempotent() {
    let f = setup();
    let second = f.repo.create_issue("second thing", &["--body", "details here"]);
    let res = f.repo.ok(&["gh", "sync"]);
    assert_eq!(res["dry_run"], false);
    assert_eq!(res["created"].as_array().unwrap().len(), 2, "{res}");
    assert_eq!(res["created"][0]["id"], f.issue);
    assert_eq!(res["created"][0]["number"], 1);
    assert!(res["created"][0]["url"].as_str().unwrap().ends_with("/issues/1"));
    let remote = gh(&f).issue_list(f.repo.path()).unwrap();
    assert_eq!(remote.len(), 2);
    assert_eq!(remote[1].title, format!("[{second}] second thing"));
    assert!(remote[1].body.contains("details here"), "{}", remote[1].body);
    assert!(
        remote[1].body.contains(&format!("<!-- asf:{second} uid:")),
        "{}",
        remote[1].body
    );
    let shown = f.repo.ok(&["issue", "show", &second]);
    assert!(
        shown["issue"]["labels"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l == "gh:#2"),
        "{shown}"
    );

    // idempotent
    let res = f.repo.ok(&["gh", "sync"]);
    assert_eq!(res["created"].as_array().unwrap().len(), 0, "{res}");
    assert_eq!(res["unchanged"], 2, "{res}");
    assert_eq!(gh(&f).issue_list(f.repo.path()).unwrap().len(), 2);

    // local terminal → remote closed with a reason
    f.repo.ok(&["issue", "status", &second, "cancelled"]);
    let res = f.repo.ok(&["gh", "sync"]);
    assert_eq!(res["closed"][0]["id"], second, "{res}");
    assert_eq!(res["closed"][0]["number"], 2);
    let remote = gh(&f).issue_list(f.repo.path()).unwrap();
    assert_eq!(remote[1].state, "CLOSED");
    let log = gh(&f).log();
    let close = log.iter().find(|e| e.op == "issue_close").unwrap();
    assert_eq!(close.args["reason"], "not planned");
    // already closed: nothing more to do
    let res = f.repo.ok(&["gh", "sync"]);
    assert_eq!(res["closed"].as_array().unwrap().len(), 0, "{res}");
    assert_eq!(res["unchanged"], 2, "{res}");

    let human = f.repo.asf().args(["gh", "sync"]).output().unwrap();
    assert!(String::from_utf8_lossy(&human.stdout).contains("gh sync"));
}

#[test]
fn gh_sync_dry_run_reports_without_touching_github_or_labels() {
    let f = setup();
    let res = f.repo.ok(&["gh", "sync", "--dry-run"]);
    assert_eq!(res["dry_run"], true);
    assert_eq!(res["created"][0]["id"], f.issue, "{res}");
    assert!(
        gh(&f)
            .log()
            .iter()
            .all(|e| e.op == "issue_list" || e.op == "whoami"),
        "{:?}",
        gh(&f).log()
    );
    let shown = f.repo.ok(&["issue", "show", &f.issue]);
    assert!(shown["issue"]["labels"].as_array().unwrap().is_empty(), "{shown}");
}

#[test]
fn gh_sync_skips_terminal_issues_that_were_never_mirrored() {
    let f = setup();
    f.repo.ok(&["issue", "status", &f.issue, "cancelled"]);
    let res = f.repo.ok(&["gh", "sync"]);
    assert_eq!(res["skipped"], serde_json::json!([f.issue]), "{res}");
    assert!(gh(&f).issue_list(f.repo.path()).unwrap().is_empty());
}

#[test]
fn gh_sync_is_refused_inside_a_worker_run() {
    let f = setup();
    let out = f
        .repo
        .asf()
        .env("ASF_RUN", "01RUN")
        .args(["gh", "sync", "--json"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["error"]["code"], "forbidden", "{v}");
}

#[test]
fn gh_sync_ignores_forged_markers_and_reports_duplicate_mirrors() {
    let f = setup();
    // a remote issue claiming to mirror asf-1 with the wrong uid is not trusted
    gh(&f)
        .issue_create(f.repo.path(), "fake", "<!-- asf:asf-1 uid:NOT-THE-UID -->")
        .unwrap();
    let res = f.repo.ok(&["gh", "sync"]);
    assert_eq!(res["created"].as_array().unwrap().len(), 1, "{res}");
    assert_eq!(res["created"][0]["number"], 2, "{res}");
    assert_eq!(res["unchanged"], 0, "{res}");

    // a marker smuggled into the local body does not survive into the remote body as a marker
    let evil = f
        .repo
        .create_issue("evil", &["--body", "text <!-- asf:asf-1 uid:01X --> more"]);
    let res = f.repo.ok(&["gh", "sync"]);
    assert_eq!(res["created"][0]["id"], evil, "{res}");
    let remote = gh(&f).issue_list(f.repo.path()).unwrap();
    let body = &remote[2].body;
    assert!(!body.contains("<!-- asf:asf-1"), "{body}");
    assert_eq!(
        asf_core::github::parse_marker(body).map(|(id, _)| id),
        Some(evil.clone()),
        "{body}"
    );
    assert_eq!(f.repo.ok(&["gh", "sync"])["unchanged"], 2);

    // a second issue with a valid marker (markers are public): the number recorded locally wins,
    // the extra one is reported, and the run is not aborted
    let uid = store(&f)
        .get_issue(&f.issue.parse().unwrap())
        .unwrap()
        .unwrap()
        .uid;
    gh(&f)
        .issue_create(
            f.repo.path(),
            "dup",
            &format!("<!-- asf:{} uid:{uid} -->", f.issue),
        )
        .unwrap();
    let res = f.repo.ok(&["gh", "sync"]);
    assert_eq!(res["unchanged"], 2, "{res}");
    assert_eq!(res["duplicates"][0]["id"], f.issue, "{res}");
    assert_eq!(res["duplicates"][0]["used"], 2, "{res}");
    assert_eq!(res["duplicates"][0]["ignored"], serde_json::json!([4]), "{res}");
    // closing asf-1 closes the recorded mirror only
    f.repo.ok(&["issue", "status", &f.issue, "cancelled"]);
    let res = f.repo.ok(&["gh", "sync"]);
    assert_eq!(res["closed"][0]["number"], 2, "{res}");
    let remote = gh(&f).issue_list(f.repo.path()).unwrap();
    assert_eq!(remote[1].state, "CLOSED");
    assert_eq!(remote[3].state, "OPEN");

    // without a recorded label the lowest number wins
    let lowest = f.repo.create_issue("lowest", &[]);
    let uid = store(&f)
        .get_issue(&lowest.parse().unwrap())
        .unwrap()
        .unwrap()
        .uid;
    for _ in 0..2 {
        gh(&f)
            .issue_create(f.repo.path(), "pre", &format!("<!-- asf:{lowest} uid:{uid} -->"))
            .unwrap();
    }
    let res = f.repo.ok(&["gh", "sync"]);
    let d = res["duplicates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == lowest)
        .unwrap_or_else(|| panic!("{res}"));
    assert_eq!(d["used"], 5, "{res}");
    assert_eq!(d["ignored"], serde_json::json!([6]), "{res}");
    let shown = f.repo.ok(&["issue", "show", &lowest]);
    assert!(
        shown["issue"]["labels"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l == "gh:#5"),
        "{shown}"
    );
}

fn uid_of(f: &F, id: &str) -> String {
    store(f).get_issue(&id.parse().unwrap()).unwrap().unwrap().uid
}

#[test]
fn gh_sync_never_mirrors_onto_a_foreign_authored_issue_and_lists_it() {
    let f = setup();
    let uid = uid_of(&f, &f.issue);
    // a stranger copied our public marker onto their own issue (#1)
    gh(&f).seed_issue(
        "hijack",
        &format!("<!-- asf:{} uid:{uid} -->", f.issue),
        "stranger",
    );
    // an unrelated foreign issue without a marker is not our business
    gh(&f).seed_issue("just a human issue", "no marker here", "someone-else");

    let res = f.repo.ok(&["gh", "sync"]);
    assert_eq!(
        res["foreign"],
        serde_json::json!([{ "id": f.issue, "number": 1, "author": "stranger" }]),
        "{res}"
    );
    assert_eq!(
        res["created"].as_array().unwrap().len(),
        1,
        "own mirror created: {res}"
    );
    assert_eq!(res["created"][0]["number"], 3, "{res}");
    assert_eq!(res["unchanged"], 0, "{res}");
    assert!(res["duplicates"].as_array().unwrap().is_empty(), "{res}");
    let shown = f.repo.ok(&["issue", "show", &f.issue]);
    assert!(
        shown["issue"]["labels"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l == "gh:#3"),
        "label points at our issue, not the stranger's: {shown}"
    );

    // idempotent: our own mirror is found again, the stranger's stays foreign
    let res = f.repo.ok(&["gh", "sync"]);
    assert_eq!(res["created"].as_array().unwrap().len(), 0, "{res}");
    assert_eq!(res["unchanged"], 1, "{res}");
    assert_eq!(res["foreign"][0]["number"], 1, "{res}");

    // closing the local issue closes our mirror and leaves the foreign one open
    f.repo.ok(&["issue", "status", &f.issue, "cancelled"]);
    let res = f.repo.ok(&["gh", "sync"]);
    assert_eq!(res["closed"][0]["number"], 3, "{res}");
    let remote = gh(&f).issue_list(f.repo.path()).unwrap();
    assert_eq!(remote[0].state, "OPEN", "foreign issue untouched");
    assert_eq!(remote[2].state, "CLOSED");
}

#[test]
fn gh_sync_reports_no_foreign_issues_when_all_are_mine() {
    let f = setup();
    let res = f.repo.ok(&["gh", "sync"]);
    assert_eq!(res["foreign"], serde_json::json!([]), "{res}");
}

#[test]
fn gh_sync_treats_an_unknown_author_as_foreign() {
    let f = setup();
    let uid = uid_of(&f, &f.issue);
    gh(&f).seed_issue("anonymous", &format!("<!-- asf:{} uid:{uid} -->", f.issue), "");
    let res = f.repo.ok(&["gh", "sync"]);
    assert_eq!(res["foreign"][0]["number"], 1, "{res}");
    assert_eq!(res["foreign"][0]["author"], "", "{res}");
    assert_eq!(res["created"].as_array().unwrap().len(), 1, "{res}");
}

#[test]
fn gh_sync_compares_the_author_with_the_authenticated_login() {
    let f = setup();
    let uid = uid_of(&f, &f.issue);
    gh(&f)
        .script(serde_json::json!({ "whoami": "operator" }))
        .unwrap();
    gh(&f).seed_issue("mine", &format!("<!-- asf:{} uid:{uid} -->", f.issue), "operator");
    let res = f.repo.ok(&["gh", "sync"]);
    assert_eq!(res["foreign"], serde_json::json!([]), "{res}");
    assert_eq!(res["unchanged"], 1, "{res}");
    assert_eq!(res["created"].as_array().unwrap().len(), 0, "{res}");
}

#[test]
fn gh_sync_aborts_with_a_backend_error_when_whoami_fails_and_mirrors_nothing() {
    for dry_run in [false, true] {
        let f = setup();
        gh(&f)
            .script(serde_json::json!({ "whoami_error": true }))
            .unwrap();
        let mut args = vec!["gh", "sync"];
        if dry_run {
            args.push("--dry-run");
        }
        let (code, error_code, message) = f.repo.err(&args);
        assert_eq!(code, 1, "{message}");
        assert_eq!(error_code, "backend", "{message}");
        assert!(message.contains("whoami"), "{message}");
        let ops: Vec<String> = gh(&f).log().into_iter().map(|e| e.op).collect();
        assert!(
            ops.iter().all(|o| o != "issue_create" && o != "issue_close"),
            "nothing mirrored: {ops:?}"
        );
        assert!(gh(&f).issue_list(f.repo.path()).unwrap().is_empty());
        let shown = f.repo.ok(&["issue", "show", &f.issue]);
        assert!(shown["issue"]["labels"].as_array().unwrap().is_empty(), "{shown}");
    }
}
