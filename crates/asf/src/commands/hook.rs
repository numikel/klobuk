//! `asf hook pre-tool|post-tool|stop|status` — Claude Code hooks run inside a worker session.
//!
//! Contract (verified live, see memory `asf-herdr-cli-facts`): JSON on stdin, decisions as JSON on
//! stdout with exit 0. A hook that cannot find its run context, or hits an internal error, prints
//! nothing on stdout and exits 0: the factory must never wedge a session by accident.

use std::io::Read;
use std::path::{Path, PathBuf};

use asf_core::*;
use globset::{Glob, GlobSetBuilder};
use serde_json::{json, Value};

use crate::cli::{Cli, HookCmd};
use crate::ctx::{Ctx, ENV_ISSUE};
use crate::output::{AppError, Rendered};

pub const ENV_RUN: &str = "ASF_RUN";

pub fn run(cli: &Cli, cmd: &HookCmd) -> Result<Rendered, AppError> {
    match inner(cli, cmd) {
        Ok(text) => Rendered::new(&Value::Null, text),
        Err(e) => {
            eprintln!("asf hook: {} (ignored)", e.message);
            Rendered::new(&Value::Null, "")
        }
    }
}

fn read_stdin() -> Result<Value, AppError> {
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw)?;
    let raw = raw.trim_start_matches('\u{feff}');
    if raw.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(raw).map_err(|e| AppError::validation(format!("hook stdin is not JSON: {e}")))
}

/// The run this hook acts for: `ASF_RUN`, else the latest active run of `ASF_ISSUE`.
fn locate_run(ctx: &Ctx) -> Result<Option<Run>, AppError> {
    if let Ok(id) = std::env::var(ENV_RUN) {
        if !id.trim().is_empty() {
            return Ok(ctx.store.get_run(id.trim())?);
        }
    }
    if let Ok(issue) = std::env::var(ENV_ISSUE) {
        if let Ok(id) = issue.trim().parse::<IssueId>() {
            return Ok(ctx.store.runs(Some(&id), true)?.into_iter().last());
        }
    }
    Ok(None)
}

fn inner(cli: &Cli, cmd: &HookCmd) -> Result<String, AppError> {
    let input = read_stdin()?;
    let ctx = Ctx::load(cli)?;
    let Some(run) = locate_run(&ctx)? else {
        eprintln!("asf hook: no run context (ASF_RUN / ASF_ISSUE); nothing to do");
        return Ok(String::new());
    };
    match cmd {
        HookCmd::PreTool => pre_tool(&ctx, &run, &input),
        HookCmd::PostTool => post_tool(&ctx, &run, &input),
        HookCmd::Stop => stop(&ctx, &run, &input),
        HookCmd::Status => status(&ctx, &run, &input),
    }
}

fn deny(reason: &str) -> String {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason
        }
    })
    .to_string()
}

fn block(reason: &str) -> String {
    json!({ "decision": "block", "reason": reason }).to_string()
}

fn canonical(p: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(p) {
        return c;
    }
    // the file may not exist yet: canonicalize the nearest existing ancestor
    let mut parts = Vec::new();
    let mut cur = p.to_path_buf();
    while !cur.exists() {
        match (cur.file_name(), cur.parent()) {
            (Some(name), Some(parent)) => {
                parts.push(name.to_os_string());
                cur = parent.to_path_buf();
            }
            _ => return p.to_path_buf(),
        }
    }
    let mut out = std::fs::canonicalize(&cur).unwrap_or(cur);
    for part in parts.iter().rev() {
        out.push(part);
    }
    out
}

/// Path of `file` relative to the worktree, forward slashes; `None` when outside.
fn relative_to_worktree(root: &Path, worktree_rel: &str, file: &Path) -> Option<String> {
    let wt = canonical(&root.join(worktree_rel));
    let f = canonical(file);
    f.strip_prefix(&wt)
        .ok()
        .map(|r| r.to_string_lossy().replace('\\', "/"))
}

