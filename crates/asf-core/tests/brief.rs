use asf_core::brief::{parse_verdict, render_brief, render_review_brief, BriefInput, ReviewInput, Verdict};
use asf_core::model::*;

fn issue() -> Issue {
    Issue {
        id: "asf-9".parse().unwrap(),
        uid: "u".into(),
        title: "Add greeting".into(),
        body: "Body text.".into(),
        kind: IssueType::Task,
        status: Status::InProgress,
        priority: 1,
        parent_id: None,
        file_scope: vec!["src/**".into()],
        acceptance: vec![
            Acceptance {
                text: "prints hello".into(),
                verify: Some("cargo run".into()),
            },
            Acceptance {
                text: "tests pass".into(),
                verify: None,
            },
        ],
        estimate: Some("S".into()),
        labels: vec![],
        source: None,
        created_at: "c".into(),
        updated_at: "u".into(),
        closed_at: None,
    }
}

fn worker_brief() -> String {
    let me = issue();
    render_brief(&BriefInput {
        issue: &me,
        deps: vec![],
        comments: &[],
        blockers: vec![],
        worktree: ".worktrees/asf-9",
        branch: "feat/asf-9",
        report_path: ".factory/runs/asf-9/report.md",
        budget_usd: 5.0,
        asf_bin: "D:/bin/asf.exe",
    })
}

fn review_brief() -> String {
    let me = issue();
    let gates = vec![
        "cargo fmt --all -- --check".to_string(),
        "cargo clippy --workspace --all-targets -- -D warnings".to_string(),
        "cargo test --workspace".to_string(),
    ];
    render_review_brief(&ReviewInput {
        issue: &me,
        base_sha: "0123abcd0123abcd0123abcd0123abcd0123abcd",
        worktree: ".worktrees/asf-9",
        branch: "feat/asf-9",
        gates: &gates,
        asf_bin: "D:/bin/asf.exe",
    })
}

// ---- worker protocol -------------------------------------------------------------------------

#[test]
fn worker_brief_routes_blocking_questions_through_ask_user_question() {
    let b = worker_brief();
    assert!(b.contains("AskUserQuestion"), "{b}");
}

#[test]
fn worker_brief_routes_non_blocking_questions_to_the_human() {
    let b = worker_brief();
    assert!(
        b.contains("D:/bin/asf.exe issue comment asf-9 --type QUESTION --to human"),
        "{b}"
    );
}

#[test]
fn worker_brief_posts_deviations_as_decisions() {
    let b = worker_brief();
    assert!(
        b.contains("D:/bin/asf.exe issue comment asf-9 --type DECISION --to human"),
        "{b}"
    );
}

#[test]
fn worker_brief_says_finish_is_refused_while_a_question_is_unanswered() {
    let b = worker_brief();
    assert!(
        b.contains("finish is refused while a QUESTION to human is unanswered"),
        "{b}"
    );
}

#[test]
fn worker_brief_report_has_no_open_questions_section() {
    let b = worker_brief();
    assert!(
        !b.contains("open questions"),
        "report must not carry an open-questions section:\n{b}"
    );
    assert!(b.contains("no open-questions section"), "{b}");
}

#[test]
fn worker_brief_keeps_the_finish_command_and_inbox() {
    let b = worker_brief();
    assert!(
        b.contains("D:/bin/asf.exe issue finish asf-9 --report .factory/runs/asf-9/report.md"),
        "{b}"
    );
    assert!(b.contains("D:/bin/asf.exe issue inbox"), "{b}");
    assert!(!b.contains('\r'));
}

// ---- review brief ----------------------------------------------------------------------------

#[test]
fn review_brief_mentions_base_sha_and_diff_command() {
    let b = review_brief();
    assert!(b.contains("base_sha"), "{b}");
    assert!(b.contains("0123abcd0123abcd0123abcd0123abcd0123abcd"), "{b}");
    assert!(
        b.contains("git diff 0123abcd0123abcd0123abcd0123abcd0123abcd"),
        "{b}"
    );
}

#[test]
fn review_brief_lists_every_acceptance_criterion() {
    let b = review_brief();
    assert!(b.contains("prints hello"), "{b}");
    assert!(b.contains("`cargo run`"), "{b}");
    assert!(b.contains("tests pass"), "{b}");
}

