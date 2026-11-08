//! Comment-preserving edits of `.factory/config.toml`.
//!
//! Pure text in, text out: the caller reads the file, calls [`set_key`] and writes the returned
//! string. The result is validated with [`Config::from_toml`] before it is returned, so a refused
//! edit leaves nothing to write.

use toml_edit::{Decor, DocumentMut, Item, Table, TableLike, Value};

use crate::config::{Config, ConfigError};

/// Keys the CLI and the TUI may change, with a one-line description each.
const EDITABLE: &[(&str, &str)] = &[
    ("planner.model", "model used by `asf plan` (headless claude -p)"),
    ("planner.budget_usd", "USD budget per planning step"),
    (
        "worker.model",
        "model workers run on (unset: Claude Code default)",
    ),
    ("worker.budget_usd", "USD budget per issue"),
    ("review.required", "run a read-only reviewer before every merge"),
    ("review.model", "model the reviewer runs on"),
    (
        "review.max_rounds",
        "`changes` verdicts allowed before the human decides",
    ),
    ("scheduler.max_parallel", "workers running at the same time"),
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