fn pre_tool(ctx: &Ctx, run: &Run, input: &Value) -> Result<String, AppError> {
    let tool = input.get("tool_name").and_then(Value::as_str).unwrap_or("");
    if !matches!(tool, "Edit" | "Write" | "MultiEdit" | "NotebookEdit") {
        return Ok(String::new());
    }
    let Some(file) = input.pointer("/tool_input/file_path").and_then(Value::as_str) else {
        return Ok(String::new());
    };
    if matches!(run.state, RunState::OverBudget | RunState::Aborted) {
        return Ok(deny(&format!(
            "asf: run {} is {} (budget exhausted or aborted); stop and report via `asf issue comment`",
            run.id, run.state
        )));
    }
    let issue = ctx
        .store
        .get_issue(&run.issue_id)?
        .ok_or_else(|| AppError::not_found("issue of run"))?;
    let Some(rel) = relative_to_worktree(&ctx.root, &run.worktree, Path::new(file)) else {
        let reason = format!(
            "asf: `{file}` is outside the worktree `{}`; only edit files inside it",
            run.worktree
        );
        ctx.store.append_event(&NewEvent {
            kind: "hook.denied".into(),
            issue_id: Some(run.issue_id.clone()),
            run_id: Some(run.id.clone()),
            actor: Some("hook".into()),
            payload: json!({ "tool": tool, "file": file, "reason": "outside_worktree" }),
        })?;
        return Ok(deny(&reason));
    };
    if issue.file_scope.is_empty() {
        return Ok(String::new());
    }
    let mut b = GlobSetBuilder::new();
    for g in &issue.file_scope {
        b.add(Glob::new(g).map_err(|e| AppError::validation(format!("bad scope glob `{g}`: {e}")))?);
    }
    let set = b.build().map_err(|e| AppError::validation(e.to_string()))?;
    if set.is_match(&rel) {
        return Ok(String::new());
    }
    let reason = format!(
        "asf: `{rel}` is outside the file scope of {} ({}). Edit only files matching the scope; if the scope is \
         wrong, post `asf issue comment {} --type BLOCKER --to human --body \"...\"` and stop.",
        issue.id,
        issue.file_scope.join(", "),
        issue.id
    );
    ctx.store.append_event(&NewEvent {
        kind: "hook.denied".into(),
        issue_id: Some(run.issue_id.clone()),
        run_id: Some(run.id.clone()),
        actor: Some("hook".into()),
        payload: json!({ "tool": tool, "file": rel, "reason": "outside_scope" }),
    })?;
    Ok(deny(&reason))
}

fn blocker(ctx: &Ctx, run: &Run, body: String) -> Result<(), AppError> {
    ctx.store.add_comment(&NewComment {
        issue_id: run.issue_id.clone(),
        author: LANE_ORCHESTRATOR.to_string(),
        kind: CommentType::Blocker,
        to_lane: LANE_HUMAN.to_string(),
        body,
        reply_to: None,
    })?;
    Ok(())
}

fn post_tool(ctx: &Ctx, run: &Run, input: &Value) -> Result<String, AppError> {
    let session = input
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let transcript = input
        .get("transcript_path")
        .and_then(Value::as_str)
        .map(str::to_string);
    let tool_calls = run.tool_calls + 1;
    let mut patch = RunPatch {
        tool_calls: Some(tool_calls),
        last_seen_at: Some(now_rfc3339()),
        claude_session_id: run.claude_session_id.clone().or(session),
        transcript_path: run.transcript_path.clone().or(transcript),
        ..RunPatch::default()
    };
    let cap = ctx.config.worker.max_tool_calls as i64;
    let trip = cap > 0 && tool_calls > cap && run.state != RunState::OverBudget;
    if trip {
        patch.state = Some(RunState::OverBudget);
        patch.exit_reason = Some(format!("max_tool_calls {cap} exceeded"));
    }
    ctx.store.update_run(&run.id, &patch)?;
    if trip {
        let msg = format!(
            "Run {} for {} exceeded max_tool_calls ({cap}). Worker is now denied further edits; review the pane and \
             either `asf run reply {}` with guidance or `asf run abort {}`.",
            run.id, run.issue_id, run.issue_id, run.issue_id
        );
        blocker(ctx, run, msg)?;
        return Ok(block(&format!(
            "asf: tool-call cap ({cap}) reached for {}. Stop working, write a short STATUS comment with \
             `asf issue comment {} --type STATUS --to human --body \"...\"` and wait for the human.",
            run.issue_id, run.issue_id
        )));
    }
    Ok(String::new())
}

