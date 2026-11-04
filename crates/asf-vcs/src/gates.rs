//! Gate runner: runs configured commands in a directory with a timeout, stopping at the first failure.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use asf_core::{GateResult, GateSpec};

const TAIL_LINES: usize = 40;
/// `link.exe` cannot open a just-written test executable while the AV scanner holds it.
const LINKER_LOCK_MARKER: &str = "LNK1104";
const LINKER_LOCK_ATTEMPTS: u32 = 5;
/// Pause before rerun `n` is `n - 1` times this (2 s, 4 s, 6 s, 8 s): the scanner needs a moment.
const LINKER_LOCK_PAUSE: Duration = Duration::from_secs(2);
/// How long to keep trying to move a locked file aside (1 s between tries).
const LINKER_LOCK_MOVE_TRIES: u32 = 10;

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
        attempts: 1,
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
        attempts: 1,
    }
}

/// Files `link.exe` could not open, taken from the quoted path on each `LNK1104` line.
fn locked_files(output: &str) -> Vec<std::path::PathBuf> {
    output
        .lines()
        .filter(|l| l.contains(LINKER_LOCK_MARKER))
        .filter_map(|l| {
            let start = l.find('"')? + 1;
            let end = l[start..].find('"')? + start;
            Some(std::path::PathBuf::from(&l[start..end]))
        })
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")))
        .collect()
}

/// Moves a locked executable to `<name>.locked.old` so the linker can write a fresh file; the
/// scanner lets go of the old one eventually, so the rename itself is retried for a while.
/// Returns `(moved, still_locked)`.
fn move_locked_aside(output: &str) -> (Vec<String>, Vec<String>) {
    let mut moved = Vec::new();
    let mut stuck = Vec::new();
    for p in locked_files(output) {
        let shown = p.to_string_lossy().replace('\\', "/");
        if !p.exists() {
            continue;
        }
        let mut aside = p.clone().into_os_string();
        aside.push(".locked.old");
        let _ = std::fs::remove_file(&aside);
        let mut done = false;
        for try_no in 1..=LINKER_LOCK_MOVE_TRIES {
            if std::fs::rename(&p, &aside).is_ok() {
                done = true;
                break;
            }
            if try_no < LINKER_LOCK_MOVE_TRIES {
                thread::sleep(Duration::from_secs(1));
            }
        }
        if done {
            moved.push(shown);
        } else {
            stuck.push(shown);
        }
    }
    (moved, stuck)
}

/// Runs one gate, rerunning it (up to [`LINKER_LOCK_ATTEMPTS`] times in total) when the only
/// evidence of failure is a Windows linker lock (`LNK1104`), which is the AV scanner holding a
/// freshly written test executable, not the code. The locked file is moved aside before each rerun.
fn run_gate_with_retry(spec: &GateSpec, cwd: &Path) -> GateResult {
    let started = Instant::now();
    let mut r = run_gate(spec, cwd);
    let mut attempt = 1;
    let mut notes: Vec<String> = Vec::new();
    while !r.ok && attempt < LINKER_LOCK_ATTEMPTS && r.tail.contains(LINKER_LOCK_MARKER) {
        // let the scanner finish before touching anything
        thread::sleep(LINKER_LOCK_PAUSE * attempt);
        let (moved, stuck) = move_locked_aside(&r.tail);
        attempt += 1;
        notes.push(format!(
            "[asf] gate `{}` hit a linker lock ({LINKER_LOCK_MARKER}); moved aside [{}]{}; rerun attempt {attempt}/{LINKER_LOCK_ATTEMPTS}",
            spec.name,
            moved.join(", "),
            if stuck.is_empty() {
                String::new()
            } else {
                format!("; still locked [{}]", stuck.join(", "))
            }
        ));
        r = run_gate(spec, cwd);
    }
    if !notes.is_empty() {
        r.tail = tail(&format!("{}\n{}", r.tail, notes.join("\n")));
        r.duration_ms = started.elapsed().as_millis() as u64;
    }
    r
}

/// Runs gates in order and stops at the first failure. The returned vector holds every gate that ran.
pub fn run_gates(specs: &[GateSpec], cwd: &Path) -> Vec<GateResult> {
    let mut results = Vec::new();
    for spec in specs {
        let r = run_gate_with_retry(spec, cwd);
        let ok = r.ok;
        results.push(r);
        if !ok {
            break;
        }
    }
    results
}
