mod common;

use asf_core::{now_rfc3339, IssueStore, MergePatch, MergeState, RunPatch, RunState};
use asf_store::SqliteStore;
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
    let out = repo
        .asf()
        .args(["tui", "--once", "--view", "board"])
        .output()
        .unwrap();
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
    assert!(text.contains("[7] Config"), "{text}");
    assert!(text.contains("fake") || text.contains("local"), "{text}");
}

#[test]
fn tui_once_board_has_a_validating_column() {
    let repo = fake_repo();
    let text = tui_once_text(&repo);
    assert!(text.contains("validating"), "{text}");
}

fn ask(repo: &Repo, issue: &str, body: &str) -> String {
    let v = repo.ok(&[
        "--lane", issue, "issue", "comment", issue, "--type", "QUESTION", "--to", "human", "--body", body,
    ]);
    v["id"].as_i64().unwrap().to_string()
}

/// The Board frame (the banner and the Board footer live there; the first view is the Now desk).
fn tui_once_text(repo: &Repo) -> String {
    tui_once_view(repo, "board")
}

fn tui_once_view(repo: &Repo, view: &str) -> String {
    let out = repo
        .asf()
        .args(["tui", "--once", "--view", view])
        .output()
        .unwrap();
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

#[test]
fn tui_once_shows_the_contextual_footer_for_each_view() {
    let repo = fake_repo();
    let board = tui_once_text(&repo);
    let footer = board.lines().rfind(|l| !l.trim().is_empty()).unwrap();
    assert!(
        footer.contains("s start") && footer.contains("? help  q quit"),
        "{board}"
    );
    let out = repo
        .asf()
        .args(["tui", "--once", "--view", "config"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let footer = text.lines().rfind(|l| !l.trim().is_empty()).unwrap();
    assert!(
        footer.contains("d unset") && !footer.contains("s start"),
        "{text}"
    );
    assert!(footer.trim_end().ends_with("? help  q quit"), "{text}");
}

/// Runs `asf --repo <root> <args> --json` from an unrelated directory, as the TUI's children do.
fn child_envelope(repo: &Repo, args: &[&str]) -> (serde_json::Value, i32) {
    let elsewhere = tempfile::tempdir().unwrap();
    let root = repo.path().to_str().unwrap();
    let out = repo
        .asf_in(elsewhere.path())
        .args(["--repo", root])
        .args(args)
        .arg("--json")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("not json: {e}\n{stdout}"));
    (v, out.status.code().unwrap_or(-1))
}

#[test]
fn the_background_children_print_the_envelopes_the_tui_summarises() {
    let repo = fake_repo();
    let a = repo.create_issue("greeting", &["--ready", "--scope", "src/**"]);

    let (tick, code) = child_envelope(&repo, &["serve", "--once"]);
    assert_eq!(code, 0, "{tick}");
    assert_eq!(tick["ok"], true, "{tick}");
    for path in [
        &tick["result"]["watch"]["transitions"],
        &tick["result"]["merge"]["items"],
        &tick["result"]["promoted"],
        &tick["result"]["closed_epics"],
    ] {
        assert!(path.is_array(), "the TUI counts these arrays: {tick}");
    }

    let (start, code) = child_envelope(&repo, &["run", "start", &a]);
    assert_eq!(code, 0, "{start}");
    assert_eq!(start["result"]["run"]["issue_id"], a.as_str(), "{start}");
    assert!(start["result"]["workspace_id"].is_string() && start["result"]["pane_id"].is_string());

    // a second start is refused with an envelope the TUI shows as `✗ <message>`
    let (again, code) = child_envelope(&repo, &["run", "start", &a]);
    assert_ne!(code, 0);
    assert_eq!(again["ok"], false, "{again}");
    assert!(
        again["error"]["message"].as_str().is_some_and(|m| !m.is_empty()),
        "{again}"
    );
}

fn store(repo: &Repo) -> SqliteStore {
    SqliteStore::open(&repo.path().join(".factory/factory.db")).unwrap()
}

#[test]
fn the_verify_and_retry_children_print_the_fields_the_now_view_summarises() {
    let repo = fake_repo();
    let a = repo.create_issue("greeting", &["--ready", "--scope", "src/**"]);
    repo.ok(&["run", "start", &a]);
    let s = store(&repo);
    let id: asf_core::IssueId = a.parse().unwrap();
    let run = s.runs(Some(&id), false).unwrap().pop().unwrap();
    s.update_run(
        &run.id,
        &RunPatch {
            state: Some(RunState::Failed),
            ..RunPatch::default()
        },
    )
    .unwrap();

    // `v`: `asf run verify <id> --json`; the TUI prints issue_id, from -> to and the detail's first line
    let (verify, code) = child_envelope(&repo, &["run", "verify", &a]);
    assert_eq!(code, 0, "{verify}");
    assert_eq!(verify["ok"], true, "{verify}");
    let r = &verify["result"];
    assert_eq!(r["issue_id"], a.as_str(), "{verify}");
    assert_eq!(r["from"], "failed", "{verify}");
    assert!(r["to"].is_string() && r["detail"].is_string(), "{verify}");

    // `r`: `asf merge retry <id> --json`; the TUI prints issue_id and the queue item number
    let item = s.enqueue_merge(&id, None, &format!("feat/{a}")).unwrap();
    s.update_merge(
        item.id,
        &MergePatch {
            state: Some(MergeState::Failed),
            last_error: Some(Some("gate failed".into())),
            ..MergePatch::default()
        },
    )
    .unwrap();
    let (retry, code) = child_envelope(&repo, &["merge", "retry", &a]);
    assert_eq!(code, 0, "{retry}");
    assert_eq!(retry["ok"], true, "{retry}");
    assert_eq!(retry["result"]["issue_id"], a.as_str(), "{retry}");
    assert_eq!(retry["result"]["id"], item.id, "{retry}");

    // retrying what is not failed is the error envelope the TUI shows as `✗ <message>`
    let (again, code) = child_envelope(&repo, &["merge", "retry", &a]);
    assert_ne!(code, 0);
    assert_eq!(again["ok"], false, "{again}");
    assert!(
        again["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("only failed/blocked")),
        "{again}"
    );
}

#[test]
fn tui_once_opens_on_the_now_view_and_view_now_shows_the_same_frame() {
    let repo = fake_repo();
    let first =
        String::from_utf8_lossy(&repo.asf().args(["tui", "--once"]).output().unwrap().stdout).to_string();
    let lines: Vec<&str> = first.lines().collect();
    assert!(
        lines[1].contains("[1] Now") && lines[1].contains("[2] Board"),
        "the tab bar starts with Now:\n{first}"
    );
    for needle in [
        "NEEDS YOU (0)",
        "Nothing needs you.",
        "RUNNING (0)",
        "No worker is running. Press Tab for the Board to start one.",
        "UP NEXT (0)",
        "DONE TODAY (0)",
    ] {
        assert!(first.contains(needle), "{needle:?}:\n{first}");
    }
    assert!(!first.contains("backlog 0"), "not the Board:\n{first}");
    assert_eq!(first, tui_once_view(&repo, "now"));
    assert_ne!(first, tui_once_view(&repo, "board"));
    // the viewing keys of the footer are the ones of the Now view
    let footer = first.lines().rfind(|l| !l.trim().is_empty()).unwrap();
    assert!(
        footer.contains("a answer") && footer.contains("Enter open issue") && !footer.contains("s start"),
        "{first}"
    );
}

#[test]
fn tui_once_now_lists_what_needs_you_what_runs_what_is_next_and_what_is_done() {
    let repo = fake_repo();
    let running = repo.create_issue("greeting", &["--ready", "--scope", "src/**"]);
    let failing = repo.create_issue("docs", &["--ready", "--scope", "docs/**"]);
    let asking = repo.create_issue("parser", &[]);
    let shipped = repo.create_issue("shipped", &["--ready"]);
    let merging = repo.create_issue("lint", &[]);
    repo.ok(&["run", "start", &running]);
    repo.ok(&["run", "start", &failing]);

    // a run that failed is not an active run; the desk still has to show it
    let s = store(&repo);
    let run = s
        .runs(Some(&failing.parse().unwrap()), false)
        .unwrap()
        .pop()
        .unwrap();
    s.update_run(
        &run.id,
        &RunPatch {
            state: Some(RunState::Failed),
            exit_reason: Some("gate `test` failed\nsecond line".into()),
            finished_at: Some(now_rfc3339()),
            ..RunPatch::default()
        },
    )
    .unwrap();
    // a question to the human, a failed merge item and an issue closed today
    ask(&repo, &asking, "Which crate owns the parser?\nsecond line");
    let item = s
        .enqueue_merge(&merging.parse().unwrap(), None, &format!("feat/{merging}"))
        .unwrap();
    s.update_merge(
        item.id,
        &MergePatch {
            state: Some(MergeState::Failed),
            last_error: Some(Some("gate `test` after rebase (2 tests)\nmore".into())),
            ..MergePatch::default()
        },
    )
    .unwrap();
    repo.ok(&["issue", "status", &shipped, "done"]);

    let text = tui_once_view(&repo, "now");
    assert!(line_with(&text, 0).contains("3 waiting"), "{text}");
    assert!(
        text.contains("NEEDS YOU (3)") && text.contains("RUNNING (1)"),
        "{text}"
    );
    for (id, parts) in [
        (&failing, ["failed", "gate `test` failed", "v verify"]),
        (&asking, ["question", "Which crate owns the parser?", "a answer"]),
        (
            &merging,
            ["merge failed", "gate `test` after rebase (2 tests)", "r retry"],
        ),
    ] {
        let row = line_with(&text, id);
        for part in parts {
            assert!(row.contains(part), "{id}: {part:?} in {row:?}");
        }
        assert!(row.contains("■") && row.contains("<1 min"), "{id}: {row:?}");
        assert!(!row.contains("second line") && !row.contains("more"), "{row:?}");
    }
    let row = line_with(&text, format!("● {running}"));
    for part in ["working", "greeting", "$0.00 / $5", "<1 min"] {
        assert!(row.contains(part), "{part:?} in {row:?}");
    }
    assert!(text.contains(&format!("DONE TODAY (1)   ✓ {shipped}")), "{text}");
    assert!(text.contains("UP NEXT (2)"), "{text}");
}

/// The first line of `text` containing `needle` (or line `n` when given a number-like index).
fn line_with(text: &str, needle: impl std::fmt::Display) -> String {
    let needle = needle.to_string();
    if let Ok(n) = needle.parse::<usize>() {
        return text.lines().nth(n).unwrap_or_default().to_string();
    }
    text.lines()
        .find(|l| l.contains(&needle))
        .unwrap_or_else(|| panic!("no line with {needle:?}:\n{text}"))
        .to_string()
}
