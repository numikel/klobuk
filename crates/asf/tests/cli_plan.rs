mod common;

use common::Repo;

const PLAN: &str = r#"# Greeting Feature Implementation Plan

**Goal:** Ship greet() with tests.

### Task 1: Scaffold module

**Files:**
- Create: `src/greet.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: failing test**
- [ ] **Step 2: implement**

### Task 2: Docs [parallel]

**Files:**
- Create: docs/greet.md

- [ ] write the doc page

### Task 3: Release notes

**Depends on:** Task 1, Task 2

- Files: `CHANGELOG.md`
- [ ] changelog entry
"#;

fn write_plan(repo: &Repo) -> String {
    let p = repo.path().join("plan.md");
    std::fs::write(&p, PLAN).unwrap();
    p.to_string_lossy().to_string()
}

#[test]
fn import_creates_epic_and_tasks_with_deps_and_scopes() {
    let r = Repo::initialised();
    let plan = write_plan(&r);
    let res = r.ok(&["plan", "import", &plan]);
    assert_eq!(res["epic"], "asf-1", "{res}");
    assert_eq!(res["created"].as_array().unwrap().len(), 3, "{res}");
    assert_eq!(res["skipped"].as_array().unwrap().len(), 0);
    let t1 = r.ok(&["issue", "show", "asf-2"]);
    assert_eq!(t1["issue"]["title"], "Scaffold module");
    assert_eq!(t1["issue"]["parent_id"], "asf-1");
    assert_eq!(
        t1["issue"]["file_scope"],
        serde_json::json!(["src/greet.rs", "src/lib.rs"])
    );
    assert_eq!(t1["issue"]["acceptance"][0]["text"], "failing test");
    assert_eq!(t1["issue"]["status"], "backlog");
    assert!(t1["issue"]["source"]
        .as_str()
        .unwrap()
        .ends_with("plan.md#task-1"));
    let t3 = r.ok(&["issue", "show", "asf-4"]);
    let deps: Vec<&str> = t3["deps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].as_str().unwrap())
        .collect();
    assert_eq!(deps, ["asf-2", "asf-3"]);
    let t2 = r.ok(&["issue", "show", "asf-3"]);
    assert!(
        t2["deps"].as_array().unwrap().is_empty(),
        "parallel task has no implicit dep"
    );
    let epic = r.ok(&["issue", "show", "asf-1"]);
    assert_eq!(epic["issue"]["type"], "epic");
    assert!(epic["issue"]["body"].as_str().unwrap().contains("Ship greet()"));
    // ready promotion makes the roots startable
    assert_eq!(r.ok(&["issue", "ready"]), serde_json::json!(["asf-2", "asf-3"]));
}

#[test]
fn import_dry_run_writes_nothing_and_reimport_skips_existing() {
    let r = Repo::initialised();
    let plan = write_plan(&r);
    let res = r.ok(&["plan", "import", &plan, "--dry-run"]);
    assert_eq!(res["dry_run"], true);
    assert_eq!(res["tasks"].as_array().unwrap().len(), 3, "{res}");
    assert_eq!(r.ok(&["issue", "list"]).as_array().unwrap().len(), 0);
    r.ok(&["plan", "import", &plan]);
    let again = r.ok(&["plan", "import", &plan]);
    assert_eq!(again["created"].as_array().unwrap().len(), 0, "{again}");
    assert_eq!(again["skipped"].as_array().unwrap().len(), 3);
    assert_eq!(
        r.ok(&["issue", "list"]).as_array().unwrap().len(),
        4,
        "epic + 3 tasks, no duplicates"
    );
}

#[test]
fn import_without_epic_and_bad_file() {
    let r = Repo::initialised();
    let plan = write_plan(&r);
    let res = r.ok(&["plan", "import", &plan, "--no-epic"]);
    assert!(res["epic"].is_null());
    assert_eq!(r.ok(&["issue", "show", "asf-1"])["issue"]["type"], "task");
    let (code, ecode, _) = r.err(&["plan", "import", "missing.md"]);
    assert_eq!((code, ecode.as_str()), (1, "io"));
    let empty = r.path().join("empty.md");
    std::fs::write(&empty, "# nothing here\n").unwrap();
    let (code, ecode, msg) = r.err(&["plan", "import", empty.to_str().unwrap()]);
    assert_eq!((code, ecode.as_str()), (1, "validation"));
    assert!(msg.contains("no tasks"), "{msg}");
}

fn fake_llm(repo: &Repo, responses: serde_json::Value) {
    std::fs::write(
        repo.path().join(".factory/fake-llm.json"),
        serde_json::json!({ "responses": responses }).to_string(),
    )
    .unwrap();
    let cfg = repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("runner = \"claude\"", "runner = \"fake\"");
    assert!(
        s.contains("runner = \"fake\""),
        "config template lacks [planner] runner"
    );
    std::fs::write(&cfg, s).unwrap();
}

fn canned() -> serde_json::Value {
    serde_json::json!({
        "prd": {
            "title": "Greeting feature",
            "summary": "Add greet() to the crate.",
            "goals": ["greet(name) returns Hello, <name>!"],
            "non_goals": ["i18n"],
            "constraints": ["no new dependencies"],
            "prd_markdown": "# Greeting feature

Add greet() to the crate.
"
        },
        "issues": {
            "epic": { "title": "Greeting feature", "body": "Add greet() to the crate." },
            "tasks": [
                { "number": 1, "title": "Implement greet", "body": "Add pub fn greet.", "file_scope": ["src/lib.rs"], "acceptance": ["greet(\"Ada\") == Hello, Ada!"], "depends_on": [], "estimate": "S" },
                { "number": 2, "title": "Document greet", "body": "README section.", "file_scope": ["README.md"], "acceptance": ["README mentions greet"], "depends_on": [1], "estimate": "S" }
            ]
        }
    })
}

#[test]
fn plan_new_runs_prd_then_issues_and_creates_them() {
    let r = Repo::initialised();
    fake_llm(&r, canned());
    let idea = r.path().join("idea.md");
    std::fs::write(
        &idea,
        "I want a greet function.
",
    )
    .unwrap();
    let res = r.ok(&["plan", "new", "--from", idea.to_str().unwrap()]);
    assert_eq!(res["slug"], "greeting-feature", "{res}");
    assert_eq!(res["epic"], "asf-1");
    assert_eq!(res["created"].as_array().unwrap().len(), 2);
    assert_eq!(res["cost_usd"], 0.0);
    let prd = std::fs::read_to_string(r.path().join(".factory/plan/greeting-feature/prd.md")).unwrap();
    assert!(prd.contains("# Greeting feature"));
    assert!(r
        .path()
        .join(".factory/plan/greeting-feature/issues.json")
        .exists());
    let t2 = r.ok(&["issue", "show", "asf-3"]);
    assert_eq!(t2["issue"]["title"], "Document greet");
    assert_eq!(t2["deps"][0]["id"], "asf-2");
    assert_eq!(t2["issue"]["estimate"], "S");
    assert_eq!(t2["issue"]["parent_id"], "asf-1");
    assert!(t2["issue"]["source"]
        .as_str()
        .unwrap()
        .starts_with("plan:greeting-feature#task-2"));
    // the fake recorded both calls with the idea on stdin of the first and the PRD on stdin of the second
    let log: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(r.path().join(".factory/fake-llm.json")).unwrap())
            .unwrap();
    let calls = log["log"].as_array().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["schema_name"], "prd");
    assert!(calls[0]["stdin"]
        .as_str()
        .unwrap()
        .contains("I want a greet function."));
    assert_eq!(calls[1]["schema_name"], "issues");
    assert!(calls[1]["stdin"].as_str().unwrap().contains("# Greeting feature"));
}

#[test]
fn plan_new_dry_run_saves_artifacts_but_creates_no_issues_and_reports_llm_errors() {
    let r = Repo::initialised();
    fake_llm(&r, canned());
    let idea = r.path().join("idea.md");
    std::fs::write(
        &idea, "idea
",
    )
    .unwrap();
    let res = r.ok(&["plan", "new", "--from", idea.to_str().unwrap(), "--dry-run"]);
    assert_eq!(res["dry_run"], true);
    assert_eq!(res["tasks"].as_array().unwrap().len(), 2, "{res}");
    assert!(r.path().join(".factory/plan/greeting-feature/prd.md").exists());
    assert_eq!(r.ok(&["issue", "list"]).as_array().unwrap().len(), 0);

    // missing canned issues response → error names the step
    let mut only_prd = canned();
    only_prd.as_object_mut().unwrap().remove("issues");
    fake_llm(&r, only_prd);
    let (code, ecode, msg) = r.err(&["plan", "new", "--from", idea.to_str().unwrap()]);
    assert_eq!(code, 1);
    assert_eq!(ecode, "backend");
    assert!(msg.contains("issues"), "{msg}");
    let (code, _, _) = r.err(&["plan", "new", "--from", "nope.md"]);
    assert_eq!(code, 1);
}

/// Copies the shipped `asf-default` methodology into `.factory/methodologies/<name>/`, replaces its
/// first prompt with `prd_prompt` and makes it the active methodology.
fn local_methodology(repo: &Repo, name: &str, prd_prompt: &str) {
    let shipped = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../methodologies/asf-default");
    let dir = repo.path().join(".factory/methodologies").join(name);
    for rel in [
        "manifest.toml",
        "prompts/idea-to-prd.md",
        "prompts/prd-to-issues.md",
        "schemas/prd.json",
        "schemas/issues.json",
    ] {
        std::fs::create_dir_all(dir.join(rel).parent().unwrap()).unwrap();
        std::fs::copy(shipped.join(rel), dir.join(rel)).unwrap();
    }
    let manifest = std::fs::read_to_string(dir.join("manifest.toml"))
        .unwrap()
        .replace("name = \"asf-default\"", &format!("name = \"{name}\""));
    std::fs::write(dir.join("manifest.toml"), manifest).unwrap();
    std::fs::write(dir.join("prompts/idea-to-prd.md"), prd_prompt).unwrap();
    let cfg = repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("name = \"asf-default\"", &format!("name = \"{name}\""));
    assert!(
        s.contains(&format!("name = \"{name}\"")),
        "config template lacks [methodology] name"
    );
    std::fs::write(&cfg, s).unwrap();
}

fn planner_log(repo: &Repo) -> Vec<serde_json::Value> {
    let log: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(repo.path().join(".factory/fake-llm.json")).unwrap())
            .unwrap();
    log["log"].as_array().unwrap().clone()
}

#[test]
fn plan_new_sends_the_prompts_of_the_active_local_methodology() {
    let r = Repo::initialised();
    fake_llm(&r, canned());
    let custom = "CUSTOM PRD PROMPT: answer in haiku.\n";
    local_methodology(&r, "custom", custom);
    let idea = r.path().join("idea.md");
    std::fs::write(&idea, "I want a greet function.\n").unwrap();
    let res = r.ok(&["plan", "new", "--from", idea.to_str().unwrap()]);
    assert_eq!(res["created"].as_array().unwrap().len(), 2, "{res}");
    let calls = planner_log(&r);
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[0]["instruction"], custom,
        "first stage must use the local prompt"
    );
    assert_eq!(calls[0]["schema_name"], "prd");
    let shipped_issues = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../methodologies/asf-default/prompts/prd-to-issues.md"),
    )
    .unwrap();
    assert_eq!(calls[1]["instruction"], shipped_issues.as_str());
    assert_eq!(calls[1]["schema_name"], "issues");
}

#[test]
fn plan_new_rejects_an_unknown_methodology_and_one_without_prompt_stages() {
    let r = Repo::initialised();
    fake_llm(&r, canned());
    let idea = r.path().join("idea.md");
    std::fs::write(&idea, "idea\n").unwrap();
    let cfg = r.path().join(".factory/config.toml");
    let base = std::fs::read_to_string(&cfg).unwrap();

    std::fs::write(&cfg, base.replace("name = \"asf-default\"", "name = \"nope\"")).unwrap();
    let (code, ecode, msg) = r.err(&["plan", "new", "--from", idea.to_str().unwrap()]);
    assert_eq!((code, ecode.as_str()), (1, "not_found"), "{msg}");
    assert!(msg.contains("nope"), "{msg}");

    std::fs::write(
        &cfg,
        base.replace("name = \"asf-default\"", "name = \"spec-kit\""),
    )
    .unwrap();
    let (code, ecode, msg) = r.err(&["plan", "new", "--from", idea.to_str().unwrap()]);
    assert_eq!((code, ecode.as_str()), (1, "validation"), "{msg}");
    assert!(msg.contains("prompt"), "{msg}");
    assert!(
        !r.path().join(".factory/plan").exists(),
        "nothing written on error"
    );
}
