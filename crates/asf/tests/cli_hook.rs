mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use asf_core::*;
use asf_store::SqliteStore;
use common::Repo;

/// Runs `asf hook <event>` with stdin JSON and env, from `cwd`. Returns (exit code, stdout, stderr).
fn hook(cwd: &Path, event: &str, env: &[(&str, &str)], stdin: &serde_json::Value) -> (i32, String, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_asf"));
    c.args(["hook", event])
        .current_dir(cwd)
        .env_remove("ASF_RUN")
        .env_remove("ASF_ISSUE");
    for (k, v) in env {
        c.env(k, v);
    }
    c.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

struct Fixture {
    repo: Repo,
    issue: IssueId,
    run_id: String,
    wt: PathBuf,
}

/// Issue `asf-1` with scope `src/**`, claimed, with a worktree and a `starting` run.
fn fixture(config_replace: Option<(&str, &str)>) -> Fixture {
    let repo = Repo::initialised();
    if let Some((from, to)) = config_replace {
        let cfg = repo.path().join(".factory/config.toml");
        let s = std::fs::read_to_string(&cfg).unwrap();
        assert!(s.contains(from), "config template lacks {from}");
        std::fs::write(&cfg, s.replace(from, to)).unwrap();
    }
    let id = repo.create_issue("scoped", &["--ready", "--scope", "src/**"]);
    repo.ok(&["--lane", &id, "issue", "claim", &id]);
    let wt = repo.worktree(&id);
    std::fs::create_dir_all(wt.join("src")).unwrap();
    let store = SqliteStore::open(&repo.path().join(".factory/factory.db")).unwrap();
    let run = store
        .create_run(&NewRun {
            issue_id: id.parse().unwrap(),
            runtime: "fake".into(),
            worktree: format!(".worktrees/{id}"),
            branch: format!("feat/{id}"),
            base_sha: "base".into(),
            budget_usd: Some(1.0),
        })
        .unwrap();
    store
        .update_run(
            &run.id,
            &RunPatch {
                state: Some(RunState::Working),
                ..RunPatch::default()
            },
        )
        .unwrap();
    Fixture {
        repo,
        issue: id.parse().unwrap(),
        run_id: run.id,
        wt,
    }
}

fn store(f: &Fixture) -> SqliteStore {
    SqliteStore::open(&f.repo.path().join(".factory/factory.db")).unwrap()
}

fn pre_tool(file: &Path, tool: &str) -> serde_json::Value {
    serde_json::json!({
        "session_id": "sess-1",
        "transcript_path": "C:/x/sess-1.jsonl",
        "cwd": "D:/x",
        "permission_mode": "acceptEdits",
        "hook_event_name": "PreToolUse",
        "tool_name": tool,
        "tool_input": { "file_path": file.to_string_lossy(), "content": "x" },
        "tool_use_id": "toolu_1"
    })
}

#[test]
fn pre_tool_denies_outside_scope_with_json_decision() {
    let f = fixture(None);
    let env = [("ASF_RUN", f.run_id.as_str())];
    let (code, out, _) = hook(
        &f.wt,
        "pre-tool",
        &env,
        &pre_tool(&f.wt.join("Cargo.toml"), "Write"),
    );
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).expect("json decision");
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
    let reason = v["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap();
    assert!(
        reason.contains("Cargo.toml") && reason.contains("src/**"),
        "{reason}"
    );
    let kinds: Vec<String> = store(&f)
        .events_since(0, 100)
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert!(kinds.contains(&"hook.denied".to_string()), "{kinds:?}");
}

#[test]
fn pre_tool_allows_in_scope_and_ignores_other_tools() {
    let f = fixture(None);
    let env = [("ASF_RUN", f.run_id.as_str())];
    let (code, out, _) = hook(
        &f.wt,
        "pre-tool",
        &env,
        &pre_tool(&f.wt.join("src").join("x.rs"), "Edit"),
    );
    assert_eq!((code, out.as_str()), (0, ""));
    // Read is not scope-checked
    let (code, out, _) = hook(
        &f.wt,
        "pre-tool",
        &env,
        &pre_tool(&f.wt.join("Cargo.toml"), "Read"),
    );
    assert_eq!((code, out.as_str()), (0, ""));
}

#[test]
fn pre_tool_denies_outside_worktree_even_if_glob_matches() {
    let f = fixture(None);
    let env = [("ASF_RUN", f.run_id.as_str())];
    let outside = f.repo.path().join("src").join("x.rs");
    let (code, out, _) = hook(&f.wt, "pre-tool", &env, &pre_tool(&outside, "Write"));
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
    assert!(v["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap()
        .contains("worktree"));
}

#[test]
fn pre_tool_resolves_run_from_issue_env_and_is_noop_without_context() {
    let f = fixture(None);
    let env = [("ASF_ISSUE", f.issue.as_str())];
    let (code, out, _) = hook(
        &f.wt,
        "pre-tool",
        &env,
        &pre_tool(&f.wt.join("Cargo.toml"), "Write"),
    );
    assert_eq!(code, 0);
    assert!(out.contains("deny"));
    let (code, out, err) = hook(
        &f.wt,
        "pre-tool",
        &[],
        &pre_tool(&f.wt.join("Cargo.toml"), "Write"),
    );
    assert_eq!(
        (code, out.as_str()),
        (0, ""),
        "no run context: never block Claude"
    );
    assert!(err.is_empty() || err.contains("no run"), "{err}");
}

#[test]
fn post_tool_counts_calls_records_session_and_trips_max_tool_calls() {
    let f = fixture(Some(("max_tool_calls = 400", "max_tool_calls = 2")));
    let env = [("ASF_RUN", f.run_id.as_str())];
    let payload = serde_json::json!({
        "session_id": "sess-9", "transcript_path": "C:/t/sess-9.jsonl", "hook_event_name": "PostToolUse",
        "tool_name": "Bash", "tool_input": {"command": "ls"}, "tool_response": {"stdout": ""}, "tool_use_id": "t", "duration_ms": 3
    });
    for _ in 0..2 {
        let (code, out, _) = hook(&f.wt, "post-tool", &env, &payload);
        assert_eq!((code, out.as_str()), (0, ""));
    }
    let run = store(&f).get_run(&f.run_id).unwrap().unwrap();
    assert_eq!(run.tool_calls, 2);
    assert_eq!(run.claude_session_id.as_deref(), Some("sess-9"));
    assert_eq!(run.transcript_path.as_deref(), Some("C:/t/sess-9.jsonl"));
    assert!(run.last_seen_at.is_some());
    assert_eq!(run.state, RunState::Working);

    let (code, out, _) = hook(&f.wt, "post-tool", &env, &payload);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).expect("block decision when over the cap");
    assert_eq!(v["decision"], "block");
    let run = store(&f).get_run(&f.run_id).unwrap().unwrap();
    assert_eq!(run.tool_calls, 3);
    assert_eq!(run.state, RunState::OverBudget);
    let blockers = store(&f).comments(&f.issue).unwrap();
    assert!(
        blockers
            .iter()
            .any(|c| c.kind == CommentType::Blocker && c.to_lane == "human"),
        "{blockers:?}"
    );
}

#[test]
fn stop_blocks_once_until_finish_then_allows() {
    let f = fixture(None);
    let env = [("ASF_RUN", f.run_id.as_str())];
    let stop = |active: bool, msg: &str| {
        serde_json::json!({
            "session_id": "sess-1", "transcript_path": "C:/x.jsonl", "hook_event_name": "Stop",
            "stop_hook_active": active, "last_assistant_message": msg, "background_tasks": [], "session_crons": []
        })
    };
    let (code, out, _) = hook(&f.wt, "stop", &env, &stop(false, "I think I am done"));
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).expect("block json");
    assert_eq!(v["decision"], "block");
    assert!(
        v["reason"].as_str().unwrap().contains("asf issue finish asf-1"),
        "{v}"
    );
    // second stop while the hook is already active: let Claude stop (no loop)
    let (code, out, _) = hook(&f.wt, "stop", &env, &stop(true, "really done"));
    assert_eq!((code, out.as_str()), (0, ""));
    let ev = store(&f).events_since(0, 100).unwrap();
    assert!(ev
        .iter()
        .any(|e| e.kind == "run.stop" && e.payload["last_assistant_message"] == "I think I am done"));

    // after the finish ritual the stop hook is silent
    let report = f.wt.join("report.md");
    std::fs::write(&report, "# done\n").unwrap();
    f.repo.ok(&[
        "--lane",
        "asf-1",
        "issue",
        "finish",
        "asf-1",
        "--report",
        report.to_str().unwrap(),
    ]);
    let (code, out, _) = hook(&f.wt, "stop", &env, &stop(false, "done"));
    assert_eq!((code, out.as_str()), (0, ""));
    let run = store(&f).get_run(&f.run_id).unwrap().unwrap();
    assert_eq!(run.state, RunState::Finishing);
    assert!(run.report_path.as_deref().unwrap().ends_with("report.md"));
}

