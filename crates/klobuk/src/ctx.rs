//! Per-invocation context: repository root, config, store and lane.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use klobuk_core::{lane, Config, IssueStore};
use klobuk_store::SqliteStore;

use crate::cli::Cli;
use crate::commands::hook::ENV_RUN;
use crate::git;
use crate::output::AppError;

pub const FACTORY_DIR: &str = ".factory";
pub const CONFIG_FILE: &str = "config.toml";
pub const DB_FILE: &str = "factory.db";
pub const ENV_ISSUE: &str = "KLOBUK_ISSUE";

pub struct Ctx {
    pub root: PathBuf,
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

/// The issue a worker process belongs to: `KLOBUK_ISSUE`, else the issue of the run in `KLOBUK_RUN`.
/// `None` for the orchestrator and the human, who run with neither variable set.
fn worker_issue(store: &SqliteStore) -> Option<String> {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    env(ENV_ISSUE).or_else(|| {
        let run = store.get_run(env(ENV_RUN)?.trim()).ok()??;
        Some(run.issue_id.to_string())
    })
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
                "no {} in {} (run `klobuk repo init`)",
                config_path.strip_prefix(&root).unwrap_or(&config_path).display(),
                root.display()
            )));
        }
        let raw = std::fs::read_to_string(&config_path)?;
        let mut config = Config::from_toml(raw.trim_start_matches('\u{feff}'))?;
        if let Some(notice) = config.apply_legacy_aliases() {
            eprintln!("klobuk: {notice}");
        }
        let db_path = factory_dir.join(DB_FILE);
        let store = SqliteStore::open(&db_path)?;
        let cwd = std::env::current_dir().map_err(|e| AppError::io(format!("current dir: {e}")))?;
        let worker_issue = worker_issue(&store);
        let lane = lane::resolve(
            cli.lane.as_deref(),
            worker_issue.as_deref(),
            &canonical(&cwd),
            &canonical(&root),
            &config.repo.worktrees_dir,
        )
        .map_err(|e| AppError::usage(e.to_string()))?;
        Ok(Ctx {
            root,
            config,
            store,
            lane,
        })
    }

    /// `agent:<lane>`, `orchestrator` or `human` — the author string for comments and events.
    pub fn author(&self) -> String {
        match self.lane.as_str() {
            klobuk_core::LANE_ORCHESTRATOR | klobuk_core::LANE_HUMAN => self.lane.clone(),
            l => format!("agent:{l}"),
        }
    }
}

/// Hash of a config file's text; equal text gives an equal hash within one process.
fn content_hash(raw: &str) -> u64 {
    let mut h = DefaultHasher::new();
    raw.hash(&mut h);
    h.finish()
}

/// Reads and parses the config file; the error is a message fit for a warning.
fn read_config(path: &Path) -> (u64, Result<Config, String>) {
    match std::fs::read_to_string(path) {
        Ok(raw) => {
            // The notice already went out when `Ctx::load` read the file; a refresh maps the alias silently.
            let parsed = Config::from_toml(raw.trim_start_matches('\u{feff}'))
                .map(|mut c| {
                    c.apply_legacy_aliases();
                    c
                })
                .map_err(|e| e.to_string());
            (content_hash(&raw), parsed)
        }
        Err(e) => {
            let msg = format!("cannot read {}: {e}", path.display());
            (content_hash(&msg), Err(msg))
        }
    }
}

/// Lets a long-running command (`klobuk serve`) pick up edits of `.factory/config.toml` between ticks.
///
/// A parse or validation error keeps the config the context already has. It is reported only the
/// first time a given content is seen, so a broken file does not repeat the warning on every tick;
/// changing the file again is noticed on the next refresh because the content hash differs.
pub struct ConfigWatch {
    path: PathBuf,
    /// Hash of the last content looked at, whether it was applied or rejected.
    seen: u64,
}

impl ConfigWatch {
    /// Starts watching with the content on disk now (what `Ctx::load` has just applied).
    pub fn new(ctx: &Ctx) -> ConfigWatch {
        let path = ctx.root.join(FACTORY_DIR).join(CONFIG_FILE);
        let (seen, _) = read_config(&path);
        ConfigWatch { path, seen }
    }

    /// Re-reads the file. A changed, valid config replaces `ctx.config` (returns `None`); changed
    /// but unusable content returns a warning and leaves the previous config in force.
    pub fn refresh(&mut self, ctx: &mut Ctx) -> Option<String> {
        let (hash, parsed) = read_config(&self.path);
        if hash == self.seen {
            return None;
        }
        self.seen = hash;
        match parsed {
            Ok(config) => {
                ctx.config = config;
                None
            }
            Err(msg) => Some(format!(
                "{CONFIG_FILE} changed but is not usable, keeping the previous config: {msg}"
            )),
        }
    }
}

