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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkerCfg {
    /// `herdr` (terminal engine) or `fake` (tests / dry runs).
    pub runtime: String,
    pub model: Option<String>,
    pub budget_usd: f64,
    pub stall_minutes: u64,
    pub max_wall_minutes: u64,
    pub max_tool_calls: u64,
    pub allowed_tools: Vec<String>,
    pub deny: Vec<String>,
    pub nudges: u32,
    /// Answer Claude's "trust this folder" dialog for worktrees under the repo root.
    pub auto_trust: bool,
    /// Pre-write `hasTrustDialogAccepted` for the repo root in `~/.claude.json` before launch.
    pub pre_trust: bool,
    pub start_timeout_ms: u64,
    pub prompt_timeout_ms: u64,
    pub heartbeat_s: u64,
}

impl Default for WorkerCfg {
    fn default() -> Self {
        WorkerCfg {
            runtime: "herdr".into(),
            model: None,
            budget_usd: 5.0,
            stall_minutes: 15,
            max_wall_minutes: 90,
            max_tool_calls: 400,
            allowed_tools: ["Read", "Edit", "Glob", "Grep", "Bash(git *)", "Bash(asf *)"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            deny: ["Read(./.env)", "Read(./.env.*)", "Read(**/secrets/**)"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            nudges: 1,
            auto_trust: true,
            pre_trust: false,
            start_timeout_ms: 90_000,
            prompt_timeout_ms: 15_000,
            heartbeat_s: 30,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GateCfg {
    pub name: String,
    pub cmd: Vec<String>,
    pub timeout_s: u64,
    pub shell: Option<String>,
}

impl Default for GateCfg {
    fn default() -> Self {
        GateCfg {
            name: String::new(),
            cmd: Vec::new(),
            timeout_s: 600,
            shell: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MergeCfg {
    /// `local` (rebase + ff) or `github` (PR via gh, M6).
    pub target: String,
    pub require_clean_main: bool,
}

impl Default for MergeCfg {
    fn default() -> Self {
        MergeCfg {
            target: "local".into(),
            require_clean_main: true,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReviewCfg {
    pub required: bool,
    pub on_labels: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub repo: RepoCfg,
    pub scheduler: SchedulerCfg,
    pub store: StoreCfg,
    pub ui: UiCfg,
    pub worker: WorkerCfg,
    #[serde(rename = "gate")]
    pub gates: Vec<GateCfg>,
    pub merge: MergeCfg,
    pub review: ReviewCfg,
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

[worker]
runtime = "herdr"         # herdr | fake
# model = "sonnet"
budget_usd = 5.0          # per issue; from statusLine cost estimate
stall_minutes = 15
max_wall_minutes = 90
max_tool_calls = 400
allowed_tools = ["Read", "Edit", "Glob", "Grep", "Bash(git *)", "Bash(asf *)"]
deny = ["Read(./.env)", "Read(./.env.*)", "Read(**/secrets/**)"]
auto_trust = true         # answer Claude's trust dialog for worktrees of this repo
pre_trust = false         # write hasTrustDialogAccepted into ~/.claude.json before launch

# Gates run in the worktree before merge, in order; first failure stops.
# [[gate]]
# name = "fmt"
# cmd = ["cargo", "fmt", "--all", "--", "--check"]
# [[gate]]
# name = "test"
# cmd = ["cargo", "test", "--workspace"]
# timeout_s = 900

[merge]
target = "local"          # local (rebase + fast-forward) | github (PR via gh)
require_clean_main = true

[review]
required = false
on_labels = []            # e.g. ["risky"]: labels that force a review worker before merge
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
        if !matches!(self.worker.runtime.as_str(), "herdr" | "fake") {
            return Err(ConfigError::Invalid(format!(
                "worker.runtime `{}` must be herdr or fake",
                self.worker.runtime
            )));
        }
        if self.worker.budget_usd < 0.0 {
            return Err(ConfigError::Invalid("worker.budget_usd must be >= 0".into()));
        }
        for g in &self.gates {
            if g.name.trim().is_empty() || g.cmd.is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "gate `{}` needs a name and a non-empty cmd",
                    g.name
                )));
            }
        }
        if !matches!(self.merge.target.as_str(), "local" | "github") {
            return Err(ConfigError::Invalid(format!(
                "merge.target `{}` must be local or github",
                self.merge.target
            )));
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

    #[test]
    fn worker_gate_merge_review_defaults() {
        let c = Config::from_toml("").unwrap();
        assert_eq!(c.worker.runtime, "herdr");
        assert_eq!(c.worker.budget_usd, 5.0);
        assert_eq!(c.worker.stall_minutes, 15);
        assert_eq!(c.worker.max_wall_minutes, 90);
        assert_eq!(c.worker.max_tool_calls, 400);
        assert!(c.worker.allowed_tools.iter().any(|t| t == "Edit"));
        assert!(c.worker.allowed_tools.iter().any(|t| t == "Bash(asf *)"));
        assert!(c.worker.deny.iter().any(|t| t == "Read(./.env)"));
        assert!(c.worker.auto_trust);
        assert!(!c.worker.pre_trust);
        assert_eq!(c.worker.start_timeout_ms, 90_000);
        assert_eq!(c.worker.prompt_timeout_ms, 15_000);
        assert_eq!(c.worker.heartbeat_s, 30);
        assert_eq!(c.worker.nudges, 1);
        assert!(c.worker.model.is_none());
        assert!(c.gates.is_empty());
        assert_eq!(c.merge.target, "local");
        assert!(c.merge.require_clean_main);
        assert!(!c.review.required);
    }

    #[test]
    fn gates_parse_as_array_of_tables() {
        let c = Config::from_toml(
            "[[gate]]\nname = \"fmt\"\ncmd = [\"cargo\", \"fmt\", \"--check\"]\n[[gate]]\nname = \"test\"\ncmd = [\"cargo\", \"test\"]\ntimeout_s = 900\nshell = \"pwsh\"\n",
        )
        .unwrap();
        assert_eq!(c.gates.len(), 2);
        assert_eq!(c.gates[0].timeout_s, 600);
        assert_eq!(c.gates[1].timeout_s, 900);
        assert_eq!(c.gates[1].shell.as_deref(), Some("pwsh"));
    }

    #[test]
    fn worker_validation() {
        assert!(matches!(
            Config::from_toml("[worker]\nruntime = \"tmux\"\n").unwrap_err(),
            ConfigError::Invalid(_)
        ));
        assert!(matches!(
            Config::from_toml("[worker]\nbudget_usd = -1\n").unwrap_err(),
            ConfigError::Invalid(_)
        ));
        assert!(matches!(
            Config::from_toml("[[gate]]\nname = \"x\"\ncmd = []\n").unwrap_err(),
            ConfigError::Invalid(_)
        ));
        assert!(matches!(
            Config::from_toml("[merge]\ntarget = \"svn\"\n").unwrap_err(),
            ConfigError::Invalid(_)
        ));
        assert!(Config::from_toml("[worker]\nruntime = \"fake\"\n").is_ok());
    }

    #[test]
    fn template_mentions_worker_and_gates() {
        let t = Config::default_toml();
        assert!(t.contains("[worker]") && t.contains("[[gate]]") && t.contains("[merge]"));
        let c = Config::from_toml(t).unwrap();
        assert_eq!(
            c,
            Config::default(),
            "template must still parse to the defaults (gates commented out)"
        );
    }
}
