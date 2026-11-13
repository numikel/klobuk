//! Comment-preserving edits of `.factory/config.toml`.
//!
//! Pure text in, text out: the caller reads the file, calls [`set_key`] and writes the returned
//! string. The result is validated with [`Config::from_toml`] before it is returned, so a refused
//! edit leaves nothing to write.

use toml_edit::{Decor, DocumentMut, Item, Table, TableLike, Value};

use crate::config::{Config, ConfigError};

/// Keys the CLI and the TUI may change, with a one-line description each. The order is the order of
/// the Config view (models, budgets, flow, notifications); [`FIELDS`] lists the same keys in it.
const EDITABLE: &[(&str, &str)] = &[
    ("planner.model", "model used by `asf plan` (headless claude -p)"),
    (
        "worker.model",
        "model workers run on (unset: Claude Code default)",
    ),
    ("review.model", "model the reviewer runs on"),
    ("planner.budget_usd", "USD budget per planning step"),
    ("worker.budget_usd", "USD budget per issue"),
    ("scheduler.max_parallel", "workers running at the same time"),
    ("review.required", "run a read-only reviewer before every merge"),
    (
        "review.max_rounds",
        "`changes` verdicts allowed before the human decides",
    ),
    (
        "merge.target",
        "local (rebase + fast-forward) or github (push + PR)",
    ),
    (
        "merge.verify_main_after",
        "run the gates in the repo root after each merge",
    ),
    (
        "merge.clean_after",
        "what the queue removes after a merge: worktree, branch, review_worktrees (a list; [] keeps all)",
    ),
    (
        "worker.remote_control",
        "worker and reviewer sessions join Remote Control (claude.ai/code, mobile app)",
    ),
    (
        "serve.interval_s",
        "seconds between ticks of `asf serve` (the --interval flag overrides it)",
    ),
    (
        "serve.max_ticks",
        "with --until-done: stop `asf serve` after this many ticks (unset: no limit; --max-ticks overrides it)",
    ),
    (
        "gate.slow_tests",
        "stop the `test` gate once this many tests print `has been running for over 60 seconds` (unset: never; set per [[gate]] in config.toml, here for the one named `test`)",
    ),
    (
        "gate.slow_after_s",
        "stop the `test` gate when its first such slow-test line is this many seconds old and it is still running (unset: never; per [[gate]] in config.toml, here for the one named `test`)",
    ),
    (
        "notify.on_new",
        "notify at once when something new waits for you (false: only reminders)",
    ),
    (
        "notify.after_minutes",
        "minutes before `asf serve` first reminds you of something waiting (0: off)",
    ),
    (
        "notify.every_minutes",
        "minutes between repeated reminders (0: remind once)",
    ),
];

/// The `gate.*` keys are not a section of `config.toml` but a field of one `[[gate]]` table. They
/// address the gate with this `name`, the one that runs `cargo test` in the dogfooding config.
const GATE_TARGET: &str = "test";
const GATE_PREFIX: &str = "gate.";

/// The keys [`set_key`] accepts, each with a one-line description.
pub fn editable_keys() -> &'static [(&'static str, &'static str)] {
    EDITABLE
}

/// When a changed key reaches the code that uses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applies {
    /// A running `asf serve` re-reads `config.toml` at the start of every tick (`ConfigWatch::refresh`
    /// in `crates/asf/src/ctx.rs`, called from `serve::run`) and the stage that uses the key takes it
    /// from that context.
    NextTick,
    /// Only the one-shot `asf plan` reads it, from a fresh load each run; `asf serve` never uses it.
    NextPlan,
    /// Read once at startup (or not shown to be reloaded): a running `asf serve` must be restarted.
    Restart,
}