/// The running klobuk binary as a forward-slash path (what workers and hooks call).
pub fn klobuk_bin() -> String {
    std::env::current_exe()
        .map(|p| {
            let s = p.to_string_lossy().replace('\\', "/");
            s.strip_prefix("//?/").map(str::to_string).unwrap_or(s)
        })
        .unwrap_or_else(|_| "klobuk".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fx {
        _dir: tempfile::TempDir,
        ctx: Ctx,
        cfg: PathBuf,
    }

    /// A context over a temp repo whose `.factory/config.toml` holds the default config.
    fn fixture() -> Fx {
        let dir = tempfile::Builder::new().prefix("klobuk-ctx-").tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let factory = root.join(FACTORY_DIR);
        std::fs::create_dir_all(&factory).unwrap();
        let cfg = factory.join(CONFIG_FILE);
        std::fs::write(&cfg, Config::default_toml()).unwrap();
        let ctx = Ctx {
            root,
            config: Config::default(),
            store: SqliteStore::open(&factory.join(DB_FILE)).unwrap(),
            lane: klobuk_core::LANE_ORCHESTRATOR.into(),
        };
        Fx { _dir: dir, ctx, cfg }
    }

    fn with_parallel(n: usize) -> String {
        let text = Config::default_toml().replace("max_parallel = 3", &format!("max_parallel = {n}"));
        assert_ne!(
            text,
            Config::default_toml(),
            "the default max_parallel is no longer 3"
        );
        text
    }

    #[test]
    fn an_unchanged_file_is_not_applied_again() {
        let mut fx = fixture();
        let mut watch = ConfigWatch::new(&fx.ctx);
        fx.ctx.config.scheduler.max_parallel = 9; // a re-parse would overwrite this
        assert_eq!(watch.refresh(&mut fx.ctx), None);
        assert_eq!(fx.ctx.config.scheduler.max_parallel, 9);
    }

    #[test]
    fn an_edited_valid_file_replaces_the_config() {
        let mut fx = fixture();
        let mut watch = ConfigWatch::new(&fx.ctx);
        std::fs::write(&fx.cfg, with_parallel(7)).unwrap();
        assert_eq!(watch.refresh(&mut fx.ctx), None);
        assert_eq!(fx.ctx.config.scheduler.max_parallel, 7);
    }

    #[test]
    fn a_bom_in_front_of_the_toml_is_ignored() {
        let mut fx = fixture();
        let mut watch = ConfigWatch::new(&fx.ctx);
        std::fs::write(&fx.cfg, format!("\u{feff}{}", with_parallel(5))).unwrap();
        assert_eq!(watch.refresh(&mut fx.ctx), None);
        assert_eq!(fx.ctx.config.scheduler.max_parallel, 5);
    }

    #[test]
    fn an_invalid_file_keeps_the_old_config_and_warns_once_per_content() {
        let mut fx = fixture();
        let mut watch = ConfigWatch::new(&fx.ctx);
        std::fs::write(&fx.cfg, with_parallel(7)).unwrap();
        assert_eq!(watch.refresh(&mut fx.ctx), None);

        // does not parse
        std::fs::write(&fx.cfg, "[scheduler]\nmax_parallel = \"many\"\n").unwrap();
        let warning = watch
            .refresh(&mut fx.ctx)
            .expect("first sight of broken content warns");
        assert!(warning.contains("keeping the previous config"), "{warning}");
        assert_eq!(fx.ctx.config.scheduler.max_parallel, 7, "old config stays");
        assert_eq!(
            watch.refresh(&mut fx.ctx),
            None,
            "same content: no second warning"
        );
        assert_eq!(watch.refresh(&mut fx.ctx), None);

        // parses but fails validation: different content, so it warns again
        std::fs::write(&fx.cfg, with_parallel(0)).unwrap();
        let warning = watch.refresh(&mut fx.ctx).expect("new broken content warns");
        assert!(warning.contains("max_parallel"), "{warning}");
        assert_eq!(fx.ctx.config.scheduler.max_parallel, 7);
        assert_eq!(watch.refresh(&mut fx.ctx), None);

        // restoring the content the context already runs with clears the warning state, and a
        // later edit is applied
        std::fs::write(&fx.cfg, with_parallel(7)).unwrap();
        assert_eq!(watch.refresh(&mut fx.ctx), None);
        std::fs::write(&fx.cfg, with_parallel(2)).unwrap();
        assert_eq!(watch.refresh(&mut fx.ctx), None);
        assert_eq!(fx.ctx.config.scheduler.max_parallel, 2);
    }

    #[test]
    fn a_vanished_file_keeps_the_old_config_and_warns_once() {
        let mut fx = fixture();
        let mut watch = ConfigWatch::new(&fx.ctx);
        std::fs::write(&fx.cfg, with_parallel(6)).unwrap();
        assert_eq!(watch.refresh(&mut fx.ctx), None);
        std::fs::remove_file(&fx.cfg).unwrap();
        let warning = watch.refresh(&mut fx.ctx).expect("a missing file warns");
        assert!(warning.contains("keeping the previous config"), "{warning}");
        assert_eq!(fx.ctx.config.scheduler.max_parallel, 6);
        assert_eq!(watch.refresh(&mut fx.ctx), None);
        std::fs::write(&fx.cfg, with_parallel(4)).unwrap();
        assert_eq!(watch.refresh(&mut fx.ctx), None);
        assert_eq!(fx.ctx.config.scheduler.max_parallel, 4);
    }
}
