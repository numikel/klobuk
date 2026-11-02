//! Git subprocess implementation of `asf_core::Vcs` and the gate runner.

pub mod gates;
pub mod git;

pub use gates::run_gates;
pub use git::GitCli;