fn stop(ctx: &Ctx, run: &Run, input: &Value) -> Result<String, AppError> {
    let active = input
        .get("stop_hook_active")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let last = input
        .get("last_assistant_message")
        .and_then(Value::as_str)
        .unwrap_or("");
    ctx.store.append_event(&NewEvent {
        kind: "run.stop".into(),
        issue_id: Some(run.issue_id.clone()),
        run_id: Some(run.id.clone()),
        actor: Some("hook".into()),
        payload: json!({ "stop_hook_active": active, "last_assistant_message": last }),
    })?;
    ctx.store.update_run(
        &run.id,
        &RunPatch {
            last_seen_at: Some(now_rfc3339()),
            ..RunPatch::default()
        },
    )?;
    let issue = ctx
        .store
        .get_issue(&run.issue_id)?
        .ok_or_else(|| AppError::not_found("issue of run"))?;
    if issue.status == Status::InReview
        || active
        || matches!(run.state, RunState::OverBudget | RunState::Aborted)
    {
        return Ok(String::new());
    }
    let report = format!(".factory/runs/{}/report.md", run.issue_id);
    Ok(block(&format!(
        "asf: before stopping, finish the ritual for {id}: make sure everything is committed (`git status` clean), \
         write the report to `{report}` (what changed, how verified, open questions) and run \
         `asf issue finish {id} --report {report}`. If you are blocked instead, post \
         `asf issue comment {id} --type BLOCKER --to human --body \"...\"` and then stop.",
        id = run.issue_id
    )))
}

fn status(ctx: &Ctx, run: &Run, input: &Value) -> Result<String, AppError> {
    let cost = input.pointer("/cost/total_cost_usd").and_then(Value::as_f64);
    let inp = input
        .pointer("/context_window/total_input_tokens")
        .and_then(Value::as_i64);
    let outp = input
        .pointer("/context_window/total_output_tokens")
        .and_then(Value::as_i64);
    let mut patch = RunPatch {
        cost_usd: cost,
        input_tokens: inp,
        output_tokens: outp,
        last_seen_at: Some(now_rfc3339()),
        ..RunPatch::default()
    };
    let cost_now = cost.unwrap_or(run.cost_usd);
    let over = run.budget_usd.map(|b| cost_now > b).unwrap_or(false);
    let trip = over && run.state != RunState::OverBudget;
    if trip {
        patch.state = Some(RunState::OverBudget);
        patch.exit_reason = Some(format!(
            "budget ${:.2} exceeded (${cost_now:.2})",
            run.budget_usd.unwrap_or(0.0)
        ));
    }
    ctx.store.update_run(&run.id, &patch)?;
    if trip {
        blocker(
            ctx,
            run,
            format!(
                "Run {} for {} is over budget: ${cost_now:.2} > ${:.2}. Further edits are denied; `asf run reply {}` \
                 to continue with a raised budget or `asf run abort {}`.",
                run.id,
                run.issue_id,
                run.budget_usd.unwrap_or(0.0),
                run.issue_id,
                run.issue_id
            ),
        )?;
    }
    let flag = if over || run.state == RunState::OverBudget {
        " OVER BUDGET"
    } else {
        ""
    };
    Ok(format!(
        "asf {} ${cost_now:.2}/{} tool_calls {}{flag}",
        run.issue_id,
        run.budget_usd
            .map(|b| format!("${b:.2}"))
            .unwrap_or_else(|| "-".into()),
        run.tool_calls
    ))
}
