//! Worker orchestration: start a run in a worktree + runtime workspace, watch it, verify it, abort it.
//!
//! Completion is never inferred from the agent's screen state: a run is verified only after the
//! worker ran `asf issue finish` (issue `in-review`), and only the orchestrator decides `succeeded`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use asf_core::*;
use asf_vcs::{run_gates, GitCli};
use serde::Serialize;
use serde_json::json;

use crate::ctx::{Ctx, ENV_ISSUE, FACTORY_DIR};
use crate::output::{AppError, ErrorCode};

pub const RUNS_DIR: &str = "runs";
pub const HOOK_TIMEOUT_S: u64 = 30;
const TRUST_MARKERS: [&str; 2] = ["trust this folder", "Yes, I trust"];

pub struct Worker<'a> {
    pub ctx: &'a Ctx,
    pub rt: Box<dyn AgentRuntime>,
    pub vcs: GitCli,
}

#[derive(Serialize)]
pub struct StartResult {
    pub run: Run,
    pub workspace_id: String,
    pub pane_id: String,
    pub agent: AgentInfo,
    pub trusted: bool,
}

#[derive(Serialize)]
pub struct Transition {
    pub run_id: String,
    pub issue_id: IssueId,
    pub from: RunState,
    pub to: RunState,
    pub detail: String,
}

#[derive(Serialize)]
pub struct WatchResult {
    pub checked: usize,
    pub transitions: Vec<Transition>,
}

#[derive(Serialize)]
pub struct ShowResult {
    pub run: Run,
    pub agent: Option<AgentInfo>,
}

fn fwd(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    s.strip_prefix("//?/").map(str::to_string).unwrap_or(s)
}

fn rt_err(e: RuntimeError) -> AppError {
    let code = match e {
        RuntimeError::NotFound(_) => ErrorCode::NotFound,
        RuntimeError::Blocked(_) | RuntimeError::NotReady(_) | RuntimeError::Stalled(_) => ErrorCode::Blocked,
        RuntimeError::Timeout(_) => ErrorCode::Blocked,
        RuntimeError::Protocol(_) => ErrorCode::Backend,
        RuntimeError::Io(_) => ErrorCode::Io,
    };
    AppError::new(code, e.to_string())
}

/// Changed paths that no scope glob matches. An empty scope means "no restriction"; an invalid
/// glob is ignored (validated at issue creation).
pub(crate) fn files_outside_scope(changed: &[String], scope: &[String]) -> Vec<String> {
    if scope.is_empty() {
        return Vec::new();
    }
    let mut b = globset::GlobSetBuilder::new();
    for g in scope {
        if let Ok(gl) = globset::Glob::new(g) {
            b.add(gl);
        }
    }
    match b.build() {
        Ok(set) => changed.iter().filter(|f| !set.is_match(f)).cloned().collect(),
        Err(_) => Vec::new(),
    }
}

fn vcs_err(e: VcsError) -> AppError {
    match e {
        VcsError::NotFound(m) => AppError::not_found(m),
        VcsError::Dirty(_) | VcsError::Unmerged(_) => AppError::conflict(e.to_string()),
        VcsError::Git(m) => AppError::new(ErrorCode::Backend, m),
        VcsError::Io(m) => AppError::io(m),
    }
}

fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

impl<'a> Worker<'a> {
    pub fn new(ctx: &'a Ctx) -> Result<Self, AppError> {
        let rt = asf_runtime::from_config(&ctx.config.worker.runtime, &ctx.root).map_err(rt_err)?;
        Ok(Worker { ctx, rt, vcs: GitCli })
    }

    /// Brief, settings and the worker's report live inside the worktree (the worker's cwd), under
    /// the gitignored `.factory/runs/<id>/`.
    fn runs_dir(wt_abs: &Path, id: &IssueId) -> PathBuf {
        wt_abs.join(FACTORY_DIR).join(RUNS_DIR).join(id.as_str())
    }

