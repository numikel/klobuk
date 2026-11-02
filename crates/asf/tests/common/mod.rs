#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

/// A temporary git repository (no spaces in path) with `asf` available.
pub struct Repo {
    pub dir: TempDir,
}

impl Repo {
    pub fn new() -> Repo {
        let dir = tempfile::Builder::new().prefix("asf-test-").tempdir().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"]);
        git(dir.path(), &["config", "user.email", "t@example.com"]);
        git(dir.path(), &["config", "user.name", "t"]);
        git(dir.path(), &["config", "core.autocrlf", "false"]);
        std::fs::write(dir.path().join("README.txt"), "hi\n").unwrap();
        git(dir.path(), &["add", "README.txt"]);
        git(dir.path(), &["commit", "-q", "-m", "init"]);
        Repo { dir }
    }

    /// Initialised with `asf repo init`.
    pub fn initialised() -> Repo {
        let r = Repo::new();
        r.asf().args(["repo", "init"]).assert().success();
        r
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn asf(&self) -> Command {
        self.asf_in(self.path())
    }

    pub fn asf_in(&self, cwd: &Path) -> Command {
        let mut c = Command::cargo_bin("asf").unwrap();
        c.current_dir(cwd);
        c.env_remove("ASF_ISSUE");
        c.env_remove("ASF_RUN");
        c
    }

    /// Runs `asf <args> --json` and returns the parsed envelope and exit code.
    pub fn json(&self, args: &[&str]) -> (Value, i32) {
        self.json_in(self.path(), args)
    }

    pub fn json_in(&self, cwd: &Path, args: &[&str]) -> (Value, i32) {
        let out = self.asf_in(cwd).args(args).arg("--json").output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let v: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
            panic!(
                "not json: {e}\nstdout: {stdout}\nstderr: {}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
        (v, out.status.code().unwrap_or(-1))
    }

    /// Like `json` but asserts `ok:true` and returns `result`.
    pub fn ok(&self, args: &[&str]) -> Value {
        let (v, code) = self.json(args);
        assert_eq!(code, 0, "{v}");
        assert_eq!(v["ok"], Value::Bool(true), "{v}");
        v["result"].clone()
    }

    /// Like `json` but asserts failure and returns `(code, error.code, error.message)`.
    pub fn err(&self, args: &[&str]) -> (i32, String, String) {
        let (v, code) = self.json(args);
        assert_eq!(v["ok"], Value::Bool(false), "{v}");
        (
            code,
            v["error"]["code"].as_str().unwrap().to_string(),
            v["error"]["message"].as_str().unwrap().to_string(),
        )
    }

    pub fn create_issue(&self, title: &str, extra: &[&str]) -> String {
        let mut args = vec!["issue", "create", "--title", title];
        args.extend_from_slice(extra);
        self.ok(&args)["id"].as_str().unwrap().to_string()
    }

    pub fn worktree(&self, id: &str) -> PathBuf {
        let wt = self.path().join(".worktrees").join(id);
        git(
            self.path(),
            &[
                "worktree",
                "add",
                "-q",
                wt.to_str().unwrap(),
                "-b",
                &format!("feat/{id}"),
            ],
        );
        wt
    }
}

pub fn git(cwd: &Path, args: &[&str]) {
    let st = Command::new("git").args(args).current_dir(cwd).status().unwrap();
    assert!(st.success(), "git {args:?} failed");
}
