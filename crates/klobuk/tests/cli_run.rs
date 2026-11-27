mod common;

use std::path::{Path, PathBuf};

use common::{git, Repo};
use klobuk_core::*;
use klobuk_runtime::FakeRuntime;
use klobuk_store::SqliteStore;

struct F {
    repo: Repo,
    issue: String,
}

/// Initialised repo on the fake runtime with `src/lib.rs` + `Cargo.toml` committed and a ready issue scoped to `src/**`.
fn setup() -> F {
    let repo = Repo::initialised();
    let cfg = repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("runtime = \"herdr\"", "runtime = \"fake\"");
    std::fs::write(&cfg, s).unwrap();
    std::fs::create_dir_all(repo.path().join("src")).unwrap();
    std::fs::write(repo.path().join("src/lib.rs"), "pub fn a() {}\n").unwrap();
    std::fs::write(repo.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "files"]);
    let issue = repo.create_issue(
        "add greeting",
        &["--ready", "--scope", "src/**", "--accept", "prints hello"],
    );
    F { repo, issue }
}

fn fake(f: &F) -> FakeRuntime {
    FakeRuntime::new(f.repo.path().join(".factory/fake-runtime.json"))
}

fn store(f: &F) -> SqliteStore {
    SqliteStore::open(&f.repo.path().join(".factory/factory.db")).unwrap()
}

fn active_run(f: &F) -> Run {
    store(f)
        .runs(Some(&f.issue.parse().unwrap()), false)
        .unwrap()
        .into_iter()
        .last()
        .unwrap()
}

fn wt(f: &F) -> PathBuf {
    f.repo.path().join(".worktrees").join(&f.issue)
}

fn ops(f: &F) -> Vec<String> {
    fake(f).log().into_iter().map(|e| e.op).collect()
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

#[test]
fn start_happy_path_creates_worktree_files_workspace_and_agent() {
    let f = setup();
    let res = f.repo.ok(&["run", "start", &f.issue]);
    assert_eq!(res["run"]["state"], "working");
    assert_eq!(res["run"]["branch"], "feat/asf-1");
    assert_eq!(res["run"]["worktree"], ".worktrees/asf-1");
    assert_eq!(res["workspace_id"], "f:w1");
    assert_eq!(res["pane_id"], "f:p1");
    let run_id = res["run"]["id"].as_str().unwrap().to_string();

    // claim + worktree + branch
    let claims = store(&f).active_claims().unwrap();
    assert_eq!(claims[0].worker, "asf-1");
    assert_eq!(claims[0].run_id.as_deref(), Some(run_id.as_str()));
    assert!(wt(&f).join("src/lib.rs").exists());
    let branch = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(wt(&f))
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&branch.stdout).trim(), "feat/asf-1");

    // brief, settings, CLAUDE.local.md
    // brief + settings live inside the worktree (the worker's cwd) under the gitignored .factory/runs/
    let runs_dir = wt(&f).join(".factory").join("runs").join("asf-1");
    let brief = std::fs::read_to_string(runs_dir.join("brief.md")).unwrap();
    assert!(brief.contains("add greeting") && brief.contains("prints hello") && brief.contains("src/**"));
    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(runs_dir.join("settings.json")).unwrap()).unwrap();
    assert!(settings["permissions"]["allow"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "Edit(src/**)"));
    let hook_cmd = settings["hooks"]["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(
        hook_cmd.ends_with("hook stop") && !hook_cmd.contains(' ') || hook_cmd.matches(' ').count() == 2,
        "{hook_cmd}"
    );
    assert!(wt(&f).join("CLAUDE.local.md").exists());
    let exclude = std::fs::read_to_string(f.repo.path().join(".git/info/exclude")).unwrap();
    assert!(exclude.lines().any(|l| l == "CLAUDE.local.md"));

    // runtime calls
    let log = fake(&f).log();
    let ops: Vec<&str> = log.iter().map(|e| e.op.as_str()).collect();
    assert_eq!(
        ops,
        ["health", "open_workspace", "report", "start_agent", "prompt"],
        "{ops:?}"
    );
    let env = &log[1].args["env"];
    assert!(
        env.as_array()
            .unwrap()
            .iter()
            .any(|p| p[0] == "KLOBUK_ISSUE" && p[1] == "asf-1"),
        "{env}"
    );
    assert!(
        env.as_array()
            .unwrap()
            .iter()
            .any(|p| p[0] == "KLOBUK_RUN" && p[1] == run_id),
        "{env}"
    );
    assert!(log[1].args["cwd"]
        .as_str()
        .unwrap()
        .replace('\\', "/")
        .ends_with(".worktrees/asf-1"));
    let args: Vec<String> = log[3].args["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap().to_string())
        .collect();
    assert!(
        args.windows(2)
            .any(|w| w[0] == "--permission-mode" && w[1] == "auto"),
        "{args:?}"
    );
    assert!(
        args.windows(2)
            .any(|w| w[0] == "--settings" && w[1].ends_with("settings.json") && !w[1].contains(' ')),
        "{args:?}"
    );
    assert!(
        args.windows(2)
            .any(|w| w[0] == "--session-id" && w[1].len() == 36),
        "{args:?}"
    );
    assert!(
        args.windows(2)
            .any(|w| w[0] == "--name" && w[1] == "asf-1-add-greeting"),
        "{args:?}"
    );
    assert_eq!(log[3].args["name"], "asf-1", "the Herdr agent name is unchanged");
    assert!(
        !args.iter().any(|a| a.starts_with("--remote-control")),
        "Remote Control is off by default: {args:?}"
    );
    assert!(log[4].args["text"].as_str().unwrap().contains("brief.md"));

    let run = active_run(&f);
    assert_eq!(run.state, RunState::Working);
    assert_eq!(run.workspace_id.as_deref(), Some("f:w1"));
    assert_eq!(run.agent_name.as_deref(), Some("asf-1"));
    assert_eq!(run.claude_session_id.as_deref().map(str::len), Some(36));
    assert_eq!(run.budget_usd, Some(5.0));
    assert_eq!(
        f.repo.ok(&["issue", "show", &f.issue])["issue"]["status"],
        "in-progress"
    );
}

#[test]
fn start_adds_remote_control_with_the_session_name_when_the_worker_switch_is_on() {
    let f = setup();
    let cfg = f.repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg).unwrap();
    assert!(s.contains("# remote_control = false"), "{s}");
    std::fs::write(
        &cfg,
        s.replacen("[worker]\n", "[worker]\nremote_control = true\n", 1),
    )
    .unwrap();
    f.repo.ok(&["run", "start", &f.issue]);
    let log = fake(&f).log();
    let start = log.iter().find(|e| e.op == "start_agent").unwrap();
    let args: Vec<&str> = start.args["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert!(
        args.windows(2)
            .any(|w| w == ["--remote-control", "asf-1-add-greeting"]),
        "{args:?}"
    );
    assert!(
        args.windows(2).any(|w| w == ["--name", "asf-1-add-greeting"]),
        "{args:?}"
    );
}

/// The brief a started run leaves in its worktree.
fn written_brief(f: &F) -> String {
    let path = wt(f).join(".factory/runs").join(&f.issue).join("brief.md");
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn start_writes_a_targeted_self_check_by_default_with_the_gates_of_the_config() {
    let f = setup();
    let cfg = f.repo.path().join(".factory/config.toml");
    let mut s = std::fs::read_to_string(&cfg).unwrap();
    assert!(s.contains("self_test = \"targeted\""), "{s}");
    s.push_str("\n[[gate]]\nname = \"lint\"\ncmd = [\"ruff\", \"check\", \".\"]\n");
    std::fs::write(&cfg, s).unwrap();
    f.repo.ok(&["run", "start", &f.issue]);
    let brief = written_brief(&f);
    assert!(brief.contains("Self-check before `finish` (targeted)"), "{brief}");
    assert!(brief.contains("Do NOT run the full test suite"), "{brief}");
    assert!(brief.contains("- `lint`: `ruff check .`"), "{brief}");
}

#[test]
fn start_writes_the_full_self_check_when_the_worker_option_says_full() {
    let f = setup();
    let cfg = f.repo.path().join(".factory/config.toml");
    let out = f.repo.ok(&["config", "set", "worker.self_test", "full"]);
    assert_eq!(out["new"], "full", "{out}");
    assert!(std::fs::read_to_string(&cfg)
        .unwrap()
        .contains("self_test = \"full\""));
    f.repo.ok(&["run", "start", &f.issue]);
    let brief = written_brief(&f);
    assert!(brief.contains("Self-check before `finish` (full)"), "{brief}");
    assert!(!brief.contains("Do NOT run the full test suite"), "{brief}");
}

#[test]
fn start_twice_is_conflict_and_touches_nothing() {
    let f = setup();
    f.repo.ok(&["run", "start", &f.issue]);
    let (code, ecode, _) = f.repo.err(&["run", "start", &f.issue]);
    assert_eq!((code, ecode.as_str()), (3, "conflict"));
    assert_eq!(ops(&f).iter().filter(|o| *o == "open_workspace").count(), 1);
    assert_eq!(store(&f).runs(None, false).unwrap().len(), 1);
}

#[test]
fn start_backlog_issue_is_conflict() {
    let f = setup();
    let b = f.repo.create_issue("later", &[]);
    let (code, ecode, msg) = f.repo.err(&["run", "start", &b]);
    assert_eq!((code, ecode.as_str()), (3, "conflict"));
    assert!(msg.contains("backlog"), "{msg}");
    assert!(!f.repo.path().join(".worktrees").join(&b).exists());
}

#[test]
fn start_answers_trust_prompt_automatically() {
    let f = setup();
    fake(&f)
        .script(serde_json::json!({
            "start_agent": [{"error": "not_ready"}, {"state": "idle"}],
            "screen": "Quick safety check: Is this a project you created?\n > No, exit\n   Yes, I trust this folder"
        }))
        .unwrap();
    let res = f.repo.ok(&["run", "start", &f.issue]);
    assert_eq!(res["run"]["state"], "working");
    assert_eq!(res["trusted"], true);
    let log = fake(&f).log();
    let keys = log.iter().find(|e| e.op == "send_keys").expect("send_keys call");
    assert_eq!(keys.args["keys"], serde_json::json!(["down", "enter"]));
    assert_eq!(ops(&f).iter().filter(|o| *o == "start_agent").count(), 2);
}

#[test]
fn a_startup_timeout_after_the_trust_answer_is_judged_by_a_re_read_of_the_agent() {
    let f = setup();
    let cfg = f.repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg).unwrap();
    std::fs::write(
        &cfg,
        s.replace("auto_trust = true", "start_timeout_ms = 1\nauto_trust = true"),
    )
    .unwrap();
    fake(&f)
        .script(serde_json::json!({
            "start_agent": [{"error": "not_ready"}, {"error": "timeout"}],
            "info": [{"state": "idle"}],
            "screen": "Quick safety check: Is this a project you created?\n > No, exit\n   Yes, I trust this folder"
        }))
        .unwrap();
    let res = f.repo.ok(&["run", "start", &f.issue]);
    assert_eq!(res["run"]["state"], "working", "{res}");
    assert_eq!(res["trusted"], true);
    assert_eq!(
        ops(&f).iter().filter(|o| *o == "start_agent").count(),
        2,
        "never restarted a third time"
    );
}

