mod common;

use std::path::PathBuf;
use std::process::Command;

use asf_core::*;
use asf_store::SqliteStore;
use common::{git, Repo};

fn rev_parse(cwd: &std::path::Path, rev: &str) -> String {
    let out = Command::new("git")
        .args(["rev-parse", rev])
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn branch_exists(repo: &Repo, branch: &str) -> bool {
    let out = Command::new("git")
        .args(["branch", "--list", branch])
        .current_dir(repo.path())
        .output()
        .unwrap();
    !String::from_utf8_lossy(&out.stdout).trim().is_empty()
}

/// An unclaimed issue `asf-1` whose lane has one commit that is already fast-forwarded into `main`.
fn finished_lane() -> (Repo, String, PathBuf) {
    let r = Repo::initialised();
    let id = r.create_issue("lane", &["--ready"]);
    let wt = r.worktree(&id);
    std::fs::write(wt.join("work.txt"), "done\n").unwrap();
    git(&wt, &["add", "work.txt"]);
    git(&wt, &["commit", "-q", "-m", "work"]);
    git(r.path(), &["merge", "-q", "--ff-only", &format!("feat/{id}")]);
    (r, id, wt)
}

#[test]
fn clean_removes_a_finished_lane_and_keeps_its_branch() {
    let (r, id, wt) = finished_lane();
    let res = r.ok(&["lane", "clean", &id]);
    assert_eq!(res["id"], id.as_str());
    assert_eq!(res["worktree"], format!(".worktrees/{id}").as_str());
    assert_eq!(res["removed"], true);
    assert!(!wt.exists(), "worktree directory must be gone");
    assert!(branch_exists(&r, &format!("feat/{id}")), "branch must be kept");
}

#[test]
fn clean_refuses_a_claimed_lane_with_exit_3() {
    let (r, id, wt) = finished_lane();
    r.ok(&["--lane", &id, "issue", "claim", &id]);
    let (code, ecode, _) = r.err(&["lane", "clean", &id]);
    assert_eq!((code, ecode.as_str()), (3, "conflict"));
    assert!(wt.is_dir(), "a claimed lane must stay");
}

#[test]
fn clean_refuses_a_dirty_worktree_with_exit_3() {
    let (r, id, wt) = finished_lane();
    std::fs::write(wt.join("scratch.txt"), "wip\n").unwrap();
    let (code, ecode, msg) = r.err(&["lane", "clean", &id]);
    assert_eq!((code, ecode.as_str()), (3, "conflict"), "{msg}");
    assert!(wt.is_dir());
    assert!(wt.join("scratch.txt").exists());
}

#[test]
fn clean_refuses_a_branch_that_is_not_merged_with_exit_3() {
    let r = Repo::initialised();
    let id = r.create_issue("lane", &["--ready"]);
    let wt = r.worktree(&id);
    std::fs::write(wt.join("work.txt"), "unmerged\n").unwrap();
    git(&wt, &["add", "work.txt"]);
    git(&wt, &["commit", "-q", "-m", "work"]);
    let (code, ecode, _) = r.err(&["lane", "clean", &id]);
    assert_eq!((code, ecode.as_str()), (3, "conflict"));
    assert!(wt.is_dir());
}

#[test]
fn clean_refuses_a_lane_with_no_commits_of_its_own() {
    // worktree_remove would remove it through the ancestor path; lane clean must not.
    let r = Repo::initialised();
    let id = r.create_issue("lane", &["--ready"]);
    let wt = r.worktree(&id);
    let store = SqliteStore::open(&r.path().join(".factory/factory.db")).unwrap();
    store
        .create_run(&NewRun {
            issue_id: id.parse().unwrap(),
            runtime: "fake".into(),
            worktree: format!(".worktrees/{id}"),
            branch: format!("feat/{id}"),
            base_sha: rev_parse(&wt, "HEAD"),
            budget_usd: None,
        })
        .unwrap();
    let (code, ecode, msg) = r.err(&["lane", "clean", &id]);
    assert_eq!((code, ecode.as_str()), (3, "conflict"), "{msg}");
    assert!(wt.is_dir());
    // dry-run applies the same guard
    let (code, _, _) = r.err(&["lane", "clean", &id, "--dry-run"]);
    assert_eq!(code, 3);
}

#[test]
fn clean_dry_run_reports_removed_false_and_changes_nothing() {
    let (r, id, wt) = finished_lane();
    let res = r.ok(&["lane", "clean", &id, "--dry-run"]);
    assert_eq!(res["id"], id.as_str());
    assert_eq!(res["worktree"], format!(".worktrees/{id}").as_str());
    assert_eq!(res["removed"], false);
    assert!(wt.is_dir(), "dry-run must leave the worktree in place");
    assert!(branch_exists(&r, &format!("feat/{id}")));
}

#[test]
fn clean_dry_run_still_refuses_a_claimed_lane() {
    let (r, id, wt) = finished_lane();
    r.ok(&["--lane", &id, "issue", "claim", &id]);
    let (code, ecode, _) = r.err(&["lane", "clean", &id, "--dry-run"]);
    assert_eq!((code, ecode.as_str()), (3, "conflict"));
    assert!(wt.is_dir());
}

#[test]
fn clean_of_a_missing_worktree_is_not_found() {
    let r = Repo::initialised();
    let id = r.create_issue("lane", &["--ready"]);
    let (code, ecode, _) = r.err(&["lane", "clean", &id]);
    assert_eq!((code, ecode.as_str()), (1, "not_found"));
}

#[test]
fn lane_help_lists_clean_with_dry_run_and_no_force() {
    let r = Repo::new();
    let help = |args: &[&str]| {
        let out = r.asf().args(args).arg("--help").output().unwrap();
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    assert!(help(&["lane"]).contains("clean"));
    let clean = help(&["lane", "clean"]);
    assert!(clean.contains("--dry-run"), "{clean}");
    assert!(!clean.contains("--force"), "{clean}");
}
