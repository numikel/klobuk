//! File-backed fake runtime: lets several `asf` processes (CLI tests) share one simulated Herdr.
//!
//! State lives in a JSON file. A `script` can pre-seed responses: `start_agent` and `info` are
//! queues consumed in order (`{"error":"not_ready"}`, `{"error":"timeout"}` or
//! `{"state":"idle","session_id":"…"}`); `screen` seeds what `read` returns. Every call is appended
//! to `log`.
//!
//! A scripted `{"error":"timeout"}` start leaves the agent registered in state `unknown`: a re-read
//! sees `unknown` unless an `info` entry (`{"state":"idle"}`) says the agent came up after all, and
//! closing the workspace drops it, so the next `start_agent` on a fresh workspace starts clean.
//!
//! A scripted `prompt` entry `{"error":"stalled"|"timeout"}` types the text but does not submit it;
//! `"swallow_enter": N` makes the input box ignore the first N Enters that follow.
//!
//! Herdr reports a state change late (up to ~90 s observed live), so the fake can lag too: a `prompt`
//! entry with `"lag_ms": N` means the submission inside the prompt call did take effect, but the agent
//! reports `working` only N ms later (nothing is left in the input box, no Enter is needed);
//! `"enter_lag_ms": N` makes the Enter that finally submits take effect that late instead of at once.
//! While such a change is pending, `wait` really waits for it (up to its timeout), like Herdr's
//! `agent wait`; without one it answers at once, as before.
//!
//! A scripted `start_agent` entry `{"error":"pane_busy"}` is Herdr's `agent_pane_busy` refusal: the
//! shell of a freshly created pane is not ready yet. It registers no agent.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
    #[serde(default)]
    pending_enter: bool,
    /// Enters that the input box still ignores before one submits the pending text.
    #[serde(default)]
    swallow_enter: u32,
    /// How late (ms) the Enter that submits the pending text shows up as `working`; 0 = at once.
    #[serde(default)]
    enter_lag_ms: u64,
    /// Epoch ms at which a submission that already took effect becomes visible as `working`.
    #[serde(default)]
    working_at_ms: Option<u64>,
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
    prompt: VecDeque<Value>,
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
        if !s.prompt.is_empty() {
            st.script.prompt = s.prompt;
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

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Makes a lagging state change visible once its time has come.
fn settle(a: &mut Agent) {
    if a.working_at_ms.is_some_and(|t| now_ms() >= t) {
        a.working_at_ms = None;
        a.info.state = AgentState::Working;
        a.info.revision += 1;
        a.info.state_change_seq = Some(a.info.revision);
    }
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
            if scripted
                .as_ref()
                .is_some_and(|v| v.get("error").and_then(Value::as_str) == Some("pane_busy"))
            {
                return Err(RuntimeError::Protocol(format!(
                    "agent target pane {} is not an available shell ({})",
                    r.pane_id,
                    crate::herdr::PANE_BUSY_CODE
                )));
            }
            let agent = st.agents.entry(r.name.clone()).or_insert_with(|| Agent {
                info: AgentInfo {
                    state: AgentState::Unknown,
                    launch_pending: true,
                    ..AgentInfo::default()
                },
                pending_enter: false,
                swallow_enter: 0,
                enter_lag_ms: 0,
                working_at_ms: None,
                pane_id: r.pane_id.clone(),
                workspace_id,
                screen,
            });
            match scripted {
                Some(v) if v.get("error").and_then(Value::as_str) == Some("timeout") => {
                    // Herdr gave up waiting for the agent status, but the agent is actually coming up
                    agent.info.state = AgentState::Unknown;
                    agent.info.launch_pending = false;
                    Err(RuntimeError::Timeout(format!(
                        "timed out waiting for agent status of {} (fake)",
                        r.name
                    )))
                }
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
            let scripted = st.script.prompt.pop_front();
            let a = agent_mut(st, &h.name)?;
            a.screen.push_str(&format!("\n> {text}\n"));
            if let Some(v) = scripted.as_ref().filter(|v| v.get("error").is_some()) {
                // text landed in the input box but Enter was not accepted; `swallow_enter` makes the
                // box ignore that many further Enters too
                a.pending_enter = true;
                a.swallow_enter = v
                    .get("swallow_enter")
                    .and_then(Value::as_u64)
                    .map_or(0, |n| n as u32);
                a.enter_lag_ms = v.get("enter_lag_ms").and_then(Value::as_u64).unwrap_or(0);
                if let Some(lag) = v.get("lag_ms").and_then(Value::as_u64) {
                    // the submission took effect after all; the runtime just reports it late
                    a.pending_enter = false;
                    a.working_at_ms = Some(now_ms() + lag);
                }
                return Err(if v.get("error").and_then(Value::as_str) == Some("timeout") {
                    RuntimeError::Timeout(format!(
                        "timed out waiting for agent status of {} (fake prompt)",
                        h.name
                    ))
                } else {
                    RuntimeError::Stalled(format!(
                        "agent prompt produced no observed working or blocked state (fake); current status is {}",
                        a.info.state
                    ))
                });
            }
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
            let deadline = Instant::now() + Duration::from_millis(timeout_ms);
            loop {
                settle(a);
                let ok = if until.is_empty() {
                    a.info.state.is_settled()
                } else {
                    until.contains(&a.info.state)
                };
                if ok {
                    return Ok(a.info.clone());
                }
                // only a pending, lagging change is worth waiting for; otherwise answer at once
                let left = deadline.saturating_duration_since(Instant::now());
                if a.working_at_ms.is_none() || left.is_zero() {
                    return Err(RuntimeError::Timeout(format!(
                        "agent {} is {} (fake wait)",
                        h.name, a.info.state
                    )));
                }
                std::thread::sleep(left.min(Duration::from_millis(20)));
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
            settle(a);
            Ok(a.info.clone())
        })
    }

    fn read(&self, h: &AgentHandle, lines: u32) -> Result<String, RuntimeError> {
        self.with("read", json!({ "name": h.name, "lines": lines }), |st| {
            // the scripted `screen` acts as the live display, appended to what the agent accumulated
            let live = st.script.screen.clone();
            let a = agent_mut(st, &h.name)?;
            let mut screen = a.screen.clone();
            if !live.is_empty() && !screen.ends_with(&live) {
                screen.push('\n');
                screen.push_str(&live);
            }
            let all: Vec<&str> = screen.lines().collect();
            let start = all.len().saturating_sub(lines as usize);
            Ok(all[start..].join("\n"))
        })
    }

    fn send_keys(&self, h: &AgentHandle, keys: &[&str]) -> Result<(), RuntimeError> {
        self.with("send_keys", json!({ "name": h.name, "keys": keys }), |st| {
            let a = agent_mut(st, &h.name)?;
            if a.pending_enter && keys.contains(&"enter") {
                if a.swallow_enter > 0 {
                    a.swallow_enter -= 1;
                } else {
                    a.pending_enter = false;
                    if a.enter_lag_ms > 0 {
                        a.working_at_ms = Some(now_ms() + a.enter_lag_ms);
                    } else {
                        a.info.state = AgentState::Working;
                        a.info.revision += 1;
                        a.info.state_change_seq = Some(a.info.revision);
                    }
                }
            }
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
