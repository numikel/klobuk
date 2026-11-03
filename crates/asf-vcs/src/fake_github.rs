//! File-backed fake GitHub so the merge queue and `asf gh sync` can be tested end-to-end.
//!
//! PRs and issues live in a JSON file. `pr_merge` really fast-forwards the remote base branch
//! (`git push <remote> <head>:<base>` from `dir`), so a following `pull_ff` observes the merge
//! exactly as it would after GitHub merged the PR. A `script` seeds what `pr_find` reports:
//! `{"checks": "none|pending|passed|failed", "mergeable": "mergeable|conflicting|unknown",
//! "remote": "origin"}`.

use std::path::{Path, PathBuf};
use std::process::Command;

use asf_core::github::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LogEntry {
    pub op: String,
    pub args: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pr {
    number: u64,
    base: String,
    head: String,
    title: String,
    body: String,
    state: PrState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Script {
    checks: ChecksState,
    mergeable: Mergeable,
    remote: String,
}

impl Default for Script {
    fn default() -> Self {
        Script {
            checks: ChecksState::None,
            mergeable: Mergeable::Mergeable,
            remote: "origin".into(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct State {
    script: Script,
    prs: Vec<Pr>,
    issues: Vec<GhIssue>,
    log: Vec<LogEntry>,
}

pub struct FakeGitHub {
    path: PathBuf,
}

fn io(e: impl std::fmt::Display) -> GitHubError {
    GitHubError::Io(e.to_string())
}

impl FakeGitHub {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        FakeGitHub { path: path.into() }
    }

    fn load(&self) -> Result<State, GitHubError> {
        if !self.path.exists() {
            return Ok(State::default());
        }
        let raw = std::fs::read_to_string(&self.path).map_err(io)?;
        serde_json::from_str(&raw).map_err(io)
    }

    fn save(&self, st: &State) -> Result<(), GitHubError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        std::fs::write(&self.path, serde_json::to_string_pretty(st).map_err(io)?).map_err(io)
    }

    fn with<T>(
        &self,
        op: &str,
        args: Value,
        f: impl FnOnce(&mut State) -> Result<T, GitHubError>,
    ) -> Result<T, GitHubError> {
        let mut st = self.load()?;
        st.log.push(LogEntry {
            op: op.to_string(),
            args,
        });
        let out = f(&mut st);
        self.save(&st)?;
        out
    }

    /// Seeds what `pr_find` reports (merged over the current script).
    pub fn script(&self, v: Value) -> Result<(), GitHubError> {
        let mut st = self.load()?;
        if let Some(c) = v.get("checks") {
            st.script.checks = serde_json::from_value(c.clone()).map_err(io)?;
        }
        if let Some(m) = v.get("mergeable") {
            st.script.mergeable = serde_json::from_value(m.clone()).map_err(io)?;
        }
        if let Some(r) = v.get("remote").and_then(Value::as_str) {
            st.script.remote = r.to_string();
        }
        self.save(&st)
    }

    pub fn log(&self) -> Vec<LogEntry> {
        self.load().map(|s| s.log).unwrap_or_default()
    }

    fn view(st: &State, pr: &Pr) -> PrInfo {
        PrInfo {
            number: pr.number,
            url: format!("https://github.example/fake/repo/pull/{}", pr.number),
            state: pr.state,
            mergeable: if pr.state == PrState::Open {
                st.script.mergeable
            } else {
                Mergeable::Unknown
            },
            checks: st.script.checks,
            head_sha: None,
        }
    }
}

impl GitHub for FakeGitHub {
    fn pr_find(&self, _dir: &Path, head: &str) -> Result<Option<PrInfo>, GitHubError> {
        self.with("pr_find", json!({ "head": head }), |st| {
            Ok(st
                .prs
                .iter()
                .rev()
                .find(|p| p.head == head)
                .map(|p| Self::view(st, p)))
        })
    }

    fn pr_create(
        &self,
        _dir: &Path,
        base: &str,
        head: &str,
        title: &str,
        body: &str,
    ) -> Result<PrInfo, GitHubError> {
        self.with(
            "pr_create",
            json!({ "base": base, "head": head, "title": title }),
            |st| {
                if st.prs.iter().any(|p| p.head == head && p.state == PrState::Open) {
                    return Err(GitHubError::Gh(format!(
                        "a pull request for branch \"{head}\" into branch \"{base}\" already exists"
                    )));
                }
                let pr = Pr {
                    number: st.prs.len() as u64 + 1,
                    base: base.into(),
                    head: head.into(),
                    title: title.into(),
                    body: body.into(),
                    state: PrState::Open,
                };
                let info = Self::view(st, &pr);
                st.prs.push(pr);
                Ok(info)
            },
        )
    }

    fn pr_merge(
        &self,
        dir: &Path,
        number: u64,
        method: &str,
        delete_branch: bool,
    ) -> Result<(), GitHubError> {
        self.with(
            "pr_merge",
            json!({ "number": number, "method": method, "delete_branch": delete_branch }),
            |st| {
                let remote = st.script.remote.clone();
                let pr = st
                    .prs
                    .iter_mut()
                    .find(|p| p.number == number)
                    .ok_or_else(|| GitHubError::Gh(format!("no pull request #{number}")))?;
                if pr.state != PrState::Open {
                    return Err(GitHubError::Gh(format!("pull request #{number} is not open")));
                }
                // GitHub would advance the base branch; do it on the remote for real.
                let refspec = format!("{}:{}", pr.head, pr.base);
                let out = Command::new("git")
                    .args(["push", &remote, &refspec])
                    .current_dir(dir)
                    .output()
                    .map_err(io)?;
                if !out.status.success() {
                    return Err(GitHubError::Gh(format!(
                        "fake merge: git push {remote} {refspec}: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    )));
                }
                pr.state = PrState::Merged;
                Ok(())
            },
        )
    }

    fn issue_list(&self, _dir: &Path) -> Result<Vec<GhIssue>, GitHubError> {
        self.with("issue_list", json!({}), |st| Ok(st.issues.clone()))
    }

    fn issue_create(&self, _dir: &Path, title: &str, body: &str) -> Result<GhIssue, GitHubError> {
        self.with("issue_create", json!({ "title": title }), |st| {
            let number = st.issues.len() as u64 + 1;
            let issue = GhIssue {
                number,
                title: title.into(),
                state: "OPEN".into(),
                url: format!("https://github.example/fake/repo/issues/{number}"),
                body: body.into(),
            };
            st.issues.push(issue.clone());
            Ok(issue)
        })
    }

    fn issue_close(&self, _dir: &Path, number: u64, reason: &str) -> Result<(), GitHubError> {
        self.with(
            "issue_close",
            json!({ "number": number, "reason": reason }),
            |st| {
                let issue = st
                    .issues
                    .iter_mut()
                    .find(|i| i.number == number)
                    .ok_or_else(|| GitHubError::Gh(format!("no issue #{number}")))?;
                issue.state = "CLOSED".into();
                Ok(())
            },
        )
    }
}
