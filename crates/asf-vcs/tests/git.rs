use std::path::{Path, PathBuf};
use std::process::Command;

use asf_core::{RebaseOutcome, Vcs, VcsError};
use asf_vcs::GitCli;
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

/// Repo with one commit on `main` containing `a.txt` and `b.txt`.
fn repo() -> (TempDir, PathBuf) {
    let dir = tempfile::Builder::new().prefix("asf-vcs-").tempdir().unwrap();
    let root = dir.path().to_path_buf();
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "user.email", "t@x"]);
    git(&root, &["config", "user.name", "t"]);
    git(&root, &["config", "core.autocrlf", "false"]);
    std::fs::write(root.join("a.txt"), "a\n").unwrap();
    std::fs::write(root.join("b.txt"), "b\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "init"]);
    (dir, root)
}

fn commit_file(dir: &Path, name: &str, content: &str, msg: &str) {
    std::fs::write(dir.join(name), content).unwrap();
    git(dir, &["add", name]);
    git(dir, &["commit", "-q", "-m", msg]);
}

#[test]
fn head_and_branch() {
    let (_d, root) = repo();
    let v = GitCli;
    assert_eq!(v.head(&root).unwrap().len(), 40);
    assert_eq!(v.current_branch(&root).unwrap(), "main");
    assert!(matches!(
        v.head(Path::new("C:/definitely/not/a/repo")).unwrap_err(),
        VcsError::Git(_) | VcsError::NotFound(_)
    ));
}

#[test]
fn worktree_add_creates_branch_from_base_and_returns_sha() {
    let (_d, root) = repo();
    let v = GitCli;
    let wt = root.join(".worktrees").join("asf-1");
    let base = v.worktree_add(&root, &wt, "feat/asf-1", "main").unwrap();
    assert_eq!(base, v.head(&root).unwrap());
    assert!(wt.join("a.txt").exists());
    assert_eq!(v.current_branch(&wt).unwrap(), "feat/asf-1");
    assert_eq!(
        v.current_branch(&root).unwrap(),
        "main",
        "main checkout untouched"
    );
    let wts = git(&root, &["worktree", "list", "--porcelain"]);
    assert!(wts.contains("feat/asf-1"));
}

#[test]
fn worktree_add_reuses_existing_branch_and_refuses_existing_dir() {
    let (_d, root) = repo();
    let v = GitCli;
    git(&root, &["branch", "feat/asf-2", "main"]);
    let wt = root.join(".worktrees").join("asf-2");
    v.worktree_add(&root, &wt, "feat/asf-2", "main").unwrap();
    assert_eq!(v.current_branch(&wt).unwrap(), "feat/asf-2");
    let e = v.worktree_add(&root, &wt, "feat/asf-2", "main").unwrap_err();
    assert!(matches!(e, VcsError::Git(_)), "{e}");
}

#[test]
fn worktree_remove_refuses_dirty_and_unmerged_then_succeeds() {
    let (_d, root) = repo();
    let v = GitCli;
    let wt = root.join(".worktrees").join("asf-3");
    v.worktree_add(&root, &wt, "feat/asf-3", "main").unwrap();
    std::fs::write(wt.join("c.txt"), "c\n").unwrap();
    let e = v.worktree_remove(&root, &wt).unwrap_err();
    assert!(
        matches!(e, VcsError::Dirty(ref f) if f.iter().any(|x| x.contains("c.txt"))),
        "{e}"
    );
    git(&wt, &["add", "c.txt"]);
    git(&wt, &["commit", "-q", "-m", "c"]);
    let e = v.worktree_remove(&root, &wt).unwrap_err();
    assert!(matches!(e, VcsError::Unmerged(_)), "{e}");
    git(&root, &["merge", "-q", "--ff-only", "feat/asf-3"]);
    v.worktree_remove(&root, &wt).unwrap();
    assert!(!wt.exists());
    assert!(
        git(&root, &["branch", "--list", "feat/asf-3"]).contains("feat/asf-3"),
        "branch kept"
    );
}

