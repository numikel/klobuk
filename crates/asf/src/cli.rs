//! clap definitions. Every subcommand honours the global `--json`, `--repo`, `--lane`, `-q`.

use std::path::PathBuf;

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
