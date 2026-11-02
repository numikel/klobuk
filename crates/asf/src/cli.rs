//! clap definitions. Every subcommand honours the global `--json`, `--repo`, `--lane`, `-q`.
//!
//! `IssueId`, `Status`, `IssueType` and `CommentType` parse through their `FromStr`, so a bad value
//! is a clap usage error (exit 2) before any store access.

use std::path::PathBuf;

use asf_core::{CommentType, IssueId, IssueType, Status};
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
}

#[derive(Subcommand, Debug)]
pub enum RepoCmd {
    /// Create `.factory/` (config + database) and register the repository.
    Init(InitArgs),
    /// Counts per status, active claims, next id.
    Status,
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
    /// Post a typed comment on an issue.
    Comment(CommentArgs),
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
