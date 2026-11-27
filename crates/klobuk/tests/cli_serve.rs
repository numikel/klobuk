mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{git, Repo};
use klobuk_core::{IssueStore, MergeState, NewEvent};
use klobuk_runtime::FakeRuntime;
use klobuk_store::SqliteStore;

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

    let human = repo.klobuk().args(["serve", "--once"]).output().unwrap();
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

    let human = repo.klobuk().args(["serve", "--once"]).output().unwrap();
    assert!(String::from_utf8_lossy(&human.stdout).contains("tick"));
}

#[test]
fn serve_interval_is_validated() {
    let repo = Repo::initialised();
    let out = repo
        .klobuk()
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

    let human = repo.klobuk().args(["serve", "--until-done"]).output().unwrap();
    assert_eq!(human.status.code(), Some(3));
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("stuck") && text.contains(&a), "{text}");
}

#[test]
fn until_done_with_dispatch_is_stuck_only_when_the_plan_has_no_slot() {
    let repo = Repo::initialised();
    use_fake_runtime(&repo);
    // `a` is cancelled, so `b` can never be promoted: a dead end, nothing is plannable or running and
    // nothing waits for the owner (a `blocked` `a` would, see `until_done_waits_for_what_the_owner_can_resolve`)
    let a = repo.create_issue("a", &["--scope", "src/a.rs"]);
    repo.ok(&["issue", "status", &a, "cancelled"]);
    repo.create_issue("b", &["--dep", &a, "--scope", "src/b.rs"]);
    // this test is about `stuck`: no alert report may precede the single summary object
    set_notify(&repo, "[notify]\non_new = false\n");
    let (v, code) = repo.json(&["serve", "--until-done", "--dispatch"]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["result"]["reason"], "stuck", "{v}");
    assert_eq!(v["result"]["open"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(
        v["result"]["waiting"],
        serde_json::json!([]),
        "nothing asks for the owner: {v}"
    );
}

#[test]
fn until_done_with_dispatch_keeps_going_while_the_plan_has_a_slot() {
    let repo = Repo::initialised();
    use_fake_runtime(&repo);
    // an empty scope only clashes with occupied scopes, so a lone issue still gets a slot
    repo.create_issue("no scope", &["--ready"]);
    let out = repo
        .klobuk()
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
        .klobuk()
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
        &["serve", "--exit-when-waiting"][..],
        &["serve", "--once", "--exit-when-waiting"][..],
    ] {
        let out = repo.klobuk().args(args).output().unwrap();
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

/// How long a test waits for the loop to reach a state before it gives up. It only matters when the loop
/// hangs: a test that waits for a condition returns the moment it holds. The 37 tests of this file run
/// in parallel next to other cargo jobs and the antivirus, where one test alone can take a minute.
const WAIT: Duration = Duration::from_secs(240);

/// A running `klobuk serve ... --json` whose first stdout line has already been read, so the test can
/// edit the config while the loop sleeps between ticks. A thread reads the pipes, so a test can look at
/// the lines the loop has printed so far, wait for a condition and then stop the loop itself: no test
/// depends on the loop ending by its own tick count.
struct LiveServe {
    child: std::process::Child,
    lines: std::sync::mpsc::Receiver<String>,
    out_reader: Option<std::thread::JoinHandle<()>>,
    err_reader: Option<std::thread::JoinHandle<()>>,
    stderr: std::sync::Arc<std::sync::Mutex<String>>,
    first: serde_json::Value,
    /// The lines after the first one that `wait_until` has taken from the pipe.
    seen: Vec<serde_json::Value>,
}

impl LiveServe {
    fn start(repo: &Repo, args: &[&str]) -> LiveServe {
        use std::io::{BufRead, Read};
        use std::process::Stdio;
        let mut child = repo
            .klobuk()
            .args(args)
            .arg("--json")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, lines) = std::sync::mpsc::channel();
        let out_reader = std::thread::spawn(move || {
            for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut err = child.stderr.take().unwrap();
        let stderr = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let sink = stderr.clone();
        let err_reader = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = err.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap().push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        });
        let mut live = LiveServe {
            child,
            lines,
            out_reader: Some(out_reader),
            err_reader: Some(err_reader),
            stderr,
            first: serde_json::Value::Null,
            seen: Vec::new(),
        };
        let line = live.recv("the first line");
        live.first = serde_json::from_str(line.trim()).unwrap_or_else(|e| panic!("first line {line:?}: {e}"));
        live
    }

    fn recv(&mut self, what: &str) -> String {
        match self.lines.recv_timeout(WAIT) {
            Ok(l) => l,
            Err(e) => panic!(
                "no {what} from `klobuk serve` within {WAIT:?}: {e}\n{}",
                self.stderr.lock().unwrap()
            ),
        }
    }

    /// Blocks until the loop prints its next line (a tick with an event, or the summary).
    fn next_line(&mut self) -> serde_json::Value {
        let line = self.recv("next line");
        serde_json::from_str(line.trim()).unwrap_or_else(|e| panic!("next line {line:?}: {e}"))
    }

    /// Polls `cond` every 250 ms, with the lines printed so far, until it holds. Panics when the loop
    /// ends first or `WAIT` passes: the test asserts on the condition, never on the loop having ended.
    fn wait_until(&mut self, mut cond: impl FnMut(&[serde_json::Value]) -> bool) {
        let deadline = std::time::Instant::now() + WAIT;
        loop {
            while let Ok(l) = self.lines.try_recv() {
                if let Ok(v) = serde_json::from_str(l.trim()) {
                    self.seen.push(v);
                }
            }
            if cond(&self.seen) {
                return;
            }
            let gone = matches!(self.child.try_wait(), Ok(Some(_)));
            assert!(
                !gone,
                "`klobuk serve` ended before the condition held: {:?}\n{}",
                self.seen,
                self.stderr.lock().unwrap()
            );
            assert!(
                std::time::Instant::now() < deadline,
                "the condition did not hold within {WAIT:?}: {:?}\n{}",
                self.seen,
                self.stderr.lock().unwrap()
            );
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    /// Ends the loop now; returns the lines after the first one and the stderr so far.
    fn stop(mut self) -> (Vec<serde_json::Value>, String) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // a gate command that outlived the loop may keep the pipes open: take what is there
        while let Ok(l) = self.lines.recv_timeout(Duration::from_secs(2)) {
            if let Ok(v) = serde_json::from_str(l.trim()) {
                self.seen.push(v);
            }
        }
        let err = self.stderr.lock().unwrap().clone();
        (std::mem::take(&mut self.seen), err)
    }

    /// Whether the loop is still going.
    fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Waits for a loop that ends by itself (at most `WAIT`; a loop that does not end is a failure, and
    /// `Drop` kills it); returns the lines after the first one and the stderr.
    fn finish(mut self) -> (Vec<serde_json::Value>, String) {
        let deadline = std::time::Instant::now() + WAIT;
        while self.running() {
            assert!(
                std::time::Instant::now() < deadline,
                "`klobuk serve` did not end within {WAIT:?}"
            );
            std::thread::sleep(Duration::from_millis(250));
        }
        self.child.wait().unwrap();
        for l in self.lines.iter() {
            self.seen
                .push(serde_json::from_str(l.trim()).unwrap_or_else(|e| panic!("line {l:?}: {e}")));
        }
        for t in [self.out_reader.take(), self.err_reader.take()]
            .into_iter()
            .flatten()
        {
            t.join().unwrap();
        }
        let err = self.stderr.lock().unwrap().clone();
        (std::mem::take(&mut self.seen), err)
    }
}

impl Drop for LiveServe {
    fn drop(&mut self) {
        // a test that panicked must not leave the loop running
        let _ = self.child.kill();
        let _ = self.child.wait();
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
    let out = repo.klobuk().args(args).arg("--json").output().unwrap();
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
    let quiet = repo.klobuk().args(["serve", "--once"]).output().unwrap();
    assert!(!String::from_utf8_lossy(&quiet.stdout).contains("waiting for you"));

    ask_human(&repo, &a, "which crate?");
    let res = repo.ok(&["serve", "--once"]);
    assert_eq!(res["questions_waiting"], 1, "{res}");
    let human = repo.klobuk().args(["serve", "--once"]).output().unwrap();
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("1 waiting for you"), "{text}");
}

#[test]
fn until_done_summary_reports_questions_waiting_until_they_are_answered() {
    let repo = Repo::initialised();
    use_fake_runtime(&repo);
    // without --dispatch nothing is ever started; --exit-when-waiting ends the loop after one tick
    // although the question could be answered (the default would wait for it)
    let a = repo.create_issue("a", &["--scope", "src/a.rs"]);
    let qid = ask_human(&repo, &a, "which crate?");

    let (v, code) = last_line_json(&repo, &["serve", "--until-done", "--exit-when-waiting"]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["result"]["reason"], "stuck", "{v}");
    assert_eq!(v["result"]["questions_waiting"], 1, "{v}");
    assert_eq!(
        v["result"]["waiting"],
        serde_json::json!([{ "issue": a.clone(), "kind": "question", "reason": "which crate?" }]),
        "{v}"
    );
    let human = repo
        .klobuk()
        .args(["serve", "--until-done", "--exit-when-waiting"])
        .output()
        .unwrap();
    assert_eq!(human.status.code(), Some(3));
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("1 waiting for you"), "{text}");
    assert!(
        text.contains(&format!("waits for you: {a}: question — which crate?")),
        "{text}"
    );

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
        "klobuk-core",
    ]);
    let (v, _) = last_line_json(&repo, &["serve", "--until-done"]);
    assert_eq!(
        v["result"]["questions_waiting"], 0,
        "an answered question stops counting: {v}"
    );
    let human = repo.klobuk().args(["serve", "--until-done"]).output().unwrap();
    assert_eq!(
        human.status.code(),
        Some(3),
        "answered, nothing waits for the owner and nothing starts without --dispatch: a dead end"
    );
    assert!(!String::from_utf8_lossy(&human.stdout).contains("waiting for you"));
}

// ---- waiting for the owner is the default of --until-done (asf-217) ----

/// The pulse the loop keeps writing, or `Null` for the instant a rewrite makes it unreadable.
fn pulse_now(repo: &Repo) -> serde_json::Value {
    std::fs::read_to_string(repo.path().join(".factory/runs/serve.json"))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(serde_json::Value::Null)
}

/// `(issue, what)` of what the pulse says the idle loop waits for.
fn pulse_waiting(repo: &Repo) -> Vec<(String, String)> {
    pulse_now(repo)["waiting"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|w| {
                    (
                        w["issue"].as_str().unwrap_or_default().to_string(),
                        w["what"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The notice the loop prints when it becomes idle on the owner (the summary has a `reason`, it has not).
fn waiting_notice(lines: &[serde_json::Value]) -> Option<&serde_json::Value> {
    lines.iter().find(|l| {
        l["result"]["reason"].is_null() && l["result"]["waiting"].as_array().is_some_and(|w| !w.is_empty())
    })
}

#[test]
fn until_done_waits_for_a_blocked_worker_and_finishes_the_work_after_the_answer() {
    let repo = Repo::initialised();
    use_fake_runtime(&repo);
    let a = repo.create_issue("a", &["--ready", "--scope", "src/**"]);
    repo.ok(&["run", "start", &a]);
    // the worker sits on a permission prompt: `run watch` (the loop's first tick) turns the run `blocked`
    fake_runtime(&repo)
        .script(serde_json::json!({ "info": [{"state": "blocked"}], "screen": "Allow Bash(rm -rf)? (y/n)" }))
        .unwrap();

    let mut live = LiveServe::start(&repo, &LOOP);
    assert_eq!(
        live.first["result"]["watch"]["transitions"][0]["to"], "blocked",
        "{}",
        live.first
    );
    // nothing runs and the only obstacle is the owner: the loop says so and does not exit
    live.wait_until(|lines| waiting_notice(lines).is_some());
    let notice = waiting_notice(&live.seen).unwrap().clone();
    assert_eq!(notice["result"]["waiting"][0]["issue"], a.as_str(), "{notice}");
    assert_eq!(notice["result"]["waiting"][0]["kind"], "run_blocked", "{notice}");
    live.wait_until(|_| pulse_waiting(&repo) == [(a.clone(), "worker waits for input".to_string())]);
    // still alive: the pulse keeps ticking while it waits
    let ticks = pulse_now(&repo)["ticks"].as_u64().unwrap_or(0);
    live.wait_until(|_| pulse_now(&repo)["ticks"].as_u64().unwrap_or(0) >= ticks + 2);
    assert!(live.running());

    // the owner answers; the worker does its job and finishes. Nobody restarts the loop.
    repo.ok(&["run", "reply", &a, "y"]);
    let wt = repo.path().join(".worktrees").join(&a);
    commit_in(&wt, "src/x.rs", "x\n");
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

    let (lines, stderr) = live.finish();
    let summary = &lines.last().expect("the summary")["result"];
    assert_eq!(summary["reason"], "all_done", "{lines:?}\n{stderr}");
    assert_eq!(summary["waiting"], serde_json::json!([]), "{summary}");
    let status = repo.ok(&["issue", "show", &a])["issue"]["status"].clone();
    assert_eq!(status, "done", "{lines:?}");
    let p = pulse_now(&repo);
    assert_eq!(p["stop_reason"], "all_done", "{p}");
    assert_eq!(
        p["waiting"],
        serde_json::json!([]),
        "a stopped loop waits for nobody: {p}"
    );
}

#[test]
fn until_done_waits_for_what_the_owner_can_resolve_and_the_dead_ends_still_exit() {
    // A question nobody answered and a blocked issue are things only the owner can resolve; the loop
    // (no --dispatch here, so nothing could start by itself) must wait for them, not exit `stuck`.
    let repo = Repo::initialised();
    use_fake_runtime(&repo);
    let q = repo.create_issue("asks", &["--scope", "src/q.rs"]);
    let qid = ask_human(&repo, &q, "which crate?");
    let blocked = repo.create_issue("blocked", &["--scope", "src/b.rs"]);
    repo.ok(&["issue", "status", &blocked, "blocked"]);

    let mut live = LiveServe::start(&repo, &LOOP);
    live.wait_until(|lines| waiting_notice(lines).is_some());
    let notice = waiting_notice(&live.seen).unwrap().clone();
    let kinds: Vec<(String, String)> = notice["result"]["waiting"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| {
            (
                w["issue"].as_str().unwrap().to_string(),
                w["kind"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [
            (q.clone(), "question".to_string()),
            (blocked.clone(), "issue_blocked".to_string())
        ],
        "the most urgent first: {notice}"
    );
    live.wait_until(|_| pulse_waiting(&repo).len() == 2);

    // the owner unblocks the issue and answers the question: nothing waits any more and nothing can
    // start without --dispatch, so what remains is a dead end and the loop ends `stuck`, exit 3
    repo.ok(&["issue", "status", &blocked, "ready"]);
    repo.ok(&[
        "--lane",
        "human",
        "issue",
        "comment",
        &q,
        "--type",
        "ANSWER",
        "--to",
        &q,
        "--reply-to",
        &qid.to_string(),
        "--body",
        "klobuk-core",
    ]);
    let (lines, stderr) = live.finish();
    let summary = &lines.last().expect("the summary")["result"];
    assert_eq!(summary["reason"], "stuck", "{lines:?}\n{stderr}");
    assert_eq!(summary["waiting"], serde_json::json!([]), "{summary}");
}

#[test]
fn exit_when_waiting_ends_the_loop_where_the_default_waits() {
    let repo = Repo::initialised();
    use_fake_runtime(&repo);
    let a = repo.create_issue("a", &["--scope", "src/a.rs"]);
    repo.ok(&["issue", "status", &a, "blocked"]);

    // the old behaviour, for scripts that want the exit code
    let (v, code) = last_line_json(&repo, &["serve", "--until-done", "--exit-when-waiting"]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["result"]["reason"], "stuck", "{v}");
    assert_eq!(v["result"]["waiting"][0]["issue"], a.as_str(), "{v}");
    assert_eq!(v["result"]["waiting"][0]["kind"], "issue_blocked", "{v}");
    assert_eq!(pulse_now(&repo)["stop_reason"], "stuck");

    // the same repository without the flag keeps waiting
    let mut live = LiveServe::start(&repo, &LOOP);
    live.wait_until(|_| pulse_waiting(&repo) == [(a.clone(), "issue blocked".to_string())]);
    assert!(live.running());
    live.stop();
}

#[test]
fn max_ticks_ends_a_loop_in_both_modes() {
    let repo = Repo::initialised();
    use_fake_runtime(&repo);
    let a = repo.create_issue("a", &["--scope", "src/a.rs"]);
    repo.ok(&["issue", "status", &a, "blocked"]);
    // waiting (the default): only the limit ends the loop, and that is not a failure
    let (v, code) = last_line_json(
        &repo,
        &["serve", "--until-done", "--max-ticks", "2", "--interval", "1"],
    );
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["result"]["reason"], "max_ticks", "{v}");
    assert_eq!(v["result"]["ticks"], 2, "{v}");
    assert_eq!(v["result"]["waiting"][0]["issue"], a.as_str(), "{v}");

    // --exit-when-waiting: work that keeps the loop busy is still cut by the limit
    let repo = Repo::initialised();
    use_fake_runtime(&repo);
    let b = repo.create_issue("b", &["--ready", "--scope", "src/b.rs"]);
    let (v, code) = last_line_json(
        &repo,
        &[
            "serve",
            "--until-done",
            "--exit-when-waiting",
            "--dispatch",
            "--max-ticks",
            "2",
            "--interval",
            "1",
        ],
    );
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["result"]["reason"], "max_ticks", "{v}");
    assert_eq!(v["result"]["open"], serde_json::json!([b]), "{v}");
}

#[test]
fn the_reminders_keep_firing_while_the_loop_waits() {
    let (repo, id) = repo_with_an_old_blocked_issue();
    set_notify(&repo, "[notify]\non_new = false\n");

    let mut live = LiveServe::start(&repo, &LOOP);
    assert_eq!(
        live.first["result"]["reminders"][0]["issue"],
        id.as_str(),
        "{}",
        live.first
    );
    live.wait_until(|_| pulse_waiting(&repo) == [(id.clone(), "issue blocked".to_string())]);
    assert!(live.running(), "the loop waits for the owner, it does not exit");
    live.stop();
    assert_eq!(notifications(&repo).len(), 1, "the reminder went out once");
}

/// A repository on the fake runtime with one blocked issue that has waited since 2020.
fn repo_with_an_old_blocked_issue() -> (Repo, String) {
    let repo = Repo::initialised();
    let cfg = repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("runtime = \"herdr\"", "runtime = \"fake\"");
    std::fs::write(&cfg, s).unwrap();
    let id = repo.create_issue("stuck", &[]);
    repo.ok(&["issue", "status", &id, "blocked"]);
    repo.ok(&[
        "issue",
        "comment",
        &id,
        "--type",
        "BLOCKER",
        "--to",
        "human",
        "--body",
        "gate test timed out\nand a long tail",
    ]);
    // Nothing in the CLI moves time, so the issue is aged through an export/import round trip.
    let file = repo.path().join("aged.jsonl");
    repo.ok(&["export", "--out", file.to_str().unwrap()]);
    let aged: Vec<String> = std::fs::read_to_string(&file)
        .unwrap()
        .lines()
        .map(|l| {
            let mut v: serde_json::Value = serde_json::from_str(l).unwrap();
            if v["kind"] == "issue" {
                v["updated_at"] = "2020-01-01T00:00:00Z".into();
            }
            v.to_string()
        })
        .collect();
    std::fs::write(&file, aged.join("\n") + "\n").unwrap();
    repo.ok(&["import", file.to_str().unwrap(), "--mode", "replace"]);
    (repo, id)
}

fn notifications(repo: &Repo) -> Vec<serde_json::Value> {
    klobuk_runtime::FakeRuntime::new(repo.path().join(".factory/fake-runtime.json"))
        .log()
        .into_iter()
        .filter(|e| e.op == "notify")
        .map(|e| e.args)
        .collect()
}

/// Replaces the `[notify]` section (the last one in the template) with `section`.
fn set_notify(repo: &Repo, section: &str) {
    let cfg = repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg).unwrap();
    let start = s.find("\n[notify]").expect("[notify] in the template");
    std::fs::write(&cfg, format!("{}\n{section}", &s[..start])).unwrap();
}

#[test]
fn serve_reminds_once_about_a_blocked_issue_and_a_restart_does_not_repeat_it() {
    let (repo, id) = repo_with_an_old_blocked_issue();
    set_notify(&repo, "[notify]\non_new = false\n");
    let res = repo.ok(&["serve", "--once"]);
    let sent = res["reminders"].as_array().unwrap_or_else(|| panic!("{res}"));
    assert_eq!(sent.len(), 1, "{res}");
    assert_eq!(sent[0]["issue"], id.as_str(), "{res}");
    assert_eq!(sent[0]["kind"], "issue_blocked", "{res}");
    assert!(sent[0]["age_minutes"].as_u64().unwrap() > 60 * 24, "{res}");
    let text = sent[0]["text"].as_str().unwrap();
    assert!(
        text.starts_with(&format!("{id} has been blocked for ")) && text.ends_with(": gate test timed out"),
        "{text}"
    );
    assert_eq!(sent[0]["notify_error"], serde_json::Value::Null, "{res}");
    assert_eq!(sent[0]["command_error"], serde_json::Value::Null, "{res}");

    let n = notifications(&repo);
    assert_eq!(n.len(), 1, "{n:?}");
    assert_eq!(n[0]["title"], format!("klobuk {id} needs you"));
    assert_eq!(n[0]["body"], text);

    // a new `klobuk serve` process: the reminder time comes back from the store
    let again = repo.ok(&["serve", "--once"]);
    assert_eq!(again["reminders"], serde_json::json!([]), "{again}");
    assert_eq!(notifications(&repo).len(), 1, "no second notification");
}

#[test]
fn serve_prints_the_reminder_in_the_human_report() {
    let (repo, id) = repo_with_an_old_blocked_issue();
    set_notify(&repo, "[notify]\non_new = false\n");
    let out = repo.klobuk().args(["serve", "--once"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains(&format!("reminded: {id} has been blocked for ")),
        "{text}"
    );
}

#[test]
fn zero_after_minutes_switches_reminders_off() {
    let (repo, _) = repo_with_an_old_blocked_issue();
    set_notify(&repo, "[notify]\nafter_minutes = 0\non_new = false\n");
    let res = repo.ok(&["serve", "--once"]);
    assert_eq!(res["reminders"], serde_json::json!([]), "{res}");
    assert_eq!(res["alerts"], serde_json::json!([]), "{res}");
    assert!(notifications(&repo).is_empty());
}

#[test]
fn a_notify_command_runs_without_a_shell_and_its_failure_never_fails_the_tick() {
    let (repo, id) = repo_with_an_old_blocked_issue();
    // `git init <dir>` leaves a directory behind: proof the command ran, with no shell involved.
    let proof = repo.path().join("proof-repo");
    set_notify(
        &repo,
        &format!(
            "[notify]\non_new = false\ncommand = [\"git\", \"init\", \"-q\", \"{}\"]\n",
            proof.to_string_lossy().replace('\\', "/")
        ),
    );
    let res = repo.ok(&["serve", "--once"]);
    assert_eq!(res["reminders"][0]["issue"], id.as_str(), "{res}");
    assert_eq!(
        res["reminders"][0]["command_error"],
        serde_json::Value::Null,
        "{res}"
    );
    assert!(proof.join(".git").exists(), "the command did not run");

    // a program that does not exist: reported in the tick, exit code still 0, reminder still recorded
    let (repo, _) = repo_with_an_old_blocked_issue();
    set_notify(
        &repo,
        "[notify]\non_new = false\ncommand = [\"klobuk-no-such-program-xyz\"]\n",
    );
    let (v, code) = repo.json(&["serve", "--once"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["ok"], true, "{v}");
    let err = v["result"]["reminders"][0]["command_error"]
        .as_str()
        .unwrap_or_default();
    assert!(err.contains("cannot start `klobuk-no-such-program-xyz`"), "{v}");
    assert_eq!(notifications(&repo).len(), 1, "the notification went out anyway");
    let again = repo.ok(&["serve", "--once"]);
    assert_eq!(again["reminders"], serde_json::json!([]), "{again}");
}

#[test]
fn a_fresh_wait_is_not_reminded_yet() {
    let repo = Repo::initialised();
    use_fake_runtime(&repo);
    let id = repo.create_issue("fresh", &[]);
    repo.ok(&["issue", "status", &id, "blocked"]);
    let res = repo.ok(&["serve", "--once"]);
    assert_eq!(res["reminders"], serde_json::json!([]), "{res}");
}

// ---- on_new: the notification at the moment something new waits for the human ----

fn comment_to_human(repo: &Repo, id: &str, kind: &str, body: &str) {
    repo.ok(&[
        "issue", "comment", id, "--type", kind, "--to", "human", "--body", body,
    ]);
}

#[test]
fn serve_notifies_at_once_about_a_new_wait_and_a_restart_neither_repeats_nor_loses_it() {
    let (repo, id) = repo_with_an_old_blocked_issue();
    let res = repo.ok(&["serve", "--once"]);
    let alerts = res["alerts"].as_array().unwrap_or_else(|| panic!("{res}"));
    assert_eq!(alerts.len(), 1, "{res}");
    assert_eq!(alerts[0]["issue"], id.as_str(), "{res}");
    assert_eq!(alerts[0]["kind"], "blocker", "the BLOCKER comment leads: {res}");
    assert_eq!(alerts[0]["age_minutes"], 0, "{res}");
    assert_eq!(
        alerts[0]["text"],
        format!("klobuk {id} is blocked: gate test timed out"),
        "{res}"
    );
    assert_eq!(alerts[0]["notify_error"], serde_json::Value::Null, "{res}");
    assert_eq!(
        res["reminders"],
        serde_json::json!([]),
        "an alert is not also a reminder: {res}"
    );

    let n = notifications(&repo);
    assert_eq!(n.len(), 1, "{n:?}");
    assert_eq!(n[0]["title"], format!("klobuk {id} is blocked"));
    assert_eq!(n[0]["body"], "gate test timed out", "first line only");
    assert_eq!(n[0]["sound"], "request", "the attention sound, not the done one");

    // a new `klobuk serve` process: what was announced comes back from the store
    let again = repo.ok(&["serve", "--once"]);
    assert_eq!(again["alerts"], serde_json::json!([]), "{again}");
    assert_eq!(
        again["reminders"],
        serde_json::json!([]),
        "the first reminder is counted from the notification, not from the 2020 wait: {again}"
    );
    assert_eq!(notifications(&repo).len(), 1, "no second notification");

    // something new on the same issue is announced in the next tick
    comment_to_human(&repo, &id, "QUESTION", "Which branch should I use?");
    let next = repo.ok(&["serve", "--once"]);
    assert_eq!(next["alerts"].as_array().map(Vec::len), Some(1), "{next}");
    assert_eq!(next["alerts"][0]["kind"], "question", "{next}");
    assert_eq!(
        next["alerts"][0]["text"],
        format!("klobuk {id} asks: Which branch should I use?")
    );
    assert_eq!(notifications(&repo).len(), 2);
}

#[test]
fn something_posted_while_serve_was_not_running_is_announced_by_the_next_serve() {
    let repo = Repo::initialised();
    use_fake_runtime(&repo);
    let id = repo.create_issue("asks later", &[]);
    assert_eq!(repo.ok(&["serve", "--once"])["alerts"], serde_json::json!([]));
    comment_to_human(&repo, &id, "QUESTION", "Scope question?");
    let res = repo.ok(&["serve", "--once"]);
    assert_eq!(res["alerts"][0]["kind"], "question", "{res}");
    assert_eq!(notifications(&repo).len(), 1);
}

#[test]
fn zero_after_minutes_keeps_the_immediate_notification_and_on_new_false_keeps_the_reminders() {
    let (repo, id) = repo_with_an_old_blocked_issue();
    set_notify(&repo, "[notify]\nafter_minutes = 0\n");
    let res = repo.ok(&["serve", "--once"]);
    assert_eq!(res["alerts"].as_array().map(Vec::len), Some(1), "{res}");
    assert_eq!(res["reminders"], serde_json::json!([]), "{res}");

    let (repo, id2) = repo_with_an_old_blocked_issue();
    set_notify(&repo, "[notify]\non_new = false\n");
    let res = repo.ok(&["serve", "--once"]);
    assert_eq!(res["alerts"], serde_json::json!([]), "{res}");
    assert_eq!(res["reminders"][0]["issue"], id2.as_str(), "{res}");
    let _ = id;
}

#[test]
fn the_notify_command_also_runs_for_an_immediate_notification() {
    let (repo, id) = repo_with_an_old_blocked_issue();
    let proof = repo.path().join("proof-alert");
    set_notify(
        &repo,
        &format!(
            "[notify]\ncommand = [\"git\", \"init\", \"-q\", \"{}\"]\n",
            proof.to_string_lossy().replace('\\', "/")
        ),
    );
    let res = repo.ok(&["serve", "--once"]);
    assert_eq!(res["alerts"][0]["issue"], id.as_str(), "{res}");
    assert_eq!(
        res["alerts"][0]["command_error"],
        serde_json::Value::Null,
        "{res}"
    );
    assert!(proof.join(".git").exists(), "the command did not run");
}

#[test]
fn serve_prints_the_notification_in_the_human_report() {
    let (repo, id) = repo_with_an_old_blocked_issue();
    let out = repo.klobuk().args(["serve", "--once"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains(&format!("notified: klobuk {id} is blocked: gate test timed out")),
        "{text}"
    );
}

// ---- gate runs leave the tick (asf-66) ----

/// A gate that records its working directory, then waits until the `release` file exists (it gives
/// up after 90 s). While it waits, the gate run is "in flight" for as long as the test wants.
const HOLD_GATE: &str = concat!(
    "import os, sys, time\n",
    "open(sys.argv[1], 'a').write(os.getcwd() + '\\n')\n",
    "t = time.time()\n",
    "while not os.path.exists(sys.argv[2]):\n",
    "    if time.time() - t > 90:\n",
    "        sys.exit(9)\n",
    "    time.sleep(0.1)\n",
);

/// A gate that always fails.
const RED_GATE: &str = "import sys\nprint('red on purpose')\nsys.exit(7)\n";

/// A repository on the fake runtime with one gate named `hold` that runs in `stages` (a TOML array).
struct Gated {
    repo: Repo,
    log: PathBuf,
    release: PathBuf,
    _dir: tempfile::TempDir,
}

fn gated(script: &str, stages: &str, verify_main_after: bool) -> Gated {
    let repo = Repo::initialised();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("gate.py");
    std::fs::write(&file, script).unwrap();
    let log = dir.path().join("gate.log");
    let release = dir.path().join("release");
    let fwd = |p: &Path| p.to_str().unwrap().replace('\\', "/");
    let cfg = repo.path().join(".factory/config.toml");
    let mut s = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("runtime = \"herdr\"", "runtime = \"fake\"");
    if verify_main_after {
        assert!(
            s.contains("verify_main_after = false"),
            "template carries the flag"
        );
        s = s.replace("verify_main_after = false", "verify_main_after = true");
    }
    s.push_str(&format!(
        "\n[[gate]]\nname = \"hold\"\ncmd = ['python', '{}', '{}', '{}']\nwhen = {stages}\n",
        fwd(&file),
        fwd(&log),
        fwd(&release)
    ));
    std::fs::write(&cfg, s).unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "cfg"]);
    Gated {
        repo,
        log,
        release,
        _dir: dir,
    }
}

impl Gated {
    /// Where each run of the gate happened, oldest first.
    fn runs(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn release(&self) {
        std::fs::write(&self.release, "").unwrap();
    }

    fn store(&self) -> SqliteStore {
        SqliteStore::open(&self.repo.path().join(".factory/factory.db")).unwrap()
    }

    fn issue_status(&self, id: &str) -> String {
        self.repo.ok(&["issue", "show", id])["issue"]["status"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn event_kinds(&self) -> Vec<(String, Option<String>)> {
        self.store()
            .events_since(0, 10_000)
            .unwrap()
            .into_iter()
            .map(|e| (e.kind, e.issue_id.map(|i| i.to_string())))
            .collect()
    }
}

/// A worker that did its job and ran `klobuk issue finish`; nothing has verified it yet.
fn finished_worker(repo: &Repo, scope: &str, file: &str) -> String {
    finished_worker_with(repo, scope, file, &[])
}

/// `finished_worker` with more `klobuk issue create` arguments (a label, say).
fn finished_worker_with(repo: &Repo, scope: &str, file: &str, extra: &[&str]) -> String {
    let mut args = vec!["--ready", "--scope", scope];
    args.extend_from_slice(extra);
    let id = repo.create_issue(file, &args);
    repo.ok(&["run", "start", &id]);
    let wt = repo.path().join(".worktrees").join(&id);
    commit_in(&wt, file, "x\n");
    let report = wt.join(".factory").join("runs").join(&id).join("report.md");
    std::fs::write(&report, "# done\n").unwrap();
    repo.ok(&[
        "--lane",
        &id,
        "issue",
        "finish",
        &id,
        "--report",
        report.to_str().unwrap(),
    ]);
    id
}

/// `finished_worker` plus the (inline) verification: the item waits in the merge queue.
fn queued_item(repo: &Repo, scope: &str, file: &str) -> String {
    let id = finished_worker(repo, scope, file);
    let res = repo.ok(&["run", "watch", "--once"]);
    assert_eq!(res["transitions"][0]["to"], "succeeded", "{res}");
    id
}

/// `queued_item` for an issue that carries the label `risky`, which `[review] on_labels` turns into a review.
fn risky_queued_item(repo: &Repo, scope: &str, file: &str) -> String {
    let id = finished_worker_with(repo, scope, file, &["--label", "risky"]);
    let res = repo.ok(&["run", "watch", "--once"]);
    assert_eq!(res["transitions"][0]["to"], "succeeded", "{res}");
    id
}

fn review_on_label_risky(g: &Gated) {
    let cfg = g.repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg).unwrap();
    assert!(s.contains("on_labels = []"), "template carries on_labels");
    std::fs::write(&cfg, s.replacen("on_labels = []", "on_labels = [\"risky\"]", 1)).unwrap();
    git(g.repo.path(), &["add", ".factory/config.toml"]);
    git(g.repo.path(), &["commit", "-q", "-m", "review"]);
}

/// The reviewer's verdict on `issue`: a DECISION authored by the lane of its newest review issue.
fn review_verdict(g: &Gated, issue: &str, body: &str) {
    let reviews = g
        .store()
        .list_issues(&klobuk_core::IssueFilter {
            kind: Some(klobuk_core::IssueType::Review),
            parent: Some(issue.parse().unwrap()),
            ..klobuk_core::IssueFilter::default()
        })
        .unwrap();
    let rid = reviews.last().expect("a reviewer was started").id.to_string();
    g.repo.ok(&[
        "--lane", &rid, "issue", "comment", issue, "--type", "DECISION", "--to", "human", "--body", body,
    ]);
}

fn reviewers_running(g: &Gated) -> u64 {
    g.repo.ok(&["run", "plan"])["reviewers_running"].as_u64().unwrap()
}

fn main_head(g: &Gated) -> String {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "main"])
        .current_dir(g.repo.path())
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn queue_states(g: &Gated) -> Vec<MergeState> {
    g.store().merge_queue().unwrap().iter().map(|m| m.state).collect()
}

/// The tick limit can never be hit in a test: under load a tick with a gate or a git call takes seconds,
/// so no tick count is both short enough to be a safety net and long enough for the work to finish. The
/// test owns the loop's lifetime instead: `LiveServe::wait_until` has a deadline and `stop` kills the loop.
const LOOP: [&str; 6] = [
    "serve",
    "--until-done",
    "--max-ticks",
    "100000",
    "--interval",
    "1",
];

fn all_merged(g: &Gated, n: usize) -> bool {
    let states = queue_states(g);
    states.len() == n && states.iter().all(|s| *s == MergeState::Merged)
}

fn merged_events(g: &Gated) -> usize {
    g.event_kinds()
        .iter()
        .filter(|(k, _)| k == "merge.merged")
        .count()
}

#[test]
fn a_merge_gate_in_flight_does_not_stop_the_loop() {
    let g = gated(HOLD_GATE, "[\"merge\"]", false);
    let a = queued_item(&g.repo, "src/**", "src/a.rs");
    let b = g.repo.create_issue("later", &["--ready", "--scope", "docs/**"]);

    let mut args = LOOP.to_vec();
    args.push("--dispatch");
    let mut live = LiveServe::start(&g.repo, &args);
    // this line exists only because the tick ended while the gate is still waiting for `release`
    let first = &live.first["result"];
    assert_eq!(first["gate_run"]["kind"], "merge", "{first}");
    assert_eq!(first["gate_run"]["issue"], a.as_str(), "{first}");
    assert_eq!(first["gate_run"]["gate"], "hold", "{first}");
    assert!(first["gate_run"]["seconds"].is_u64(), "{first}");
    assert_eq!(
        first["started"],
        serde_json::json!([b.clone()]),
        "a ready issue is dispatched in the tick that starts the gate run: {first}"
    );
    assert_eq!(queue_states(&g), vec![MergeState::Gating]);
    assert!(!g.release.exists());

    // a question raised during the gate run is announced by a later tick, the gate still held
    g.repo.ok(&[
        "issue",
        "comment",
        &b,
        "--type",
        "QUESTION",
        "--to",
        "human",
        "--body",
        "Which table?",
    ]);
    let alert = live.next_line();
    assert_eq!(alert["result"]["alerts"][0]["kind"], "question", "{alert}");
    assert_eq!(
        alert["result"]["gate_run"]["issue"],
        a.as_str(),
        "still in flight: {alert}"
    );
    assert_eq!(queue_states(&g), vec![MergeState::Gating]);

    g.release();
    live.wait_until(|lines| {
        let reported = lines.iter().any(|l| {
            l["result"]["merge"]["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|m| m["state"] == "merged"))
        });
        reported && all_merged(&g, 1)
    });
    let (lines, stderr) = live.stop();
    assert_eq!(queue_states(&g), vec![MergeState::Merged], "{lines:?}\n{stderr}");
    assert_eq!(g.issue_status(&a), "done");
    assert_eq!(g.runs().len(), 1, "the gate ran once: {:?}", g.runs());
    let merged = lines
        .iter()
        .flat_map(|l| {
            l["result"]["merge"]["items"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .any(|m| m["state"] == "merged");
    assert!(
        merged,
        "the tick that reaped the result reports the merge: {lines:?}"
    );
}

#[test]
fn serve_does_not_exit_at_the_tick_limit_while_a_gate_run_is_in_flight() {
    let g = gated(HOLD_GATE, "[\"merge\"]", false);
    let a = queued_item(&g.repo, "src/**", "src/a.rs");

    // two ticks only, one second apart; the gate is held for much longer than that
    let mut live = LiveServe::start(
        &g.repo,
        &["serve", "--until-done", "--max-ticks", "2", "--interval", "1"],
    );
    assert_eq!(
        live.first["result"]["gate_run"]["issue"],
        a.as_str(),
        "{}",
        live.first
    );
    std::thread::sleep(Duration::from_secs(5)); // well past the limit
    assert!(
        live.running(),
        "serve left while its gate command was still running: the process would outlive klobuk"
    );
    assert_eq!(queue_states(&g), vec![MergeState::Gating]);

    g.release();
    let (lines, stderr) = live.finish();
    // it exited only after the result was concluded: merged, not left `gating`
    assert_eq!(queue_states(&g), vec![MergeState::Merged], "{lines:?}\n{stderr}");
    assert_eq!(g.issue_status(&a), "done");
    // the limit was passed long ago; with the run concluded and nothing open, `all_done` is the reason
    let summary = &lines.last().expect("the summary")["result"];
    assert_eq!(summary["reason"], "all_done", "{lines:?}");
    assert!(summary["ticks"].as_u64().unwrap() > 2, "{summary}");
    assert_eq!(g.runs().len(), 1, "the gate ran once: {:?}", g.runs());
}

#[test]
fn a_review_verdict_is_read_while_a_gate_run_is_in_flight() {
    let g = gated(HOLD_GATE, "[\"merge\"]", false);
    review_on_label_risky(&g);
    let r = risky_queued_item(&g.repo, "docs/**", "docs/r.md");
    let b = queued_item(&g.repo, "src/**", "src/b.rs");
    // `r` goes into review while the gate is open; then the gate closes again for `b`
    g.release();
    let res = g.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "reviewing", "{res}");
    std::fs::remove_file(&g.release).unwrap();
    assert_eq!(reviewers_running(&g), 1);

    let mut live = LiveServe::start(&g.repo, &LOOP);
    assert_eq!(
        live.first["result"]["gate_run"]["issue"],
        b.as_str(),
        "{}",
        live.first
    );
    review_verdict(&g, &r, "approve");

    // the tick that reads the verdict is printed while `b`'s gate is still held
    let line = live.next_line();
    let items = line["result"]["merge"]["items"].as_array().unwrap();
    assert!(
        items
            .iter()
            .any(|m| m["issue_id"] == r.as_str() && m["state"] == "merged"),
        "{line}"
    );
    assert_eq!(
        line["result"]["gate_run"]["issue"],
        b.as_str(),
        "still in flight: {line}"
    );
    assert_eq!(reviewers_running(&g), 0, "the reviewer's slot is free again");
    assert!(g
        .event_kinds()
        .contains(&("merge.merged".to_string(), Some(r.clone()))));

    g.release();
    live.wait_until(|_| all_merged(&g, 2));
    live.stop();
}

#[test]
fn an_approved_item_is_not_merged_while_main_is_being_checked() {
    let g = gated(HOLD_GATE, "[\"main\"]", true);
    review_on_label_risky(&g);
    let a = queued_item(&g.repo, "src/**", "src/a.rs");
    let r = risky_queued_item(&g.repo, "docs/**", "docs/r.md");
    // `a` has no review and merges first (the open gate lets its check through); `r` is then rebased
    // onto that main and enters review, so its approval would fast-forward cleanly
    g.release();
    let res = g.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    let res = g.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "reviewing", "{res}");
    // the inline tick ran that check to its end; owe it again so that the loop has one to hold
    g.store()
        .append_event(&NewEvent {
            kind: "merge.main_check_pending".into(),
            issue_id: Some(a.parse().unwrap()),
            run_id: None,
            actor: Some("orchestrator".into()),
            payload: serde_json::json!({}),
        })
        .unwrap();
    std::fs::remove_file(&g.release).ok();
    let before = main_head(&g);

    let mut live = LiveServe::start(&g.repo, &LOOP);
    assert_eq!(live.first["result"]["gate_run"]["kind"], "main", "{}", live.first);
    review_verdict(&g, &r, "approve");
    std::thread::sleep(Duration::from_secs(3)); // several ticks with the check of main held
    assert_eq!(
        main_head(&g),
        before,
        "main's tree must not change under its check"
    );
    assert_eq!(
        queue_states(&g),
        vec![MergeState::Merged, MergeState::Reviewing],
        "approved, but parked"
    );
    assert_eq!(
        reviewers_running(&g),
        0,
        "the verdict was read and the reviewer's slot freed"
    );

    g.release();
    live.wait_until(|_| all_merged(&g, 2));
    let (lines, stderr) = live.stop();
    assert_eq!(
        queue_states(&g),
        vec![MergeState::Merged, MergeState::Merged],
        "{lines:?}\n{stderr}"
    );
    assert_ne!(main_head(&g), before, "merged once the check was over");
}

#[test]
fn nothing_is_merged_in_the_tick_that_finds_main_red() {
    let g = gated(RED_GATE, "[\"main\"]", false);
    review_on_label_risky(&g);
    let a = queued_item(&g.repo, "src/**", "src/a.rs");
    let r = risky_queued_item(&g.repo, "docs/**", "docs/r.md");
    let res = g.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    let res = g.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "reviewing", "{res}");
    // the operator turns the check of main on, and `a`'s check is owed (as after a stopped serve)
    let cfg = g.repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg).unwrap();
    std::fs::write(
        &cfg,
        s.replace("verify_main_after = false", "verify_main_after = true"),
    )
    .unwrap();
    g.store()
        .append_event(&NewEvent {
            kind: "merge.main_check_pending".into(),
            issue_id: Some(a.parse().unwrap()),
            run_id: None,
            actor: Some("orchestrator".into()),
            payload: serde_json::json!({}),
        })
        .unwrap();

    // tick 1 starts the check; the approval arrives before tick 2, which finds main red
    let mut live = LiveServe::start(&g.repo, &LOOP);
    assert_eq!(live.first["result"]["gate_run"]["kind"], "main", "{}", live.first);
    review_verdict(&g, &r, "approve");
    let red = live.next_line();
    let items = red["result"]["merge"]["items"].as_array().unwrap();
    assert!(
        items
            .iter()
            .any(|m| m["issue_id"] == a.as_str() && !m["last_error"].is_null()),
        "the tick that finds main red reports it: {red}"
    );
    assert!(
        !items.iter().any(|m| m["issue_id"] == r.as_str()),
        "and merges nothing on top of it: {red}"
    );
    // The approved item is only parked. Whether this serve goes on to merge it in a later tick or
    // reports `stuck` first depends on timing; a serve started afterwards merges it in any case.
    live.stop();
    let mut again = LiveServe::start(&g.repo, &LOOP);
    again.wait_until(|_| queue_states(&g)[1] == MergeState::Merged);
    again.stop();
}

#[test]
fn only_one_gate_run_is_in_flight_and_the_queue_stays_fifo() {
    let g = gated(HOLD_GATE, "[\"merge\"]", false);
    let a = queued_item(&g.repo, "src/**", "src/a.rs");
    let b = queued_item(&g.repo, "docs/**", "docs/b.md");

    let mut live = LiveServe::start(&g.repo, &LOOP);
    assert_eq!(live.first["result"]["gate_run"]["issue"], a.as_str());
    std::thread::sleep(Duration::from_secs(3)); // several ticks with the first gate held
    assert_eq!(
        g.runs().len(),
        1,
        "no second gate run while one is in flight: {:?}",
        g.runs()
    );
    assert_eq!(queue_states(&g), vec![MergeState::Gating, MergeState::Queued]);

    g.release();
    live.wait_until(|_| all_merged(&g, 2) && merged_events(&g) == 2);
    let (lines, stderr) = live.stop();
    assert_eq!(
        queue_states(&g),
        vec![MergeState::Merged, MergeState::Merged],
        "{lines:?}\n{stderr}"
    );
    let order: Vec<_> = g
        .event_kinds()
        .into_iter()
        .filter(|(k, _)| k == "merge.merged")
        .filter_map(|(_, i)| i)
        .collect();
    assert_eq!(order, vec![a.clone(), b.clone()], "FIFO");
    let runs = g.runs();
    assert!(
        runs.len() == 2 && runs[0].ends_with(&a) && runs[1].ends_with(&b),
        "{runs:?}"
    );
}

#[test]
fn the_result_of_an_item_skipped_while_its_gates_ran_is_dropped() {
    let g = gated(HOLD_GATE, "[\"merge\"]", false);
    let a = queued_item(&g.repo, "src/**", "src/a.rs");
    let mut live = LiveServe::start(&g.repo, &LOOP);
    assert_eq!(live.first["result"]["gate_run"]["issue"], a.as_str());
    // the operator gives up on the item while its gates run
    g.repo.ok(&["merge", "skip", &a]);
    g.release();
    // the tick that takes the finished gate run off the slot is printed, with no gate run in flight
    live.wait_until(|lines| lines.iter().any(|l| l["result"]["gate_run"].is_null()));
    let (lines, stderr) = live.stop();
    assert_eq!(queue_states(&g), vec![MergeState::Skipped], "{lines:?}\n{stderr}");
    assert_eq!(g.issue_status(&a), "validating", "nothing was merged or closed");
    assert!(!g.event_kinds().iter().any(|(k, _)| k == "merge.merged"));
}

/// A gate that fails on its first run and passes afterwards: the log holds one line per run.
const FAIL_ONCE_GATE: &str = "import sys\nlog = sys.argv[1]\ntry:\n    n = len(open(log).read().splitlines())\nexcept OSError:\n    n = 0\nopen(log, 'a').write('run\\n')\nif n == 0:\n    print('error: lint found a problem')\n    sys.exit(7)\n";

#[test]
fn the_loop_queues_a_sent_back_worker_that_committed_its_fix_without_a_finish() {
    let g = gated(FAIL_ONCE_GATE, "[\"merge\"]", false);
    let a = queued_item(&g.repo, "src/**", "src/a.rs");
    // the queue's gate fails after the rebase and the idle worker is sent back
    let res = g.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "failed", "{res}");
    assert!(g
        .event_kinds()
        .iter()
        .any(|(k, i)| k == "run.fix_sent_back" && i.as_deref() == Some(a.as_str())));
    // the worker commits its fix and goes idle; it never runs `klobuk issue finish`
    commit_in(&g.repo.path().join(".worktrees").join(&a), "src/fix.rs", "x\n");
    FakeRuntime::new(g.repo.path().join(".factory/fake-runtime.json"))
        .script(serde_json::json!({ "info": [{ "state": "idle" }] }))
        .unwrap();

    let mut live = LiveServe::start(&g.repo, &LOOP);
    live.wait_until(|_| all_merged(&g, 1) && g.issue_status(&a) == "done");
    live.stop();
    assert_eq!(
        g.runs().len(),
        2,
        "the gate failed once and passed on the second run"
    );
    let kinds = g.event_kinds();
    assert!(
        kinds
            .iter()
            .any(|(k, i)| k == "run.auto_finished" && i.as_deref() == Some(a.as_str())),
        "{kinds:?}"
    );
}

#[test]
fn serve_start_puts_items_left_in_rebasing_or_gating_back_in_the_queue() {
    let g = gated(HOLD_GATE, "[\"merge\"]", false);
    let a = queued_item(&g.repo, "src/**", "src/a.rs");
    let b = queued_item(&g.repo, "docs/**", "docs/b.md");
    g.release(); // the gate passes at once; what matters is the state serve finds
                 // `klobuk serve` was stopped while `a` was gating and `b` was rebasing
    let store = g.store();
    for (item, state) in store
        .merge_queue()
        .unwrap()
        .iter()
        .zip([MergeState::Gating, MergeState::Rebasing])
    {
        store
            .update_merge(
                item.id,
                &klobuk_core::MergePatch {
                    state: Some(state),
                    ..klobuk_core::MergePatch::default()
                },
            )
            .unwrap();
    }
    drop(store);

    let mut live = LiveServe::start(&g.repo, &LOOP);
    live.wait_until(|_| all_merged(&g, 2) && g.issue_status(&a) == "done" && g.issue_status(&b) == "done");
    live.stop();
    assert_eq!(queue_states(&g), vec![MergeState::Merged, MergeState::Merged]);
    assert_eq!(g.issue_status(&a), "done");
    assert_eq!(g.issue_status(&b), "done");
    let recovered: Vec<_> = g
        .event_kinds()
        .into_iter()
        .filter(|(k, _)| k == "merge.recovered")
        .filter_map(|(_, i)| i)
        .collect();
    assert_eq!(recovered, vec![a, b], "one event per recovered item");
}

#[test]
fn serve_start_verifies_again_a_run_a_stopped_serve_left_verifying() {
    let g = gated(HOLD_GATE, "[\"verify\"]", false);
    let a = finished_worker(&g.repo, "src/**", "src/a.rs");
    g.release();
    // the previous serve had set the run `verifying` when it was stopped; the issue is still in-review
    let store = g.store();
    let run = store.runs(Some(&a.parse().unwrap()), true).unwrap().remove(0);
    store
        .update_run(
            &run.id,
            &klobuk_core::RunPatch {
                state: Some(klobuk_core::RunState::Verifying),
                ..klobuk_core::RunPatch::default()
            },
        )
        .unwrap();
    drop(store);

    let mut live = LiveServe::start(&g.repo, &LOOP);
    live.wait_until(|_| g.repo.ok(&["run", "list"])[0]["state"] == "succeeded" && all_merged(&g, 1));
    live.stop();
    let runs = g.repo.ok(&["run", "list"]);
    assert_eq!(runs[0]["state"], "succeeded", "{runs}");
    assert_eq!(queue_states(&g), vec![MergeState::Merged]);
    assert_eq!(g.runs().len(), 1, "the gate ran once, after the restart");
}

#[test]
fn serve_once_waits_for_the_gate_it_started() {
    let g = gated(HOLD_GATE, "[\"merge\"]", false);
    let a = queued_item(&g.repo, "src/**", "src/a.rs");
    let release = g.release.clone();
    let helper = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(2));
        std::fs::write(release, "").unwrap();
    });
    let started = std::time::Instant::now();
    let res = g.repo.ok(&["serve", "--once"]);
    helper.join().unwrap();
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "returned before the gate finished"
    );
    assert_eq!(res["merge"]["items"][0]["state"], "merged", "{res}");
    assert_eq!(
        res["gate_run"],
        serde_json::Value::Null,
        "nothing is left in flight: {res}"
    );
    assert_eq!(g.issue_status(&a), "done");
}

#[test]
fn verification_of_finished_runs_leaves_the_tick_and_shares_the_single_slot() {
    let g = gated(HOLD_GATE, "[\"verify\"]", false);
    let a = finished_worker(&g.repo, "src/**", "src/a.rs");
    let b = finished_worker(&g.repo, "docs/**", "docs/b.md");
    let c = g.repo.create_issue("later", &["--ready", "--scope", "lib/**"]);

    let mut args = LOOP.to_vec();
    args.push("--dispatch");
    let mut live = LiveServe::start(&g.repo, &args);
    let first = &live.first["result"];
    assert_eq!(first["gate_run"]["kind"], "verify", "{first}");
    assert_eq!(first["gate_run"]["issue"], a.as_str(), "{first}");
    assert_eq!(first["started"], serde_json::json!([c]), "{first}");
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(
        g.runs().len(),
        1,
        "the second run waits for the slot: {:?}",
        g.runs()
    );

    g.release();
    live.wait_until(|_| {
        let runs = g.repo.ok(&["run", "list"]);
        let done = |id: &str| {
            runs.as_array()
                .unwrap()
                .iter()
                .any(|r| r["issue_id"] == id && r["state"] == "succeeded")
        };
        done(&a) && done(&b) && all_merged(&g, 2)
    });
    let (lines, stderr) = live.stop();
    let runs = g.repo.ok(&["run", "list"]);
    let state_of = |id: &str| {
        runs.as_array()
            .unwrap()
            .iter()
            .find(|r| r["issue_id"] == id)
            .map(|r| r["state"].clone())
    };
    assert_eq!(
        state_of(&a),
        Some("succeeded".into()),
        "{runs}\n{lines:?}\n{stderr}"
    );
    assert_eq!(state_of(&b), Some("succeeded".into()), "{runs}");
    // no merge-stage gate is configured, so both verified branches go straight through the queue
    assert_eq!(queue_states(&g), vec![MergeState::Merged, MergeState::Merged]);
}

#[test]
fn the_check_of_main_after_a_merge_runs_in_the_background() {
    let g = gated(HOLD_GATE, "[\"main\"]", true);
    let a = queued_item(&g.repo, "src/**", "src/a.rs");

    let mut live = LiveServe::start(&g.repo, &LOOP);
    let first = &live.first["result"];
    assert_eq!(first["merge"]["items"][0]["state"], "merged", "{first}");
    assert_eq!(first["gate_run"]["kind"], "main", "{first}");
    assert_eq!(first["gate_run"]["issue"], a.as_str(), "{first}");
    assert_eq!(first["gate_run"]["gate"], "hold", "{first}");
    assert_eq!(
        g.issue_status(&a),
        "done",
        "the merge is not held back by the check of main"
    );
    assert!(g
        .event_kinds()
        .contains(&("merge.main_check_pending".to_string(), Some(a.clone()))));

    g.release();
    live.wait_until(|_| {
        g.event_kinds()
            .contains(&("merge.main_checked".to_string(), Some(a.clone())))
    });
    let (lines, stderr) = live.stop();
    let kinds = g.event_kinds();
    assert!(
        kinds.contains(&("merge.main_checked".to_string(), Some(a.clone()))),
        "{kinds:?}\n{lines:?}\n{stderr}"
    );
    assert!(!kinds.iter().any(|(k, _)| k == "merge.main_red"), "{kinds:?}");
    assert_eq!(g.runs().len(), 1);
}

#[test]
fn a_check_of_main_left_pending_by_a_stopped_serve_is_run_again_and_a_red_one_is_reported() {
    let g = gated(RED_GATE, "[\"main\"]", false);
    let a = queued_item(&g.repo, "src/**", "src/a.rs");
    // merged by hand while `verify_main_after` is off, then the operator turns it on ...
    let res = g.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "merged", "{res}");
    let cfg = g.repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg).unwrap();
    std::fs::write(
        &cfg,
        s.replace("verify_main_after = false", "verify_main_after = true"),
    )
    .unwrap();
    // ... and a previous serve died between the merge and the end of its check
    g.store()
        .append_event(&NewEvent {
            kind: "merge.main_check_pending".into(),
            issue_id: Some(a.parse().unwrap()),
            run_id: None,
            actor: Some("orchestrator".into()),
            payload: serde_json::json!({}),
        })
        .unwrap();

    let mut live = LiveServe::start(&g.repo, &LOOP);
    live.wait_until(|_| {
        g.event_kinds()
            .contains(&("merge.main_red".to_string(), Some(a.clone())))
            && g.store().merge_queue().unwrap()[0].last_error.is_some()
    });
    live.stop();
    let kinds = g.event_kinds();
    assert!(
        kinds.contains(&("merge.main_red".to_string(), Some(a.clone()))),
        "{kinds:?}"
    );
    let item = &g.store().merge_queue().unwrap()[0];
    assert_eq!(item.state, MergeState::Merged, "no automatic revert");
    assert_eq!(
        item.last_error.as_deref(),
        Some("main red after merge: gate `hold` failed (exit 7)")
    );
    // the check is over: a further serve does not run it again (a few ticks are enough to see it not run)
    let again = g
        .repo
        .klobuk()
        .args(["serve", "--until-done", "--max-ticks", "3", "--interval", "1"])
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(again.status.code(), Some(0), "{again:?}");
    let reds = g
        .event_kinds()
        .into_iter()
        .filter(|(k, _)| k == "merge.main_red")
        .count();
    assert_eq!(reds, 1);
}

// --- the pulse (`.factory/runs/serve.json`) ---

fn pulse_of(repo: &Repo) -> serde_json::Value {
    let raw = std::fs::read_to_string(repo.path().join(".factory/runs/serve.json")).expect("a pulse file");
    serde_json::from_str(&raw).unwrap()
}

fn write_pulse(repo: &Repo, pid: u32, last_tick_at: &str, stopped: Option<&str>) {
    let dir = repo.path().join(".factory/runs");
    std::fs::create_dir_all(&dir).unwrap();
    let pulse = serde_json::json!({
        "pid": pid, "started_at": last_tick_at, "last_tick_at": last_tick_at,
        "mode": "dispatch", "interval_s": 10, "max_ticks": null, "ticks": 4,
        "stopped_at": stopped.map(|_| last_tick_at), "stop_reason": stopped,
    });
    std::fs::write(dir.join("serve.json"), pulse.to_string()).unwrap();
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap()
}

#[test]
fn a_loop_that_stops_for_a_reason_leaves_stopped_at_and_the_reason() {
    // all_done
    let repo = Repo::initialised();
    use_fake_runtime(&repo);
    let a = repo.create_issue("a", &["--scope", "src/a.rs"]);
    repo.ok(&["issue", "status", &a, "done"]);
    repo.ok(&["serve", "--until-done"]);
    let p = pulse_of(&repo);
    assert_eq!(p["stop_reason"], "all_done", "{p}");
    assert!(p["stopped_at"].is_string(), "{p}");
    assert_eq!(p["ticks"], 1, "{p}");
    assert_eq!(p["mode"], "observe", "{p}");
    assert!(p["pid"].as_u64().unwrap() > 0, "{p}");

    // stuck (exit 3 happens after the pulse is written)
    let repo = Repo::initialised();
    use_fake_runtime(&repo);
    repo.create_issue("no scope", &["--ready"]);
    let (_, code) = repo.json(&["serve", "--until-done"]);
    assert_eq!(code, 3);
    assert_eq!(pulse_of(&repo)["stop_reason"], "stuck");

    // max_ticks, with the pulse carrying mode, interval and limit
    let repo = Repo::initialised();
    use_fake_runtime(&repo);
    repo.create_issue("work", &["--ready", "--scope", "src/a.rs"]);
    repo.klobuk()
        .args([
            "serve",
            "--until-done",
            "--dispatch",
            "--max-ticks",
            "2",
            "--interval",
            "1",
        ])
        .output()
        .unwrap();
    let p = pulse_of(&repo);
    assert_eq!(p["stop_reason"], "max_ticks", "{p}");
    assert_eq!(
        (p["ticks"].clone(), p["max_ticks"].clone()),
        (2.into(), 2.into()),
        "{p}"
    );
    assert_eq!(
        (p["mode"].clone(), p["interval_s"].clone()),
        ("dispatch".into(), 1.into()),
        "{p}"
    );
}

#[test]
fn serve_once_leaves_no_pulse_and_does_not_touch_a_running_loops() {
    let repo = Repo::initialised();
    use_fake_runtime(&repo);
    repo.ok(&["serve", "--once"]);
    assert!(!repo.path().join(".factory/runs/serve.json").exists());
    write_pulse(&repo, std::process::id(), &now_rfc3339(), None);
    repo.ok(&["serve", "--once"]);
    assert_eq!(
        pulse_of(&repo)["ticks"],
        4,
        "--once must not overwrite the loop's pulse"
    );
}

#[test]
fn a_second_loop_is_refused_while_a_live_one_has_a_fresh_pulse() {
    let repo = Repo::initialised();
    use_fake_runtime(&repo);
    write_pulse(&repo, std::process::id(), &now_rfc3339(), None); // this test process is alive
    let out = repo.klobuk().args(["serve", "--until-done"]).output().unwrap();
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("another `klobuk serve` loop") && err.contains(&std::process::id().to_string()),
        "{err}"
    );
    assert_eq!(pulse_of(&repo)["ticks"], 4, "the refused loop wrote nothing");
}

