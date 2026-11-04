//! `asf plan import <file.md>` and `asf plan new --from <idea.md>`.
//!
//! Both end in the same place: an epic plus one issue per task, with dependencies, file scopes and
//! acceptance criteria, each carrying a `source` marker so re-running is idempotent.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use asf_core::*;
use asf_runtime::llm::{planner_from_config, StructuredRequest};
use serde::Serialize;
use serde_json::Value;

use super::method::{active_prompts, PromptStage};
use crate::cli::{Cli, PlanCmd};
use crate::ctx::{Ctx, FACTORY_DIR};
use crate::output::{AppError, ErrorCode, Rendered};

#[derive(Serialize)]
struct DryRun {
    dry_run: bool,
    title: String,
    goal: Option<String>,
    tasks: Vec<DraftTask>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prd_path: Option<String>,
}

#[derive(Serialize)]
struct ImportResult {
    epic: Option<IssueId>,
    created: Vec<IssueId>,
    skipped: Vec<IssueId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prd_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cost_usd: Option<f64>,
}

pub fn run(cli: &Cli, cmd: &PlanCmd) -> Result<Rendered, AppError> {
    match cmd {
        PlanCmd::Import {
            file,
            dry_run,
            no_epic,
            format,
        } => import(cli, file, *dry_run, *no_epic, *format),
        PlanCmd::New {
            from,
            dry_run,
            no_epic,
            slug,
        } => new(cli, from, *dry_run, *no_epic, slug.as_deref()),
    }
}

fn read_md(file: &Path) -> Result<String, AppError> {
    let raw = std::fs::read(file).map_err(|e| AppError::io(format!("{}: {e}", file.display())))?;
    let text = String::from_utf8(raw)
        .map_err(|_| AppError::validation(format!("{} is not UTF-8", file.display())))?;
    Ok(text.trim_start_matches('\u{feff}').replace("\r\n", "\n"))
}

fn dry_run_view(plan: &ParsedPlan, prd_path: Option<String>) -> Result<Rendered, AppError> {
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
    Rendered::new(
        &DryRun {
            dry_run: true,
            title: plan.title.clone(),
            goal: plan.goal.clone(),
            tasks: plan.tasks.clone(),
            prd_path,
        },
        text,
    )
}

/// Creates the epic and the tasks of `plan`; issues whose `source` already exists are skipped.
fn create_from_plan(
    ctx: &Ctx,
    plan: &ParsedPlan,
    prefix: &str,
    no_epic: bool,
) -> Result<ImportResult, AppError> {
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
            estimate: t.estimate.clone(),
            source: Some(src),
            deps,
            ..NewIssue::default()
        })?;
        ids.insert(t.number, issue.id.clone());
        created.push(issue.id);
    }
    super::export::auto(ctx)?;
    Ok(ImportResult {
        epic,
        created,
        skipped,
        slug: None,
        prd_path: None,
        cost_usd: None,
    })
}

fn import(
    cli: &Cli,
    file: &Path,
    dry_run: bool,
    no_epic: bool,
    format: PlanFormat,
) -> Result<Rendered, AppError> {
    let text = read_md(file)?;
    let plan = parse_plan(&text, format).map_err(|e| AppError::validation(e.to_string()))?;
    if dry_run {
        return dry_run_view(&plan, None);
    }
    let ctx = Ctx::load(cli)?;
    let prefix = file.to_string_lossy().replace('\\', "/");
    let res = create_from_plan(&ctx, &plan, &prefix, no_epic)?;
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

pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    let t = out.trim_end_matches('-').to_string();
    if t.is_empty() {
        "plan".into()
    } else {
        t.chars()
            .take(48)
            .collect::<String>()
            .trim_end_matches('-')
            .to_string()
    }
}

