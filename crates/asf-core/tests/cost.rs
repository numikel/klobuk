//! The cost rollup: per epic and repo-wide, from plain issues and runs.

use asf_core::cost::{epic_breakdown, rollup};
use asf_core::{Issue, IssueId, IssueType, Run, RunState, Status};

fn id(s: &str) -> IssueId {
    s.parse().unwrap()
}

fn approx(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-9, "{a} != {b}");
}

fn issue(i: &str, kind: IssueType, status: Status, parent: Option<&str>, title: &str) -> Issue {
    Issue {
        id: id(i),
        uid: format!("uid-{i}"),
        title: title.into(),
        body: String::new(),
        kind,
        status,
        priority: 2,
        parent_id: parent.map(id),
        file_scope: Vec::new(),
        acceptance: Vec::new(),
        estimate: None,
        labels: Vec::new(),
        source: None,
        created_at: "2026-11-08T10:00:00Z".into(),
        updated_at: "2026-11-08T10:00:00Z".into(),
        closed_at: None,
    }
}

fn epic(i: &str) -> Issue {
    issue(i, IssueType::Epic, Status::Backlog, None, &format!("Epic {i}"))
}

fn task(i: &str, status: Status, parent: Option<&str>) -> Issue {
    issue(i, IssueType::Task, status, parent, &format!("Task {i}"))
}

fn review(i: &str, parent: Option<&str>, title: &str) -> Issue {
    issue(i, IssueType::Review, Status::Done, parent, title)
}

fn run(issue: &str, state: RunState, cost: f64) -> Run {
    Run {
        id: format!("run-{issue}-{cost}"),
        issue_id: id(issue),
        attempt: 1,
        runtime: "herdr".into(),
        agent_name: None,
        workspace_id: None,
        pane_id: None,
        claude_session_id: None,
        transcript_path: None,
        worktree: format!(".worktrees/{issue}"),
        branch: format!("feat/{issue}"),
        base_sha: "abc".into(),
        state,
        budget_usd: None,
        cost_usd: cost,
        input_tokens: 0,
        output_tokens: 0,
        turns: 0,
        tool_calls: 0,
        exit_reason: None,
        report_path: None,
        started_at: "2026-11-08T10:00:00Z".into(),
        last_seen_at: None,
        finished_at: None,
        herdr_revision: None,
    }
}

/// Epic asf-1: tasks asf-2 (done) and asf-3 (aborted run, still open), three reviews (two of asf-2, one
/// of asf-3). Epic asf-7: task asf-8 (done) with one review, asf-9.
fn desk() -> (Vec<Issue>, Vec<Run>) {
    let issues = vec![
        epic("asf-1"),
        task("asf-2", Status::Done, Some("asf-1")),
        task("asf-3", Status::InProgress, Some("asf-1")),
        review("asf-4", Some("asf-2"), "Review asf-2: Task asf-2"),
        review("asf-5", Some("asf-2"), "Review asf-2: Task asf-2"),
        review("asf-6", Some("asf-3"), "Review asf-3: Task asf-3"),
        epic("asf-7"),
        task("asf-8", Status::Done, Some("asf-7")),
        review("asf-9", Some("asf-8"), "Review asf-8: Task asf-8"),
    ];
    let runs = vec![
        run("asf-2", RunState::Succeeded, 2.0),
        run("asf-3", RunState::Aborted, 1.5),
        run("asf-4", RunState::Succeeded, 0.5),
        run("asf-5", RunState::Succeeded, 0.25),
        run("asf-6", RunState::Aborted, 0.25),
        run("asf-8", RunState::Succeeded, 3.0),
        run("asf-9", RunState::Succeeded, 0.4),
    ];
    (issues, runs)
}

#[test]
fn an_epic_sums_its_task_runs_and_the_reviews_of_its_tasks() {
    let (issues, runs) = desk();
    let report = rollup(&issues, &runs);
    let e = report.epics.iter().find(|e| e.epic == id("asf-1")).unwrap();
    assert_eq!((e.cost.tasks, e.cost.done), (2, 1));
    approx(e.cost.task_cost, 3.5);
    assert_eq!(e.cost.reviews, 3);
    approx(e.cost.review_cost, 1.0);
    approx(e.cost.total, 4.5);
    assert_eq!(e.title, "Epic asf-1");
}