#[test]
fn start_blocked_for_other_reason_keeps_run_blocked_and_posts_blocker() {
    let f = setup();
    fake(&f)
        .script(serde_json::json!({ "start_agent": [{"error": "not_ready"}], "screen": "Please log in: run /login" }))
        .unwrap();
    let (code, ecode, msg) = f.repo.err(&["run", "start", &f.issue]);
    assert_eq!(code, 1);
    assert_eq!(ecode, "blocked");
    assert!(msg.contains("asf-1"), "{msg}");
    let run = active_run(&f);
    assert_eq!(run.state, RunState::Blocked);
    assert_eq!(
        store(&f).active_claims().unwrap().len(),
        1,
        "claim kept so the pane can be inspected"
    );
    let cs = store(&f).comments(&f.issue.parse().unwrap()).unwrap();
    let b = cs
        .iter()
        .find(|c| c.kind == CommentType::Blocker)
        .expect("BLOCKER");
    assert!(b.body.contains("log in"), "{}", b.body);
    assert_eq!(b.to_lane, "human");
}

#[test]
fn list_show_reply_keys_nudge() {
    let f = setup();
    f.repo.ok(&["run", "start", &f.issue]);
    let list = f.repo.ok(&["run", "list"]);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["issue_id"], "asf-1");
    let show = f.repo.ok(&["run", "show", &f.issue]);
    assert_eq!(show["run"]["state"], "working");
    assert_eq!(show["agent"]["state"], "working");
    f.repo.ok(&["run", "reply", &f.issue, "use println"]);
    f.repo.ok(&["run", "keys", &f.issue, "enter"]);
    f.repo.ok(&["run", "nudge", &f.issue]);
    let log = fake(&f).log();
    assert!(log
        .iter()
        .any(|e| e.op == "prompt" && e.args["text"] == "use println"));
    assert!(log
        .iter()
        .any(|e| e.op == "send_keys" && e.args["keys"] == serde_json::json!(["enter"])));
    assert!(log
        .iter()
        .any(|e| e.op == "prompt" && e.args["text"].as_str().unwrap().contains("issue finish")));
    let (code, ecode, _) = f.repo.err(&["run", "show", "asf-9"]);
    assert_eq!((code, ecode.as_str()), (1, "not_found"));
}

#[test]
fn abort_releases_claim_and_keeps_worktree() {
    let f = setup();
    f.repo.ok(&["run", "start", &f.issue]);
    let res = f.repo.ok(&["run", "abort", &f.issue]);
    assert_eq!(res["state"], "aborted");
    assert!(store(&f).active_claims().unwrap().is_empty());
    assert_eq!(
        f.repo.ok(&["issue", "show", &f.issue])["issue"]["status"],
        "ready"
    );
    assert!(wt(&f).exists(), "worktree kept for inspection");
    let log = fake(&f).log();
    assert!(log
        .iter()
        .any(|e| e.op == "send_keys" && e.args["keys"].as_array().unwrap().iter().any(|k| k == "esc")));
    assert!(log.iter().any(|e| e.op == "close_workspace"));
    assert_eq!(active_run(&f).state, RunState::Aborted);
}

#[test]
fn watch_verifies_finished_run_and_enqueues_merge() {
    let f = setup();
    f.repo.ok(&["run", "start", &f.issue]);
    // the worker: commits an in-scope change, writes the report, runs the finish ritual
    commit_in(&wt(&f), "src/greet.rs", "pub fn hello() {}\n");
    let report = wt(&f)
        .join(".factory")
        .join("runs")
        .join("asf-1")
        .join("report.md");
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
    fake(&f)
        .script(serde_json::json!({ "info": [{"state": "idle"}] }))
        .unwrap();

    let res = f.repo.ok(&["run", "watch", "--once"]);
    assert_eq!(res["checked"], 1);
    assert_eq!(res["transitions"][0]["to"], "succeeded", "{res}");
    let run = active_run(&f);
    assert_eq!(run.state, RunState::Succeeded);
    assert!(run.finished_at.is_some());
    let q = store(&f).merge_queue().unwrap();
    assert_eq!(q.len(), 1);
    assert_eq!(q[0].branch, "feat/asf-1");
    assert_eq!(q[0].state, MergeState::Queued);
    assert!(fake(&f).log().iter().any(|e| e.op == "notify"));
    // idempotent
    let res = f.repo.ok(&["run", "watch", "--once"]);
    assert_eq!(res["transitions"].as_array().unwrap().len(), 0);
}

#[test]
fn watch_fails_run_with_out_of_scope_change_and_dirty_tree() {
    let f = setup();
    f.repo.ok(&["run", "start", &f.issue]);
    commit_in(&wt(&f), "Cargo.toml", "[package]\nname = \"changed\"\n");
    std::fs::write(wt(&f).join("src/untracked.rs"), "").unwrap();
    let report = wt(&f)
        .join(".factory")
        .join("runs")
        .join("asf-1")
        .join("report.md");
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
    assert_eq!(res["transitions"][0]["to"], "failed", "{res}");
    let run = active_run(&f);
    assert_eq!(run.state, RunState::Failed);
    let reason = run.exit_reason.unwrap();
    assert!(
        reason.contains("Cargo.toml") && reason.contains("untracked.rs"),
        "{reason}"
    );
    let cs = store(&f).comments(&f.issue.parse().unwrap()).unwrap();
    assert!(cs
        .iter()
        .any(|c| c.kind == CommentType::Blocker && c.body.contains("Cargo.toml")));
    assert!(store(&f).merge_queue().unwrap().is_empty());
}

#[test]
fn watch_runs_gates_and_records_failure() {
    let f = setup();
    no_send_back(&f);
    let cfg = f.repo.path().join(".factory/config.toml");
    let mut s = std::fs::read_to_string(&cfg).unwrap();
    s.push_str("\n[[gate]]\nname = \"nope\"\ncmd = [\"python\", \"-c\", \"import sys; print('gate says no'); sys.exit(2)\"]\n");
    std::fs::write(&cfg, s).unwrap();
    f.repo.ok(&["run", "start", &f.issue]);
    commit_in(&wt(&f), "src/greet.rs", "");
    let report = wt(&f)
        .join(".factory")
        .join("runs")
        .join("asf-1")
        .join("report.md");
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
    assert_eq!(res["transitions"][0]["to"], "failed", "{res}");
    let reason = active_run(&f).exit_reason.unwrap();
    assert!(
        reason.contains("nope") && reason.contains("gate says no"),
        "{reason}"
    );
}

#[test]
fn watch_blocked_agent_posts_inbox_item_once_per_revision() {
    let f = setup();
    f.repo.ok(&["run", "start", &f.issue]);
    fake(&f)
        .script(serde_json::json!({ "info": [{"state": "blocked"}, {"state": "blocked"}], "screen": "Allow Bash(rm -rf)? (y/n)" }))
        .unwrap();
    // the fake applies scripted info on each `info` call; both say blocked with the same revision bump
    let res = f.repo.ok(&["run", "watch", "--once"]);
    assert_eq!(res["transitions"][0]["to"], "blocked", "{res}");
    let cs = store(&f).comments(&f.issue.parse().unwrap()).unwrap();
    let blockers: Vec<_> = cs.iter().filter(|c| c.kind == CommentType::Blocker).collect();
    assert_eq!(blockers.len(), 1);
    assert!(blockers[0].body.contains("rm -rf"), "{}", blockers[0].body);
    assert_eq!(active_run(&f).state, RunState::Blocked);
    f.repo.ok(&["run", "watch", "--once"]);
    let n = store(&f)
        .comments(&f.issue.parse().unwrap())
        .unwrap()
        .iter()
        .filter(|c| c.kind == CommentType::Blocker)
        .count();
    assert_eq!(
        n, 1,
        "no duplicate BLOCKER while the screen revision is unchanged"
    );
    // human answers via reply: the run goes back to working
    f.repo.ok(&["run", "reply", &f.issue, "y"]);
    assert_eq!(active_run(&f).state, RunState::Working);
}

// ---- SCOPE dialogs: the answer in the pane grants (or refuses) the scope -------------------------

const SCOPE_DIALOG: &str = "SCOPE: Cargo.toml because the greeting needs a new dependency\n \
     ❯ 1. Grant Cargo.toml\n   2. Refuse";

/// A started run whose worker posted the `SCOPE:` QUESTION for the record and now waits on the dialog
/// that `run watch` has quoted in a BLOCKER. Returns the QUESTION id.
fn scope_dialog_posted(f: &F, screen: &str) -> i64 {
    f.repo.ok(&["run", "start", &f.issue]);
    let q = f.repo.ok(&[
        "--lane",
        &f.issue,
        "issue",
        "comment",
        &f.issue,
        "--type",
        "QUESTION",
        "--to",
        "human",
        "--body",
        "SCOPE: Cargo.toml because the greeting needs a new dependency",
    ]);
    fake(f)
        .script(serde_json::json!({ "info": [{"state": "blocked"}], "screen": screen }))
        .unwrap();
    let res = f.repo.ok(&["run", "watch", "--once"]);
    assert_eq!(res["transitions"][0]["to"], "blocked", "{res}");
    assert_eq!(active_run(f).state, RunState::Blocked);
    q["id"].as_i64().unwrap()
}

fn file_scope(f: &F) -> serde_json::Value {
    f.repo.ok(&["issue", "show", &f.issue])["issue"]["file_scope"].clone()
}

fn keys_sent(f: &F) -> Vec<serde_json::Value> {
    fake(f)
        .log()
        .into_iter()
        .filter(|e| e.op == "send_keys")
        .map(|e| e.args["keys"].clone())
        .collect()
}

fn answers_to(f: &F, question: i64) -> Vec<Comment> {
    store(f)
        .comments(&f.issue.parse().unwrap())
        .unwrap()
        .into_iter()
        .filter(|c| c.kind == CommentType::Answer && c.reply_to == Some(question))
        .collect()
}

#[test]
fn answering_a_scope_dialog_with_the_grant_extends_the_scope_and_resolves_the_question() {
    let f = setup();
    let q = scope_dialog_posted(&f, SCOPE_DIALOG);
    assert_eq!(file_scope(&f), serde_json::json!(["src/**"]));

    f.repo.ok(&["run", "keys", &f.issue, "1"]);

    assert_eq!(file_scope(&f), serde_json::json!(["src/**", "Cargo.toml"]));
    let answers = answers_to(&f, q);
    assert_eq!(answers.len(), 1, "the QUESTION is answered once");
    assert_eq!(answers[0].body, "scope extended: Cargo.toml");
    assert_eq!(keys_sent(&f), vec![serde_json::json!(["1"])]);
    assert_eq!(active_run(&f).state, RunState::Working);
}

/// The same dialog with the cursor on the refusal (the human pressed Down in the overlay).
const SCOPE_DIALOG_CURSOR_ON_REFUSAL: &str =
    "SCOPE: Cargo.toml because the greeting needs a new dependency\n \
     \x20 1. Grant Cargo.toml\n ❯ 2. Refuse";

#[test]
fn confirming_a_scope_dialog_with_enter_on_the_grant_extends_the_scope() {
    let f = setup();
    let q = scope_dialog_posted(&f, SCOPE_DIALOG);

    f.repo.ok(&["run", "keys", &f.issue, "enter"]);

    assert_eq!(file_scope(&f), serde_json::json!(["src/**", "Cargo.toml"]));
    assert_eq!(answers_to(&f, q).len(), 1, "the QUESTION is answered once");
    assert_eq!(keys_sent(&f), vec![serde_json::json!(["enter"])]);
    assert_eq!(active_run(&f).state, RunState::Working);
}