fn tasks_from_value(v: &Value) -> Result<ParsedPlan, AppError> {
    let epic_title = v
        .pointer("/epic/title")
        .and_then(Value::as_str)
        .unwrap_or("Plan")
        .to_string();
    let epic_body = v
        .pointer("/epic/body")
        .and_then(Value::as_str)
        .map(str::to_string);
    let arr = v
        .get("tasks")
        .and_then(Value::as_array)
        .ok_or_else(|| AppError::validation("issues output has no tasks"))?;
    let strs = |x: Option<&Value>| -> Vec<String> {
        x.and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default()
    };
    let mut tasks = Vec::new();
    for (i, t) in arr.iter().enumerate() {
        tasks.push(DraftTask {
            number: t.get("number").and_then(Value::as_u64).unwrap_or(i as u64 + 1) as u32,
            title: t
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("untitled")
                .to_string(),
            body: t.get("body").and_then(Value::as_str).unwrap_or("").to_string(),
            file_scope: strs(t.get("file_scope")),
            acceptance: strs(t.get("acceptance")),
            depends_on: t
                .get("depends_on")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_u64).map(|n| n as u32).collect())
                .unwrap_or_default(),
            parallel: false,
            estimate: t.get("estimate").and_then(Value::as_str).map(str::to_string),
        });
    }
    let numbers: Vec<u32> = tasks.iter().map(|t| t.number).collect();
    for t in &tasks {
        for d in &t.depends_on {
            if !numbers.contains(d) {
                return Err(AppError::validation(format!(
                    "task {} depends on unknown task {d}",
                    t.number
                )));
            }
        }
    }
    Ok(ParsedPlan {
        title: epic_title,
        goal: epic_body,
        tasks,
    })
}

fn new(
    cli: &Cli,
    from: &Path,
    dry_run: bool,
    no_epic: bool,
    slug: Option<&str>,
) -> Result<Rendered, AppError> {
    let idea = read_md(from)?;
    let ctx = Ctx::load(cli)?;
    let method = active_prompts(&ctx)?;
    let cfg = &ctx.config.planner;
    let llm = planner_from_config(&cfg.runner, &ctx.root)
        .map_err(|e| AppError::new(ErrorCode::Backend, e.to_string()))?;
    let request = |stage: &PromptStage, stdin: String| StructuredRequest {
        schema_name: stage.schema_name.clone(),
        instruction: stage.prompt.replace("{{language}}", &method.language),
        stdin,
        schema: stage.schema.clone(),
        model: cfg.model.clone(),
        max_turns: cfg.max_turns,
        budget_usd: cfg.budget_usd,
        add_dirs: vec![],
    };
    let backend = |e: RuntimeError| AppError::new(ErrorCode::Backend, e.to_string());
    let pretty = |v: &Value| serde_json::to_string_pretty(v).unwrap_or_default();

    // Each prompt stage reads the previous stage's output (the idea for the first one): its
    // `prd_markdown` when it has one, the JSON otherwise. The last stage must yield the task list.
    let mut input = idea.clone();
    let mut cost = 0.0;
    let mut dir: Option<(String, PathBuf)> = None;
    let mut prd_path = String::new();
    let mut last = Value::Null;
    for stage in &method.stages {
        let out = llm.run(&request(stage, input.clone())).map_err(backend)?;
        cost += out.cost_usd;
        let (_, plan_dir) = match &dir {
            Some(d) => d,
            None => {
                let title = out.output.get("title").and_then(Value::as_str).unwrap_or("plan");
                let slug = slug.map(str::to_string).unwrap_or_else(|| slugify(title));
                let plan_dir: PathBuf = ctx.root.join(FACTORY_DIR).join("plan").join(&slug);
                std::fs::create_dir_all(&plan_dir)?;
                std::fs::write(plan_dir.join("idea.md"), &idea)?;
                dir.insert((slug, plan_dir))
            }
        };
        let json_file = plan_dir.join(format!("{}.json", stage.schema_name));
        std::fs::write(&json_file, pretty(&out.output))?;
        let markdown = out.output.get("prd_markdown").and_then(Value::as_str);
        if let Some(md) = markdown {
            std::fs::write(plan_dir.join("prd.md"), md)?;
        }
        if prd_path.is_empty() || markdown.is_some() {
            let shown = if markdown.is_some() {
                plan_dir.join("prd.md")
            } else {
                json_file
            };
            prd_path = shown.to_string_lossy().replace('\\', "/");
        }
        input = markdown
            .map(str::to_string)
            .unwrap_or_else(|| pretty(&out.output));
        last = out.output;
    }
    let (slug, _) = dir.expect("active_prompts guarantees at least one prompt stage");
    let plan = tasks_from_value(&last)?;
    if dry_run {
        return dry_run_view(&plan, Some(prd_path));
    }
    let mut res = create_from_plan(&ctx, &plan, &format!("plan:{slug}"), no_epic)?;
    res.slug = Some(slug.clone());
    res.prd_path = Some(prd_path.clone());
    res.cost_usd = Some(cost);
    let text = format!(
        "plan `{slug}`: PRD at {prd_path}; {} issue(s) created, {} skipped{}; llm cost ${cost:.3}",
        res.created.len(),
        res.skipped.len(),
        res.epic
            .as_ref()
            .map(|e| format!(", epic {e}"))
            .unwrap_or_default()
    );
    Rendered::new(&res, text)
}
