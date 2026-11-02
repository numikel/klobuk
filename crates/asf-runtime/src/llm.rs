//! Structured-output LLM runs for planning: `claude -p --output-format json --json-schema …`.
//!
//! Contract verified live on Claude Code 2.1.316 (see project memory): the long prompt goes on
//! stdin, the schema inline in argv, and the answer is the `structured_output` object of the
//! single JSON result on stdout. Planning runs are read-only (`Read,Glob,Grep`) and budgeted.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use asf_core::RuntimeError;
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct StructuredRequest {
    /// Name of the step (`prd`, `issues`); the fake serves canned answers by it.
    pub schema_name: String,
    /// Short instruction passed as the `-p` argument.
    pub instruction: String,
    /// Long context written to stdin.
    pub stdin: String,
    pub schema: Value,
    pub model: String,
    pub max_turns: u32,
    pub budget_usd: f64,
    pub add_dirs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructuredResult {
    pub output: Value,
    pub cost_usd: f64,
    pub num_turns: i64,
    pub session_id: Option<String>,
}

pub trait Structured {
    fn run(&self, req: &StructuredRequest) -> Result<StructuredResult, RuntimeError>;
}

pub const READ_ONLY_TOOLS: &str = "Read,Glob,Grep";

/// argv for `claude` (without the program name). Pure, so it is unit-tested.
pub fn claude_args(req: &StructuredRequest) -> Vec<String> {
    let mut a = vec![
        "-p".to_string(),
        req.instruction.clone(),
        "--output-format".into(),
        "json".into(),
        "--json-schema".into(),
        serde_json::to_string(&req.schema).unwrap_or_else(|_| "{}".into()),
        "--model".into(),
        req.model.clone(),
        "--permission-mode".into(),
        "dontAsk".into(),
        "--tools".into(),
        READ_ONLY_TOOLS.into(),
        "--allowedTools".into(),
        READ_ONLY_TOOLS.into(),
        "--permission-prompts".into(),
        "none".into(),
        "--max-turns".into(),
        req.max_turns.to_string(),
        "--max-budget-usd".into(),
        format!("{:.2}", req.budget_usd),
        "--no-session-persistence".into(),
    ];
    for d in &req.add_dirs {
        a.push("--add-dir".into());
        a.push(d.clone());
    }
    a
}

/// Interprets stdout/stderr of a finished `claude -p` run.
pub fn parse_result(stdout: &str, stderr: &str) -> Result<StructuredResult, RuntimeError> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Err(RuntimeError::Protocol(format!(
            "claude produced no JSON result: {}",
            stderr.trim()
        )));
    }
    let v: Value = serde_json::from_str(trimmed).map_err(|e| {
        RuntimeError::Protocol(format!(
            "claude output is not JSON ({e}): {}",
            &trimmed[..trimmed.len().min(300)]
        ))
    })?;
    let subtype = v.get("subtype").and_then(Value::as_str).unwrap_or("unknown");
    let cost = v.get("total_cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
    let turns = v.get("num_turns").and_then(Value::as_i64).unwrap_or(0);
    let session_id = v.get("session_id").and_then(Value::as_str).map(str::to_string);
    if subtype != "success" {
        let errors = v
            .get("errors")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("; "))
            .unwrap_or_default();
        return Err(RuntimeError::Protocol(format!(
            "claude run ended with {subtype} after {turns} turn(s), ${cost:.3}: {errors}"
        )));
    }
    match v.get("structured_output") {
        Some(out) if !out.is_null() => Ok(StructuredResult {
            output: out.clone(),
            cost_usd: cost,
            num_turns: turns,
            session_id,
        }),
        _ => {
            let text = v.get("result").and_then(Value::as_str).unwrap_or("");
            Err(RuntimeError::Protocol(format!(
                "claude run succeeded but returned no structured_output; result text: {}",
                &text[..text.len().min(400)]
            )))
        }
    }
}

/// Runs the real `claude` binary with the repo as working directory.
pub struct ClaudeCli {
    pub bin: String,
    pub cwd: PathBuf,
}

impl ClaudeCli {
    pub fn new(cwd: &Path) -> Self {
        ClaudeCli {
            bin: "claude".into(),
            cwd: cwd.to_path_buf(),
        }
    }
}

impl Structured for ClaudeCli {
    fn run(&self, req: &StructuredRequest) -> Result<StructuredResult, RuntimeError> {
        let mut child = Command::new(&self.bin)
            .args(claude_args(req))
            .current_dir(&self.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| RuntimeError::Io(format!("cannot start {}: {e}", self.bin)))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(req.stdin.as_bytes())
                .map_err(|e| RuntimeError::Io(e.to_string()))?;
        }
        let out = child
            .wait_with_output()
            .map_err(|e| RuntimeError::Io(e.to_string()))?;
        parse_result(
            &String::from_utf8_lossy(&out.stdout),
            &String::from_utf8_lossy(&out.stderr),
        )
    }
}

/// File-backed fake: `{"responses": {"<schema_name>": <object>}, "log": [...]}`.
pub struct FakeLlm {
    path: PathBuf,
}

impl FakeLlm {
    pub fn new(path: &Path) -> Self {
        FakeLlm {
            path: path.to_path_buf(),
        }
    }

    fn load(&self) -> Result<Value, RuntimeError> {
        let raw = std::fs::read_to_string(&self.path)
            .map_err(|e| RuntimeError::Io(format!("{}: {e}", self.path.display())))?;
        serde_json::from_str(&raw).map_err(|e| RuntimeError::Io(e.to_string()))
    }

    pub fn log(&self) -> Vec<Value> {
        self.load()
            .ok()
            .and_then(|v| v.get("log").and_then(Value::as_array).cloned())
            .unwrap_or_default()
    }
}

impl Structured for FakeLlm {
    fn run(&self, req: &StructuredRequest) -> Result<StructuredResult, RuntimeError> {
        let mut state = self.load()?;
        let entry = json!({ "schema_name": req.schema_name, "instruction": req.instruction, "stdin": req.stdin, "model": req.model });
        match state.get_mut("log") {
            Some(Value::Array(a)) => a.push(entry),
            _ => {
                state["log"] = json!([entry]);
            }
        }
        std::fs::write(
            &self.path,
            serde_json::to_string_pretty(&state).map_err(|e| RuntimeError::Io(e.to_string()))?,
        )
        .map_err(|e| RuntimeError::Io(e.to_string()))?;
        let out = state
            .pointer(&format!("/responses/{}", req.schema_name))
            .cloned()
            .ok_or_else(|| {
                RuntimeError::Protocol(format!(
                    "fake llm has no canned response for step `{}`",
                    req.schema_name
                ))
            })?;
        Ok(StructuredResult {
            output: out,
            cost_usd: 0.0,
            num_turns: 1,
            session_id: Some("fake".into()),
        })
    }
}

/// `[planner] runner = "claude" | "fake"`.
pub fn planner_from_config(runner: &str, root: &Path) -> Result<Box<dyn Structured>, RuntimeError> {
    match runner {
        "claude" => Ok(Box::new(ClaudeCli::new(root))),
        "fake" => Ok(Box::new(FakeLlm::new(
            &root.join(".factory").join("fake-llm.json"),
        ))),
        other => Err(RuntimeError::Protocol(format!(
            "unknown planner runner `{other}` (expected claude or fake)"
        ))),
    }
}