/// How each key of [`editable_keys`] reaches its user, in the same order. `planner.*` is read only by
/// `commands/plan.rs`; every other key is read from `ctx.config` by a stage of the serve tick
/// (`worker.rs`, `commands/merge.rs`, `commands/run.rs`, `commands/remind.rs`). A new editable key
/// must be added here (a test checks it).
const APPLIES: &[(&str, Applies)] = &[
    ("planner.model", Applies::NextPlan),
    ("worker.model", Applies::NextTick),
    ("review.model", Applies::NextTick),
    ("planner.budget_usd", Applies::NextPlan),
    ("worker.budget_usd", Applies::NextTick),
    ("scheduler.max_parallel", Applies::NextTick),
    ("review.required", Applies::NextTick),
    ("review.max_rounds", Applies::NextTick),
    ("merge.target", Applies::NextTick),
    ("merge.verify_main_after", Applies::NextTick),
    ("merge.clean_after", Applies::NextTick),
    ("worker.remote_control", Applies::NextTick),
    // `serve::serve_loop` takes both from the refreshed config every tick, unless a flag sets them
    ("serve.interval_s", Applies::NextTick),
    ("serve.max_ticks", Applies::NextTick),
    // `stage_specs` builds the gate specs from the refreshed config for every run
    ("gate.slow_tests", Applies::NextTick),
    ("gate.slow_after_s", Applies::NextTick),
    ("notify.on_new", Applies::NextTick),
    ("notify.after_minutes", Applies::NextTick),
    ("notify.every_minutes", Applies::NextTick),
];

/// When a change of `key` takes effect. A key that is not classified counts as [`Applies::Restart`],
/// the one claim that is safe without proof.
pub fn applies(key: &str) -> Applies {
    APPLIES
        .iter()
        .find(|(k, _)| *k == key)
        .map_or(Applies::Restart, |(_, a)| *a)
}

/// The one true sentence about when a change of `key` takes effect. The CLI (`asf config set`) and
/// the TUI Config view both print this, so they cannot disagree.
pub fn apply_note(key: &str) -> &'static str {
    match applies(key) {
        Applies::NextTick => "a running `asf serve` picks this up on its next tick",
        Applies::NextPlan => "the next `asf plan` uses this; `asf serve` never does",
        Applies::Restart => "restart `asf serve` to apply",
    }
}

/// The section of the Config view a field is listed under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigGroup {
    Models,
    Budgets,
    Flow,
    Notifications,
}

impl ConfigGroup {
    /// The groups in the order the view lists them.
    pub const ALL: [ConfigGroup; 4] = [
        ConfigGroup::Models,
        ConfigGroup::Budgets,
        ConfigGroup::Flow,
        ConfigGroup::Notifications,
    ];

    pub fn title(self) -> &'static str {
        match self {
            ConfigGroup::Models => "Models",
            ConfigGroup::Budgets => "Budgets",
            ConfigGroup::Flow => "Flow",
            ConfigGroup::Notifications => "Notifications",
        }
    }
}

/// What kind of value a field holds, and so how the Config view edits it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    /// One of a short list (`h`/`l` step through it). A stored value outside the list is legal: the view
    /// keeps it, and a custom value is typed with Enter.
    Choice(&'static [&'static str]),
    /// On or off (`Space` toggles it).
    Bool,
    /// A whole number or an amount in USD (typed with Enter).
    Number,
    /// Free text or a TOML list (typed with Enter): `merge.clean_after`.
    Text,
}

/// One editable key as the Config view presents it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigField {
    /// The dotted key, one of [`editable_keys`].
    pub key: &'static str,
    /// The name the view shows instead of the dotted key.
    pub label: &'static str,
    pub group: ConfigGroup,
    pub kind: FieldKind,
    /// One line in plain words.
    pub help: &'static str,
}

impl ConfigField {
    /// The default of the key as the text [`set_key`] takes, read from [`Config::default`]. `None` when
    /// the key is optional and absent by default (`worker.model`): returning to the default then means
    /// [`unset_key`].
    pub fn default_value(&self) -> Option<String> {
        let root = serde_json::to_value(Config::default()).ok()?;
        match lookup(&root, self.key)? {
            serde_json::Value::Null => None,
            serde_json::Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        }
    }
}

