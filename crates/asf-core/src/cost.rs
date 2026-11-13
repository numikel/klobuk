//! What the runs cost: totals per epic and averages per task run and per review run.
//!
//! A pure function of the issues and the runs, so the CLI and the TUI agree on every number.
//!
//! Rules:
//! - Every run counts in the cost, whatever its state (an aborted or failed run was paid for). Only a
//!   `succeeded` run is a data point of the averages, and only a `done` task counts as done.
//! - A task belongs to the nearest epic above it. A review is charged to the epic of the issue it
//!   reviewed: its parent (the store keeps the reviewed task there), or, for a review without a parent,
//!   the issue named in its title (`Review <id>: ...`).
//! - Tasks and reviews with no epic are rolled up apart ([`CostReport::no_epic`]) but still count in the
//!   repo-wide averages and total.
//! - A run whose issue is unknown, and a run of an epic itself, is ignored.

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;
use time::format_description::well_known::Rfc3339;
use time::{Date, OffsetDateTime, UtcOffset};

use crate::ids::IssueId;
use crate::model::{Issue, IssueType, Status};
use crate::run::{Run, RunState};

/// How far up the parent chain an epic is looked for.
const MAX_DEPTH: usize = 8;

/// Cost figures of one group of issues (an epic, or everything without one).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Costs {
    /// Tasks in the group.
    pub tasks: usize,
    /// Tasks that are `done`.
    pub done: usize,
    /// Cost of every run of those tasks.
    pub task_cost: f64,
    /// Review runs charged to the group.
    pub reviews: usize,
    /// Cost of those review runs.
    pub review_cost: f64,
    /// `task_cost + review_cost`.
    pub total: f64,
}

impl Costs {
    fn finish(&mut self) {
        self.total = self.task_cost + self.review_cost;
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EpicCost {
    pub epic: IssueId,
    pub title: String,
    #[serde(flatten)]
    pub cost: Costs,
}

/// Count, mean and median of the cost of a kind of run. `mean` and `median` are `None` without a run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct RunStat {
    pub n: usize,
    pub mean: Option<f64>,
    pub median: Option<f64>,
}

impl RunStat {
    fn of(mut costs: Vec<f64>) -> RunStat {
        costs.sort_by(f64::total_cmp);
        let n = costs.len();
        if n == 0 {
            return RunStat::default();
        }
        let median = if n % 2 == 1 {
            costs[n / 2]
        } else {
            (costs[n / 2 - 1] + costs[n / 2]) / 2.0
        };
        RunStat {
            n,
            mean: Some(costs.iter().sum::<f64>() / n as f64),
            median: Some(median),
        }
    }
}

/// Repo-wide averages over the `succeeded` runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Averages {
    pub task_runs: RunStat,
    pub review_runs: RunStat,
    /// Review cost over total cost (0..=1); `None` when nothing was spent.
    pub review_share: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CostReport {
    /// Every epic, ordered by id.
    pub epics: Vec<EpicCost>,
    /// Tasks and reviews that sit under no epic.
    pub no_epic: Costs,
    pub averages: Averages,
    /// Everything that was spent, the epics and `no_epic` together.
    pub total: f64,
}

/// One task of an epic with its own cost and the cost of the reviews of it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IssueCost {
    pub id: IssueId,
    pub title: String,
    pub status: Status,
    /// Runs of the task.
    pub runs: usize,
    pub task_cost: f64,
    /// Review runs of the task.
    pub reviews: usize,
    pub review_cost: f64,
    pub total: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EpicBreakdown {
    #[serde(flatten)]
    pub epic: EpicCost,
    /// The tasks of the epic, ordered by id.
    pub issues: Vec<IssueCost>,
}

/// The issue a review reviewed: its parent, else the id in a `Review <id>: ...` title.
fn reviewed(review: &Issue) -> Option<IssueId> {
    if let Some(p) = &review.parent_id {
        return Some(p.clone());
    }
    review
        .title
        .strip_prefix("Review ")?
        .split(':')
        .next()?
        .trim()
        .parse()
        .ok()
}

/// The nearest epic above `start` (or `start` itself when it is one).
fn epic_above<'a>(by_id: &HashMap<&'a IssueId, &'a Issue>, start: &IssueId) -> Option<&'a IssueId> {
    let mut at = start;
    for _ in 0..MAX_DEPTH {
        let i = by_id.get(at)?;
        if i.kind == IssueType::Epic {
            return Some(&i.id);
        }
        at = i.parent_id.as_ref()?;
    }
    None
}

/// Where the cost of an issue's runs goes: the task whose cost it is, and the epic above it.
struct Placement<'a> {
    /// The reviewed task for a review, the task itself otherwise.
    task: IssueId,
    review: bool,
    epic: Option<&'a IssueId>,
}

