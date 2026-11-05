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

#[test]
fn script_can_make_prompt_stall_once() {
    let dir = tempfile::tempdir().unwrap();
    let rt = FakeRuntime::new(dir.path().join("fake.json"));
    rt.script(serde_json::json!({ "prompt": [{"error": "stalled"}] }))
        .unwrap();
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
    assert!(matches!(
        rt.prompt(&h, "go", &[AgentState::Working], 1).unwrap_err(),
        RuntimeError::Stalled(_)
    ));
    assert_eq!(
        rt.info(&h).unwrap().state,
        AgentState::Idle,
        "a stalled prompt leaves the agent idle"
    );
    // the text still landed in the input box: Enter submits it
    rt.send_keys(&h, &["enter"]).unwrap();
    assert_eq!(rt.info(&h).unwrap().state, AgentState::Working);
    assert_eq!(
        rt.prompt(&h, "again", &[AgentState::Working], 1).unwrap().state,
        AgentState::Working
    );
}

#[test]
fn script_start_timeout_registers_the_agent_anyway() {
    let dir = tempfile::tempdir().unwrap();
    let rt = FakeRuntime::new(dir.path().join("fake.json"));
    rt.script(serde_json::json!({ "start_agent": [{"error": "timeout"}] }))
        .unwrap();
    let ws = rt
        .open_workspace(&OpenWorkspace {
            cwd: dir.path().to_path_buf(),
            label: "x".into(),
            env: vec![],
        })
        .unwrap();
    let e = rt
        .start_agent(&StartAgent {
            name: "a".into(),
            pane_id: ws.pane_id,
            args: vec![],
            timeout_ms: 1,
        })
        .unwrap_err();
    assert!(matches!(e, RuntimeError::Timeout(_)), "{e:?}");
    assert_eq!(rt.info(&handle("a")).unwrap().state, AgentState::Unknown);
}

/// The two behaviours `Worker::start`'s retry relies on, in one place: after a scripted start timeout
/// the agent can come up on its own (`info` script), or the dead workspace is closed and a fresh one
/// starts cleanly on the next, unscripted `start_agent`.
#[test]
fn start_timeout_then_idle_by_script_or_a_clean_restart_on_a_fresh_workspace() {
    let open = |rt: &FakeRuntime| {
        rt.open_workspace(&OpenWorkspace {
            cwd: std::env::temp_dir(),
            label: "x".into(),
            env: vec![],
        })
        .unwrap()
    };
    let start = |rt: &FakeRuntime, pane: &str| {
        rt.start_agent(&StartAgent {
            name: "a".into(),
            pane_id: pane.into(),
            args: vec![],
            timeout_ms: 1,
        })
    };

    // 1. state idle after the timeout: the scripted `info` is what a re-read sees
    let dir = tempfile::tempdir().unwrap();
    let rt = FakeRuntime::new(dir.path().join("fake.json"));
    rt.script(serde_json::json!({ "start_agent": [{"error": "timeout"}], "info": [{"state": "idle"}] }))
        .unwrap();
    let ws = open(&rt);
    assert!(matches!(
        start(&rt, &ws.pane_id).unwrap_err(),
        RuntimeError::Timeout(_)
    ));
    assert_eq!(rt.info(&handle("a")).unwrap().state, AgentState::Idle);

    // 2. first start times out, the workspace is closed and a second one starts cleanly
    let dir = tempfile::tempdir().unwrap();
    let rt = FakeRuntime::new(dir.path().join("fake.json"));
    rt.script(serde_json::json!({ "start_agent": [{"error": "timeout"}] }))
        .unwrap();
    let first = open(&rt);
    assert!(matches!(
        start(&rt, &first.pane_id).unwrap_err(),
        RuntimeError::Timeout(_)
    ));
    assert_eq!(rt.info(&handle("a")).unwrap().state, AgentState::Unknown);
    rt.close_workspace(&first.workspace_id).unwrap();
    assert!(
        matches!(rt.info(&handle("a")).unwrap_err(), RuntimeError::NotFound(_)),
        "closing the workspace drops the half-started agent"
    );
    let second = open(&rt);
    assert_ne!(second.workspace_id, first.workspace_id);
    assert_eq!(second.pane_id, "f:p2");
    let up = start(&rt, &second.pane_id).unwrap();
    assert_eq!(up.state, AgentState::Idle);
    assert!(up.interactive_ready);
    let starts: Vec<_> = rt.log().into_iter().filter(|e| e.op == "start_agent").collect();
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[1].args["pane_id"], "f:p2");
}