/// The brief has the worker post the QUESTION first, so its Bash call (with the `SCOPE:` text) and a
/// numbered line of its own prose sit in the pane right above the dialog.
const SCOPE_DIALOG_BELOW_THE_RECORD: &str = "● Bash(klobuk issue comment klobuk-x --type QUESTION --body \"SCOPE: Cargo.toml because the greeting needs a new dependency\")\n  \
     Plan:\n  1. Ask for Cargo.toml\n  2. Wait for the answer\n \
     ☐ Scope\nSCOPE: Cargo.toml because the greeting needs a new dependency\n \
     ❯ 1. Grant Cargo.toml\n   2. Refuse";

#[test]
fn a_dialog_below_the_workers_own_record_of_the_question_still_grants() {
    let f = setup();
    let q = scope_dialog_posted(&f, SCOPE_DIALOG_BELOW_THE_RECORD);

    f.repo.ok(&["run", "keys", &f.issue, "1"]);

    assert_eq!(file_scope(&f), serde_json::json!(["src/**", "Cargo.toml"]));
    assert_eq!(answers_to(&f, q).len(), 1, "the QUESTION is answered once");
}

#[test]
fn enter_on_a_dialog_below_the_workers_own_record_of_the_question_grants_too() {
    let f = setup();
    let q = scope_dialog_posted(&f, SCOPE_DIALOG_BELOW_THE_RECORD);

    f.repo.ok(&["run", "keys", &f.issue, "enter"]);

    assert_eq!(file_scope(&f), serde_json::json!(["src/**", "Cargo.toml"]));
    assert_eq!(answers_to(&f, q).len(), 1);
}

#[test]
fn enter_is_judged_on_the_same_window_as_the_blocker_not_on_older_scrollback() {
    let f = setup();
    // 25 lines of older output above the dialog: an old SCOPE line and a numbered item of prose
    let old =
        "SCOPE: old.rs because earlier\n1. an older numbered item\n".to_string() + &"output\n".repeat(23);
    let q = scope_dialog_posted(&f, &format!("{old}{SCOPE_DIALOG}"));

    f.repo.ok(&["run", "keys", &f.issue, "enter"]);

    assert_eq!(file_scope(&f), serde_json::json!(["src/**", "Cargo.toml"]));
    assert_eq!(answers_to(&f, q).len(), 1);
}

#[test]
fn enter_with_the_cursor_on_the_refusal_only_sends_the_keys() {
    let f = setup();
    // the BLOCKER quoted the dialog with the cursor on option 1; the human then moved it down
    let q = scope_dialog_posted(&f, SCOPE_DIALOG);
    fake(&f)
        .script(serde_json::json!({ "screen": SCOPE_DIALOG_CURSOR_ON_REFUSAL }))
        .unwrap();

    f.repo.ok(&["run", "keys", &f.issue, "enter"]);

    assert_eq!(file_scope(&f), serde_json::json!(["src/**"]), "scope unchanged");
    assert!(answers_to(&f, q).is_empty(), "the QUESTION stays open");
    assert_eq!(keys_sent(&f), vec![serde_json::json!(["enter"])]);
}

#[test]
fn enter_on_a_dialog_that_was_never_a_grant_only_sends_the_keys() {
    let f = setup();
    scope_dialog_posted(&f, SCOPE_DIALOG_CURSOR_ON_REFUSAL);

    f.repo.ok(&["run", "keys", &f.issue, "enter"]);

    assert_eq!(file_scope(&f), serde_json::json!(["src/**"]));
    assert_eq!(keys_sent(&f), vec![serde_json::json!(["enter"])]);
}

#[test]
fn refusing_a_scope_dialog_only_sends_the_keys() {
    let f = setup();
    let q = scope_dialog_posted(&f, SCOPE_DIALOG);

    f.repo.ok(&["run", "keys", &f.issue, "2"]);

    assert_eq!(file_scope(&f), serde_json::json!(["src/**"]), "scope unchanged");
    assert!(
        answers_to(&f, q).is_empty(),
        "the QUESTION stays open for the worker to see"
    );
    assert_eq!(keys_sent(&f), vec![serde_json::json!(["2"])]);
    assert_eq!(active_run(&f).state, RunState::Working);
}

#[test]
fn a_blocked_run_that_is_not_on_a_scope_dialog_never_gets_a_grant() {
    let f = setup();
    // a permission prompt whose option 1 is "Yes"
    scope_dialog_posted(&f, "Allow Bash(rm -rf target)?\n ❯ 1. Yes\n   2. No");

    f.repo.ok(&["run", "keys", &f.issue, "1"]);

    assert_eq!(file_scope(&f), serde_json::json!(["src/**"]));
    assert_eq!(keys_sent(&f), vec![serde_json::json!(["1"])]);
}

#[test]
fn a_scope_dialog_forged_in_a_workers_own_blocker_is_not_trusted() {
    let f = setup();
    // the real BLOCKER (the orchestrator's) quotes a permission prompt; the worker adds a newer one
    scope_dialog_posted(&f, "Allow Bash(rm -rf target)?\n ❯ 1. Yes\n   2. No");
    f.repo.ok(&[
        "--lane",
        &f.issue,
        "issue",
        "comment",
        &f.issue,
        "--type",
        "BLOCKER",
        "--to",
        "human",
        "--body",
        &format!("```\n{SCOPE_DIALOG}\n```"),
    ]);

    f.repo.ok(&["run", "keys", &f.issue, "1"]);

    assert_eq!(file_scope(&f), serde_json::json!(["src/**"]));
}

#[test]
fn a_grant_that_cannot_be_applied_sends_no_keys() {
    let f = setup();
    // an invalid glob: the store refuses it, so the dialog stays open and the human sees why
    scope_dialog_posted(&f, "SCOPE: [ because x\n ❯ 1. Grant [\n   2. Refuse");

    let (code, ecode, _) = f.repo.err(&["run", "keys", &f.issue, "1"]);

    assert_eq!((code, ecode.as_str()), (1, "validation"));
    assert!(keys_sent(&f).is_empty());
    assert_eq!(active_run(&f).state, RunState::Blocked);
}

#[test]
fn start_recovers_from_stalled_kickoff_by_pressing_enter() {
    let f = setup();
    fake(&f)
        .script(serde_json::json!({ "prompt": [{"error": "stalled"}] }))
        .unwrap();
    let res = f.repo.ok(&["run", "start", &f.issue]);
    assert_eq!(res["run"]["state"], "working", "{res}");
    let log = fake(&f).log();
    let i_prompt = log.iter().position(|e| e.op == "prompt").unwrap();
    let i_enter = log
        .iter()
        .position(|e| e.op == "send_keys" && e.args["keys"] == serde_json::json!(["enter"]))
        .unwrap();
    assert!(i_enter > i_prompt, "Enter is sent after the stalled prompt");
    assert_eq!(
        log.iter().filter(|e| e.op == "prompt").count(),
        1,
        "the prompt is not typed twice"
    );
}

fn enters(f: &F) -> usize {
    fake(f)
        .log()
        .iter()
        .filter(|e| e.op == "send_keys" && e.args["keys"] == serde_json::json!(["enter"]))
        .count()
}

/// Sets `[worker] start_timeout_ms`, the window every wait of the start flow uses.
fn start_window(f: &F, ms: u64) {
    let cfg = f.repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg).unwrap();
    assert!(s.contains("auto_trust = true"), "config template changed");
    std::fs::write(
        &cfg,
        s.replace(
            "auto_trust = true",
            &format!(
                "start_timeout_ms = {ms}
auto_trust = true"
            ),
        ),
    )
    .unwrap();
}

/// `timeout_ms` of every `wait` call, in order.
fn wait_timeouts(f: &F) -> Vec<u64> {
    fake(f)
        .log()
        .iter()
        .filter(|e| e.op == "wait")
        .filter_map(|e| e.args["timeout_ms"].as_u64())
        .collect()
}

fn no_blocker(f: &F) -> bool {
    store(f)
        .comments(&f.issue.parse().unwrap())
        .unwrap()
        .iter()
        .all(|c| c.kind != CommentType::Blocker)
}

#[test]
fn start_presses_one_extra_enter_after_waiting_the_whole_start_window() {
    // the prompt stalled and its Enter was lost: the box still holds the text
    let f = setup();
    start_window(&f, 4_321);
    fake(&f)
        .script(serde_json::json!({ "prompt": [{"error": "stalled"}] }))
        .unwrap();
    let res = f.repo.ok(&["run", "start", &f.issue]);
    assert_eq!(res["run"]["state"], "working", "{res}");
    assert_eq!(enters(&f), 1);
    let log = fake(&f).log();
    let i_wait = log.iter().position(|e| e.op == "wait").expect("a wait first");
    let i_enter = log.iter().position(|e| e.op == "send_keys").unwrap();
    assert!(
        i_wait < i_enter,
        "the wait comes before the extra press: {:?}",
        ops(&f)
    );
    assert_eq!(
        wait_timeouts(&f)[0],
        4_321,
        "the wait is as long as the start window, not a few seconds"
    );
    assert_eq!(count_op(&f, "prompt"), 1, "the prompt is never typed twice");
    assert!(no_blocker(&f));
}

#[test]
fn start_succeeds_when_the_runtime_reports_working_late_after_the_prompts_own_submit() {
    // the asf-109 shape: the submission inside `agent prompt` worked, Herdr says so 1.5 s late
    // (live it was up to 90 s); pressing Enter would only land in an empty box
    let f = setup();
    start_window(&f, 5_000);
    fake(&f)
        .script(serde_json::json!({ "prompt": [{"error": "stalled", "lag_ms": 1500}] }))
        .unwrap();
    let t = std::time::Instant::now();
    let res = f.repo.ok(&["run", "start", &f.issue]);
    assert_eq!(res["run"]["state"], "working", "{res}");
    assert!(
        t.elapsed() >= std::time::Duration::from_millis(1_400),
        "it waited for the runtime"
    );
    assert_eq!(
        enters(&f),
        0,
        "no key while the first submission may still be landing"
    );
    assert_eq!(count_op(&f, "prompt"), 1);
    assert_eq!(count_op(&f, "close_workspace"), 0);
    assert!(no_blocker(&f));
}

#[test]
fn start_succeeds_when_the_extra_enter_is_reported_late_too() {
    let f = setup();
    start_window(&f, 5_000);
    fake(&f)
        .script(serde_json::json!({ "prompt": [{"error": "stalled", "enter_lag_ms": 1500}] }))
        .unwrap();
    let res = f.repo.ok(&["run", "start", &f.issue]);
    assert_eq!(res["run"]["state"], "working", "{res}");
    assert_eq!(enters(&f), 1, "one extra press, then it waits for the answer");
    assert_eq!(wait_timeouts(&f), [5_000, 5_000]);
    assert!(no_blocker(&f));
}

#[test]
fn start_does_not_trust_a_prompt_call_that_timed_out() {
    // `prompt` ended in `timeout` and the agent is still idle: the text was never submitted
    let f = setup();
    fake(&f)
        .script(serde_json::json!({ "prompt": [{"error": "timeout"}] }))
        .unwrap();
    let res = f.repo.ok(&["run", "start", &f.issue]);
    assert_eq!(res["run"]["state"], "working", "{res}");
    assert_eq!(
        enters(&f),
        1,
        "an idle agent after the kick-off gets the submit key"
    );
    assert_eq!(count_op(&f, "prompt"), 1);
}

