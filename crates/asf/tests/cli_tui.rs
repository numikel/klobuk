mod common;

use common::{git, Repo};

fn fake_repo() -> Repo {
    let repo = Repo::initialised();
    let cfg = repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("runtime = \"herdr\"", "runtime = \"fake\"");
    std::fs::write(&cfg, s).unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "cfg"]);
    repo
}

#[test]
fn tui_once_prints_a_frame_with_board_and_running_issue() {
    let repo = fake_repo();
    let a = repo.create_issue("greeting", &["--ready", "--scope", "src/**"]);
    repo.create_issue("docs", &["--scope", "docs/**"]);
    repo.ok(&["run", "start", &a]);
    let out = repo.asf().args(["tui", "--once"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("asf-1") && text.contains("asf-2"), "{text}");
    assert!(text.contains("in-progress") && text.contains("backlog"), "{text}");
    assert!(text.contains("workers 1/3"), "{text}");
    assert!(!text.contains("\u{1b}["), "no ANSI escapes in --once output");
}

#[test]
fn tui_once_can_open_other_views() {
    let repo = fake_repo();
    let a = repo.create_issue("greeting", &["--ready", "--scope", "src/**"]);
    repo.ok(&["run", "start", &a]);
    repo.ok(&[
        "--lane",
        "orchestrator",
        "issue",
        "comment",
        &a,
        "--type",
        "BLOCKER",
        "--to",
        "human",
        "--body",
        "needs a decision",
    ]);
    let out = repo
        .asf()
        .args(["tui", "--once", "--view", "inbox"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("BLOCKER") && text.contains("needs a decision"),
        "{text}"
    );
    let out = repo
        .asf()
        .args(["tui", "--once", "--view", "agents"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("asf-1") && text.contains("working"), "{text}");
    let out = repo
        .asf()
        .args(["tui", "--once", "--view", "bogus"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}
