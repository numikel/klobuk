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

// ---- lane clean --all -----------------------------------------------------------------------------

/// A second (third, ...) finished lane: one commit, merged into `main` with a merge commit because
/// `main` has moved since the lane was created.
fn finished_lane_in(r: &Repo, title: &str) -> (String, PathBuf) {
    let id = r.create_issue(title, &["--ready"]);
    let wt = r.worktree(&id);
    std::fs::write(wt.join(format!("{id}.txt")), "done\n").unwrap();
    git(&wt, &["add", "."]);
    git(&wt, &["commit", "-q", "-m", "work"]);
    git(r.path(), &["merge", "-q", "--no-edit", &format!("feat/{id}")]);
    (id, wt)
}

fn ids(v: &serde_json::Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_string())
        .collect()
}

/// Four lanes: `done` (finished), `dirty` (finished, loose file), `claimed` (finished, claim held)
/// and `unmerged` (a commit that never reached main).
struct Lanes {
    r: Repo,
    done: (String, PathBuf),
    dirty: (String, PathBuf),
    claimed: (String, PathBuf),
    unmerged: (String, PathBuf),
}

fn lanes() -> Lanes {
    let r = Repo::initialised();
    let done = finished_lane_in(&r, "done");
    let dirty = finished_lane_in(&r, "dirty");
    std::fs::write(dirty.1.join("scratch.txt"), "wip\n").unwrap();
    let claimed = finished_lane_in(&r, "claimed");
    r.ok(&["--lane", &claimed.0, "issue", "claim", &claimed.0]);
    let uid = r.create_issue("unmerged", &["--ready"]);
    let uwt = r.worktree(&uid);
    std::fs::write(uwt.join("u.txt"), "x\n").unwrap();
    git(&uwt, &["add", "u.txt"]);
    git(&uwt, &["commit", "-q", "-m", "unmerged work"]);
    Lanes {
        r,
        done,
        dirty,
        claimed,
        unmerged: (uid, uwt),
    }
}

#[test]
fn clean_all_removes_every_finished_lane_and_reports_each_refusal_with_its_reason() {
    let l = lanes();
    let res = l.r.ok(&["lane", "clean", "--all"]);
    assert_eq!(res["dry_run"], false, "{res}");
    assert_eq!(ids(&res["cleaned"]), std::slice::from_ref(&l.done.0), "{res}");
    assert_eq!(res["cleaned"][0]["removed"], true, "{res}");
    assert!(!l.done.1.exists(), "the finished lane is gone");
    assert!(
        branch_exists(&l.r, &format!("feat/{}", l.done.0)),
        "the branch stays"
    );

    let refused = res["refused"].as_array().unwrap();
    let mut refused_ids = ids(&res["refused"]);
    refused_ids.sort();
    let mut want = vec![l.dirty.0.clone(), l.claimed.0.clone(), l.unmerged.0.clone()];
    want.sort();
    assert_eq!(refused_ids, want, "{res}");
    let reason = |id: &str| -> String {
        refused.iter().find(|e| e["id"] == id).unwrap()["reason"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert!(reason(&l.claimed.0).contains("claimed"), "{res}");
    assert!(reason(&l.dirty.0).contains("scratch.txt"), "{res}");
    assert!(!reason(&l.unmerged.0).is_empty(), "{res}");
    for (_, wt) in [&l.dirty, &l.claimed, &l.unmerged] {
        assert!(wt.is_dir(), "a refused lane stays: {}", wt.display());
    }
    assert!(l.dirty.1.join("scratch.txt").exists());
}

#[test]
fn clean_all_dry_run_lists_what_it_would_remove_and_removes_nothing() {
    let l = lanes();
    let res = l.r.ok(&["lane", "clean", "--all", "--dry-run"]);
    assert_eq!(res["dry_run"], true, "{res}");
    assert_eq!(ids(&res["cleaned"]), std::slice::from_ref(&l.done.0), "{res}");
    assert_eq!(res["cleaned"][0]["removed"], false, "{res}");
    assert!(
        ids(&res["refused"]).contains(&l.claimed.0) && ids(&res["refused"]).contains(&l.dirty.0),
        "a dry run refuses what the real run refuses: {res}"
    );
    for (_, wt) in [&l.done, &l.dirty, &l.claimed, &l.unmerged] {
        assert!(wt.is_dir(), "dry-run removed {}", wt.display());
    }
    let human =
        l.r.asf()
            .args(["lane", "clean", "--all", "--dry-run"])
            .output()
            .unwrap();
    assert_eq!(human.status.code(), Some(0));
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains(&l.done.0) && text.contains("dry-run"), "{text}");
}

#[test]
fn clean_all_with_nothing_to_clean_is_an_empty_report_not_an_error() {
    let r = Repo::initialised();
    let res = r.ok(&["lane", "clean", "--all"]);
    assert_eq!(res["cleaned"], serde_json::json!([]), "{res}");
    assert_eq!(res["refused"], serde_json::json!([]), "{res}");
}

#[test]
fn clean_needs_exactly_one_of_an_id_and_all() {
    let (r, id, wt) = finished_lane();
    let (code, ecode, _) = r.err(&["lane", "clean"]);
    assert_eq!((code, ecode.as_str()), (2, "usage"));
    let (code, ecode, _) = r.err(&["lane", "clean", &id, "--all"]);
    assert_eq!((code, ecode.as_str()), (2, "usage"));
    assert!(wt.is_dir(), "a usage error removes nothing");
}

#[test]
fn lane_help_lists_all_for_clean() {
    let r = Repo::new();
    let out = r.asf().args(["lane", "clean", "--help"]).output().unwrap();
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("--all") && help.contains("--dry-run"), "{help}");
}
