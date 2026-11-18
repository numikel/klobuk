pub mod config;
pub mod cost;
pub mod doctor;
pub mod export;
pub mod gh;
pub mod hook;
pub mod issue;
pub mod lane;
pub mod merge;
pub mod method;
pub mod plan;
pub mod remind;
pub mod repo;
pub mod run;
pub mod serve;
pub mod tui;

use crate::cli::{Cli, Cmd, ConfigCmd, IssueCmd};
use crate::output::{AppError, Rendered};

/// Commands a worker may run from inside its pane (`KLOBUK_RUN` set): read, talk, hand off, finish.
/// Everything that claims, releases, changes status by hand, starts runs or merges is the
/// orchestrator's and is refused there, whatever the Claude Code allowlist says.
fn allowed_in_worker(cmd: &Cmd) -> bool {
    match cmd {
        // `cost` only reads the store.
        Cmd::Hook { .. } | Cmd::Lane { .. } | Cmd::Method { .. } | Cmd::Cost(_) => true,
        Cmd::Repo { cmd } => !matches!(cmd, crate::cli::RepoCmd::Init(_)),
        // Reading the settings is harmless; changing them is the operator's.
        Cmd::Config { cmd } => matches!(cmd, ConfigCmd::Show),
        Cmd::Issue { cmd } => matches!(
            cmd,
            IssueCmd::List(_)
                | IssueCmd::Show { .. }
                | IssueCmd::Dep { .. }
                | IssueCmd::Finish { .. }
                | IssueCmd::Comment(_)
                | IssueCmd::Inbox { .. }
                | IssueCmd::Handoff { .. }
        ),
        Cmd::Gh { .. }
        | Cmd::Plan { .. }
        | Cmd::Run { .. }
        | Cmd::Merge { .. }
        | Cmd::Tui(_)
        | Cmd::Serve(_)
        | Cmd::Export(_)
        | Cmd::Import(_) => false,
    }
}

fn top_level_name(cmd: &Cmd) -> &'static str {
    match cmd {
        Cmd::Repo { .. } => "repo",
        Cmd::Issue { .. } => "issue",
        Cmd::Plan { .. } => "plan",
        Cmd::Lane { .. } => "lane",
        Cmd::Method { .. } => "method",
        Cmd::Run { .. } => "run",
        Cmd::Merge { .. } => "merge",
        Cmd::Hook { .. } => "hook",
        Cmd::Gh { .. } => "gh",
        Cmd::Tui(_) => "tui",
        Cmd::Serve(_) => "serve",
        Cmd::Export(_) => "export",
        Cmd::Import(_) => "import",
        Cmd::Config { .. } => "config",
        Cmd::Cost(_) => "cost",
    }
}

pub fn dispatch(cli: &Cli) -> Result<Rendered, AppError> {
    if std::env::var_os(hook::ENV_RUN).is_some() && !allowed_in_worker(&cli.cmd) {
        return Err(AppError::forbidden(format!(
            "`klobuk {}` is an orchestrator command and is refused inside a worker run ({} is set); \
             workers report with `klobuk issue comment` and hand over with `klobuk issue finish`",
            top_level_name(&cli.cmd),
            hook::ENV_RUN
        )));
    }
    match &cli.cmd {
        Cmd::Repo { cmd } => repo::run(cli, cmd),
        Cmd::Issue { cmd } => issue::run(cli, cmd),
        Cmd::Plan { cmd } => plan::run(cli, cmd),
        Cmd::Lane { cmd } => lane::run(cli, cmd),
        Cmd::Method { cmd } => method::run(cli, cmd),
        Cmd::Run { cmd } => run::run(cli, cmd),
        Cmd::Merge { cmd } => merge::run(cli, cmd),
        Cmd::Hook { cmd } => hook::run(cli, cmd),
        Cmd::Gh { cmd } => gh::run(cli, cmd),
        Cmd::Tui(a) => tui::run(cli, a),
        Cmd::Serve(a) => serve::run(cli, a),
        Cmd::Export(a) => export::export(cli, a),
        Cmd::Import(a) => export::import(cli, a),
        Cmd::Config { cmd } => config::run(cli, cmd),
        Cmd::Cost(a) => cost::run(cli, a),
    }
}
