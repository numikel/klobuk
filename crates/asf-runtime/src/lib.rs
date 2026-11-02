//! `AgentRuntime` adapters.
//!
//! - [`HerdrRuntime`]: drives the `herdr` CLI (JSON envelope on stdout, `{"error":{code,message}}` on failure).
//! - [`FakeRuntime`]: a file-backed double so the CLI can be tested end-to-end without Herdr.

pub mod exec;
pub mod fake;
pub mod herdr;

pub use exec::{Exec, ExecOutput, SystemExec};
pub use fake::FakeRuntime;
pub use herdr::HerdrRuntime;