    fn worktree_rel(&self, id: &IssueId) -> String {
        format!(
            "{}/{}",
            self.ctx.config.repo.worktrees_dir.trim_end_matches('/'),
            id
        )
    }

    fn handle(run: &Run) -> Result<AgentHandle, AppError> {
        Ok(AgentHandle {
            name: run.agent_name.clone().unwrap_or_else(|| run.issue_id.to_string()),
            pane_id: run
                .pane_id
                .clone()
                .ok_or_else(|| AppError::validation(format!("run {} has no pane", run.id)))?,
            workspace_id: run.workspace_id.clone().unwrap_or_default(),
        })
    }

    /// Latest run for an issue (active preferred).
    pub fn run_for(&self, id: &IssueId) -> Result<Run, AppError> {
        let store = &self.ctx.store;
        store
            .get_issue(id)?
            .ok_or_else(|| AppError::not_found(format!("issue {id}")))?;
        if let Some(r) = store.runs(Some(id), true)?.into_iter().last() {
            return Ok(r);
        }
        store
            .runs(Some(id), false)?
            .into_iter()
            .last()
            .ok_or_else(|| AppError::not_found(format!("no run for {id}")))
    }

    fn blocker(&self, run: &Run, body: String) -> Result<(), AppError> {
        self.ctx.store.add_comment(&NewComment {
            issue_id: run.issue_id.clone(),
            author: LANE_ORCHESTRATOR.to_string(),
            kind: CommentType::Blocker,
            to_lane: LANE_HUMAN.to_string(),
            body,
            reply_to: None,
        })?;
        Ok(())
    }

    fn set_state(&self, run: &Run, state: RunState, reason: Option<String>) -> Result<Run, AppError> {
        let finished = state.is_terminal().then(now_rfc3339);
        Ok(self.ctx.store.update_run(
            &run.id,
            &RunPatch {
                state: Some(state),
                exit_reason: reason,
                finished_at: finished,
                ..RunPatch::default()
            },
        )?)
    }

    fn write_brief_files(&self, issue: &Issue, run: &Run, wt_abs: &Path) -> Result<PathBuf, AppError> {
        let store = &self.ctx.store;
        let all_deps = store.deps()?;
        let mut deps: Vec<(Issue, Vec<Comment>)> = Vec::new();
        for d in all_deps.iter().filter(|d| d.issue_id == issue.id) {
            if let Some(dep) = store.get_issue(&d.depends_on)? {
                let handoffs: Vec<Comment> = store
                    .comments(&dep.id)?
                    .into_iter()
                    .filter(|c| {
                        c.kind == CommentType::Handoff
                            && (c.to_lane == issue.id.as_str() || c.to_lane == LANE_ALL)
                    })
                    .collect();
                deps.push((dep, handoffs));
            }
        }
        let mut blockers = Vec::new();
        for d in all_deps.iter().filter(|d| d.depends_on == issue.id) {
            if let Some(x) = store.get_issue(&d.issue_id)? {
                blockers.push(x);
            }
        }
        let comments = store.comments(&issue.id)?;
        let runs_dir = Self::runs_dir(wt_abs, &issue.id);
        std::fs::create_dir_all(&runs_dir)?;
        let asf_bin = crate::ctx::asf_bin();
        let report_rel = format!("{FACTORY_DIR}/{RUNS_DIR}/{}/report.md", issue.id);
        let brief = render_brief(&BriefInput {
            issue,
            deps: deps.iter().map(|(d, h)| (d, h.iter().collect())).collect(),
            comments: &comments,
            blockers: blockers.iter().collect(),
            worktree: &run.worktree,
            branch: &run.branch,
            report_path: &report_rel,
            budget_usd: run.budget_usd.unwrap_or(0.0),
            asf_bin: &asf_bin,
        });
        std::fs::write(runs_dir.join("brief.md"), brief)?;
        let w = &self.ctx.config.worker;
        let settings = render_settings(&SettingsInput {
            asf_bin: &asf_bin,
            allowed_tools: &w.allowed_tools,
            deny: &w.deny,
            scope: &issue.file_scope,
            hook_timeout_s: HOOK_TIMEOUT_S,
        });
        let settings_path = runs_dir.join("settings.json");
        std::fs::write(
            &settings_path,
            serde_json::to_string_pretty(&settings).map_err(|e| AppError::io(e.to_string()))?,
        )?;
        let brief_rel = format!("{FACTORY_DIR}/{RUNS_DIR}/{}/brief.md", issue.id);
        std::fs::write(
            wt_abs.join("CLAUDE.local.md"),
            render_claude_local(&issue.id, &brief_rel, &asf_bin),
        )?;
        Ok(settings_path)
    }

