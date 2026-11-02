//! `AgentRuntime` adapters.
//!
//! - [`HerdrRuntime`]: drives the `herdr` CLI (JSON envelope on stdout, `{"error":{code,message}}` on failure).
//! - [`FakeRuntime`]: a file-backed double so the CLI can be tested end-to-end without Herdr.

pub mod exec;
pub mod fake;
pub mod herdr;
pub mod llm;

use std::path::Path;

use asf_core::{AgentRuntime, RuntimeError};

pub use exec::{Exec, ExecOutput, SystemExec};
pub use fake::FakeRuntime;
pub use herdr::HerdrRuntime;

/// File the fake runtime keeps its state in, relative to the repo root.
pub const FAKE_STATE_FILE: &str = ".factory/fake-runtime.json";

/// Builds the runtime named in `[worker] runtime` (`herdr` | `fake`).
pub fn from_config(name: &str, root: &Path) -> Result<Box<dyn AgentRuntime>, RuntimeError> {
    match name {
        "herdr" => Ok(Box::new(HerdrRuntime::new("herdr"))),
        "fake" => Ok(Box::new(FakeRuntime::new(root.join(FAKE_STATE_FILE)))),
        other => Err(RuntimeError::Protocol(format!(
            "unknown runtime `{other}` (expected herdr or fake)"
        ))),
    }
}
