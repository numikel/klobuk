//! `asf config show` and `asf config set <key> <value>`.
//!
//! Both work on `.factory/config.toml` only; neither opens the issue store. `set` is the text edit of
//! `asf_core::config_edit::set_key`, written back as UTF-8 without BOM and with LF endings, and only
//! when the edit succeeded — a refused key or value leaves the file byte-identical.

use std::path::PathBuf;

use asf_core::config_edit::{apply_note, editable_keys, lookup, set_key};
use asf_core::Config;
use serde::Serialize;
use serde_json::Value;

use crate::cli::{Cli, ConfigCmd};
use crate::ctx::{start_dir, CONFIG_FILE, FACTORY_DIR};
use crate::git;
use crate::output::{AppError, ErrorCode, Rendered};

pub fn run(cli: &Cli, cmd: &ConfigCmd) -> Result<Rendered, AppError> {
    match cmd {
        ConfigCmd::Show => show(cli),
        ConfigCmd::Set { key, value } => set(cli, key, value),
    }
}

/// The config file and its text with the BOM dropped and line endings normalised to LF.
fn read_config(cli: &Cli) -> Result<(PathBuf, String), AppError> {
    let root = git::locate_root(&start_dir(cli)?)?;
    let path = root.join(FACTORY_DIR).join(CONFIG_FILE);
    if !path.exists() {
        return Err(AppError::not_found(format!(
            "no {FACTORY_DIR}/{CONFIG_FILE} in {} (run `asf repo init`)",
            root.display()
        )));
    }
    let raw = std::fs::read_to_string(&path)?;
    let text = raw.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    Ok((path, text))
}

/// The value of the dotted `key` in `config`; `null` for an unset optional such as `worker.model`.
fn value_of(config: &Config, key: &str) -> Result<Value, AppError> {
    let root = serde_json::to_value(config).map_err(|e| AppError::new(ErrorCode::Backend, e.to_string()))?;
    Ok(lookup(&root, key).cloned().unwrap_or(Value::Null))
}

fn display(value: &Value) -> String {
    match value {
        Value::Null => "(unset)".to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[derive(Serialize)]
struct KeyView {
    key: &'static str,
    value: Value,
    description: &'static str,
}

fn show(cli: &Cli) -> Result<Rendered, AppError> {
    let (_, text) = read_config(cli)?;
    let config = Config::from_toml(&text)?;
    let mut rows = Vec::new();
    for (key, description) in editable_keys() {
        rows.push(KeyView {
            key,
            value: value_of(&config, key)?,
            description,
        });
    }
    let width = rows.iter().map(|r| r.key.len()).max().unwrap_or(0);
    let human = rows
        .iter()
        .map(|r| format!("{:<width$}  {:<16}  {}", r.key, display(&r.value), r.description))
        .collect::<Vec<_>>()
        .join("\n");
    Rendered::new(&rows, human)
}

#[derive(Serialize)]
struct SetView {
    key: String,
    old: Value,
    new: Value,
    note: &'static str,
}

fn set(cli: &Cli, key: &str, value: &str) -> Result<Rendered, AppError> {
    let (path, text) = read_config(cli)?;
    let old = value_of(&Config::from_toml(&text)?, key)?;
    let edited = set_key(&text, key, value)?;
    let new = value_of(&Config::from_toml(&edited)?, key)?;
    std::fs::write(&path, edited.as_bytes())?;
    // The same sentence the TUI Config view shows, so the two cannot disagree.
    let note = apply_note(key);
    let human = format!("{key}: {} -> {}\n{note}", display(&old), display(&new));
    Rendered::new(
        &SetView {
            key: key.to_string(),
            old,
            new,
            note,
        },
        human,
    )
}
