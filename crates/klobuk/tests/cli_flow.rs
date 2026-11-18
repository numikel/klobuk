mod common;

use common::Repo;

#[test]
fn claim_release_flow() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--ready"]);
    let c = r.ok(&["--lane", "asf-1", "issue", "claim", &a, "--note", "spike"]);
    assert_eq!(c["worker"], "asf-1");
    assert_eq!(c["note"], "spike");
    assert_eq!(c["branch"], "feat/asf-1");
    assert_eq!(c["worktree"], ".worktrees/asf-1");
    assert_eq!(r.ok(&["issue", "show", &a])["issue"]["status"], "in-progress");
    let (code, ecode, msg) = r.err(&["--lane", "asf-7", "issue", "claim", &a]);
    assert_eq!((code, ecode.as_str()), (3, "conflict"));
    assert!(msg.contains("asf-1"), "{msg}");
    r.ok(&["issue", "release", &a]);
    assert_eq!(r.ok(&["issue", "show", &a])["issue"]["status"], "ready");
    let (code, ecode, _) = r.err(&["issue", "release", &a]);
    assert_eq!((code, ecode.as_str()), (1, "not_found"));
}

#[test]
fn claim_with_explicit_worker_and_not_ready_conflict() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &[]);
    let (code, ecode, msg) = r.err(&["issue", "claim", &a, "--worker", "bot"]);
    assert_eq!((code, ecode.as_str()), (3, "conflict"));
    assert!(msg.contains("backlog"), "{msg}");
    r.ok(&["issue", "status", &a, "ready"]);
    assert_eq!(r.ok(&["issue", "claim", &a, "--worker", "bot"])["worker"], "bot");
}

#[test]
fn finish_requires_report_and_sets_in_review() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--ready"]);
    r.ok(&["--lane", &a, "issue", "claim", &a]);
    let (code, ecode, _) = r.err(&["--lane", &a, "issue", "finish", &a, "--report", "missing.md"]);
    assert_eq!((code, ecode.as_str()), (1, "validation"));
    let report = r.path().join("report.md");
    std::fs::write(&report, "# done\n").unwrap();
    let res = r.ok(&[
        "--lane",
        &a,
        "issue",
        "finish",
        &a,
        "--report",
        report.to_str().unwrap(),
    ]);
    assert_eq!(res["issue"]["status"], "in-review");
    assert_eq!(res["comment"]["type"], "STATUS");
    assert!(res["comment"]["body"].as_str().unwrap().contains("report.md"));
    // claim is kept until release/merge
    assert_eq!(r.ok(&["lane", "list"]).as_array().unwrap().len(), 1);
}

