//! Gate runner: runs configured commands in a directory with a timeout, stopping at the first failure.

use std::collections::BTreeSet;
use std::io::{ErrorKind, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use asf_core::config::{GateCfg, GateWhen};
use asf_core::{GateResult, GateSpec};

const TAIL_LINES: usize = 40;
/// How long to wait for the output pipes to close once the gate has exited or been killed. A
/// descendant of the gate (`sh -c`, `cargo test`, ...) can inherit the pipes and keep them open long
/// after the gate itself is gone; whatever was read by then is the output.
const DRAIN_GRACE: Duration = Duration::from_secs(2);
/// `link.exe` cannot open a just-written test executable while the AV scanner holds it.
const LINKER_LOCK_MARKER: &str = "LNK1104";
/// How many attempts may fail on a linker lock without making progress before the gate gives up.
const LINKER_LOCK_ATTEMPTS: u32 = 5;
/// Hard cap on reruns after a linker lock, progress or not, so a gate can never loop forever (each
/// attempt is still bounded by the gate's own `timeout_s`).
const LINKER_LOCK_MAX_RERUNS: u32 = 12;
/// Pauses before reruns 1..=4 when a locked file exists and is moved aside (2 s, 4 s, 6 s, 8 s):
/// the scanner needs a moment before the file can be touched.
const LINKER_LOCK_PAUSES_MOVED: [Duration; 4] = [
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(6),
    Duration::from_secs(8),
];
/// Pauses before reruns 1..=4 when the file named by the linker does not exist (or no path could be
/// read): the scanner holds the name of a just-deleted executable, nothing can be moved, so only
/// time helps.
const LINKER_LOCK_PAUSES_ABSENT: [Duration; 4] = [
    Duration::from_secs(15),
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(60),
];
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

/// Reads one output pipe on its own thread into a shared buffer, so the bytes read so far stay
/// available when the pipe never reaches EOF.
struct Drain {
    buf: Arc<Mutex<Vec<u8>>>,
    eof: Receiver<()>,
}

impl Drain {
    fn start<R: Read + Send + 'static>(mut pipe: R) -> Self {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let (done, eof) = mpsc::channel();
        let sink = Arc::clone(&buf);
        thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => sink
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            let _ = done.send(());
        });
        Self { buf, eof }
    }

    /// Waits for EOF until `deadline` and returns what was read (lossy UTF-8). When the pipe is still
    /// open at the deadline its thread is left to finish on its own once the last holder lets go.
    fn collect(self, deadline: Instant) -> String {
        let _ = self
            .eof
            .recv_timeout(deadline.saturating_duration_since(Instant::now()));
        let bytes = self.buf.lock().unwrap_or_else(PoisonError::into_inner);
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// Runs one gate; never panics. After the gate exits or is killed it waits at most [`DRAIN_GRACE`]
/// for its output, so a descendant that holds the pipes open cannot delay the result. The process
/// start itself is not bounded: the timeout clock runs from before it, so a slow start only means
/// the timeout is already due when the gate finally runs.
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
    let t_out = Drain::start(child.stdout.take().expect("piped"));
    let t_err = Drain::start(child.stderr.take().expect("piped"));
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
    let drain_until = Instant::now() + DRAIN_GRACE;
    let stdout = t_out.collect(drain_until);
    let stderr = t_err.collect(drain_until);
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

/// How long to wait before each rerun after a linker lock; index `n - 1` is the pause before rerun
/// `n`, the last entry repeats if a schedule is shorter than the number of reruns.
#[derive(Debug, Clone, Copy)]
pub struct LinkerLockPauses<'a> {
    /// Used when a locked file exists and is moved aside.
    pub moved: &'a [Duration],
    /// Used when the file named by the linker does not exist (or no path could be read).
    pub absent: &'a [Duration],
}

impl LinkerLockPauses<'static> {
    /// The real schedules that [`run_gates`] uses.
    pub const DEFAULT: Self = Self {
        moved: &LINKER_LOCK_PAUSES_MOVED,
        absent: &LINKER_LOCK_PAUSES_ABSENT,
    };
}

fn pause_before_rerun(schedule: &[Duration], rerun: u32) -> Duration {
    let i = (rerun as usize).saturating_sub(1);
    schedule
        .get(i)
        .or_else(|| schedule.last())
        .copied()
        .unwrap_or_default()
}

/// The locked files as a comparable set (separators and case normalised, as Windows paths are).
fn locked_set(files: &[std::path::PathBuf]) -> BTreeSet<String> {
    files
        .iter()
        .map(|p| p.to_string_lossy().replace('\\', "/").to_lowercase())
        .collect()
}

