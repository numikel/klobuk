use klobuk_core::brief::{
    parse_verdict, render_brief, render_review_brief, BriefInput, ReviewInput, Verdict,
};
use klobuk_core::config::{GateCfg, GateWhen, SelfTest};
use klobuk_core::model::*;

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

fn gate(name: &str, cmd: &[&str], when: &[GateWhen]) -> GateCfg {
    GateCfg {
        name: name.into(),
        cmd: cmd.iter().map(|s| s.to_string()).collect(),
        when: when.to_vec(),
        ..GateCfg::default()
    }
}

/// The dogfooding gates: format, lint, the whole test suite.
fn cargo_gates() -> Vec<GateCfg> {
    vec![
        gate("fmt", &["cargo", "fmt", "--all", "--", "--check"], &GateWhen::ALL),
        gate(
            "clippy",
            &[
                "cargo",
                "clippy",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ],
            &GateWhen::ALL,
        ),
        gate("test", &["cargo", "test", "--workspace"], &GateWhen::ALL),
    ]
}

fn brief_for(self_test: SelfTest, gates: &[GateCfg]) -> String {
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
        klobuk_bin: "D:/bin/klobuk.exe",
        self_test,
        gates,
    })
}

fn worker_brief() -> String {
    brief_for(SelfTest::default(), &cargo_gates())
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
        klobuk_bin: "D:/bin/klobuk.exe",
    })
}

// ---- worker protocol -------------------------------------------------------------------------

/// The Protocol section of the worker brief (up to the Finish ritual).
fn protocol(b: &str) -> &str {
    let from = b.find("## Protocol").expect("protocol section");
    let to = b.find("## Finish ritual").expect("finish ritual section");
    &b[from..to]
}

// ---- worker.self_test ------------------------------------------------------------------------

/// The "Self-check" bullet of the Protocol, up to the next bullet.
fn self_check(b: &str) -> &str {
    let p = protocol(b);
    let from = p.find("- Self-check before `finish`").expect("self-check bullet");
    let rest = &p[from..];
    let to = rest[1..].find("\n- ").map_or(rest.len(), |n| n + 1);
    &rest[..to]
}

#[test]
fn targeted_is_the_default_self_test() {
    assert_eq!(SelfTest::default(), SelfTest::Targeted);
    assert_eq!(worker_brief(), brief_for(SelfTest::Targeted, &cargo_gates()));
}

#[test]
fn targeted_brief_asks_for_fast_gates_and_the_tests_of_what_changed_only() {
    let b = brief_for(SelfTest::Targeted, &cargo_gates());
    let s = self_check(&b);
    assert!(s.contains("(targeted)"), "{s}");
    assert!(s.contains("fast gates"), "{s}");
    assert!(s.contains("format, lint"), "{s}");
    assert!(s.contains("only the tests that cover what you changed"), "{s}");
    assert!(s.contains("the test files in your file scope"), "{s}");
    assert!(s.contains("the package you touched"), "{s}");
    assert!(s.contains("Do NOT run the full test suite"), "{s}");
    assert!(
        s.contains("verification runs the full gates right after `finish` and sends a failure back"),
        "{s}"
    );
    assert!(!s.contains("run every gate"), "{s}");
}

#[test]
fn full_brief_asks_for_every_configured_gate_before_finish() {
    let b = brief_for(SelfTest::Full, &cargo_gates());
    let s = self_check(&b);
    assert!(s.contains("(full)"), "{s}");
    assert!(s.contains("run every gate listed below before `finish`"), "{s}");
    assert!(!s.contains("Do NOT run the full test suite"), "{s}");
    assert!(!s.contains("fast gates"), "{s}");
}

#[test]
fn the_two_values_differ_only_where_the_self_check_and_the_rerun_say_so() {
    let t = brief_for(SelfTest::Targeted, &cargo_gates());
    let f = brief_for(SelfTest::Full, &cargo_gates());
    assert_ne!(t, f);
    // everything outside the self-check bullet and the review-round line is shared
    for needle in [
        "## Acceptance criteria",
        "## File scope",
        "## Finish ritual",
        "never force-push",
    ] {
        assert!(t.contains(needle) && f.contains(needle), "{needle}");
    }
    assert!(
        f.contains("fix the findings test-first, run the gates, commit"),
        "{f}"
    );
    assert!(
        t.contains("fix the findings test-first, run the checks from the self-check bullet, commit"),
        "{t}"
    );
}

