pub mod repo;

use crate::cli::{Cli, Cmd};
use crate::output::{AppError, Rendered};

pub fn dispatch(cli: &Cli) -> Result<Rendered, AppError> {
    match &cli.cmd {
        Cmd::Repo { cmd } => repo::run(cli, cmd),
    }
}
