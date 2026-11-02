//! `asf method list` and `asf method validate <dir>`.
//!
//! Shipped methodologies are embedded in the binary; repo-local ones live under
//! `.factory/methodologies/<name>/manifest.toml`. Only `list` needs a repository.

use std::path::{Path, PathBuf};

use asf_core::method::{parse_manifest, validate, Manifest};
use serde::Serialize;

use crate::cli::{Cli, MethodCmd};
use crate::ctx::{Ctx, FACTORY_DIR};
use crate::output::{AppError, Rendered};

const LOCAL_DIR: &str = "methodologies";

/// (name, manifest, files shipped next to it)
const BUILTIN: &[(&str, &str, &[&str])] = &[
    (
        "asf-default",
        include_str!("../../../../methodologies/asf-default/manifest.toml"),
        &[
            "prompts/idea-to-prd.md",
            "prompts/prd-to-issues.md",
            "schemas/prd.json",
            "schemas/issues.json",
        ],
    ),
    (
        "spec-kit",
        include_str!("../../../../methodologies/spec-kit/manifest.toml"),
        &[],
    ),
    (
        "superpowers",
        include_str!("../../../../methodologies/superpowers/manifest.toml"),
        &[],
    ),
];

#[derive(Serialize)]
pub struct Row {
    pub name: String,
    pub version: String,
    pub language: String,
    pub description: String,
    pub stages: usize,
    /// `builtin` or the repo-relative directory.
    pub source: String,
    pub valid: bool,
    pub problems: Vec<String>,
    pub active: bool,
}

#[derive(Serialize)]
pub struct Validation {
    pub name: String,
    pub dir: String,
    pub stages: usize,
    pub valid: bool,
    pub problems: Vec<String>,
}

pub fn run(cli: &Cli, cmd: &MethodCmd) -> Result<Rendered, AppError> {
    match cmd {
        MethodCmd::List => list(cli),
        MethodCmd::Validate { dir } => validate_dir(dir),
    }
}

fn row(m: &Manifest, source: String, problems: Vec<String>, active: &str) -> Row {
    Row {
        name: m.methodology.name.clone(),
        version: m.methodology.version.clone(),
        language: m.methodology.language.clone(),
        description: m.methodology.description.clone(),
        stages: m.stages.len(),
        source,
        valid: problems.is_empty(),
        active: m.methodology.name == active,
        problems,
    }
}

fn list(cli: &Cli) -> Result<Rendered, AppError> {
    let ctx = Ctx::load(cli)?;
    let active = ctx.config.methodology.name.as_str();
    let mut rows: Vec<Row> = Vec::new();
    for (name, text, files) in BUILTIN {
        let m = parse_manifest(text)
            .map_err(|e| AppError::validation(format!("builtin manifest {name}: {e}")))?;
        let problems = validate(&m, &|p| files.contains(&p));
        rows.push(row(&m, "builtin".into(), problems, active));
    }
    let local_root = ctx.root.join(FACTORY_DIR).join(LOCAL_DIR);
    let mut local: Vec<PathBuf> = match std::fs::read_dir(&local_root) {
        Ok(rd) => rd
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.join("manifest.toml").is_file())
            .collect(),
        Err(_) => Vec::new(),
    };
    local.sort();
    for dir in local {
        let rel = format!(
            "{FACTORY_DIR}/{LOCAL_DIR}/{}",
            dir.file_name().unwrap_or_default().to_string_lossy()
        );
        match load(&dir) {
            Ok((m, problems)) => rows.push(row(&m, rel, problems, active)),
            Err(e) => rows.push(Row {
                name: dir.file_name().unwrap_or_default().to_string_lossy().to_string(),
                version: String::new(),
                language: String::new(),
                description: String::new(),
                stages: 0,
                source: rel,
                valid: false,
                problems: vec![e.message],
                active: false,
            }),
        }
    }
    let text = rows
        .iter()
        .map(|r| {
            format!(
                "{} {:<14} {:<7} {:<30} {} stage(s) {}{}",
                if r.active { "*" } else { " " },
                r.name,
                r.version,
                r.source,
                r.stages,
                if r.valid { "ok" } else { "INVALID" },
                if r.problems.is_empty() {
                    String::new()
                } else {
                    format!(": {}", r.problems.join("; "))
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Rendered::new(&rows, text)
}

/// Parses `<dir>/manifest.toml` and validates it against the files in `dir`.
fn load(dir: &Path) -> Result<(Manifest, Vec<String>), AppError> {
    let path = dir.join("manifest.toml");
    if !dir.is_dir() || !path.is_file() {
        return Err(AppError::not_found(format!(
            "no manifest.toml in {}",
            dir.display()
        )));
    }
    let raw = std::fs::read(&path).map_err(|e| AppError::io(format!("{}: {e}", path.display())))?;
    let text = String::from_utf8(raw)
        .map_err(|_| AppError::validation(format!("{} is not UTF-8", path.display())))?;
    let m = parse_manifest(text.trim_start_matches('\u{feff}'))
        .map_err(|e| AppError::validation(format!("{}: {e}", path.display())))?;
    let problems = validate(&m, &|p| dir.join(p).is_file());
    Ok((m, problems))
}

fn validate_dir(dir: &Path) -> Result<Rendered, AppError> {
    let (m, problems) = load(dir)?;
    if !problems.is_empty() {
        return Err(AppError::validation(format!(
            "{} manifest invalid:\n- {}",
            dir.display(),
            problems.join("\n- ")
        )));
    }
    let v = Validation {
        name: m.methodology.name.clone(),
        dir: dir.display().to_string(),
        stages: m.stages.len(),
        valid: true,
        problems,
    };
    let text = format!(
        "{} ok: {} stage(s) [{}]",
        v.name,
        v.stages,
        m.stages
            .iter()
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>()
            .join(" -> ")
    );
    Rendered::new(&v, text)
}
