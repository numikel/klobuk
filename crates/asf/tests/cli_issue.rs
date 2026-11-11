mod common;

use assert_cmd::prelude::*;
use common::Repo;

#[test]
fn create_prints_sequential_ids_and_defaults() {
    let r = Repo::initialised();
    let a = r.ok(&["issue", "create", "--title", "first"]);
    assert_eq!(a["id"], "asf-1");
    assert_eq!(a["status"], "backlog");
    assert_eq!(a["type"], "task");
    assert_eq!(a["priority"], 2);
    let b = r.ok(&[
        "issue",
        "create",
        "--title",
        "second",
        "--type",
        "epic",
        "--priority",
        "1",
        "--label",
        "risky",
        "--label",
        "ui",
        "--scope",
        "src/**",
        "--scope",
        "Cargo.toml",
        "--accept",
        "builds",
        "--accept",
        "tests pass",
        "--estimate",
        "M",
        "--ready",
    ]);
    assert_eq!(b["id"], "asf-2");
    assert_eq!(b["type"], "epic");
    assert_eq!(b["status"], "ready");
    assert_eq!(b["labels"], serde_json::json!(["risky", "ui"]));
    assert_eq!(b["file_scope"], serde_json::json!(["src/**", "Cargo.toml"]));
    assert_eq!(b["acceptance"][1]["text"], "tests pass");
    assert_eq!(b["estimate"], "M");
}

