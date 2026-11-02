//! File-backed fake runtime: lets several `asf` processes (CLI tests) share one simulated Herdr.
//!
//! State lives in a JSON file. A `script` can pre-seed responses: `start_agent` and `info` are
//! queues consumed in order (`{"error":"not_ready"}` or `{"state":"idle","session_id":"…"}`);
//! `screen` seeds what `read` returns. Every call is appended to `log`.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};

use asf_core::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LogEntry {
    pub op: String,
    pub args: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Agent {
    info: AgentInfo,
    pane_id: String,
    workspace_id: String,
    screen: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Script {
    #[serde(default)]
    start_agent: VecDeque<Value>,
    #[serde(default)]
    info: VecDeque<Value>,
    #[serde(default)]
    screen: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct State {
    script: Script,
    next_ws: u32,
    workspaces: BTreeMap<String, String>,
    agents: BTreeMap<String, Agent>,
    log: Vec<LogEntry>,
}

pub struct FakeRuntime {
    path: PathBuf,
}

fn io(e: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::Io(e.to_string())
}

impl FakeRuntime {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        FakeRuntime { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn load(&self) -> Result<State, RuntimeError> {
        if !self.path.exists() {
            return Ok(State::default());
        }
        let raw = std::fs::read_to_string(&self.path).map_err(io)?;
        serde_json::from_str(&raw).map_err(io)
    }

    fn save(&self, st: &State) -> Result<(), RuntimeError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        std::fs::write(&self.path, serde_json::to_string_pretty(st).map_err(io)?).map_err(io)
    }

    /// Mutates state under load/save, logging the call first.
    fn with<T>(
        &self,
        op: &str,
        args: Value,
        f: impl FnOnce(&mut State) -> Result<T, RuntimeError>,
    ) -> Result<T, RuntimeError> {
        let mut st = self.load()?;
        st.log.push(LogEntry {
            op: op.to_string(),
            args,
        });
        let out = f(&mut st);
        self.save(&st)?;
        out
    }

    /// Seeds scripted responses (merged over the current script).
    pub fn script(&self, v: Value) -> Result<(), RuntimeError> {
        let mut st = self.load()?;
        let s: Script = serde_json::from_value(v).map_err(io)?;
        if !s.start_agent.is_empty() {
            st.script.start_agent = s.start_agent;
        }
        if !s.info.is_empty() {
            st.script.info = s.info;
        }
        if !s.screen.is_empty() {
            st.script.screen = s.screen;
        }
        self.save(&st)
    }

    pub fn log(&self) -> Vec<LogEntry> {
        self.load().map(|s| s.log).unwrap_or_default()
    }

    fn apply_scripted(agent: &mut Agent, v: &Value) {
        if let Some(st) = v.get("state").and_then(Value::as_str) {
            agent.info.state = st.parse().unwrap_or(AgentState::Unknown);
        }
        if let Some(sid) = v.get("session_id").and_then(Value::as_str) {
            agent.info.session_id = Some(sid.to_string());
        }
        agent.info.revision += 1;
        agent.info.state_change_seq = Some(agent.info.revision);
    }
}

fn agent_mut<'a>(st: &'a mut State, name: &str) -> Result<&'a mut Agent, RuntimeError> {
    st.agents
        .get_mut(name)
        .ok_or_else(|| RuntimeError::NotFound(format!("agent {name} not found")))
}

impl AgentRuntime for FakeRuntime {
    fn health(&self) -> Result<RuntimeInfo, RuntimeError> {
        self.with("health", json!({}), |_| {
            Ok(RuntimeInfo {
                name: "fake".into(),
                version: "0".into(),
                protocol: None,
            })
        })
    }

    fn open_workspace(&self, r: &OpenWorkspace) -> Result<WorkspaceRef, RuntimeError> {
        let args = json!({ "cwd": r.cwd.to_string_lossy(), "label": r.label, "env": r.env });
        self.with("open_workspace", args, |st| {
            st.next_ws += 1;
            let ws = format!("f:w{}", st.next_ws);
            let pane = format!("f:p{}", st.next_ws);
            st.workspaces.insert(ws.clone(), pane.clone());
            Ok(WorkspaceRef {
                workspace_id: ws,
                pane_id: pane,
            })
        })
    }

    fn start_agent(&self, r: &StartAgent) -> Result<AgentInfo, RuntimeError> {
        let args = json!({ "name": r.name, "pane_id": r.pane_id, "args": r.args });
        self.with("start_agent", args, |st| {
            let workspace_id = st
                .workspaces
                .iter()
                .find(|(_, p)| **p == r.pane_id)
                .map(|(w, _)| w.clone())
                .unwrap_or_default();
            let screen = st.script.screen.clone();
            let scripted = st.script.start_agent.pop_front();
            let agent = st.agents.entry(r.name.clone()).or_insert_with(|| Agent {
                info: AgentInfo {
                    state: AgentState::Unknown,
                    launch_pending: true,
                    ..AgentInfo::default()
                },
                pane_id: r.pane_id.clone(),
                workspace_id,
                screen,
            });
            match scripted {
                Some(v) if v.get("error").is_some() => {
                    agent.info.state = AgentState::Blocked;
                    agent.info.launch_pending = true;
                    Err(RuntimeError::NotReady(format!(
                        "agent {} is blocked during startup (fake)",
                        r.name
                    )))
                }
                Some(v) => {
                    FakeRuntime::apply_scripted(agent, &v);
                    agent.info.launch_pending = false;
                    agent.info.interactive_ready = true;
                    Ok(agent.info.clone())
                }
                None => {
                    agent.info.state = AgentState::Idle;
                    agent.info.launch_pending = false;
                    agent.info.interactive_ready = true;
                    agent.info.revision += 1;
                    Ok(agent.info.clone())
                }
            }
        })
    }

    fn prompt(
        &self,
        h: &AgentHandle,
        text: &str,
        until: &[AgentState],
        _timeout_ms: u64,
    ) -> Result<AgentInfo, RuntimeError> {
        let args = json!({ "name": h.name, "text": text, "until": until });
        self.with("prompt", args, |st| {
            let a = agent_mut(st, &h.name)?;
            a.screen.push_str(&format!("\n> {text}\n"));
            a.info.state = until.first().copied().unwrap_or(AgentState::Working);
            a.info.revision += 1;
            a.info.state_change_seq = Some(a.info.revision);
            Ok(a.info.clone())
        })
    }

    fn wait(
        &self,
        h: &AgentHandle,
        until: &[AgentState],
        timeout_ms: u64,
    ) -> Result<AgentInfo, RuntimeError> {
        let args = json!({ "name": h.name, "until": until, "timeout_ms": timeout_ms });
        self.with("wait", args, |st| {
            let a = agent_mut(st, &h.name)?;
            let ok = if until.is_empty() {
                a.info.state.is_settled()
            } else {
                until.contains(&a.info.state)
            };
            if ok {
                Ok(a.info.clone())
            } else {
                Err(RuntimeError::Timeout(format!(
                    "agent {} is {} (fake wait)",
                    h.name, a.info.state
                )))
            }
        })
    }

    fn info(&self, h: &AgentHandle) -> Result<AgentInfo, RuntimeError> {
        self.with("info", json!({ "name": h.name }), |st| {
            let scripted = st.script.info.pop_front();
            let a = agent_mut(st, &h.name)?;
            if let Some(v) = scripted {
                FakeRuntime::apply_scripted(a, &v);
            }
            Ok(a.info.clone())
        })
    }

    fn read(&self, h: &AgentHandle, lines: u32) -> Result<String, RuntimeError> {
        self.with("read", json!({ "name": h.name, "lines": lines }), |st| {
            let a = agent_mut(st, &h.name)?;
            let all: Vec<&str> = a.screen.lines().collect();
            let start = all.len().saturating_sub(lines as usize);
            Ok(all[start..].join("\n"))
        })
    }

    fn send_keys(&self, h: &AgentHandle, keys: &[&str]) -> Result<(), RuntimeError> {
        self.with("send_keys", json!({ "name": h.name, "keys": keys }), |st| {
            agent_mut(st, &h.name)?;
            Ok(())
        })
    }

    fn focus(&self, h: &AgentHandle) -> Result<(), RuntimeError> {
        self.with("focus", json!({ "name": h.name }), |_| Ok(()))
    }

    fn report(
        &self,
        pane_id: &str,
        title: Option<&str>,
        tokens: &[(String, String)],
    ) -> Result<(), RuntimeError> {
        self.with(
            "report",
            json!({ "pane_id": pane_id, "title": title, "tokens": tokens }),
            |_| Ok(()),
        )
    }

    fn close_workspace(&self, workspace_id: &str) -> Result<(), RuntimeError> {
        self.with("close_workspace", json!({ "workspace_id": workspace_id }), |st| {
            st.workspaces.remove(workspace_id);
            st.agents.retain(|_, a| a.workspace_id != workspace_id);
            Ok(())
        })
    }

    fn notify(&self, title: &str, body: &str, sound: Option<&str>) -> Result<(), RuntimeError> {
        self.with(
            "notify",
            json!({ "title": title, "body": body, "sound": sound }),
            |_| Ok(()),
        )
    }
}