#[test]
fn a_stale_or_stopped_pulse_is_taken_over() {
    for (last_tick, stopped) in [
        ("2020-01-01T00:00:00Z", None),
        ("2020-01-01T00:00:00Z", Some("max_ticks")),
    ] {
        let repo = Repo::initialised();
        use_fake_runtime(&repo);
        write_pulse(&repo, std::process::id(), last_tick, stopped);
        let out = repo.klobuk().args(["serve", "--until-done"]).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{stopped:?}: {out:?}");
        let p = pulse_of(&repo);
        assert_eq!(p["stop_reason"], "all_done", "{p}");
        assert_ne!(
            p["started_at"], last_tick,
            "a new pulse replaced the old one: {p}"
        );
    }
}

#[test]
fn the_pulse_records_the_start_window_that_stretches_the_stale_limit() {
    let repo = Repo::initialised();
    use_fake_runtime(&repo);
    let cfg = repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg).unwrap();
    assert!(s.contains("auto_trust = true"), "template changed");
    std::fs::write(
        &cfg,
        s.replace(
            "auto_trust = true",
            "start_timeout_ms = 100500\nauto_trust = true",
        ),
    )
    .unwrap();
    repo.ok(&["serve", "--until-done"]);
    let p = pulse_of(&repo);
    assert_eq!(p["start_timeout_s"], 101, "rounded up from 100500 ms: {p}");
}

