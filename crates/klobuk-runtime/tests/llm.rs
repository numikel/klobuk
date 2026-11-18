use klobuk_core::RuntimeError;
use klobuk_runtime::llm::{claude_args, parse_result, FakeLlm, Structured, StructuredRequest};
use serde_json::json;

fn req() -> StructuredRequest {
    StructuredRequest {
        schema_name: "prd".into(),
        instruction: "Produce the PRD object.".into(),
        stdin: "idea text".into(),
        schema: json!({"type":"object","properties":{"title":{"type":"string"}},"required":["title"]}),
        model: "claude-haiku-5-5".into(),
        max_turns: 15,
        budget_usd: 1.0,
        add_dirs: vec![],
    }
}

// Recorded from the live smoke test (Claude Code 2.1.316).
const SUCCESS: &str = r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":4000,"num_turns":2,"result":"{\"greeting\":\"Cześć, Ada!\",\"name\":\"Ada\"}","structured_output":{"greeting":"Cześć, Ada!","name":"Ada"},"session_id":"s-1","total_cost_usd":0.0184355,"stop_reason":"tool_use","usage":{"input_tokens":2,"output_tokens":575},"modelUsage":{"claude-haiku-5-5":{"costUSD":0.0184355}},"permission_denials":[],"uuid":"u"}"#;
const RETRIES: &str = r#"{"type":"result","subtype":"error_max_structured_output_retries","is_error":true,"num_turns":4,"errors":["output did not match schema"],"session_id":"s-2","total_cost_usd":0.03,"usage":{},"modelUsage":{},"permission_denials":[]}"#;
const SUCCESS_NO_OUTPUT: &str = r#"{"type":"result","subtype":"success","is_error":false,"num_turns":1,"result":"I cannot do that.","session_id":"s-3","total_cost_usd":0.001,"usage":{},"modelUsage":{},"permission_denials":[]}"#;

#[test]
fn claude_args_are_safe_and_complete() {
    let a = claude_args(&req());
    assert_eq!(a[0], "-p");
    assert_eq!(a[1], "Produce the PRD object.");
    let has = |k: &str, v: &str| a.windows(2).any(|w| w[0] == k && w[1] == v);
    assert!(has("--output-format", "json"));
    assert!(a
        .windows(2)
        .any(|w| w[0] == "--json-schema" && w[1].starts_with('{') && !w[1].contains('\n')));
    assert!(has("--model", "claude-haiku-5-5"));
    assert!(has("--permission-mode", "dontAsk"));
    assert!(has("--tools", "Read,Glob,Grep"));
    assert!(has("--allowedTools", "Read,Glob,Grep"));
    assert!(has("--permission-prompts", "none"));
    assert!(has("--max-turns", "15"));
    assert!(has("--max-budget-usd", "1.00"));
    assert!(a.contains(&"--no-session-persistence".to_string()));
    assert!(
        !a.contains(&"--bare".to_string()),
        "bare mode needs an API key; subscription logins must keep working"
    );
    let mut r = req();
    r.add_dirs = vec!["D:/x".into()];
    assert!(claude_args(&r)
        .windows(2)
        .any(|w| w[0] == "--add-dir" && w[1] == "D:/x"));
}

#[test]
fn parse_result_accepts_success_and_maps_failures() {
    let ok = parse_result(SUCCESS, "").unwrap();
    assert_eq!(ok.output["name"], "Ada");
    assert_eq!(ok.cost_usd, 0.0184355);
    assert_eq!(ok.num_turns, 2);
    assert_eq!(ok.session_id.as_deref(), Some("s-1"));
    let e = parse_result(RETRIES, "").unwrap_err();
    assert!(
        matches!(e, RuntimeError::Protocol(ref m) if m.contains("error_max_structured_output_retries") && m.contains("did not match")),
        "{e:?}"
    );
    let e = parse_result(SUCCESS_NO_OUTPUT, "").unwrap_err();
    assert!(
        matches!(e, RuntimeError::Protocol(ref m) if m.contains("no structured_output") && m.contains("I cannot do that")),
        "{e:?}"
    );
    let e = parse_result("", "Error: --json-schema is not a valid JSON Schema").unwrap_err();
    assert!(
        matches!(e, RuntimeError::Protocol(ref m) if m.contains("not a valid JSON Schema")),
        "{e:?}"
    );
}

#[test]
fn fake_llm_serves_canned_responses_by_schema_name_and_logs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fake-llm.json");
    std::fs::write(
        &path,
        json!({ "responses": { "prd": { "title": "Greeting" } } }).to_string(),
    )
    .unwrap();
    let llm = FakeLlm::new(&path);
    let out = llm.run(&req()).unwrap();
    assert_eq!(out.output["title"], "Greeting");
    assert_eq!(out.cost_usd, 0.0);
    let mut other = req();
    other.schema_name = "issues".into();
    let e = llm.run(&other).unwrap_err();
    assert!(
        matches!(e, RuntimeError::Protocol(ref m) if m.contains("issues")),
        "{e:?}"
    );
    let log = llm.log();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0]["schema_name"], "prd");
    assert_eq!(log[0]["stdin"], "idea text");
    assert!(log[0]["instruction"].as_str().unwrap().contains("PRD"));
}
