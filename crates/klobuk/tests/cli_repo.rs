mod common;

use assert_cmd::prelude::*;
use common::Repo;
use predicates::prelude::*;

#[test]
fn init_creates_factory_config_db_and_ignore_lines() {
    let r = Repo::new();
    let res = r.ok(&["repo", "init", "--prefix", "law"]);
    assert_eq!(res["prefix"], "law");
    assert_eq!(res["created"], true);
    assert!(r.path().join(".factory/config.toml").exists());
    assert!(r.path().join(".factory/factory.db").exists());
    let cfg = std::fs::read_to_string(r.path().join(".factory/config.toml")).unwrap();
    assert!(cfg.contains("prefix = \"law\""), "{cfg}");
    assert!(!cfg.starts_with('\u{feff}'));
    assert!(!cfg.contains('\r'));
    for f in [".gitignore", ".git/info/exclude"] {
        let s = std::fs::read_to_string(r.path().join(f)).unwrap();
        for line in [
            ".factory/factory.db",
            ".factory/factory.db-wal",
            ".factory/runs/",
            ".factory/plan/",
            ".worktrees/",
            "CLAUDE.local.md",
        ] {
            assert!(s.lines().any(|l| l == line), "{f} missing {line}:\n{s}");
        }
    }
}

#[test]
fn init_is_idempotent_on_gitignore_and_on_a_second_run() {
    let r = Repo::new();
    std::fs::write(r.path().join(".gitignore"), "target/\n.worktrees/\n").unwrap();
    r.ok(&["repo", "init"]);
    let s = std::fs::read_to_string(r.path().join(".gitignore")).unwrap();
    assert_eq!(s.lines().filter(|l| *l == ".worktrees/").count(), 1);
    assert!(s.starts_with("target/\n"));
    let res = r.ok(&["repo", "init"]);
    assert_eq!(res["created"], false, "{res}");
    assert_eq!(res["prefix"], "kb", "{res}");
}

/// An existing repo (older `init`, no plan line) gets `.factory/plan/` exactly once per file,
/// and a second run leaves both files byte-for-byte unchanged.
#[test]
fn init_adds_the_plan_dir_once_and_a_second_run_changes_nothing() {
    let r = Repo::new();
    std::fs::write(r.path().join(".gitignore"), "target/\n.factory/runs/\n").unwrap();
    r.ok(&["repo", "init"]);
    let read = |f: &str| std::fs::read_to_string(r.path().join(f)).unwrap();
    let (gi, ex) = (read(".gitignore"), read(".git/info/exclude"));
    for (f, s) in [(".gitignore", &gi), (".git/info/exclude", &ex)] {
        assert_eq!(
            s.lines().filter(|l| *l == ".factory/plan/").count(),
            1,
            "{f}:\n{s}"
        );
    }
    r.ok(&["repo", "init"]);
    assert_eq!(read(".gitignore"), gi, ".gitignore changed on a second run");
    assert_eq!(read(".git/info/exclude"), ex, "exclude changed on a second run");
}

/// A clone brings the committed `.factory/config.toml` but no database: init adopts the config.
#[test]
fn init_adopts_a_committed_config_and_creates_the_database() {
    let r = Repo::new();
    r.ok(&["repo", "init", "--prefix", "law"]);
    let cfg_path = r.path().join(".factory/config.toml");
    let before = std::fs::read_to_string(&cfg_path).unwrap();
    for f in ["factory.db", "factory.db-wal", "factory.db-shm"] {
        let _ = std::fs::remove_file(r.path().join(".factory").join(f));
    }
    let (code, ecode, _) = r.err(&["repo", "doctor"]);
    assert_eq!(
        (code, ecode.as_str()),
        (1, "validation"),
        "doctor must flag the missing database"
    );

    let res = r.ok(&["repo", "init"]);
    assert_eq!(res["created"], true, "{res}");
    assert_eq!(
        res["prefix"], "law",
        "prefix comes from the committed config: {res}"
    );
    assert_eq!(
        std::fs::read_to_string(&cfg_path).unwrap(),
        before,
        "config untouched"
    );
    assert_eq!(r.ok(&["issue", "create", "--title", "x"])["id"], "law-1");
    r.ok(&["repo", "doctor"]);

    let (code, ecode, msg) = r.err(&["repo", "init", "--prefix", "other"]);
    assert_eq!((code, ecode.as_str()), (1, "validation"), "{msg}");
    assert!(msg.contains("law"), "{msg}");
}

