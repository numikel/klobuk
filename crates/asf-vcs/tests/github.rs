//! `Vcs::push` / `pull_ff` against a local bare remote, and the file-backed `FakeGitHub`.

use std::path::{Path, PathBuf};
use std::process::Command;

use asf_core::github::*;
use asf_core::Vcs;
use asf_vcs::{FakeGitHub, GitCli};
use tempfile::TempDir;

fn git(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(args).current_dir(cwd).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit_file(dir: &Path, name: &str, content: &str, msg: &str) {
    std::fs::write(dir.join(name), content).unwrap();
    git(dir, &["add", name]);
    git(
        dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@x",
            "commit",
            "-q",
            "-m",
            msg,
        ],
    );
}

/// `origin` (bare) + a clone on `main` with one commit pushed.
fn repo_with_remote() -> (TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let bare = tmp.path().join("origin.git");
    git(tmp.path(), &["init", "-q", "--bare", "-b", "main", "origin.git"]);
    let root = tmp.path().join("work");
    git(tmp.path(), &["init", "-q", "-b", "main", "work"]);
    git(&root, &["remote", "add", "origin", bare.to_str().unwrap()]);
    commit_file(&root, "README.md", "# hi\n", "init");
    git(&root, &["push", "-q", "-u", "origin", "main"]);
    (tmp, bare, root)
}

#[test]
fn push_and_pull_ff_round_trip_through_the_remote() {
    let (_t, bare, root) = repo_with_remote();
    let vcs = GitCli;
    git(&root, &["checkout", "-q", "-b", "feat/x"]);
    commit_file(&root, "x.txt", "x\n", "feat x");
    vcs.push(&root, "origin", "feat/x", false).unwrap();
    assert!(git(&bare, &["branch", "--list", "feat/x"]).contains("feat/x"));

    // rewrite history (rebase-like) → plain push would be rejected, force-with-lease is accepted
    git(
        &root,
        &["commit", "-q", "--amend", "-m", "feat x (amended)", "--no-edit"],
    );
    let plain = vcs.push(&root, "origin", "feat/x", false);
    assert!(plain.is_err(), "non-ff push must fail without lease");
    vcs.push(&root, "origin", "feat/x", true).unwrap();

    // the remote's main moves (someone merged feat/x there); pull_ff brings it in
    git(&bare, &["update-ref", "refs/heads/main", "refs/heads/feat/x"]);
    git(&root, &["checkout", "-q", "main"]);
    let head = vcs.pull_ff(&root, "origin", "main").unwrap();
    assert_eq!(head, git(&root, &["rev-parse", "feat/x"]));
    assert!(root.join("x.txt").exists());
}

#[test]
fn pull_ff_refuses_a_diverged_main() {
    let (_t, bare, root) = repo_with_remote();
    git(&root, &["checkout", "-q", "-b", "feat/y"]);
    commit_file(&root, "y.txt", "y\n", "remote side");
    git(&root, &["push", "-q", "origin", "feat/y"]);
    git(&bare, &["update-ref", "refs/heads/main", "refs/heads/feat/y"]);
    git(&root, &["checkout", "-q", "main"]);
    commit_file(&root, "local.txt", "l\n", "local side");
    let e = GitCli.pull_ff(&root, "origin", "main").unwrap_err();
    assert!(e.to_string().contains("fast-forward"), "{e}");
}

#[test]
fn fake_github_opens_polls_and_merges_a_pr_into_the_remote() {
    let (_t, bare, root) = repo_with_remote();
    let gh = FakeGitHub::new(root.join(".factory/fake-github.json"));
    git(&root, &["checkout", "-q", "-b", "feat/z"]);
    commit_file(&root, "z.txt", "z\n", "feat z");
    GitCli.push(&root, "origin", "feat/z", false).unwrap();

    assert_eq!(gh.pr_find(&root, "feat/z").unwrap(), None);
    let pr = gh.pr_create(&root, "main", "feat/z", "feat z", "body").unwrap();
    assert_eq!(pr.number, 1);
    assert_eq!(pr.state, PrState::Open);
    assert!(pr.url.contains("/pull/1"), "{}", pr.url);
    assert_eq!(pr.checks, ChecksState::None);

    gh.script(serde_json::json!({ "checks": "pending" })).unwrap();
    let pr = gh.pr_find(&root, "feat/z").unwrap().unwrap();
    assert_eq!(pr.checks, ChecksState::Pending);
    gh.script(serde_json::json!({ "checks": "passed", "mergeable": "conflicting" }))
        .unwrap();
    let pr = gh.pr_find(&root, "feat/z").unwrap().unwrap();
    assert_eq!(pr.mergeable, Mergeable::Conflicting);
    gh.script(serde_json::json!({ "mergeable": "mergeable" }))
        .unwrap();

    gh.pr_merge(&root, 1, "rebase", false).unwrap();
    let pr = gh.pr_find(&root, "feat/z").unwrap().unwrap();
    assert_eq!(pr.state, PrState::Merged);
    assert_eq!(
        git(&bare, &["rev-parse", "main"]),
        git(&root, &["rev-parse", "feat/z"]),
        "fake merge fast-forwards the remote base"
    );
    let ops: Vec<String> = gh.log().into_iter().map(|e| e.op).collect();
    assert_eq!(
        ops,
        vec![
            "pr_find",
            "pr_create",
            "pr_find",
            "pr_find",
            "pr_merge",
            "pr_find"
        ]
    );
}

#[test]
fn fake_github_issues_create_list_close() {
    let (_t, _bare, root) = repo_with_remote();
    let gh = FakeGitHub::new(root.join(".factory/fake-github.json"));
    assert!(gh.issue_list(&root).unwrap().is_empty());
    let a = gh
        .issue_create(&root, "[asf-1] First", "body\n<!-- asf:asf-1 uid:01X -->")
        .unwrap();
    assert_eq!(a.number, 1);
    assert_eq!(a.state, "OPEN");
    gh.issue_create(&root, "[asf-2] Second", "b2").unwrap();
    gh.issue_close(&root, 1, "completed").unwrap();
    let all = gh.issue_list(&root).unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].state, "CLOSED");
    assert_eq!(parse_marker(&all[0].body), Some(("asf-1".into(), "01X".into())));
    assert_eq!(parse_marker(&all[1].body), None);
}