    pub fn start(&self, id: &IssueId, approve: Option<&str>) -> Result<StartResult, AppError> {
        let store = &self.ctx.store;
        let issue = store
            .get_issue(id)?
            .ok_or_else(|| AppError::not_found(format!("issue {id}")))?;
        if let Some(hash) = approve {
            let current = crate::commands::run::current_plan(self.ctx)?.inputs_hash;
            if hash != current {
                return Err(AppError::conflict(format!(
                    "plan changed since approval (now {current}); re-run `asf run plan`"
                )));
            }
        }
        let worktree_rel = self.worktree_rel(id);
        let branch = format!("feat/{id}");
        store.claim(
            id,
            &NewClaim {
                worker: id.to_string(),
                run_id: None,
                worktree: Some(worktree_rel.clone()),
                branch: Some(branch.clone()),
                note: Some("asf run start".into()),
            },
        )?;
        let release = |e: AppError| -> AppError {
            let _ = store.release(id);
            e
        };
        self.rt.health().map_err(rt_err).map_err(release)?;

        let wt_abs = self.ctx.root.join(&worktree_rel);
        let created_wt = !wt_abs.exists();
        let base = self
            .vcs
            .worktree_add(
                &self.ctx.root,
                &wt_abs,
                &branch,
                &self.ctx.config.repo.default_branch,
            )
            .map_err(vcs_err)
            .map_err(release)?;
        let rollback = |e: AppError| -> AppError {
            if created_wt {
                let _ = self.vcs.worktree_remove(&self.ctx.root, &wt_abs);
            }
            let _ = store.release(id);
            e
        };
        let run = store
            .create_run(&NewRun {
                issue_id: id.clone(),
                runtime: self.ctx.config.worker.runtime.clone(),
                worktree: worktree_rel.clone(),
                branch: branch.clone(),
                base_sha: base,
                budget_usd: Some(self.ctx.config.worker.budget_usd),
            })
            .map_err(AppError::from)
            .map_err(rollback)?;
        store.set_claim_run(id, &run.id)?;
        let fail_run = |e: AppError| -> AppError {
            let _ = self.set_state(&run, RunState::Failed, Some(e.message.clone()));
            rollback(e)
        };
        let settings_path = self.write_brief_files(&issue, &run, &wt_abs).map_err(fail_run)?;

        let ws = self
            .rt
            .open_workspace(&OpenWorkspace {
                cwd: wt_abs.clone(),
                label: format!("{id} {}", issue.title),
                env: vec![
                    (ENV_ISSUE.into(), id.to_string()),
                    (crate::commands::hook::ENV_RUN.into(), run.id.clone()),
                ],
            })
            .map_err(rt_err)
            .map_err(fail_run)?;
        let session_id = uuid::Uuid::new_v4().to_string();
        let run = store.update_run(
            &run.id,
            &RunPatch {
                workspace_id: Some(ws.workspace_id.clone()),
                pane_id: Some(ws.pane_id.clone()),
                agent_name: Some(id.to_string()),
                claude_session_id: Some(session_id.clone()),
                ..RunPatch::default()
            },
        )?;
        let _ = self.rt.report(
            &ws.pane_id,
            Some(id.as_str()),
            &[
                ("issue".into(), id.to_string()),
                (
                    "budget".into(),
                    format!("{:.2}", self.ctx.config.worker.budget_usd),
                ),
            ],
        );
        let mut args = vec![
            "--permission-mode".to_string(),
            self.ctx.config.worker.permission_mode.clone(),
            "--settings".to_string(),
            fwd(&settings_path),
            "--session-id".to_string(),
            session_id,
        ];
        if let Some(m) = &self.ctx.config.worker.model {
            args.push("--model".into());
            args.push(m.clone());
        }
        let start = StartAgent {
            name: id.to_string(),
            pane_id: ws.pane_id.clone(),
            args,
            timeout_ms: self.ctx.config.worker.start_timeout_ms,
        };
        let handle = AgentHandle {
            name: id.to_string(),
            pane_id: ws.pane_id.clone(),
            workspace_id: ws.workspace_id.clone(),
        };
        let mut trusted = false;
        let agent = match self.rt.start_agent(&start) {
            Ok(a) => a,
            Err(RuntimeError::NotReady(msg)) => {
                let screen = self.rt.read(&handle, 60).unwrap_or_default();
                let is_trust = TRUST_MARKERS
                    .iter()
                    .any(|m| screen.to_lowercase().contains(&m.to_lowercase()));
                if is_trust && self.ctx.config.worker.auto_trust {
                    self.rt.send_keys(&handle, &["down", "enter"]).map_err(rt_err)?;
                    trusted = true;
                    match self.rt.wait(
                        &handle,
                        &[AgentState::Idle],
                        self.ctx.config.worker.start_timeout_ms,
                    ) {
                        Ok(a) => a,
                        Err(_) => self
                            .rt
                            .start_agent(&start)
                            .map_err(|e| self.block_start(&run, &handle, e))?,
                    }
                } else {
                    return Err(self.block_start(&run, &handle, RuntimeError::NotReady(msg)));
                }
            }
            Err(RuntimeError::Timeout(msg)) => {
                // Herdr's startup detection gave up; the agent may still be coming up. Poll instead of blocking.
                let deadline = std::time::Instant::now()
                    + Duration::from_millis(self.ctx.config.worker.start_timeout_ms);
                loop {
                    if let Ok(info) = self.rt.info(&handle) {
                        if info.state.is_settled() || info.state == AgentState::Working {
                            break info;
                        }
                    }
                    if std::time::Instant::now() >= deadline {
                        return Err(self.block_start(&run, &handle, RuntimeError::Timeout(msg)));
                    }
                    std::thread::sleep(Duration::from_millis(2000));
                }
            }
            Err(e) => return Err(self.block_start(&run, &handle, e)),
        };
        let kick = format!("Start {id}: read {FACTORY_DIR}/{RUNS_DIR}/{id}/brief.md and follow it.");
        let agent = match self.rt.prompt(
            &handle,
            &kick,
            &[AgentState::Working],
            self.ctx.config.worker.prompt_timeout_ms,
        ) {
            Ok(a) => a,
            Err(RuntimeError::Timeout(_)) => agent,
            Err(RuntimeError::Stalled(_)) => {
                // Verified live: the text can sit in Claude's input box with Enter swallowed. Never retype
                // the prompt; press Enter once and give the agent a moment to start.
                self.rt
                    .send_keys(&handle, &["enter"])
                    .map_err(|e| self.block_start(&run, &handle, e))?;
                match self.rt.wait(
                    &handle,
                    &[AgentState::Working],
                    self.ctx.config.worker.prompt_timeout_ms,
                ) {
                    Ok(a) => a,
                    Err(e) => return Err(self.block_start(&run, &handle, e)),
                }
            }
            Err(e) => return Err(self.block_start(&run, &handle, e)),
        };
        let run = store.update_run(
            &run.id,
            &RunPatch {
                state: Some(RunState::Working),
                last_seen_at: Some(now_rfc3339()),
                herdr_revision: Some(agent.revision),
                claude_session_id: agent.session_id.clone().or(run.claude_session_id.clone()),
                ..RunPatch::default()
            },
        )?;
        Ok(StartResult {
            run,
            workspace_id: ws.workspace_id,
            pane_id: ws.pane_id,
            agent,
            trusted,
        })
    }

