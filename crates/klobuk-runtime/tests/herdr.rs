use std::cell::RefCell;
use std::collections::VecDeque;

use klobuk_core::*;
use klobuk_runtime::{Exec, ExecOutput, HerdrRuntime};

/// Returns canned outputs in order and records every invocation.
struct Recording {
    outputs: RefCell<VecDeque<ExecOutput>>,
    calls: RefCell<Vec<(String, Vec<String>)>>,
}

impl Recording {
    fn new(outs: Vec<(i32, &str, &str)>) -> Self {
        Recording {
            outputs: RefCell::new(
                outs.into_iter()
                    .map(|(s, o, e)| ExecOutput {
                        status: s,
                        stdout: o.into(),
                        stderr: e.into(),
                    })
                    .collect(),
            ),
            calls: RefCell::new(Vec::new()),
        }
    }
    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.borrow().iter().map(|(_, a)| a.clone()).collect()
    }
}

impl Exec for &Recording {
    fn run(&self, program: &str, args: &[String]) -> Result<ExecOutput, RuntimeError> {
        self.calls.borrow_mut().push((program.to_string(), args.to_vec()));
        self.outputs
            .borrow_mut()
            .pop_front()
            .ok_or_else(|| RuntimeError::Io("no canned output left".into()))
    }
}

// Canned JSON copied from the live spike (Herdr 0.9.5, protocol 22).
const WORKSPACE_CREATED: &str = r#"{"id":"cli:workspace:create","result":{"root_pane":{"agent_status":"unknown","cwd":"D:\\x\\repo\\","focused":false,"pane_id":"w3D:p1","revision":0,"tab_id":"w3D:t1","terminal_id":"term_1","workspace_id":"w3D"},"tab":{"agent_status":"unknown","focused":false,"label":"1","number":1,"pane_count":1,"tab_id":"w3D:t1","workspace_id":"w3D"},"type":"workspace_created","workspace":{"active_tab_id":"w3D:t1","agent_status":"unknown","focused":false,"label":"klobuk-spike","number":7,"pane_count":1,"tab_count":1,"workspace_id":"w3D"}}}"#;
const AGENT_NOT_READY: &str = r#"{"error":{"code":"agent_not_ready","message":"agent spike1 is blocked during startup and is not ready for prompts"},"id":"cli:agent:start"}"#;
const AGENT_INFO_BLOCKED: &str = r#"{"id":"cli:agent:get","result":{"agent":{"agent":"claude","agent_status":"blocked","cwd":"D:\\x\\repo","focused":false,"launch_pending":true,"name":"spike1","pane_id":"w3D:p1","revision":1,"state_change_seq":482,"tab_id":"w3D:t1","terminal_id":"term_1","workspace_id":"w3D"},"type":"agent_info"}}"#;
const AGENT_STARTED: &str = r#"{"id":"cli:agent:start","result":{"agent":{"agent":"claude","agent_status":"idle","interactive_ready":true,"name":"spike1","pane_id":"w3D:p1","revision":2,"state_change_seq":483,"tab_id":"w3D:t1","terminal_id":"term_1","workspace_id":"w3D"},"argv":["claude","--permission-mode","acceptEdits"],"type":"agent_started"}}"#;
const AGENT_PROMPTED_DONE: &str = r#"{"id":"cli:agent:prompt","result":{"agent":{"agent":"claude","agent_session":{"agent":"claude","kind":"id","source":"herdr:claude","value":"72ca581f-9bb5-48f8-be1a-4e742a53afa2"},"agent_status":"done","completion_seq":485,"name":"spike1","pane_id":"w3D:p1","revision":3,"state_change_seq":485,"tab_id":"w3D:t1","terminal_id":"term_1","workspace_id":"w3D"},"type":"agent_prompted"}}"#;
const PANE_READ: &str =
    "PS D:\\x\\repo> claude\n Quick safety check: ...\n > No, exit\n   Yes, I trust this folder\n";
const PANE_READ_JSON: &str = r#"{"id":"cli:agent:read","result":{"read":{"format":"text","pane_id":"w3D:p1","revision":3,"source":"recent_unwrapped","tab_id":"w3D:t1","text":"json text\n","truncated":false,"workspace_id":"w3D"},"type":"pane_read"}}"#;
const OK: &str = r#"{"id":"cli:agent:send-keys","result":{"type":"ok"}}"#;
const STATUS: &str = "client:\n  version: 0.9.5\n  channel: stable\n  protocol: 22\n\nserver:\n  status: running\n  version: 0.9.5\n  private_protocol: 22\n";

fn handle() -> AgentHandle {
    AgentHandle {
        name: "spike1".into(),
        pane_id: "w3D:p1".into(),
        workspace_id: "w3D".into(),
    }
}