/// Runs one gate, rerunning it when the only evidence of failure is a Windows linker lock
/// (`LNK1104`), which is the AV scanner holding a freshly written test executable, not the code. A
/// locked file that exists is moved aside before each rerun after a short pause; when the linker
/// names no existing file there is nothing to move and the rerun waits out the longer `absent`
/// schedule instead.
///
/// A rerun counts against [`LINKER_LOCK_ATTEMPTS`] unless the attempt made progress: after a rebase
/// the whole workspace relinks and every attempt trips over different files, so a rerun whose set
/// of locked files differs from the previous attempt's is free. Reruns are capped at
/// [`LINKER_LOCK_MAX_RERUNS`] in any case.
fn run_gate_with_retry(spec: &GateSpec, cwd: &Path, pauses: &LinkerLockPauses) -> GateResult {
    let started = Instant::now();
    let mut r = run_gate(spec, cwd);
    // attempts that count against the limit, the first run included
    let mut counted = 1;
    let mut reruns = 0;
    let mut previous: Option<BTreeSet<String>> = None;
    let mut notes: Vec<String> = Vec::new();
    while !r.ok
        && counted < LINKER_LOCK_ATTEMPTS
        && reruns < LINKER_LOCK_MAX_RERUNS
        && r.tail.contains(LINKER_LOCK_MARKER)
    {
        let named = locked_files(&r.tail);
        let now = locked_set(&named);
        // Progress needs names to compare: a message without any path says nothing about it.
        let progress = !now.is_empty() && previous.as_ref().is_some_and(|p| *p != now);
        let exists = named.iter().any(|p| p.exists());
        reruns += 1;
        // let the scanner finish before touching anything
        let pause = pause_before_rerun(if exists { pauses.moved } else { pauses.absent }, reruns);
        thread::sleep(pause);
        let (moved, stuck) = move_locked_aside(&r.tail);
        let verdict = if progress {
            format!(
                "linker lock on {} other file(s); progress, rerun {reruns} (does not count)",
                now.len()
            )
        } else {
            counted += 1;
            let why = match &previous {
                None => "first linker lock".to_string(),
                Some(_) if now.is_empty() => "no file names to compare, no progress".to_string(),
                Some(_) => format!("same {} file(s) as the previous attempt, no progress", now.len()),
            };
            format!("{why}; rerun attempt {counted}/{LINKER_LOCK_ATTEMPTS} (counts)")
        };
        previous = Some(now);
        let what = if moved.is_empty() && stuck.is_empty() {
            let why = if named.is_empty() {
                "no file path in the linker message"
            } else {
                "the file does not exist yet"
            };
            format!("nothing to move aside ({why}); waiting {} s", pause.as_secs())
        } else {
            format!(
                "moved aside [{}]{}",
                moved.join(", "),
                if stuck.is_empty() {
                    String::new()
                } else {
                    format!("; still locked [{}]", stuck.join(", "))
                }
            )
        };
        notes.push(format!(
            "[asf] gate `{}` hit a linker lock ({LINKER_LOCK_MARKER}); {what}; {verdict}",
            spec.name,
        ));
        r = run_gate(spec, cwd);
    }
    if !r.ok && !notes.is_empty() && r.tail.contains(LINKER_LOCK_MARKER) {
        let reason = if reruns >= LINKER_LOCK_MAX_RERUNS {
            format!("the cap of {LINKER_LOCK_MAX_RERUNS} reruns is used up")
        } else {
            format!("{LINKER_LOCK_ATTEMPTS} attempts failed on a linker lock without progress")
        };
        notes.push(format!("[asf] gate `{}` gives up: {reason}", spec.name));
    }
    if !notes.is_empty() {
        r.tail = tail(&format!("{}\n{}", r.tail, notes.join("\n")));
        r.duration_ms = started.elapsed().as_millis() as u64;
    }
    r
}

/// Runs gates in order and stops at the first failure. The returned vector holds every gate that ran.
pub fn run_gates(specs: &[GateSpec], cwd: &Path) -> Vec<GateResult> {
    run_gates_with_pauses(specs, cwd, &LinkerLockPauses::DEFAULT)
}

/// [`run_gates`] with the linker-lock pause schedules injected (tests use millisecond pauses).
pub fn run_gates_with_pauses(specs: &[GateSpec], cwd: &Path, pauses: &LinkerLockPauses) -> Vec<GateResult> {
    let mut results = Vec::new();
    for spec in specs {
        let r = run_gate_with_retry(spec, cwd, pauses);
        let ok = r.ok;
        results.push(r);
        if !ok {
            break;
        }
    }
    results
}

/// The gates that run in `stage`, in config order, as runnable specs.
pub fn stage_specs(gates: &[GateCfg], stage: GateWhen) -> Vec<GateSpec> {
    gates
        .iter()
        .filter(|g| g.runs_in(stage))
        .map(|g| GateSpec {
            name: g.name.clone(),
            cmd: g.cmd.clone(),
            timeout_s: g.timeout_s,
            shell: g.shell.clone(),
        })
        .collect()
}
