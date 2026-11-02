//! Gate runner: runs configured commands in a directory with a timeout, stopping at the first failure.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use asf_core::{GateResult, GateSpec};

const TAIL_LINES: usize = 40;

fn tail(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(TAIL_LINES);
    lines[start..].join("\n")
}

fn build(spec: &GateSpec) -> Option<Command> {
    match spec.shell.as_deref() {
        Some("pwsh") | Some("powershell") => {
            let mut c = Command::new("powershell");
            c.args(["-NoProfile", "-NonInteractive", "-Command", &spec.cmd.join(" ")]);
            Some(c)
        }
        Some("sh") | Some("bash") => {
            let mut c = Command::new("sh");
            c.args(["-c", &spec.cmd.join(" ")]);
            Some(c)
        }
        _ => {
            let (prog, rest) = spec.cmd.split_first()?;
            let mut c = Command::new(prog);
            c.args(rest);
            Some(c)
        }
    }
}

/// Runs one gate; never panics, never blocks longer than `timeout_s` (plus a short grace period).
pub fn run_gate(spec: &GateSpec, cwd: &Path) -> GateResult {
    let started = Instant::now();
    let fail = |tail: String| GateResult {
        name: spec.name.clone(),
        ok: false,
        exit_code: None,
        duration_ms: started.elapsed().as_millis() as u64,
        tail,
    };
    let Some(mut cmd) = build(spec) else {
        return fail("gate has an empty cmd".into());
    };
    cmd.current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return fail(format!("cannot start `{}`: {e}", spec.cmd.join(" "))),
    };
    let mut out = child.stdout.take().expect("piped");
    let mut err = child.stderr.take().expect("piped");
    let t_out = thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let t_err = thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    let timeout = Duration::from_secs(spec.timeout_s.max(1));
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                timed_out = true;
                break None;
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(_) => break None,
        }
    };
    let stdout = t_out.join().unwrap_or_default();
    let stderr = t_err.join().unwrap_or_default();
    let mut combined = stdout;
    if !stderr.trim().is_empty() {
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(&stderr);
    }
    if timed_out {
        combined.push_str(&format!(
            "\n[asf] gate `{}` killed: timeout after {}s",
            spec.name, spec.timeout_s
        ));
    }
    let exit_code = status.and_then(|s| s.code());
    GateResult {
        name: spec.name.clone(),
        ok: !timed_out && exit_code == Some(0),
        exit_code,
        duration_ms: started.elapsed().as_millis() as u64,
        tail: tail(&combined),
    }
}

/// Runs gates in order and stops at the first failure. The returned vector holds every gate that ran.
pub fn run_gates(specs: &[GateSpec], cwd: &Path) -> Vec<GateResult> {
    let mut results = Vec::new();
    for spec in specs {
        let r = run_gate(spec, cwd);
        let ok = r.ok;
        results.push(r);
        if !ok {
            break;
        }
    }
    results
}