#[test]
fn init_rejects_bad_prefix_as_usage() {
    let r = Repo::new();
    let (code, ecode, msg) = r.err(&["repo", "init", "--prefix", "Bad"]);
    assert_eq!(code, 1);
    assert_eq!(ecode, "validation");
    assert!(msg.contains("prefix"));
    assert!(
        !r.path().join(".factory").exists(),
        "nothing written on validation failure"
    );
}

#[test]
fn status_json_envelope() {
    let r = Repo::initialised();
    let res = r.ok(&["repo", "status"]);
    assert_eq!(res["prefix"], "asf");
    assert_eq!(res["next_seq"], 1);
    assert_eq!(res["counts"]["backlog"], 0);
    assert_eq!(res["claims"], 0);
    assert_eq!(res["lane"], "orchestrator");
    assert!(res["root"].as_str().unwrap().len() > 3);
}

#[test]
fn status_human_output_is_text() {
    let r = Repo::initialised();
    r.klobuk()
        .args(["repo", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("prefix: asf").and(predicate::str::contains("{\"ok\"").not()));
}

#[test]
fn not_initialised_is_not_found_exit_1() {
    let r = Repo::new();
    let (code, ecode, msg) = r.err(&["repo", "status"]);
    assert_eq!(code, 1);
    assert_eq!(ecode, "not_found");
    assert!(msg.contains("klobuk repo init"), "{msg}");
}

#[test]
fn outside_git_repo_is_not_found() {
    let r = Repo::new();
    let plain = tempfile::tempdir().unwrap();
    let (v, code) = r.json_in(plain.path(), &["repo", "status"]);
    assert_eq!(code, 1);
    assert_eq!(v["error"]["code"], "not_found");
}

#[test]
fn locate_from_worktree_and_repo_flag() {
    let r = Repo::initialised();
    let wt = r.worktree("asf-9");
    let from_wt = r.json_in(&wt, &["repo", "status"]).0;
    let from_root = r.json(&["repo", "status"]).0;
    assert_eq!(from_wt["result"]["root"], from_root["result"]["root"]);
    assert_eq!(
        from_wt["result"]["lane"], "asf-9",
        "lane resolved from worktree cwd"
    );

    let elsewhere = tempfile::tempdir().unwrap();
    let root = r.path().to_str().unwrap().to_string();
    let via_flag = r
        .json_in(elsewhere.path(), &["--repo", &root, "repo", "status"])
        .0;
    assert_eq!(via_flag["result"]["root"], from_root["result"]["root"]);
}

#[test]
fn lane_flag_and_env() {
    let r = Repo::initialised();
    assert_eq!(r.ok(&["--lane", "asf-3", "repo", "status"])["lane"], "asf-3");
    let out = r
        .klobuk()
        .env("KLOBUK_ISSUE", "asf-4")
        .args(["repo", "status", "--json"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["result"]["lane"], "asf-4");
}

#[test]
fn usage_error_exit_2_and_help_exit_0() {
    let r = Repo::initialised();
    r.klobuk().args(["repo", "bogus"]).assert().code(2);
    r.klobuk()
        .args(["--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("klobuk"));
    r.klobuk().args(["repo", "init", "--prefix"]).assert().code(2);
}

/// Repo whose `[merge] target` is `github`, with an `origin` remote when `with_remote`.
fn github_target_repo(with_remote: bool) -> Repo {
    let r = Repo::initialised();
    let cfg = r.path().join(".factory/config.toml");
    let s = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("target = \"local\"", "target = \"github\"");
    std::fs::write(&cfg, s).unwrap();
    if with_remote {
        common::git(
            r.path(),
            &["remote", "add", "origin", "https://example.test/acme/widgets.git"],
        );
    }
    r
}

/// `klobuk repo doctor --json` with `git` standing in for `gh` (`KLOBUK_GH_BIN=git`). With `logged_in`,
/// a git alias makes `git auth status` succeed and print a line like `gh auth status` does;
/// without it `git auth status` fails ("not a git command"), like an unauthenticated `gh`.
fn doctor_with_fake_gh(r: &Repo, logged_in: bool) -> (serde_json::Value, i32) {
    let mut c = r.klobuk();
    c.env("KLOBUK_GH_BIN", "git");
    if logged_in {
        c.env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "alias.auth")
            .env("GIT_CONFIG_VALUE_0", "!echo Logged in to github.com as tester");
    }
    let out = c.args(["repo", "doctor", "--json"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("not json: {e}\n{stdout}"));
    (v, out.status.code().unwrap_or(-1))
}

fn check<'a>(report: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no `{name}` check in {report}"))
}

#[test]
fn doctor_github_target_passes_with_authenticated_gh_and_a_remote() {
    let r = github_target_repo(true);
    let (v, code) = doctor_with_fake_gh(&r, true);
    assert_eq!((code, &v["ok"]), (0, &serde_json::Value::Bool(true)), "{v}");
    let gh = check(&v["result"], "github");
    assert_eq!(gh["ok"], true, "{gh}");
    assert_eq!(gh["required"], true, "{gh}");
    let detail = gh["detail"].as_str().unwrap();
    assert!(detail.contains("Logged in to github.com as tester"), "{detail}");
    assert!(
        detail.contains("https://example.test/acme/widgets.git"),
        "{detail}"
    );
}

#[test]
fn doctor_github_target_fails_when_gh_auth_status_fails() {
    let r = github_target_repo(true);
    let (v, code) = doctor_with_fake_gh(&r, false);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["ok"], false, "{v}");
    assert_eq!(v["error"]["code"], "validation", "{v}");
    let msg = v["error"]["message"].as_str().unwrap();
    assert!(msg.contains("github"), "{msg}");
}

#[test]
fn doctor_github_target_fails_when_the_remote_does_not_resolve() {
    let r = github_target_repo(false);
    let (v, code) = doctor_with_fake_gh(&r, true);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["error"]["code"], "validation", "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("github"), "{v}");
}

#[test]
fn doctor_local_target_skips_the_github_check_and_never_fails() {
    let r = Repo::initialised();
    // gh is not even looked up for the local target
    let out = r
        .klobuk()
        .env("KLOBUK_GH_BIN", "klobuk-no-such-gh-binary")
        .args(["repo", "doctor", "--json"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["result"]["ok"], true, "{v}");
    let gh = check(&v["result"], "github");
    assert_eq!(gh["ok"], false, "{gh}");
    assert_eq!(gh["required"], false, "{gh}");
    assert!(gh["detail"].as_str().unwrap().contains("local"), "{gh}");
}

/// `repo doctor --json` result for a local-target repo; asserts the exit code is 0 (never `fail`).
fn doctor_gitignore(r: &Repo) -> serde_json::Value {
    let out = r.klobuk().args(["repo", "doctor", "--json"]).output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["result"]["ok"], true, "{v}");
    check(&v["result"], "gitignore").clone()
}

#[test]
fn doctor_gitignore_is_ok_after_init() {
    let r = Repo::initialised();
    let c = doctor_gitignore(&r);
    assert_eq!(c["ok"], true, "{c}");
    assert_eq!(c["required"], false, "{c}");
}

#[test]
fn doctor_gitignore_accepts_lines_split_across_gitignore_and_exclude() {
    let r = Repo::initialised();
    let drop_plan = |f: &str| {
        let p = r.path().join(f);
        let s = std::fs::read_to_string(&p).unwrap();
        std::fs::write(&p, s.replace(".factory/plan/\n", "")).unwrap();
    };
    // gone from .gitignore only: still ignored through .git/info/exclude
    drop_plan(".gitignore");
    assert_eq!(doctor_gitignore(&r)["ok"], true);
    // gone from both: warn, never fail
    drop_plan(".git/info/exclude");
    let c = doctor_gitignore(&r);
    assert_eq!(c["ok"], false, "{c}");
    assert_eq!(c["required"], false, "{c}");
    let d = c["detail"].as_str().unwrap();
    assert!(d.contains(".factory/plan/"), "{d}");
    assert!(d.contains("run `klobuk repo init` to add them"), "{d}");
    assert!(!d.contains(".worktrees/"), "only missing lines are listed: {d}");
    // init repairs it
    r.ok(&["repo", "init"]);
    assert_eq!(doctor_gitignore(&r)["ok"], true);
}

#[test]
fn doctor_gitignore_warns_when_neither_file_exists() {
    let r = Repo::initialised();
    std::fs::remove_file(r.path().join(".gitignore")).unwrap();
    std::fs::remove_file(r.path().join(".git/info/exclude")).unwrap();
    let c = doctor_gitignore(&r);
    assert_eq!(c["ok"], false, "{c}");
    assert_eq!(c["required"], false, "{c}");
    let d = c["detail"].as_str().unwrap();
    assert!(d.contains("neither"), "{d}");
    assert!(d.contains(".factory/plan/") && d.contains(".worktrees/"), "{d}");
}

fn write_pulse(r: &Repo, pid: u32, last_tick_at: &str, stop_reason: Option<&str>) {
    let dir = r.path().join(".factory/runs");
    std::fs::create_dir_all(&dir).unwrap();
    let pulse = serde_json::json!({
        "pid": pid, "started_at": last_tick_at, "last_tick_at": last_tick_at,
        "mode": "dispatch", "interval_s": 10, "max_ticks": 633, "ticks": 633,
        "stopped_at": stop_reason.map(|_| last_tick_at), "stop_reason": stop_reason,
    });
    std::fs::write(dir.join("serve.json"), pulse.to_string()).unwrap();
}

fn doctor_loop(r: &Repo) -> serde_json::Value {
    let out = r.klobuk().args(["repo", "doctor", "--json"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "the loop never fails doctor");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    check(&v["result"], "loop").clone()
}

#[test]
fn doctor_reports_a_loop_that_never_ran() {
    let r = Repo::initialised();
    let c = doctor_loop(&r);
    assert_eq!(
        (c["ok"].clone(), c["required"].clone()),
        (false.into(), false.into()),
        "{c}"
    );
    assert_eq!(
        c["detail"], "the loop is not running — start it: klobuk serve --dispatch",
        "{c}"
    );
}

#[test]
fn doctor_reports_a_live_loop_as_ok_and_names_its_pid() {
    let r = Repo::initialised();
    let now = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    write_pulse(&r, std::process::id(), &now, None); // this test process is alive
    let c = doctor_loop(&r);
    assert_eq!(c["ok"], true, "{c}");
    let d = c["detail"].as_str().unwrap();
    assert!(
        d.contains("running (dispatch)") && d.contains(&format!("pid {}", std::process::id())),
        "{d}"
    );
}

#[test]
fn doctor_reports_a_stopped_loop_with_the_reason_and_the_same_words_as_the_tui() {
    let r = Repo::initialised();
    write_pulse(&r, 1, "2020-01-01T00:00:00Z", Some("max_ticks"));
    let c = doctor_loop(&r);
    assert_eq!(c["ok"], false, "{c}");
    let d = c["detail"].as_str().unwrap();
    assert!(
        d.starts_with("the loop stopped ")
            && d.ends_with(" ago: max_ticks (633) — start it again: klobuk serve --dispatch"),
        "{d}"
    );
    // and in the human output
    let out = r.klobuk().args(["repo", "doctor"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("warn loop") && text.contains("max_ticks (633)"),
        "{text}"
    );
}

#[test]
fn doctor_treats_a_stale_pulse_without_a_stop_as_stopped_without_a_reason() {
    let r = Repo::initialised();
    write_pulse(&r, std::process::id(), "2020-01-01T00:00:00Z", None);
    let c = doctor_loop(&r);
    assert_eq!(c["ok"], false, "{c}");
    assert!(c["detail"].as_str().unwrap().contains("without a reason"), "{c}");
}

#[test]
fn json_usage_error_is_still_an_envelope() {
    let r = Repo::initialised();
    let out = r.klobuk().args(["--json", "repo", "bogus"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "usage");
}

/// A new repository numbers its issues `kb-N`; a repository whose committed config says `asf`
/// keeps that prefix (nothing is migrated).
#[test]
fn a_new_repository_defaults_to_kb_and_a_config_that_says_asf_keeps_asf() {
    let fresh = Repo::new();
    assert_eq!(fresh.ok(&["repo", "init"])["prefix"], "kb");
    assert_eq!(fresh.ok(&["issue", "create", "--title", "x"])["id"], "kb-1");

    let old = Repo::new();
    old.ok(&["repo", "init", "--prefix", "asf"]);
    for f in ["factory.db", "factory.db-wal", "factory.db-shm"] {
        let _ = std::fs::remove_file(old.path().join(".factory").join(f));
    }
    let res = old.ok(&["repo", "init"]);
    assert_eq!(res["prefix"], "asf", "prefix comes from the committed config: {res}");
    assert_eq!(old.ok(&["issue", "create", "--title", "x"])["id"], "asf-1");
    old.ok(&["repo", "doctor"]);
}