    /// Startup could not reach a working agent: keep the claim and workspace for inspection, tell the human.
    fn block_start(&self, run: &Run, handle: &AgentHandle, e: RuntimeError) -> AppError {
        let screen = self.rt.read(handle, 40).unwrap_or_default();
        let _ = self.set_state(run, RunState::Blocked, Some(e.to_string()));
        let _ = self.blocker(
            run,
            format!(
                "Worker for {} could not start: {e}\n\nScreen:\n```\n{}\n```\nInspect the pane (`asf run show {}`), answer with \
                 `asf run keys {} <key>` / `asf run reply {} <text>`, or `asf run abort {}`.",
                run.issue_id,
                tail(&screen, 20),
                run.issue_id,
                run.issue_id,
                run.issue_id,
                run.issue_id
            ),
        );
        AppError::new(
            ErrorCode::Blocked,
            format!("worker for {} is blocked at startup: {e}", run.issue_id),
        )
    }

    pub fn list(&self) -> Result<Vec<Run>, AppError> {
        Ok(self.ctx.store.runs(None, false)?)
    }

    pub fn show(&self, id: &IssueId) -> Result<ShowResult, AppError> {
        let run = self.run_for(id)?;
        let agent = Self::handle(&run).ok().and_then(|h| self.rt.info(&h).ok());
        Ok(ShowResult { run, agent })
    }