#[test]
fn comment_inbox_cursor_and_handoff() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--ready"]);
    let b = r.create_issue("b", &["--ready"]);
    r.ok(&["--lane", &a, "issue", "claim", &a]);
    r.ok(&["--lane", &b, "issue", "claim", &b]);

    let q = r.ok(&[
        "--lane",
        &a,
        "issue",
        "comment",
        &b,
        "--type",
        "QUESTION",
        "--to",
        &b,
        "--body",
        "which port?",
    ]);
    assert_eq!(q["author"], "agent:asf-1");
    let inbox_b = r.ok(&["--lane", &b, "issue", "inbox"]);
    assert_eq!(inbox_b.as_array().unwrap().len(), 1);
    assert_eq!(inbox_b[0]["body"], "which port?");
    assert_eq!(
        r.ok(&["--lane", &b, "issue", "inbox"]).as_array().unwrap().len(),
        0,
        "cursor advanced"
    );

    let h = r.ok(&[
        "--lane",
        &b,
        "issue",
        "handoff",
        &b,
        "--to",
        &a,
        "--body",
        "port 7700",
    ]);
    assert_eq!(h["type"], "HANDOFF");
    assert_eq!(
        r.ok(&["--lane", &a, "issue", "inbox", "--peek"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        r.ok(&["--lane", &a, "issue", "inbox", "--peek"])
            .as_array()
            .unwrap()
            .len(),
        1,
        "peek keeps cursor"
    );
    let human = r
        .klobuk()
        .args(["--lane", &a, "issue", "inbox"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("HANDOFF") && text.contains("port 7700"), "{text}");
    let empty = r
        .klobuk()
        .args(["--lane", &a, "issue", "inbox"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&empty.stdout).trim(), "INBOX-EMPTY");

    // human lane posts as `human`; orchestrator sees everything
    let hc = r.ok(&[
        "--lane", "human", "issue", "comment", &a, "--type", "DECISION", "--body", "ship it",
    ]);
    assert_eq!(hc["author"], "human");
    let all = r.ok(&["issue", "inbox", "--all"]);
    assert_eq!(all.as_array().unwrap().len(), 3);
}

#[test]
fn lane_resolved_from_env_and_worktree() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--ready"]);
    let out = r
        .klobuk()
        .env("KLOBUK_ISSUE", &a)
        .args(["issue", "claim", &a, "--json"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["result"]["worker"], "asf-1");
    r.ok(&["issue", "release", &a]);
    let wt = r.worktree("asf-1");
    let v = r.json_in(&wt, &["issue", "claim", &a]).0;
    assert_eq!(v["result"]["worker"], "asf-1");
}

#[test]
fn lane_list_and_check() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--ready"]);
    let b = r.create_issue("b", &["--ready"]);
    r.ok(&["--lane", &a, "issue", "claim", &a]);
    r.ok(&["--lane", &b, "issue", "claim", &b]);
    r.worktree("asf-1");
    let lanes = r.ok(&["lane", "list"]);
    assert_eq!(lanes.as_array().unwrap().len(), 2);
    assert_eq!(lanes[0]["issue_id"], "asf-1");
    assert_eq!(lanes[0]["worktree_exists"], true);
    assert_eq!(lanes[1]["worktree_exists"], false);
    let check = r.ok(&["lane", "check"]);
    assert_eq!(check["stale"], serde_json::json!(["asf-2"]));
    assert_eq!(check["ok"], false);
}

#[test]
fn run_plan_json_has_slots_and_deferred() {
    let r = Repo::initialised();
    std::fs::create_dir_all(r.path().join("src")).unwrap();
    std::fs::create_dir_all(r.path().join("tests")).unwrap();
    std::fs::write(r.path().join("src/a.rs"), "").unwrap();
    std::fs::write(r.path().join("tests/t.rs"), "").unwrap();
    common::git(r.path(), &["add", "."]);
    common::git(r.path(), &["commit", "-q", "-m", "files"]);
    let a = r.create_issue("a", &["--ready", "--scope", "src/a.rs"]);
    let _b = r.create_issue("b", &["--ready", "--scope", "src/**"]);
    let c = r.create_issue("c", &["--ready", "--scope", "tests/**"]);
    let _d = r.create_issue("d", &["--scope", "x.rs"]); // backlog: not listed
    let plan = r.ok(&["run", "plan"]);
    let slots: Vec<&str> = plan["slots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["issue"].as_str().unwrap())
        .collect();
    assert_eq!(slots, [a.as_str(), c.as_str()]);
    assert_eq!(plan["slots"][0]["branch"], "feat/asf-1");
    assert_eq!(plan["slots"][0]["worktree"], ".worktrees/asf-1");
    assert_eq!(plan["deferred"][0]["issue"], "asf-2");
    assert_eq!(plan["deferred"][0]["reason"], "scope_conflict");
    assert_eq!(plan["deferred"][0]["detail"], "asf-1");
    assert_eq!(plan["inputs_hash"].as_str().unwrap().len(), 16);
    assert_eq!(plan["capacity"], 3);
    let human = r.klobuk().args(["run", "plan"]).output().unwrap();
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(
        text.contains("asf-1") && text.contains("scope_conflict"),
        "{text}"
    );
}

#[test]
fn run_plan_bad_glob_exit_1() {
    let r = Repo::initialised();
    // the store rejects bad globs on create, so inject one via a direct edit of config hotspots instead
    let cfg = r.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("hotspots = []", "hotspots = [\"src/[\"]");
    std::fs::write(&cfg, s).unwrap();
    r.create_issue("a", &["--ready", "--scope", "src/a.rs"]);
    r.create_issue("b", &["--ready", "--scope", "src/b.rs"]);
    let (code, ecode, msg) = r.err(&["run", "plan"]);
    assert_eq!((code, ecode.as_str()), (1, "validation"));
    assert!(msg.contains("src/["), "{msg}");
}
