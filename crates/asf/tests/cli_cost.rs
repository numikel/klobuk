mod common;

use asf_core::*;
use asf_store::SqliteStore;
use assert_cmd::prelude::*;
use common::Repo;

fn store(r: &Repo) -> SqliteStore {
    SqliteStore::open(&r.path().join(".factory/factory.db")).unwrap()
}

fn make(s: &SqliteStore, title: &str, kind: IssueType, status: Status, parent: Option<&IssueId>) -> IssueId {
    s.create_issue(&NewIssue {
        title: title.into(),
        kind,
        status,
        parent_id: parent.cloned(),
        ..NewIssue::default()
    })
    .unwrap()
    .id
}

/// A run of `issue` that ended in `state` having cost `cost`.
fn spend(s: &SqliteStore, issue: &IssueId, state: RunState, cost: f64) {
    let run = s
        .create_run(&NewRun {
            issue_id: issue.clone(),
            runtime: "herdr".into(),
            worktree: format!(".worktrees/{issue}"),
            branch: format!("feat/{issue}"),
            base_sha: "abc".into(),
            budget_usd: None,
        })
        .unwrap();
    s.update_run(
        &run.id,
        &RunPatch {
            state: Some(state),
            cost_usd: Some(cost),
            ..RunPatch::default()
        },
    )
    .unwrap();
}

/// Epic `Round 4` with a done task (one succeeded run, two reviews), an open task (an aborted run) and a
/// third task without a run; a second epic with one done task and one review.
fn seeded() -> (Repo, [IssueId; 2]) {
    let r = Repo::initialised();
    let s = store(&r);
    let e1 = make(&s, "Round 4", IssueType::Epic, Status::Backlog, None);
    let a = make(&s, "Strip env vars", IssueType::Task, Status::Done, Some(&e1));
    let b = make(&s, "Retry start", IssueType::Task, Status::InProgress, Some(&e1));
    make(&s, "Idle task", IssueType::Task, Status::Backlog, Some(&e1));
    let ra = make(&s, "Review a", IssueType::Review, Status::Done, Some(&a));
    let rb = make(&s, "Review a again", IssueType::Review, Status::Done, Some(&a));
    let e2 = make(&s, "Round 3", IssueType::Epic, Status::Done, None);
    let c = make(&s, "Review gate", IssueType::Task, Status::Done, Some(&e2));
    let rc = make(&s, "Review c", IssueType::Review, Status::Done, Some(&c));
    spend(&s, &a, RunState::Succeeded, 2.0);
    spend(&s, &b, RunState::Aborted, 1.5);
    spend(&s, &ra, RunState::Succeeded, 0.5);
    spend(&s, &rb, RunState::Succeeded, 0.25);
    spend(&s, &c, RunState::Succeeded, 3.0);
    spend(&s, &rc, RunState::Succeeded, 0.4);
    (r, [e1, e2])
}

