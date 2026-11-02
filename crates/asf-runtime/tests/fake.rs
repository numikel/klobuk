use asf_core::*;
use asf_runtime::FakeRuntime;

fn handle(name: &str) -> AgentHandle {
    AgentHandle {
        name: name.into(),
        pane_id: "f:p1".into(),
        workspace_id: "f:w1".into(),
    }
}

#[test]
fn default_script_happy_path_and_log() {
    let dir = tempfile::tempdir().unwrap();
    let rt = FakeRuntime::new(dir.path().join("fake.json"));
    assert_eq!(rt.health().unwrap().name, "fake");
    let ws = rt
        .open_workspace(&OpenWorkspace {
            cwd: dir.path().to_path_buf(),
            label: "asf-1".into(),
            env: vec![],
        })
        .unwrap();
    assert_eq!(ws.workspace_id, "f:w1");
    assert_eq!(ws.pane_id, "f:p1");
    let info = rt
        .start_agent(&StartAgent {
            name: "asf-1".into(),
            pane_id: ws.pane_id.clone(),
            args: vec![],
            timeout_ms: 1,
        })
        .unwrap();
    assert_eq!(info.state, AgentState::Idle);
    assert!(info.interactive_ready);
    let h = handle("asf-1");
    let after = rt.prompt(&h, "go", &[AgentState::Working], 1).unwrap();
    assert_eq!(after.state, AgentState::Working);
    assert_eq!(rt.info(&h).unwrap().state, AgentState::Working);
    // a second runtime instance on the same file sees the same state (CLI processes share it)
    let rt2 = FakeRuntime::new(dir.path().join("fake.json"));
    assert_eq!(rt2.info(&h).unwrap().state, AgentState::Working);
    assert!(
        rt2.read(&h, 10).unwrap().contains("go"),
        "prompt text is echoed into the fake screen"
    );
    rt2.send_keys(&h, &["esc"]).unwrap();
    rt2.close_workspace("f:w1").unwrap();
    let log = rt2.log();
    let ops: Vec<&str> = log.iter().map(|e| e.op.as_str()).collect();
    assert_eq!(
        ops,
        [
            "health",
            "open_workspace",
            "start_agent",
            "prompt",
            "info",
            "info",
            "read",
            "send_keys",
            "close_workspace"
        ]
    );
    assert_eq!(log[3].args["text"], "go");
}

#[test]
fn script_drives_start_errors_and_states() {
    let dir = tempfile::tempdir().unwrap();
    let rt = FakeRuntime::new(dir.path().join("fake.json"));
    // script: start fails with NotReady once, screen shows the trust prompt, then idle; later info says done
    rt.script(serde_json::json!({
        "start_agent": [{"error": "not_ready"}, {"state": "idle"}],
        "screen": "Quick safety check: Yes, I trust this folder",
        "info": [{"state": "working"}, {"state": "done", "session_id": "sess-1"}]
    }))
    .unwrap();
    let ws = rt
        .open_workspace(&OpenWorkspace {
            cwd: dir.path().to_path_buf(),
            label: "x".into(),
            env: vec![],
        })
        .unwrap();
    let start = StartAgent {
        name: "a".into(),
        pane_id: ws.pane_id,
        args: vec![],
        timeout_ms: 1,
    };
    assert!(matches!(
        rt.start_agent(&start).unwrap_err(),
        RuntimeError::NotReady(_)
    ));
    let h = handle("a");
    assert!(rt.read(&h, 5).unwrap().contains("trust this folder"));
    rt.send_keys(&h, &["down", "enter"]).unwrap();
    assert_eq!(rt.start_agent(&start).unwrap().state, AgentState::Idle);
    assert_eq!(rt.info(&h).unwrap().state, AgentState::Working);
    let done = rt.info(&h).unwrap();
    assert_eq!(done.state, AgentState::Done);
    assert_eq!(done.session_id.as_deref(), Some("sess-1"));
    assert_eq!(
        rt.info(&h).unwrap().state,
        AgentState::Done,
        "last scripted state sticks"
    );
    assert!(matches!(
        rt.info(&handle("ghost")).unwrap_err(),
        RuntimeError::NotFound(_)
    ));
}

#[test]
fn wait_returns_current_state_when_it_matches_and_times_out_otherwise() {
    let dir = tempfile::tempdir().unwrap();
    let rt = FakeRuntime::new(dir.path().join("fake.json"));
    let ws = rt
        .open_workspace(&OpenWorkspace {
            cwd: dir.path().to_path_buf(),
            label: "x".into(),
            env: vec![],
        })
        .unwrap();
    rt.start_agent(&StartAgent {
        name: "a".into(),
        pane_id: ws.pane_id,
        args: vec![],
        timeout_ms: 1,
    })
    .unwrap();
    let h = handle("a");
    assert_eq!(
        rt.wait(&h, &[AgentState::Idle], 10).unwrap().state,
        AgentState::Idle
    );
    assert!(matches!(
        rt.wait(&h, &[AgentState::Done], 10).unwrap_err(),
        RuntimeError::Timeout(_)
    ));
}
