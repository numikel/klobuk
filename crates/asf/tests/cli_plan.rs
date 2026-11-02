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
