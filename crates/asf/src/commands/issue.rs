//! `asf issue …`

use std::path::Path;

use asf_core::*;
use serde::Serialize;

use crate::cli::{Cli, CommentArgs, CreateArgs, DepCmd, EditArgs, IssueCmd, ListArgs};
use crate::ctx::Ctx;
use crate::output::{AppError, Rendered};

pub fn run(cli: &Cli, cmd: &IssueCmd) -> Result<Rendered, AppError> {
    let ctx = Ctx::load(cli)?;
    match cmd {
        IssueCmd::List(a) => list(&ctx, a),
        IssueCmd::Show { id, dossier } => show(&ctx, id, *dossier),
        IssueCmd::Create(a) => create(&ctx, a),
        IssueCmd::Edit(a) => edit(&ctx, a),
        IssueCmd::Ready => ready(&ctx),
        IssueCmd::Status { id, status } => set_status(&ctx, id, *status),
        IssueCmd::Dep { cmd } => dep(&ctx, cmd),
        IssueCmd::Comment(a) => comment(&ctx, a),
    }
}

/// Reads `--body` or `--body-file` (UTF-8, BOM stripped, CRLF normalised).
pub fn read_body(inline: Option<&str>, file: Option<&Path>) -> Result<String, AppError> {
    match (inline, file) {
        (Some(b), _) => Ok(b.to_string()),
        (None, Some(p)) => {
            let raw = std::fs::read(p).map_err(|e| AppError::io(format!("{}: {e}", p.display())))?;
            let s = String::from_utf8(raw)
                .map_err(|_| AppError::validation(format!("{} is not valid UTF-8", p.display())))?;
            Ok(s.trim_start_matches('\u{feff}').replace("\r\n", "\n"))
        }
        (None, None) => Ok(String::new()),
    }
}

fn issue_line(i: &Issue) -> String {
    format!(
        "{:<10} {:<12} P{} {:<6} {}",
        i.id, i.status, i.priority, i.kind, i.title
    )
}

fn list(ctx: &Ctx, a: &ListArgs) -> Result<Rendered, AppError> {
    let filter = IssueFilter {
        status: if a.ready { Some(Status::Ready) } else { a.status },
        kind: a.kind,
        label: a.label.clone(),
        parent: None,
    };
    let issues = ctx.store.list_issues(&filter)?;
    let text = issues.iter().map(issue_line).collect::<Vec<_>>().join("\n");
    Rendered::new(&issues, text)
}

#[derive(Serialize)]
struct DepView {
    id: IssueId,
    title: String,
    status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    handoffs: Option<Vec<Comment>>,
}

#[derive(Serialize)]
struct ShowView {
    issue: Issue,
    deps: Vec<DepView>,
    dependents: Vec<DepView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    claim: Option<Claim>,
    #[serde(skip_serializing_if = "Option::is_none")]
    comments: Option<Vec<Comment>>,
}

fn require(ctx: &Ctx, id: &IssueId) -> Result<Issue, AppError> {
    ctx.store
        .get_issue(id)?
        .ok_or_else(|| AppError::not_found(format!("issue {id}")))
}

fn show(ctx: &Ctx, id: &IssueId, dossier: bool) -> Result<Rendered, AppError> {
    let issue = require(ctx, id)?;
    let deps = ctx.store.deps()?;
    let mut view_of = |other: &IssueId, with_handoffs: bool| -> Result<DepView, AppError> {
        let o = require(ctx, other)?;
        let handoffs = if with_handoffs {
            Some(
                ctx.store
                    .comments(other)?
                    .into_iter()
                    .filter(|c| {
                        c.kind == CommentType::Handoff && (c.to_lane == id.as_str() || c.to_lane == LANE_ALL)
                    })
                    .collect(),
            )
        } else {
            None
        };
        Ok(DepView {
            id: o.id,
            title: o.title,
            status: o.status,
            handoffs,
        })
    };
    let mut dep_views = Vec::new();
    for d in deps.iter().filter(|d| &d.issue_id == id) {
        dep_views.push(view_of(&d.depends_on, dossier)?);
    }
    let mut dependents = Vec::new();
    for d in deps.iter().filter(|d| &d.depends_on == id) {
        dependents.push(view_of(&d.issue_id, false)?);
    }
    let claim = ctx.store.active_claims()?.into_iter().find(|c| &c.issue_id == id);
    let comments = if dossier {
        Some(ctx.store.comments(id)?)
    } else {
        None
    };
    let view = ShowView {
        issue,
        deps: dep_views,
        dependents,
        claim,
        comments,
    };

    let mut text = format!(
        "{}\n{}\nstatus: {}  type: {}  priority: {}  estimate: {}\nscope: {}\n",
        view.issue.id,
        view.issue.title,
        view.issue.status,
        view.issue.kind,
        view.issue.priority,
        view.issue.estimate.as_deref().unwrap_or("-"),
        if view.issue.file_scope.is_empty() {
            "-".to_string()
        } else {
            view.issue.file_scope.join(" ")
        },
    );
    if !view.issue.body.is_empty() {
        text.push_str(&format!("\n{}\n", view.issue.body.trim_end()));
    }
    if !view.issue.acceptance.is_empty() {
        text.push_str("\nacceptance:\n");
        for a in &view.issue.acceptance {
            text.push_str(&format!(
                "  - {}{}\n",
                a.text,
                a.verify.as_ref().map(|v| format!(" [{v}]")).unwrap_or_default()
            ));
        }
    }
    if !view.deps.is_empty() {
        text.push_str("\ndepends on:\n");
        for d in &view.deps {
            text.push_str(&format!("  {} [{}] {}\n", d.id, d.status, d.title));
            for h in d.handoffs.iter().flatten() {
                text.push_str(&format!("    HANDOFF from {}: {}\n", h.author, h.body.trim_end()));
            }
        }
    }
    if let Some(c) = &view.claim {
        text.push_str(&format!("\nclaimed by {} at {}\n", c.worker, c.claimed_at));
    }
    if let Some(cs) = &view.comments {
        text.push_str(&format!("\ncomments ({}):\n", cs.len()));
        for c in cs {
            text.push_str(&format!(
                "  #{} **[{}] {}** -> to: {}\n    {}\n",
                c.id,
                c.author,
                c.kind,
                c.to_lane,
                c.body.trim_end()
            ));
        }
    }
    Rendered::new(&view, text.trim_end())
}

