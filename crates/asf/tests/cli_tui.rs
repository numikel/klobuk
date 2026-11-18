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

/// A pulse the way `asf serve` writes it; `stop_reason` makes it a clean stop.
fn write_pulse(repo: &Repo, last_tick_at: &str, stop_reason: Option<&str>) {
    let dir = repo.path().join(".factory/runs");
    std::fs::create_dir_all(&dir).unwrap();
    let pulse = serde_json::json!({
        "pid": std::process::id(), "started_at": last_tick_at, "last_tick_at": last_tick_at,
        "mode": "dispatch", "interval_s": 10, "max_ticks": 633, "ticks": 633,
        "stopped_at": stop_reason.map(|_| last_tick_at), "stop_reason": stop_reason,
    });
    std::fs::write(dir.join("serve.json"), pulse.to_string()).unwrap();
}

/// A loop that ticked a moment ago, so a test about something else sees a healthy header.
fn live_loop(repo: &Repo) {
    write_pulse(repo, &now_rfc3339(), None);
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
fn tui_once_config_view_shows_the_groups_with_labels_and_the_note() {
    let repo = fake_repo();
    let out = repo
        .asf()
        .args(["tui", "--once", "--view", "config"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    for title in ["MODELS", "BUDGETS", "FLOW"] {
        assert!(text.contains(title), "missing {title} in:\n{text}");
    }
    // the first screen of the list: labels instead of dotted keys
    for label in ["Planning", "Workers", "Reviewer", "Per issue", "Merge target"] {
        assert!(text.contains(label), "missing {label} in:\n{text}");
    }
    for (key, _) in asf_core::config_edit::editable_keys() {
        assert!(!text.contains(key), "dotted key {key} in:\n{text}");
    }
    assert!(text.contains("[7] Config"), "{text}");
    assert!(text.contains("local · github"), "{text}");
    // the first row (Planning) is selected, and only `asf plan` reads it
    assert!(
        text.contains(
            "Saved to config.toml · comments kept · the next `asf plan` uses this; `asf serve` never does"
        ),
        "{text}"
    );
}

#[test]
fn tui_once_config_view_reads_the_file_as_it_is_now() {
    let repo = fake_repo();
    let path = repo.path().join(".factory/config.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("max_parallel = 3", "max_parallel = 7")).unwrap();
    let out = repo
        .asf()
        .args(["tui", "--once", "--view", "config"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let row = text.lines().find(|l| l.contains("Workers at once")).unwrap();
    assert!(row.contains('7') && row.contains("default 3"), "{text}");
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
        footer.contains("d back to default") && !footer.contains("s start"),
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
    live_loop(&repo);
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
    // the header carries the age of the pulse, which moves between two runs: compare the rest
    let below_header = |t: &str| t.lines().skip(1).collect::<Vec<_>>().join("\n");
    assert!(lines[0].contains("loop ok "), "{first}");
    assert_eq!(below_header(&first), below_header(&tui_once_view(&repo, "now")));
    assert_ne!(below_header(&first), below_header(&tui_once_view(&repo, "board")));
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
    live_loop(&repo);

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

#[test]
fn tui_once_draws_the_same_frame_around_every_view() {
    let repo = fake_repo();
    let a = repo.create_issue("greeting", &["--ready", "--scope", "src/**"]);
    repo.ok(&["run", "start", &a]);
    let views = ["now", "board", "agents", "inbox", "queue", "log", "config"];
    for (n, view) in views.iter().enumerate() {
        let out = repo
            .asf()
            .args(["tui", "--once", "--view", view])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{view}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines.len() >= 4, "{view}:\n{text}");
        assert!(
            lines[0].contains("asf ·") && lines[0].contains("workers 1/3"),
            "{view} header: {:?}",
            lines[0]
        );
        let tab = format!("[{}] ", n + 1);
        assert!(lines[1].contains(&tab), "{view} tabs: {:?}", lines[1]);
        // a full-width rule closes the top chrome: under the tabs, or under the waiting banner
        let top = if lines[2].trim_start().starts_with('■') {
            3
        } else {
            2
        };
        assert_eq!(
            lines[top],
            "─".repeat(120),
            "{view}: a rule above the view:\n{text}"
        );
        let last = lines.len() - 1;
        assert_eq!(
            lines[last - 1],
            "─".repeat(120),
            "{view}: a rule above the key hints"
        );
        assert!(
            lines[last].ends_with("? help  q quit"),
            "{view} hints: {:?}",
            lines[last]
        );
    }
}

#[test]
fn tui_once_with_no_color_set_prints_the_same_text_and_still_no_escapes() {
    let repo = fake_repo();
    let out = repo
        .asf()
        .env("NO_COLOR", "1")
        .args(["tui", "--once", "--view", "now"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("NEEDS YOU") && !text.contains("\u{1b}["), "{text}");
}

#[test]
fn tui_once_header_says_loop_ok_with_a_fresh_pulse_and_no_banner() {
    let repo = fake_repo();
    live_loop(&repo);
    for view in ["now", "board", "queue"] {
        let text = tui_once_view(&repo, view);
        let head = line_with(&text, 0);
        assert!(
            head.contains("loop ok ") && !head.contains("LOOP STOPPED"),
            "{view}: {head}"
        );
        assert!(
            !text.contains("the loop"),
            "{view}:
{text}"
        );
    }
}

#[test]
fn tui_once_without_a_pulse_says_the_loop_is_not_running_everywhere() {
    let repo = fake_repo();
    let now = tui_once_view(&repo, "now");
    assert!(line_with(&now, 0).contains("loop not running"), "{now}");
    assert!(
        now.contains("NEEDS YOU (1)") && now.contains("Enter explain"),
        "{now}"
    );
    let board = tui_once_view(&repo, "board");
    assert!(
        line_with(&board, 2).contains("the loop is not running — start it: asf serve --dispatch"),
        "{board}"
    );
}

#[test]
fn tui_once_shows_why_a_stopped_loop_stopped_in_the_header_and_the_banner() {
    let repo = fake_repo();
    write_pulse(&repo, "2020-01-01T00:00:00Z", Some("max_ticks"));
    let board = tui_once_view(&repo, "board");
    assert!(line_with(&board, 0).contains("LOOP STOPPED"), "{board}");
    let banner = line_with(&board, 2);
    assert!(
        banner.contains("max_ticks (633) — start it again: asf serve --dispatch"),
        "{board}"
    );
    // a pulse that just went silent without a stop is "without a reason"
    write_pulse(&repo, "2020-01-01T00:00:00Z", None);
    assert!(line_with(&tui_once_view(&repo, "agents"), 2).contains("without a reason"));
}

/// The gate run in flight the way `asf serve` writes it (`.factory/runs/gate-live.json`).
fn write_gate_live(repo: &Repo, kind: &str, issue: &str, gate: &str, finished: &[(&str, u64)]) {
    let dir = repo.path().join(".factory/runs");
    std::fs::create_dir_all(&dir).unwrap();
    let live = serde_json::json!({
        "kind": kind, "issue": issue, "gates": ["alpha", "beta", "gamma"],
        "started_at": now_rfc3339(), "gate": gate, "gate_started_at": now_rfc3339(),
        "finished": finished
            .iter()
            .map(|(n, ms)| serde_json::json!({ "name": n, "ok": true, "duration_ms": ms, "tail": "ok" }))
            .collect::<Vec<_>>(),
        "tail": "compiling beta\nrunning 3 tests", "updated_at": now_rfc3339(),
    });
    std::fs::write(dir.join("gate-live.json"), live.to_string()).unwrap();
}

/// A config whose gates are no cargo commands: the steps must carry these names.
fn with_gates(repo: &Repo) {
    let cfg = repo.path().join(".factory/config.toml");
    let mut s = std::fs::read_to_string(&cfg).unwrap();
    for name in ["alpha", "beta", "gamma"] {
        s.push_str(&format!(
            "\n[[gate]]\nname = \"{name}\"\ncmd = [\"echo\", \"{name}\"]\ntimeout_s = 60\n"
        ));
    }
    std::fs::write(&cfg, s).unwrap();
}

#[test]
fn tui_once_queue_prints_the_first_item_expanded_with_the_gates_of_the_config_and_the_gate_in_flight() {
    let repo = fake_repo();
    with_gates(&repo);
    live_loop(&repo);
    let a = repo.create_issue("greeting", &["--ready", "--scope", "src/**"]);
    let b = repo.create_issue("docs", &["--ready", "--scope", "docs/**"]);
    let s = store(&repo);
    for (issue, state) in [(&a, MergeState::Gating), (&b, MergeState::Queued)] {
        let item = s
            .enqueue_merge(&issue.parse().unwrap(), None, &format!("feat/{issue}"))
            .unwrap();
        s.update_merge(
            item.id,
            &MergePatch {
                state: Some(state),
                ..MergePatch::default()
            },
        )
        .unwrap();
    }
    write_gate_live(&repo, "merge", &a, "beta", &[("alpha", 2000)]);
    let text = tui_once_view(&repo, "queue");
    assert!(text.contains("IN THE QUEUE (2)"), "{text}");
    for part in [
        "attempt 1",
        "rebase onto main",
        "✓ gate alpha",
        "● gate beta",
        "running",
        "○ gate gamma",
        "○ merge",
    ] {
        assert!(text.contains(part), "{part:?} missing:\n{text}");
    }
    assert!(!text.contains("cargo"), "{text}");
    // only the first item is open: the second one is its row and nothing else
    assert_eq!(
        text.lines().filter(|l| l.contains(&format!("feat/{b}"))).count(),
        1,
        "{text}"
    );
}

#[test]
fn tui_once_now_says_why_a_finishing_run_waits_and_what_a_verifying_run_runs() {
    let repo = fake_repo();
    with_gates(&repo);
    live_loop(&repo);
    let holder = repo.create_issue("holder", &["--ready", "--scope", "a/**"]);
    let waiting = repo.create_issue("waiter", &["--ready", "--scope", "b/**"]);
    repo.ok(&["run", "start", &holder]);
    repo.ok(&["run", "start", &waiting]);
    let s = store(&repo);
    let set = |issue: &str, state: RunState| {
        let run = s
            .runs(Some(&issue.parse().unwrap()), true)
            .unwrap()
            .pop()
            .unwrap();
        s.update_run(
            &run.id,
            &RunPatch {
                state: Some(state),
                ..RunPatch::default()
            },
        )
        .unwrap();
    };
    set(&holder, RunState::Verifying);
    set(&waiting, RunState::Finishing);
    write_gate_live(&repo, "verify", &holder, "beta", &[("alpha", 2000)]);
    let text = tui_once_view(&repo, "now");
    let wait = text
        .lines()
        .find(|l| l.contains("waiting for the gate slot"))
        .expect(&text);
    assert!(wait.contains(&format!("{holder} is on gate beta")), "{wait}");
    let verifying = text
        .lines()
        .find(|l| l.contains("gate beta running"))
        .expect(&text);
    assert!(verifying.contains("✓ alpha 2 s"), "{verifying}");
}
