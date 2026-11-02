mod cli;
mod commands;
mod ctx;
mod git;
mod output;
mod worker;

use clap::error::ErrorKind;
use clap::Parser;

use cli::Cli;
use output::{print_err, print_ok, AppError};

fn wants_json() -> bool {
    std::env::args().skip(1).any(|a| a == "--json")
}

fn main() {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
                e.exit();
            }
            if wants_json() {
                let msg = e.to_string().lines().next().unwrap_or("usage error").to_string();
                print_err(true, &AppError::usage(msg));
                std::process::exit(2);
            }
            e.exit();
        }
    };
    match commands::dispatch(&cli) {
        Ok(rendered) => print_ok(cli.json, cli.quiet, &rendered),
        Err(e) => {
            print_err(cli.json, &e);
            std::process::exit(e.exit_code());
        }
    }
}
