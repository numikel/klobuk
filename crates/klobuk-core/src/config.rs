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
            prefix: "kb".into(),
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

/// How much a worker tests on its own before `klobuk issue finish` (`[worker] self_test`). Verification
/// runs the full gates right after `finish` either way, so this only decides who pays for the first run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SelfTest {
    /// The fast gates (format, lint) and only the tests that cover what the worker changed.
    #[default]
    Targeted,
    /// Every gate that verification runs, before `finish`.
    Full,
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
    /// Claude Code `--permission-mode` for workers: `auto` (classifier approves what the allowlist
    /// does not; deny rules still win), `acceptEdits`, `dontAsk` or `bypassPermissions`.
    pub permission_mode: String,
    pub nudges: u32,
    /// Answer Claude's "trust this folder" dialog for worktrees under the repo root.
    pub auto_trust: bool,
    /// Pre-write `hasTrustDialogAccepted` for the repo root in `~/.claude.json` before launch.
    pub pre_trust: bool,
    /// Start worker and reviewer sessions with `claude --remote-control <session name>`, so they show
    /// up in claude.ai/code and the mobile app. Planner calls (headless `claude -p`) are not affected.
    pub remote_control: bool,
    /// What the brief tells the worker to run before `finish`: see [`SelfTest`].
    pub self_test: SelfTest,
    pub start_timeout_ms: u64,
    pub prompt_timeout_ms: u64,
    pub heartbeat_s: u64,
    /// How many times `klobuk serve` sends the turn again when an agent ends it on an API or network
    /// error (`API Error: ...`), waiting 1, 2, 4, ... minutes between the sends, before the run is
    /// `stalled`. A resend uses neither a run attempt nor a review round.
    pub api_retries: u32,
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
            allowed_tools: ["Read", "Edit", "Glob", "Grep", "Bash(git *)", "Bash(klobuk *)"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            deny: ["Read(./.env)", "Read(./.env.*)", "Read(**/secrets/**)"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            permission_mode: "auto".into(),
            nudges: 1,
            auto_trust: true,
            pre_trust: false,
            remote_control: false,
            self_test: SelfTest::default(),
            start_timeout_ms: 90_000,
            prompt_timeout_ms: 15_000,
            heartbeat_s: 30,
            api_retries: 5,
        }
    }
}

/// The stage a `[[gate]]` runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GateWhen {
    /// Worker verification in the worktree, before the issue is enqueued.
    Verify,
    /// Merge queue, in the worktree after the rebase.
    Merge,
    /// Repository root after a merge (`[merge] verify_main_after`).
    Main,
}