    fn unblock(&self, run: &Run) -> Result<Run, AppError> {
        if matches!(run.state, RunState::Blocked | RunState::Stalled) {
            return self.set_state(run, RunState::Working, None);
        }
        Ok(run.clone())
    }

    pub fn reply(&self, id: &IssueId, text: &str) -> Result<Run, AppError> {
        let run = self.run_for(id)?;
        let h = Self::handle(&run)?;
        match self
            .rt
            .prompt(&h, text, &[], self.ctx.config.worker.prompt_timeout_ms)
        {
            Ok(_) | Err(RuntimeError::Timeout(_)) | Err(RuntimeError::Stalled(_)) => {}
            Err(e) => return Err(rt_err(e)),
        }
        self.unblock(&run)
    }

    pub fn keys(&self, id: &IssueId, keys: &[String]) -> Result<Run, AppError> {
        let run = self.run_for(id)?;
        let h = Self::handle(&run)?;
        let ks: Vec<&str> = keys.iter().map(String::as_str).collect();
        self.rt.send_keys(&h, &ks).map_err(rt_err)?;
        self.unblock(&run)
    }

    pub fn nudge(&self, id: &IssueId) -> Result<Run, AppError> {
        let asf = crate::ctx::asf_bin();
        let text = format!(
            "Reminder from asf: when the acceptance criteria hold, commit everything, write {FACTORY_DIR}/{RUNS_DIR}/{id}/report.md \
             and run `{asf} issue finish {id} --report {FACTORY_DIR}/{RUNS_DIR}/{id}/report.md`. If you are blocked, post a BLOCKER comment."
        );
        self.reply(id, &text)
    }