#[test]
fn create_human_output_is_just_the_id_line() {
    let r = Repo::initialised();
    let out = r
        .asf()
        .args(["issue", "create", "--title", "x"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "asf-1");
}

#[test]
fn create_body_from_file_keeps_utf8() {
    let r = Repo::initialised();
    let body = r.path().join("body.md");
    std::fs::write(&body, "Zażółć gęślą jaźń\nline 2\n").unwrap();
    let id = r.create_issue("utf", &["--body-file", body.to_str().unwrap()]);
    let shown = r.ok(&["issue", "show", &id]);
    assert_eq!(shown["issue"]["body"], "Zażółć gęślą jaźń\nline 2\n");
}

#[test]
fn create_validations() {
    let r = Repo::initialised();
    let (code, ecode, _) = r.err(&["issue", "create", "--title", "   "]);
    assert_eq!((code, ecode.as_str()), (1, "validation"));
    let (code, ecode, msg) = r.err(&["issue", "create", "--title", "x", "--dep", "asf-9"]);
    assert_eq!((code, ecode.as_str()), (1, "validation"));
    assert!(msg.contains("asf-9"), "{msg}");
    let (_, ecode, _) = r.err(&["issue", "create", "--title", "x", "--scope", "src/["]);
    assert_eq!(ecode, "validation");
    r.create_issue("a", &[]);
    let (_, ecode, msg) = r.err(&["issue", "create", "--title", "x", "--dep", "asf-1", "--ready"]);
    assert_eq!(ecode, "validation");
    assert!(msg.contains("--ready"), "{msg}");
    r.asf()
        .args(["issue", "create", "--title", "x", "--type", "bogus"])
        .assert()
        .code(2);
    r.asf()
        .args(["issue", "create", "--title", "x", "--dep", "not-an-id!"])
        .assert()
        .code(2);
    assert_eq!(
        r.ok(&["repo", "status"])["next_seq"],
        2,
        "failed creates must not burn ids"
    );
}

#[test]
fn list_filters_and_ready_flag() {
    let r = Repo::initialised();
    r.create_issue("a", &["--ready"]);
    r.create_issue("b", &["--label", "risky"]);
    r.create_issue("c", &["--type", "review", "--ready"]);
    let all = r.ok(&["issue", "list"]);
    assert_eq!(all.as_array().unwrap().len(), 3);
    assert_eq!(r.ok(&["issue", "list", "--ready"]).as_array().unwrap().len(), 2);
    assert_eq!(
        r.ok(&["issue", "list", "--status", "backlog"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(r.ok(&["issue", "list", "--label", "risky"])[0]["title"], "b");
    assert_eq!(r.ok(&["issue", "list", "--type", "review"])[0]["id"], "asf-3");
    let human = r.asf().args(["issue", "list"]).output().unwrap();
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(
        text.contains("asf-1") && text.contains("ready") && text.contains("a"),
        "{text}"
    );
}

#[test]
fn show_and_dossier() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--ready"]);
    let b = r.create_issue("b", &["--dep", &a]);
    r.ok(&[
        "--lane",
        &a,
        "issue",
        "comment",
        &a,
        "--type",
        "HANDOFF",
        "--to",
        &b,
        "--body",
        "api in src/api.rs",
    ]);
    let plain = r.ok(&["issue", "show", &b]);
    assert_eq!(plain["issue"]["id"], "asf-2");
    assert_eq!(plain["deps"][0]["id"], "asf-1");
    assert_eq!(plain["deps"][0]["status"], "ready");
    assert!(plain.get("comments").is_none() || plain["comments"].as_array().unwrap().is_empty());
    let dossier = r.ok(&["issue", "show", &b, "--dossier"]);
    assert_eq!(dossier["deps"][0]["handoffs"][0]["body"], "api in src/api.rs");
    assert_eq!(dossier["comments"].as_array().unwrap().len(), 0);
    assert_eq!(dossier["dependents"].as_array().unwrap().len(), 0);
    let (code, ecode, _) = r.err(&["issue", "show", "asf-9"]);
    assert_eq!((code, ecode.as_str()), (1, "not_found"));
}

#[test]
fn edit_patches_only_given_fields() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--estimate", "S", "--scope", "x.rs"]);
    let e = r.ok(&["issue", "edit", &a, "--title", "renamed", "--priority", "1"]);
    assert_eq!(e["title"], "renamed");
    assert_eq!(e["priority"], 1);
    assert_eq!(e["estimate"], "S");
    assert_eq!(e["file_scope"], serde_json::json!(["x.rs"]));
    let e = r.ok(&[
        "issue",
        "edit",
        &a,
        "--scope",
        "y.rs",
        "--scope",
        "z.rs",
        "--clear-estimate",
    ]);
    assert_eq!(e["file_scope"], serde_json::json!(["y.rs", "z.rs"]));
    assert!(e["estimate"].is_null());
}

#[test]
fn status_transitions_and_terminal_conflict() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &[]);
    assert_eq!(r.ok(&["issue", "status", &a, "ready"])["status"], "ready");
    assert_eq!(r.ok(&["issue", "status", &a, "done"])["status"], "done");
    let (code, ecode, _) = r.err(&["issue", "status", &a, "ready"]);
    assert_eq!((code, ecode.as_str()), (3, "conflict"));
    r.asf().args(["issue", "status", &a, "nonsense"]).assert().code(2);
}

#[test]
fn deps_add_rm_cycle_and_ready_promotion() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &[]);
    let b = r.create_issue("b", &[]);
    r.ok(&["issue", "dep", "add", &b, &a]);
    let (code, ecode, _) = r.err(&["issue", "dep", "add", &a, &b]);
    assert_eq!((code, ecode.as_str()), (1, "validation"));
    let promoted = r.ok(&["issue", "ready"]);
    assert_eq!(promoted, serde_json::json!(["asf-1"]));
    r.ok(&["issue", "status", &a, "done"]);
    assert_eq!(r.ok(&["issue", "ready"]), serde_json::json!(["asf-2"]));
    r.ok(&["issue", "dep", "rm", &b, &a]);
    let (_, ecode, _) = r.err(&["issue", "dep", "rm", &b, &a]);
    assert_eq!(ecode, "not_found");
}

// ---- `asf issue finish` guard -----------------------------------------------------------------

fn report_file(r: &Repo, name: &str) -> String {
    let p = r.path().join(name);
    std::fs::write(&p, "# done\n").unwrap();
    p.to_string_lossy().replace('\\', "/")
}