#[test]
fn an_aborted_run_counts_in_the_cost_but_its_task_is_not_done() {
    let (issues, runs) = desk();
    let report = rollup(&issues, &runs);
    let e = report.epics.iter().find(|e| e.epic == id("asf-1")).unwrap();
    // asf-3 is still in progress: 1.5 of its aborted run is in the cost, it is not in `done`
    assert_eq!(e.cost.done, 1);
    approx(e.cost.task_cost, 2.0 + 1.5);
}

#[test]
fn a_review_is_charged_to_the_epic_of_the_task_it_reviewed() {
    let (issues, runs) = desk();
    let report = rollup(&issues, &runs);
    let other = report.epics.iter().find(|e| e.epic == id("asf-7")).unwrap();
    assert_eq!((other.cost.tasks, other.cost.done), (1, 1));
    approx(other.cost.task_cost, 3.0);
    assert_eq!(other.cost.reviews, 1);
    approx(other.cost.review_cost, 0.4);
    approx(other.cost.total, 3.4);
    // and not to asf-1
    let first = report.epics.iter().find(|e| e.epic == id("asf-1")).unwrap();
    approx(first.cost.review_cost, 1.0);
}

#[test]
fn epics_come_in_id_order() {
    let (issues, runs) = desk();
    let ids: Vec<String> = rollup(&issues, &runs)
        .epics
        .iter()
        .map(|e| e.epic.to_string())
        .collect();
    assert_eq!(ids, ["asf-1", "asf-7"]);
}

#[test]
fn averages_use_succeeded_runs_only() {
    let (issues, runs) = desk();
    let a = rollup(&issues, &runs).averages;
    // task runs: asf-2 2.0 and asf-8 3.0 (the aborted asf-3 run is left out)
    assert_eq!(a.task_runs.n, 2);
    approx(a.task_runs.mean.unwrap(), 2.5);
    approx(a.task_runs.median.unwrap(), 2.5);
    // review runs: 0.5, 0.25, 0.4 (the aborted 0.25 is left out)
    assert_eq!(a.review_runs.n, 3);
    approx(a.review_runs.mean.unwrap(), 1.15 / 3.0);
    approx(a.review_runs.median.unwrap(), 0.4);
}

#[test]
fn the_review_share_is_review_cost_over_everything_aborted_runs_included() {
    let (issues, runs) = desk();
    let report = rollup(&issues, &runs);
    approx(report.total, 4.5 + 3.4);
    approx(report.averages.review_share.unwrap(), 1.4 / 7.9);
}

#[test]
fn the_median_of_an_odd_count_is_the_middle_run() {
    let issues = vec![
        task("asf-1", Status::Done, None),
        task("asf-2", Status::Done, None),
        task("asf-3", Status::Done, None),
    ];
    let runs = vec![
        run("asf-1", RunState::Succeeded, 9.0),
        run("asf-2", RunState::Succeeded, 1.0),
        run("asf-3", RunState::Succeeded, 2.0),
    ];
    let a = rollup(&issues, &runs).averages;
    approx(a.task_runs.median.unwrap(), 2.0);
    approx(a.task_runs.mean.unwrap(), 4.0);
}

#[test]
fn nothing_to_average_gives_none_not_zero() {
    let report = rollup(&[], &[]);
    assert!(report.epics.is_empty());
    assert_eq!(report.averages.task_runs.n, 0);
    assert!(report.averages.task_runs.mean.is_none());
    assert!(report.averages.task_runs.median.is_none());
    assert!(report.averages.review_runs.mean.is_none());
    assert!(report.averages.review_share.is_none());
    approx(report.total, 0.0);
}

#[test]
fn tasks_without_an_epic_are_kept_apart_but_count_in_the_averages() {
    let issues = vec![
        task("asf-1", Status::Done, None),
        review("asf-2", Some("asf-1"), "Review asf-1: Task asf-1"),
    ];
    let runs = vec![
        run("asf-1", RunState::Succeeded, 4.0),
        run("asf-2", RunState::Succeeded, 1.0),
    ];
    let report = rollup(&issues, &runs);
    assert!(report.epics.is_empty());
    assert_eq!((report.no_epic.tasks, report.no_epic.done), (1, 1));
    approx(report.no_epic.task_cost, 4.0);
    assert_eq!(report.no_epic.reviews, 1);
    approx(report.no_epic.review_cost, 1.0);
    approx(report.no_epic.total, 5.0);
    approx(report.total, 5.0);
    approx(report.averages.task_runs.mean.unwrap(), 4.0);
    approx(report.averages.review_share.unwrap(), 0.2);
}

