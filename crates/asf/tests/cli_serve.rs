mod common;

use std::path::Path;

use common::{git, Repo};

fn commit_in(dir: &Path, rel: &str, content: &str) {
    if let Some(p) = Path::new(rel).parent() {
        std::fs::create_dir_all(dir.join(p)).unwrap();
    }
    std::fs::write(dir.join(rel), content).unwrap();
    git(dir, &["add", rel]);
    git(
        dir,
        &[
            "-c",
            "user.name=w",
            "-c",
            "user.email=w@x",
            "commit",
            "-q",
            "-m",
            &format!("add {rel}"),
        ],
    );
}

#[test]
fn serve_once_watches_merges_and_promotes() {
    let repo = Repo::initialised();
    let cfg = repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("runtime = \"herdr\"", "runtime = \"fake\"");
    std::fs::write(&cfg, s).unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "cfg"]);
    let a = repo.create_issue("first", &["--ready", "--scope", "src/**"]);
    let b = repo.create_issue("second", &["--dep", &a, "--scope", "docs/**"]);
    assert_eq!(repo.ok(&["issue", "show", &b])["issue"]["status"], "backlog");

    repo.ok(&["run", "start", &a]);
    let wt = repo.path().join(".worktrees").join(&a);
    commit_in(&wt, "src/x.rs", "");
    let report = wt.join(".factory").join("runs").join(&a).join("report.md");
    std::fs::write(&report, "# done\n").unwrap();
    repo.ok(&[
        "--lane",
        &a,
        "issue",
        "finish",
        &a,
        "--report",
        report.to_str().unwrap(),
    ]);

    let res = repo.ok(&["serve", "--once"]);
    assert_eq!(res["watch"]["transitions"][0]["to"], "succeeded", "{res}");
    assert_eq!(res["merge"]["processed"], 1, "{res}");
    assert_eq!(res["merge"]["items"][0]["state"], "merged", "{res}");
    assert_eq!(res["promoted"], serde_json::json!(["asf-2"]), "{res}");
    assert_eq!(repo.ok(&["issue", "show", &a])["issue"]["status"], "done");
    assert_eq!(repo.ok(&["issue", "show", &b])["issue"]["status"], "ready");

    let human = repo.asf().args(["serve", "--once"]).output().unwrap();
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("tick"), "{text}");
}

#[test]
fn serve_once_closes_epics_whose_children_are_all_done() {
    let repo = Repo::initialised();
    let cfg = repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("runtime = \"herdr\"", "runtime = \"fake\"");
    std::fs::write(&cfg, s).unwrap();
    let epic = repo.create_issue("epic", &["--type", "epic"]);
    let child = repo.create_issue("child", &["--parent", &epic]);

    let res = repo.ok(&["serve", "--once"]);
    assert_eq!(res["closed_epics"], serde_json::json!([]), "{res}");
    repo.ok(&["issue", "status", &child, "done"]);
    let res = repo.ok(&["serve", "--once"]);
    assert_eq!(res["closed_epics"], serde_json::json!([epic.clone()]), "{res}");
    assert_eq!(repo.ok(&["issue", "show", &epic])["issue"]["status"], "done");

    let human = repo.asf().args(["serve", "--once"]).output().unwrap();
    assert!(String::from_utf8_lossy(&human.stdout).contains("tick"));
}

#[test]
fn serve_interval_is_validated() {
    let repo = Repo::initialised();
    let out = repo
        .asf()
        .args(["serve", "--once", "--interval", "0"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn serve_dispatch_starts_ready_slots_from_the_plan() {
    let repo = Repo::initialised();
    let cfg = repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("runtime = \"herdr\"", "runtime = \"fake\"")
        .replace("max_parallel = 3", "max_parallel = 2");
    std::fs::write(&cfg, s).unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "cfg"]);
    let a = repo.create_issue("a", &["--ready", "--scope", "src/a.rs"]);
    let b = repo.create_issue("b", &["--ready", "--scope", "src/b.rs"]);
    let c = repo.create_issue("c", &["--ready", "--scope", "src/c.rs"]);
    let d = repo.create_issue("d", &["--dep", &a, "--scope", "docs/**"]);

    let res = repo.ok(&["serve", "--once", "--dispatch"]);
    assert_eq!(res["started"], serde_json::json!([a, b]), "capacity 2: {res}");
    assert_eq!(res["start_errors"].as_array().unwrap().len(), 0, "{res}");
    assert_eq!(
        repo.ok(&["issue", "show", &c])["issue"]["status"],
        "ready",
        "third waits for a slot"
    );
    assert_eq!(repo.ok(&["issue", "show", &d])["issue"]["status"], "backlog");
    assert_eq!(repo.ok(&["run", "list"]).as_array().unwrap().len(), 2);
    // without --dispatch nothing is started
    let res = repo.ok(&["serve", "--once"]);
    assert!(
        res.get("started").is_none() || res["started"].as_array().unwrap().is_empty(),
        "{res}"
    );
    assert_eq!(repo.ok(&["run", "list"]).as_array().unwrap().len(), 2);
    // a second dispatch tick is idempotent while the slots are full
    let res = repo.ok(&["serve", "--once", "--dispatch"]);
    assert_eq!(res["started"].as_array().unwrap().len(), 0, "{res}");
}

fn use_fake_runtime(repo: &Repo) {
    let cfg = repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("runtime = \"herdr\"", "runtime = \"fake\"");
    std::fs::write(&cfg, s).unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "cfg"]);
}

