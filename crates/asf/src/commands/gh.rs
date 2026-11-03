//! `asf gh sync [--dry-run]`: mirror local issues to GitHub Issues (one way).
//!
//! Every mirrored GitHub issue carries `<!-- asf:<id> uid:<uid> -->` in its body, and the local
//! issue gets the label `gh:#<number>`. Local `done`/`cancelled` close the GitHub issue
//! (`completed` / `not planned`). Nothing flows back yet.

use std::collections::BTreeMap;

use asf_core::github::{parse_marker, strip_markers, sync_marker, GhIssue};
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
    /// Extra GitHub issues carrying a valid marker for an issue that already has a mirror.
    pub duplicates: Vec<Duplicate>,
}

#[derive(Serialize)]
pub struct Duplicate {
    pub id: IssueId,
    pub used: u64,
    pub ignored: Vec<u64>,
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
        b.push_str(&strip_markers(i.body.trim_end()));
        b.push_str("\n\n");
    }
    if !i.acceptance.is_empty() {
        b.push_str("### Acceptance\n");
        for a in &i.acceptance {
            b.push_str(&format!("- [ ] {}\n", strip_markers(&a.text)));
        }
        b.push('\n');
    }
    if !i.file_scope.is_empty() {
        b.push_str(&format!(
            "**Scope:** `{}`\n\n",
            strip_markers(&i.file_scope.join("`, `"))
        ));
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
    let local = ctx.store.list_issues(&IssueFilter::default())?;
    // A remote issue mirrors a local one only when both id and uid of its marker match; anything
    // else (a forged or stale marker) is ignored. Markers are public, so several candidates can
    // still claim one issue: the number recorded locally as `gh:#<n>` wins (the tool wrote it),
    // then the lowest number; the rest is reported, never fatal.
    let mut candidates: BTreeMap<&str, Vec<GhIssue>> = BTreeMap::new();
    for g in gh
        .issue_list(&ctx.root)
        .map_err(|e| AppError::new(ErrorCode::Backend, e.to_string()))?
    {
        let Some((id, uid)) = parse_marker(&g.body) else {
            continue;
        };
        if let Some(i) = local.iter().find(|i| i.id.as_str() == id && i.uid == uid) {
            candidates.entry(i.id.as_str()).or_default().push(g);
        }
    }
    let mut remote: BTreeMap<&str, GhIssue> = BTreeMap::new();
    let mut duplicates: Vec<Duplicate> = Vec::new();
    for i in &local {
        let Some(mut found) = candidates.remove(i.id.as_str()) else {
            continue;
        };
        found.sort_by_key(|g| g.number);
        let recorded = i
            .labels
            .iter()
            .find_map(|l| l.strip_prefix("gh:#").and_then(|n| n.parse::<u64>().ok()));
        let pick = recorded
            .and_then(|n| found.iter().position(|g| g.number == n))
            .unwrap_or(0);
        let chosen = found.remove(pick);
        if !found.is_empty() {
            duplicates.push(Duplicate {
                id: i.id.clone(),
                used: chosen.number,
                ignored: found.iter().map(|g| g.number).collect(),
            });
        }
        remote.insert(i.id.as_str(), chosen);
    }

    let mut report = SyncReport {
        dry_run,
        created: Vec::new(),
        closed: Vec::new(),
        unchanged: 0,
        skipped: Vec::new(),
        duplicates,
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
    let text = report.duplicates.iter().fold(text, |mut t, d| {
        t.push_str(&format!(
            "\nduplicate mirrors of {}: using #{}, ignoring {}",
            d.id,
            d.used,
            d.ignored
                .iter()
                .map(|n| format!("#{n}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        t
    });
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