fn create(ctx: &Ctx, a: &CreateArgs) -> Result<Rendered, AppError> {
    if a.ready && !a.deps.is_empty() {
        return Err(AppError::validation(
            "--ready cannot be combined with --dep; use `asf issue ready` once deps are done",
        ));
    }
    let n = NewIssue {
        title: a.title.clone(),
        body: read_body(a.body.as_deref(), a.body_file.as_deref())?,
        kind: a.kind,
        status: if a.ready { Status::Ready } else { Status::Backlog },
        priority: a.priority,
        parent_id: a.parent.clone(),
        file_scope: a.file_scope.clone(),
        acceptance: a
            .acceptance
            .iter()
            .map(|t| Acceptance {
                text: t.clone(),
                verify: None,
            })
            .collect(),
        estimate: a.estimate.clone(),
        labels: a.labels.clone(),
        source: a.source.clone(),
        deps: a.deps.clone(),
    };
    let issue = ctx.store.create_issue(&n)?;
    let text = issue.id.to_string();
    Rendered::new(&issue, text)
}

fn edit(ctx: &Ctx, a: &EditArgs) -> Result<Rendered, AppError> {
    let patch = IssuePatch {
        title: a.title.clone(),
        body: a.body.clone(),
        priority: a.priority,
        file_scope: (!a.file_scope.is_empty()).then(|| a.file_scope.clone()),
        acceptance: (!a.acceptance.is_empty()).then(|| {
            a.acceptance
                .iter()
                .map(|t| Acceptance {
                    text: t.clone(),
                    verify: None,
                })
                .collect()
        }),
        estimate: if a.clear_estimate {
            Some(None)
        } else {
            a.estimate.clone().map(Some)
        },
        labels: (!a.labels.is_empty()).then(|| a.labels.clone()),
        parent_id: if a.clear_parent {
            Some(None)
        } else {
            a.parent.clone().map(Some)
        },
    };
    let issue = ctx.store.update_issue(&a.id, &patch)?;
    let text = issue_line(&issue);
    Rendered::new(&issue, text)
}

fn ready(ctx: &Ctx) -> Result<Rendered, AppError> {
    let promoted = ctx.store.promote_ready()?;
    let text = if promoted.is_empty() {
        "nothing to promote".to_string()
    } else {
        format!(
            "promoted: {}",
            promoted
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" ")
        )
    };
    Rendered::new(&promoted, text)
}

fn set_status(ctx: &Ctx, id: &IssueId, status: Status) -> Result<Rendered, AppError> {
    let issue = ctx.store.set_status(id, status, &ctx.author())?;
    let text = issue_line(&issue);
    Rendered::new(&issue, text)
}

fn dep(ctx: &Ctx, cmd: &DepCmd) -> Result<Rendered, AppError> {
    match cmd {
        DepCmd::Add { id, on } => {
            ctx.store.add_dep(id, on)?;
            Rendered::new(
                &Dep {
                    issue_id: id.clone(),
                    depends_on: on.clone(),
                },
                format!("{id} depends on {on}"),
            )
        }
        DepCmd::Rm { id, on } => {
            ctx.store.remove_dep(id, on)?;
            Rendered::new(
                &Dep {
                    issue_id: id.clone(),
                    depends_on: on.clone(),
                },
                format!("removed {id} -> {on}"),
            )
        }
    }
}

fn comment(ctx: &Ctx, a: &CommentArgs) -> Result<Rendered, AppError> {
    let body = read_body(a.body.as_deref(), a.body_file.as_deref())?;
    let c = ctx.store.add_comment(&NewComment {
        issue_id: a.id.clone(),
        author: ctx.author(),
        kind: a.kind,
        to_lane: a.to.trim().to_string(),
        body,
        reply_to: a.reply_to,
    })?;
    let text = format!(
        "#{} {} {} -> to: {} on {}",
        c.id, c.author, c.kind, c.to_lane, c.issue_id
    );
    Rendered::new(&c, text)
}
