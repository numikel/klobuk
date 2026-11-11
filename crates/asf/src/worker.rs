//! Worker orchestration: start a run in a worktree + runtime workspace, watch it, verify it, abort it.
//!
//! Completion is never inferred from the agent's screen state: a run is verified only after the
//! worker ran `asf issue finish` (issue `in-review`), and only the orchestrator decides `succeeded`.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

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
/// Extra submit-key presses a kick-off gets, after a full wait, before the start gives up. The
/// submission inside `agent prompt` counts as the first press; Herdr may report its effect up to
/// ~90 s late, so a second key must not follow while that one may still be landing.
const KICKOFF_ENTERS: usize = 1;
/// Characters of the kick-off line that identify it on the pane text.
const KICKOFF_PROBE_CHARS: usize = 40;
/// How long `agent start` is retried while Herdr says the pane's shell is not ready yet (capped by
/// `start_timeout_ms`), and how often.
const PANE_BUSY_WAIT_MS: u64 = 15_000;
const PANE_BUSY_POLL_MS: u64 = 500;

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
    /// Claude Code's session display name (`--name`); the Herdr agent name stays the issue id.
    session_name: String,
    settings_path: PathBuf,
    model: Option<String>,
    kick: String,
    /// A reviewer, not a worker. After a startup-detection timeout whose agent never came up, a worker
    /// starts once more in a fresh workspace; a reviewer keeps the single attempt, and when it fails the
    /// run and the review issue are closed (see `start_failed`), because the merge queue starts a fresh
    /// reviewer on its next tick.
    reviewer: bool,
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

/// Where the gates of a verification or of a merge step run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateMode {
    /// In the calling thread: the call returns when the gates are finished (one-shot commands).
    Inline,
    /// On the `GateSlot` of `asf serve`: the call starts the gate run and returns at once; a later tick
    /// concludes it. Store writes stay on the tick thread either way.
    Background,
}

/// A worker's verification waiting for its gates (`GateWhen::Verify`, in the run's worktree).
pub struct VerifyJob {
    /// The run as it was before the verification set it `verifying` (the transition starts there).
    pub run: Run,
    pub issue: Issue,
    pub wt: PathBuf,
    /// What the cheap checks before the gates found; the gates add their own.
    pub problems: Vec<String>,
    pub changed: usize,
}

/// A rebased merge-queue item waiting for its gates (`GateWhen::Merge`, in its worktree).
pub struct MergeJob {
    pub item: MergeItem,
    pub run: Option<Run>,
    pub wt: PathBuf,
}

/// The check of the default branch after `item` was merged (`GateWhen::Main`, in the repository root).
/// `covered` are further merged items whose check was still pending: one run answers for all of them.
pub struct MainJob {
    pub item: MergeItem,
    pub head: String,
    pub covered: Vec<IssueId>,
}

/// What a gate run is for, with what the tick needs to conclude it once the gates are done.
pub enum Job {
    Verify(VerifyJob),
    Merge(MergeJob),
    Main(MainJob),
}

impl Job {
    pub fn kind(&self) -> &'static str {
        match self {
            Job::Verify(_) => "verify",
            Job::Merge(_) => "merge",
            Job::Main(_) => "main",
        }
    }

    pub fn issue(&self) -> &IssueId {
        match self {
            Job::Verify(j) => &j.run.issue_id,
            Job::Merge(j) => &j.item.issue_id,
            Job::Main(j) => &j.item.issue_id,
        }
    }
}

/// A gate run that is over: the job and what its gates reported (every gate that ran, the failed one last).
pub struct Finished {
    pub job: Job,
    pub results: Vec<GateResult>,
}

/// The gate run in flight, as the tick report shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GateRun {
    /// `verify`, `merge` or `main`.
    pub kind: &'static str,
    pub issue: IssueId,
    /// The gate being run right now.
    pub gate: String,
    pub seconds: u64,
}

struct Running {
    job: Job,
    since: Instant,
    gate: Arc<Mutex<String>>,
    results: mpsc::Receiver<Vec<GateResult>>,
}

/// The one place where a gate run can be in flight while `asf serve` goes on ticking. Gate runs are
/// heavy (a full `cargo test` fights over the linker and the antivirus), so there is room for one.
///
/// The thread behind a run executes the gate commands and nothing else: it holds no store, no `Ctx`
/// and no transaction. Everything that must be written (state, comments, events) is written by the tick
/// thread when `take_finished` hands the result over, so nothing but the thread itself lives only in
/// memory. A restart loses the thread and the store still says `gating`/`verifying`/pending, which
/// is what the recovery on the next start acts on.
#[derive(Default)]
pub struct GateSlot {
    running: Option<Running>,
    started: u64,
}

fn gate_failure(name: &str, why: String) -> GateResult {
    GateResult {
        name: name.to_string(),
        ok: false,
        exit_code: None,
        duration_ms: 0,
        tail: why,
        attempts: 0,
    }
}

/// Runs the gates in order, stopping at the first failure like `run_gates`, and tells `current` the
/// name of each gate before it starts.
fn run_gates_reporting(specs: &[GateSpec], cwd: &Path, mut current: impl FnMut(&str)) -> Vec<GateResult> {
    let mut results = Vec::new();
    for spec in specs {
        current(&spec.name);
        let one = run_gates(std::slice::from_ref(spec), cwd);
        let stop = one.last().is_some_and(|r| !r.ok);
        results.extend(one);
        if stop {
            break;
        }
    }
    results
}

impl GateSlot {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_busy(&self) -> bool {
        self.running.is_some()
    }

    /// How many gate runs were started on this slot; a tick compares it before and after to learn
    /// whether it started one.
    pub fn started(&self) -> u64 {
        self.started
    }