#[test]
fn a_review_without_a_parent_is_found_by_its_title() {
    let issues = vec![
        epic("asf-1"),
        task("asf-2", Status::Done, Some("asf-1")),
        review("asf-3", None, "Review asf-2: Task asf-2"),
    ];
    let runs = vec![
        run("asf-2", RunState::Succeeded, 2.0),
        run("asf-3", RunState::Succeeded, 0.5),
    ];
    let report = rollup(&issues, &runs);
    assert_eq!(report.epics[0].cost.reviews, 1);
    approx(report.epics[0].cost.review_cost, 0.5);
}

#[test]
fn a_task_under_a_task_belongs_to_the_epic_above_both() {
    let issues = vec![
        epic("asf-1"),
        task("asf-2", Status::Done, Some("asf-1")),
        task("asf-3", Status::Done, Some("asf-2")),
    ];
    let runs = vec![run("asf-3", RunState::Succeeded, 1.0)];
    let report = rollup(&issues, &runs);
    assert_eq!(report.epics[0].cost.tasks, 2);
    approx(report.epics[0].cost.task_cost, 1.0);
}

#[test]
fn several_runs_of_one_task_all_count_in_its_cost() {
    let issues = vec![epic("asf-1"), task("asf-2", Status::Done, Some("asf-1"))];
    let runs = vec![
        run("asf-2", RunState::Failed, 1.0),
        run("asf-2", RunState::Succeeded, 2.0),
    ];
    let report = rollup(&issues, &runs);
    approx(report.epics[0].cost.task_cost, 3.0);
    // one succeeded run is one data point of the average
    assert_eq!(report.averages.task_runs.n, 1);
    approx(report.averages.task_runs.mean.unwrap(), 2.0);
}

#[test]
fn a_run_of_an_unknown_issue_is_ignored() {
    let issues = vec![epic("asf-1"), task("asf-2", Status::Done, Some("asf-1"))];
    let runs = vec![run("asf-99", RunState::Succeeded, 5.0)];
    let report = rollup(&issues, &runs);
    approx(report.total, 0.0);
    approx(report.epics[0].cost.total, 0.0);
}

#[test]
fn an_epic_without_children_is_listed_with_zero() {
    let report = rollup(&[epic("asf-1")], &[]);
    assert_eq!(report.epics.len(), 1);
    assert_eq!(report.epics[0].cost.tasks, 0);
    approx(report.epics[0].cost.total, 0.0);
}

#[test]
fn the_breakdown_lists_the_tasks_with_their_own_and_their_reviews_cost() {
    let (issues, runs) = desk();
    let b = epic_breakdown(&issues, &runs, &id("asf-1")).unwrap();
    approx(b.epic.cost.total, 4.5);
    let ids: Vec<String> = b.issues.iter().map(|i| i.id.to_string()).collect();
    assert_eq!(ids, ["asf-2", "asf-3"]);
    let first = &b.issues[0];
    assert_eq!(first.status, Status::Done);
    approx(first.task_cost, 2.0);
    assert_eq!(first.reviews, 2);
    approx(first.review_cost, 0.75);
    approx(first.total, 2.75);
    let second = &b.issues[1];
    approx(second.task_cost, 1.5);
    assert_eq!(second.reviews, 1);
    approx(second.review_cost, 0.25);
    // the rows add up to the epic
    approx(b.issues.iter().map(|i| i.total).sum::<f64>(), b.epic.cost.total);
}

#[test]
fn the_breakdown_of_something_that_is_not_an_epic_is_none() {
    let (issues, runs) = desk();
    assert!(epic_breakdown(&issues, &runs, &id("asf-2")).is_none());
    assert!(epic_breakdown(&issues, &runs, &id("asf-99")).is_none());
}

#[test]
fn the_report_serialises_with_flat_cost_fields() {
    let (issues, runs) = desk();
    let v = serde_json::to_value(rollup(&issues, &runs)).unwrap();
    let e = &v["epics"][0];
    assert_eq!(e["epic"], "asf-1");
    assert_eq!(e["tasks"], 2);
    assert_eq!(e["done"], 1);
    assert_eq!(e["reviews"], 3);
    assert!(e["task_cost"].is_number() && e["review_cost"].is_number() && e["total"].is_number());
    assert_eq!(v["averages"]["task_runs"]["n"], 2);
    assert!(v["averages"]["review_share"].is_number());
}