fn place<'a>(by_id: &HashMap<&'a IssueId, &'a Issue>, issue: &Issue) -> Option<Placement<'a>> {
    match issue.kind {
        IssueType::Epic => None,
        IssueType::Task => Some(Placement {
            task: issue.id.clone(),
            review: false,
            epic: issue.parent_id.as_ref().and_then(|p| epic_above(by_id, p)),
        }),
        IssueType::Review => Some(match reviewed(issue) {
            Some(target) => Placement {
                epic: epic_above(by_id, &target),
                task: target,
                review: true,
            },
            // nothing says what it reviewed: its cost still counts, under no epic
            None => Placement {
                task: issue.id.clone(),
                review: true,
                epic: None,
            },
        }),
    }
}

/// Rolls the runs up per epic and repo-wide. See the module docs for the rules.
pub fn rollup(issues: &[Issue], runs: &[Run]) -> CostReport {
    let by_id: HashMap<&IssueId, &Issue> = issues.iter().map(|i| (&i.id, i)).collect();
    let mut epics: BTreeMap<&IssueId, Costs> = issues
        .iter()
        .filter(|i| i.kind == IssueType::Epic)
        .map(|i| (&i.id, Costs::default()))
        .collect();
    let mut no_epic = Costs::default();

    for i in issues.iter().filter(|i| i.kind == IssueType::Task) {
        let group = place(&by_id, i)
            .and_then(|p| p.epic)
            .and_then(|e| epics.get_mut(e))
            .unwrap_or(&mut no_epic);
        group.tasks += 1;
        group.done += usize::from(i.status == Status::Done);
    }

    let (mut task_runs, mut review_runs) = (Vec::new(), Vec::new());
    for r in runs {
        let Some(issue) = by_id.get(&r.issue_id) else {
            continue;
        };
        let Some(p) = place(&by_id, issue) else {
            continue;
        };
        let group = p.epic.and_then(|e| epics.get_mut(e)).unwrap_or(&mut no_epic);
        if p.review {
            group.reviews += 1;
            group.review_cost += r.cost_usd;
            if r.state == RunState::Succeeded {
                review_runs.push(r.cost_usd);
            }
        } else {
            group.task_cost += r.cost_usd;
            if r.state == RunState::Succeeded {
                task_runs.push(r.cost_usd);
            }
        }
    }

    let mut report = CostReport {
        epics: Vec::new(),
        no_epic,
        averages: Averages::default(),
        total: 0.0,
    };
    for (id, mut cost) in epics {
        cost.finish();
        report.epics.push(EpicCost {
            epic: id.clone(),
            title: by_id[id].title.clone(),
            cost,
        });
    }
    report.no_epic.finish();
    let (task_total, review_total) = report
        .epics
        .iter()
        .map(|e| e.cost)
        .chain([report.no_epic])
        .fold((0.0, 0.0), |(t, r), c| (t + c.task_cost, r + c.review_cost));
    report.total = task_total + review_total;
    report.averages = Averages {
        task_runs: RunStat::of(task_runs),
        review_runs: RunStat::of(review_runs),
        review_share: (report.total > 0.0).then(|| review_total / report.total),
    };
    report
}

/// What the runs started on one UTC date cost.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TodayCost {
    /// The UTC date, `YYYY-MM-DD`.
    pub date: String,
    pub task_runs: usize,
    pub review_runs: usize,
    pub task_cost: f64,
    pub review_cost: f64,
    /// `task_cost + review_cost`.
    pub total: f64,
}

/// The runs whose `started_at` falls on the UTC date `date`, split into task and review runs. The date is
/// a parameter so the core reads no clock. The cost rules of the module apply: every run state counts, a
/// run of an epic or of an unknown issue is ignored. A `started_at` that is not RFC 3339 counts for no day.
pub fn today(issues: &[Issue], runs: &[Run], date: Date) -> TodayCost {
    let by_id: HashMap<&IssueId, &Issue> = issues.iter().map(|i| (&i.id, i)).collect();
    let mut out = TodayCost {
        date: date.to_string(),
        task_runs: 0,
        review_runs: 0,
        task_cost: 0.0,
        review_cost: 0.0,
        total: 0.0,
    };
    for r in runs {
        let started = OffsetDateTime::parse(&r.started_at, &Rfc3339)
            .ok()
            .map(|t| t.to_offset(UtcOffset::UTC).date());
        if started != Some(date) {
            continue;
        }
        let Some(p) = by_id.get(&r.issue_id).and_then(|issue| place(&by_id, issue)) else {
            continue;
        };
        if p.review {
            out.review_runs += 1;
            out.review_cost += r.cost_usd;
        } else {
            out.task_runs += 1;
            out.task_cost += r.cost_usd;
        }
    }
    out.total = out.task_cost + out.review_cost;
    out
}