#[test]
fn finish_is_refused_while_a_question_to_human_is_unanswered() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--ready"]);
    let report = report_file(&r, "report.md");
    let q = r.ok(&[
        "--lane",
        &a,
        "issue",
        "comment",
        &a,
        "--type",
        "QUESTION",
        "--to",
        "human",
        "--body",
        "which crate?",
    ]);
    let qid = q["id"].as_i64().unwrap().to_string();

    let (code, ecode, msg) = r.err(&["--lane", &a, "issue", "finish", &a, "--report", &report]);
    assert_eq!((code, ecode.as_str()), (3, "conflict"), "{msg}");
    assert!(msg.contains("QUESTION") && msg.contains("unanswered"), "{msg}");
    assert_eq!(
        r.ok(&["issue", "show", &a])["issue"]["status"],
        "ready",
        "a refused finish changes nothing"
    );

    // an ANSWER without reply_to that is not addressed to the asking lane does not resolve it
    r.ok(&[
        "--lane",
        "human",
        "issue",
        "comment",
        &a,
        "--type",
        "ANSWER",
        "--to",
        "all",
        "--body",
        "general remark",
    ]);
    let (code, _, _) = r.err(&["--lane", &a, "issue", "finish", &a, "--report", &report]);
    assert_eq!(code, 3);

    // the answer to that very question does
    r.ok(&[
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
    let done = r.ok(&["--lane", &a, "issue", "finish", &a, "--report", &report]);
    assert_eq!(done["issue"]["status"], "in-review");
}

#[test]
fn finish_ignores_questions_that_are_not_to_human() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--ready"]);
    let b = r.create_issue("b", &["--ready"]);
    let report = report_file(&r, "report.md");
    r.ok(&[
        "--lane",
        &a,
        "issue",
        "comment",
        &a,
        "--type",
        "QUESTION",
        "--to",
        &b,
        "--body",
        "peer question",
    ]);
    let done = r.ok(&["--lane", &a, "issue", "finish", &a, "--report", &report]);
    assert_eq!(done["issue"]["status"], "in-review");
}

// ---- ANSWER without --reply-to ----------------------------------------------------------------

/// `asf issue comment <id> --type ANSWER --to <to> [extra...] --body <body>` from the human lane.
fn answer_args<'a>(id: &'a str, to: &'a str, extra: &[&'a str], body: &'a str) -> Vec<&'a str> {
    let mut args = vec![
        "--lane", "human", "issue", "comment", id, "--type", "ANSWER", "--to", to,
    ];
    args.extend_from_slice(extra);
    args.extend_from_slice(&["--body", body]);
    args
}

#[test]
fn answer_without_reply_to_resolves_the_only_open_question() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--ready"]);
    let report = report_file(&r, "report.md");
    let qid = ask(&r, &a, "human", "which crate?");

    let (code, _, _) = r.err(&["--lane", &a, "issue", "finish", &a, "--report", &report]);
    assert_eq!(code, 3, "the open question blocks finish");

    let ans = r.ok(&answer_args(&a, &a, &[], "asf-core"));
    assert_eq!(ans["reply_to"].as_i64().unwrap().to_string(), qid, "{ans}");

    let done = r.ok(&["--lane", &a, "issue", "finish", &a, "--report", &report]);
    assert_eq!(done["issue"]["status"], "in-review");
}

#[test]
fn answer_without_reply_to_says_which_question_it_answers() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--ready"]);
    let qid = ask(&r, &a, "human", "which crate?");

    let out = r
        .asf()
        .args(answer_args(&a, &a, &[], "asf-core"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(&format!("answers #{qid}")), "{stdout}");
}