fn text(r: &Repo, args: &[&str]) -> String {
    let out = r.asf().args(args).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

fn num(v: &serde_json::Value) -> f64 {
    v.as_f64().unwrap_or_else(|| panic!("not a number: {v}"))
}

#[test]
fn json_has_a_row_per_epic_and_the_averages() {
    let (r, [e1, e2]) = seeded();
    let res = r.ok(&["cost"]);
    let epics = res["epics"].as_array().unwrap();
    assert_eq!(epics.len(), 2);
    let first = &epics[0];
    assert_eq!(first["epic"], e1.as_str());
    assert_eq!(first["title"], "Round 4");
    assert_eq!(
        (first["tasks"].as_u64(), first["done"].as_u64()),
        (Some(3), Some(1))
    );
    assert!((num(&first["task_cost"]) - 3.5).abs() < 1e-9);
    assert_eq!(first["reviews"], 2);
    assert!((num(&first["review_cost"]) - 0.75).abs() < 1e-9);
    assert!((num(&first["total"]) - 4.25).abs() < 1e-9);
    assert_eq!(epics[1]["epic"], e2.as_str());
    assert!((num(&epics[1]["total"]) - 3.4).abs() < 1e-9);

    let a = &res["averages"];
    assert_eq!(a["task_runs"]["n"], 2);
    assert!((num(&a["task_runs"]["mean"]) - 2.5).abs() < 1e-9);
    assert_eq!(a["review_runs"]["n"], 3);
    assert!((num(&a["review_runs"]["median"]) - 0.4).abs() < 1e-9);
    assert!((num(&a["review_share"]) - 1.15 / 7.65).abs() < 1e-9);
    assert!((num(&res["total"]) - 7.65).abs() < 1e-9);
}

#[test]
fn the_table_lists_every_epic_and_one_averages_line() {
    let (r, [e1, e2]) = seeded();
    let out = text(&r, &["cost"]);
    let lines: Vec<&str> = out.lines().collect();
    let header = lines[0];
    for col in ["epic", "tasks", "task cost", "reviews", "review cost", "total"] {
        assert!(header.contains(col), "{header}");
    }
    let row1 = lines.iter().find(|l| l.starts_with(e1.as_str())).unwrap();
    assert!(
        row1.contains("1/3") && row1.contains("$3.50") && row1.contains("$0.75"),
        "{row1}"
    );
    assert!(row1.contains("$4.25") && row1.contains("Round 4"), "{row1}");
    let row2 = lines.iter().find(|l| l.starts_with(e2.as_str())).unwrap();
    assert!(
        row2.contains("1/1") && row2.contains("$3.00") && row2.contains("$3.40"),
        "{row2}"
    );
    let avg: Vec<&&str> = lines.iter().filter(|l| l.contains("median")).collect();
    assert_eq!(avg.len(), 1, "{out}");
    assert!(avg[0].contains("$2.50") && avg[0].contains("$0.40"), "{}", avg[0]);
    assert!(avg[0].contains("15%"), "{}", avg[0]);
}

#[test]
fn an_empty_store_says_so_and_has_no_averages() {
    let r = Repo::initialised();
    let res = r.ok(&["cost"]);
    assert_eq!(res["epics"].as_array().unwrap().len(), 0);
    assert!(res["averages"]["task_runs"]["mean"].is_null());
    assert!(res["averages"]["review_share"].is_null());
    let out = text(&r, &["cost"]);
    assert!(out.contains("no epics"), "{out}");
}

#[test]
fn work_without_an_epic_gets_its_own_row() {
    let r = Repo::initialised();
    let s = store(&r);
    let t = make(&s, "Loose", IssueType::Task, Status::Done, None);
    spend(&s, &t, RunState::Succeeded, 1.25);
    let res = r.ok(&["cost"]);
    assert!((num(&res["no_epic"]["total"]) - 1.25).abs() < 1e-9);
    let out = text(&r, &["cost"]);
    assert!(out.contains("(no epic)") && out.contains("$1.25"), "{out}");
}

#[test]
fn epic_lists_its_tasks_with_task_and_review_cost() {
    let (r, [e1, _]) = seeded();
    let res = r.ok(&["cost", "--epic", e1.as_str()]);
    assert_eq!(res["epic"], e1.as_str());
    assert!((num(&res["total"]) - 4.25).abs() < 1e-9);
    let rows = res["issues"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["status"], "done");
    assert_eq!(rows[0]["reviews"], 2);
    assert!((num(&rows[0]["task_cost"]) - 2.0).abs() < 1e-9);
    assert!((num(&rows[0]["review_cost"]) - 0.75).abs() < 1e-9);
    assert_eq!(rows[1]["status"], "in-progress");
    assert!((num(&rows[1]["task_cost"]) - 1.5).abs() < 1e-9);
    assert_eq!(rows[2]["runs"], 0);

    let out = text(&r, &["cost", "--epic", e1.as_str()]);
    assert!(out.lines().next().unwrap().contains("Round 4"), "{out}");
    let a = out.lines().find(|l| l.contains("Strip env vars")).unwrap();
    assert!(
        a.contains("done") && a.contains("$2.00") && a.contains("$0.75") && a.contains("$2.75"),
        "{a}"
    );
    let b = out.lines().find(|l| l.contains("Retry start")).unwrap();
    assert!(b.contains("in-progress") && b.contains("$1.50"), "{b}");
}

#[test]
fn epic_of_something_that_is_not_an_epic_is_not_found() {
    let (r, _) = seeded();
    let task = r.ok(&["issue", "list", "--type", "task"])[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    for id in [task.as_str(), "asf-999"] {
        let (code, ecode, msg) = r.err(&["cost", "--epic", id]);
        assert_eq!((code, ecode.as_str()), (1, "not_found"), "{id}");
        assert!(msg.contains("not an epic"), "{msg}");
    }
}

#[test]
fn a_bad_epic_id_is_a_usage_error() {
    let (r, _) = seeded();
    r.asf().args(["cost", "--epic", "NOT AN ID"]).assert().code(2);
}

#[test]
fn a_worker_may_read_the_costs() {
    let (r, _) = seeded();
    let out = r
        .asf()
        .env("ASF_RUN", "run-1")
        .args(["cost", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
}

/// The UTC date the command is about to read; taken before and after so a midnight in between is no flake.
fn utc_today() -> String {
    time::OffsetDateTime::now_utc().date().to_string()
}

#[test]
fn json_has_a_today_object_next_to_the_unchanged_fields() {
    let (r, _) = seeded();
    let before = utc_today();
    let res = r.ok(&["cost"]);
    let after = utc_today();
    let t = &res["today"];
    let date = t["date"].as_str().unwrap();
    assert!(date == before || date == after, "{date}");
    // every seeded run started just now: 3 task runs ($2.00 + $1.50 + $3.00), 3 review runs ($1.15)
    assert_eq!(
        (t["task_runs"].as_u64(), t["review_runs"].as_u64()),
        (Some(3), Some(3))
    );
    assert!((num(&t["task_cost"]) - 6.5).abs() < 1e-9);
    assert!((num(&t["review_cost"]) - 1.15).abs() < 1e-9);
    assert!((num(&t["total"]) - 7.65).abs() < 1e-9);
    for key in ["epics", "no_epic", "averages", "total"] {
        assert!(!res[key].is_null(), "{key} is gone");
    }
}

#[test]
fn the_text_ends_with_the_today_line() {
    let (r, _) = seeded();
    let before = utc_today();
    let out = text(&r, &["cost"]);
    let after = utc_today();
    let last = out.lines().last().unwrap();
    let ok = [before, after]
        .iter()
        .any(|d| last == format!("today {d}: 3 task runs $6.50, 3 review runs $1.15, total $7.65"));
    assert!(ok, "{out}");
}

#[test]
fn with_no_runs_today_says_so_with_zeros() {
    let r = Repo::initialised();
    let res = r.ok(&["cost"]);
    let t = &res["today"];
    assert_eq!(
        (t["task_runs"].as_u64(), t["review_runs"].as_u64()),
        (Some(0), Some(0))
    );
    assert_eq!(num(&t["total"]), 0.0);
    let out = text(&r, &["cost"]);
    let last = out.lines().last().unwrap();
    assert!(
        last.starts_with("today ") && last.ends_with("0 task runs $0.00, 0 review runs $0.00, total $0.00"),
        "{out}"
    );
}

#[test]
fn epic_output_has_no_today() {
    let (r, [e1, _]) = seeded();
    assert!(r.ok(&["cost", "--epic", e1.as_str()])["today"].is_null());
    assert!(!text(&r, &["cost", "--epic", e1.as_str()]).contains("today"));
}
