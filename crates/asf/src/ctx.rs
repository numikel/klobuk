//! Per-invocation context: repository root, config, store and lane.

use std::path::{Path, PathBuf};

use asf_core::{lane, Config};
use asf_store::SqliteStore;

use crate::cli::Cli;
use crate::git;
use crate::output::AppError;

pub const FACTORY_DIR: &str = ".factory";
pub const CONFIG_FILE: &str = "config.toml";
pub const DB_FILE: &str = "factory.db";
pub const ENV_ISSUE: &str = "ASF_ISSUE";

#[allow(dead_code)] // fields consumed by issue/run commands (M1 tasks 11-13)
pub struct Ctx {
    pub root: PathBuf,
    pub factory_dir: PathBuf,
    pub db_path: PathBuf,
    pub config: Config,
    pub store: SqliteStore,
    pub lane: String,
}

/// Where to start looking for the repository: `--repo` or the current directory.
pub fn start_dir(cli: &Cli) -> Result<PathBuf, AppError> {
    match &cli.repo {
        Some(p) => Ok(p.clone()),
        None => std::env::current_dir().map_err(|e| AppError::io(format!("current dir: {e}"))),
    }
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

impl Ctx {
    pub fn load(cli: &Cli) -> Result<Ctx, AppError> {
        let root = git::locate_root(&start_dir(cli)?)?;
        let factory_dir = root.join(FACTORY_DIR);
        let config_path = factory_dir.join(CONFIG_FILE);
        if !config_path.exists() {
            return Err(AppError::not_found(format!(
                "no {} in {} (run `asf repo init`)",
                config_path.strip_prefix(&root).unwrap_or(&config_path).display(),
                root.display()
            )));
        }
        let raw = std::fs::read_to_string(&config_path)?;
        let config = Config::from_toml(raw.trim_start_matches('\u{feff}'))?;
        let db_path = factory_dir.join(DB_FILE);
        let store = SqliteStore::open(&db_path)?;
        let cwd = std::env::current_dir().map_err(|e| AppError::io(format!("current dir: {e}")))?;
        let lane = lane::resolve(
            cli.lane.as_deref(),
            std::env::var(ENV_ISSUE).ok().as_deref(),
            &canonical(&cwd),
            &canonical(&root),
            &config.repo.worktrees_dir,
        );
        Ok(Ctx {
            root,
            factory_dir,
            db_path,
            config,
            store,
            lane,
        })
    }

    #[allow(dead_code)] // used by comment commands (task 12)
    /// `agent:<lane>`, `orchestrator` or `human` — the author string for comments and events.
    pub fn author(&self) -> String {
        match self.lane.as_str() {
            asf_core::LANE_ORCHESTRATOR | asf_core::LANE_HUMAN => self.lane.clone(),
            l => format!("agent:{l}"),
        }
    }
}
