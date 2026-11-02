//! Methodology manifests (`methodologies/<name>/manifest.toml`).
//!
//! A methodology declares the stages that turn an idea into a task list and which built-in
//! parser turns the final artifact into issues. Parsing and validation are pure; the caller
//! injects file existence so this crate stays IO-free.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum MethodError {
    #[error("manifest parse error: {0}")]
    Parse(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default)]
    pub description: String,
}

fn default_language() -> String {
    "en".into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StageKind {
    /// A prompt file run headless (`claude -p`) with an optional JSON schema.
    Prompt,
    /// A Claude Code skill the operator runs interactively.
    Skill,
    /// Produced outside asf (another tool, by hand); asf only imports the outputs.
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Runner {
    Headless,
    Interactive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArtifactParser {
    #[serde(rename = "builtin:json-issues")]
    JsonIssues,
    #[serde(rename = "builtin:md-tasks")]
    MdTasks,
    #[serde(rename = "builtin:spec-kit-tasks")]
    SpecKitTasks,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stage {
    pub id: String,
    pub title: String,
    pub kind: StageKind,
    /// Prompt file, relative to the methodology dir (kind `prompt`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Skill name, e.g. `superpowers:writing-plans` (kind `skill`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill: Option<String>,
    /// Defaults to `headless` for `prompt` stages, `interactive` otherwise.
    pub runner: Runner,
    #[serde(default)]
    pub inputs: Vec<String>,
    /// Globs (relative to the repo) of the artifacts this stage produces.
    #[serde(default)]
    pub outputs: Vec<String>,
    /// JSON schema file, relative to the methodology dir.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// How asf turns the outputs into issues (required for `external`, optional otherwise).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parser: Option<ArtifactParser>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct IssueDefaults {
    pub labels: Vec<String>,
    pub priority: Option<u8>,
    pub estimate: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Manifest {
    pub methodology: Meta,
    pub stages: Vec<Stage>,
    pub issue_defaults: IssueDefaults,
}

#[derive(Deserialize)]
struct RawStage {
    id: String,
    title: String,
    kind: StageKind,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    skill: Option<String>,
    #[serde(default)]
    runner: Option<Runner>,
    #[serde(default)]
    inputs: Vec<String>,
    #[serde(default)]
    outputs: Vec<String>,
    #[serde(default)]
    schema: Option<String>,
    #[serde(default)]
    parser: Option<ArtifactParser>,
}

#[derive(Deserialize)]
struct RawManifest {
    methodology: Meta,
    #[serde(default, rename = "stage")]
    stages: Vec<RawStage>,
    #[serde(default)]
    issue_defaults: IssueDefaults,
}

pub fn parse_manifest(text: &str) -> Result<Manifest, MethodError> {
    let raw: RawManifest = toml::from_str(text).map_err(|e| MethodError::Parse(e.to_string()))?;
    let stages = raw
        .stages
        .into_iter()
        .map(|s| {
            let runner = s.runner.unwrap_or(match s.kind {
                StageKind::Prompt => Runner::Headless,
                StageKind::Skill | StageKind::External => Runner::Interactive,
            });
            Stage {
                id: s.id,
                title: s.title,
                kind: s.kind,
                prompt: s.prompt,
                skill: s.skill,
                runner,
                inputs: s.inputs,
                outputs: s.outputs,
                schema: s.schema,
                parser: s.parser,
            }
        })
        .collect();
    Ok(Manifest {
        methodology: raw.methodology,
        stages,
        issue_defaults: raw.issue_defaults,
    })
}

pub fn is_valid_method_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && name.len() <= 32
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A manifest-relative path must stay inside the methodology dir.
fn inside_dir(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with('/')
        && !p.starts_with('\\')
        && !p.contains(':')
        && !p.split(['/', '\\']).any(|seg| seg == "..")
}

/// Problems that make the manifest unusable; an empty list means valid. `exists` answers
/// whether a manifest-relative file (prompt, schema) is present.
pub fn validate(m: &Manifest, exists: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut out = Vec::new();
    if !is_valid_method_name(&m.methodology.name) {
        out.push(format!(
            "name `{}` must match ^[a-z][a-z0-9-]{{0,31}}$",
            m.methodology.name
        ));
    }
    if m.stages.is_empty() {
        out.push("at least one [[stage]] is required".into());
        return out;
    }
    let mut seen = std::collections::BTreeSet::new();
    for s in &m.stages {
        if s.id.is_empty() {
            out.push("stage with empty id".into());
        } else if !seen.insert(s.id.as_str()) {
            out.push(format!("duplicate stage id `{}`", s.id));
        }
        let id = &s.id;
        match s.kind {
            StageKind::Prompt => {
                if s.prompt.is_none() {
                    out.push(format!("stage `{id}`: kind `prompt` requires `prompt`"));
                }
            }
            StageKind::Skill => {
                if s.skill.is_none() {
                    out.push(format!("stage `{id}`: kind `skill` requires `skill`"));
                }
            }
            StageKind::External => {
                if s.outputs.is_empty() {
                    out.push(format!("stage `{id}`: kind `external` requires `outputs`"));
                }
                if s.parser.is_none() {
                    out.push(format!("stage `{id}`: kind `external` requires `parser`"));
                }
            }
        }
        for (what, path) in [("prompt", &s.prompt), ("schema", &s.schema)] {
            let Some(p) = path else { continue };
            if !inside_dir(p) {
                out.push(format!(
                    "stage `{id}`: {what} `{p}` must be a relative path inside the methodology dir"
                ));
            } else if !exists(p) {
                out.push(format!("stage `{id}`: {what} `{p}` not found"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(is_valid_method_name("asf-default"));
        assert!(is_valid_method_name("a"));
        assert!(!is_valid_method_name("Bad Name"));
        assert!(!is_valid_method_name("-x"));
        assert!(!is_valid_method_name(""));
    }

    #[test]
    fn paths_inside_dir() {
        assert!(inside_dir("prompts/a.md"));
        assert!(!inside_dir("../a.md"));
        assert!(!inside_dir("prompts/../../a.md"));
        assert!(!inside_dir("/etc/a"));
        assert!(!inside_dir("C:\\a.md"));
        assert!(!inside_dir(""));
    }
}