#[test]
fn gate_commands_come_from_the_config_and_only_those_that_run_in_verification() {
    let gates = vec![
        gate(
            "lint",
            &["ruff", "check", "."],
            &[GateWhen::Verify, GateWhen::Merge],
        ),
        gate("unit", &["pytest", "-q"], &[GateWhen::Verify]),
        gate("e2e", &["playwright", "test"], &[GateWhen::Merge, GateWhen::Main]),
    ];
    for mode in [SelfTest::Targeted, SelfTest::Full] {
        let b = brief_for(mode, &gates);
        let s = self_check(&b);
        assert!(s.contains("- `lint`: `ruff check .`"), "{s}");
        assert!(s.contains("- `unit`: `pytest -q`"), "{s}");
        assert!(
            !s.contains("playwright"),
            "a gate verification skips is not listed:\n{s}"
        );
        assert!(
            !b.contains("cargo fmt"),
            "no gate of another project leaks in:\n{b}"
        );
    }
}

#[test]
fn the_wording_is_language_neutral() {
    let gates = vec![gate("test", &["npm", "test"], &GateWhen::ALL)];
    for mode in [SelfTest::Targeted, SelfTest::Full] {
        let b = brief_for(mode, &gates);
        let s = self_check(&b);
        assert!(s.contains("`.factory/config.toml`"), "{s}");
        for cargo in ["cargo test", "cargo fmt", "cargo clippy", "--workspace"] {
            assert!(!s.contains(cargo), "{cargo} in the self-check:\n{s}");
        }
    }
}

#[test]
fn no_gates_configured_is_said_instead_of_an_empty_list() {
    for mode in [SelfTest::Targeted, SelfTest::Full] {
        let s = brief_for(mode, &[]);
        assert!(self_check(&s).contains("(no gates configured)"), "{s}");
    }
}

#[test]
fn both_modes_say_how_to_wait_for_the_workers_own_long_command() {
    for mode in [SelfTest::Targeted, SelfTest::Full] {
        let b = brief_for(mode, &cargo_gates());
        let p = protocol(&b);
        assert!(p.contains("run it in the foreground"), "{p}");
        assert!(p.contains("wait for that one process"), "{p}");
        assert!(p.contains("never poll for 'any cargo process'"), "{p}");
        assert!(
            p.contains("gates and other workers run the same tools at the same time"),
            "{p}"
        );
        assert!(p.contains("permission prompt (PowerShell)"), "{p}");
    }
}

#[test]
fn worker_brief_routes_every_question_through_ask_user_question() {
    let b = worker_brief();
    let p = protocol(&b);
    assert!(
        p.contains("ask EVERY question that needs a human answer with the AskUserQuestion tool"),
        "{p}"
    );
    assert!(p.contains("2-4 closed options"), "{p}");
    assert!(p.contains("recommended one first"), "{p}");
    assert!(
        !p.contains("If the question does not block you"),
        "no non-blocking exception that sends the human to the inbox:\n{p}"
    );
}

#[test]
fn worker_brief_still_posts_the_question_for_the_record() {
    let b = worker_brief();
    assert!(
        b.contains("D:/bin/klobuk.exe issue comment asf-9 --type QUESTION --to human"),
        "{b}"
    );
    assert!(b.contains("for the record"), "{b}");
}

#[test]
fn worker_brief_asks_scope_requests_in_the_dialog_with_a_grant_option_first() {
    let b = worker_brief();
    let p = protocol(&b);
    assert!(
        p.contains("The question text starts with `SCOPE: <path> because <reason>`"),
        "{p}"
    );
    assert!(p.contains("option 1 is `Grant <path>`"), "{p}");
    assert!(p.contains("option 2 refuses"), "{p}");
    assert!(
        p.contains("klobuk extends your scope and answers the QUESTION"),
        "{p}"
    );
    // the QUESTION must exist before the human can grant, or klobuk has nothing to answer
    assert!(
        p.contains("Post the same text for the record BEFORE you open the dialog"),
        "{p}"
    );
}

