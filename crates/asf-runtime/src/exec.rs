//! Process execution seam so the Herdr adapter can be tested with canned output.

use std::process::Command;

use asf_core::RuntimeError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

pub trait Exec {
    fn run(&self, program: &str, args: &[String]) -> Result<ExecOutput, RuntimeError>;
}

pub struct SystemExec;

impl Exec for SystemExec {
    fn run(&self, program: &str, args: &[String]) -> Result<ExecOutput, RuntimeError> {
        let out = Command::new(program)
            .args(args)
            .output()
            .map_err(|e| RuntimeError::Io(format!("cannot run {program}: {e}")))?;
        Ok(ExecOutput {
            status: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        })
    }
}