#[test]
fn start_blocks_when_the_kickoff_stays_unsent_after_the_extra_submit_key() {
    let f = setup();
    fake(&f)
        .script(serde_json::json!({ "prompt": [{"error": "stalled", "swallow_enter": 99}] }))
        .unwrap();
    let (code, ecode, msg) = f.repo.err(&["run", "start", &f.issue]);
    assert_eq!((code, ecode.as_str()), (1, "blocked"), "{msg}");
    assert!(msg.contains("not submitted"), "{msg}");
    assert!(msg.contains("1 submit key press"), "{msg}");
    assert_eq!(
        enters(&f),
        1,
        "one extra submit key after a real wait, not a burst"
    );
    assert_eq!(
        wait_timeouts(&f),
        [15_000, 75_000],
        "a short first window, the key, then what is left of the 90 s start budget"
    );
    assert_eq!(count_op(&f, "prompt"), 1);
    assert_eq!(active_run(&f).state, RunState::Blocked);
    let cs = store(&f).comments(&f.issue.parse().unwrap()).unwrap();
    assert_eq!(
        cs.iter().filter(|c| c.kind == CommentType::Blocker).count(),
        1,
        "{cs:?}"
    );
}

#[test]
fn start_presses_the_submit_key_after_a_short_first_window_not_the_whole_budget() {
    // asf-164: the prompt was typed, the Enter lost, and the key came after the full 90 s or never
    let f = setup();
    fake(&f)
        .script(serde_json::json!({ "prompt": [{"error": "timeout"}] }))
        .unwrap();
    let res = f.repo.ok(&["run", "start", &f.issue]);
    assert_eq!(res["run"]["state"], "working", "{res}");
    assert_eq!(
        enters(&f),
        1,
        "an idle pane with the kick-off in its input gets the key"
    );
    assert_eq!(
        wait_timeouts(&f)[0],
        15_000,
        "the first wait is the short window, not the 90 s start budget"
    );
    assert_eq!(count_op(&f, "prompt"), 1, "the prompt is never typed twice");
    assert!(no_blocker(&f));
}

#[test]
fn start_reports_a_kickoff_line_that_is_not_on_the_pane_as_the_reason_for_no_key() {
    // the line scrolled out of the 60 lines that are read: nothing proves there is text to submit
    let f = setup();
    let noise = "output line\n".repeat(100);
    fake(&f)
        .script(serde_json::json!({ "prompt": [{"error": "stalled", "swallow_enter": 99}], "screen": noise }))
        .unwrap();
    let (code, ecode, msg) = f.repo.err(&["run", "start", &f.issue]);
    assert_eq!((code, ecode.as_str()), (1, "blocked"), "{msg}");
    assert!(msg.contains("0 submit key press"), "{msg}");
    assert!(msg.contains("kick-off line not found on the pane"), "{msg}");
    assert_eq!(enters(&f), 0, "no key without the line in the input box");
    assert_eq!(
        wait_timeouts(&f),
        [15_000, 75_000],
        "the first window ended without a key, the rest of the budget was still waited"
    );
}

#[test]
fn start_presses_the_submit_key_when_the_input_box_wrapped_the_kickoff_line() {
    // asf-164 suspect: the input box breaks the typed line mid-word and draws its border between the
    // halves, so the line is never found as one string; only the dense match (no whitespace, no
    // box-drawing characters) still sees it. The raw echo is scrolled out of the 60 lines read.
    let f = setup();
    let id = &f.issue;
    let kick = format!("Start {id}: read .factory/runs/{id}/brief.md and follow it.");
    // inside the first 40 characters that the probe compares, else the first half alone would match
    let cut = kick.find("factory").unwrap() + 3;
    let (head, tail) = kick.split_at(cut);
    let box_screen = format!(
        "{}\n╭{}╮\n│ > {head}{}│\n│   {tail}{}│\n╰{}╯",
        "output line\n".repeat(60).trim_end(),
        "─".repeat(70),
        " ".repeat(70 - 4 - head.len()),
        " ".repeat(70 - 4 - tail.len()),
        "─".repeat(70),
    );
    fake(&f)
        .script(serde_json::json!({ "prompt": [{"error": "stalled"}], "screen": box_screen }))
        .unwrap();
    let res = f.repo.ok(&["run", "start", &f.issue]);
    assert_eq!(res["run"]["state"], "working", "{res}");
    assert_eq!(enters(&f), 1, "the wrapped kick-off line is still found");
    assert!(no_blocker(&f));
}

#[test]
fn start_reports_an_agent_that_is_not_idle_as_the_reason_for_no_key() {
    let f = setup();
    fake(&f)
        .script(serde_json::json!({
            "prompt": [{"error": "stalled", "swallow_enter": 99}],
            "info": vec![serde_json::json!({"state": "unknown"}); 20]
        }))
        .unwrap();
    let (code, ecode, msg) = f.repo.err(&["run", "start", &f.issue]);
    assert_eq!((code, ecode.as_str()), (1, "blocked"), "{msg}");
    assert!(
        msg.contains("agent still unknown after 0 submit key press"),
        "{msg}"
    );
    assert!(msg.contains("state not idle"), "{msg}");
    assert_eq!(
        enters(&f),
        0,
        "Enter on a pane Herdr cannot classify could answer a dialog"
    );
}

#[test]
fn start_tolerates_a_startup_detection_timeout_when_the_agent_comes_up() {
    let f = setup();
    fake(&f)
        .script(serde_json::json!({ "start_agent": [{"error": "timeout"}], "info": [{"state": "idle"}] }))
        .unwrap();
    let res = f.repo.ok(&["run", "start", &f.issue]);
    assert_eq!(res["run"]["state"], "working", "{res}");
    assert_eq!(
        fake(&f).log().iter().filter(|e| e.op == "start_agent").count(),
        1,
        "never restarted"
    );
}

// ---- start retry after a startup-detection timeout --------------------------------------------

/// Shrinks the startup window, so a timed-out start is judged after one `info` read instead of 90 s.
fn short_start_window(f: &F) {
    let cfg = f.repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg).unwrap();
    assert!(s.contains("auto_trust = true"), "config template changed");
    std::fs::write(
        &cfg,
        s.replace("auto_trust = true", "start_timeout_ms = 1\nauto_trust = true"),
    )
    .unwrap();
}

fn count_op(f: &F, op: &str) -> usize {
    fake(f).log().iter().filter(|e| e.op == op).count()
}

fn start_retries(f: &F) -> Vec<Event> {
    store(f)
        .events_since(0, 1000)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "run.start_retry")
        .collect()
}

#[test]
fn start_timeout_with_an_idle_or_done_agent_continues_without_a_retry() {
    for state in ["idle", "done"] {
        let f = setup();
        short_start_window(&f);
        fake(&f)
            .script(serde_json::json!({ "start_agent": [{"error": "timeout"}], "info": [{"state": state}] }))
            .unwrap();
        let res = f.repo.ok(&["run", "start", &f.issue]);
        assert_eq!(res["run"]["state"], "working", "{state}: {res}");
        assert_eq!(count_op(&f, "start_agent"), 1, "{state}: never restarted");
        assert_eq!(count_op(&f, "open_workspace"), 1, "{state}: one workspace");
        assert_eq!(count_op(&f, "close_workspace"), 0, "{state}: nothing closed");
        assert!(start_retries(&f).is_empty(), "{state}: no retry recorded");
    }
}

#[test]
fn start_timeout_with_a_dead_pane_retries_once_on_a_fresh_workspace() {
    let f = setup();
    short_start_window(&f);
    // the first start times out and the agent never leaves `unknown`; the second start is unscripted (idle)
    fake(&f)
        .script(serde_json::json!({ "start_agent": [{"error": "timeout"}] }))
        .unwrap();
    let res = f.repo.ok(&["run", "start", &f.issue]);
    assert_eq!(res["run"]["state"], "working", "{res}");
    assert_eq!(
        res["workspace_id"], "f:w2",
        "the run now lives in the new workspace"
    );
    assert_eq!(res["pane_id"], "f:p2");
    assert_eq!(res["run"]["workspace_id"], "f:w2");
    assert_eq!(res["run"]["pane_id"], "f:p2");

    let log = fake(&f).log();
    let pos = |op: &str, nth: usize| {
        log.iter()
            .enumerate()
            .filter(|(_, e)| e.op == op)
            .nth(nth)
            .map(|(i, _)| i)
            .unwrap_or_else(|| panic!("no {op} #{nth} in {:?}", ops(&f)))
    };
    assert_eq!(count_op(&f, "start_agent"), 2, "exactly one retry");
    assert_eq!(count_op(&f, "open_workspace"), 2);
    assert_eq!(count_op(&f, "close_workspace"), 1);
    assert_eq!(log[pos("close_workspace", 0)].args["workspace_id"], "f:w1");
    assert!(
        pos("start_agent", 0) < pos("close_workspace", 0)
            && pos("close_workspace", 0) < pos("open_workspace", 1)
            && pos("open_workspace", 1) < pos("start_agent", 1),
        "{:?}",
        ops(&f)
    );
    assert_eq!(log[pos("start_agent", 1)].args["pane_id"], "f:p2");
    let session = |i: usize| {
        let a = log[i].args["args"].as_array().unwrap();
        let k = a.iter().position(|x| x == "--session-id").unwrap();
        a[k + 1].as_str().unwrap().to_string()
    };
    assert_ne!(
        session(pos("start_agent", 0)),
        session(pos("start_agent", 1)),
        "a retry must not reuse the session id of the abandoned launch"
    );
    assert_eq!(
        res["run"]["claude_session_id"].as_str().unwrap(),
        session(pos("start_agent", 1))
    );

    let ev = start_retries(&f);
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert_eq!(
        ev[0].issue_id.as_ref().map(|i| i.as_str()),
        Some(f.issue.as_str())
    );
    assert_eq!(ev[0].run_id.as_deref(), Some(active_run(&f).id.as_str()));
    let reason = ev[0].payload["reason"].as_str().expect("reason");
    assert!(
        reason.contains("timed out") && reason.contains("unknown"),
        "{reason}"
    );
    assert_eq!(store(&f).active_claims().unwrap().len(), 1);
}

#[test]
fn start_blocks_with_a_blocker_after_a_second_startup_failure() {
    let f = setup();
    short_start_window(&f);
    fake(&f)
        .script(serde_json::json!({ "start_agent": [{"error": "timeout"}, {"error": "timeout"}] }))
        .unwrap();
    let (code, ecode, msg) = f.repo.err(&["run", "start", &f.issue]);
    assert_eq!((code, ecode.as_str()), (1, "blocked"), "{msg}");
    assert!(msg.contains("asf-1") && msg.contains("timed out"), "{msg}");

    assert_eq!(count_op(&f, "start_agent"), 2, "one retry, not more");
    assert_eq!(start_retries(&f).len(), 1);
    let run = active_run(&f);
    assert_eq!(run.state, RunState::Blocked);
    assert_eq!(
        run.workspace_id.as_deref(),
        Some("f:w2"),
        "the second pane is kept for inspection"
    );
    assert_eq!(
        count_op(&f, "close_workspace"),
        1,
        "only the abandoned first workspace is closed"
    );
    assert_eq!(store(&f).active_claims().unwrap().len(), 1, "claim kept");
    let cs = store(&f).comments(&f.issue.parse().unwrap()).unwrap();
    let blockers: Vec<_> = cs.iter().filter(|c| c.kind == CommentType::Blocker).collect();
    assert_eq!(blockers.len(), 1, "{cs:?}");
    assert_eq!(blockers[0].to_lane, "human");
    assert!(
        blockers[0].body.contains("could not start"),
        "{}",
        blockers[0].body
    );
}

