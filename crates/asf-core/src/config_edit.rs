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

/// The keys [`set_key`] accepts, each with a one-line description.
pub fn editable_keys() -> &'static [(&'static str, &'static str)] {
    EDITABLE
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
    /// Free text (typed with Enter). No editable key is plain text yet.
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
        match self.key.split('.').try_fold(&root, |node, seg| node.get(seg))? {
            serde_json::Value::Null => None,
            serde_json::Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        }
    }
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
        help: "minutes of waiting before the first reminder (0: no reminders)",
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

    let mut table: &mut dyn TableLike = doc.as_table_mut();
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
const UNSETTABLE: &[&str] = &["worker.model", "review.model"];

/// The keys [`unset_key`] accepts.
pub fn unsettable_keys() -> &'static [&'static str] {
    UNSETTABLE
}

/// Removes the dotted `key` from the TOML document `toml` and returns the new text.
///
/// Only optional keys (`worker.model`, `review.model`) can be removed; any other key, known or not,
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
    let mut table: &mut dyn TableLike = doc.as_table_mut();
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
    fn every_editable_key_is_dotted_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for (k, _) in EDITABLE {
            assert_eq!(k.split('.').count(), 2, "{k}");
            assert!(seen.insert(*k), "duplicate {k}");
        }
        assert_eq!(EDITABLE.len(), 13);
    }
}
