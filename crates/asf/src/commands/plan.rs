//! `asf plan import <file.md> [--dry-run] [--no-epic]`

use std::collections::BTreeMap;
use std::path::Path;

use asf_core::*;
use serde::Serialize;

use crate::cli::{Cli, PlanCmd};
use crate::ctx::Ctx;
use crate::output::{AppError, Rendered};

#[derive(Serialize)]
struct DryRun {
    dry_run: bool,
    title: String,
    goal: Option<String>,
    tasks: Vec<DraftTask>,
}

#[derive(Serialize)]
struct ImportResult {
    epic: Option<IssueId>,
    created: Vec<IssueId>,
    skipped: Vec<IssueId>,
}

pub fn run(cli: &Cli, cmd: &PlanCmd) -> Result<Rendered, AppError> {
    match cmd {
        PlanCmd::Import {
            file,
            dry_run,
            no_epic,
        } => import(cli, file, *dry_run, *no_epic),
    }
}

fn source_prefix(file: &Path) -> String {
    file.to_string_lossy().replace('\\', "/")
}

fn import(cli: &Cli, file: &Path, dry_run: bool, no_epic: bool) -> Result<Rendered, AppError> {
    let raw = std::fs::read(file).map_err(|e| AppError::io(format!("{}: {e}", file.display())))?;
    let text = String::from_utf8(raw)
        .map_err(|_| AppError::validation(format!("{} is not UTF-8", file.display())))?;
    let plan = parse_md_tasks(text.trim_start_matches('\u{feff}'))
        .map_err(|e| AppError::validation(e.to_string()))?;
    if dry_run {
        let text = format!(
            "{}\n{}\n{}",
            plan.title,
            plan.goal.as_deref().unwrap_or(""),
            plan.tasks
                .iter()
                .map(|t| format!(
                    "  Task {}: {} [deps {:?}] scope {:?}",
                    t.number, t.title, t.depends_on, t.file_scope
                ))
                .collect::<Vec<_>>()
                .join("\n")
        );
        return Rendered::new(
            &DryRun {
                dry_run: true,
                title: plan.title,
                goal: plan.goal,
                tasks: plan.tasks,
            },
            text,
        );
    }
    let ctx = Ctx::load(cli)?;
    let prefix = source_prefix(file);
    let existing = ctx.store.list_issues(&IssueFilter::default())?;
    let by_source: BTreeMap<String, IssueId> = existing
        .iter()
        .filter_map(|i| i.source.clone().map(|s| (s, i.id.clone())))
        .collect();

    let epic = if no_epic {
        None
    } else {
        let src = format!("{prefix}#epic");
        match by_source.get(&src) {
            Some(id) => Some(id.clone()),
            None => Some(
                ctx.store
                    .create_issue(&NewIssue {
                        title: plan.title.clone(),
                        body: plan.goal.clone().unwrap_or_default(),
                        kind: IssueType::Epic,
                        source: Some(src),
                        ..NewIssue::default()
                    })?
                    .id,
            ),
        }
    };

    let mut ids: BTreeMap<u32, IssueId> = BTreeMap::new();
    let mut created = Vec::new();
    let mut skipped = Vec::new();
    for t in &plan.tasks {
        let src = format!("{prefix}#task-{}", t.number);
        if let Some(id) = by_source.get(&src) {
            ids.insert(t.number, id.clone());
            skipped.push(id.clone());
            continue;
        }
        let deps: Vec<IssueId> = t.depends_on.iter().filter_map(|n| ids.get(n).cloned()).collect();
        let issue = ctx.store.create_issue(&NewIssue {
            title: t.title.clone(),
            body: t.body.clone(),
            kind: IssueType::Task,
            parent_id: epic.clone(),
            file_scope: t.file_scope.clone(),
            acceptance: t
                .acceptance
                .iter()
                .map(|a| Acceptance {
                    text: a.clone(),
                    verify: None,
                })
                .collect(),
            source: Some(src),
            deps,
            ..NewIssue::default()
        })?;
        ids.insert(t.number, issue.id.clone());
        created.push(issue.id);
    }
    super::export::auto(&ctx)?;
    let res = ImportResult {
        epic,
        created,
        skipped,
    };
    let text = format!(
        "imported {}: {} created, {} skipped{}",
        plan.title,
        res.created.len(),
        res.skipped.len(),
        res.epic
            .as_ref()
            .map(|e| format!(", epic {e}"))
            .unwrap_or_default()
    );
    Rendered::new(&res, text)
}
