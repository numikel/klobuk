//! Herdr CLI adapter. Verified against Herdr 0.9.5 / protocol 22 (see project memory
//! `asf-herdr-cli-facts`): success is `{"id":…,"result":{…}}` on stdout, failure is
//! `{"error":{"code","message"},"id"}` (stdout or stderr) with exit 1.

use std::path::Path;

use asf_core::*;
use serde_json::Value;

use crate::exec::{Exec, SystemExec};

pub const REQUIRED_PROTOCOL: i64 = 22;

/// Herdr's refusal of `agent start` while the shell of a pane is not ready yet ("agent target pane
/// … is not an available shell"). It is transient: right after `workspace create`, under load, the
/// shell needs a few seconds. It comes back as a `Protocol` error that names this code.
pub const PANE_BUSY_CODE: &str = "agent_pane_busy";

/// Whether `e` is Herdr's transient "pane is not an available shell yet" refusal.
pub fn is_pane_busy(e: &RuntimeError) -> bool {
    matches!(e, RuntimeError::Protocol(m) if m.contains(PANE_BUSY_CODE))
}

pub struct HerdrRuntime<E: Exec = SystemExec> {
    bin: String,
    exec: E,
}

impl HerdrRuntime<SystemExec> {
    pub fn new(bin: &str) -> Self {
        HerdrRuntime {
            bin: bin.to_string(),
            exec: SystemExec,
        }
    }
}

fn s(v: &str) -> String {
    v.to_string()
}

fn map_error(code: &str, message: &str) -> RuntimeError {
    let m = format!("{message} ({code})");
    match code {
        "agent_not_ready" => RuntimeError::NotReady(m),
        "agent_blocked" => RuntimeError::Blocked(m),
        "agent_prompt_stalled" => RuntimeError::Stalled(m),
        "timeout" => RuntimeError::Timeout(m),
        c if c.ends_with("_not_found") || c == "not_found" => RuntimeError::NotFound(m),
        _ => RuntimeError::Protocol(m),
    }
}

/// Parses an `AgentInfo` from Herdr's `AgentInfo` object.
pub fn agent_info(v: &Value) -> Result<AgentInfo, RuntimeError> {
    let state_s = v
        .get("agent_status")
        .and_then(Value::as_str)
        .ok_or_else(|| RuntimeError::Protocol(format!("agent info without agent_status: {v}")))?;
    let state: AgentState = state_s.parse().unwrap_or(AgentState::Unknown);
    let session_id = v
        .get("agent_session")
        .filter(|x| x.get("kind").and_then(Value::as_str) == Some("id"))
        .and_then(|x| x.get("value"))
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok(AgentInfo {
        state,
        revision: v.get("revision").and_then(Value::as_i64).unwrap_or(0),
        state_change_seq: v.get("state_change_seq").and_then(Value::as_i64),
        completion_seq: v.get("completion_seq").and_then(Value::as_i64),
        interactive_ready: v
            .get("interactive_ready")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        launch_pending: v.get("launch_pending").and_then(Value::as_bool).unwrap_or(false),
        session_id,
        cwd: v.get("cwd").and_then(Value::as_str).map(str::to_string),
    })
}

impl<E: Exec> HerdrRuntime<E> {
    pub fn with_exec(bin: &str, exec: E) -> Self {
        HerdrRuntime {
            bin: bin.to_string(),
            exec,
        }
    }

    /// Runs a CLI command and returns the `result` object.
    fn call(&self, args: &[String]) -> Result<Value, RuntimeError> {
        let out = self.exec.run(&self.bin, args)?;
        for text in [&out.stdout, &out.stderr] {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
                if let Some(err) = v.get("error") {
                    let code = err.get("code").and_then(Value::as_str).unwrap_or("unknown");
                    let msg = err.get("message").and_then(Value::as_str).unwrap_or("");
                    return Err(map_error(code, msg));
                }
                if let Some(res) = v.get("result") {
                    return Ok(res.clone());
                }
            }
        }
        if out.status != 0 {
            let detail = if out.stderr.trim().is_empty() {
                out.stdout.trim()
            } else {
                out.stderr.trim()
            };
            return Err(RuntimeError::Protocol(format!(
                "herdr {} exited {}: {detail}",
                args.first().map(String::as_str).unwrap_or(""),
                out.status
            )));
        }
        Err(RuntimeError::Protocol(format!(
            "herdr {}: no JSON envelope in output",
            args.join(" ")
        )))
    }

    fn agent_result(&self, args: &[String]) -> Result<AgentInfo, RuntimeError> {
        let res = self.call(args)?;
        match res.get("agent") {
            Some(a) => agent_info(a),
            None if res.get("agent_status").is_some() => agent_info(&res),
            None => Err(RuntimeError::Protocol(format!("expected agent in result: {res}"))),
        }
    }
}

