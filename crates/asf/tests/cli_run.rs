mod common;

use std::path::{Path, PathBuf};

use asf_core::*;
use asf_runtime::FakeRuntime;
use asf_store::SqliteStore;
use common::{git, Repo};

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
    let runs_dir = f.repo.path().join(".factory/runs/asf-1");
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
            .any(|p| p[0] == "ASF_ISSUE" && p[1] == "asf-1"),
        "{env}"
    );
    assert!(
        env.as_array()
            .unwrap()
            .iter()
            .any(|p| p[0] == "ASF_RUN" && p[1] == run_id),
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
            .any(|w| w[0] == "--permission-mode" && w[1] == "acceptEdits"),
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
    let report = f.repo.path().join(".factory/runs/asf-1/report.md");
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
    let report = f.repo.path().join(".factory/runs/asf-1/report.md");
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
    let cfg = f.repo.path().join(".factory/config.toml");
    let mut s = std::fs::read_to_string(&cfg).unwrap();
    s.push_str("\n[[gate]]\nname = \"nope\"\ncmd = [\"python\", \"-c\", \"import sys; print('gate says no'); sys.exit(2)\"]\n");
    std::fs::write(&cfg, s).unwrap();
    f.repo.ok(&["run", "start", &f.issue]);
    commit_in(&wt(&f), "src/greet.rs", "");
    let report = f.repo.path().join(".factory/runs/asf-1/report.md");
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