    /// Starts `specs` in `cwd` on a new thread. With nothing to run, or when no thread can be had, the
    /// run is over at once and comes back as `Some`; the caller concludes it like any other. The slot
    /// must be free (`is_busy`): two gate runs at a time is what this type exists to prevent.
    pub fn start(&mut self, job: Job, specs: Vec<GateSpec>, cwd: PathBuf) -> Option<Finished> {
        assert!(!self.is_busy(), "a gate run is already in flight");
        if specs.is_empty() {
            return Some(Finished {
                job,
                results: Vec::new(),
            });
        }
        let (tx, results) = mpsc::channel();
        // named before the thread exists, so the report never shows a run with no gate
        let gate = Arc::new(Mutex::new(specs[0].name.clone()));
        let shared = Arc::clone(&gate);
        let spawned = thread::Builder::new().name("asf-gates".into()).spawn(move || {
            let results = run_gates_reporting(&specs, &cwd, |name| {
                *shared.lock().unwrap_or_else(PoisonError::into_inner) = name.to_string();
            });
            let _ = tx.send(results);
        });
        if let Err(e) = spawned {
            return Some(Finished {
                job,
                results: vec![gate_failure(
                    "gate runner",
                    format!("cannot start the gate thread: {e}"),
                )],
            });
        }
        self.started += 1;
        self.running = Some(Running {
            job,
            since: Instant::now(),
            gate,
            results,
        });
        None
    }

    /// Whether the gate run in flight is a check of the default branch. It builds and tests the working
    /// tree of the repository root, which therefore must not change until it is over.
    pub fn checks_main(&self) -> bool {
        matches!(self.running.as_ref().map(|r| &r.job), Some(Job::Main(_)))
    }

    /// The gate run in flight, if any, once its gates are done; frees the slot.
    pub fn take_finished(&mut self) -> Option<Finished> {
        let results = match self.running.as_ref()?.results.try_recv() {
            Ok(results) => results,
            Err(TryRecvError::Empty) => return None,
            // the thread died without a word (a panic): that is a failed gate run, not a green one
            Err(TryRecvError::Disconnected) => {
                let name = self.current().map(|r| r.gate).unwrap_or_default();
                vec![gate_failure(
                    &name,
                    "the gate thread died before it reported".into(),
                )]
            }
        };
        let running = self.running.take()?;
        Some(Finished {
            job: running.job,
            results,
        })
    }

    /// Blocks until the gate run in flight is over (each gate is bounded by its own `timeout_s`) and hands
    /// it over; frees the slot. `None` when nothing is in flight. For a loop that must not leave while a
    /// gate command is still running: the command is a child of this process and would outlive it.
    pub fn wait_finished(&mut self) -> Option<Finished> {
        let results = match self.running.as_ref()?.results.recv() {
            Ok(results) => results,
            Err(_) => {
                let name = self.current().map(|r| r.gate).unwrap_or_default();
                vec![gate_failure(
                    &name,
                    "the gate thread died before it reported".into(),
                )]
            }
        };
        let running = self.running.take()?;
        Some(Finished {
            job: running.job,
            results,
        })
    }

    /// The gate run in flight: whose it is, which gate runs now and for how long the run has been going.
    pub fn current(&self) -> Option<GateRun> {
        let r = self.running.as_ref()?;
        Some(GateRun {
            kind: r.job.kind(),
            issue: r.job.issue().clone(),
            gate: r.gate.lock().unwrap_or_else(PoisonError::into_inner).clone(),
            seconds: r.since.elapsed().as_secs(),
        })
    }
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
/// (`MultiEdit` is gone from Claude Code; a deny rule for a missing tool only prints a warning.)
#[allow(dead_code)] // wired into the merge flow by asf-21
const REVIEW_DENY: [&str; 3] = ["Edit", "Write", "NotebookEdit"];

/// Longest value passed to `claude --name`.
const SESSION_NAME_MAX: usize = 60;

/// Display name for a Claude Code session (`claude --name`): `prefix`, a hyphen and a slug of `title`.
/// The runtime types the arguments into a shell, so the result has no spaces, quotes or other
/// shell-special characters: the slug is lowercase ASCII letters and digits with single hyphens
/// (every other character, non-ASCII included, is a separator) and the whole value is at most
/// `SESSION_NAME_MAX` characters. A title without ASCII letters or digits yields just `prefix`.
fn session_name(prefix: &str, title: &str) -> String {
    let mut slug = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    // The slug is ASCII, so cutting at a byte offset cannot split a character.
    slug.truncate(SESSION_NAME_MAX.saturating_sub(prefix.len() + 1));
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}-{slug}")
    }
}

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

/// Lines of wrapped question text read after the `SCOPE:` line while looking for `because`.
const SCOPE_WRAP_LINES: usize = 3;

/// Lines of the live pane read to see where the cursor of a `SCOPE:` dialog is when Enter is pressed.
const SCOPE_LIVE_LINES: u32 = 40;

/// True when the option-1 line of the screen carries the selection cursor (`❯` or `>`), i.e. Enter
/// would pick it.
fn first_option_has_cursor(screen: &str) -> bool {
    screen.lines().any(|l| {
        let option = l.trim_start_matches(|c: char| !c.is_alphanumeric());
        option.starts_with("1.") && l[..l.len() - option.len()].contains(['❯', '>'])
    })
}

/// Splits a path list (`a, b` or `a b`) and strips the quotes a worker may put around each path.
fn split_globs(list: &str) -> Vec<String> {
    list.split(|c: char| c.is_whitespace() || c == ',')
        .map(|p| p.trim_matches(['`', '"', '\'']))
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect()
}