#[test]
fn answer_without_reply_to_with_several_open_questions_refuses_and_lists_them() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--ready"]);
    let q1 = ask(&r, &a, "human", "first question");
    let q2 = ask(&r, &a, "human", "second question");

    let (code, ecode, msg) = r.err(&answer_args(&a, &a, &[], "an answer"));
    assert_eq!((code, ecode.as_str()), (2, "usage"), "{msg}");
    assert!(
        msg.contains(&format!("#{q1}")) && msg.contains(&format!("#{q2}")),
        "{msg}"
    );
    assert!(
        msg.contains("first question") && msg.contains("--reply-to"),
        "{msg}"
    );
    assert!(
        comments_of(&r, &a).iter().all(|c| c["type"] != "ANSWER"),
        "a refused answer is not stored"
    );

    // naming the question explicitly still works
    let ans = r.ok(&answer_args(&a, &a, &["--reply-to", &q2], "an answer"));
    assert_eq!(ans["reply_to"].as_i64().unwrap().to_string(), q2);
}

#[test]
fn answer_without_reply_to_only_looks_at_the_addressed_lane() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--ready"]);
    let b = r.create_issue("b", &["--ready"]);
    let qa = ask(&r, &a, "human", "from a");
    // a second lane asks on the same issue (a's issue), so there are two open questions in all
    let qb = r.ok(&[
        "--lane", &b, "issue", "comment", &a, "--type", "QUESTION", "--to", "human", "--body", "from b",
    ])["id"]
        .as_i64()
        .unwrap()
        .to_string();

    let ans = r.ok(&answer_args(&a, &b, &[], "for b"));
    assert_eq!(ans["reply_to"].as_i64().unwrap().to_string(), qb, "{ans}");
    let ans = r.ok(&answer_args(&a, &a, &[], "for a"));
    assert_eq!(ans["reply_to"].as_i64().unwrap().to_string(), qa, "{ans}");
}

fn question_of<'a>(comments: &'a [serde_json::Value], qid: &str) -> &'a serde_json::Value {
    comments
        .iter()
        .find(|c| c["id"].as_i64().unwrap().to_string() == qid)
        .unwrap_or_else(|| panic!("no comment #{qid} in {comments:?}"))
}

#[test]
fn answer_with_reply_to_sets_resolved_at_on_the_question() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--ready"]);
    let q1 = ask(&r, &a, "human", "first");
    let q2 = ask(&r, &a, "human", "second");
    assert!(question_of(&comments_of(&r, &a), &q1)["resolved_at"].is_null());

    r.ok(&answer_args(&a, &a, &["--reply-to", &q1], "one"));
    let comments = comments_of(&r, &a);
    assert!(
        question_of(&comments, &q1)["resolved_at"].is_string(),
        "{comments:?}"
    );
    assert!(
        question_of(&comments, &q2)["resolved_at"].is_null(),
        "the other question stays open"
    );
}

#[test]
fn auto_linked_answer_sets_resolved_at_and_a_second_answer_keeps_the_first_time() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--ready"]);
    let q = ask(&r, &a, "human", "which crate?");
    r.ok(&answer_args(&a, &a, &[], "asf-core"));
    let first = question_of(&comments_of(&r, &a), &q)["resolved_at"].clone();
    assert!(first.is_string(), "{first}");

    r.ok(&answer_args(&a, &a, &["--reply-to", &q], "asf-core, really"));
    assert_eq!(question_of(&comments_of(&r, &a), &q)["resolved_at"], first);
}

/// Ids of the comments the human lane would read now (`inbox --peek` leaves the cursor alone).
fn human_inbox_ids(r: &Repo) -> Vec<String> {
    r.ok(&["--lane", "human", "issue", "inbox", "--peek"])
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_i64().unwrap().to_string())
        .collect()
}

