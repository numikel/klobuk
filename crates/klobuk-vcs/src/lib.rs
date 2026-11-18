//! Git subprocess implementation of `klobuk_core::Vcs` and the gate runner.

pub mod fake_github;
pub mod gates;
pub mod gh;
pub mod git;

use std::path::Path;

use klobuk_core::github::{GitHub, GitHubError};

pub use fake_github::FakeGitHub;
pub use gates::run_gates;
pub use gh::GhCli;
pub use git::GitCli;

/// File the fake GitHub keeps its state in, relative to the repo root.
pub const FAKE_GITHUB_FILE: &str = ".factory/fake-github.json";

/// Builds the GitHub client named in `[merge.github] runner` (`gh` | `fake`).
pub fn github_from_config(runner: &str, root: &Path) -> Result<Box<dyn GitHub>, GitHubError> {
    match runner {
        "gh" => Ok(Box::new(GhCli::new("gh"))),
        "fake" => Ok(Box::new(FakeGitHub::new(root.join(FAKE_GITHUB_FILE)))),
        other => Err(GitHubError::Protocol(format!(
            "unknown github runner `{other}` (expected gh or fake)"
        ))),
    }
}
