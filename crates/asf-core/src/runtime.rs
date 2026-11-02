//! `AgentRuntime` port: how the factory starts, prompts and observes a coding agent.
//!
//! Adapter 1 is the Herdr CLI (`asf-runtime::HerdrRuntime`), adapter 2 a file-backed fake for
//! tests. State names mirror Herdr's `agent_status` so no mapping layer is needed.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::{str_enum, ParseEnumError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentState {
    Idle,
    Working,
    Blocked,
    Done,
    #[default]
    Unknown,
}
str_enum!(AgentState, "agent state", {
    Idle => "idle", Working => "working", Blocked => "blocked", Done => "done", Unknown => "unknown",
});

impl AgentState {
    /// A state in which the agent is waiting for input (prompt or approval) or has finished.
    pub fn is_settled(&self) -> bool {
        matches!(self, AgentState::Idle | AgentState::Done | AgentState::Blocked)
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RuntimeError {
    #[error("agent not ready: {0}")]
    NotReady(String),
    #[error("agent blocked: {0}")]
    Blocked(String),
    #[error("prompt stalled: {0}")]
    Stalled(String),
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("io: {0}")]
    Io(String),
}

impl RuntimeError {
    pub fn code(&self) -> &'static str {
        match self {
            RuntimeError::NotReady(_) => "not_ready",
            RuntimeError::Blocked(_) => "blocked",
            RuntimeError::Stalled(_) => "stalled",
            RuntimeError::Timeout(_) => "timeout",
            RuntimeError::NotFound(_) => "not_found",
            RuntimeError::Protocol(_) => "protocol",
            RuntimeError::Io(_) => "io",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRef {
    pub workspace_id: String,
    pub pane_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentHandle {
    pub name: String,
    pub pane_id: String,
    pub workspace_id: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AgentInfo {
    pub state: AgentState,
    pub revision: i64,
    pub state_change_seq: Option<i64>,
    pub completion_seq: Option<i64>,
    pub interactive_ready: bool,
    pub launch_pending: bool,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeInfo {
    pub name: String,
    pub version: String,
    pub protocol: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenWorkspace {
    pub cwd: PathBuf,
    pub label: String,
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartAgent {
    pub name: String,
    pub pane_id: String,
    pub args: Vec<String>,
    pub timeout_ms: u64,
}

/// The runtime port. Implementations are synchronous; the orchestrator loop is single-threaded.
pub trait AgentRuntime {
    fn health(&self) -> Result<RuntimeInfo, RuntimeError>;
    fn open_workspace(&self, r: &OpenWorkspace) -> Result<WorkspaceRef, RuntimeError>;
    /// `NotReady` when the agent is blocked during startup (e.g. a trust dialog).
    fn start_agent(&self, r: &StartAgent) -> Result<AgentInfo, RuntimeError>;
    /// Sends text + Enter, then waits for one of `until` (empty = any settled state).
    fn prompt(
        &self,
        h: &AgentHandle,
        text: &str,
        until: &[AgentState],
        timeout_ms: u64,
    ) -> Result<AgentInfo, RuntimeError>;
    fn wait(&self, h: &AgentHandle, until: &[AgentState], timeout_ms: u64)
        -> Result<AgentInfo, RuntimeError>;
    fn info(&self, h: &AgentHandle) -> Result<AgentInfo, RuntimeError>;
    /// Recent unwrapped screen text.
    fn read(&self, h: &AgentHandle, lines: u32) -> Result<String, RuntimeError>;
    fn send_keys(&self, h: &AgentHandle, keys: &[&str]) -> Result<(), RuntimeError>;
    fn focus(&self, h: &AgentHandle) -> Result<(), RuntimeError>;
    /// Sidebar metadata: title and `name=value` tokens (display only).
    fn report(
        &self,
        pane_id: &str,
        title: Option<&str>,
        tokens: &[(String, String)],
    ) -> Result<(), RuntimeError>;
    fn close_workspace(&self, workspace_id: &str) -> Result<(), RuntimeError>;
    fn notify(&self, title: &str, body: &str, sound: Option<&str>) -> Result<(), RuntimeError>;
}

impl fmt::Debug for dyn AgentRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<AgentRuntime>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_state_serde_and_parse() {
        assert_eq!(
            serde_json::to_string(&AgentState::Working).unwrap(),
            "\"working\""
        );
        assert_eq!("blocked".parse::<AgentState>().unwrap(), AgentState::Blocked);
        assert_eq!(AgentState::Done.to_string(), "done");
        assert!("nope".parse::<AgentState>().is_err());
        assert!(AgentState::Idle.is_settled());
        assert!(AgentState::Done.is_settled());
        assert!(AgentState::Blocked.is_settled());
        assert!(!AgentState::Working.is_settled());
        assert!(!AgentState::Unknown.is_settled());
    }

    #[test]
    fn runtime_error_display_and_code() {
        let e = RuntimeError::NotReady("agent x is blocked during startup".into());
        assert_eq!(e.code(), "not_ready");
        assert!(e.to_string().contains("blocked during startup"));
        assert_eq!(RuntimeError::Timeout("t".into()).code(), "timeout");
    }

    #[test]
    fn agent_info_default_is_unknown() {
        let i = AgentInfo::default();
        assert_eq!(i.state, AgentState::Unknown);
        assert!(!i.interactive_ready);
        assert!(i.session_id.is_none());
    }
}
