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
    assert_eq!(res["prefix"], "asf", "{res}");
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
    r.asf()
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
    assert!(msg.contains("asf repo init"), "{msg}");
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
        .asf()
        .env("ASF_ISSUE", "asf-4")
        .args(["repo", "status", "--json"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["result"]["lane"], "asf-4");
}

#[test]
fn usage_error_exit_2_and_help_exit_0() {
    let r = Repo::initialised();
    r.asf().args(["repo", "bogus"]).assert().code(2);
    r.asf()
        .args(["--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("asf"));
    r.asf().args(["repo", "init", "--prefix"]).assert().code(2);
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

/// `asf repo doctor --json` with `git` standing in for `gh` (`ASF_GH_BIN=git`). With `logged_in`,
/// a git alias makes `git auth status` succeed and print a line like `gh auth status` does;
/// without it `git auth status` fails ("not a git command"), like an unauthenticated `gh`.
fn doctor_with_fake_gh(r: &Repo, logged_in: bool) -> (serde_json::Value, i32) {
    let mut c = r.asf();
    c.env("ASF_GH_BIN", "git");
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
        .asf()
        .env("ASF_GH_BIN", "asf-no-such-gh-binary")
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

#[test]
fn json_usage_error_is_still_an_envelope() {
    let r = Repo::initialised();
    let out = r.asf().args(["--json", "repo", "bogus"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "usage");
}