#[test]
fn review_brief_lists_the_gate_commands() {
    let b = review_brief();
    for gate in [
        "cargo fmt --all -- --check",
        "cargo clippy --workspace --all-targets -- -D warnings",
        "cargo test --workspace",
    ] {
        assert!(b.contains(gate), "missing gate {gate:?}:\n{b}");
    }
}

#[test]
fn review_brief_states_the_single_final_decision_rule() {
    let b = review_brief();
    assert!(b.contains("exactly one DECISION"), "{b}");
    assert!(b.contains("`approve`"), "{b}");
    assert!(b.contains("`changes:`"), "{b}");
}

#[test]
fn review_brief_verdict_command_addresses_the_reviewed_issue() {
    let b = review_brief();
    assert!(
        b.contains("D:/bin/asf.exe issue comment asf-9 --type DECISION --to asf-9 --body \"approve\""),
        "{b}"
    );
    assert!(b.contains("--body \"changes:"), "{b}");
    for wrong in ["--to human", "--to orchestrator"] {
        assert!(
            !b.contains(wrong),
            "verdict must not be addressed with {wrong:?}:\n{b}"
        );
    }
}

#[test]
fn review_brief_says_the_full_gates_already_passed_and_must_not_be_rerun() {
    let b = review_brief();
    assert!(
        b.contains("already passed in the merge queue on this commit"),
        "{b}"
    );
    assert!(b.contains("after the rebase"), "{b}");
    assert!(b.contains("Do not rerun the whole suite"), "{b}");
    assert!(b.contains("read the diff"), "{b}");
    assert!(!b.contains("Run the gates from the worktree"), "{b}");
}

#[test]
fn review_brief_asks_for_targeted_commands_and_naming_them_in_the_verdict() {
    let b = review_brief();
    assert!(b.contains("targeted"), "{b}");
    assert!(b.contains("a single test"), "{b}");
    assert!(b.contains("one test file"), "{b}");
    assert!(b.contains("one package"), "{b}");
    assert!(b.contains("name in the verdict"), "{b}");
}

#[test]
fn review_brief_is_read_only() {
    let b = review_brief();
    assert!(b.contains("Never edit"), "{b}");
    for allowed in ["may edit", "you can edit", "feel free to edit", "fix it yourself"] {
        assert!(
            !b.contains(allowed),
            "review brief permits editing ({allowed:?}):\n{b}"
        );
    }
}

// ---- verdict parser --------------------------------------------------------------------------

#[test]
fn parse_verdict_approve() {
    assert_eq!(parse_verdict("approve"), Some(Verdict::Approve));
    assert_eq!(parse_verdict("approve\n"), Some(Verdict::Approve));
    assert_eq!(parse_verdict("  approve  \n\nlooks good"), Some(Verdict::Approve));
}

#[test]
fn parse_verdict_changes_with_bullets() {
    let body = "changes:\n- missing test for the empty case\n- clippy fails on brief.rs\n";
    assert_eq!(
        parse_verdict(body),
        Some(Verdict::Changes(vec![
            "missing test for the empty case".into(),
            "clippy fails on brief.rs".into(),
        ]))
    );
}

#[test]
fn parse_verdict_is_case_insensitive_on_the_first_line() {
    assert_eq!(parse_verdict("APPROVE"), Some(Verdict::Approve));
    assert_eq!(parse_verdict("Approve"), Some(Verdict::Approve));
    assert_eq!(
        parse_verdict("CHANGES:\n* one"),
        Some(Verdict::Changes(vec!["one".into()]))
    );
    assert_eq!(
        parse_verdict("Changes:\n- one"),
        Some(Verdict::Changes(vec!["one".into()]))
    );
}

#[test]
fn parse_verdict_joins_wrapped_bullet_lines() {
    let body = "changes:\n- first finding\n  continues here\n- second";
    assert_eq!(
        parse_verdict(body),
        Some(Verdict::Changes(vec![
            "first finding continues here".into(),
            "second".into(),
        ]))
    );
}

#[test]
fn parse_verdict_changes_without_findings_is_none() {
    assert_eq!(parse_verdict("changes:"), None);
    assert_eq!(parse_verdict("changes:\n\n"), None);
}

#[test]
fn parse_verdict_garbage_is_none() {
    for garbage in [
        "",
        "   \n  ",
        "looks fine to me",
        "approved",
        "I approve",
        "change: x",
        "- approve",
    ] {
        assert_eq!(parse_verdict(garbage), None, "{garbage:?}");
    }
}