/// A glob a one-key grant may add: inside the repo (no `..`, no absolute path or drive) and not a
/// wildcard that covers everything. Anything wider is the operator's call through `asf issue scope add`.
fn grantable_glob(g: &str) -> bool {
    let absolute = g.starts_with(['/', '\\']) || g.as_bytes().get(1) == Some(&b':');
    let climbs = g.split(['/', '\\']).any(|part| part == "..");
    let everything = g.chars().all(|c| matches!(c, '*' | '/'));
    !absolute && !climbs && !everything
}

/// The paths of a scope request in a BLOCKER body that quotes a worker's AskUserQuestion screen: the
/// question starts with `SCOPE: <path> because <reason>` (it may wrap onto the next lines) and option 1
/// reads `Grant <path>`. The dialog is worker-written text, so it must agree with itself: one `SCOPE:`
/// line, one option 1, the same paths in both, each of them grantable. `None` otherwise (including a
/// scope dialog whose first option is not the grant), so "1" never extends a scope by accident.
fn scope_dialog_grant(body: &str) -> Option<Vec<String>> {
    const MARK: &str = "SCOPE:";
    let lines: Vec<&str> = body.lines().collect();
    // the cursor / box characters before the option number are not part of the option
    let mut options = lines.iter().filter_map(|l| {
        l.trim_start_matches(|c: char| !c.is_alphanumeric())
            .strip_prefix("1.")
    });
    let option = options.next()?.trim_start();
    if options.next().is_some() {
        return None;
    }
    let granted = split_globs(option.strip_prefix("Grant")?);
    let mut marks = lines.iter().enumerate().filter(|(_, l)| l.contains(MARK));
    let (at, line) = marks.next()?;
    if marks.next().is_some() {
        return None;
    }
    let mut text = line[line.find(MARK)? + MARK.len()..].trim().to_string();
    for next in lines[at + 1..].iter().take(SCOPE_WRAP_LINES) {
        let next = next.trim();
        if text.contains("because") || next.is_empty() || next.starts_with(|c: char| c.is_ascii_digit()) {
            break;
        }
        text.push(' ');
        text.push_str(next);
    }
    let (paths, _reason) = text.split_once("because")?;
    let globs = split_globs(paths);
    (!globs.is_empty() && globs == granted && globs.iter().all(|g| grantable_glob(g))).then_some(globs)
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
                session_name: session_name(id.as_str(), &issue.title),
                settings_path,
                model: self.ctx.config.worker.model.clone(),
                kick: format!("Start {id}: read {FACTORY_DIR}/{RUNS_DIR}/{id}/brief.md and follow it."),
                reviewer: false,
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
        let give_up =
            |run: &Run, handle: &AgentHandle, e: RuntimeError| self.start_failed(l, run, handle, e, fail_run);
        let mut trusted = false;
        let _up = match self.start_agent_patiently(&start) {
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
                            .start_and_await(&start, &handle)
                            .map_err(|e| give_up(&run, &handle, e))?,
                    }
                } else {
                    return Err(give_up(&run, &handle, RuntimeError::NotReady(msg)));
                }
            }
            Err(RuntimeError::Timeout(msg)) => match self.await_started(&handle, msg) {
                Ok(a) => a,
                Err(first) if !l.reviewer => {
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
                    self.start_and_await(&start, &handle)
                        .map_err(|e| give_up(&run, &handle, e))?
                }
                Err(e) => return Err(give_up(&run, &handle, e)),
            },
            Err(e) => return Err(give_up(&run, &handle, e)),
        };
        let agent = self
            .send_kickoff(&handle, &l.kick)
            .map_err(|e| give_up(&run, &handle, e))?;
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
            "--name".to_string(),
            l.session_name.clone(),
        ];
        if self.ctx.config.worker.remote_control {
            args.push("--remote-control".into());
            args.push(l.session_name.clone());
        }
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

    /// `start_agent`, and when Herdr's startup detection times out, a re-read of the agent instead of
    /// a verdict: it may be up and ready (`await_started`).
    fn start_and_await(&self, start: &StartAgent, handle: &AgentHandle) -> Result<AgentInfo, RuntimeError> {
        match self.start_agent_patiently(start) {
            Err(RuntimeError::Timeout(msg)) => self.await_started(handle, msg),
            other => other,
        }
    }

    /// `start_agent`, polled while Herdr refuses it because the shell of the fresh pane is not ready
    /// yet (`agent_pane_busy`, seen under load right after `workspace create`). The polling lasts at
    /// most `PANE_BUSY_WAIT_MS` and never longer than `start_timeout_ms`; any other outcome, or a pane
    /// that is still busy at the end, is returned as it came.
    fn start_agent_patiently(&self, start: &StartAgent) -> Result<AgentInfo, RuntimeError> {
        let budget = self.ctx.config.worker.start_timeout_ms.min(PANE_BUSY_WAIT_MS);
        let deadline = std::time::Instant::now() + Duration::from_millis(budget);
        loop {
            match self.rt.start_agent(start) {
                Err(e) if asf_runtime::herdr::is_pane_busy(&e) => {
                    let left = deadline.saturating_duration_since(std::time::Instant::now());
                    if left.is_zero() {
                        return Err(e);
                    }
                    std::thread::sleep(left.min(Duration::from_millis(PANE_BUSY_POLL_MS)));
                }
                other => return other,
            }
        }
    }

    /// Types the kick-off prompt and makes sure Claude took it. `agent prompt --wait` has two ways to
    /// end without seeing the agent work: `agent_prompt_stalled` (no `working`/`blocked` within 5 s of
    /// the submission) and `timeout` (its own clock ran out). In both the text may sit in Claude's input
    /// box with the Enter swallowed, so neither counts as delivered: `ensure_submitted` looks at the
    /// agent and presses the submit key. The prompt itself is never typed twice.
    fn send_kickoff(&self, handle: &AgentHandle, text: &str) -> Result<AgentInfo, RuntimeError> {
        match self.rt.prompt(
            handle,
            text,
            &[AgentState::Working],
            self.ctx.config.worker.prompt_timeout_ms,
        ) {
            Ok(a) => Ok(a),
            Err(RuntimeError::Timeout(_) | RuntimeError::Stalled(_)) => self.ensure_submitted(handle, text),
            Err(e) => Err(e),
        }
    }

    /// After a kick-off that Herdr could not see start. The runtime reports a state change late (up to
    /// ~90 s observed live), so the first thing to do is wait, for as long as the start itself may take
    /// (`start_timeout_ms`), for the agent to turn `working`, `blocked` (a dialog took over, the watch
    /// loop answers it) or `done`. Only an agent that is still `idle` after that, with the kick-off line
    /// still on the pane, gets the submit key, once (`KICKOFF_ENTERS`), followed by one more full wait.
    /// `unknown` gets no key at all: Enter on a dialog Herdr cannot classify could answer it with its
    /// default. Whatever is left ends in a `Timeout` naming the last state seen, which becomes the
    /// BLOCKER text.
    fn ensure_submitted(&self, handle: &AgentHandle, text: &str) -> Result<AgentInfo, RuntimeError> {
        let window = self.ctx.config.worker.start_timeout_ms;
        let taken = [AgentState::Working, AgentState::Blocked, AgentState::Done];
        let mut pressed = 0;
        loop {
            match self.rt.wait(handle, &taken, window) {
                Ok(a) if taken.contains(&a.state) => return Ok(a),
                Ok(_) | Err(RuntimeError::Timeout(_)) => {}
                Err(e) => return Err(e),
            }
            let info = self.rt.info(handle)?;
            if taken.contains(&info.state) {
                return Ok(info);
            }
            if info.state != AgentState::Idle
                || pressed >= KICKOFF_ENTERS
                || !self.kickoff_in_input_box(handle, text)
            {
                return Err(RuntimeError::Timeout(format!(
                    "kick-off prompt not submitted: agent still {} after {pressed} submit key press(es)",
                    info.state
                )));
            }
            self.rt.send_keys(handle, &["enter"])?;
            pressed += 1;
        }
    }

    /// Second signal next to the agent state: is the kick-off line still on the pane? A line that is
    /// gone was submitted or cleared, so another Enter has nothing to submit. The pane text keeps the
    /// line after a submission too (it moves into the history), so this can only withhold the key,
    /// never prove a success. An unreadable or blank pane counts as "still there": the key is pressed.
    fn kickoff_in_input_box(&self, handle: &AgentHandle, text: &str) -> bool {
        let squash = |t: &str| t.split_whitespace().collect::<Vec<_>>().join(" ");
        let Ok(screen) = self.rt.read(handle, 60) else {
            return true;
        };
        let screen = squash(&screen);
        let probe: String = squash(text).chars().take(KICKOFF_PROBE_CHARS).collect();
        screen.is_empty() || screen.contains(&probe)
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
                session_name: session_name(&format!("review-{}", issue.id), &issue.title),
                settings_path,
                model: Some(self.ctx.config.review.model.clone()),
                kick: format!("Start {rid}: read {FACTORY_DIR}/{RUNS_DIR}/{rid}/brief.md and follow it."),
                reviewer: true,
            },
            &fail_run,
        )
    }

    /// Startup of `l`'s agent failed for good. A worker is blocked (`block_start`). A reviewer is not
    /// worth a human's attention yet: its workspace is closed, its run `failed`, its claim released and
    /// its review issue `cancelled`, so no slot stays taken and no BLOCKER is posted; the merge queue
    /// decides whether to start a fresh reviewer.
    fn start_failed(
        &self,
        l: &Launch<'_>,
        run: &Run,
        handle: &AgentHandle,
        e: RuntimeError,
        fail_run: &dyn Fn(AppError) -> AppError,
    ) -> AppError {
        if !l.reviewer {
            return self.block_start(run, handle, e);
        }
        let _ = self.rt.close_workspace(&handle.workspace_id);
        let err = fail_run(AppError::new(
            ErrorCode::Blocked,
            format!("reviewer {} could not start: {e}", run.issue_id),
        ));
        let _ = self
            .ctx
            .store
            .set_status(&run.issue_id, Status::Cancelled, LANE_ORCHESTRATOR);
        err
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

    /// Sends a worker whose run already `succeeded` back to work. The run is over (verified, queued,
    /// reviewed) but its agent may still sit idle in the pane; when it can be reached, it gets `text`
    /// and the same run is reopened as `working`, so the normal `asf issue finish` -> verify path
    /// applies again. The run is reopened, not replaced, because the pane's environment carries its
    /// `ASF_RUN`: the hooks (budget, scope, cost) keep resolving this run. `why` becomes its exit reason
    /// until the next terminal state replaces it.
    ///
    /// Errors (nothing is written to the store then): the latest run is not `succeeded`, it has no
    /// pane, the agent is gone, or it waits on a dialog (`blocked`) and a prompt would answer that
    /// dialog instead of reaching Claude.
    pub fn send_back(&self, id: &IssueId, text: &str, why: &str) -> Result<Run, AppError> {
        let run = self.run_for(id)?;
        if run.state != RunState::Succeeded {
            return Err(AppError::conflict(format!(
                "run {} is {}, only a succeeded run is sent back",
                run.id, run.state
            )));
        }
        let h = Self::handle(&run)?;
        let info = self.rt.info(&h).map_err(rt_err)?;
        if matches!(info.state, AgentState::Blocked | AgentState::Unknown) {
            return Err(AppError::new(
                ErrorCode::Blocked,
                format!("the agent of {id} is {} and cannot take a prompt", info.state),
            ));
        }
        match self.rt.prompt(
            &h,
            text,
            &[AgentState::Working],
            self.ctx.config.worker.prompt_timeout_ms,
        ) {
            Ok(_) | Err(RuntimeError::Timeout(_)) => {}
            // The text can sit in Claude's input box with Enter swallowed (see `launch`): press it once.
            Err(RuntimeError::Stalled(_)) => self.rt.send_keys(&h, &["enter"]).map_err(rt_err)?,
            Err(e) => return Err(rt_err(e)),
        }
        Ok(self.ctx.store.update_run(
            &run.id,
            &RunPatch {
                state: Some(RunState::Working),
                exit_reason: Some(why.to_string()),
                last_seen_at: Some(now_rfc3339()),
                ..RunPatch::default()
            },
        )?)
    }

    /// Sends keys to the worker's pane. When the run waits on a `SCOPE:` dialog and the key picks its
    /// first option (`Grant <path>`: the key `1`, or Enter while the cursor is on that option), the scope
    /// is extended first with the code of `asf issue scope add` (which also answers the `SCOPE:`
    /// QUESTION), so the worker's hook lets the file through the moment the dialog closes. Any other
    /// key, or a dialog that is not a scope request, only sends the keys.
    pub fn keys(&self, id: &IssueId, keys: &[String]) -> Result<Run, AppError> {
        let run = self.run_for(id)?;
        let h = Self::handle(&run)?;
        if let Some(globs) = self.granted_scope(&run, &h, keys)? {
            crate::commands::issue::scope(
                self.ctx,
                &crate::cli::ScopeCmd::Add {
                    id: run.issue_id.clone(),
                    globs,
                },
            )?;
        }
        let ks: Vec<&str> = keys.iter().map(String::as_str).collect();
        self.rt.send_keys(&h, &ks).map_err(rt_err)?;
        self.unblock(&run)
    }

    /// The paths a `SCOPE:` dialog asks for when `keys` answers it with the grant and the run is blocked
    /// on that dialog. The dialog is read from the newest BLOCKER the orchestrator posted for the run: a
    /// comment by anyone else could be a worker faking a dialog to get a grant for the human's next key.
    /// "1" picks the grant by itself. Enter picks whatever is highlighted, and the BLOCKER is only a
    /// snapshot (the human may have moved the cursor with Down since), so the live pane must show the
    /// same dialog with the cursor on option 1; a pane that cannot be read or shows anything else is
    /// "not a grant".
    fn granted_scope(
        &self,
        run: &Run,
        h: &AgentHandle,
        keys: &[String],
    ) -> Result<Option<Vec<String>>, AppError> {
        let enter = keys == ["enter"];
        if run.state != RunState::Blocked || !(enter || keys == ["1"]) {
            return Ok(None);
        }
        let comments = self.ctx.store.comments(&run.issue_id)?;
        let Some(globs) = comments
            .iter()
            .rev()
            .find(|c| c.kind == CommentType::Blocker)
            .filter(|c| c.author == LANE_ORCHESTRATOR)
            .and_then(|c| scope_dialog_grant(&c.body))
        else {
            return Ok(None);
        };
        if enter {
            let live = self.rt.read(h, SCOPE_LIVE_LINES).unwrap_or_default();
            if scope_dialog_grant(&live).as_ref() != Some(&globs) || !first_option_has_cursor(&live) {
                return Ok(None);
            }
        }
        Ok(Some(globs))
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

    /// One pass over the active runs; the runs that wait for verification are verified (gates and all)
    /// before it returns.
    pub fn watch(&self) -> Result<WatchResult, AppError> {
        self.watch_in(GateMode::Inline)
    }

    /// `watch` in either mode. In `Background` mode the pass only looks at the agents (blocked, working,
    /// heartbeat): runs that wait for verification are left alone, because verifying them takes a gate
    /// run and the tick must not wait for one. `start_verification` hands them to the gate slot.
    pub fn watch_in(&self, mode: GateMode) -> Result<WatchResult, AppError> {
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
                if mode == GateMode::Inline {
                    let t = self.verify(run, &issue)?;
                    transitions.push(t);
                }
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
        let again = if mode == GateMode::Inline {
            self.finished_again()?
        } else {
            Vec::new()
        };
        for (run, issue) in &again {
            transitions.push(self.verify(run, issue)?);
        }
        Ok(WatchResult {
            checked: active.len() + again.len(),
            transitions,
        })
    }

    /// The run that waits longest for its verification: an active run whose worker has finished
    /// (issue `in-review`, which includes a run a stopped `asf serve` left `verifying`), else a failed
    /// run whose worker has finished again.
    fn next_to_verify(&self) -> Result<Option<(Run, Issue)>, AppError> {
        let store = &self.ctx.store;
        for run in store.runs(None, true)? {
            if run.state == RunState::Starting {
                continue;
            }
            let Some(issue) = store.get_issue(&run.issue_id)? else {
                continue;
            };
            if issue.status == Status::InReview {
                return Ok(Some((run, issue)));
            }
        }
        Ok(self.finished_again()?.into_iter().next())
    }

    /// Hands the next run that waits for verification to the gate slot (if it is free): the cheap checks
    /// run now, the gates in the background. A verification with no gates to run is over at once, and
    /// the next run gets its turn. Returns the verifications that ended in this call.
    pub fn start_verification(&self, slot: &mut GateSlot) -> Result<Vec<Transition>, AppError> {
        let mut ended = Vec::new();
        while !slot.is_busy() {
            let Some((run, issue)) = self.next_to_verify()? else {
                break;
            };
            let job = self.verify_prepare(&run, &issue)?;
            let wt = job.wt.clone();
            let Some(finished) = slot.start(Job::Verify(job), self.verify_specs(), wt) else {
                break;
            };
            let Job::Verify(job) = finished.job else {
                unreachable!("the slot hands back the job it was given");
            };
            ended.push(self.verify_conclude(job, finished.results)?);
        }
        Ok(ended)
    }

    /// The end of a verification that ran on the gate slot. `None` when the run is no longer
    /// `verifying` (the operator aborted it while the gates ran): its result belongs to nobody.
    pub fn conclude_verification(
        &self,
        job: VerifyJob,
        results: Vec<GateResult>,
    ) -> Result<Option<Transition>, AppError> {
        let still_verifying = self
            .ctx
            .store
            .get_run(&job.run.id)?
            .is_some_and(|r| r.state == RunState::Verifying);
        if !still_verifying {
            return Ok(None);
        }
        self.verify_conclude(job, results).map(Some)
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

    /// Independent verification after the worker's finish ritual, gates included, in this thread.
    fn verify(&self, run: &Run, issue: &Issue) -> Result<Transition, AppError> {
        let job = self.verify_prepare(run, issue)?;
        let results = run_gates(&self.verify_specs(), &job.wt);
        self.verify_conclude(job, results)
    }

    fn verify_specs(&self) -> Vec<GateSpec> {
        stage_specs(&self.ctx.config.gates, GateWhen::Verify)
    }

    /// The cheap part of a verification: the run goes `verifying`, then the report, the worktree and the
    /// scope are checked. What the checks found travels with the job; the gates come next.
    fn verify_prepare(&self, run: &Run, issue: &Issue) -> Result<VerifyJob, AppError> {
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
        // Against the default branch, not `run.base_sha`: `changed_files` takes the merge-base with HEAD, and
        // after the merge queue rebased the branch the stored base predates that rebase, so main's own
        // commits would be counted as the worker's.
        let changed = self
            .vcs
            .changed_files(&wt_abs, &self.ctx.config.repo.default_branch)
            .map_err(vcs_err)?;
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
        Ok(VerifyJob {
            run: run.clone(),
            issue: issue.clone(),
            wt: wt_abs,
            problems,
            changed: changed.len(),
        })
    }

    /// The end of a verification: the gates' verdict joins the problems of the cheap checks, and the
    /// run is `succeeded` (and queued for merge) or `failed` (with a BLOCKER).
    fn verify_conclude(&self, job: VerifyJob, results: Vec<GateResult>) -> Result<Transition, AppError> {
        let VerifyJob {
            run,
            issue,
            mut problems,
            changed,
            ..
        } = job;
        let (run, issue) = (&run, &issue);
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
            // reused: a second `enqueue_merge` would be a conflict that aborts the whole tick. An item
            // that `failed` is the one a reviewer's `changes` verdict left behind while the worker fixed
            // the findings: the worker is done and verified, so it is queued again without a command.
            let existing = self
                .ctx
                .store
                .merge_queue()?
                .into_iter()
                .find(|m| m.issue_id == run.issue_id && !m.state.is_terminal());
            match existing {
                None => {
                    self.ctx
                        .store
                        .enqueue_merge(&run.issue_id, Some(&run.id), &run.branch)?;
                }
                Some(m) if m.state == MergeState::Failed => {
                    self.ctx.store.update_merge(
                        m.id,
                        &MergePatch {
                            state: Some(MergeState::Queued),
                            last_error: Some(None),
                            ..MergePatch::default()
                        },
                    )?;
                    self.ctx.store.append_event(&NewEvent {
                        kind: "merge.requeued".into(),
                        issue_id: Some(run.issue_id.clone()),
                        run_id: Some(run.id.clone()),
                        actor: Some(LANE_ORCHESTRATOR.into()),
                        payload: json!({ "reason": "worker_finished_again", "branch": m.branch }),
                    })?;
                }
                Some(_) => {}
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
                detail: format!("{changed} files, {} gates", results.len()),
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

    fn blocker_with(screen: &str) -> String {
        format!("Worker for asf-1 is waiting for input.\n\nScreen:\n```\n{screen}\n```\nAnswer with `asf run keys asf-1 y`.")
    }

    #[test]
    fn scope_dialog_grant_reads_the_paths_of_a_dialog_whose_first_option_is_the_grant() {
        let one = blocker_with(" ☐ Scope\nSCOPE: Cargo.toml because a new dependency is needed\n ❯ 1. Grant Cargo.toml\n   2. Refuse");
        assert_eq!(scope_dialog_grant(&one), Some(vec!["Cargo.toml".to_string()]));
        let many = blocker_with(
            "SCOPE: `src/a.rs`, \"src/b/**\" because x\n  1. Grant src/a.rs, src/b/**\n  2. Refuse",
        );
        assert_eq!(
            scope_dialog_grant(&many),
            Some(vec!["src/a.rs".to_string(), "src/b/**".to_string()])
        );
    }

    #[test]
    fn scope_dialog_grant_joins_a_wrapped_question() {
        let wrapped = blocker_with("SCOPE: crates/asf/tests/cli_run.rs\nbecause the flow test lives there\n 1. Grant crates/asf/tests/cli_run.rs\n 2. Refuse");
        assert_eq!(
            scope_dialog_grant(&wrapped),
            Some(vec!["crates/asf/tests/cli_run.rs".to_string()])
        );
    }

    #[test]
    fn scope_dialog_grant_refuses_anything_that_is_not_a_grant_first_scope_dialog() {
        // a permission prompt
        assert_eq!(
            scope_dialog_grant(&blocker_with("Allow Bash(rm -rf target)?\n 1. Yes\n 2. No")),
            None
        );
        // a scope question whose option 1 is not the grant
        assert_eq!(
            scope_dialog_grant(&blocker_with(
                "SCOPE: Cargo.toml because x\n 1. Refuse\n 2. Grant Cargo.toml"
            )),
            None
        );
        // no reason, so no way to tell where the paths end
        assert_eq!(
            scope_dialog_grant(&blocker_with(
                "SCOPE: Cargo.toml\n\n 1. Grant Cargo.toml\n 2. Refuse"
            )),
            None
        );
        // no path at all
        assert_eq!(
            scope_dialog_grant(&blocker_with("SCOPE: because x\n 1. Grant\n 2. Refuse")),
            None
        );
    }

    #[test]
    fn first_option_has_cursor_sees_the_marker_only_on_option_one() {
        assert!(first_option_has_cursor(" ❯ 1. Grant a.rs\n   2. Refuse"));
        assert!(first_option_has_cursor("> 1. Grant a.rs\n  2. Refuse"));
        assert!(!first_option_has_cursor("   1. Grant a.rs\n ❯ 2. Refuse"));
        assert!(!first_option_has_cursor("   1. Grant a.rs\n   2. Refuse"));
        assert!(
            !first_option_has_cursor("a > b\n 11. Grant a.rs"),
            "a > elsewhere is no cursor"
        );
    }

    #[test]
    fn scope_dialog_grant_refuses_a_dialog_that_disagrees_with_itself_or_asks_too_much() {
        // the question says one thing, option 1 (what the human reads) another
        assert_eq!(
            scope_dialog_grant(&blocker_with(
                "SCOPE: ** because x\n 1. Grant Cargo.toml\n 2. Refuse"
            )),
            None
        );
        // two questions or two first options on one screen
        assert_eq!(
            scope_dialog_grant(&blocker_with(
                "SCOPE: a.rs because x\nSCOPE: ** because y\n 1. Grant a.rs\n 2. Refuse"
            )),
            None
        );
        assert_eq!(
            scope_dialog_grant(&blocker_with(
                "SCOPE: a.rs because x\n 1. Grant a.rs\n 1. Grant a.rs"
            )),
            None
        );
        // paths that leave the repo or cover all of it are for `asf issue scope add`
        for wide in [
            "**",
            "*",
            "**/*",
            "../x.rs",
            "src/../../x",
            "/etc/passwd",
            "C:/x",
            "\\\\srv\\x",
        ] {
            let screen = format!("SCOPE: {wide} because x\n 1. Grant {wide}\n 2. Refuse");
            assert_eq!(scope_dialog_grant(&blocker_with(&screen)), None, "{wide}");
        }
        // a narrow glob is fine
        assert_eq!(
            scope_dialog_grant(&blocker_with(
                "SCOPE: src/**/*.rs because x\n 1. Grant src/**/*.rs"
            )),
            Some(vec!["src/**/*.rs".to_string()])
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
    fn session_name_is_the_prefix_and_a_slug_of_the_title() {
        assert_eq!(
            session_name("asf-27", "Add asf issue scope add/rm and scope request"),
            "asf-27-add-asf-issue-scope-add-rm-and-scope-request"
        );
        assert_eq!(
            session_name("review-asf-1", "Add Greeting"),
            "review-asf-1-add-greeting"
        );
    }

    #[test]
    fn session_name_treats_non_ascii_as_a_separator() {
        assert_eq!(session_name("asf-1", "Zażółć gęślą jaźń 2"), "asf-1-za-g-l-ja-2");
    }

    #[test]
    fn session_name_collapses_runs_of_punctuation_and_trims_the_ends() {
        assert_eq!(
            session_name("asf-1", "  --Fix `foo::bar()`!! (really) -- "),
            "asf-1-fix-foo-bar-really"
        );
    }

    #[test]
    fn session_name_is_just_the_prefix_without_ascii_alphanumerics() {
        for title in ["", "!!! --- ???", "日本語のタイトル", "  \t "] {
            assert_eq!(session_name("asf-1", title), "asf-1", "{title:?}");
        }
    }

    #[test]
    fn session_name_is_at_most_60_characters_and_never_ends_in_a_hyphen() {
        let long = "word ".repeat(40);
        let n = session_name("asf-123", &long);
        assert!(n.len() <= 60, "{n}");
        assert!(n.starts_with("asf-123-word-word"), "{n}");
        assert!(!n.ends_with('-'), "{n}");

        // The cut lands exactly on a hyphen: 5 + 1 + 53 letters fill 59, the 60th character is '-'.
        let n = session_name("asf-1", &format!("{} b", "a".repeat(53)));
        assert_eq!(n, format!("asf-1-{}", "a".repeat(53)));

        // A cut inside a word keeps the letters before it.
        let n = session_name("asf-1", &"a".repeat(100));
        assert_eq!(n.len(), 60);
        assert_eq!(n, format!("asf-1-{}", "a".repeat(54)));
    }

    #[test]
    fn session_name_has_only_lowercase_ascii_letters_digits_and_hyphens() {
        let n = session_name("asf-9", "He said \"hi\" & it's $HOME; rm -rf / ĄĘ 42");
        assert!(
            n.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "{n}"
        );
        assert!(!n.contains("--"), "{n}");
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
        for t in ["Edit", "Write", "NotebookEdit"] {
            assert!(deny.iter().any(|d| d == t), "{t} missing in {deny:?}");
        }
        // MultiEdit no longer exists in Claude Code; a deny rule for it only prints a warning.
        assert!(!deny.iter().any(|d| d == "MultiEdit"), "{deny:?}");
        let log = FakeRuntime::new(fake_path(&fx)).log();
        let start = log.iter().find(|e| e.op == "start_agent").unwrap();
        let args: Vec<&str> = start.args["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap())
            .collect();
        assert!(args.windows(2).any(|p| p == ["--model", "opus"]), "{args:?}");
        // the session is named after the reviewed issue, not after the review issue's own title
        assert!(
            args.windows(2)
                .any(|p| p == ["--name", "review-asf-1-add-greeting"]),
            "{args:?}"
        );
        assert_eq!(
            start.args["name"],
            review.as_str(),
            "the Herdr agent name is unchanged"
        );

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

    /// The `claude` arguments of the one `start_agent` call in the fake runtime's log.
    fn started_args(fx: &Fx) -> Vec<String> {
        let log = FakeRuntime::new(fake_path(fx)).log();
        let start = log.iter().find(|e| e.op == "start_agent").unwrap();
        start.args["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn a_reviewer_joins_remote_control_under_its_session_name_only_when_switched_on() {
        let mut fx = fixture();
        fx.ctx.config.worker.remote_control = true;
        worker(&fx).start_review(&fx.issue, &fx.run).unwrap();
        let args = started_args(&fx);
        let name = "review-asf-1-add-greeting";
        assert!(args.windows(2).any(|p| p == ["--name", name]), "{args:?}");
        assert!(
            args.windows(2).any(|p| p == ["--remote-control", name]),
            "{args:?}"
        );

        let fx = fixture();
        worker(&fx).start_review(&fx.issue, &fx.run).unwrap();
        let args = started_args(&fx);
        assert!(
            !args.iter().any(|a| a.starts_with("--remote-control")),
            "off by default: {args:?}"
        );
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

#[cfg(test)]
mod gate_slot_tests {
    use super::*;

    fn spec(name: &str, code: &str) -> GateSpec {
        GateSpec {
            name: name.to_string(),
            cmd: vec!["python".into(), "-c".into(), code.into()],
            timeout_s: 60,
            shell: None,
        }
    }

    fn job(id: u32) -> Job {
        Job::Merge(MergeJob {
            item: MergeItem {
                id: i64::from(id),
                issue_id: format!("asf-{id}").parse().unwrap(),
                run_id: None,
                branch: format!("feat/asf-{id}"),
                state: MergeState::Gating,
                attempts: 1,
                head_sha: None,
                rebased_onto: None,
                pr_url: None,
                gate_results: Vec::new(),
                last_error: None,
                enqueued_at: String::new(),
                updated_at: String::new(),
            },
            run: None,
            wt: PathBuf::new(),
        })
    }

    /// Polls the slot until its run is over (15 s at most).
    fn wait(slot: &mut GateSlot) -> Finished {
        let until = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(f) = slot.take_finished() {
                return f;
            }
            assert!(Instant::now() < until, "the gate run never finished");
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_run_names_the_gate_in_flight_and_hands_over_its_results() {
        let dir = tempfile::tempdir().unwrap();
        let release = dir.path().join("release");
        let hold = format!(
            "import os, time\nwhile not os.path.exists({:?}):\n    time.sleep(0.05)",
            release.to_string_lossy().replace('\\', "/")
        );
        let mut slot = GateSlot::new();
        assert!(!slot.is_busy() && slot.current().is_none());
        assert!(slot
            .start(job(4), vec![spec("hold", &hold)], dir.path().to_path_buf())
            .is_none());
        assert!(slot.is_busy());
        assert_eq!(slot.started(), 1);
        let now = slot.current().unwrap();
        assert_eq!(
            (now.kind, now.issue.to_string(), now.gate.as_str()),
            ("merge", "asf-4".into(), "hold")
        );
        assert!(slot.take_finished().is_none(), "the gate is still waiting");

        std::fs::write(&release, "").unwrap();
        let done = wait(&mut slot);
        assert_eq!(done.job.issue().to_string(), "asf-4");
        assert_eq!(done.results.len(), 1);
        assert!(done.results[0].ok, "{:?}", done.results);
        assert!(
            !slot.is_busy() && slot.current().is_none(),
            "the slot is free again"
        );
    }

    #[test]
    fn waiting_for_the_run_in_flight_hands_over_its_results_and_frees_the_slot() {
        let dir = tempfile::tempdir().unwrap();
        let mut slot = GateSlot::new();
        assert!(
            slot.wait_finished().is_none(),
            "nothing in flight, nothing to wait for"
        );
        let slow = spec("slow", "import time; time.sleep(1)");
        assert!(slot.start(job(7), vec![slow], dir.path().to_path_buf()).is_none());
        let done = slot.wait_finished().expect("the run in flight");
        assert_eq!(done.job.issue().to_string(), "asf-7");
        assert!(done.results[0].ok, "{:?}", done.results);
        assert!(!slot.is_busy());
    }

    #[test]
    fn gates_run_in_order_and_stop_at_the_first_failure() {
        let dir = tempfile::tempdir().unwrap();
        let mut slot = GateSlot::new();
        let specs = vec![
            spec("one", "pass"),
            spec("two", "import sys; sys.exit(3)"),
            spec("three", "pass"),
        ];
        assert!(slot.start(job(1), specs, dir.path().to_path_buf()).is_none());
        let done = wait(&mut slot);
        let names: Vec<_> = done.results.iter().map(|r| (r.name.as_str(), r.ok)).collect();
        assert_eq!(names, [("one", true), ("two", false)]);
        assert_eq!(done.results[1].exit_code, Some(3));
    }

    #[test]
    fn a_run_without_gates_is_over_at_once_and_takes_no_slot() {
        let mut slot = GateSlot::new();
        let done = slot
            .start(job(2), Vec::new(), PathBuf::new())
            .expect("nothing to wait for");
        assert!(done.results.is_empty());
        assert!(!slot.is_busy());
        assert_eq!(slot.started(), 0);
    }

    #[test]
    #[should_panic(expected = "already in flight")]
    fn a_second_run_is_refused_while_one_is_in_flight() {
        let dir = tempfile::tempdir().unwrap();
        let mut slot = GateSlot::new();
        let slow = spec("slow", "import time; time.sleep(3)");
        let _ = slot.start(job(1), vec![slow.clone()], dir.path().to_path_buf());
        let _ = slot.start(job(2), vec![slow], dir.path().to_path_buf());
    }
}