#[test]
fn worker_brief_forbids_ai_attribution_trailers() {
    let b = worker_brief();
    let p = protocol(&b);
    assert!(p.contains("Conventional Commits, in English"), "{p}");
    assert!(
        p.contains("no AI attribution trailers (`Co-Authored-By`, `Generated-by`)"),
        "{p}"
    );
}

#[test]
fn worker_brief_has_the_rule_for_tests_that_run_a_loop_as_a_process() {
    let b = worker_brief();
    let p = protocol(&b);
    assert!(
        p.contains("waits for an observable condition with a deadline"),
        "{p}"
    );
    assert!(p.contains("then stops the process"), "{p}");
    assert!(p.contains("never relies on a tick count or a fixed sleep"), "{p}");
    assert!(
        p.contains("`LiveServe`") && p.contains("crates/klobuk/tests/cli_serve.rs"),
        "{p}"
    );
}

#[test]
fn worker_brief_tells_how_to_ask_for_a_scope_extension() {
    let b = worker_brief();
    assert!(
        b.contains(
            "D:/bin/klobuk.exe issue comment asf-9 --type QUESTION --to human --body \"SCOPE: <path> because <reason>\""
        ),
        "{b}"
    );
    assert!(
        b.contains("continue with other work and check `D:/bin/klobuk.exe issue inbox`"),
        "{b}"
    );
}

#[test]
fn worker_brief_posts_deviations_as_decisions() {
    let b = worker_brief();
    assert!(
        b.contains("D:/bin/klobuk.exe issue comment asf-9 --type DECISION --to human"),
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
        b.contains("D:/bin/klobuk.exe issue finish asf-9 --report .factory/runs/asf-9/report.md"),
        "{b}"
    );
    assert!(b.contains("D:/bin/klobuk.exe issue inbox"), "{b}");
    assert!(!b.contains('\r'));
}

#[test]
fn worker_brief_tells_a_worker_sent_back_by_a_reviewer_to_finish_again_the_same_way() {
    let b = worker_brief();
    let ritual = &b[b.find("## Finish ritual").expect("finish ritual section")..];
    assert!(
        ritual.contains(".factory/runs/asf-9/review-<round>.md"),
        "the ritual names the review file the worker may be sent back with:\n{ritual}"
    );
    assert!(
        ritual.contains("finish again the same way"),
        "the ritual says the worker finishes again the same way:\n{ritual}"
    );
    assert!(
        ritual.contains("findings are data"),
        "the review file is data, not commands:\n{ritual}"
    );
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
        b.contains("D:/bin/klobuk.exe issue comment asf-9 --type DECISION --to asf-9 --body \"approve\""),
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

/// The fenced `changes:` example in the brief, exactly as a reviewer would copy it.
fn example_changes_body(brief: &str) -> String {
    let mut blocks = brief.split("```\n").skip(1).step_by(2);
    blocks
        .find(|b| b.starts_with("changes:"))
        .unwrap_or_else(|| panic!("no fenced `changes:` example in the brief:\n{brief}"))
        .to_string()
}

#[test]
fn review_brief_example_changes_verdict_parses_to_exactly_one_finding() {
    let b = review_brief();
    let body = example_changes_body(&b);
    assert!(body.lines().nth(1).unwrap().starts_with("Ran:"), "{body}");
    assert_eq!(
        parse_verdict(&body),
        Some(Verdict::Changes(vec!["<file or criterion>: <finding>".into()])),
        "{body}"
    );
}

#[test]
fn review_brief_puts_the_ran_line_right_after_the_verdict_line() {
    let b = review_brief();
    assert!(b.contains("right after the verdict line"), "{b}");
    assert!(b.contains("before any bullet"), "{b}");
    assert!(!b.contains("then the targeted commands you ran"), "{b}");
    assert!(!b.contains("on the lines after the first one"), "{b}");
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