#[test]
fn open_workspace_parses_root_pane_and_passes_env() {
    let rec = Recording::new(vec![(0, WORKSPACE_CREATED, "")]);
    let rt = HerdrRuntime::with_exec("herdr", &rec);
    let ws = rt
        .open_workspace(&OpenWorkspace {
            cwd: "D:/x/repo".into(),
            label: "asf-1 title".into(),
            env: vec![("KLOBUK_ISSUE".into(), "asf-1".into())],
        })
        .unwrap();
    assert_eq!(
        ws,
        WorkspaceRef {
            workspace_id: "w3D".into(),
            pane_id: "w3D:p1".into()
        }
    );
    let args = &rec.calls()[0];
    assert_eq!(args[0..2], ["workspace".to_string(), "create".to_string()]);
    assert!(args.contains(&"--no-focus".to_string()));
    assert!(args
        .windows(2)
        .any(|w| w[0] == "--env" && w[1] == "KLOBUK_ISSUE=asf-1"));
    assert!(args
        .windows(2)
        .any(|w| w[0] == "--label" && w[1] == "asf-1 title"));
}

#[test]
fn start_agent_not_ready_maps_error_even_when_on_stdout() {
    let rec = Recording::new(vec![(1, AGENT_NOT_READY, "")]);
    let rt = HerdrRuntime::with_exec("herdr", &rec);
    let e = rt
        .start_agent(&StartAgent {
            name: "spike1".into(),
            pane_id: "w3D:p1".into(),
            args: vec!["--permission-mode".into(), "acceptEdits".into()],
            timeout_ms: 90000,
        })
        .unwrap_err();
    assert!(
        matches!(e, RuntimeError::NotReady(ref m) if m.contains("blocked during startup")),
        "{e:?}"
    );
    let args = &rec.calls()[0];
    assert_eq!(
        args[0..3],
        ["agent".to_string(), "start".to_string(), "spike1".to_string()]
    );
    assert!(args.windows(2).any(|w| w[0] == "--kind" && w[1] == "claude"));
    assert!(args.windows(2).any(|w| w[0] == "--timeout" && w[1] == "90000"));
    let dash = args.iter().position(|a| a == "--").expect("-- separator");
    assert_eq!(
        args[dash + 1..],
        ["--permission-mode".to_string(), "acceptEdits".to_string()]
    );
}

#[test]
fn error_envelope_on_stderr_is_also_parsed() {
    let rec = Recording::new(vec![(1, "", AGENT_NOT_READY)]);
    let rt = HerdrRuntime::with_exec("herdr", &rec);
    let e = rt.info(&handle()).unwrap_err();
    assert!(matches!(e, RuntimeError::NotReady(_)));
    let rec = Recording::new(vec![(1, "", "garbage")]);
    let rt = HerdrRuntime::with_exec("herdr", &rec);
    assert!(matches!(
        rt.info(&handle()).unwrap_err(),
        RuntimeError::Protocol(_)
    ));
}

#[test]
fn start_agent_parses_agent_started() {
    let rec = Recording::new(vec![(0, AGENT_STARTED, "")]);
    let rt = HerdrRuntime::with_exec("herdr", &rec);
    let info = rt
        .start_agent(&StartAgent {
            name: "spike1".into(),
            pane_id: "w3D:p1".into(),
            args: vec![],
            timeout_ms: 1000,
        })
        .unwrap();
    assert_eq!(info.state, AgentState::Idle);
    assert!(info.interactive_ready);
    assert_eq!(info.revision, 2);
    assert!(
        !rec.calls()[0].contains(&"--".to_string()),
        "no separator without args"
    );
}

#[test]
fn info_parses_blocked_launch_pending() {
    let rec = Recording::new(vec![(0, AGENT_INFO_BLOCKED, "")]);
    let rt = HerdrRuntime::with_exec("herdr", &rec);
    let info = rt.info(&handle()).unwrap();
    assert_eq!(info.state, AgentState::Blocked);
    assert!(info.launch_pending);
    assert_eq!(info.state_change_seq, Some(482));
    assert_eq!(info.cwd.as_deref(), Some("D:\\x\\repo"));
}

#[test]
fn prompt_parses_done_with_session_and_passes_wait_flags() {
    let rec = Recording::new(vec![(0, AGENT_PROMPTED_DONE, "")]);
    let rt = HerdrRuntime::with_exec("herdr", &rec);
    let info = rt
        .prompt(&handle(), "Start asf-1", &[AgentState::Working], 15000)
        .unwrap();
    assert_eq!(info.state, AgentState::Done);
    assert_eq!(
        info.session_id.as_deref(),
        Some("72ca581f-9bb5-48f8-be1a-4e742a53afa2")
    );
    assert_eq!(info.completion_seq, Some(485));
    let args = &rec.calls()[0];
    assert_eq!(
        args[0..4],
        [
            "agent".to_string(),
            "prompt".to_string(),
            "spike1".to_string(),
            "Start asf-1".to_string()
        ]
    );
    assert!(args.contains(&"--wait".to_string()));
    assert!(args.windows(2).any(|w| w[0] == "--until" && w[1] == "working"));
    assert!(args.windows(2).any(|w| w[0] == "--timeout" && w[1] == "15000"));
}

