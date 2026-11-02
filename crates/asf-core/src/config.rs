//! `.factory/config.toml`.
//!
//! Unknown sections are ignored so later milestones (`[worker]`, `[[gate]]`, `[merge]`, …) can add
//! their own tables without breaking older binaries.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ids::is_valid_prefix;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ConfigError {
    #[error("config parse error: {0}")]
    Parse(String),
    #[error("invalid config: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RepoCfg {
    pub prefix: String,
    pub default_branch: String,
    pub worktrees_dir: String,
}

impl Default for RepoCfg {
    fn default() -> Self {
        RepoCfg {
            prefix: "asf".into(),
            default_branch: "main".into(),
            worktrees_dir: ".worktrees".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SchedulerCfg {
    pub max_parallel: usize,
    pub hotspots: Vec<String>,
}

impl Default for SchedulerCfg {
    fn default() -> Self {
        SchedulerCfg {
            max_parallel: 3,
            hotspots: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoreCfg {
    pub auto_export: bool,
    pub export_comments: bool,
}

impl Default for StoreCfg {
    fn default() -> Self {
        StoreCfg {
            auto_export: true,
            export_comments: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiCfg {
    pub language: String,
}

impl Default for UiCfg {
    fn default() -> Self {
        UiCfg {
            language: "en".into(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub repo: RepoCfg,
    pub scheduler: SchedulerCfg,
    pub store: StoreCfg,
    pub ui: UiCfg,
}

const TEMPLATE: &str = r#"# auto-software-factory configuration (committed). Unknown sections are ignored.

[repo]
prefix = "asf"            # issue id prefix: <prefix>-<n>; ^[a-z][a-z0-9]{0,15}$
default_branch = "main"
worktrees_dir = ".worktrees"

[scheduler]
max_parallel = 3          # concurrent workers
hotspots = []             # globs that are never edited in parallel, e.g. ["Cargo.toml", "Cargo.lock"]

[store]
auto_export = true        # rewrite .factory/issues.jsonl after every mutating command
export_comments = true

[ui]
language = "en"           # en | pl
"#;

impl Config {
    /// Parses TOML and validates the result.
    pub fn from_toml(s: &str) -> Result<Config, ConfigError> {
        let cfg: Config = toml::from_str(s).map_err(|e| ConfigError::Parse(e.to_string()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if !is_valid_prefix(&self.repo.prefix) {
            return Err(ConfigError::Invalid(format!(
                "repo.prefix `{}` must match ^[a-z][a-z0-9]{{0,15}}$",
                self.repo.prefix
            )));
        }
        if self.scheduler.max_parallel == 0 {
            return Err(ConfigError::Invalid("scheduler.max_parallel must be >= 1".into()));
        }
        if self.repo.default_branch.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "repo.default_branch must not be empty".into(),
            ));
        }
        if self.repo.worktrees_dir.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "repo.worktrees_dir must not be empty".into(),
            ));
        }
        Ok(())
    }

    /// The commented template written by `asf repo init`; parses to `Config::default()`.
    pub fn default_toml() -> &'static str {
        TEMPLATE
    }

    /// The template with `repo.prefix` replaced.
    pub fn default_toml_with_prefix(prefix: &str) -> String {
        TEMPLATE.replacen("prefix = \"asf\"", &format!("prefix = \"{prefix}\""), 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_toml_is_default() {
        let c = Config::from_toml("").unwrap();
        assert_eq!(c, Config::default());
        assert_eq!(c.repo.prefix, "asf");
        assert_eq!(c.repo.default_branch, "main");
        assert_eq!(c.repo.worktrees_dir, ".worktrees");
        assert_eq!(c.scheduler.max_parallel, 3);
        assert!(c.scheduler.hotspots.is_empty());
        assert!(c.store.auto_export);
        assert!(c.store.export_comments);
        assert_eq!(c.ui.language, "en");
    }

    #[test]
    fn default_template_parses_to_default() {
        let c = Config::from_toml(Config::default_toml()).unwrap();
        assert_eq!(c, Config::default());
    }

    #[test]
    fn bad_prefix_rejected() {
        let e = Config::from_toml("[repo]\nprefix = \"Asf\"\n").unwrap_err();
        assert!(matches!(e, ConfigError::Invalid(_)), "{e}");
    }

    #[test]
    fn max_parallel_zero_rejected() {
        let e = Config::from_toml("[scheduler]\nmax_parallel = 0\n").unwrap_err();
        assert!(matches!(e, ConfigError::Invalid(_)), "{e}");
    }

    #[test]
    fn unknown_section_ignored() {
        let c = Config::from_toml("[worker]\nmodel = \"sonnet\"\n[repo]\nprefix = \"law\"\n").unwrap();
        assert_eq!(c.repo.prefix, "law");
    }

    #[test]
    fn parse_error_is_parse() {
        let e = Config::from_toml("[repo\n").unwrap_err();
        assert!(matches!(e, ConfigError::Parse(_)));
    }

    #[test]
    fn with_prefix_substitutes_template() {
        let t = Config::default_toml_with_prefix("law");
        let c = Config::from_toml(&t).unwrap();
        assert_eq!(c.repo.prefix, "law");
    }
}