#[test]
fn answered_questions_leave_the_human_inbox() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--ready"]);
    let b = r.create_issue("b", &["--ready"]);
    let still_open = r.create_issue("c", &["--ready"]);
    let qa = ask(&r, &a, "human", "auto-linked?");
    let qb = ask(&r, &b, "human", "explicit?");
    let qc = ask(&r, &still_open, "human", "nobody answers this");
    let inbox = human_inbox_ids(&r);
    assert!(
        [&qa, &qb, &qc].iter().all(|q| inbox.contains(q)),
        "unanswered questions are listed: {inbox:?}"
    );

    r.ok(&answer_args(&a, &a, &[], "yes"));
    r.ok(&answer_args(&b, &b, &["--reply-to", &qb], "yes"));

    let inbox = human_inbox_ids(&r);
    assert!(
        !inbox.contains(&qa),
        "auto-linked answer hides the question: {inbox:?}"
    );
    assert!(
        !inbox.contains(&qb),
        "explicit answer hides the question: {inbox:?}"
    );
    assert!(inbox.contains(&qc), "an open question stays listed: {inbox:?}");
}

#[test]
fn answer_to_a_non_question_resolves_nothing() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--ready"]);
    let note = r.ok(&[
        "--lane", &a, "issue", "comment", &a, "--type", "FYI", "--to", "all", "--body", "note",
    ])["id"]
        .as_i64()
        .unwrap()
        .to_string();
    r.ok(&answer_args(&a, &a, &["--reply-to", &note], "reply"));
    assert!(question_of(&comments_of(&r, &a), &note)["resolved_at"].is_null());
}

#[test]
fn answer_to_a_lane_without_an_open_question_stays_a_plain_comment() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--ready"]);
    let ans = r.ok(&answer_args(&a, &a, &[], "nothing was asked"));
    assert!(ans["reply_to"].is_null(), "{ans}");
}

// ---- `asf issue scope add|rm` -----------------------------------------------------------------

fn scope_of(r: &Repo, id: &str) -> serde_json::Value {
    r.ok(&["issue", "show", id])["issue"]["file_scope"].clone()
}

fn comments_of(r: &Repo, id: &str) -> Vec<serde_json::Value> {
    r.ok(&["issue", "show", id, "--dossier"])["comments"]
        .as_array()
        .unwrap()
        .clone()
}

fn ask(r: &Repo, id: &str, to: &str, body: &str) -> String {
    let q = r.ok(&[
        "--lane", id, "issue", "comment", id, "--type", "QUESTION", "--to", to, "--body", body,
    ]);
    q["id"].as_i64().unwrap().to_string()
}

