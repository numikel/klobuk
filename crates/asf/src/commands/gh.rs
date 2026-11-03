//! `asf gh sync [--dry-run]`: mirror local issues to GitHub Issues (one way).
//!
//! Every mirrored GitHub issue carries `<!-- asf:<id> uid:<uid> -->` in its body, and the local
//! issue gets the label `gh:#<number>`. Local `done`/`cancelled` close the GitHub issue
//! (`completed` / `not planned`). Nothing flows back yet.

use std::collections::BTreeMap;

use asf_core::github::{parse_marker, sync_marker, GhIssue};
use asf_core::*;
use serde::Serialize;

use crate::cli::{Cli, GhCmd};
use crate::ctx::Ctx;
use crate::output::{AppError, ErrorCode, Rendered};

#[derive(Serialize)]
pub struct Mirrored {
    pub id: IssueId,
    pub number: u64,
    pub url: String,
}

#[derive(Serialize)]
pub struct SyncReport {
    pub dry_run: bool,
    pub created: Vec<Mirrored>,
    pub closed: Vec<Mirrored>,
    pub unchanged: usize,
    /// Local terminal issues that never reached GitHub (not created retroactively).
    pub skipped: Vec<IssueId>,
}

pub fn run(cli: &Cli, cmd: &GhCmd) -> Result<Rendered, AppError> {
    match cmd {
        GhCmd::Sync { dry_run } => sync(cli, *dry_run),
    }
}

fn gh_label(number: u64) -> String {
    format!("gh:#{number}")
}

fn issue_body(i: &Issue) -> String {
    let mut b = String::new();
    if !i.body.trim().is_empty() {
        b.push_str(i.body.trim_end());
        b.push_str("\n\n");
    }
    if !i.acceptance.is_empty() {
        b.push_str("### Acceptance\n");
        for a in &i.acceptance {
            b.push_str(&format!("- [ ] {}\n", a.text));
        }
        b.push('\n');
    }
    if !i.file_scope.is_empty() {
        b.push_str(&format!("**Scope:** `{}`\n\n", i.file_scope.join("`, `")));
    }
    b.push_str(&format!(
        "_{} · {} · priority P{}_\n\n{}",
        i.id,
        i.kind,
        i.priority,
        sync_marker(i.id.as_str(), &i.uid)
    ));
    b
}

fn sync(cli: &Cli, dry_run: bool) -> Result<Rendered, AppError> {
    let ctx = Ctx::load(cli)?;
    let cfg = &ctx.config.merge.github;
    let gh = asf_vcs::github_from_config(&cfg.runner, &ctx.root)
        .map_err(|e| AppError::new(ErrorCode::Backend, e.to_string()))?;
    let remote: BTreeMap<String, GhIssue> = gh
        .issue_list(&ctx.root)
        .map_err(|e| AppError::new(ErrorCode::Backend, e.to_string()))?
        .into_iter()
        .filter_map(|g| parse_marker(&g.body).map(|(id, _)| (id, g)))
        .collect();
    let local = ctx.store.list_issues(&IssueFilter::default())?;

    let mut report = SyncReport {
        dry_run,
        created: Vec::new(),
        closed: Vec::new(),
        unchanged: 0,
        skipped: Vec::new(),
    };
    for i in &local {
        match remote.get(i.id.as_str()) {
            None if i.status.is_terminal() => report.skipped.push(i.id.clone()),
            None => {
                let title = format!("[{}] {}", i.id, i.title);
                let mirrored = if dry_run {
                    Mirrored {
                        id: i.id.clone(),
                        number: 0,
                        url: String::new(),
                    }
                } else {
                    let g = gh
                        .issue_create(&ctx.root, &title, &issue_body(i))
                        .map_err(|e| AppError::new(ErrorCode::Backend, e.to_string()))?;
                    ensure_label(&ctx, i, g.number)?;
                    Mirrored {
                        id: i.id.clone(),
                        number: g.number,
                        url: g.url,
                    }
                };
                report.created.push(mirrored);
            }
            Some(g) => {
                if i.status.is_terminal() && g.state.eq_ignore_ascii_case("open") {
                    if !dry_run {
                        let reason = if i.status == Status::Done {
                            "completed"
                        } else {
                            "not planned"
                        };
                        gh.issue_close(&ctx.root, g.number, reason)
                            .map_err(|e| AppError::new(ErrorCode::Backend, e.to_string()))?;
                    }
                    report.closed.push(Mirrored {
                        id: i.id.clone(),
                        number: g.number,
                        url: g.url.clone(),
                    });
                } else {
                    report.unchanged += 1;
                }
                if !dry_run {
                    ensure_label(&ctx, i, g.number)?;
                }
            }
        }
    }
    let text = format!(
        "gh sync{}: created {} · closed {} · unchanged {} · skipped {}{}{}",
        if dry_run { " (dry run)" } else { "" },
        report.created.len(),
        report.closed.len(),
        report.unchanged,
        report.skipped.len(),
        report
            .created
            .iter()
            .map(|m| format!("\ncreated {} -> #{} {}", m.id, m.number, m.url))
            .collect::<String>(),
        report
            .closed
            .iter()
            .map(|m| format!("\nclosed  {} -> #{}", m.id, m.number))
            .collect::<String>(),
    );
    Rendered::new(&report, text)
}

/// Adds `gh:#<n>` to the local labels when missing.
fn ensure_label(ctx: &Ctx, i: &Issue, number: u64) -> Result<(), AppError> {
    let label = gh_label(number);
    if i.labels.iter().any(|l| l == &label) {
        return Ok(());
    }
    let mut labels: Vec<String> = i
        .labels
        .iter()
        .filter(|l| !l.starts_with("gh:#"))
        .cloned()
        .collect();
    labels.push(label);
    ctx.store.update_issue(
        &i.id,
        &IssuePatch {
            labels: Some(labels),
            ..IssuePatch::default()
        },
    )?;
    Ok(())
}