/// The tasks of `epic` with their own cost and the cost of their reviews; `None` when `epic` is not an
/// epic of the store. The rows add up to the epic's total.
pub fn epic_breakdown(issues: &[Issue], runs: &[Run], epic: &IssueId) -> Option<EpicBreakdown> {
    let report = rollup(issues, runs);
    let head = report.epics.into_iter().find(|e| &e.epic == epic)?;
    let by_id: HashMap<&IssueId, &Issue> = issues.iter().map(|i| (&i.id, i)).collect();

    let mut rows: BTreeMap<&IssueId, IssueCost> = issues
        .iter()
        .filter(|i| i.kind == IssueType::Task)
        .filter(|i| place(&by_id, i).and_then(|p| p.epic) == Some(epic))
        .map(|i| {
            (
                &i.id,
                IssueCost {
                    id: i.id.clone(),
                    title: i.title.clone(),
                    status: i.status,
                    runs: 0,
                    task_cost: 0.0,
                    reviews: 0,
                    review_cost: 0.0,
                    total: 0.0,
                },
            )
        })
        .collect();
    for r in runs {
        let Some(p) = by_id.get(&r.issue_id).and_then(|issue| place(&by_id, issue)) else {
            continue;
        };
        let Some(row) = rows.get_mut(&p.task) else {
            continue;
        };
        if p.review {
            row.reviews += 1;
            row.review_cost += r.cost_usd;
        } else {
            row.runs += 1;
            row.task_cost += r.cost_usd;
        }
    }
    let issues: Vec<IssueCost> = rows
        .into_values()
        .map(|mut row| {
            row.total = row.task_cost + row.review_cost;
            row
        })
        .collect();
    Some(EpicBreakdown { epic: head, issues })
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    fn issue(id: &str, kind: IssueType, parent: Option<&str>) -> Issue {
        Issue {
            id: id.parse().unwrap(),
            uid: format!("uid-{id}"),
            title: id.into(),
            body: String::new(),
            kind,
            status: Status::Done,
            priority: 2,
            parent_id: parent.map(|p| p.parse().unwrap()),
            file_scope: Vec::new(),
            acceptance: Vec::new(),
            estimate: None,
            labels: Vec::new(),
            source: None,
            created_at: "2026-11-01T00:00:00Z".into(),
            updated_at: "2026-11-01T00:00:00Z".into(),
            closed_at: None,
        }
    }

    fn run(issue: &str, started_at: &str, cost: f64) -> Run {
        Run {
            id: format!("run-{issue}-{started_at}"),
            issue_id: issue.parse().unwrap(),
            attempt: 1,
            runtime: "herdr".into(),
            agent_name: None,
            workspace_id: None,
            pane_id: None,
            claude_session_id: None,
            transcript_path: None,
            worktree: String::new(),
            branch: String::new(),
            base_sha: String::new(),
            state: RunState::Aborted,
            budget_usd: None,
            cost_usd: cost,
            input_tokens: 0,
            output_tokens: 0,
            turns: 0,
            tool_calls: 0,
            exit_reason: None,
            report_path: None,
            started_at: started_at.into(),
            last_seen_at: None,
            finished_at: None,
            herdr_revision: None,
        }
    }

    fn desk() -> Vec<Issue> {
        vec![
            issue("asf-1", IssueType::Epic, None),
            issue("asf-2", IssueType::Task, Some("asf-1")),
            issue("asf-3", IssueType::Review, Some("asf-2")),
        ]
    }

    #[test]
    fn the_utc_day_has_its_edges_at_midnight() {
        let runs = [
            run("asf-2", "2026-11-12T23:59:59Z", 1.0),
            run("asf-2", "2026-11-13T00:00:00Z", 2.0),
            run("asf-3", "2026-11-13T23:59:59Z", 0.5),
            run("asf-3", "2026-11-14T00:00:00Z", 4.0),
        ];
        let t = today(&desk(), &runs, date!(2026 - 11 - 13));
        assert_eq!(t.date, "2026-11-13");
        assert_eq!((t.task_runs, t.review_runs), (1, 1));
        assert_eq!((t.task_cost, t.review_cost, t.total), (2.0, 0.5, 2.5));
        let before = today(&desk(), &runs, date!(2026 - 11 - 12));
        assert_eq!((before.task_runs, before.review_runs, before.total), (1, 0, 1.0));
    }

    #[test]
    fn an_offset_is_converted_to_utc_before_the_date_is_taken() {
        // 01:30 at +02:00 is 23:30 UTC the day before
        let runs = [run("asf-2", "2026-11-13T01:30:00+02:00", 3.0)];
        assert_eq!(today(&desk(), &runs, date!(2026 - 11 - 12)).total, 3.0);
        assert_eq!(today(&desk(), &runs, date!(2026 - 11 - 13)).total, 0.0);
    }

    #[test]
    fn runs_of_epics_and_unknown_issues_and_bad_dates_do_not_count() {
        let runs = [
            run("asf-1", "2026-11-13T10:00:00Z", 9.0),
            run("asf-9", "2026-11-13T10:00:00Z", 9.0),
            run("asf-2", "yesterday", 9.0),
        ];
        let t = today(&desk(), &runs, date!(2026 - 11 - 13));
        assert_eq!((t.task_runs, t.review_runs, t.total), (0, 0, 0.0));
    }
}