/// The value of the dotted `key` in `root`, a [`Config`] serialized to JSON; `None` when the path does
/// not exist. A `gate.*` key is read from the `[[gate]]` named `test`, because `gate` is an array.
/// An unset optional key is `Some(Null)`: callers treat both alike.
pub fn lookup<'a>(root: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    let mut path = key.split('.');
    let mut node = root;
    if key.starts_with(GATE_PREFIX) {
        path.next();
        node = root
            .get("gate")?
            .as_array()?
            .iter()
            .find(|g| g.get("name").and_then(serde_json::Value::as_str) == Some(GATE_TARGET))?;
    }
    path.try_fold(node, |n, seg| n.get(seg))
}

/// The `[[gate]]` table the `gate.*` keys edit, if the document has one.
fn target_gate(doc: &mut DocumentMut) -> Option<&mut Table> {
    doc.get_mut("gate")?
        .as_array_of_tables_mut()?
        .iter_mut()
        .find(|g| g.get("name").and_then(Item::as_str) == Some(GATE_TARGET))
}

/// The three model keys take these names; any other model id is kept as it is.
const MODELS: &[&str] = &["opus", "sonnet", "haiku"];

const FIELDS: &[ConfigField] = &[
    ConfigField {
        key: "planner.model",
        label: "Planning",
        group: ConfigGroup::Models,
        kind: FieldKind::Choice(MODELS),
        help: "turns an idea into issues (`asf plan new`)",
    },
    ConfigField {
        key: "worker.model",
        label: "Workers",
        group: ConfigGroup::Models,
        kind: FieldKind::Choice(MODELS),
        help: "writes the code for each issue",
    },
    ConfigField {
        key: "review.model",
        label: "Reviewer",
        group: ConfigGroup::Models,
        kind: FieldKind::Choice(MODELS),
        help: "reads the diff with a clean context before merge",
    },
    ConfigField {
        key: "planner.budget_usd",
        label: "Planning step",
        group: ConfigGroup::Budgets,
        kind: FieldKind::Number,
        help: "ceiling for one planning call",
    },
    ConfigField {
        key: "worker.budget_usd",
        label: "Per issue",
        group: ConfigGroup::Budgets,
        kind: FieldKind::Number,
        help: "the worker is stopped when it is exceeded",
    },
    ConfigField {
        key: "scheduler.max_parallel",
        label: "Workers at once",
        group: ConfigGroup::Flow,
        kind: FieldKind::Number,
        help: "how many issues run in parallel",
    },
    ConfigField {
        key: "review.required",
        label: "Review before merge",
        group: ConfigGroup::Flow,
        kind: FieldKind::Bool,
        help: "every merge waits for the reviewer's approve",
    },
    ConfigField {
        key: "review.max_rounds",
        label: "Review rounds",
        group: ConfigGroup::Flow,
        kind: FieldKind::Number,
        help: "`changes` verdicts before the queue asks you",
    },
    ConfigField {
        key: "merge.target",
        label: "Merge target",
        group: ConfigGroup::Flow,
        kind: FieldKind::Choice(&["local", "github"]),
        help: "fast-forward locally, or pull requests",
    },
    ConfigField {
        key: "merge.verify_main_after",
        label: "Gates on main",
        group: ConfigGroup::Flow,
        kind: FieldKind::Bool,
        help: "run the gates again after each merge",
    },
    ConfigField {
        key: "merge.clean_after",
        label: "Clean up after merge",
        group: ConfigGroup::Flow,
        kind: FieldKind::Text,
        help: "worktree, branch, review_worktrees; [] keeps all",
    },
    ConfigField {
        key: "worker.remote_control",
        label: "Remote control",
        group: ConfigGroup::Flow,
        kind: FieldKind::Bool,
        help: "sessions show up in claude.ai/code and mobile app",
    },
    ConfigField {
        key: "serve.interval_s",
        label: "Loop interval (s)",
        group: ConfigGroup::Flow,
        kind: FieldKind::Number,
        help: "seconds between ticks of `asf serve`",
    },
    ConfigField {
        key: "serve.max_ticks",
        label: "Loop tick limit",
        group: ConfigGroup::Flow,
        kind: FieldKind::Number,
        help: "stop after this many ticks with --until-done",
    },
    ConfigField {
        key: "gate.slow_tests",
        label: "Slow tests to stop",
        group: ConfigGroup::Flow,
        kind: FieldKind::Number,
        help: "stop `test` gate at this many tests over 60 s",
    },
    ConfigField {
        key: "gate.slow_after_s",
        label: "Slow test wait (s)",
        group: ConfigGroup::Flow,
        kind: FieldKind::Number,
        help: "stop `test` gate if a slow test is this old (s)",
    },
    ConfigField {
        key: "notify.on_new",
        label: "Notify at once",
        group: ConfigGroup::Notifications,
        kind: FieldKind::Bool,
        help: "tell you the moment something new waits for you",
    },
    ConfigField {
        key: "notify.after_minutes",
        label: "First reminder",
        group: ConfigGroup::Notifications,
        kind: FieldKind::Number,
        help: "minutes before the first reminder (0: off)",
    },
    ConfigField {
        key: "notify.every_minutes",
        label: "Repeat reminder",
        group: ConfigGroup::Notifications,
        kind: FieldKind::Number,
        help: "minutes between reminders (0: remind once)",
    },
];