#[test]
fn until_done_stops_with_all_done_after_one_tick() {
    let repo = Repo::initialised();
    use_fake_runtime(&repo);
    let a = repo.create_issue("a", &["--scope", "src/a.rs"]);
    repo.ok(&["issue", "status", &a, "done"]);

    let res = repo.ok(&["serve", "--until-done"]);
    assert_eq!(res["reason"], "all_done", "{res}");
    assert_eq!(res["ticks"], 1, "{res}");
    assert_eq!(res["open"], serde_json::json!([]), "{res}");
    assert_eq!(res["runs"], serde_json::json!([]), "{res}");
}

#[test]
fn until_done_ignores_open_epics() {
    let repo = Repo::initialised();
    use_fake_runtime(&repo);
    // an epic without children is never closed automatically, yet it is not work to wait for
    repo.create_issue("epic", &["--type", "epic"]);
    let res = repo.ok(&["serve", "--until-done"]);
    assert_eq!(res["reason"], "all_done", "{res}");
}

#[test]
fn until_done_reports_stuck_with_exit_3_and_ok_true() {
    let repo = Repo::initialised();
    use_fake_runtime(&repo);
    let a = repo.create_issue("no scope", &["--ready"]);

    let (v, code) = repo.json(&["serve", "--until-done"]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["result"]["reason"], "stuck", "{v}");
    assert_eq!(v["result"]["ticks"], 1, "{v}");
    assert_eq!(v["result"]["open"], serde_json::json!([a.clone()]), "{v}");
    assert_eq!(v["result"]["runs"], serde_json::json!([]), "{v}");

    let human = repo.asf().args(["serve", "--until-done"]).output().unwrap();
    assert_eq!(human.status.code(), Some(3));
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("stuck") && text.contains(&a), "{text}");
}

#[test]
fn until_done_with_dispatch_is_stuck_only_when_the_plan_has_no_slot() {
    let repo = Repo::initialised();
    use_fake_runtime(&repo);
    // `a` is blocked by hand, so `b` can never be promoted and nothing is plannable or running
    let a = repo.create_issue("a", &["--scope", "src/a.rs"]);
    repo.ok(&["issue", "status", &a, "blocked"]);
    repo.create_issue("b", &["--dep", &a, "--scope", "src/b.rs"]);
    let (v, code) = repo.json(&["serve", "--until-done", "--dispatch"]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["result"]["reason"], "stuck", "{v}");
    assert_eq!(v["result"]["open"].as_array().unwrap().len(), 2, "{v}");
}

