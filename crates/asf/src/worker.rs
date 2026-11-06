//! Worker orchestration: start a run in a worktree + runtime workspace, watch it, verify it, abort it.
//!
//! Completion is never inferred from the agent's screen state: a run is verified only after the
//! worker ran `asf issue finish` (issue `in-review`), and only the orchestrator decides `succeeded`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use asf_core::config::GateWhen;
use asf_core::*;
use asf_vcs::gates::stage_specs;
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

/// Everything `launch` needs beyond the run itself.
struct Launch<'a> {
    run: &'a Run,
    wt_abs: &'a Path,
    label: String,
    settings_path: PathBuf,
    model: Option<String>,
    kick: String,
    /// After a startup-detection timeout whose agent never came up, start once more in a fresh
    /// workspace. Workers do; reviewers keep the single attempt.
    retry_start: bool,
}

/// A runtime workspace whose agent is about to be started: the run (patched with the workspace, the
/// pane and a fresh Claude session id) and everything `start_agent` and the later calls need.
struct Pane {
    run: Run,
    ws: WorkspaceRef,
    start: StartAgent,
    handle: AgentHandle,
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

/// Tools a reviewer may use (gate commands are appended): reading, the two read-only git views, and `asf`.
#[allow(dead_code)] // wired into the merge flow by asf-21
const REVIEW_TOOLS: [&str; 6] = [
    "Read",
    "Grep",
    "Glob",
    "Bash(git diff *)",
    "Bash(git log *)",
    "Bash(asf *)",
];
/// Denied outright, so `permission_mode = auto` can never approve an edit for a reviewer.
#[allow(dead_code)] // wired into the merge flow by asf-21
const REVIEW_DENY: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// Run `git` in `dir` and return trimmed stdout (for what `asf_vcs::GitCli` does not offer).
#[allow(dead_code)] // wired into the merge flow by asf-21
fn git_out(dir: &Path, args: &[&str]) -> Result<String, AppError> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| AppError::io(format!("cannot run git: {e}")))?;
    if !out.status.success() {
        return Err(AppError::new(
            ErrorCode::Backend,
            format!(
                "git {} (in {}): {}",
                args.join(" "),
                dir.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Body lines of every `Open questions` heading (any level, case-insensitive, optional trailing colon)
/// that hold real content. A section that only says "None" (or `n/a`, `-`, ...) counts as empty.
pub(crate) fn open_questions(report: &str) -> Vec<String> {
    fn heading(line: &str) -> Option<&str> {
        let t = line.trim_start();
        let rest = t.trim_start_matches('#');
        let level = t.len() - rest.len();
        ((1..=6).contains(&level) && rest.starts_with(char::is_whitespace))
            .then(|| rest.trim().trim_end_matches(':').trim())
    }
    fn is_none(line: &str) -> bool {
        let t = line
            .trim()
            .trim_start_matches(['-', '*', '+'])
            .trim()
            .trim_end_matches(['.', '!'])
            .to_lowercase();
        matches!(
            t.as_str(),
            "" | "none" | "n/a" | "na" | "nothing" | "no" | "no open questions" | "brak"
        )
    }
    let mut out = Vec::new();
    let mut inside = false;
    for line in report.lines() {
        if let Some(h) = heading(line) {
            inside = h.eq_ignore_ascii_case("open questions");
        } else if inside && !is_none(line) {
            out.push(line.trim_end().to_string());
        }
    }
    out
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
        self.launch(
            &Launch {
                run: &run,
                wt_abs: &wt_abs,
                label: format!("{id} {}", issue.title),
                settings_path,
                model: self.ctx.config.worker.model.clone(),
                kick: format!("Start {id}: read {FACTORY_DIR}/{RUNS_DIR}/{id}/brief.md and follow it."),
                retry_start: true,
            },
            &fail_run,
        )
    }

    /// Open the runtime workspace, start Claude with the prepared settings, send the kick-off prompt
    /// and mark the run `working`. Shared by workers and reviewers; `fail_run` undoes whatever the
    /// caller set up when a step before the agent exists fails.
    fn launch(
        &self,
        l: &Launch<'_>,
        fail_run: &dyn Fn(AppError) -> AppError,
    ) -> Result<StartResult, AppError> {
        let store = &self.ctx.store;
        let Pane {
            mut run,
            mut ws,
            mut start,
            mut handle,
        } = self.open_pane(l, l.run).map_err(fail_run)?;
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
            Err(RuntimeError::Timeout(msg)) => match self.await_started(&handle, msg) {
                Ok(a) => a,
                Err(first) if l.retry_start => {
                    // The pane never came up: record why, drop the dead workspace and try exactly once more
                    // in a fresh one (new pane, new session id). A second failure blocks as before.
                    store.append_event(&NewEvent {
                        kind: "run.start_retry".into(),
                        issue_id: Some(run.issue_id.clone()),
                        run_id: Some(run.id.clone()),
                        actor: Some(LANE_ORCHESTRATOR.into()),
                        payload: json!({ "reason": first.to_string(), "workspace_id": ws.workspace_id }),
                    })?;
                    let _ = self.rt.close_workspace(&ws.workspace_id);
                    let pane = self.open_pane(l, &run).map_err(fail_run)?;
                    (run, ws, start, handle) = (pane.run, pane.ws, pane.start, pane.handle);
                    match self.rt.start_agent(&start) {
                        Ok(a) => a,
                        Err(RuntimeError::Timeout(msg)) => self
                            .await_started(&handle, msg)
                            .map_err(|e| self.block_start(&run, &handle, e))?,
                        Err(e) => return Err(self.block_start(&run, &handle, e)),
                    }
                }
                Err(e) => return Err(self.block_start(&run, &handle, e)),
            },
            Err(e) => return Err(self.block_start(&run, &handle, e)),
        };
        let agent = match self.rt.prompt(
            &handle,
            &l.kick,
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

    /// Open a runtime workspace for `run` and prepare the `start_agent` call: record workspace, pane
    /// and a new Claude session id on the run, label the pane and build the `claude` arguments.
    /// A retry calls this again, so nothing from an abandoned launch (notably the session id) is reused.
    fn open_pane(&self, l: &Launch<'_>, run: &Run) -> Result<Pane, AppError> {
        let id = &run.issue_id;
        let ws = self
            .rt
            .open_workspace(&OpenWorkspace {
                cwd: l.wt_abs.to_path_buf(),
                label: l.label.clone(),
                env: vec![
                    (ENV_ISSUE.into(), id.to_string()),
                    (crate::commands::hook::ENV_RUN.into(), run.id.clone()),
                ],
            })
            .map_err(rt_err)?;
        let session_id = uuid::Uuid::new_v4().to_string();
        let run = self.ctx.store.update_run(
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
                    format!(
                        "{:.2}",
                        run.budget_usd.unwrap_or(self.ctx.config.worker.budget_usd)
                    ),
                ),
            ],
        );
        let mut args = vec![
            "--permission-mode".to_string(),
            self.ctx.config.worker.permission_mode.clone(),
            "--settings".to_string(),
            fwd(&l.settings_path),
            "--session-id".to_string(),
            session_id,
        ];
        if let Some(m) = &l.model {
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
        Ok(Pane {
            run,
            ws,
            start,
            handle,
        })
    }

    /// `start_agent` timed out: Herdr's startup detection gave up, but the agent may still be coming
    /// up. Re-read its state for up to `start_timeout_ms` and accept `idle`, `done` or `working`
    /// (it is up). Anything else (`unknown`, `blocked`) ends in a timeout error that names the last
    /// state seen, which becomes the reason of a retry or of the BLOCKER.
    fn await_started(&self, handle: &AgentHandle, msg: String) -> Result<AgentInfo, RuntimeError> {
        let deadline =
            std::time::Instant::now() + Duration::from_millis(self.ctx.config.worker.start_timeout_ms);
        let mut last = AgentState::Unknown;
        loop {
            if let Ok(info) = self.rt.info(handle) {
                if matches!(
                    info.state,
                    AgentState::Idle | AgentState::Done | AgentState::Working
                ) {
                    return Ok(info);
                }
                last = info.state;
            }
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return Err(RuntimeError::Timeout(format!(
                    "{msg}; agent state after re-read: {last}"
                )));
            }
            std::thread::sleep(left.min(Duration::from_millis(2000)));
        }
    }

    /// The review child of `issue`: reuse the open one, else create it `ready` (the claim moves it to
    /// `in-progress`). It carries the worker's file scope so the scheduler sees the same footprint.
    #[allow(dead_code)] // wired into the merge flow by asf-21
    fn review_issue(&self, issue: &Issue) -> Result<Issue, AppError> {
        let title = format!("Review {}: {}", issue.id, issue.title);
        let store = &self.ctx.store;
        let open = store
            .list_issues(&IssueFilter {
                kind: Some(IssueType::Review),
                parent: Some(issue.id.clone()),
                ..IssueFilter::default()
            })?
            .into_iter()
            .rfind(|i| i.title == title && !i.status.is_terminal());
        if let Some(i) = open {
            return Ok(i);
        }
        Ok(store.create_issue(&NewIssue {
            title,
            body: format!(
                "Read-only review of {} on branch feat/{}. The verdict is the DECISION comment the reviewer posts on {}.",
                issue.id, issue.id, issue.id
            ),
            kind: IssueType::Review,
            status: Status::Ready,
            priority: issue.priority,
            parent_id: Some(issue.id.clone()),
            file_scope: issue.file_scope.clone(),
            acceptance: issue.acceptance.clone(),
            ..NewIssue::default()
        })?)
    }

    /// A worktree at `sha` with a detached HEAD, so the branch itself stays checked out where it is
    /// (git refuses to check one branch out twice). An existing one is moved to `sha`.
    #[allow(dead_code)] // wired into the merge flow by asf-21
    fn detached_worktree(&self, wt_abs: &Path, sha: &str) -> Result<(), AppError> {
        if wt_abs.exists() {
            if !wt_abs.join(".git").exists() {
                return Err(AppError::conflict(format!(
                    "{} already exists and is not a worktree",
                    wt_abs.display()
                )));
            }
            if git_out(wt_abs, &["rev-parse", "HEAD"])? != sha {
                git_out(wt_abs, &["checkout", "--detach", sha])?;
            }
            return Ok(());
        }
        if let Some(parent) = wt_abs.parent() {
            std::fs::create_dir_all(parent)?;
        }
        git_out(
            &self.ctx.root,
            &["worktree", "add", "--detach", &fwd(wt_abs), sha],
        )?;
        Ok(())
    }

    /// Brief, settings and `CLAUDE.local.md` of a reviewer. Edit and Write are neither allowed nor
    /// reachable: they are denied outright, and the `Stop` hook is dropped because it would push a
    /// reviewer (who never runs `issue finish`) through the worker's finish ritual.
    #[allow(dead_code)] // wired into the merge flow by asf-21
    fn write_review_files(
        &self,
        issue: &Issue,
        run: &Run,
        review: &Run,
        wt_abs: &Path,
    ) -> Result<PathBuf, AppError> {
        let asf_bin = crate::ctx::asf_bin();
        let gates: Vec<String> = self.ctx.config.gates.iter().map(|g| g.cmd.join(" ")).collect();
        let runs_dir = Self::runs_dir(wt_abs, &review.issue_id);
        std::fs::create_dir_all(&runs_dir)?;
        let brief = asf_core::brief::render_review_brief(&asf_core::brief::ReviewInput {
            issue,
            base_sha: &run.base_sha,
            worktree: &review.worktree,
            branch: &run.branch,
            gates: &gates,
            asf_bin: &asf_bin,
        });
        std::fs::write(runs_dir.join("brief.md"), brief)?;

        let mut allowed: Vec<String> = REVIEW_TOOLS.iter().map(|t| t.to_string()).collect();
        allowed.extend(gates.iter().map(|g| format!("Bash({g})")));
        let mut deny = self.ctx.config.worker.deny.clone();
        for t in REVIEW_DENY {
            if !deny.iter().any(|d| d == t) {
                deny.push(t.to_string());
            }
        }
        let mut settings = render_settings(&SettingsInput {
            asf_bin: &asf_bin,
            allowed_tools: &allowed,
            deny: &deny,
            scope: &[],
            hook_timeout_s: HOOK_TIMEOUT_S,
        });
        if let Some(hooks) = settings.get_mut("hooks").and_then(|h| h.as_object_mut()) {
            hooks.remove("Stop");
        }
        let settings_path = runs_dir.join("settings.json");
        std::fs::write(
            &settings_path,
            serde_json::to_string_pretty(&settings).map_err(|e| AppError::io(e.to_string()))?,
        )?;
        let brief_rel = format!("{FACTORY_DIR}/{RUNS_DIR}/{}/brief.md", review.issue_id);
        std::fs::write(
            wt_abs.join("CLAUDE.local.md"),
            format!(
                "# Reviewer {rid}\n\nYou are the read-only reviewer of `{issue}`. Your full brief is in `{brief_rel}` — read it \
                 first and follow its Review steps and Verdict. Never edit, create or delete files.\n",
                rid = review.issue_id,
                issue = issue.id
            ),
        )?;
        Ok(settings_path)
    }

    /// Start the read-only reviewer for `issue`, whose worker ran as `run`: a child `review` issue
    /// (`Review <id>: <title>`, same file scope, `in-progress`), a detached worktree at
    /// `.worktrees/<review-id>` on the branch tip, and Claude on `[review] model` with read-only tools.
    ///
    /// Idempotent: the review issue and its worktree are reused, and a review run that is still
    /// active is returned as it is instead of starting a second agent. The review issue's id is the
    /// reviewer's lane, so its comments are authored `agent:<review-id>`.
    #[allow(dead_code)] // wired into the merge flow by asf-21
    pub fn start_review(&self, issue: &Issue, run: &Run) -> Result<StartResult, AppError> {
        let store = &self.ctx.store;
        let review = self.review_issue(issue)?;
        let rid = review.id.clone();

        if let Some(active) = store.runs(Some(&rid), true)?.into_iter().last() {
            let agent = self.rt.info(&Self::handle(&active)?).map_err(rt_err)?;
            return Ok(StartResult {
                workspace_id: active.workspace_id.clone().unwrap_or_default(),
                pane_id: active.pane_id.clone().unwrap_or_default(),
                run: active,
                agent,
                trusted: false,
            });
        }

        let sha = git_out(
            &self.ctx.root,
            &["rev-parse", "--verify", &format!("refs/heads/{}", run.branch)],
        )?;
        let wt_rel = self.worktree_rel(&rid);
        let wt_abs = self.ctx.root.join(&wt_rel);
        self.detached_worktree(&wt_abs, &sha)?;

        // A claim left over from a crashed review is dropped; the claim itself moves the issue to in-progress.
        if store.active_claims()?.iter().any(|c| c.issue_id == rid) {
            store.release(&rid)?;
        }
        if store.get_issue(&rid)?.map(|i| i.status) != Some(Status::Ready) {
            store.set_status(&rid, Status::Ready, LANE_ORCHESTRATOR)?;
        }
        store.claim(
            &rid,
            &NewClaim {
                worker: rid.to_string(),
                run_id: None,
                worktree: Some(wt_rel.clone()),
                branch: Some(run.branch.clone()),
                note: Some("asf review".into()),
            },
        )?;
        let release = |e: AppError| -> AppError {
            let _ = store.release(&rid);
            e
        };
        self.rt.health().map_err(rt_err).map_err(release)?;
        let rrun = store
            .create_run(&NewRun {
                issue_id: rid.clone(),
                runtime: self.ctx.config.worker.runtime.clone(),
                worktree: wt_rel,
                branch: run.branch.clone(),
                base_sha: run.base_sha.clone(),
                budget_usd: Some(self.ctx.config.worker.budget_usd),
            })
            .map_err(AppError::from)
            .map_err(release)?;
        store.set_claim_run(&rid, &rrun.id)?;
        let fail_run = |e: AppError| -> AppError {
            let _ = self.set_state(&rrun, RunState::Failed, Some(e.message.clone()));
            release(e)
        };
        let settings_path = self
            .write_review_files(issue, run, &rrun, &wt_abs)
            .map_err(fail_run)?;
        self.launch(
            &Launch {
                run: &rrun,
                wt_abs: &wt_abs,
                label: format!("{rid} {}", review.title),
                settings_path,
                model: Some(self.ctx.config.review.model.clone()),
                kick: format!("Start {rid}: read {FACTORY_DIR}/{RUNS_DIR}/{rid}/brief.md and follow it."),
                retry_start: false,
            },
            &fail_run,
        )
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
        let again = self.finished_again()?;
        for (run, issue) in &again {
            transitions.push(self.verify(run, issue)?);
        }
        Ok(WatchResult {
            checked: active.len() + again.len(),
            transitions,
        })
    }

    /// Failed runs whose worker has finished again: the issue's latest run is `failed`, the issue is
    /// `in-review` and a worker STATUS comment is newer than the run's `finished_at` (the latest
    /// STATUS counts). `asf issue finish` only touches active runs, so without this the operator
    /// would have to run `asf run verify` by hand. Timestamps have one-second resolution and the
    /// comparison is strict: the STATUS that triggered the failed verification is never newer than it.
    fn finished_again(&self) -> Result<Vec<(Run, Issue)>, AppError> {
        let store = &self.ctx.store;
        // runs come back ordered by start, so the last one per issue is its latest
        let mut latest: Vec<Run> = Vec::new();
        for run in store.runs(None, false)? {
            match latest.iter_mut().find(|r| r.issue_id == run.issue_id) {
                Some(slot) => *slot = run,
                None => latest.push(run),
            }
        }
        let mut again = Vec::new();
        for run in latest.into_iter().filter(|r| r.state == RunState::Failed) {
            let Some(finished_at) = run.finished_at.clone() else {
                continue;
            };
            let Some(issue) = store.get_issue(&run.issue_id)? else {
                continue;
            };
            if issue.status != Status::InReview {
                continue;
            }
            let status_at = store
                .comments(&run.issue_id)?
                .into_iter()
                .filter(|c| c.kind == CommentType::Status && c.author.starts_with("agent:"))
                .map(|c| c.created_at)
                .max();
            if status_at.is_some_and(|at| at > finished_at) {
                again.push((run, issue));
            }
        }
        Ok(again)
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

    /// Copy the worker's report to `<root>/.factory/runs/<run-id>/report.md` (the worktree copy dies
    /// with `lane clean`) and return its text. The error string becomes a verification problem.
    fn archive_report(&self, run: &Run, wt_abs: &Path) -> Result<String, String> {
        let src = match &run.report_path {
            Some(p) if Path::new(p).is_absolute() => PathBuf::from(p),
            Some(p) => wt_abs.join(p),
            None => Self::runs_dir(wt_abs, &run.issue_id).join("report.md"),
        };
        let text = std::fs::read(&src).map_err(|e| format!("report {} is unreadable: {e}", src.display()))?;
        let dest_dir = self.ctx.root.join(FACTORY_DIR).join(RUNS_DIR).join(&run.id);
        std::fs::create_dir_all(&dest_dir)
            .and_then(|_| std::fs::write(dest_dir.join("report.md"), &text))
            .map_err(|e| format!("cannot archive the report to {}: {e}", dest_dir.display()))?;
        Ok(String::from_utf8_lossy(&text).into_owned())
    }

    /// Independent verification after the worker's finish ritual.
    fn verify(&self, run: &Run, issue: &Issue) -> Result<Transition, AppError> {
        let wt_abs = self.ctx.root.join(&run.worktree);
        let mut problems: Vec<String> = Vec::new();
        self.set_state(run, RunState::Verifying, None)?;
        // The report first: whatever else fails, the human gets the worker's own account of the run.
        match self.archive_report(run, &wt_abs) {
            Ok(text) => {
                let open = open_questions(&text);
                if !open.is_empty() {
                    problems.push(format!(
                        "report has unresolved open questions (ask them as QUESTION comments and finish again):\n{}",
                        open.join("\n")
                    ));
                }
            }
            Err(e) => problems.push(e),
        }
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
        let specs = stage_specs(&self.ctx.config.gates, GateWhen::Verify);
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
                .set_status(&run.issue_id, Status::Validating, LANE_ORCHESTRATOR)?;
            // A branch that is already queued (an earlier verification, or `merge enqueue` by hand) is
            // reused: a second `enqueue_merge` would be a conflict that aborts the whole tick.
            let queued = self
                .ctx
                .store
                .merge_queue()?
                .iter()
                .any(|m| m.issue_id == run.issue_id && !m.state.is_terminal());
            if !queued {
                self.ctx
                    .store
                    .enqueue_merge(&run.issue_id, Some(&run.id), &run.branch)?;
            }
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
            self.ctx
                .store
                .set_status(&run.issue_id, Status::Blocked, LANE_ORCHESTRATOR)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use asf_runtime::FakeRuntime;
    use asf_store::SqliteStore;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn open_questions_reads_only_real_content_under_the_heading() {
        assert!(open_questions("# done\n\n## Open questions\n\nNone.\n").is_empty());
        assert!(open_questions("## Open questions:\n- n/a\n\n## Notes\nfine\n").is_empty());
        assert!(
            open_questions("## Notes\n- should we?\n").is_empty(),
            "other sections are ignored"
        );
        assert_eq!(
            open_questions("# r\n### OPEN QUESTIONS\n- one?\n- two?\n## Next\nx\n"),
            ["- one?", "- two?"]
        );
        assert_eq!(
            open_questions("## Open Questions\nWhich crate?"),
            ["Which crate?"]
        );
        assert!(
            open_questions("#hashtag open questions\nx").is_empty(),
            "a heading needs a space after #"
        );
    }

    struct Fx {
        dir: tempfile::TempDir,
        ctx: Ctx,
        issue: Issue,
        run: Run,
    }

    /// A repo with a worker branch `feat/asf-1` (one commit ahead of `main`) in its own worktree.
    fn fixture() -> Fx {
        let dir = tempfile::Builder::new().prefix("asf-wk-").tempdir().unwrap();
        let root = dir.path().to_path_buf();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "user.email", "t@example.com"]);
        git(&root, &["config", "user.name", "t"]);
        std::fs::write(root.join("a.txt"), "a\n").unwrap();
        git(&root, &["add", "a.txt"]);
        git(&root, &["commit", "-q", "-m", "init"]);
        std::fs::create_dir_all(root.join(FACTORY_DIR)).unwrap();
        let store = SqliteStore::open(&root.join(FACTORY_DIR).join("factory.db")).unwrap();
        store.init_repo(&fwd(&root), "asf", "main").unwrap();
        let mut config = Config::default();
        config.gates = vec![asf_core::config::GateCfg {
            name: "fmt".into(),
            cmd: vec!["cargo".into(), "fmt".into(), "--check".into()],
            ..asf_core::config::GateCfg::default()
        }];
        let ctx = Ctx {
            root: root.clone(),
            config,
            store,
            lane: LANE_ORCHESTRATOR.into(),
        };
        let issue = ctx
            .store
            .create_issue(&NewIssue {
                title: "add greeting".into(),
                status: Status::Ready,
                file_scope: vec!["src/**".into()],
                acceptance: vec![Acceptance {
                    text: "prints hello".into(),
                    verify: None,
                }],
                ..NewIssue::default()
            })
            .unwrap();
        let wt = root.join(".worktrees").join("asf-1");
        let base = GitCli.worktree_add(&root, &wt, "feat/asf-1", "main").unwrap();
        std::fs::write(wt.join("b.txt"), "b\n").unwrap();
        git(&wt, &["add", "b.txt"]);
        git(&wt, &["commit", "-q", "-m", "work"]);
        let run = ctx
            .store
            .create_run(&NewRun {
                issue_id: issue.id.clone(),
                runtime: "fake".into(),
                worktree: ".worktrees/asf-1".into(),
                branch: "feat/asf-1".into(),
                base_sha: base,
                budget_usd: Some(5.0),
            })
            .unwrap();
        Fx { dir, ctx, issue, run }
    }

    fn fake_path(fx: &Fx) -> PathBuf {
        fx.dir.path().join(FACTORY_DIR).join("fake-runtime.json")
    }

    fn worker(fx: &Fx) -> Worker<'_> {
        Worker {
            ctx: &fx.ctx,
            rt: Box::new(FakeRuntime::new(fake_path(fx))),
            vcs: GitCli,
        }
    }

    fn reviews(fx: &Fx) -> Vec<Issue> {
        fx.ctx
            .store
            .list_issues(&IssueFilter {
                kind: Some(IssueType::Review),
                ..IssueFilter::default()
            })
            .unwrap()
    }

    #[test]
    fn start_review_twice_reuses_the_review_issue_and_its_detached_worktree() {
        let fx = fixture();
        let root = fx.dir.path();
        let w = worker(&fx);
        let tip = git(root, &["rev-parse", "feat/asf-1"]);

        let first = w.start_review(&fx.issue, &fx.run).unwrap();
        let review = first.run.issue_id.clone();
        assert_eq!(first.run.state, RunState::Working);

        let rs = reviews(&fx);
        assert_eq!(rs.len(), 1);
        let r = &rs[0];
        assert_eq!(r.id, review);
        assert_eq!(r.title, "Review asf-1: add greeting");
        assert_eq!(r.parent_id.as_ref(), Some(&fx.issue.id));
        assert_eq!(r.file_scope, fx.issue.file_scope);
        assert_eq!(r.status, Status::InProgress);

        // detached worktree at the branch tip; the branch stays checked out in its own worktree
        let wt = root.join(".worktrees").join(review.as_str());
        assert_eq!(git(&wt, &["rev-parse", "HEAD"]), tip);
        assert!(
            Command::new("git")
                .args(["symbolic-ref", "-q", "HEAD"])
                .current_dir(&wt)
                .status()
                .unwrap()
                .code()
                != Some(0),
            "HEAD must be detached"
        );
        assert_eq!(
            git(
                &root.join(".worktrees").join("asf-1"),
                &["rev-parse", "--abbrev-ref", "HEAD"]
            ),
            "feat/asf-1"
        );

        // brief, read-only settings and the model for the reviewer
        let runs_dir = wt.join(FACTORY_DIR).join(RUNS_DIR).join(review.as_str());
        let brief = std::fs::read_to_string(runs_dir.join("brief.md")).unwrap();
        assert!(
            brief.contains("# Review asf-1: add greeting") && brief.contains("cargo fmt --check"),
            "{brief}"
        );
        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(runs_dir.join("settings.json")).unwrap()).unwrap();
        let strs = |k: &str| -> Vec<String> {
            settings["permissions"][k]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect()
        };
        let allow = strs("allow");
        for t in [
            "Read",
            "Grep",
            "Glob",
            "Bash(git diff *)",
            "Bash(git log *)",
            "Bash(asf *)",
            "Bash(cargo fmt --check)",
        ] {
            assert!(allow.iter().any(|a| a == t), "{t} missing in {allow:?}");
        }
        assert!(
            !allow
                .iter()
                .any(|a| a.starts_with("Edit") || a.starts_with("Write")),
            "{allow:?}"
        );
        let deny = strs("deny");
        for t in ["Edit", "Write", "MultiEdit", "NotebookEdit"] {
            assert!(deny.iter().any(|d| d == t), "{t} missing in {deny:?}");
        }
        let log = FakeRuntime::new(fake_path(&fx)).log();
        let start = log.iter().find(|e| e.op == "start_agent").unwrap();
        let args: Vec<&str> = start.args["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap())
            .collect();
        assert!(args.windows(2).any(|p| p == ["--model", "opus"]), "{args:?}");

        // second call: same issue, same worktree, same (still active) run, no second agent
        let second = w.start_review(&fx.issue, &fx.run).unwrap();
        assert_eq!(second.run.id, first.run.id);
        assert_eq!(reviews(&fx).len(), 1);
        assert_eq!(
            git(root, &["worktree", "list", "--porcelain"])
                .matches("worktree ")
                .count(),
            3
        );
        let log = FakeRuntime::new(fake_path(&fx)).log();
        assert_eq!(log.iter().filter(|e| e.op == "start_agent").count(), 1);
    }

    #[test]
    fn start_review_after_an_aborted_review_run_starts_a_new_run_on_the_same_issue() {
        let fx = fixture();
        let w = worker(&fx);
        let first = w.start_review(&fx.issue, &fx.run).unwrap();
        w.abort(&first.run.issue_id).unwrap();
        let again = w.start_review(&fx.issue, &fx.run).unwrap();
        assert_eq!(again.run.issue_id, first.run.issue_id);
        assert_ne!(again.run.id, first.run.id);
        assert_eq!(again.run.state, RunState::Working);
        assert_eq!(reviews(&fx).len(), 1);
    }
}