#[test]
fn status_updates_cost_and_trips_budget_once() {
    let f = fixture(None);
    let env = [("ASF_RUN", f.run_id.as_str())];
    let status = |cost: f64| {
        serde_json::json!({
            "session_id": "sess-1", "transcript_path": "C:/x.jsonl",
            "cost": { "total_cost_usd": cost, "total_duration_ms": 1000 },
            "context_window": { "total_input_tokens": 1200, "total_output_tokens": 300 }
        })
    };
    let (code, out, _) = hook(&f.wt, "status", &env, &status(0.5));
    assert_eq!(code, 0);
    assert!(out.contains("asf-1") && out.contains("$0.50"), "{out}");
    let run = store(&f).get_run(&f.run_id).unwrap().unwrap();
    assert_eq!(run.cost_usd, 0.5);
    assert_eq!(run.input_tokens, 1200);
    assert_eq!(run.state, RunState::Working);

    let (code, out, _) = hook(&f.wt, "status", &env, &status(1.5));
    assert_eq!(code, 0);
    assert!(out.contains("OVER BUDGET"), "{out}");
    let run = store(&f).get_run(&f.run_id).unwrap().unwrap();
    assert_eq!(run.state, RunState::OverBudget);
    let n = store(&f)
        .comments(&f.issue)
        .unwrap()
        .iter()
        .filter(|c| c.kind == CommentType::Blocker)
        .count();
    assert_eq!(n, 1);
    hook(&f.wt, "status", &env, &status(1.6));
    let n = store(&f)
        .comments(&f.issue)
        .unwrap()
        .iter()
        .filter(|c| c.kind == CommentType::Blocker)
        .count();
    assert_eq!(n, 1, "BLOCKER posted once");
    // once over budget, pre-tool denies everything
    let (_, out, _) = hook(
        &f.wt,
        "pre-tool",
        &env,
        &pre_tool(&f.wt.join("src").join("ok.rs"), "Edit"),
    );
    assert!(out.contains("deny") && out.contains("budget"), "{out}");
}