#[test]
fn until_done_with_dispatch_keeps_going_while_the_plan_has_a_slot() {
    let repo = Repo::initialised();
    use_fake_runtime(&repo);
    // an empty scope only clashes with occupied scopes, so a lone issue still gets a slot
    repo.create_issue("no scope", &["--ready"]);
    let out = repo
        .asf()
        .args([
            "serve",
            "--until-done",
            "--dispatch",
            "--max-ticks",
            "1",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    // the tick that started the worker is printed first, the summary is the last line
    let stdout = String::from_utf8_lossy(&out.stdout);
    let last: serde_json::Value = serde_json::from_str(stdout.lines().last().unwrap()).unwrap();
    let res = &last["result"];
    assert_eq!(res["reason"], "max_ticks", "{stdout}");
    assert_eq!(res["runs"].as_array().unwrap().len(), 1, "{stdout}");
}

#[test]
fn until_done_stops_at_max_ticks_and_prints_only_ticks_with_events() {
    let repo = Repo::initialised();
    use_fake_runtime(&repo);
    let a = repo.create_issue("a", &["--ready", "--scope", "src/a.rs"]);

    let out = repo
        .asf()
        .args([
            "serve",
            "--until-done",
            "--max-ticks",
            "2",
            "--interval",
            "1",
            "--dispatch",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<serde_json::Value> = stdout.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    // tick 1 started the worker (an event); tick 2 was quiet and printed nothing
    assert_eq!(lines.len(), 2, "{stdout}");
    assert_eq!(lines[0]["result"]["started"], serde_json::json!([a.clone()]));
    let summary = &lines[1]["result"];
    assert_eq!(lines[1]["ok"], true);
    assert_eq!(summary["reason"], "max_ticks", "{stdout}");
    assert_eq!(summary["ticks"], 2, "{stdout}");
    assert_eq!(summary["open"], serde_json::json!([a.clone()]), "{stdout}");
    assert_eq!(summary["runs"].as_array().unwrap().len(), 1, "{stdout}");
    assert_eq!(summary["runs"][0]["issue"], a.as_str(), "{stdout}");
}

#[test]
fn until_done_flag_combinations_are_usage_errors() {
    let repo = Repo::initialised();
    for args in [
        &["serve", "--once", "--until-done"][..],
        &["serve", "--max-ticks", "2"][..],
        &["serve", "--until-done", "--max-ticks", "0"][..],
    ] {
        let out = repo.asf().args(args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}: {out:?}");
    }
}

/// Fake runtime and `max_parallel = 1`, committed (like `use_fake_runtime`).
fn fake_runtime_one_slot(repo: &Repo) {
    let cfg = repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("runtime = \"herdr\"", "runtime = \"fake\"")
        .replace("max_parallel = 3", "max_parallel = 1");
    std::fs::write(&cfg, s).unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "cfg"]);
}

/// A running `asf serve ... --json` whose first stdout line has already been read, so the test can
/// edit the config while the loop sleeps between ticks.
struct LiveServe {
    child: std::process::Child,
    stdout: std::io::BufReader<std::process::ChildStdout>,
    first: serde_json::Value,
}

impl LiveServe {
    fn start(repo: &Repo, args: &[&str]) -> LiveServe {
        use std::io::BufRead;
        use std::process::Stdio;
        let mut child = repo
            .asf()
            .args(args)
            .arg("--json")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let first = serde_json::from_str(line.trim()).unwrap_or_else(|e| panic!("first line {line:?}: {e}"));
        LiveServe { child, stdout, first }
    }

    /// Waits for the process; returns the remaining stdout lines (JSON) and the whole stderr.
    fn finish(mut self) -> (Vec<serde_json::Value>, String) {
        use std::io::Read;
        let mut rest = String::new();
        self.stdout.read_to_string(&mut rest).unwrap();
        let mut err = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut err)
            .unwrap();
        self.child.wait().unwrap();
        let lines = rest.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        (lines, err)
    }
}

#[test]
fn serve_picks_up_a_config_edit_on_the_next_tick() {
    let repo = Repo::initialised();
    fake_runtime_one_slot(&repo);
    let a = repo.create_issue("a", &["--ready", "--scope", "src/a.rs"]);
    let b = repo.create_issue("b", &["--ready", "--scope", "src/b.rs"]);

    let live = LiveServe::start(
        &repo,
        &[
            "serve",
            "--until-done",
            "--dispatch",
            "--max-ticks",
            "3",
            "--interval",
            "2",
        ],
    );
    assert_eq!(
        live.first["result"]["started"],
        serde_json::json!([a.clone()]),
        "one slot"
    );
    // widen the capacity while the loop sleeps; the next tick must see it
    let cfg = repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg).unwrap();
    assert!(s.contains("max_parallel = 1"), "{s}");
    std::fs::write(&cfg, s.replace("max_parallel = 1", "max_parallel = 2")).unwrap();

    let (lines, stderr) = live.finish();
    assert_eq!(lines.len(), 2, "tick 2 started b, then the summary: {lines:?}");
    assert_eq!(
        lines[0]["result"]["started"],
        serde_json::json!([b.clone()]),
        "{lines:?}"
    );
    assert_eq!(
        lines[1]["result"]["runs"].as_array().unwrap().len(),
        2,
        "{lines:?}"
    );
    assert!(!stderr.contains("keeping the previous config"), "{stderr}");
}