#[test]
fn config_sets_the_interval_and_the_tick_limit_and_flags_override_it() {
    let repo = Repo::initialised();
    use_fake_runtime(&repo);
    let cfg = repo.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("interval_s = 10", "interval_s = 1")
        .replace("# max_ticks = 600", "max_ticks = 1");
    assert!(
        s.contains("interval_s = 1 ") && s.contains("\nmax_ticks = 1 "),
        "template changed"
    );
    std::fs::write(&cfg, s).unwrap();
    repo.create_issue("work", &["--ready", "--scope", "src/a.rs"]);
    repo.klobuk()
        .args(["serve", "--until-done", "--dispatch"])
        .output()
        .unwrap();
    let p = pulse_of(&repo);
    assert_eq!(
        (p["max_ticks"].clone(), p["interval_s"].clone()),
        (1.into(), 1.into()),
        "{p}"
    );
    assert_eq!(p["stop_reason"], "max_ticks", "{p}");
    assert_eq!(p["ticks"], 1, "{p}");

    repo.klobuk()
        .args(["serve", "--until-done", "--dispatch", "--max-ticks", "2"])
        .output()
        .unwrap();
    let p = pulse_of(&repo);
    assert_eq!(
        (p["max_ticks"].clone(), p["ticks"].clone()),
        (2.into(), 2.into()),
        "the flag wins: {p}"
    );
}

