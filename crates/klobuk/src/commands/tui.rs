//! `klobuk tui [--once] [--view <board|agents|inbox|queue|log>]`

use serde::Serialize;

use crate::cli::{Cli, TuiArgs};
use crate::ctx::Ctx;
use crate::output::{AppError, Rendered};
use crate::tui::{self, app::View};

#[derive(Serialize)]
struct Frame {
    frame: String,
}

pub fn run(cli: &Cli, a: &TuiArgs) -> Result<Rendered, AppError> {
    let view =
        match &a.view {
            Some(v) => Some(View::parse(v).ok_or_else(|| {
                AppError::usage(format!("unknown view `{v}` (board|agents|inbox|queue|log)"))
            })?),
            None => None,
        };
    let ctx = Ctx::load(cli)?;
    if a.once {
        let text = tui::run_once(&ctx, view)?;
        return Rendered::new(&Frame { frame: text.clone() }, text.trim_end());
    }
    if std::env::var("HERDR_ENV").ok().as_deref() != Some("1") && ctx.config.worker.runtime == "herdr" {
        eprintln!(
            "klobuk tui: not running inside a Herdr pane (HERDR_ENV != 1); agent focus will not work from here"
        );
    }
    tui::run_interactive(&ctx, view)?;
    Rendered::new(&serde_json::Value::Null, "")
}