/// Every editable key as the Config view presents it, in [`editable_keys`] order: label, group, kind,
/// choices and a plain-words help line. The default of each comes from [`ConfigField::default_value`].
pub fn fields() -> &'static [ConfigField] {
    FIELDS
}

/// Sets the dotted `key` to `value` in the TOML document `toml` and returns the new text.
///
/// Comments, key order and unrelated content survive, and an inline comment after the old value
/// stays on the line. A missing section is created. `value` is read as a TOML scalar or array
/// (`true`, `5`, `2.5`, `["a"]`, `"quoted"`); anything else is taken as a plain string.
///
/// Errors when `key` is not in [`editable_keys`], when `toml` does not parse, or when the edited
/// document does not pass [`Config::from_toml`]; in every case the input is untouched.
pub fn set_key(toml: &str, key: &str, value: &str) -> Result<String, ConfigError> {
    if !EDITABLE.iter().any(|(k, _)| *k == key) {
        return Err(ConfigError::Invalid(format!(
            "unknown or non-editable config key `{key}`"
        )));
    }
    let mut doc: DocumentMut = toml
        .parse()
        .map_err(|e: toml_edit::TomlError| ConfigError::Parse(e.to_string()))?;

    let mut path: Vec<&str> = key.split('.').collect();
    let leaf = path.pop().unwrap_or_default();

    let mut table: &mut dyn TableLike = if key.starts_with(GATE_PREFIX) {
        path.clear();
        target_gate(&mut doc).ok_or_else(|| {
            ConfigError::Invalid(format!(
                "`{key}` edits the [[gate]] named `{GATE_TARGET}`, and config.toml has none"
            ))
        })?
    } else {
        doc.as_table_mut()
    };
    for segment in path {
        let current = table;
        let item = current.entry(segment).or_insert(Item::Table(Table::new()));
        if let Some(t) = item.as_table_mut() {
            // A parent that only existed through `[a.b]` is implicit and would print no header.
            t.set_implicit(false);
        }
        table = item
            .as_table_like_mut()
            .ok_or_else(|| ConfigError::Invalid(format!("`{segment}` in `{key}` is not a table")))?;
    }

    let mut new = parse_value(value);
    match table.get_mut(leaf) {
        Some(Item::Value(old)) => {
            // Keeps the spacing and the trailing `# comment` of the line being replaced.
            *new.decor_mut() = old.decor().clone();
            *old = new;
        }
        Some(_) => {
            return Err(ConfigError::Invalid(format!("`{key}` is not a plain value")));
        }
        None => {
            table.insert(leaf, Item::Value(new));
        }
    }

    let out = doc.to_string();
    Config::from_toml(&out)?;
    Ok(out)
}

/// Optional keys: removing one is safe because the loader falls back to a default (`worker.model`
/// to the Claude Code default, `review.model` to `opus`). Every other key is required.
const UNSETTABLE: &[&str] = &[
    "worker.model",
    "review.model",
    "serve.max_ticks",
    "gate.slow_tests",
    "gate.slow_after_s",
];