fn until_args(until: &[AgentState]) -> Vec<String> {
    until.iter().flat_map(|u| [s("--until"), u.to_string()]).collect()
}

impl<E: Exec> AgentRuntime for HerdrRuntime<E> {
    fn health(&self) -> Result<RuntimeInfo, RuntimeError> {
        let out = self.exec.run(&self.bin, &[s("status")])?;
        if out.status != 0 {
            return Err(RuntimeError::Io(format!(
                "herdr status exited {}: {}",
                out.status,
                out.stderr.trim()
            )));
        }
        let mut version = None;
        let mut protocol = None;
        let mut in_client = false;
        for line in out.stdout.lines() {
            let t = line.trim();
            if t.ends_with(':') && !line.starts_with(' ') {
                in_client = t == "client:";
                continue;
            }
            if in_client {
                if let Some(v) = t.strip_prefix("version:") {
                    version = Some(v.trim().to_string());
                } else if let Some(p) = t.strip_prefix("protocol:") {
                    protocol = p.trim().parse::<i64>().ok();
                }
            }
        }
        let version =
            version.ok_or_else(|| RuntimeError::Protocol("herdr status: no client version".into()))?;
        match protocol {
            Some(p) if p == REQUIRED_PROTOCOL => Ok(RuntimeInfo {
                name: "herdr".into(),
                version,
                protocol,
            }),
            Some(p) => Err(RuntimeError::Protocol(format!(
                "herdr protocol {p}, asf needs {REQUIRED_PROTOCOL}"
            ))),
            None => Err(RuntimeError::Protocol("herdr status: no protocol line".into())),
        }
    }

    fn open_workspace(&self, r: &OpenWorkspace) -> Result<WorkspaceRef, RuntimeError> {
        let mut args = vec![
            s("workspace"),
            s("create"),
            s("--cwd"),
            path_arg(&r.cwd),
            s("--label"),
            r.label.clone(),
        ];
        for (k, v) in &r.env {
            args.push(s("--env"));
            args.push(format!("{k}={v}"));
        }
        args.push(s("--no-focus"));
        let res = self.call(&args)?;
        let workspace_id = res
            .pointer("/workspace/workspace_id")
            .and_then(Value::as_str)
            .ok_or_else(|| RuntimeError::Protocol(format!("workspace create: no workspace_id in {res}")))?;
        let pane_id = res
            .pointer("/root_pane/pane_id")
            .and_then(Value::as_str)
            .ok_or_else(|| RuntimeError::Protocol(format!("workspace create: no root_pane in {res}")))?;
        Ok(WorkspaceRef {
            workspace_id: workspace_id.to_string(),
            pane_id: pane_id.to_string(),
        })
    }

    fn start_agent(&self, r: &StartAgent) -> Result<AgentInfo, RuntimeError> {
        let mut args = vec![
            s("agent"),
            s("start"),
            r.name.clone(),
            s("--kind"),
            s("claude"),
            s("--pane"),
            r.pane_id.clone(),
            s("--timeout"),
            r.timeout_ms.to_string(),
        ];
        if !r.args.is_empty() {
            args.push(s("--"));
            args.extend(r.args.iter().cloned());
        }
        self.agent_result(&args)
    }

    fn prompt(
        &self,
        h: &AgentHandle,
        text: &str,
        until: &[AgentState],
        timeout_ms: u64,
    ) -> Result<AgentInfo, RuntimeError> {
        let mut args = vec![
            s("agent"),
            s("prompt"),
            h.name.clone(),
            text.to_string(),
            s("--wait"),
        ];
        args.extend(until_args(until));
        args.push(s("--timeout"));
        args.push(timeout_ms.to_string());
        self.agent_result(&args)
    }