impl GateWhen {
    /// Every stage, in pipeline order; the default for a gate without `when`.
    pub const ALL: [GateWhen; 3] = [GateWhen::Verify, GateWhen::Merge, GateWhen::Main];
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GateCfg {
    pub name: String,
    pub cmd: Vec<String>,
    pub timeout_s: u64,
    pub shell: Option<String>,
    /// Stages the gate runs in; absent means all of them.
    pub when: Vec<GateWhen>,
    /// Stop the gate once at least this many distinct tests reported `has been running for over <n>
    /// seconds` (the libtest hang warning). Absent: never stop for that.
    pub slow_tests: Option<u32>,
    /// Stop the gate once its first slow-test report is this many seconds old and it has not finished.
    /// Absent: never stop for that.
    pub slow_after_s: Option<u64>,
}

impl Default for GateCfg {
    fn default() -> Self {
        GateCfg {
            name: String::new(),
            cmd: Vec::new(),
            timeout_s: 600,
            shell: None,
            when: GateWhen::ALL.to_vec(),
            slow_tests: None,
            slow_after_s: None,
        }
    }
}

impl GateCfg {
    /// Whether the gate runs in `stage`.
    pub fn runs_in(&self, stage: GateWhen) -> bool {
        self.when.contains(&stage)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GitHubCfg {
    /// `gh` (the GitHub CLI) or `fake` (tests).
    pub runner: String,
    pub remote: String,
    /// PR base branch; defaults to `[repo] default_branch`.
    pub base: Option<String>,
    /// `merge` (default: keeps worker commits, so worktrees stay cleanable) | `rebase` | `squash`.
    pub method: String,
    /// Keep the PR open until every check passed (`false`: merge as soon as it is mergeable).
    pub wait_checks: bool,
    pub delete_branch: bool,
}

impl Default for GitHubCfg {
    fn default() -> Self {
        GitHubCfg {
            runner: "gh".into(),
            remote: "origin".into(),
            base: None,
            method: "merge".into(),
            wait_checks: true,
            delete_branch: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MergeCfg {
    /// `local` (rebase + ff) or `github` (push + PR via gh).
    pub target: String,
    pub require_clean_main: bool,
    /// Run the `[[gate]]`s in the repository root right after every merge to detect a red default branch.
    pub verify_main_after: bool,
    /// Skip the queue's gates for an item whose run was verified, whose rebase changed nothing and whose
    /// branch head is the head that was verified: the verification's results stand in for them. A real
    /// rebase or a later commit always runs the gates. Off: the queue gates every item, as before.
    pub reuse_verification: bool,
    /// What the queue removes after a successful merge (and, with `verify_main_after`, a green main).
    /// Empty keeps everything.
    pub clean_after: Vec<CleanKind>,
    /// Times one item may be sent back to its worker for a gate failure after the rebase, in the
    /// worker's own verification, or for a rebase conflict, before the human is asked. `0` never sends
    /// back. Edited as `gate.max_rounds` (`[[gate]]` is an array, so it cannot hold the key itself).
    pub gate_max_rounds: u32,
    pub github: GitHubCfg,
}

impl Default for MergeCfg {
    fn default() -> Self {
        MergeCfg {
            target: "local".into(),
            require_clean_main: true,
            verify_main_after: false,
            reuse_verification: true,
            clean_after: vec![CleanKind::Worktree, CleanKind::ReviewWorktrees],
            gate_max_rounds: 2,
            github: GitHubCfg::default(),
        }
    }
}

/// One kind of clean-up the merge queue runs after a merge (`[merge] clean_after`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanKind {
    /// `.worktrees/<id>` of the merged issue, with the refusals of `klobuk lane clean <id>`.
    Worktree,
    /// `git branch -d feat/<id>`, after the worktree is gone (lowercase `-d`: refuses an unmerged branch).
    Branch,
    /// The detached `.worktrees/<review-id>` checkouts of the issue's review runs.
    ReviewWorktrees,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlannerCfg {
    /// `claude` (headless `claude -p`) or `fake` (tests).
    pub runner: String,
    pub model: String,
    pub budget_usd: f64,
    pub max_turns: u32,
}

impl Default for PlannerCfg {
    fn default() -> Self {
        PlannerCfg {
            runner: "claude".into(),
            model: "claude-haiku-5-5".into(),
            budget_usd: 1.0,
            max_turns: 15,
        }
    }
}

/// The methodology that ships as the default, and the name it had before the project became Klobuk.
pub const DEFAULT_METHOD: &str = "klobuk-default";
pub const LEGACY_DEFAULT_METHOD: &str = "asf-default";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MethodologyCfg {
    /// Name of the active methodology (`klobuk method list`).
    pub name: String,
}

impl Default for MethodologyCfg {
    fn default() -> Self {
        MethodologyCfg {
            name: DEFAULT_METHOD.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReviewCfg {
    pub required: bool,
    pub on_labels: Vec<String>,
    /// Model the read-only reviewer worker runs on.
    pub model: String,
    /// Review rounds that may end in `changes` before the human is asked to decide.
    pub max_rounds: u32,
    /// Reviewer runs that may be active at once; reviewers do not count against
    /// `[scheduler] max_parallel`.
    pub max_parallel: u32,
}

impl Default for ReviewCfg {
    fn default() -> Self {
        ReviewCfg {
            required: false,
            on_labels: Vec::new(),
            model: "opus".into(),
            max_rounds: 2,
            max_parallel: 2,
        }
    }
}

/// `[notify]`: how `klobuk serve` nags the human when something waits for them too long.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifyCfg {
    /// First reminder after this many minutes of waiting; `0` switches reminders off.
    pub after_minutes: u64,
    /// Repeat every this many minutes while the thing still waits; `0` reminds only once.
    pub every_minutes: u64,
    /// Optional program run once per notification or reminder, without a shell. Empty: no command.
    pub command: Vec<String>,
    /// Notify at once, one time, when something new starts to wait for the human (a question or
    /// blocker to them, a blocked or failed run, a failed or blocked merge, a blocked issue).
    /// Independent of `after_minutes`: `false` switches only these immediate notifications off.
    pub on_new: bool,
}

impl Default for NotifyCfg {
    fn default() -> Self {
        NotifyCfg {
            after_minutes: 15,
            every_minutes: 30,
            command: Vec::new(),
            on_new: true,
        }
    }
}

/// `[serve]`: defaults of the `klobuk serve` loop; the `--interval` and `--max-ticks` flags override them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServeCfg {
    /// Seconds between ticks (>= 1).
    pub interval_s: u64,
    /// With `--until-done`: stop after this many ticks (>= 1). Absent: no limit.
    pub max_ticks: Option<u64>,
}

impl Default for ServeCfg {
    fn default() -> Self {
        ServeCfg {
            interval_s: 10,
            max_ticks: None,
        }
    }
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
    pub planner: PlannerCfg,
    pub methodology: MethodologyCfg,
    pub notify: NotifyCfg,
    pub serve: ServeCfg,
}

const TEMPLATE: &str = r#"# Klobuk configuration (committed). Unknown sections are ignored.

[repo]
prefix = "kb"              # issue id prefix: <prefix>-<n>; ^[a-z][a-z0-9]{0,15}$
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

[methodology]
name = "klobuk-default"      # `klobuk method list`; repo-local ones live in .factory/methodologies/<name>/

[worker]
runtime = "herdr"         # herdr | fake
# model = "sonnet"        # fable | opus | sonnet | haiku, or a full model id (claude-...); unset: Claude Code default
budget_usd = 5.0          # per issue; cost as reported by Claude Code for each session (API list prices incl. cache; on a subscription an equivalent, not a bill)
stall_minutes = 15
api_retries = 5           # an agent whose turn ends on an API or network error (ENOTFOUND, 5xx, timeout) gets the turn sent again after 1, 2, 4, ... min; after this many sends the run is stalled with a BLOCKER (a send uses no attempt and no review round)
max_wall_minutes = 90
max_tool_calls = 400
allowed_tools = ["Read", "Edit", "Glob", "Grep", "Bash(git *)", "Bash(klobuk *)"]
deny = ["Read(./.env)", "Read(./.env.*)", "Read(**/secrets/**)"]
permission_mode = "auto"  # auto | acceptEdits | dontAsk | bypassPermissions (deny rules always win)
auto_trust = true         # answer Claude's trust dialog for worktrees of this repo
pre_trust = false         # write hasTrustDialogAccepted into ~/.claude.json before launch
# remote_control = false  # true: worker and reviewer sessions join Remote Control (claude.ai/code, mobile app)
self_test = "targeted"    # targeted | full: what the worker runs before `finish`. targeted: the fast gates (format, lint) and only the tests of what it changed; full: every gate. Verification runs the full gates right after `finish` either way

# Gates run in the worktree before merge, in order; first failure stops.
# [[gate]]
# name = "fmt"
# cmd = ["cargo", "fmt", "--all", "--", "--check"]
# [[gate]]
# name = "test"
# cmd = ["cargo", "test", "--workspace"]
# timeout_s = 900
# when = ["verify", "merge"]   # stages the gate runs in: verify (worker) | merge (queue, after rebase) | main (root, after merge); default: all three
# slow_tests = 3               # optional: stop the gate once this many tests print "has been running for over 60 seconds" (cargo test)
# slow_after_s = 300           # optional: stop it when the first such line is this old and the gate is still running

[merge]
target = "local"          # local (rebase + fast-forward) | github (push + PR via gh)
require_clean_main = true
verify_main_after = false # run the gates in the repo root after each merge; a red main keeps the item `merged`, posts a BLOCKER and halts the tick; costs one more full gate run per merge, and is redundant after a fast-forward (main is then the tree the queue gates passed)
reuse_verification = true # skip the queue gates when the worker's verification passed them on this very commit and the rebase changed nothing; false: every item pays another full gate run
clean_after = ["worktree", "review_worktrees"] # removed after a merge (and a green main when verify_main_after is on): worktree | branch (git branch -d) | review_worktrees; [] keeps everything; a refusal (claimed, dirty or unmerged lane) is reported, never fails the merge
# gate_max_rounds = 2     # edited as `gate.max_rounds`: send-backs to the worker for a gate failure or a rebase conflict per item before the human is asked; 0 never sends back
# [merge.github]          # used when target = "github"
# remote = "origin"
# base = "main"            # default: [repo] default_branch
# method = "merge"         # merge | rebase | squash (rebase/squash rewrite commits: `klobuk lane clean` then refuses)
# wait_checks = true       # merge only after every check passed
# delete_branch = false

[review]
required = false
on_labels = []            # e.g. ["risky"]: labels that force a review worker before merge
# model = "opus"          # model of the read-only reviewer: fable | opus | sonnet | haiku, or a full model id
# max_rounds = 2          # `changes` verdicts allowed before the human has to decide
# max_parallel = 2        # reviewers running at once (own limit, not counted in [scheduler] max_parallel); >= 1

[planner]
runner = "claude"         # claude (headless `claude -p`, read-only tools) | fake (tests)
model = "claude-haiku-5-5"  # fable | opus | sonnet | haiku, or a full model id
budget_usd = 1.0          # per planning step; cost as reported by Claude Code (see [worker] budget_usd)
max_turns = 15

[notify]                  # `klobuk serve` tells you when something starts to wait for you and reminds you while it still does (blocked issue, failed merge, stalled run, question)
on_new = true      # notify at once, one time, when something new waits for you; false switches only that off
after_minutes = 15 # first reminder this long after the notification; 0 switches reminders off (not the immediate notification)
every_minutes = 30 # then again this often while it still waits; 0 reminds only once
# command = ["pwsh", "-File", "notify.ps1"]  # optional, run once per notification or reminder without a shell (10 s limit) with
#                                            # KLOBUK_REMIND_ISSUE, KLOBUK_REMIND_KIND, KLOBUK_REMIND_AGE_MINUTES, KLOBUK_REMIND_TEXT set

[serve]                   # defaults of `klobuk serve`; the --interval and --max-ticks flags override them; an edit applies on the next tick of a running serve
interval_s = 10           # seconds between ticks; >= 1
# max_ticks = 600         # with --until-done: stop after this many ticks (the pulse and the TUI then say `max_ticks`); absent: no limit
"#;

impl Config {
    /// Parses TOML and validates the result.
    pub fn from_toml(s: &str) -> Result<Config, ConfigError> {
        let cfg: Config = toml::from_str(s).map_err(|e| ConfigError::Parse(e.to_string()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Maps names retired by the rename to their current ones; returns the one-line deprecation
    /// notice when something was mapped. Kept apart from `from_toml` so only the caller decides
    /// where the notice goes (the core does no IO).
    pub fn apply_legacy_aliases(&mut self) -> Option<String> {
        if self.methodology.name == LEGACY_DEFAULT_METHOD {
            self.methodology.name = DEFAULT_METHOD.to_string();
            return Some(format!(
                "[methodology] name = \"{LEGACY_DEFAULT_METHOD}\" is deprecated, using \"{DEFAULT_METHOD}\"; rename it in .factory/config.toml"
            ));
        }
        None
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
        if self.review.max_parallel == 0 {
            return Err(ConfigError::Invalid("review.max_parallel must be >= 1".into()));
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
        if !matches!(
            self.worker.permission_mode.as_str(),
            "auto" | "acceptEdits" | "dontAsk" | "bypassPermissions"
        ) {
            return Err(ConfigError::Invalid(format!(
                "worker.permission_mode `{}` must be auto, acceptEdits, dontAsk or bypassPermissions",
                self.worker.permission_mode
            )));
        }
        for g in &self.gates {
            if g.name.trim().is_empty() || g.cmd.is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "gate `{}` needs a name and a non-empty cmd",
                    g.name
                )));
            }
            if g.when.is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "gate `{}`: `when` must list at least one of verify, merge, main",
                    g.name
                )));
            }
            // 0 would stop the gate before any test could report: a typo, not a setting
            if g.slow_tests == Some(0) || g.slow_after_s == Some(0) {
                return Err(ConfigError::Invalid(format!(
                    "gate `{}`: `slow_tests` and `slow_after_s` must be at least 1 (leave them out to turn the check off)",
                    g.name
                )));
            }
        }
        if self.notify.command.first().is_some_and(|p| p.trim().is_empty()) {
            return Err(ConfigError::Invalid(
                "notify.command must start with a program name".into(),
            ));
        }
        if !matches!(self.planner.runner.as_str(), "claude" | "fake") {
            return Err(ConfigError::Invalid(format!(
                "planner.runner `{}` must be claude or fake",
                self.planner.runner
            )));
        }
        if !matches!(self.merge.github.runner.as_str(), "gh" | "fake") {
            return Err(ConfigError::Invalid(format!(
                "merge.github.runner `{}` must be gh or fake",
                self.merge.github.runner
            )));
        }
        if !matches!(self.merge.github.method.as_str(), "rebase" | "squash" | "merge") {
            return Err(ConfigError::Invalid(format!(
                "merge.github.method `{}` must be rebase, squash or merge",
                self.merge.github.method
            )));
        }
        if self.serve.interval_s == 0 {
            return Err(ConfigError::Invalid("serve.interval_s must be >= 1".into()));
        }
        if self.serve.max_ticks == Some(0) {
            return Err(ConfigError::Invalid("serve.max_ticks must be >= 1".into()));
        }
        if !matches!(self.merge.target.as_str(), "local" | "github") {
            return Err(ConfigError::Invalid(format!(
                "merge.target `{}` must be local or github",
                self.merge.target
            )));
        }
        Ok(())
    }

    /// The commented template written by `klobuk repo init`; parses to `Config::default()`.
    pub fn default_toml() -> &'static str {
        TEMPLATE
    }

    /// The template with `repo.prefix` replaced.
    pub fn default_toml_with_prefix(prefix: &str) -> String {
        TEMPLATE.replacen("prefix = \"kb\"", &format!("prefix = \"{prefix}\""), 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_toml_is_default() {
        let c = Config::from_toml("").unwrap();
        assert_eq!(c, Config::default());
        assert_eq!(c.repo.prefix, "kb");
        assert_eq!(c.repo.default_branch, "main");
        assert_eq!(c.repo.worktrees_dir, ".worktrees");
        assert_eq!(c.scheduler.max_parallel, 3);
        assert!(c.scheduler.hotspots.is_empty());
        assert!(c.store.auto_export);
        assert!(c.store.export_comments);
        assert_eq!(c.ui.language, "en");
    }

    #[test]
    fn the_old_default_methodology_name_is_an_alias_with_one_notice() {
        let mut c = Config::from_toml("[methodology]\nname = \"asf-default\"\n").unwrap();
        let notice = c.apply_legacy_aliases().expect("a notice for the old name");
        assert_eq!(c.methodology.name, "klobuk-default");
        assert_eq!(notice.lines().count(), 1, "{notice}");
        assert!(
            notice.contains("asf-default") && notice.contains("klobuk-default"),
            "{notice}"
        );
        assert_eq!(c.apply_legacy_aliases(), None, "nothing left to map");
        let mut own = Config::from_toml("[methodology]\nname = \"spec-kit\"\n").unwrap();
        assert_eq!(own.apply_legacy_aliases(), None);
        assert_eq!(own.methodology.name, "spec-kit");
    }

    #[test]
    fn default_template_parses_to_default() {
        let c = Config::from_toml(Config::default_toml()).unwrap();
        assert_eq!(c, Config::default());
    }

    #[test]
    fn serve_defaults_and_limits() {
        let c = Config::from_toml("").unwrap();
        assert_eq!(c.serve.interval_s, 10);
        assert_eq!(c.serve.max_ticks, None);
        let c = Config::from_toml("[serve]\ninterval_s = 5\nmax_ticks = 600\n").unwrap();
        assert_eq!((c.serve.interval_s, c.serve.max_ticks), (5, Some(600)));
        for bad in ["interval_s = 0", "max_ticks = 0"] {
            let e = Config::from_toml(&format!("[serve]\n{bad}\n")).unwrap_err();
            assert!(matches!(e, ConfigError::Invalid(_)), "{bad}: {e}");
        }
    }

    #[test]
    fn serve_template_header_says_an_edit_reaches_the_running_loop() {
        // the loop re-reads [serve] every tick (`Applies::NextTick`), so the template must not ask for a restart
        let header = Config::default_toml()
            .lines()
            .find(|l| l.starts_with("[serve]"))
            .expect("the template has a [serve] section");
        assert!(!header.contains("restart"), "{header}");
        assert!(header.contains("next tick"), "{header}");
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
        assert!(c.worker.allowed_tools.iter().any(|t| t == "Bash(klobuk *)"));
        assert!(c.worker.deny.iter().any(|t| t == "Read(./.env)"));
        assert!(c.worker.auto_trust);
        assert!(!c.worker.pre_trust);
        assert_eq!(c.worker.start_timeout_ms, 90_000);
        assert_eq!(c.worker.prompt_timeout_ms, 15_000);
        assert_eq!(c.worker.heartbeat_s, 30);
        assert_eq!(c.worker.api_retries, 5);
        assert_eq!(c.worker.nudges, 1);
        assert!(c.worker.model.is_none());
        assert!(c.gates.is_empty());
        assert_eq!(c.merge.target, "local");
        assert_eq!(
            c.merge.github.method, "merge",
            "PR merges keep worker commits intact"
        );
        assert_eq!(c.merge.github.runner, "gh");
        assert_eq!(
            c.worker.permission_mode, "auto",
            "workers must not stop on permission prompts"
        );
        assert!(c.merge.require_clean_main);
        assert!(!c.merge.verify_main_after, "off unless asked for");
        assert!(c.merge.reuse_verification, "on unless switched off");
        assert!(!c.review.required);
    }

    #[test]
    fn review_max_parallel_defaults_to_two_and_rejects_zero() {
        assert_eq!(Config::from_toml("").unwrap().review.max_parallel, 2);
        let c = Config::from_toml("[review]\nmax_parallel = 1\n").unwrap();
        assert_eq!(c.review.max_parallel, 1);
        assert_eq!(c.review.max_rounds, 2, "other review defaults survive");
        let e = Config::from_toml("[review]\nmax_parallel = 0\n").unwrap_err();
        assert!(
            matches!(e, ConfigError::Invalid(ref m) if m.contains("review.max_parallel")),
            "{e}"
        );
    }

    #[test]
    fn template_documents_review_max_parallel() {
        let t = Config::default_toml();
        assert!(t.contains("# max_parallel = 2"), "commented line in [review]");
        assert_eq!(Config::from_toml(t).unwrap().review.max_parallel, 2);
    }

    #[test]
    fn verify_main_after_parses_and_template_keeps_it_off() {
        let c = Config::from_toml("[merge]\nverify_main_after = true\n").unwrap();
        assert!(c.merge.verify_main_after);
        assert!(c.merge.require_clean_main, "other merge defaults survive");
        let t = Config::from_toml(Config::default_toml()).unwrap();
        assert!(!t.merge.verify_main_after);
    }

    #[test]
    fn reuse_verification_is_on_by_default_parses_and_the_template_keeps_it_on() {
        assert!(Config::from_toml("").unwrap().merge.reuse_verification);
        let c = Config::from_toml("[merge]\nreuse_verification = false\n").unwrap();
        assert!(!c.merge.reuse_verification);
        assert!(c.merge.require_clean_main, "other merge defaults survive");
        let t = Config::from_toml(Config::default_toml()).unwrap();
        assert!(t.merge.reuse_verification);
    }

    #[test]
    fn remote_control_is_off_by_default_parses_and_is_documented_in_the_template() {
        assert!(!Config::default().worker.remote_control);
        assert!(!Config::from_toml("").unwrap().worker.remote_control);
        let c = Config::from_toml("[worker]\nremote_control = true\n").unwrap();
        assert!(c.worker.remote_control);
        assert_eq!(c.worker.permission_mode, "auto", "other worker defaults survive");
        let t = Config::default_toml();
        assert!(
            t.contains("# remote_control = false"),
            "commented line in [worker]"
        );
        assert!(!Config::from_toml(t).unwrap().worker.remote_control);
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
    fn slow_test_keys_are_optional_per_gate() {
        let c = Config::from_toml(
            "[[gate]]\nname = \"fmt\"\ncmd = [\"cargo\", \"fmt\"]\n\
             [[gate]]\nname = \"test\"\ncmd = [\"cargo\", \"test\"]\nslow_tests = 3\nslow_after_s = 300\n",
        )
        .unwrap();
        assert_eq!((c.gates[0].slow_tests, c.gates[0].slow_after_s), (None, None));
        assert_eq!(
            (c.gates[1].slow_tests, c.gates[1].slow_after_s),
            (Some(3), Some(300))
        );
    }

    #[test]
    fn slow_test_keys_of_zero_are_refused() {
        for key in ["slow_tests", "slow_after_s"] {
            let toml = format!("[[gate]]\nname = \"t\"\ncmd = [\"cargo\", \"test\"]\n{key} = 0\n");
            assert!(
                matches!(Config::from_toml(&toml).unwrap_err(), ConfigError::Invalid(_)),
                "{key} = 0"
            );
        }
    }

    #[test]
    fn worker_validation() {
        assert!(matches!(
            Config::from_toml("[worker]\npermission_mode = \"yolo\"\n").unwrap_err(),
            ConfigError::Invalid(_)
        ));
        assert_eq!(
            Config::from_toml("[worker]\npermission_mode = \"acceptEdits\"\n")
                .unwrap()
                .worker
                .permission_mode,
            "acceptEdits"
        );
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
