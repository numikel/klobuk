//! clap definitions. Every subcommand honours the global `--json`, `--repo`, `--lane`, `-q`.
//!
//! `IssueId`, `Status`, `IssueType` and `CommentType` parse through their `FromStr`, so a bad value
//! is a clap usage error (exit 2) before any store access.

use std::path::PathBuf;

use asf_core::{CommentType, ImportMode, IssueId, IssueType, PlanFormat, Status};
use clap::{Args, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "asf",
    version,
    about = "auto-software-factory: local issue store and worker dispatcher"
)]
#[command(propagate_version = true)]
pub struct Cli {
    /// Repository root or any path inside it (default: current directory).
    #[arg(long, global = true, value_name = "PATH")]
    pub repo: Option<PathBuf>,
    /// Act as this lane (issue id, `orchestrator` or `human`). Default: env ASF_ISSUE, then worktree cwd.
    #[arg(long, global = true, value_name = "LANE")]
    pub lane: Option<String>,
    /// Machine-readable output: `{"ok":true,"result":…}` or `{"ok":false,"error":{…}}`.
    #[arg(long, global = true)]
    pub json: bool,
    /// Suppress human output (JSON output is unaffected).
    #[arg(short = 'q', long, global = true)]
    pub quiet: bool,
    #[command(subcommand)]
    pub cmd: Cmd,
}

// Parsed once per process; the size difference between variants is irrelevant here.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Repository registration and health.
    Repo {
        #[command(subcommand)]
        cmd: RepoCmd,
    },
    /// Issues, dependencies, claims and comments.
    Issue {
        #[command(subcommand)]
        cmd: IssueCmd,
    },
    /// Plans: import a markdown task list as issues.
    Plan {
        #[command(subcommand)]
        cmd: PlanCmd,
    },
    /// Lanes = active claims and their worktrees.
    Lane {
        #[command(subcommand)]
        cmd: LaneCmd,
    },
    /// Worker runs (M1: allocation plan only).
    Run {
        #[command(subcommand)]
        cmd: RunCmd,
    },
    /// Local merge queue: rebase, gates, fast-forward.
    Merge {
        #[command(subcommand)]
        cmd: MergeCmd,
    },
    /// Claude Code hooks (run inside a worker session; JSON on stdin).
    Hook {
        #[command(subcommand)]
        cmd: HookCmd,
    },
    /// Terminal UI (run it as a pane in Herdr).
    Tui(TuiArgs),
    /// Orchestrator loop: promote ready issues, watch runs, process the merge queue.
    Serve(ServeArgs),
    /// Write `.factory/issues.jsonl` (or `--out`).
    Export(ExportArgs),
    /// Load a JSONL export into this store.
    Import(ImportArgs),
}

#[derive(Args, Debug)]
pub struct TuiArgs {
    /// Render one frame as plain text and exit (no raw mode).
    #[arg(long)]
    pub once: bool,
    /// Initial view: board | agents | inbox | queue | log.
    #[arg(long)]
    pub view: Option<String>,
}

#[derive(Args, Debug)]
pub struct ServeArgs {
    /// Seconds between ticks (>= 1).
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..))]
    pub interval: u64,
    /// Run a single tick and exit.
    #[arg(long)]
    pub once: bool,
}

