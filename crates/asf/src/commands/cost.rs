//! `asf cost` — what the runs cost, per epic and on average. Read-only.

use asf_core::cost::{epic_breakdown, rollup, CostReport, Costs, EpicBreakdown, RunStat};
use asf_core::{IssueFilter, IssueStore};

use crate::cli::{Cli, CostArgs};
use crate::ctx::Ctx;
use crate::output::{AppError, Rendered};

pub fn run(cli: &Cli, args: &CostArgs) -> Result<Rendered, AppError> {
    let ctx = Ctx::load(cli)?;
    let issues = ctx.store.list_issues(&IssueFilter::default())?;
    let runs = ctx.store.runs(None, false)?;
    match &args.epic {
        Some(id) => {
            let b = epic_breakdown(&issues, &runs, id)
                .ok_or_else(|| AppError::not_found(format!("{id} is not an epic")))?;
            let text = epic_text(&b);
            Rendered::new(&b, text)
        }
        None => {
            let report = rollup(&issues, &runs);
            let text = report_text(&report);
            Rendered::new(&report, text)
        }
    }
}

fn usd(v: f64) -> String {
    format!("${v:.2}")
}

/// Left- or right-aligned columns separated by two blanks; the last column is not padded.
fn table(header: &[&str], rows: &[Vec<String>], right: &[bool]) -> String {
    let mut widths: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let line = |cells: Vec<&str>| {
        let last = cells.len() - 1;
        let parts: Vec<String> = cells
            .iter()
            .enumerate()
            .map(|(n, c)| match (n == last, right[n]) {
                (true, _) => (*c).to_string(),
                (false, true) => format!("{c:>w$}", w = widths[n]),
                (false, false) => format!("{c:<w$}", w = widths[n]),
            })
            .collect();
        parts.join("  ").trim_end().to_string()
    };
    let mut out = vec![line(header.to_vec())];
    out.extend(rows.iter().map(|r| line(r.iter().map(String::as_str).collect())));
    out.join("\n")
}

fn group_cells(c: &Costs) -> [String; 5] {
    [
        format!("{}/{}", c.done, c.tasks),
        usd(c.task_cost),
        c.reviews.to_string(),
        usd(c.review_cost),
        usd(c.total),
    ]
}

fn stat_text(s: &RunStat) -> String {
    match (s.mean, s.median) {
        (Some(mean), Some(median)) => format!("mean {}, median {} (n={})", usd(mean), usd(median), s.n),
        _ => "n/a (n=0)".to_string(),
    }
}

fn report_text(r: &CostReport) -> String {
    let loose = r.no_epic.tasks > 0 || r.no_epic.reviews > 0 || r.no_epic.total > 0.0;
    if r.epics.is_empty() && !loose {
        return "no epics or tasks yet".to_string();
    }
    let mut rows: Vec<Vec<String>> = Vec::new();
    for e in &r.epics {
        let mut row = vec![e.epic.to_string()];
        row.extend(group_cells(&e.cost));
        row.push(e.title.clone());
        rows.push(row);
    }
    if loose {
        let mut row = vec!["(no epic)".to_string()];
        row.extend(group_cells(&r.no_epic));
        row.push(String::new());
        rows.push(row);
    }
    if rows.len() > 1 {
        let all = r
            .epics
            .iter()
            .map(|e| e.cost)
            .chain([r.no_epic])
            .fold(Costs::default(), |mut a, c| {
                a.tasks += c.tasks;
                a.done += c.done;
                a.task_cost += c.task_cost;
                a.reviews += c.reviews;
                a.review_cost += c.review_cost;
                a.total += c.total;
                a
            });
        let mut row = vec!["total".to_string()];
        row.extend(group_cells(&all));
        row.push(String::new());
        rows.push(row);
    }
    let mut out = table(
        &[
            "epic",
            "tasks",
            "task cost",
            "reviews",
            "review cost",
            "total",
            "title",
        ],
        &rows,
        &[false, true, true, true, true, true, false],
    );
    let share = r
        .averages
        .review_share
        .map_or_else(|| "n/a".to_string(), |s| format!("{:.0}%", s * 100.0));
    out.push_str(&format!(
        "\nsucceeded runs, task: {} · review: {} · reviews are {share} of the total",
        stat_text(&r.averages.task_runs),
        stat_text(&r.averages.review_runs),
    ));
    out
}

fn epic_text(b: &EpicBreakdown) -> String {
    let c = &b.epic.cost;
    let mut out = format!(
        "{}  {}  tasks {}/{} · task cost {} · reviews {} ({}) · total {}",
        b.epic.epic,
        b.epic.title,
        c.done,
        c.tasks,
        usd(c.task_cost),
        c.reviews,
        usd(c.review_cost),
        usd(c.total)
    );
    if b.issues.is_empty() {
        out.push_str("\nno tasks under this epic");
        return out;
    }
    let rows: Vec<Vec<String>> = b
        .issues
        .iter()
        .map(|i| {
            vec![
                i.id.to_string(),
                i.status.to_string(),
                i.runs.to_string(),
                usd(i.task_cost),
                i.reviews.to_string(),
                usd(i.review_cost),
                usd(i.total),
                i.title.clone(),
            ]
        })
        .collect();
    out.push('\n');
    out.push_str(&table(
        &[
            "issue",
            "status",
            "runs",
            "task cost",
            "reviews",
            "review cost",
            "total",
            "title",
        ],
        &rows,
        &[false, false, true, true, true, true, true, false],
    ));
    out
}
