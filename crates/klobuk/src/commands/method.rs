//! `klobuk method list` and `klobuk method validate <dir>`.
//!
//! Shipped methodologies are embedded in the binary; repo-local ones live under
//! `.factory/methodologies/<name>/manifest.toml`. Only `list` needs a repository.

use std::path::{Path, PathBuf};

use klobuk_core::method::{is_valid_method_name, parse_manifest, validate, Manifest, StageKind};
use serde::Serialize;
use serde_json::Value;

use crate::cli::{Cli, MethodCmd};
use crate::ctx::{Ctx, FACTORY_DIR};
use crate::output::{AppError, Rendered};

const LOCAL_DIR: &str = "methodologies";

/// (name, manifest, files shipped next to it as (manifest-relative path, content))
type Builtin = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
);

const BUILTIN: &[Builtin] = &[
    (
        "klobuk-default",
        include_str!("../../methodologies/klobuk-default/manifest.toml"),
        &[
            (
                "prompts/idea-to-prd.md",
                include_str!("../../methodologies/klobuk-default/prompts/idea-to-prd.md"),
            ),
            (
                "prompts/prd-to-issues.md",
                include_str!("../../methodologies/klobuk-default/prompts/prd-to-issues.md"),
            ),
            (
                "schemas/prd.json",
                include_str!("../../methodologies/klobuk-default/schemas/prd.json"),
            ),
            (
                "schemas/issues.json",
                include_str!("../../methodologies/klobuk-default/schemas/issues.json"),
            ),
        ],
    ),
    (
        "spec-kit",
        include_str!("../../methodologies/spec-kit/manifest.toml"),
        &[],
    ),
    (
        "superpowers",
        include_str!("../../methodologies/superpowers/manifest.toml"),
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
        let problems = validate(&m, &|p| files.iter().any(|(f, _)| *f == p));
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

/// A `prompt` stage of the active methodology with its prompt and schema already read.
#[derive(Debug, Clone)]
pub struct PromptStage {
    /// Prompt text exactly as stored in the methodology (embedded or read from disk).
    pub prompt: String,
    /// File stem of the schema (`schemas/prd.json` -> `prd`); names the planner request and its artifact.
    pub schema_name: String,
    pub schema: Value,
}

/// The `prompt` stages of the active methodology, in manifest order.
#[derive(Debug, Clone)]
pub struct ActivePrompts {
    /// `[methodology] language` of the manifest (`en` unless declared); consumed by the
    /// `{{language}}` placeholder of the prompts (asf-9).
    #[allow(dead_code)]
    pub language: String,
    pub stages: Vec<PromptStage>,
}

/// Where a methodology's files come from.
enum Files {
    Builtin(&'static [(&'static str, &'static str)]),
    Dir(PathBuf),
}

impl Files {
    fn read(&self, rel: &str) -> Result<String, AppError> {
        match self {
            Files::Builtin(files) => files
                .iter()
                .find(|(f, _)| *f == rel)
                .map(|(_, text)| (*text).to_string())
                .ok_or_else(|| AppError::validation(format!("builtin file {rel} missing"))),
            Files::Dir(dir) => {
                let path = dir.join(rel);
                let raw =
                    std::fs::read(&path).map_err(|e| AppError::io(format!("{}: {e}", path.display())))?;
                let text = String::from_utf8(raw)
                    .map_err(|_| AppError::validation(format!("{} is not UTF-8", path.display())))?;
                Ok(text.strip_prefix('\u{feff}').unwrap_or(&text).to_string())
            }
        }
    }
}

/// Finds `name`: a repo-local methodology (`.factory/methodologies/<name>/`) shadows a shipped one.
fn resolve(ctx: &Ctx, name: &str) -> Result<(Manifest, Files), AppError> {
    let not_found = || {
        AppError::not_found(format!(
            "methodology `{name}` not found (see `klobuk method list`)"
        ))
    };
    if !is_valid_method_name(name) {
        return Err(not_found());
    }
    let dir = ctx.root.join(FACTORY_DIR).join(LOCAL_DIR).join(name);
    let (m, problems, files) = if dir.join("manifest.toml").is_file() {
        let (m, problems) = load(&dir)?;
        (m, problems, Files::Dir(dir))
    } else {
        let (_, text, files) = BUILTIN
            .iter()
            .find(|(n, _, _)| *n == name)
            .ok_or_else(not_found)?;
        let m = parse_manifest(text)
            .map_err(|e| AppError::validation(format!("builtin manifest {name}: {e}")))?;
        let problems = validate(&m, &|p| files.iter().any(|(f, _)| *f == p));
        (m, problems, Files::Builtin(files))
    };
    if m.methodology.name != name {
        return Err(AppError::validation(format!(
            "methodology directory `{name}` declares name `{}`",
            m.methodology.name
        )));
    }
    if !problems.is_empty() {
        return Err(AppError::validation(format!(
            "methodology `{name}` is invalid:\n- {}",
            problems.join("\n- ")
        )));
    }
    Ok((m, files))
}

/// Resolves the active methodology (`[methodology] name`) and loads the prompt and schema of each
/// of its `prompt` stages. Fails when the methodology is unknown, invalid, or has no prompt stage.
pub fn active_prompts(ctx: &Ctx) -> Result<ActivePrompts, AppError> {
    let name = ctx.config.methodology.name.as_str();
    let (m, files) = resolve(ctx, name)?;
    let mut stages = Vec::new();
    for s in m.stages.iter().filter(|s| s.kind == StageKind::Prompt) {
        let (Some(prompt), Some(schema)) = (&s.prompt, &s.schema) else {
            return Err(AppError::validation(format!(
                "methodology `{name}`: prompt stage `{}` needs both `prompt` and `schema` to run headless",
                s.id
            )));
        };
        let schema_text = files.read(schema)?;
        let schema_value: Value = serde_json::from_str(&schema_text)
            .map_err(|e| AppError::validation(format!("methodology `{name}`: {schema}: {e}")))?;
        let schema_name = Path::new(schema)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| s.id.clone());
        stages.push(PromptStage {
            prompt: files.read(prompt)?,
            schema_name,
            schema: schema_value,
        });
    }
    if stages.is_empty() {
        return Err(AppError::validation(format!(
            "methodology `{name}` has no `prompt` stage; use `klobuk plan import` for plans produced elsewhere"
        )));
    }
    Ok(ActivePrompts {
        language: m.methodology.language.clone(),
        stages,
    })
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
