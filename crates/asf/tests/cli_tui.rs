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

#[test]
fn tui_once_config_view_prints_every_editable_key() {
    let repo = fake_repo();
    let out = repo
        .asf()
        .args(["tui", "--once", "--view", "config"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    for (key, _) in asf_core::config_edit::editable_keys() {
        assert!(text.contains(key), "missing {key} in:\n{text}");
    }
    assert!(text.contains("[6] Config"), "{text}");
    assert!(text.contains("fake") || text.contains("local"), "{text}");
}

#[test]
fn tui_once_board_has_a_validating_column() {
    let repo = fake_repo();
    let out = repo.asf().args(["tui", "--once"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("validating"), "{text}");
}

fn ask(repo: &Repo, issue: &str, body: &str) -> String {
    let v = repo.ok(&[
        "--lane", issue, "issue", "comment", issue, "--type", "QUESTION", "--to", "human", "--body", body,
    ]);
    v["id"].as_i64().unwrap().to_string()
}

fn tui_once_text(repo: &Repo) -> String {
    let out = repo.asf().args(["tui", "--once"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn tui_once_shows_a_banner_for_an_unanswered_question_and_drops_it_when_answered() {
    let repo = fake_repo();
    let a = repo.create_issue("greeting", &["--ready", "--scope", "src/**"]);
    let quiet = tui_once_text(&repo);
    assert!(!quiet.contains("waiting") && !quiet.contains(" asks:"), "{quiet}");

    let qid = ask(&repo, &a, "Which crate owns the parser?\nsecond line");
    let text = tui_once_text(&repo);
    let banner = text.lines().nth(2).unwrap_or_default();
    assert_eq!(
        banner.trim(),
        format!("■ {a} asks: Which crate owns the parser?"),
        "{text}"
    );
    assert!(text.lines().next().unwrap().contains("1 waiting"), "{text}");

    // Reading the inbox (cursor advance) must not hide a question that still blocks `finish`.
    repo.ok(&["--lane", "human", "issue", "inbox"]);
    let text = tui_once_text(&repo);
    assert!(
        text.lines().nth(2).unwrap_or_default().contains(" asks:"),
        "{text}"
    );

    repo.ok(&[
        "--lane",
        "human",
        "issue",
        "comment",
        &a,
        "--type",
        "ANSWER",
        "--to",
        &a,
        "--reply-to",
        &qid,
        "--body",
        "asf-core",
    ]);
    let text = tui_once_text(&repo);
    assert!(!text.contains("waiting") && !text.contains(" asks:"), "{text}");
}

#[test]
fn tui_once_banner_shows_in_other_views_too() {
    let repo = fake_repo();
    let a = repo.create_issue("greeting", &["--ready", "--scope", "src/**"]);
    ask(&repo, &a, "pick one");
    for view in ["agents", "queue", "log", "config"] {
        let out = repo
            .asf()
            .args(["tui", "--once", "--view", view])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            text.lines()
                .nth(2)
                .unwrap_or_default()
                .starts_with(&format!("■ {a} asks: pick one")),
            "{view}:\n{text}"
        );
    }
}