    pub fn abort(&self, id: &IssueId) -> Result<Run, AppError> {
        let run = self.run_for(id)?;
        if run.state.is_terminal() {
            return Err(AppError::conflict(format!(
                "run {} is already {}",
                run.id, run.state
            )));
        }
        if let Ok(h) = Self::handle(&run) {
            let _ = self.rt.send_keys(&h, &["esc"]);
            let _ = self.rt.send_keys(&h, &["ctrl+c"]);
            let _ = self.rt.send_keys(&h, &["ctrl+c"]);
        }
        if let Some(ws) = &run.workspace_id {
            let _ = self.rt.close_workspace(ws);
        }
        let run = self.set_state(&run, RunState::Aborted, Some("aborted by operator".into()))?;
        let _ = self.ctx.store.release(id);
        Ok(run)
    }

    /// One pass over the active runs.
    pub fn watch(&self) -> Result<WatchResult, AppError> {
        let store = &self.ctx.store;
        let active = store.runs(None, true)?;
        let mut transitions = Vec::new();
        for run in &active {
            if run.state == RunState::Starting {
                continue; // another process is still in `start`
            }
            let issue = match store.get_issue(&run.issue_id)? {
                Some(i) => i,
                None => continue,
            };
            let handle = Self::handle(run)?;
            let info = self.rt.info(&handle).ok();
            if issue.status == Status::InReview {
                let t = self.verify(run, &issue)?;
                transitions.push(t);
                continue;
            }
            let Some(info) = info else { continue };
            store.update_run(
                &run.id,
                &RunPatch {
                    herdr_revision: Some(info.revision),
                    ..RunPatch::default()
                },
            )?;
            match info.state {
                AgentState::Blocked if run.state != RunState::Blocked => {
                    let screen = self.rt.read(&handle, 40).unwrap_or_default();
                    self.set_state(run, RunState::Blocked, None)?;
                    self.blocker(
                        run,
                        format!(
                            "Worker for {} is waiting for input.\n\nScreen:\n```\n{}\n```\nAnswer with `asf run keys {} y` / \
                             `asf run reply {} <text>` (or open the pane).",
                            run.issue_id,
                            tail(&screen, 20),
                            run.issue_id,
                            run.issue_id
                        ),
                    )?;
                    transitions.push(Transition {
                        run_id: run.id.clone(),
                        issue_id: run.issue_id.clone(),
                        from: run.state,
                        to: RunState::Blocked,
                        detail: "agent blocked on a prompt".into(),
                    });
                }
                AgentState::Working if run.state != RunState::Working => {
                    self.set_state(run, RunState::Working, None)?;
                    transitions.push(Transition {
                        run_id: run.id.clone(),
                        issue_id: run.issue_id.clone(),
                        from: run.state,
                        to: RunState::Working,
                        detail: "agent working".into(),
                    });
                }
                AgentState::Working => {
                    store.update_run(
                        &run.id,
                        &RunPatch {
                            last_seen_at: Some(now_rfc3339()),
                            ..RunPatch::default()
                        },
                    )?;
                }
                _ => {}
            }
        }
        Ok(WatchResult {
            checked: active.len(),
            transitions,
        })
    }

    /// Operator-triggered rerun of the verification `watch` performs. State checks come first and
    /// are conflicts (exit 3); only `failed`, `finishing` and `verifying` runs reach the gates.
    pub fn verify_issue(&self, id: &IssueId) -> Result<Transition, AppError> {
        let store = &self.ctx.store;
        let issue = store
            .get_issue(id)?
            .ok_or_else(|| AppError::not_found(format!("issue {id}")))?;
        let run = store
            .runs(Some(id), false)?
            .into_iter()
            .last()
            .ok_or_else(|| AppError::conflict("no run to verify"))?;
        match run.state {
            RunState::Succeeded => Err(AppError::conflict("already verified and queued")),
            RunState::Starting
            | RunState::Working
            | RunState::Blocked
            | RunState::Stalled
            | RunState::OverBudget => Err(AppError::conflict(
                "worker still active; wait for `asf issue finish` or `asf run abort`",
            )),
            RunState::Aborted => Err(AppError::conflict("run was aborted; dispatch again")),
            RunState::Failed | RunState::Finishing | RunState::Verifying => self.verify(&run, &issue),
        }
    }