#[test]
fn status_and_changed_files() {
    let (_d, root) = repo();
    let v = GitCli;
    let wt = root.join(".worktrees").join("asf-4");
    let base = v.worktree_add(&root, &wt, "feat/asf-4", "main").unwrap();
    assert!(v.status_porcelain(&wt).unwrap().is_empty());
    std::fs::create_dir_all(wt.join("src")).unwrap();
    commit_file(&wt, "src/x.rs", "x", "x");
    std::fs::write(wt.join("a.txt"), "changed\n").unwrap();
    assert_eq!(v.status_porcelain(&wt).unwrap().len(), 1, "uncommitted a.txt");
    let changed = v.changed_files(&wt, &base).unwrap();
    assert_eq!(
        changed,
        vec!["src/x.rs".to_string()],
        "committed only, forward slashes"
    );
    // main moved on: changed_files still measures against merge-base
    commit_file(&root, "b.txt", "b2\n", "main moves");
    assert_eq!(
        v.changed_files(&wt, "main").unwrap(),
        vec!["src/x.rs".to_string()]
    );
}

#[test]
fn rebase_clean_and_conflict() {
    let (_d, root) = repo();
    let v = GitCli;
    let wt = root.join(".worktrees").join("asf-5");
    v.worktree_add(&root, &wt, "feat/asf-5", "main").unwrap();
    commit_file(&wt, "c.txt", "c\n", "feature");
    commit_file(&root, "b.txt", "b2\n", "main moves");
    match v.rebase(&wt, "main").unwrap() {
        RebaseOutcome::Clean { head } => assert_eq!(head, v.head(&wt).unwrap()),
        other => panic!("{other:?}"),
    }
    assert_eq!(v.changed_files(&wt, "main").unwrap(), vec!["c.txt".to_string()]);

    // conflict: both sides edit a.txt
    commit_file(&wt, "a.txt", "feature\n", "feat a");
    commit_file(&root, "a.txt", "main\n", "main a");
    match v.rebase(&wt, "main").unwrap() {
        RebaseOutcome::Conflict { files } => assert_eq!(files, vec!["a.txt".to_string()]),
        other => panic!("{other:?}"),
    }
    assert!(
        v.status_porcelain(&wt).unwrap().is_empty(),
        "aborted rebase leaves a clean tree"
    );
    assert!(!wt.join(".git").is_dir() || !root.join(".git/worktrees/asf-5/rebase-merge").exists());
    assert_eq!(v.current_branch(&wt).unwrap(), "feat/asf-5");
}

#[test]
fn ff_merge_fast_forwards_and_refuses_non_ff_or_dirty() {
    let (_d, root) = repo();
    let v = GitCli;
    let wt = root.join(".worktrees").join("asf-6");
    v.worktree_add(&root, &wt, "feat/asf-6", "main").unwrap();
    commit_file(&wt, "c.txt", "c\n", "feature");
    let new_head = v.ff_merge(&root, "feat/asf-6").unwrap();
    assert_eq!(new_head, v.head(&wt).unwrap());
    assert!(root.join("c.txt").exists());

    // non-ff: main diverged
    commit_file(&wt, "d.txt", "d\n", "more");
    commit_file(&root, "e.txt", "e\n", "main moves");
    assert!(matches!(
        v.ff_merge(&root, "feat/asf-6").unwrap_err(),
        VcsError::Git(_)
    ));

    // dirty main refuses before touching anything
    std::fs::write(root.join("a.txt"), "dirty\n").unwrap();
    assert!(matches!(
        v.ff_merge(&root, "feat/asf-6").unwrap_err(),
        VcsError::Dirty(_)
    ));
}

#[test]
fn ls_files_lists_tracked_with_forward_slashes() {
    let (_d, root) = repo();
    std::fs::create_dir_all(root.join("src")).unwrap();
    commit_file(&root, "src/m.rs", "", "m");
    let files = GitCli.ls_files(&root).unwrap();
    assert_eq!(
        files,
        vec!["a.txt".to_string(), "b.txt".to_string(), "src/m.rs".to_string()]
    );
}