/// The keys [`unset_key`] accepts.
pub fn unsettable_keys() -> &'static [&'static str] {
    UNSETTABLE
}

/// Removes the dotted `key` from the TOML document `toml` and returns the new text.
///
/// Only optional keys (`worker.model`, `review.model`, `serve.max_ticks`, `gate.slow_tests`,
/// `gate.slow_after_s`) can be removed; any other key, known or not,
/// is refused with [`ConfigError::Invalid`]. Removing a key that is absent returns `toml` unchanged.
/// Other comments, key order and formatting survive; the comment lines directly above the removed
/// key go with it. The result is validated with [`Config::from_toml`] before it is returned.
pub fn unset_key(toml: &str, key: &str) -> Result<String, ConfigError> {
    if !UNSETTABLE.contains(&key) {
        return Err(ConfigError::Invalid(format!(
            "config key `{key}` is required and cannot be unset"
        )));
    }
    let mut doc: DocumentMut = toml
        .parse()
        .map_err(|e: toml_edit::TomlError| ConfigError::Parse(e.to_string()))?;

    let mut path: Vec<&str> = key.split('.').collect();
    let leaf = path.pop().unwrap_or_default();
    let mut table: &mut dyn TableLike = if key.starts_with(GATE_PREFIX) {
        path.clear();
        match target_gate(&mut doc) {
            Some(gate) => gate,
            None => return Ok(toml.to_string()), // no such gate, so no such key
        }
    } else {
        doc.as_table_mut()
    };
    for segment in path {
        let Some(next) = table.get_mut(segment).and_then(Item::as_table_like_mut) else {
            return Ok(toml.to_string()); // the section is absent, so is the key
        };
        table = next;
    }
    if table.remove(leaf).is_none() {
        return Ok(toml.to_string());
    }

    let out = doc.to_string();
    Config::from_toml(&out)?;
    Ok(out)
}

/// A TOML scalar or array when `raw` is one, otherwise `raw` as a string.
fn parse_value(raw: &str) -> Value {
    let raw = raw.trim();
    match raw.parse::<Value>() {
        // Tables are not settable here and a bare date is far likelier a name than a datetime.
        Ok(v) if v.is_inline_table() || v.is_datetime() => Value::from(raw),
        Ok(mut v) => {
            *v.decor_mut() = Decor::default();
            v
        }
        Err(_) => Value::from(raw),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_value_types() {
        assert!(parse_value("true").is_bool());
        assert!(parse_value("42").is_integer());
        assert!(parse_value("2.5").is_float());
        assert!(parse_value("\"a b\"").is_str());
        assert!(parse_value("opus").is_str());
        assert_eq!(parse_value("  opus  ").as_str(), Some("opus"));
        assert!(parse_value("").is_str());
        let arr = parse_value("[\"a\", \"b\"]");
        assert_eq!(arr.as_array().map(|a| a.len()), Some(2));
    }

    #[test]
    fn parse_value_keeps_tables_and_dates_as_strings() {
        assert_eq!(parse_value("{ a = 1 }").as_str(), Some("{ a = 1 }"));
        assert_eq!(parse_value("2026-11-05").as_str(), Some("2026-11-05"));
    }

    #[test]
    fn every_unsettable_key_is_editable() {
        for k in UNSETTABLE {
            assert!(EDITABLE.iter().any(|(e, _)| e == k), "{k}");
        }
    }

    #[test]
    fn every_editable_key_is_classified_once_in_the_same_order() {
        let editable: Vec<&str> = EDITABLE.iter().map(|(k, _)| *k).collect();
        let classified: Vec<&str> = APPLIES.iter().map(|(k, _)| *k).collect();
        assert_eq!(editable, classified);
    }

    #[test]
    fn every_editable_key_is_dotted_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for (k, _) in EDITABLE {
            assert_eq!(k.split('.').count(), 2, "{k}");
            assert!(seen.insert(*k), "duplicate {k}");
        }
        assert_eq!(EDITABLE.len(), 19);
    }
}