#[test]
fn serve_keeps_the_old_config_and_warns_once_per_distinct_invalid_content() {
    let repo = Repo::initialised();
    fake_runtime_one_slot(&repo);
    let a = repo.create_issue("a", &["--ready", "--scope", "src/a.rs"]);
    repo.create_issue("b", &["--ready", "--scope", "src/b.rs"]);

    let live = LiveServe::start(
        &repo,
        &[
            "serve",
            "--until-done",
            "--dispatch",
            "--max-ticks",
            "4",
            "--interval",
            "1",
        ],
    );
    assert_eq!(live.first["result"]["started"], serde_json::json!([a.clone()]));
    let cfg = repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg).unwrap();
    // a type error: the file no longer parses, for three ticks in a row
    std::fs::write(&cfg, s.replace("max_parallel = 1", "max_parallel = \"many\"")).unwrap();

    let (lines, stderr) = live.finish();
    assert_eq!(lines.len(), 1, "no more starts, only the summary: {lines:?}");
    let summary = &lines[0]["result"];
    assert_eq!(summary["reason"], "max_ticks", "{lines:?}");
    assert_eq!(summary["ticks"], 4, "{lines:?}");
    // the old max_parallel = 1 still applies: b never got a slot (the default 3 would have given one)
    assert_eq!(summary["runs"].as_array().unwrap().len(), 1, "{lines:?}");
    assert_eq!(
        stderr.matches("keeping the previous config").count(),
        1,
        "one warning for unchanged content, not one per tick: {stderr}"
    );
}

/// `serve --until-done --json` prints a line per eventful tick, then the summary: parse the last.
fn last_line_json(repo: &Repo, args: &[&str]) -> (serde_json::Value, i32) {
    let out = repo.asf().args(args).arg("--json").output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v = serde_json::from_str(stdout.lines().last().unwrap()).unwrap();
    (v, out.status.code().unwrap_or(-1))
}

fn ask_human(repo: &Repo, issue: &str, body: &str) -> i64 {
    let q = repo.ok(&[
        "--lane", issue, "issue", "comment", issue, "--type", "QUESTION", "--to", "human", "--body", body,
    ]);
    q["id"].as_i64().unwrap()
}

#[test]
fn serve_once_reports_questions_waiting_in_json_and_text() {
    let repo = Repo::initialised();
    use_fake_runtime(&repo);
    let a = repo.create_issue("a", &["--scope", "src/a.rs"]);

    let res = repo.ok(&["serve", "--once"]);
    assert_eq!(res["questions_waiting"], 0, "always present, even at zero: {res}");
    let quiet = repo.asf().args(["serve", "--once"]).output().unwrap();
    assert!(!String::from_utf8_lossy(&quiet.stdout).contains("waiting for you"));

    ask_human(&repo, &a, "which crate?");
    let res = repo.ok(&["serve", "--once"]);
    assert_eq!(res["questions_waiting"], 1, "{res}");
    let human = repo.asf().args(["serve", "--once"]).output().unwrap();
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("1 waiting for you"), "{text}");
}

#[test]
fn until_done_summary_reports_questions_waiting_until_they_are_answered() {
    let repo = Repo::initialised();
    use_fake_runtime(&repo);
    // without --dispatch nothing is ever started: the loop is stuck after one tick
    let a = repo.create_issue("a", &["--scope", "src/a.rs"]);
    let qid = ask_human(&repo, &a, "which crate?");

    let (v, code) = last_line_json(&repo, &["serve", "--until-done"]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["result"]["reason"], "stuck", "{v}");
    assert_eq!(v["result"]["questions_waiting"], 1, "{v}");
    let human = repo.asf().args(["serve", "--until-done"]).output().unwrap();
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("1 waiting for you"), "{text}");

    repo.ok(&[
        "--lane",
        "human",
        "issue",
        "comment",
        &a,
        "--type",
        "ANSWER",
        "--to",
        &a,
        "--reply-to",
        &qid.to_string(),
        "--body",
        "asf-core",
    ]);
    let (v, _) = last_line_json(&repo, &["serve", "--until-done"]);
    assert_eq!(
        v["result"]["questions_waiting"], 0,
        "an answered question stops counting: {v}"
    );
    let human = repo.asf().args(["serve", "--until-done"]).output().unwrap();
    assert!(!String::from_utf8_lossy(&human.stdout).contains("waiting for you"));
}