#[derive(Args, Debug)]
pub struct ExportArgs {
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct ImportArgs {
    pub file: PathBuf,
    /// `upsert` (match on uid, newer wins) or `replace` (wipe first).
    #[arg(long, default_value = "upsert")]
    pub mode: ImportMode,
}

#[derive(Subcommand, Debug)]
pub enum RepoCmd {
    /// Create `.factory/` (config + database) and register the repository.
    Init(InitArgs),
    /// Counts per status, active claims, next id.
    Status,
    /// Check git, config, database and optional tools (herdr, claude).
    Doctor,
}

#[derive(Args, Debug)]
pub struct InitArgs {
    /// Issue id prefix (`^[a-z][a-z0-9]{0,15}$`), default `asf`.
    #[arg(long)]
    pub prefix: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum IssueCmd {
    /// List issues (ordered by id).
    List(ListArgs),
    /// Show one issue with its dependencies; `--dossier` adds comments and dependency hand-offs.
    Show {
        id: IssueId,
        #[arg(long)]
        dossier: bool,
    },
    /// Create an issue (backlog unless `--ready`).
    Create(CreateArgs),
    /// Patch fields of an issue; omitted flags are left untouched.
    Edit(EditArgs),
    /// Promote backlog issues whose dependencies are all done to `ready`.
    Ready,
    /// Set the status directly (`backlog|ready|in-progress|blocked|in-review|done|cancelled`).
    Status { id: IssueId, status: Status },
    /// Dependency edges.
    Dep {
        #[command(subcommand)]
        cmd: DepCmd,
    },
    /// Claim a ready issue for a worker (default worker = current lane). Exit 3 if already claimed.
    Claim {
        id: IssueId,
        #[arg(long)]
        worker: Option<String>,
        #[arg(long)]
        note: Option<String>,
    },
    /// Drop the claim; `in-progress` goes back to `ready`.
    Release { id: IssueId },
    /// Worker ritual: mark `in-review` and post a STATUS comment pointing at the report file.
    Finish {
        id: IssueId,
        #[arg(long)]
        report: PathBuf,
    },
    /// Post a typed comment on an issue.
    Comment(CommentArgs),
    /// Unread comments relevant to the current lane (advances the read cursor unless `--peek`).
    Inbox {
        #[arg(long)]
        peek: bool,
        /// Ignore relevance; show every unread comment.
        #[arg(long)]
        all: bool,
    },
    /// Shorthand for `comment --type HANDOFF`.
    Handoff {
        id: IssueId,
        #[arg(long)]
        to: String,
        #[arg(long)]
        body: String,
    },
}

#[derive(Args, Debug, Default)]
pub struct ListArgs {
    #[arg(long)]
    pub status: Option<Status>,
    #[arg(long = "type")]
    pub kind: Option<IssueType>,
    #[arg(long)]
    pub label: Option<String>,
    /// Only `ready` issues (shorthand for `--status ready`).
    #[arg(long, conflicts_with = "status")]
    pub ready: bool,
}

#[derive(Args, Debug)]
pub struct CreateArgs {
    #[arg(long)]
    pub title: String,
    #[arg(long, conflicts_with = "body_file")]
    pub body: Option<String>,
    /// Read the body from a UTF-8 file.
    #[arg(long)]
    pub body_file: Option<PathBuf>,
    #[arg(long = "type", default_value = "task")]
    pub kind: IssueType,
    #[arg(long, default_value_t = 2)]
    pub priority: i64,
    #[arg(long)]
    pub parent: Option<IssueId>,
    /// File scope glob (repeatable).
    #[arg(long = "scope")]
    pub file_scope: Vec<String>,
    /// Acceptance criterion (repeatable).
    #[arg(long = "accept")]
    pub acceptance: Vec<String>,
    #[arg(long)]
    pub estimate: Option<String>,
    #[arg(long = "label")]
    pub labels: Vec<String>,
    /// Dependency (repeatable).
    #[arg(long = "dep")]
    pub deps: Vec<IssueId>,
    /// Start as `ready` (only allowed without `--dep`).
    #[arg(long)]
    pub ready: bool,
    /// Origin marker, e.g. `specs/001/tasks.md#T003`.
    #[arg(long)]
    pub source: Option<String>,
}

#[derive(Args, Debug)]
pub struct EditArgs {
    pub id: IssueId,
    #[arg(long)]
    pub title: Option<String>,
    #[arg(long)]
    pub body: Option<String>,
    #[arg(long)]
    pub priority: Option<i64>,
    /// Replace the whole file scope (repeatable).
    #[arg(long = "scope")]
    pub file_scope: Vec<String>,
    /// Replace all acceptance criteria (repeatable).
    #[arg(long = "accept")]
    pub acceptance: Vec<String>,
    /// Replace all labels (repeatable).
    #[arg(long = "label")]
    pub labels: Vec<String>,
    #[arg(long, conflicts_with = "clear_estimate")]
    pub estimate: Option<String>,
    #[arg(long)]
    pub clear_estimate: bool,
    #[arg(long, conflicts_with = "clear_parent")]
    pub parent: Option<IssueId>,
    #[arg(long)]
    pub clear_parent: bool,
}

#[derive(Subcommand, Debug)]
pub enum DepCmd {
    /// `<id>` depends on `<on>`.
    Add { id: IssueId, on: IssueId },
    /// Remove the edge `<id>` -> `<on>`.
    Rm { id: IssueId, on: IssueId },
}

#[derive(Args, Debug)]
pub struct CommentArgs {
    pub id: IssueId,
    /// QUESTION|ANSWER|DECISION|HANDOFF|BLOCKER|STATUS|FYI
    #[arg(long = "type")]
    pub kind: CommentType,
    /// Addressee lane: an issue id, `all` or `human`.
    #[arg(long, default_value = "all")]
    pub to: String,
    #[arg(long, conflicts_with = "body_file")]
    pub body: Option<String>,
    #[arg(long)]
    pub body_file: Option<PathBuf>,
    #[arg(long)]
    pub reply_to: Option<i64>,
}

#[derive(Subcommand, Debug)]
pub enum MergeCmd {
    /// Queue an issue's branch (`feat/<id>`) by hand.
    Enqueue { id: IssueId },
    /// The queue in FIFO order.
    List,
    /// Process queued items: rebase onto the default branch, run gates, fast-forward.
    Tick {
        /// Process at most one item.
        #[arg(long)]
        once: bool,
    },
    /// Re-queue a failed or blocked item.
    Retry { id: IssueId },
    /// Drop a pending item (the issue can be enqueued again later).
    Skip { id: IssueId },
}

#[derive(Subcommand, Debug)]
pub enum HookCmd {
    /// PreToolUse: deny edits outside the issue's file scope or worktree.
    PreTool,
    /// PostToolUse: count tool calls, record session, enforce max_tool_calls.
    PostTool,
    /// Stop: block once until `asf issue finish` ran.
    Stop,
    /// statusLine: record cost from Claude's estimate, enforce budget.
    Status,
}

#[derive(Subcommand, Debug)]
pub enum PlanCmd {
    /// Create an epic + one issue per `### Task N:` heading (deps, file scope and acceptance from the text).
    Import {
        file: PathBuf,
        /// Parse and print, write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Do not create a parent epic.
        #[arg(long)]
        no_epic: bool,
        /// auto | md-tasks | spec-kit
        #[arg(long, default_value = "auto")]
        format: PlanFormat,
    },
    /// Idea → PRD → issues via headless Claude (`[planner]` in config); artifacts under `.factory/plan/<slug>/`.
    New {
        /// Markdown/text file with the idea or brainstorm.
        #[arg(long)]
        from: PathBuf,
        /// Generate artifacts, create no issues.
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        no_epic: bool,
        /// Plan folder name (default: slug of the PRD title).
        #[arg(long)]
        slug: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum LaneCmd {
    /// Active claims with worktree existence.
    List,
    /// Report claims whose worktree directory is missing (`stale`).
    Check,
}

#[derive(Subcommand, Debug)]
pub enum RunCmd {
    /// Deterministic allocation plan: which ready issues can start now and why the rest wait.
    Plan,
    /// Claim the issue, create its worktree and launch a worker in the runtime (Herdr workspace).
    Start {
        id: IssueId,
        /// `inputs_hash` from `asf run plan`; refuses to start if the plan changed since.
        #[arg(long)]
        approve: Option<String>,
    },
    /// All runs (active and finished).
    List,
    /// Latest run of an issue plus the live agent state.
    Show { id: IssueId },
    /// Send a prompt to the worker (also unblocks a `blocked` run).
    Reply { id: IssueId, text: String },
    /// Send logical keys (`enter`, `esc`, `down`, `y`, `ctrl+c`, …) to the worker pane.
    Keys {
        id: IssueId,
        #[arg(required = true)]
        keys: Vec<String>,
    },
    /// Remind the worker about the finish ritual.
    Nudge { id: IssueId },
    /// Stop the worker, release the claim, close the workspace; the worktree is kept.
    Abort { id: IssueId },
    /// Poll active runs: post BLOCKERs for waiting agents, verify finished work, enqueue merges.
    Watch {
        /// One pass (the default for now; `asf serve` loops).
        #[arg(long)]
        once: bool,
    },
}