    /// Independent verification after the worker's finish ritual.
    fn verify(&self, run: &Run, issue: &Issue) -> Result<Transition, AppError> {
        let wt_abs = self.ctx.root.join(&run.worktree);
        let mut problems: Vec<String> = Vec::new();
        self.set_state(run, RunState::Verifying, None)?;
        let dirty = self.vcs.status_porcelain(&wt_abs).map_err(vcs_err)?;
        if !dirty.is_empty() {
            problems.push(format!("uncommitted changes in worktree: {}", dirty.join(", ")));
        }
        let changed = self.vcs.changed_files(&wt_abs, &run.base_sha).map_err(vcs_err)?;
        if changed.is_empty() {
            problems.push("no committed changes since the base".into());
        }
        let outside = files_outside_scope(&changed, &issue.file_scope);
        if !outside.is_empty() {
            problems.push(format!(
                "files outside scope [{}]: {}",
                issue.file_scope.join(", "),
                outside.join(", ")
            ));
        }
        let specs: Vec<GateSpec> = self
            .ctx
            .config
            .gates
            .iter()
            .map(|g| GateSpec {
                name: g.name.clone(),
                cmd: g.cmd.clone(),
                timeout_s: g.timeout_s,
                shell: g.shell.clone(),
            })
            .collect();
        let results = run_gates(&specs, &wt_abs);
        for r in results.iter().filter(|r| !r.ok) {
            problems.push(format!(
                "gate `{}` failed (exit {:?}):\n{}",
                r.name,
                r.exit_code,
                tail(&r.tail, 15)
            ));
        }
        if problems.is_empty() {
            let r = self.set_state(run, RunState::Succeeded, Some("verified".into()))?;
            self.ctx
                .store
                .enqueue_merge(&run.issue_id, Some(&run.id), &run.branch)?;
            let _ = self.rt.notify(
                &format!("asf {} verified", run.issue_id),
                &format!("{} queued for merge", issue.title),
                Some("done"),
            );
            Ok(Transition {
                run_id: r.id,
                issue_id: run.issue_id.clone(),
                from: run.state,
                to: RunState::Succeeded,
                detail: format!("{} files, {} gates", changed.len(), results.len()),
            })
        } else {
            let reason = problems.join("\n");
            self.set_state(run, RunState::Failed, Some(reason.clone()))?;
            self.blocker(
                run,
                format!(
                    "Verification of {} failed:\n{reason}\n\nFix in the worktree ({}) and re-run `asf issue finish`, or \
                     `asf run abort {}`.",
                    run.issue_id, run.worktree, run.issue_id
                ),
            )?;
            Ok(Transition {
                run_id: run.id.clone(),
                issue_id: run.issue_id.clone(),
                from: run.state,
                to: RunState::Failed,
                detail: reason,
            })
        }
    }
}

pub fn transitions_text(w: &WatchResult) -> String {
    if w.transitions.is_empty() {
        return format!("watched {} run(s); no changes", w.checked);
    }
    w.transitions
        .iter()
        .map(|t| {
            format!(
                "{} {} -> {}: {}",
                t.issue_id,
                t.from,
                t.to,
                t.detail.lines().next().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn run_line(r: &Run) -> String {
    format!(
        "{:<10} {:<12} ${:.2} calls {:<4} {} {}",
        r.issue_id,
        r.state,
        r.cost_usd,
        r.tool_calls,
        r.pane_id.as_deref().unwrap_or("-"),
        r.exit_reason
            .as_deref()
            .map(|s| s.lines().next().unwrap_or(""))
            .unwrap_or("")
    )
}

pub fn start_json(s: &StartResult) -> serde_json::Value {
    json!({ "run": s.run, "workspace_id": s.workspace_id, "pane_id": s.pane_id, "agent": s.agent, "trusted": s.trusted })
}