// ---- API errors and infrastructure stops (asf-158) ----

fn fake_runtime(repo: &Repo) -> FakeRuntime {
    FakeRuntime::new(repo.path().join(".factory/fake-runtime.json"))
}

/// The pane Claude Code leaves after a turn that died on `error`.
fn pane_after_error(error: &str) -> String {
    format!(
        "> Start x: read the brief and follow it.\n\n  ⎿  {error}\n\n\
         ╭────────────────────────────────╮\n│ >                              │\n╰────────────────────────────────╯\n  ? for shortcuts"
    )
}

#[test]
fn a_reviewer_that_died_on_an_api_error_is_started_again_like_a_worker() {
    const ERROR: &str = "API Error: Can't reach the API server — check your internet or DNS (ENOTFOUND)";
    let g = gated(HOLD_GATE, "[\"merge\"]", false);
    review_on_label_risky(&g);
    let r = risky_queued_item(&g.repo, "docs/**", "docs/r.md");
    g.release();
    let res = g.repo.ok(&["merge", "tick", "--once"]);
    assert_eq!(res["items"][0]["state"], "reviewing", "{res}");
    assert_eq!(reviewers_running(&g), 1);
    let rid = g
        .store()
        .list_issues(&klobuk_core::IssueFilter {
            kind: Some(klobuk_core::IssueType::Review),
            parent: Some(r.parse().unwrap()),
            ..klobuk_core::IssueFilter::default()
        })
        .unwrap()
        .last()
        .expect("a reviewer was started")
        .id
        .to_string();
    let prompts = |g: &Gated| -> Vec<String> {
        fake_runtime(&g.repo)
            .log()
            .into_iter()
            .filter(|e| e.op == "prompt" && e.args["name"] == rid.as_str())
            .map(|e| e.args["text"].as_str().unwrap().to_string())
            .collect()
    };
    let kickoff = prompts(&g).remove(0);
    assert!(kickoff.starts_with(&format!("Start {rid}: read")), "{kickoff}");

    // the kick-off prompt died on the error: the pane is idle with no tool call
    fake_runtime(&g.repo)
        .script(serde_json::json!({ "info": [{"state": "idle"}], "screen": pane_after_error(ERROR) }))
        .unwrap();
    g.repo.ok(&["serve", "--once"]);
    let api = |g: &Gated| -> Vec<klobuk_core::Event> {
        g.store()
            .events_since(0, 10_000)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "run.api_error" || e.kind == "run.api_resend")
            .collect()
    };
    assert_eq!(api(&g).len(), 1, "first sight: noted, nothing sent yet");
    assert_eq!(prompts(&g).len(), 1);
    // the wait of a minute is over
    let run = g
        .store()
        .runs(Some(&rid.parse().unwrap()), true)
        .unwrap()
        .remove(0);
    g.store()
        .append_event(&NewEvent {
            kind: "run.api_error".into(),
            issue_id: Some(run.issue_id.clone()),
            run_id: Some(run.id.clone()),
            actor: Some("orchestrator".into()),
            payload: serde_json::json!({
                "error": ERROR, "tool_calls": 0, "resends": 0, "due_at": "2000-01-01T00:00:00Z", "gave_up": false,
            }),
        })
        .unwrap();
    let res = g.repo.ok(&["serve", "--once"]);
    assert!(
        res["watch"]["transitions"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("kickoff"),
        "{res}"
    );
    assert_eq!(
        prompts(&g),
        vec![kickoff.clone(), kickoff],
        "the kick-off line again"
    );
    // the review is still on: no round and no queue change came out of the resend
    assert_eq!(queue_states(&g), vec![MergeState::Reviewing]);
    assert!(g
        .event_kinds()
        .iter()
        .all(|(k, _)| !k.contains("sent_back") && k != "run.fix_sent_back"));
}