#[test]
fn a_start_that_fails_for_another_reason_is_not_retried() {
    let f = setup();
    short_start_window(&f);
    fake(&f)
        .script(serde_json::json!({ "start_agent": [{"error": "not_ready"}], "screen": "Please log in: run /login" }))
        .unwrap();
    let (code, ecode, _) = f.repo.err(&["run", "start", &f.issue]);
    assert_eq!((code, ecode.as_str()), (1, "blocked"));
    assert_eq!(count_op(&f, "start_agent"), 1);
    assert_eq!(count_op(&f, "close_workspace"), 0);
    assert!(start_retries(&f).is_empty());
}

// ---- a pane whose shell is not ready yet ----------------------------------------------------------

#[test]
fn start_polls_a_pane_that_is_not_an_available_shell_yet() {
    // 2026-11-08: `agent start` right after `workspace create` answered agent_pane_busy under load
    let f = setup();
    start_window(&f, 5_000);
    fake(&f)
        .script(serde_json::json!({ "start_agent": [{"error": "pane_busy"}, {"error": "pane_busy"}] }))
        .unwrap();
    let res = f.repo.ok(&["run", "start", &f.issue]);
    assert_eq!(res["run"]["state"], "working", "{res}");
    assert_eq!(count_op(&f, "start_agent"), 3, "two refusals, then the start");
    assert_eq!(count_op(&f, "open_workspace"), 1, "same workspace, no new pane");
    assert_eq!(count_op(&f, "close_workspace"), 0);
    assert!(no_blocker(&f));
}

#[test]
fn start_blocks_when_the_pane_never_becomes_a_shell() {
    let f = setup();
    start_window(&f, 1_200);
    fake(&f)
        .script(serde_json::json!({ "start_agent": vec![serde_json::json!({"error": "pane_busy"}); 60] }))
        .unwrap();
    let (code, ecode, msg) = f.repo.err(&["run", "start", &f.issue]);
    assert_eq!((code, ecode.as_str()), (1, "blocked"), "{msg}");
    assert!(msg.contains("agent_pane_busy"), "{msg}");
    let tries = count_op(&f, "start_agent");
    assert!(
        (2..=6).contains(&tries),
        "polled for a bounded time, not once and not forever: {tries}"
    );
    assert_eq!(active_run(&f).state, RunState::Blocked);
}

// ---- `klobuk run verify`-------------------------------------------------------------------------

/// A failed gate in the worker's verification goes back to the idle worker while `gate.max_rounds`
/// allows it (tested in `cli_merge.rs`). The tests that call this are about the BLOCKER and the failed
/// run that follow when the worker is not sent back, so they switch the send-back off.
fn no_send_back(f: &F) {
    f.repo.ok(&["config", "set", "gate.max_rounds", "0"]);
}

/// Adds a gate that passes only once `src/fixed.rs` exists in the worktree.
fn add_fixed_file_gate(f: &F) {
    no_send_back(f);
    let cfg = f.repo.path().join(".factory/config.toml");
    let mut s = std::fs::read_to_string(&cfg).unwrap();
    s.push_str(
        "\n[[gate]]\nname = \"needs-fix\"\ncmd = [\"python\", \"-c\", \"import os, sys; print('fix missing'); sys.exit(0 if os.path.exists('src/fixed.rs') else 2)\"]\n",
    );
    std::fs::write(&cfg, s).unwrap();
}

/// The worker's side: commit an in-scope change, write the report, run `issue finish`.
fn worker_finishes(f: &F) {
    commit_in(&wt(f), "src/greet.rs", "pub fn hello() {}\n");
    let report = wt(f)
        .join(".factory")
        .join("runs")
        .join(&f.issue)
        .join("report.md");
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
}

fn blockers(f: &F) -> usize {
    store(f)
        .comments(&f.issue.parse().unwrap())
        .unwrap()
        .iter()
        .filter(|c| c.kind == CommentType::Blocker)
        .count()
}

#[test]
fn verify_succeeds_after_a_failed_gate_is_fixed_and_enqueues_the_branch() {
    let f = setup();
    add_fixed_file_gate(&f);
    f.repo.ok(&["run", "start", &f.issue]);
    worker_finishes(&f);
    let res = f.repo.ok(&["run", "watch", "--once"]);
    assert_eq!(res["transitions"][0]["to"], "failed", "{res}");
    assert!(active_run(&f).exit_reason.unwrap().contains("needs-fix"));
    assert!(store(&f).merge_queue().unwrap().is_empty());

    // still broken: verify fails again, the run stays failed and a new BLOCKER is posted
    let res = f.repo.ok(&["run", "verify", &f.issue]);
    assert_eq!(
        (res["from"].as_str(), res["to"].as_str()),
        (Some("failed"), Some("failed")),
        "{res}"
    );
    assert_eq!(active_run(&f).state, RunState::Failed);
    assert!(active_run(&f).exit_reason.unwrap().contains("fix missing"));
    assert_eq!(blockers(&f), 2);
    assert!(store(&f).merge_queue().unwrap().is_empty());

    // fix it, verify again
    commit_in(&wt(&f), "src/fixed.rs", "\n");
    let res = f.repo.ok(&["run", "verify", &f.issue]);
    assert_eq!(res["from"], "failed", "{res}");
    assert_eq!(res["to"], "succeeded", "{res}");
    let run = active_run(&f);
    assert_eq!(run.state, RunState::Succeeded);
    assert_eq!(run.exit_reason.as_deref(), Some("verified"));
    let q = store(&f).merge_queue().unwrap();
    assert_eq!(q.len(), 1);
    assert_eq!(q[0].branch, "feat/asf-1");
    assert_eq!(q[0].state, MergeState::Queued);
    assert_eq!(blockers(&f), 2, "no BLOCKER on success");
}

#[test]
fn verify_ignores_the_stored_base_sha_when_the_branch_was_rebased_onto_a_moved_main() {
    let f = setup();
    f.repo.ok(&["run", "start", &f.issue]);
    commit_in(f.repo.path(), "README.md", "# moved on\n");
    commit_in(f.repo.path(), "docs/notes.txt", "also moved\n");
    git(&wt(&f), &["rebase", "main"]);
    worker_finishes(&f);

    let res = f.repo.ok(&["run", "verify", &f.issue]);
    assert_eq!(res["to"], "succeeded", "{res}");
    assert!(
        res["detail"].as_str().unwrap().starts_with("1 files"),
        "only src/greet.rs is the worker's: {res}"
    );
    assert_eq!(active_run(&f).exit_reason.as_deref(), Some("verified"));
}

#[test]
fn verify_accepts_a_finishing_run_without_waiting_for_watch() {
    let f = setup();
    f.repo.ok(&["run", "start", &f.issue]);
    worker_finishes(&f);
    assert_eq!(active_run(&f).state, RunState::Finishing);
    let res = f.repo.ok(&["run", "verify", &f.issue]);
    assert_eq!(res["to"], "succeeded", "{res}");
    assert_eq!(store(&f).merge_queue().unwrap().len(), 1);
}

#[test]
fn verify_without_a_run_is_a_conflict() {
    let f = setup();
    let (code, ecode, msg) = f.repo.err(&["run", "verify", &f.issue]);
    assert_eq!((code, ecode.as_str()), (3, "conflict"));
    assert_eq!(msg, "no run to verify");
}

#[test]
fn verify_of_a_succeeded_run_is_a_conflict() {
    let f = setup();
    f.repo.ok(&["run", "start", &f.issue]);
    worker_finishes(&f);
    f.repo.ok(&["run", "verify", &f.issue]);
    let (code, ecode, msg) = f.repo.err(&["run", "verify", &f.issue]);
    assert_eq!((code, ecode.as_str()), (3, "conflict"));
    assert_eq!(msg, "already verified and queued");
    assert_eq!(store(&f).merge_queue().unwrap().len(), 1);
}

#[test]
fn verify_while_the_worker_is_active_is_a_conflict() {
    let f = setup();
    f.repo.ok(&["run", "start", &f.issue]);
    for state in [RunState::Starting, RunState::Working, RunState::Blocked] {
        store(&f)
            .update_run(
                &active_run(&f).id,
                &RunPatch {
                    state: Some(state),
                    ..RunPatch::default()
                },
            )
            .unwrap();
        let (code, ecode, msg) = f.repo.err(&["run", "verify", &f.issue]);
        assert_eq!((code, ecode.as_str()), (3, "conflict"), "{state}");
        assert_eq!(
            msg, "worker still active; wait for `klobuk issue finish` or `klobuk run abort`",
            "{state}"
        );
        assert_eq!(active_run(&f).state, state, "refusal changes nothing");
    }
}

#[test]
fn verify_of_an_aborted_run_is_a_conflict() {
    let f = setup();
    f.repo.ok(&["run", "start", &f.issue]);
    f.repo.ok(&["run", "abort", &f.issue]);
    let (code, ecode, msg) = f.repo.err(&["run", "verify", &f.issue]);
    assert_eq!((code, ecode.as_str()), (3, "conflict"));
    assert_eq!(msg, "run was aborted; dispatch again");
}

