//! `asf repo doctor` — environment and store health.

use std::process::Command;

use asf_core::IssueStore;
use serde::Serialize;

use crate::cli::Cli;
use crate::ctx::{Ctx, CONFIG_FILE, FACTORY_DIR};
use crate::output::{AppError, Rendered};

#[derive(Serialize)]
struct Check {
    name: &'static str,
    ok: bool,
    required: bool,
    detail: String,
}

#[derive(Serialize)]
struct Report {
    ok: bool,
    checks: Vec<Check>,
}

fn version_of(bin: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(bin).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    Some(s.lines().next().unwrap_or("").trim().to_string())
}

fn tool(name: &'static str, bin: &str, args: &[&str], required: bool) -> Check {
    match version_of(bin, args) {
        Some(v) => Check {
            name,
            ok: true,
            required,
            detail: v,
        },
        None => Check {
            name,
            ok: false,
            required,
            detail: format!("{bin} not found on PATH"),
        },
    }
}

pub fn run(cli: &Cli) -> Result<Rendered, AppError> {
    let ctx = Ctx::load(cli)?;
    let mut checks = vec![tool("git", "git", &["--version"], true)];
    checks.push(Check {
        name: "repo_root",
        ok: true,
        required: true,
        detail: ctx.root.to_string_lossy().replace('\\', "/"),
    });
    checks.push(Check {
        name: "config",
        ok: true,
        required: true,
        detail: format!(
            "{FACTORY_DIR}/{CONFIG_FILE}: prefix {}, max_parallel {}",
            ctx.config.repo.prefix, ctx.config.scheduler.max_parallel
        ),
    });
    let db = match (ctx.store.schema_version(), ctx.store.repo()) {
        (Ok(v), Ok(repo)) => Check {
            name: "database",
            ok: v == 1,
            required: true,
            detail: format!(
                "schema {v}, prefix {}, next_seq {}",
                repo.id_prefix, repo.next_seq
            ),
        },
        (Err(e), _) | (_, Err(e)) => Check {
            name: "database",
            ok: false,
            required: true,
            detail: e.to_string(),
        },
    };
    checks.push(db);
    checks.push(tool("herdr", "herdr", &["--version"], false));
    checks.push(tool("claude", "claude", &["--version"], false));
    let ok = checks.iter().filter(|c| c.required).all(|c| c.ok);
    let report = Report { ok, checks };
    let mut text = String::new();
    for c in &report.checks {
        let mark = if c.ok {
            "ok  "
        } else if c.required {
            "FAIL"
        } else {
            "warn"
        };
        text.push_str(&format!("{mark} {:<10} {}\n", c.name, c.detail));
    }
    text.push_str(if report.ok { "doctor: OK" } else { "doctor: FAILED" });
    let rendered = Rendered::new(&report, text)?;
    if !report.ok {
        // Human and JSON output still carry the full report; the exit code signals failure.
        return Err(AppError::new(
            crate::output::ErrorCode::Validation,
            format!(
                "doctor failed: {}",
                report
                    .checks
                    .iter()
                    .filter(|c| c.required && !c.ok)
                    .map(|c| c.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
    Ok(rendered)
}
