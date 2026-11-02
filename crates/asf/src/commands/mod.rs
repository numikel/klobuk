pub mod doctor;
pub mod export;
pub mod hook;
pub mod issue;
pub mod lane;
pub mod merge;
pub mod repo;
pub mod run;
pub mod serve;

use crate::cli::{Cli, Cmd};
use crate::output::{AppError, Rendered};

pub fn dispatch(cli: &Cli) -> Result<Rendered, AppError> {
    match &cli.cmd {
        Cmd::Repo { cmd } => repo::run(cli, cmd),
        Cmd::Issue { cmd } => issue::run(cli, cmd),
        Cmd::Lane { cmd } => lane::run(cli, cmd),
        Cmd::Run { cmd } => run::run(cli, cmd),
        Cmd::Merge { cmd } => merge::run(cli, cmd),
        Cmd::Hook { cmd } => hook::run(cli, cmd),
        Cmd::Serve(a) => serve::run(cli, a),
        Cmd::Export(a) => export::export(cli, a),
        Cmd::Import(a) => export::import(cli, a),
    }
}