#[test]
fn verify_is_refused_inside_a_worker_run() {
    let f = setup();
    f.repo.ok(&["run", "start", &f.issue]);
    worker_finishes(&f);
    let out = f
        .repo
        .klobuk()
        .env("KLOBUK_RUN", "01RUN")
        .args(["run", "verify", &f.issue, "--json"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["error"]["code"], "forbidden", "{v}");
    assert_eq!(active_run(&f).state, RunState::Finishing, "nothing was verified");
}

// ---- report verification ----------------------------------------------------------------------

/// Like `worker_finishes`, but with the given report text.
fn worker_finishes_with(f: &F, report_text: &str) {
    commit_in(&wt(f), "src/greet.rs", "pub fn hello() {}\n");
    let report = wt(f)
        .join(".factory")
        .join("runs")
        .join(&f.issue)
        .join("report.md");
    std::fs::write(&report, report_text).unwrap();
    f.repo.ok(&[
        "--lane",
        &f.issue,
        "issue",
        "finish",
        &f.issue,
        "--report",
        report.to_str().unwrap(),
    ]);
}

fn issue_status(f: &F) -> String {
    f.repo.ok(&["issue", "show", &f.issue])["issue"]["status"]
        .as_str()
        .unwrap()
        .to_string()
}

fn archived_report(f: &F) -> PathBuf {
    f.repo
        .path()
        .join(".factory")
        .join("runs")
        .join(active_run(f).id)
        .join("report.md")
}

#[test]
fn clean_report_sets_validating_and_archives_report_under_the_run_id() {
    let f = setup();
    f.repo.ok(&["run", "start", &f.issue]);
    worker_finishes_with(&f, "# done\n\n## Open questions\n\nNone.\n");
    let res = f.repo.ok(&["run", "verify", &f.issue]);
    assert_eq!(res["to"], "succeeded", "{res}");
    assert_eq!(issue_status(&f), "validating");
    assert_eq!(
        std::fs::read_to_string(archived_report(&f)).unwrap(),
        "# done\n\n## Open questions\n\nNone.\n",
        "report.md is copied to <root>/.factory/runs/<run-id>/"
    );
    assert_eq!(blockers(&f), 0);
}

#[test]
fn report_with_open_questions_fails_verification_and_blocks_the_issue() {
    let f = setup();
    f.repo.ok(&["run", "start", &f.issue]);
    worker_finishes_with(&f, "# done\n\n## OPEN QUESTIONS\n\n- should hello() print?\n");
    let res = f.repo.ok(&["run", "verify", &f.issue]);
    assert_eq!(res["to"], "failed", "{res}");
    assert_eq!(issue_status(&f), "blocked");
    assert_eq!(active_run(&f).state, RunState::Failed);
    let cs = store(&f).comments(&f.issue.parse().unwrap()).unwrap();
    let b = cs
        .iter()
        .find(|c| c.kind == CommentType::Blocker && c.body.to_lowercase().contains("open questions"))
        .expect("BLOCKER naming the open questions");
    assert_eq!(b.to_lane, "human");
    assert!(store(&f).merge_queue().unwrap().is_empty());
    assert_eq!(
        std::fs::read_to_string(archived_report(&f)).unwrap(),
        "# done\n\n## OPEN QUESTIONS\n\n- should hello() print?\n",
        "the report is archived first, even when verification fails"
    );
}

#[test]
fn empty_open_questions_heading_is_not_a_problem() {
    let f = setup();
    f.repo.ok(&["run", "start", &f.issue]);
    worker_finishes_with(&f, "# done\n\n## Open questions\n\n## Notes\n\nfine\n");
    let res = f.repo.ok(&["run", "verify", &f.issue]);
    assert_eq!(res["to"], "succeeded", "{res}");
    assert_eq!(issue_status(&f), "validating");
}

#[test]
fn failed_gate_also_blocks_the_issue_and_archives_the_report() {
    let f = setup();
    add_fixed_file_gate(&f);
    f.repo.ok(&["run", "start", &f.issue]);
    worker_finishes(&f);
    let res = f.repo.ok(&["run", "watch", "--once"]);
    assert_eq!(res["transitions"][0]["to"], "failed", "{res}");
    assert_eq!(issue_status(&f), "blocked");
    assert!(archived_report(&f).is_file());
}

// ---- watch re-verification, verify-stage gates, queue reuse -----------------------------------

/// The worker's second `issue finish` after a failed verification: commit the fix, finish again.
fn worker_refinishes(f: &F) {
    commit_in(&wt(f), "src/fixed.rs", "\n");
    let report = wt(f)
        .join(".factory")
        .join("runs")
        .join(&f.issue)
        .join("report.md");
    f.repo.ok(&[
        "--lane",
        &f.issue,
        "issue",
        "finish",
        &f.issue,
        "--report",
        report.to_str().unwrap(),
    ]);
}

#[test]
fn watch_reverifies_a_failed_run_once_the_worker_finishes_again() {
    let f = setup();
    add_fixed_file_gate(&f);
    f.repo.ok(&["run", "start", &f.issue]);
    worker_finishes(&f);
    let res = f.repo.ok(&["run", "watch", "--once"]);
    assert_eq!(res["transitions"][0]["to"], "failed", "{res}");
    assert_eq!(issue_status(&f), "blocked");

    // blocked and no new STATUS: nothing to do
    let res = f.repo.ok(&["run", "watch", "--once"]);
    assert_eq!(res["transitions"].as_array().unwrap().len(), 0, "{res}");

    // in-review, but the only STATUS is older than (or as old as) the failure: no loop
    store(&f)
        .set_status(&f.issue.parse().unwrap(), Status::InReview, "human")
        .unwrap();
    let res = f.repo.ok(&["run", "watch", "--once"]);
    assert_eq!(res["transitions"].as_array().unwrap().len(), 0, "{res}");
    assert_eq!(active_run(&f).state, RunState::Failed);

    // timestamps have one-second resolution: let the second STATUS land in a later second
    std::thread::sleep(std::time::Duration::from_millis(1200));
    worker_refinishes(&f);
    assert_eq!(
        active_run(&f).state,
        RunState::Failed,
        "finish does not touch a failed run"
    );

    let res = f.repo.ok(&["run", "watch", "--once"]);
    assert_eq!(
        (
            res["transitions"][0]["from"].as_str(),
            res["transitions"][0]["to"].as_str()
        ),
        (Some("failed"), Some("succeeded")),
        "{res}"
    );
    assert_eq!(active_run(&f).state, RunState::Succeeded);
    assert_eq!(issue_status(&f), "validating");
    assert_eq!(store(&f).merge_queue().unwrap().len(), 1);

    // idempotent
    let res = f.repo.ok(&["run", "watch", "--once"]);
    assert_eq!(res["transitions"].as_array().unwrap().len(), 0, "{res}");
}

#[test]
fn verification_runs_only_the_gates_whose_when_includes_verify() {
    let f = setup();
    let log = f.repo.path().join("gate-log.txt");
    let cfg = f.repo.path().join(".factory/config.toml");
    let mut s = std::fs::read_to_string(&cfg).unwrap();
    // `v` records that it ran; `m` (merge stage only) would fail the run if verification ran it
    s.push_str(&format!(
        "\n[[gate]]\nname = \"v\"\nwhen = [\"verify\"]\ncmd = ['python', '-c', 'import sys; open(sys.argv[1], \"a\").write(\"v;\")', '{}']\n",
        log.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/")
    ));
    s.push_str(
        "\n[[gate]]\nname = \"m\"\nwhen = [\"merge\"]\ncmd = ['python', '-c', 'import sys; sys.exit(2)']\n",
    );
    std::fs::write(&cfg, s).unwrap();
    f.repo.ok(&["run", "start", &f.issue]);
    worker_finishes(&f);
    let res = f.repo.ok(&["run", "watch", "--once"]);
    assert_eq!(res["transitions"][0]["to"], "succeeded", "{res}");
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "v;");
}

#[test]
fn verification_reuses_a_branch_that_is_already_in_the_merge_queue() {
    let f = setup();
    f.repo.ok(&["run", "start", &f.issue]);
    worker_finishes(&f);
    let run = active_run(&f);
    let queued = store(&f)
        .enqueue_merge(&f.issue.parse().unwrap(), Some(&run.id), &run.branch)
        .unwrap();

    let res = f.repo.ok(&["run", "watch", "--once"]);
    assert_eq!(res["transitions"][0]["to"], "succeeded", "{res}");
    assert_eq!(active_run(&f).state, RunState::Succeeded);
    let q = store(&f).merge_queue().unwrap();
    assert_eq!(q.len(), 1, "no second item");
    assert_eq!(q[0].id, queued.id, "the existing item is reused");
    assert_eq!(q[0].state, MergeState::Queued);
    assert_eq!(issue_status(&f), "validating");
}

// ---- capacity: worker slots count active runs, claims only block scopes ---------------------------