    fn wait(
        &self,
        h: &AgentHandle,
        until: &[AgentState],
        timeout_ms: u64,
    ) -> Result<AgentInfo, RuntimeError> {
        let mut args = vec![s("agent"), s("wait"), h.name.clone()];
        args.extend(until_args(until));
        args.push(s("--timeout"));
        args.push(timeout_ms.to_string());
        self.agent_result(&args)
    }

    fn info(&self, h: &AgentHandle) -> Result<AgentInfo, RuntimeError> {
        self.agent_result(&[s("agent"), s("get"), h.name.clone()])
    }

    /// `herdr agent read` prints the screen as plain text (verified live); a JSON envelope with
    /// `result.read.text` is accepted too in case a future version switches.
    fn read(&self, h: &AgentHandle, lines: u32) -> Result<String, RuntimeError> {
        let args = vec![
            s("agent"),
            s("read"),
            h.name.clone(),
            s("--source"),
            s("recent-unwrapped"),
            s("--lines"),
            lines.to_string(),
        ];
        let out = self.exec.run(&self.bin, &args)?;
        if out.status == 0 {
            if let Ok(v) = serde_json::from_str::<Value>(out.stdout.trim()) {
                if let Some(t) = v.pointer("/result/read/text").and_then(Value::as_str) {
                    return Ok(t.to_string());
                }
                if v.get("error").is_none() {
                    return Ok(out.stdout);
                }
            } else {
                return Ok(out.stdout);
            }
        }
        // non-zero exit or an error envelope: map the error code from what we already have
        for text in [&out.stdout, &out.stderr] {
            if let Ok(v) = serde_json::from_str::<Value>(text.trim()) {
                if let Some(err) = v.get("error") {
                    let code = err.get("code").and_then(Value::as_str).unwrap_or("unknown");
                    let msg = err.get("message").and_then(Value::as_str).unwrap_or("");
                    return Err(map_error(code, msg));
                }
            }
        }
        Err(RuntimeError::Protocol(format!(
            "herdr agent read exited {}: {}",
            out.status,
            out.stderr.trim()
        )))
    }

    fn send_keys(&self, h: &AgentHandle, keys: &[&str]) -> Result<(), RuntimeError> {
        let mut args = vec![s("agent"), s("send-keys"), h.name.clone()];
        args.extend(keys.iter().map(|k| s(k)));
        self.call(&args).map(|_| ())
    }

    fn focus(&self, h: &AgentHandle) -> Result<(), RuntimeError> {
        self.call(&[s("agent"), s("focus"), h.name.clone()]).map(|_| ())
    }

    fn report(
        &self,
        pane_id: &str,
        title: Option<&str>,
        tokens: &[(String, String)],
    ) -> Result<(), RuntimeError> {
        let mut args = vec![
            s("pane"),
            s("report-metadata"),
            pane_id.to_string(),
            s("--source"),
            s("asf"),
        ];
        if let Some(t) = title {
            args.push(s("--title"));
            args.push(t.to_string());
        }
        for (k, v) in tokens {
            args.push(s("--token"));
            args.push(format!("{k}={v}"));
        }
        self.call(&args).map(|_| ())
    }

    fn close_workspace(&self, workspace_id: &str) -> Result<(), RuntimeError> {
        self.call(&[s("workspace"), s("close"), workspace_id.to_string()])
            .map(|_| ())
    }

    fn notify(&self, title: &str, body: &str, sound: Option<&str>) -> Result<(), RuntimeError> {
        let mut args = vec![
            s("notification"),
            s("show"),
            title.to_string(),
            s("--body"),
            body.to_string(),
        ];
        if let Some(snd) = sound {
            args.push(s("--sound"));
            args.push(snd.to_string());
        }
        self.call(&args).map(|_| ())
    }
}

fn path_arg(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_busy_pane_refusal_is_told_apart_from_other_protocol_errors() {
        let busy = map_error(
            PANE_BUSY_CODE,
            "agent target pane w73:p1 is not an available shell",
        );
        assert!(is_pane_busy(&busy), "{busy:?}");
        assert!(!is_pane_busy(&map_error("agent_not_ready", "starting")));
        assert!(!is_pane_busy(&map_error("pane_not_found", "no such pane")));
        assert!(!is_pane_busy(&RuntimeError::Timeout("agent_pane_busy".into())));
    }
}