#[test]
fn prompt_maps_blocked_and_stalled_codes() {
    for (code, pat) in [
        ("agent_blocked", "Blocked"),
        ("agent_prompt_stalled", "Stalled"),
        ("timeout", "Timeout"),
    ] {
        let body = format!(r#"{{"error":{{"code":"{code}","message":"m"}},"id":"cli:agent:prompt"}}"#);
        let rec = Recording::new(vec![(1, &body, "")]);
        let rt = HerdrRuntime::with_exec("herdr", &rec);
        let e = rt.prompt(&handle(), "x", &[], 10).unwrap_err();
        assert_eq!(format!("{e:?}").split('(').next().unwrap(), pat, "{code}");
    }
}

#[test]
fn wait_without_until_omits_flag() {
    let rec = Recording::new(vec![(0, AGENT_PROMPTED_DONE, "")]);
    let rt = HerdrRuntime::with_exec("herdr", &rec);
    rt.wait(&handle(), &[], 5000).unwrap();
    let args = &rec.calls()[0];
    assert_eq!(
        args[0..3],
        ["agent".to_string(), "wait".to_string(), "spike1".to_string()]
    );
    assert!(!args.contains(&"--until".to_string()));
}

#[test]
fn read_returns_text_and_send_keys_focus_close_report_notify_are_ok() {
    let rec = Recording::new(vec![
        (0, PANE_READ, ""),
        (0, OK, ""),
        (0, OK, ""),
        (0, OK, ""),
        (0, OK, ""),
        (0, OK, ""),
    ]);
    let rt = HerdrRuntime::with_exec("herdr", &rec);
    let text = rt.read(&handle(), 60).unwrap();
    assert!(text.contains("trust this folder"));
    rt.send_keys(&handle(), &["down", "enter"]).unwrap();
    rt.focus(&handle()).unwrap();
    rt.report("w3D:p1", Some("asf-1"), &[("issue".into(), "asf-1".into())])
        .unwrap();
    rt.close_workspace("w3D").unwrap();
    rt.notify("klobuk", "done", Some("done")).unwrap();
    let calls = rec.calls();
    assert_eq!(
        calls[0][0..3],
        ["agent".to_string(), "read".to_string(), "spike1".to_string()]
    );
    assert!(calls[0].windows(2).any(|w| w[0] == "--lines" && w[1] == "60"));
    assert_eq!(calls[1], ["agent", "send-keys", "spike1", "down", "enter"]);
    assert_eq!(calls[2][0..2], ["agent".to_string(), "focus".to_string()]);
    assert_eq!(
        calls[3][0..3],
        [
            "pane".to_string(),
            "report-metadata".to_string(),
            "w3D:p1".to_string()
        ]
    );
    assert!(calls[3]
        .windows(2)
        .any(|w| w[0] == "--token" && w[1] == "issue=asf-1"));
    assert!(calls[3]
        .windows(2)
        .any(|w| w[0] == "--source" && w[1] == "klobuk"));
    assert_eq!(calls[4], ["workspace", "close", "w3D"]);
    assert_eq!(
        calls[5][0..3],
        [
            "notification".to_string(),
            "show".to_string(),
            "klobuk".to_string()
        ]
    );
}

#[test]
fn health_parses_status_text_and_requires_protocol_22() {
    let rec = Recording::new(vec![(0, STATUS, "")]);
    let rt = HerdrRuntime::with_exec("herdr", &rec);
    let h = rt.health().unwrap();
    assert_eq!(h.name, "herdr");
    assert_eq!(h.version, "0.9.5");
    assert_eq!(h.protocol, Some(22));
    let rec = Recording::new(vec![(0, &STATUS.replace("protocol: 22", "protocol: 23"), "")]);
    let rt = HerdrRuntime::with_exec("herdr", &rec);
    assert!(matches!(rt.health().unwrap_err(), RuntimeError::Protocol(_)));
    let rec = Recording::new(vec![(1, "", "not running")]);
    let rt = HerdrRuntime::with_exec("herdr", &rec);
    assert!(matches!(
        rt.health().unwrap_err(),
        RuntimeError::Io(_) | RuntimeError::Protocol(_)
    ));
}

#[test]
fn read_accepts_json_envelope_too_and_fails_on_error_exit() {
    let rec = Recording::new(vec![(0, PANE_READ_JSON, "")]);
    let rt = HerdrRuntime::with_exec("herdr", &rec);
    assert_eq!(rt.read(&handle(), 10).unwrap(), "json text\n");
    let rec = Recording::new(vec![(
        1,
        "",
        r#"{"error":{"code":"agent_not_found","message":"x"},"id":"cli:agent:read"}"#,
    )]);
    let rt = HerdrRuntime::with_exec("herdr", &rec);
    assert!(matches!(
        rt.read(&handle(), 10).unwrap_err(),
        RuntimeError::NotFound(_)
    ));
}