fn plan_reasons(plan: &serde_json::Value) -> Vec<(String, String)> {
    plan["deferred"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["issue"].as_str().unwrap().to_string(),
                d["reason"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn plan_counts_active_worker_runs_not_held_claims_and_shows_both_limits() {
    let f = setup();
    let cfg = f.repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("max_parallel = 3 ", "max_parallel = 1 ");
    std::fs::write(&cfg, s).unwrap();
    let clash = f
        .repo
        .create_issue("clashes with asf-1", &["--ready", "--scope", "src/**"]);
    let apart = f
        .repo
        .create_issue("touches other files", &["--ready", "--scope", "docs/x.md"]);

    // a working run takes the only worker slot: the disjoint issue waits for a slot, the other one for the scope
    f.repo.ok(&["run", "start", &f.issue]);
    let plan = f.repo.ok(&["run", "plan"]);
    assert_eq!(
        (plan["capacity"].as_u64(), plan["running"].as_u64()),
        (Some(1), Some(1))
    );
    assert_eq!(
        (
            plan["review_capacity"].as_u64(),
            plan["reviewers_running"].as_u64()
        ),
        (Some(2), Some(0))
    );
    assert!(plan["slots"].as_array().unwrap().is_empty(), "{plan}");
    assert_eq!(
        plan_reasons(&plan),
        [
            (clash.clone(), "scope_conflict".to_string()),
            (apart.clone(), "slots_full".to_string())
        ]
    );
    let text =
        String::from_utf8_lossy(&f.repo.klobuk().args(["run", "plan"]).output().unwrap().stdout).to_string();
    assert!(
        text.contains("workers 1/1 running, reviewers 0/2 running"),
        "{text}"
    );

    // the worker finishes and is verified: the issue waits in validating and keeps its claim,
    // but no run is active any more, so the slot is free while the scope stays taken
    worker_finishes(&f);
    let res = f.repo.ok(&["run", "watch", "--once"]);
    assert_eq!(res["transitions"][0]["to"], "succeeded", "{res}");
    assert_eq!(issue_status(&f), "validating");
    assert_eq!(store(&f).active_claims().unwrap().len(), 1, "the claim is held");
    let plan = f.repo.ok(&["run", "plan"]);
    assert_eq!(plan["running"], 0, "{plan}");
    let slots: Vec<&str> = plan["slots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["issue"].as_str().unwrap())
        .collect();
    assert_eq!(slots, [apart.as_str()], "{plan}");
    assert_eq!(plan_reasons(&plan), [(clash, "scope_conflict".to_string())]);
    assert_eq!(plan["deferred"][0]["detail"], f.issue.as_str());
    let text =
        String::from_utf8_lossy(&f.repo.klobuk().args(["run", "plan"]).output().unwrap().stdout).to_string();
    assert!(text.contains("workers 0/1 running"), "{text}");
}

#[test]
fn a_run_env_alone_pins_the_lane_to_the_run_issue() {
    let f = setup();
    f.repo.ok(&["run", "start", &f.issue]);
    let run = active_run(&f).id;
    let say = |lane: &str| {
        let out = f
            .repo
            .klobuk()
            .env("KLOBUK_RUN", &run)
            .args([
                "--lane", lane, "issue", "comment", &f.issue, "--type", "FYI", "--body", "n", "--json",
            ])
            .output()
            .unwrap();
        (
            serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap(),
            out.status.code(),
        )
    };
    let (v, code) = say("human");
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (Some(2), Some("usage")),
        "{v}"
    );
    let (v, code) = say(&f.issue);
    assert_eq!(code, Some(0), "{v}");
}

// ---- klobuk run budget / run reply --budget (asf-140) ----

/// `klobuk hook <event>` as the worker's Claude Code would call it: from the worktree, `KLOBUK_RUN` set.
fn hook_call(f: &F, event: &str, stdin: &serde_json::Value) -> String {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new(env!("CARGO_BIN_EXE_klobuk"))
        .args(["hook", event])
        .current_dir(wt(f))
        .env_remove("KLOBUK_ISSUE")
        .env("KLOBUK_RUN", active_run(f).id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Whether the pre-tool hook lets the worker edit `src/ok.rs`.
fn edit_allowed(f: &F) -> bool {
    let payload = serde_json::json!({
        "session_id": "s", "transcript_path": "C:/x.jsonl", "hook_event_name": "PreToolUse",
        "tool_name": "Edit", "tool_use_id": "t",
        "tool_input": { "file_path": wt(f).join("src").join("ok.rs").to_string_lossy(), "content": "x" }
    });
    !hook_call(f, "pre-tool", &payload).contains("deny")
}

fn status_line(f: &F, cost: f64) -> String {
    let payload = serde_json::json!({
        "session_id": "s", "transcript_path": "C:/x.jsonl",
        "cost": { "total_cost_usd": cost, "total_duration_ms": 1000 },
        "context_window": { "total_input_tokens": 1, "total_output_tokens": 1 }
    });
    hook_call(f, "status", &payload)
}

fn patch_run(f: &F, p: RunPatch) {
    store(f).update_run(&active_run(f).id, &p).unwrap();
}

fn budget_events(f: &F) -> Vec<serde_json::Value> {
    store(f)
        .events_since(0, 1000)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "run.budget_raised")
        .map(|e| e.payload)
        .collect()
}

#[test]
fn budget_raises_the_latest_run_writes_the_event_and_prints_old_to_new() {
    let f = setup();
    f.repo.ok(&["run", "start", &f.issue]);
    patch_run(
        &f,
        RunPatch {
            cost_usd: Some(3.0),
            ..RunPatch::default()
        },
    );
    let res = f.repo.ok(&["run", "budget", &f.issue, "14"]);
    assert_eq!(
        (res["old"].as_f64(), res["new"].as_f64()),
        (Some(5.0), Some(14.0))
    );
    assert_eq!(active_run(&f).budget_usd, Some(14.0));
    let events = budget_events(&f);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(
        (events[0]["old"].as_f64(), events[0]["new"].as_f64()),
        (Some(5.0), Some(14.0))
    );
    let text = f
        .repo
        .klobuk()
        .args(["run", "budget", &f.issue, "14.5"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("$14.00 -> $14.50"), "{text}");
}

#[test]
fn budget_below_the_cost_or_not_a_number_is_refused_with_exit_2() {
    let f = setup();
    f.repo.ok(&["run", "start", &f.issue]);
    patch_run(
        &f,
        RunPatch {
            cost_usd: Some(3.0),
            ..RunPatch::default()
        },
    );
    let (code, ecode, msg) = f.repo.err(&["run", "budget", &f.issue, "2.5"]);
    assert_eq!((code, ecode.as_str()), (2, "usage"), "{msg}");
    assert!(msg.contains("$3.00"), "{msg}");
    let (code, ecode, _) = f.repo.err(&["run", "budget", &f.issue, "nan"]);
    assert_eq!((code, ecode.as_str()), (2, "usage"));
    assert_eq!(
        active_run(&f).budget_usd,
        Some(5.0),
        "a refused value changes nothing"
    );
    assert!(budget_events(&f).is_empty());
    // the cost itself is allowed: the hook trips only above the budget
    f.repo.ok(&["run", "budget", &f.issue, "3"]);
    let (code, ecode, _) = f.repo.err(&["run", "budget", "asf-9", "9"]);
    assert_eq!((code, ecode.as_str()), (1, "not_found"));
}

#[test]
fn raising_an_over_budget_run_sets_it_working_clears_the_reason_and_lets_the_hook_allow_edits() {
    let f = setup();
    f.repo.ok(&["run", "start", &f.issue]);
    assert!(edit_allowed(&f));
    // the statusline trips the budget: BLOCKER, state, denied edits
    assert!(status_line(&f, 6.0).contains("OVER BUDGET"));
    let run = active_run(&f);
    assert_eq!(run.state, RunState::OverBudget);
    assert!(run.exit_reason.is_some());
    let blocker = store(&f)
        .comments(&f.issue.parse().unwrap())
        .unwrap()
        .into_iter()
        .find(|c| c.kind == CommentType::Blocker)
        .unwrap();
    assert!(
        blocker
            .body
            .contains(&format!("klobuk run budget {} <usd>", f.issue))
            && blocker
                .body
                .contains(&format!("klobuk run reply --budget <usd> {}", f.issue)),
        "{}",
        blocker.body
    );
    assert!(!edit_allowed(&f));

    let res = f.repo.ok(&["run", "budget", &f.issue, "14"]);
    assert_eq!(
        (res["state_before"].as_str(), res["state"].as_str()),
        (Some("over_budget"), Some("working"))
    );
    let run = active_run(&f);
    assert_eq!((run.state, run.exit_reason.clone()), (RunState::Working, None));
    assert!(edit_allowed(&f), "the next hook call sees the new limit");
    // the next status line is under the new budget: no second BLOCKER, still working
    assert!(!status_line(&f, 6.5).contains("OVER BUDGET"));
    let blockers = store(&f)
        .comments(&f.issue.parse().unwrap())
        .unwrap()
        .into_iter()
        .filter(|c| c.kind == CommentType::Blocker)
        .count();
    assert_eq!(blockers, 1);
    assert_eq!(active_run(&f).state, RunState::Working);
}

#[test]
fn the_hook_compares_cost_with_the_budget_and_not_the_stored_state() {
    let f = setup();
    f.repo.ok(&["run", "start", &f.issue]);
    // a run left in over_budget although the cost is back under the budget (an operator edited the row)
    patch_run(
        &f,
        RunPatch {
            state: Some(RunState::OverBudget),
            cost_usd: Some(1.0),
            ..RunPatch::default()
        },
    );
    assert!(edit_allowed(&f));
    // the other way round: state working, cost above the budget
    patch_run(
        &f,
        RunPatch {
            state: Some(RunState::Working),
            cost_usd: Some(5.5),
            ..RunPatch::default()
        },
    );
    assert!(!edit_allowed(&f));
}

#[test]
fn a_run_stopped_by_the_tool_call_cap_stays_denied() {
    let f = setup();
    f.repo.ok(&["run", "start", &f.issue]);
    patch_run(
        &f,
        RunPatch {
            state: Some(RunState::OverBudget),
            tool_calls: Some(100_000),
            ..RunPatch::default()
        },
    );
    assert!(!edit_allowed(&f));
}

#[test]
fn raising_the_budget_of_a_cap_tripped_run_keeps_it_over_budget_and_adds_no_second_blocker() {
    let f = setup();
    let cfg = f.repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg).unwrap();
    assert!(
        s.contains("max_tool_calls = 1000"),
        "config template lacks the cap"
    );
    std::fs::write(&cfg, s.replace("max_tool_calls = 1000", "max_tool_calls = 2")).unwrap();
    f.repo.ok(&["run", "start", &f.issue]);
    let post_tool = serde_json::json!({
        "session_id": "s", "transcript_path": "C:/x.jsonl", "hook_event_name": "PostToolUse",
        "tool_name": "Bash", "tool_input": {"command": "ls"}, "tool_response": {"stdout": ""},
        "tool_use_id": "t", "duration_ms": 1
    });
    for _ in 0..3 {
        hook_call(&f, "post-tool", &post_tool);
    }
    let blockers = |f: &F| {
        store(f)
            .comments(&f.issue.parse().unwrap())
            .unwrap()
            .into_iter()
            .filter(|c| c.kind == CommentType::Blocker)
            .count()
    };
    assert_eq!(active_run(&f).state, RunState::OverBudget);
    assert_eq!(blockers(&f), 1);
    assert!(!edit_allowed(&f));

    // the budget is not what stopped it: the run stays stopped and the answer says so
    let out = f
        .repo
        .klobuk()
        .args(["run", "budget", &f.issue, "14"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("still over_budget"), "{text}");
    assert!(!text.contains("over_budget -> working"), "{text}");
    let run = active_run(&f);
    assert_eq!((run.state, run.budget_usd), (RunState::OverBudget, Some(14.0)));
    assert!(run
        .exit_reason
        .as_deref()
        .is_some_and(|r| r.contains("max_tool_calls")));
    assert!(
        !edit_allowed(&f),
        "the cap still holds after the budget is raised"
    );
    hook_call(&f, "post-tool", &post_tool);
    assert_eq!(blockers(&f), 1, "no second BLOCKER");
    assert_eq!(budget_events(&f).len(), 1);
}

#[test]
fn reply_with_budget_raises_and_sends_in_one_step() {
    let f = setup();
    f.repo.ok(&["run", "start", &f.issue]);
    status_line(&f, 6.0);
    assert_eq!(active_run(&f).state, RunState::OverBudget);
    assert!(!edit_allowed(&f));
    f.repo.ok(&[
        "run",
        "reply",
        "--budget",
        "14",
        &f.issue,
        "carry on with the review fixes",
    ]);
    let run = active_run(&f);
    assert_eq!((run.state, run.budget_usd), (RunState::Working, Some(14.0)));
    assert!(fake(&f)
        .log()
        .iter()
        .any(|e| e.op == "prompt" && e.args["text"] == "carry on with the review fixes"));
    assert!(edit_allowed(&f));
    assert_eq!(budget_events(&f).len(), 1);
    // a refused amount sends nothing
    let sent = fake(&f).log().iter().filter(|e| e.op == "prompt").count();
    let (code, ecode, _) = f
        .repo
        .err(&["run", "reply", "--budget", "1", &f.issue, "never sent"]);
    assert_eq!((code, ecode.as_str()), (2, "usage"));
    assert_eq!(fake(&f).log().iter().filter(|e| e.op == "prompt").count(), sent);
}

// ---- a turn that ends on an API or network error is sent again (asf-158) ----

const API_ERROR_LINE: &str = "API Error: Can't reach the API server — check your internet or DNS (ENOTFOUND)";

/// The pane Claude Code leaves after a turn that died on `error`: the echoed prompt, the error and the
/// input box with `input` in it.
fn pane_after(error: &str, input: &str) -> String {
    format!(
        "> Start asf-1: read .factory/runs/asf-1/brief.md and follow it.\n\n  ⎿  {error}\n\n\
         ╭────────────────────────────────╮\n│ > {input:<28} │\n╰────────────────────────────────╯\n  ? for shortcuts"
    )
}

/// The agent has ended its turn: Herdr calls it idle and its pane shows `screen`.
fn agent_idle_showing(f: &F, screen: &str) {
    fake(f)
        .script(serde_json::json!({ "info": [{"state": "idle"}], "screen": screen }))
        .unwrap();
}

fn api_events(f: &F) -> Vec<Event> {
    store(f)
        .events_since(0, 10_000)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "run.api_error" || e.kind == "run.api_resend")
        .collect()
}

/// The prompts that reached the agent, oldest first.
fn prompts(f: &F) -> Vec<String> {
    fake(f)
        .log()
        .into_iter()
        .filter(|e| e.op == "prompt")
        .map(|e| e.args["text"].as_str().unwrap().to_string())
        .collect()
}

/// Seconds between an API event and the moment its `due_at` names.
fn wait_s(e: &Event) -> i64 {
    use time::format_description::well_known::Rfc3339;
    let at = |s: &str| time::OffsetDateTime::parse(s, &Rfc3339).unwrap();
    (at(e.payload["due_at"].as_str().unwrap()) - at(&e.ts)).whole_seconds()
}

/// Lets the wait before the next resend run out without sleeping: the newest event of the episode,
/// again, with a `due_at` long past.
fn wait_is_over(f: &F) {
    let run = active_run(f);
    let last = api_events(f).pop().expect("an API event to carry on from");
    store(f)
        .append_event(&NewEvent {
            kind: "run.api_error".into(),
            issue_id: Some(run.issue_id.clone()),
            run_id: Some(run.id.clone()),
            actor: Some(LANE_ORCHESTRATOR.into()),
            payload: serde_json::json!({
                "error": last.payload["error"], "tool_calls": run.tool_calls,
                "resends": last.payload["resends"], "due_at": "2000-01-01T00:00:00Z", "gave_up": false,
            }),
        })
        .unwrap();
}

fn watch_once(f: &F) -> serde_json::Value {
    f.repo.ok(&["run", "watch", "--once"])
}

fn set_api_retries(f: &F, n: u32) {
    let cfg = f.repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg).unwrap();
    assert!(s.contains("api_retries = 5"), "the template carries the default");
    std::fs::write(&cfg, s.replace("api_retries = 5", &format!("api_retries = {n}"))).unwrap();
}

#[test]
fn a_kickoff_that_died_on_an_api_error_is_sent_again_once_the_wait_is_over() {
    let f = setup();
    f.repo.ok(&["run", "start", &f.issue]);
    let kickoff = prompts(&f).remove(0);
    agent_idle_showing(&f, &pane_after(API_ERROR_LINE, ""));

    // the first sight only starts the wait of a minute
    let res = watch_once(&f);
    assert!(res["transitions"].as_array().unwrap().is_empty(), "{res}");
    let events = api_events(&f);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "run.api_error");
    assert!(events[0].payload["error"].as_str().unwrap().contains("ENOTFOUND"));
    assert!((59..=61).contains(&wait_s(&events[0])), "{}", wait_s(&events[0]));
    watch_once(&f);
    assert_eq!(
        prompts(&f).len(),
        1,
        "back-off: nothing goes out before the wait is over"
    );

    wait_is_over(&f);
    let res = watch_once(&f);
    assert_eq!(res["transitions"][0]["to"], "working", "{res}");
    assert!(
        res["transitions"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("kickoff"),
        "{res}"
    );
    assert_eq!(
        prompts(&f),
        vec![kickoff.clone(), kickoff],
        "the kick-off line again"
    );
    let sent = api_events(&f).pop().unwrap();
    assert_eq!(
        (sent.kind.as_str(), sent.payload["resends"].as_u64()),
        ("run.api_resend", Some(1))
    );
    assert_eq!(sent.payload["what"], "kickoff");
    assert!(
        (119..=121).contains(&wait_s(&sent)),
        "the next wait is two minutes"
    );

    // a resend is no new attempt, no review round and no BLOCKER
    let run = active_run(&f);
    assert_eq!((run.state, run.attempt), (RunState::Working, 1));
    assert_eq!(blockers(&f), 0);
    assert!(store(&f)
        .events_since(0, 10_000)
        .unwrap()
        .iter()
        .all(|e| !e.kind.contains("sent_back")));
}

#[test]
fn a_run_that_failed_mid_turn_gets_a_short_continue_message() {
    let f = setup();
    f.repo.ok(&["run", "start", &f.issue]);
    patch_run(
        &f,
        RunPatch {
            tool_calls: Some(7),
            ..RunPatch::default()
        },
    );
    agent_idle_showing(&f, &pane_after("API Error: 529 overloaded_error", ""));
    watch_once(&f);
    wait_is_over(&f);
    let res = watch_once(&f);
    assert!(
        res["transitions"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("continue"),
        "{res}"
    );
    assert_eq!(
        prompts(&f).pop().unwrap(),
        "Your last turn ended with an API error; continue where you left off."
    );
    assert_eq!(api_events(&f).pop().unwrap().payload["what"], "continue");
}

#[test]
fn the_wait_doubles_with_every_resend_and_the_run_stalls_after_api_retries() {
    let f = setup();
    set_api_retries(&f, 2);
    f.repo.ok(&["run", "start", &f.issue]);
    patch_run(
        &f,
        RunPatch {
            tool_calls: Some(3),
            ..RunPatch::default()
        },
    );
    agent_idle_showing(&f, &pane_after(API_ERROR_LINE, ""));
    watch_once(&f);
    assert!((59..=61).contains(&wait_s(&api_events(&f)[0])));
    for expected in [120, 240] {
        wait_is_over(&f);
        agent_idle_showing(&f, &pane_after(API_ERROR_LINE, ""));
        let res = watch_once(&f);
        assert_eq!(res["transitions"][0]["to"], "working", "{res}");
        let wait = wait_s(&api_events(&f).pop().unwrap());
        assert!(
            (expected - 1..=expected + 1).contains(&wait),
            "{wait} vs {expected}"
        );
    }
    assert_eq!(prompts(&f).len(), 3, "the kick-off and two resends");
    assert_eq!(active_run(&f).state, RunState::Working);

    // the error again after the last allowed resend: the run gives up
    agent_idle_showing(&f, &pane_after(API_ERROR_LINE, ""));
    let res = watch_once(&f);
    assert_eq!(res["transitions"][0]["to"], "stalled", "{res}");
    let run = active_run(&f);
    assert_eq!((run.state, run.attempt), (RunState::Stalled, 1));
    assert!(run.exit_reason.unwrap().contains("ENOTFOUND"));
    let comments = store(&f).comments(&f.issue.parse().unwrap()).unwrap();
    let blocker = comments
        .iter()
        .find(|c| c.kind == CommentType::Blocker)
        .expect("a BLOCKER");
    assert!(
        blocker.body.contains("ENOTFOUND") && blocker.body.contains("api_retries"),
        "{}",
        blocker.body
    );
    assert_eq!(blockers(&f), 1);
    assert_eq!(prompts(&f).len(), 3, "nothing is sent after the run gave up");
    // a stalled run is left alone by the next passes
    agent_idle_showing(&f, &pane_after(API_ERROR_LINE, ""));
    watch_once(&f);
    assert_eq!((prompts(&f).len(), blockers(&f)), (3, 1));
}

#[test]
fn an_error_the_resend_cannot_cure_or_text_in_the_input_box_is_left_alone() {
    let f = setup();
    f.repo.ok(&["run", "start", &f.issue]);
    // a rejected key: no event at all
    agent_idle_showing(&f, &pane_after("API Error: 401 Invalid API key", ""));
    watch_once(&f);
    assert!(api_events(&f).is_empty());
    // the human is typing something else into the box
    agent_idle_showing(&f, &pane_after(API_ERROR_LINE, "wait, one more thing"));
    watch_once(&f);
    wait_is_over(&f);
    watch_once(&f);
    assert_eq!(prompts(&f).len(), 1, "text in the input box is not typed over");
    // a working agent is not looked at
    fake(&f)
        .script(
            serde_json::json!({ "info": [{"state": "working"}], "screen": pane_after(API_ERROR_LINE, "") }),
        )
        .unwrap();
    let before = api_events(&f).len();
    watch_once(&f);
    assert_eq!(api_events(&f).len(), before);
}

#[test]
fn a_kickoff_line_that_claude_put_back_in_the_input_box_only_gets_its_enter() {
    let f = setup();
    f.repo.ok(&["run", "start", &f.issue]);
    let kickoff = prompts(&f).remove(0);
    agent_idle_showing(&f, &pane_after(API_ERROR_LINE, &kickoff));
    watch_once(&f);
    wait_is_over(&f);
    watch_once(&f);
    assert_eq!(prompts(&f).len(), 1, "the line is not typed a second time");
    let keys: Vec<_> = fake(&f)
        .log()
        .into_iter()
        .filter(|e| e.op == "send_keys")
        .map(|e| e.args["keys"].clone())
        .collect();
    assert_eq!(keys, vec![serde_json::json!(["enter"])]);
    assert_eq!(api_events(&f).pop().unwrap().kind, "run.api_resend");
}

#[test]
fn an_outage_the_agent_got_over_starts_the_count_again() {
    let f = setup();
    set_api_retries(&f, 1);
    f.repo.ok(&["run", "start", &f.issue]);
    agent_idle_showing(&f, &pane_after(API_ERROR_LINE, ""));
    watch_once(&f);
    wait_is_over(&f);
    watch_once(&f);
    assert_eq!(prompts(&f).len(), 2);
    // the agent worked (its hooks counted a tool call) and then met the next outage: a new episode
    patch_run(
        &f,
        RunPatch {
            tool_calls: Some(4),
            ..RunPatch::default()
        },
    );
    agent_idle_showing(&f, &pane_after(API_ERROR_LINE, ""));
    let res = watch_once(&f);
    assert!(
        res["transitions"].as_array().unwrap().is_empty(),
        "first sight again: {res}"
    );
    assert_eq!(active_run(&f).state, RunState::Working);
    assert_eq!(api_events(&f).pop().unwrap().payload["resends"], 0);
}

// ---- a gate stopped by the infrastructure is not a code failure (asf-158) ----

/// A gate that is stopped by its 1 s timeout for as many runs as the log has lines below `stops`, then passes.
fn stopped_gate(f: &F, stops: usize) -> PathBuf {
    let dir = f.repo.path().join(".factory");
    let script = dir.join("stopped_gate.py");
    std::fs::write(
        &script,
        format!(
            "import sys, time\nlog = sys.argv[1]\ntry:\n    n = len(open(log).read().splitlines())\nexcept OSError:\n    n = 0\n\
             open(log, 'a').write('run\\n')\nif n < {stops}:\n    time.sleep(60)\n"
        ),
    )
    .unwrap();
    let log = dir.join("stopped_gate.log");
    let fwd = |p: &Path| p.to_str().unwrap().replace('\\', "/");
    let cfg = f.repo.path().join(".factory/config.toml");
    let mut s = std::fs::read_to_string(&cfg).unwrap();
    s.push_str(&format!(
        "\n[[gate]]\nname = \"flaky\"\ncmd = ['python', '{}', '{}']\ntimeout_s = 1\n",
        fwd(&script),
        fwd(&log)
    ));
    std::fs::write(&cfg, s).unwrap();
    log
}

fn gate_runs(log: &Path) -> usize {
    std::fs::read_to_string(log).unwrap_or_default().lines().count()
}

fn rerun_events(f: &F) -> usize {
    store(f)
        .events_since(0, 10_000)
        .unwrap()
        .iter()
        .filter(|e| e.kind == "run.verify_rerun")
        .count()
}

#[test]
fn a_gate_stopped_by_the_infrastructure_is_run_again_once_and_a_pass_succeeds() {
    let f = setup();
    let log = stopped_gate(&f, 1);
    f.repo.ok(&["run", "start", &f.issue]);
    worker_finishes(&f);
    let res = watch_once(&f);
    assert_eq!(res["transitions"][0]["to"], "succeeded", "{res}");
    assert_eq!(gate_runs(&log), 2, "stopped once, then passed");
    assert_eq!(rerun_events(&f), 1);
    assert_eq!(active_run(&f).state, RunState::Succeeded);
    assert_eq!(store(&f).merge_queue().unwrap().len(), 1);
    assert_eq!(blockers(&f), 0);
}

#[test]
fn a_second_infrastructure_stop_blocks_the_issue_and_says_so() {
    let f = setup();
    let log = stopped_gate(&f, 99);
    f.repo.ok(&["run", "start", &f.issue]);
    worker_finishes(&f);
    let res = watch_once(&f);
    assert_eq!(res["transitions"][0]["to"], "failed", "{res}");
    assert_eq!(gate_runs(&log), 2, "one automatic rerun, not more");
    assert_eq!(rerun_events(&f), 1);
    let run = active_run(&f);
    assert_eq!(run.state, RunState::Failed);
    assert!(
        run.exit_reason.as_deref().unwrap().contains("infrastructure"),
        "{run:?}"
    );
    let comments = store(&f).comments(&f.issue.parse().unwrap()).unwrap();
    let blocker = comments
        .iter()
        .find(|c| c.kind == CommentType::Blocker)
        .expect("a BLOCKER");
    assert!(
        blocker.body.contains("infrastructure") && blocker.body.contains("flaky"),
        "{}",
        blocker.body
    );
    assert_eq!(issue_status(&f), "blocked");
    // an operator-triggered verification of the same commit does not rerun by itself again
    f.repo.ok(&["run", "verify", &f.issue]);
    assert_eq!(gate_runs(&log), 3);
    assert_eq!(rerun_events(&f), 1);
}
