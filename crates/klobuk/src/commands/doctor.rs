//! `klobuk repo doctor` — environment and store health.

use std::collections::HashSet;
use std::fs;
use std::process::Command;

use klobuk_core::IssueStore;
use klobuk_store::serve_pulse;
use serde::Serialize;

use super::repo::IGNORE_LINES;
use crate::cli::Cli;
use crate::ctx::{Ctx, CONFIG_FILE, FACTORY_DIR};
use crate::git;
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

/// Env var that replaces the `gh` binary the github check runs (tests point it at a stand-in).
const GH_BIN_ENV: &str = "KLOBUK_GH_BIN";

/// First non-empty line of a command's output, stdout before stderr (older `gh` printed to stderr).
fn first_line(out: &std::process::Output) -> String {
    [&out.stdout, &out.stderr]
        .iter()
        .flat_map(|b| {
            String::from_utf8_lossy(b)
                .lines()
                .map(str::trim)
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .find(|l| !l.is_empty())
        .unwrap_or_default()
}

/// `[merge] target = "github"` needs an authenticated `gh` and a resolvable remote; with the
/// `local` target the check is only informational (`required: false`) and runs nothing.
fn github_check(ctx: &Ctx) -> Check {
    if ctx.config.merge.target != "github" {
        return Check {
            name: "github",
            ok: false,
            required: false,
            detail: format!("skipped: [merge] target is {}", ctx.config.merge.target),
        };
    }
    let gh_bin = std::env::var(GH_BIN_ENV).unwrap_or_else(|_| "gh".into());
    let remote = &ctx.config.merge.github.remote;
    let auth = match Command::new(&gh_bin).args(["auth", "status"]).output() {
        Ok(out) if out.status.success() => Ok(first_line(&out)),
        Ok(out) => {
            let line = first_line(&out);
            Err(format!(
                "`{gh_bin} auth status` failed ({}){}",
                out.status,
                if line.is_empty() {
                    String::new()
                } else {
                    format!(": {line}")
                }
            ))
        }
        Err(_) => Err(format!("{gh_bin} not found on PATH")),
    };
    let url = match Command::new("git")
        .args(["remote", "get-url", remote])
        .current_dir(&ctx.root)
        .output()
    {
        Ok(out) if out.status.success() => Ok(String::from_utf8_lossy(&out.stdout).trim().to_string()),
        Ok(out) => {
            let line = first_line(&out);
            Err(format!("`git remote get-url {remote}` failed: {line}"))
        }
        Err(e) => Err(format!("git remote get-url {remote}: {e}")),
    };
    let ok = auth.is_ok() && url.is_ok();
    let part = |label: &str, r: Result<String, String>| match r {
        Ok(v) => format!("{label}: {v}"),
        Err(e) => e,
    };
    Check {
        name: "github",
        ok,
        required: true,
        detail: format!("{}; {}", part("gh", auth), part(remote, url)),
    }
}

/// Every `IGNORE_LINES` entry must be ignored through `.gitignore` or `.git/info/exclude` (either
/// one is enough, as the lines are effective from both). Advisory only: a gap is a `warn`
/// (`required: false`), never a failure, because the repository still works without the lines.
fn gitignore_check(ctx: &Ctx) -> Check {
    let sources = [
        ctx.root.join(".gitignore"),
        git::common_dir(&ctx.root)
            .map(|d| d.join("info").join("exclude"))
            .unwrap_or_else(|_| ctx.root.join(".git").join("info").join("exclude")),
    ];
    // An unreadable or missing file counts as empty; if neither exists every line is missing.
    let contents: Vec<Option<String>> = sources.iter().map(|p| fs::read_to_string(p).ok()).collect();
    let present: HashSet<&str> = contents
        .iter()
        .flatten()
        .flat_map(|s| s.lines().map(str::trim))
        .collect();
    let missing: Vec<&str> = IGNORE_LINES
        .iter()
        .copied()
        .filter(|l| !present.contains(l))
        .collect();
    let detail = if missing.is_empty() {
        "all klobuk paths are ignored".to_string()
    } else {
        let none_exist = contents.iter().all(Option::is_none);
        format!(
            "{}missing: {}; run `klobuk repo init` to add them",
            if none_exist {
                "neither .gitignore nor .git/info/exclude exists; "
            } else {
                ""
            },
            missing.join(", ")
        )
    };
    Check {
        name: "gitignore",
        ok: missing.is_empty(),
        required: false,
        detail,
    }
}

/// The state of the `klobuk serve` loop, worded like the TUI banner. Advisory (`required: false`): a repo
/// is healthy without a loop, it just does nothing on its own. Unlike the TUI this also looks the
/// pid up, so a loop killed a moment ago is reported at once.
fn loop_check(ctx: &Ctx) -> Check {
    let pulse = match serve_pulse::read(&ctx.root) {
        Ok(p) => p,
        Err(e) => {
            return Check {
                name: "loop",
                ok: false,
                required: false,
                detail: format!("the pulse is unreadable ({e}); the next `klobuk serve` replaces it"),
            }
        }
    };
    let alive = pulse
        .as_ref()
        .filter(|p| p.stopped_at.is_none())
        .and_then(|p| serve_pulse::pid_alive(p.pid));
    let state = serve_pulse::judge(pulse.as_ref(), time::OffsetDateTime::now_utc(), alive);
    let mode = pulse
        .as_ref()
        .map_or(serve_pulse::ServeMode::Dispatch, |p| p.mode);
    let detail = match (&state, &pulse) {
        (serve_pulse::LoopState::Running { age_s, .. }, Some(p)) => format!(
            "running ({}), pid {}, last tick {} ago, {} tick(s)",
            p.mode.as_str(),
            p.pid,
            serve_pulse::fmt_age(*age_s),
            p.ticks
        ),
        _ => state.banner(mode, None).unwrap_or_default(),
    };
    Check {
        name: "loop",
        ok: state.is_running(),
        required: false,
        detail,
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
            ok: v == klobuk_store::SCHEMA_VERSION,
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
    checks.push(github_check(&ctx));
    checks.push(gitignore_check(&ctx));
    checks.push(loop_check(&ctx));
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
