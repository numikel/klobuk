//! `GhCli`: shells out to `gh`. Never interactive (`--json`/explicit flags everywhere).

use std::path::Path;
use std::process::Command;

use asf_core::github::*;
use serde_json::Value;

pub struct GhCli {
    bin: String,
}

impl GhCli {
    pub fn new(bin: impl Into<String>) -> Self {
        GhCli { bin: bin.into() }
    }

    fn run(&self, dir: &Path, args: &[&str]) -> Result<String, GitHubError> {
        if !dir.is_dir() {
            return Err(GitHubError::Io(format!("{} is not a directory", dir.display())));
        }
        let out = Command::new(&self.bin)
            .args(args)
            .current_dir(dir)
            .env("GH_PROMPT_DISABLED", "1")
            .env("NO_COLOR", "1")
            .output()
            .map_err(|e| GitHubError::Io(format!("cannot run {}: {e}", self.bin)))?;
        let stdout = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(GitHubError::Gh(format!(
                "gh {}: {}",
                args.join(" "),
                if stderr.is_empty() { stdout } else { stderr }
            )));
        }
        Ok(stdout)
    }

    fn json(&self, dir: &Path, args: &[&str]) -> Result<Value, GitHubError> {
        let out = self.run(dir, args)?;
        serde_json::from_str(&out)
            .map_err(|e| GitHubError::Protocol(format!("gh {}: not JSON ({e}): {out}", args.join(" "))))
    }
}

impl GitHub for GhCli {
    fn pr_find(&self, dir: &Path, head: &str) -> Result<Option<PrInfo>, GitHubError> {
        match self.json(dir, &["pr", "view", head, "--json", PR_JSON_FIELDS]) {
            Ok(v) => pr_from_json(&v).map(Some),
            Err(GitHubError::Gh(msg)) if msg.to_ascii_lowercase().contains("no pull requests found") => {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    fn pr_create(
        &self,
        dir: &Path,
        base: &str,
        head: &str,
        title: &str,
        body: &str,
    ) -> Result<PrInfo, GitHubError> {
        // `pr create` prints the URL; re-read the PR for the structured view.
        self.run(
            dir,
            &[
                "pr", "create", "--base", base, "--head", head, "--title", title, "--body", body,
            ],
        )?;
        self.pr_find(dir, head)?
            .ok_or_else(|| GitHubError::Protocol(format!("PR for {head} not visible after create")))
    }

    fn pr_merge(
        &self,
        dir: &Path,
        number: u64,
        method: &str,
        delete_branch: bool,
    ) -> Result<(), GitHubError> {
        let n = number.to_string();
        let flag = match method {
            "squash" => "--squash",
            "merge" => "--merge",
            _ => "--rebase",
        };
        let mut args = vec!["pr", "merge", n.as_str(), flag];
        if delete_branch {
            args.push("--delete-branch");
        }
        self.run(dir, &args).map(|_| ())
    }

    fn issue_list(&self, dir: &Path) -> Result<Vec<GhIssue>, GitHubError> {
        let v = self.json(
            dir,
            &[
                "issue",
                "list",
                "--state",
                "all",
                "--limit",
                "500",
                "--json",
                ISSUE_JSON_FIELDS,
            ],
        )?;
        serde_json::from_value(v).map_err(|e| GitHubError::Protocol(format!("issue list: {e}")))
    }

    fn issue_create(&self, dir: &Path, title: &str, body: &str) -> Result<GhIssue, GitHubError> {
        let url = self.run(dir, &["issue", "create", "--title", title, "--body", body])?;
        let url = url.lines().last().unwrap_or_default().trim().to_string();
        let number: u64 = url
            .rsplit('/')
            .next()
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| GitHubError::Protocol(format!("issue create returned `{url}`")))?;
        Ok(GhIssue {
            number,
            title: title.to_string(),
            state: "OPEN".into(),
            url,
            body: body.to_string(),
            author: String::new(),
        })
    }

    fn issue_close(&self, dir: &Path, number: u64, reason: &str) -> Result<(), GitHubError> {
        let n = number.to_string();
        self.run(dir, &["issue", "close", &n, "--reason", reason])
            .map(|_| ())
    }
}