/// A gate that its 1 s timeout stops on the first run (no exit code) and that passes afterwards.
const STOPPED_ONCE_GATE: &str = "import sys, time\nlog = sys.argv[1]\ntry:\n    n = len(open(log).read().splitlines())\nexcept OSError:\n    n = 0\nopen(log, 'a').write('run\\n')\nif n == 0:\n    time.sleep(60)\n";

#[test]
fn the_loop_runs_a_verification_again_once_when_a_gate_was_stopped_by_the_infrastructure() {
    let g = gated(STOPPED_ONCE_GATE, "[\"verify\"]", false);
    let cfg = g.repo.path().join(".factory/config.toml");
    let mut s = std::fs::read_to_string(&cfg).unwrap();
    s.push_str("timeout_s = 1\n");
    std::fs::write(&cfg, s).unwrap();
    let a = finished_worker(&g.repo, "src/**", "src/a.rs");
    // a few ticks at most: the rerun is the same verification, picked up by the next call
    for _ in 0..4 {
        g.repo.ok(&["serve", "--once"]);
        if g.repo.ok(&["run", "list"])[0]["state"] == "succeeded" {
            break;
        }
    }
    let runs = g.repo.ok(&["run", "list"]);
    assert_eq!(runs[0]["state"], "succeeded", "{runs}");
    assert_eq!(g.runs().len(), 2, "stopped once, then passed");
    let reruns = g
        .event_kinds()
        .into_iter()
        .filter(|(k, i)| k == "run.verify_rerun" && i.as_deref() == Some(a.as_str()))
        .count();
    assert_eq!(reruns, 1);
    // verified, queued and (by a later tick of the loop) merged: the issue is not blocked
    assert!(
        ["validating", "done"].contains(&g.issue_status(&a).as_str()),
        "{}",
        g.issue_status(&a)
    );
}