#[test]
fn scope_add_appends_ignores_duplicates_and_reports_the_list() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--scope", "src/**"]);

    let v = r.ok(&[
        "issue",
        "scope",
        "add",
        &a,
        "docs/**",
        "src/**",
        "docs/**",
        "Cargo.toml",
    ]);
    assert_eq!(v["id"], "asf-1");
    assert_eq!(
        v["file_scope"],
        serde_json::json!(["src/**", "docs/**", "Cargo.toml"])
    );
    assert_eq!(v["changed"], serde_json::json!(["docs/**", "Cargo.toml"]));
    assert_eq!(scope_of(&r, &a), v["file_scope"]);

    // adding only known globs changes nothing
    let again = r.ok(&["issue", "scope", "add", &a, "docs/**"]);
    assert_eq!(again["file_scope"], v["file_scope"]);
    assert_eq!(again["changed"], serde_json::json!([]));

    let out = r
        .asf()
        .args(["issue", "scope", "add", &a, "tests/**"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("asf-1") && text.contains("src/** docs/** Cargo.toml tests/**"),
        "{text}"
    );
}

#[test]
fn scope_add_to_an_issue_without_scope_starts_the_list() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &[]);
    let v = r.ok(&["issue", "scope", "add", &a, "src/**"]);
    assert_eq!(v["file_scope"], serde_json::json!(["src/**"]));
}

#[test]
fn scope_add_rejects_invalid_globs_and_changes_nothing() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--scope", "src/**"]);
    let (code, ecode, msg) = r.err(&["issue", "scope", "add", &a, "docs/**", "src/["]);
    assert_eq!((code, ecode.as_str()), (1, "validation"));
    assert!(msg.contains("src/["), "{msg}");
    assert_eq!(
        scope_of(&r, &a),
        serde_json::json!(["src/**"]),
        "valid globs of a failed call are not applied"
    );
    let (code, ecode, _) = r.err(&["issue", "scope", "add", "asf-9", "src/**"]);
    assert_eq!((code, ecode.as_str()), (1, "not_found"));
    r.asf().args(["issue", "scope", "add", &a]).assert().code(2);
    r.asf().args(["issue", "scope", "rm", &a]).assert().code(2);
}

#[test]
fn scope_rm_removes_listed_globs_and_ignores_unknown_ones() {
    let r = Repo::initialised();
    let a = r.create_issue(
        "a",
        &["--scope", "src/**", "--scope", "docs/**", "--scope", "Cargo.toml"],
    );

    let v = r.ok(&["issue", "scope", "rm", &a, "docs/**", "nope/**", "docs/**"]);
    assert_eq!(v["file_scope"], serde_json::json!(["src/**", "Cargo.toml"]));
    assert_eq!(v["changed"], serde_json::json!(["docs/**"]));
    assert_eq!(scope_of(&r, &a), v["file_scope"]);

    let out = r
        .asf()
        .args(["issue", "scope", "rm", &a, "Cargo.toml"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("src/**"),
        "text shows the remaining list"
    );

    // the last glob may go; an empty scope means "no restriction" and the text says so
    let out = r
        .asf()
        .args(["issue", "scope", "rm", &a, "src/**"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("no restriction"), "{text}");
    assert_eq!(scope_of(&r, &a), serde_json::json!([]));
}

#[test]
fn scope_commands_are_refused_inside_a_worker_run() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--scope", "src/**"]);
    for args in [
        ["issue", "scope", "add", a.as_str(), "docs/**"],
        ["issue", "scope", "rm", a.as_str(), "src/**"],
    ] {
        let out = r
            .asf()
            .env("ASF_RUN", "01RUN")
            .args(args)
            .arg("--json")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["ok"], false, "{v}");
        assert_eq!(v["error"]["code"], "forbidden", "{v}");
    }
    assert_eq!(scope_of(&r, &a), serde_json::json!(["src/**"]), "scope untouched");
}

#[test]
fn scope_add_answers_a_scope_question_and_releases_the_finish_guard() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--ready", "--scope", "src/**"]);
    let report = report_file(&r, "report.md");
    let qid = ask(
        &r,
        &a,
        "human",
        "SCOPE: Cargo.toml because a new dependency is needed",
    );

    let (code, _, _) = r.err(&["--lane", &a, "issue", "finish", &a, "--report", &report]);
    assert_eq!(code, 3, "the open SCOPE question blocks finish");

    let v = r.ok(&["issue", "scope", "add", &a, "Cargo.toml"]);
    assert_eq!(v["answered"], serde_json::json!([qid.parse::<i64>().unwrap()]));

    let comments = comments_of(&r, &a);
    let answer = comments
        .iter()
        .find(|c| c["type"] == "ANSWER")
        .unwrap_or_else(|| panic!("no ANSWER in {comments:?}"));
    assert_eq!(answer["body"], "scope extended: Cargo.toml");
    assert_eq!(answer["reply_to"], qid.parse::<i64>().unwrap());
    assert_eq!(answer["to_lane"], a.as_str());

    let done = r.ok(&["--lane", &a, "issue", "finish", &a, "--report", &report]);
    assert_eq!(done["issue"]["status"], "in-review");

    // a second add does not answer the same question again
    let v = r.ok(&["issue", "scope", "add", &a, "docs/**"]);
    assert_eq!(v["answered"], serde_json::json!([]));
    assert_eq!(
        comments_of(&r, &a)
            .iter()
            .filter(|c| c["type"] == "ANSWER")
            .count(),
        1
    );
}

#[test]
fn scope_add_leaves_other_questions_unanswered() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--ready", "--scope", "src/**"]);
    let b = r.create_issue("b", &["--ready"]);
    let report = report_file(&r, "report.md");
    ask(&r, &a, "human", "which crate?");
    ask(&r, &a, &b, "SCOPE: Cargo.toml because a peer asked");

    let v = r.ok(&["issue", "scope", "add", &a, "Cargo.toml"]);
    assert_eq!(v["answered"], serde_json::json!([]));
    assert!(comments_of(&r, &a).iter().all(|c| c["type"] != "ANSWER"));
    let (code, _, _) = r.err(&["--lane", &a, "issue", "finish", &a, "--report", &report]);
    assert_eq!(code, 3, "the ordinary question to human still blocks finish");
}

#[test]
fn scope_rm_does_not_answer_scope_questions() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--ready", "--scope", "src/**", "--scope", "docs/**"]);
    ask(&r, &a, "human", "SCOPE: Cargo.toml because x");
    let v = r.ok(&["issue", "scope", "rm", &a, "docs/**"]);
    assert!(v.get("answered").is_none() || v["answered"] == serde_json::json!([]));
    assert!(comments_of(&r, &a).iter().all(|c| c["type"] != "ANSWER"));
}

// ---- a worker cannot pick its lane ------------------------------------------------------------

/// `asf <args> --json` as a worker pane runs it: `ASF_ISSUE` set to the worker's issue.
fn as_worker(r: &Repo, issue: &str, args: &[&str]) -> (serde_json::Value, i32) {
    let out = r
        .asf()
        .env("ASF_ISSUE", issue)
        .args(args)
        .arg("--json")
        .output()
        .unwrap();
    let v = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("not json: {e}\n{}", String::from_utf8_lossy(&out.stderr)));
    (v, out.status.code().unwrap_or(-1))
}

fn comment_as_worker(r: &Repo, issue: &str, lane: Option<&str>) -> (serde_json::Value, i32) {
    let mut args = Vec::new();
    if let Some(l) = lane {
        args.extend(["--lane", l]);
    }
    args.extend(["issue", "comment", issue, "--type", "FYI", "--body", "note"]);
    as_worker(r, issue, &args)
}

#[test]
fn a_worker_cannot_comment_as_human_orchestrator_or_another_issue() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--ready"]);
    let qid = ask(&r, &a, "human", "which crate?");
    for lane in ["human", "orchestrator", "asf-9"] {
        let (v, code) = as_worker(
            &r,
            &a,
            &[
                "--lane",
                lane,
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
                "self-service",
            ],
        );
        assert_eq!(code, 2, "--lane {lane}: {v}");
        assert_eq!(v["error"]["code"], "usage", "{v}");
        let msg = v["error"]["message"].as_str().unwrap();
        assert!(msg.contains("a worker speaks only as its own lane"), "{msg}");
        assert!(msg.contains(lane), "{msg}");
    }
    assert!(
        comments_of(&r, &a).iter().all(|c| c["type"] != "ANSWER"),
        "a refused call writes nothing"
    );
}

#[test]
fn a_worker_keeps_its_own_lane_with_or_without_the_flag() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--ready"]);
    for lane in [Some(a.as_str()), None] {
        let (v, code) = comment_as_worker(&r, &a, lane);
        assert_eq!(code, 0, "{v}");
        assert_eq!(v["result"]["author"], format!("agent:{a}"), "{v}");
    }
}

#[test]
fn the_flag_still_selects_any_lane_outside_a_worker() {
    let r = Repo::initialised();
    let a = r.create_issue("a", &["--ready"]);
    for (lane, author) in [("human", "human"), ("orchestrator", "orchestrator")] {
        let v = r.ok(&[
            "--lane", lane, "issue", "comment", &a, "--type", "FYI", "--body", "n",
        ]);
        assert_eq!(v["author"], author, "{v}");
    }
}
