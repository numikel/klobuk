//! Gate runner: runs configured commands in a directory with a timeout, stopping at the first failure.

use std::collections::BTreeSet;
use std::io::{ErrorKind, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use klobuk_core::config::{GateCfg, GateWhen};
use klobuk_core::{GateResult, GateSpec};

const TAIL_LINES: usize = 40;
/// How many failing tests the summary at the end of a cut tail names (see `failed_summary`).
const SUMMARY_MAX_TESTS: usize = 20;
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
/// The libtest warning is `test <name> has been running for over <n> seconds`; this is the middle.
const SLOW_MARKER: &str = " has been running for over ";
/// libtest's own threshold, used in the header when no warning said otherwise.
const SLOW_DEFAULT_SECS: u64 = 60;
/// How many slow tests the header of a stopped gate names before it says `... and N more`.
const SLOW_MAX_LISTED: usize = 50;
/// How often the poll loop looks at the process and at the new output.
const POLL: Duration = Duration::from_millis(50);

fn tail(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(TAIL_LINES);
    lines[start..].join("\n")
}

/// The program a `shell = "..."` setting runs the gate command through, `None` for no shell (the
/// command then starts directly). Windows PowerShell is `powershell`; PowerShell 7, the only one on
/// Linux and macOS, is `pwsh`.
fn shell_program(shell: &str, windows: bool) -> Option<&'static str> {
    match shell {
        "pwsh" | "powershell" => Some(if windows { "powershell" } else { "pwsh" }),
        "sh" | "bash" => Some("sh"),
        _ => None,
    }
}

/// The command that runs `spec` on Windows (`windows`) or elsewhere; [`build`] picks the real one.
fn build_for(spec: &GateSpec, windows: bool) -> Option<Command> {
    let line = spec.cmd.join(" ");
    match spec
        .shell
        .as_deref()
        .and_then(|s| shell_program(s, windows).map(|p| (s, p)))
    {
        Some(("pwsh" | "powershell", program)) => {
            let mut c = Command::new(program);
            c.args(["-NoProfile", "-NonInteractive", "-Command", &line]);
            Some(c)
        }
        Some((_, program)) => {
            let mut c = Command::new(program);
            c.args(["-c", &line]);
            Some(c)
        }
        None => {
            let (prog, rest) = spec.cmd.split_first()?;
            let mut c = Command::new(prog);
            c.args(rest);
            Some(c)
        }
    }
}

fn build(spec: &GateSpec) -> Option<Command> {
    build_for(spec, cfg!(windows))
}

/// Why `program` could not be started for `spec`. A gate that asked for a shell names the shell
/// program (the gate command is not what is missing) and the setting that asked for it.
fn start_error(spec: &GateSpec, program: &str, e: &std::io::Error) -> String {
    match spec.shell.as_deref().filter(|s| shell_program(s, cfg!(windows)).is_some()) {
        Some(shell) => format!(
            "cannot start `{program}`: {e}; gate `{}` asked for it through `shell = \"{shell}\"`, is it installed and on PATH?",
            spec.name
        ),
        None => format!("cannot start `{}`: {e}", spec.cmd.join(" ")),
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

    /// The complete lines read since the last call. `from` is the caller's offset into the buffer and
    /// moves past them; a line still being written is left for the next call.
    fn new_lines(&self, from: &mut usize) -> Vec<String> {
        let bytes = self.buf.lock().unwrap_or_else(PoisonError::into_inner);
        let end = bytes[*from..]
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(*from, |i| *from + i + 1);
        let text = String::from_utf8_lossy(&bytes[*from..end]).into_owned();
        *from = end;
        text.lines().map(str::to_owned).collect()
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

/// `(test name, seconds)` of a libtest hang warning (`test a::b has been running for over 60 seconds`).
fn slow_test_line(line: &str) -> Option<(&str, u64)> {
    let (name, rest) = line.trim().strip_prefix("test ")?.split_once(SLOW_MARKER)?;
    let secs = rest.strip_suffix(" seconds")?.trim().parse().ok()?;
    (!name.is_empty() && !name.contains(char::is_whitespace)).then_some((name, secs))
}

/// The tests a gate has reported as slow so far, in the order first seen, each once.
#[derive(Default)]
struct SlowTests {
    names: Vec<String>,
    /// The threshold the first warning named (libtest says 60).
    secs: Option<u64>,
    /// When this runner first saw a warning; `slow_after_s` counts from here.
    first_seen: Option<Instant>,
}

impl SlowTests {
    fn feed(&mut self, line: &str) {
        let Some((name, secs)) = slow_test_line(line) else {
            return;
        };
        if !self.names.iter().any(|n| n == name) {
            self.names.push(name.to_string());
        }
        self.secs.get_or_insert(secs);
        self.first_seen.get_or_insert_with(Instant::now);
    }

    /// Whether one of the gate's two limits is reached; a gate without limits never gives up.
    fn limit_hit(&self, spec: &GateSpec) -> bool {
        let enough = spec
            .slow_tests
            .is_some_and(|n| self.names.len() >= n.max(1) as usize);
        let old = spec
            .slow_after_s
            .zip(self.first_seen)
            .is_some_and(|(s, seen)| seen.elapsed() >= Duration::from_secs(s));
        enough || old
    }

    /// `gate `x` <what>: N tests ran for more than 60 s:` and the names one per line; `None` when no
    /// test was reported slow.
    fn header(&self, gate: &str, what: &str) -> Option<String> {
        if self.names.is_empty() {
            return None;
        }
        let n = self.names.len();
        let mut out = format!(
            "gate `{gate}` {what}: {n} {} ran for more than {} s:",
            if n == 1 { "test" } else { "tests" },
            self.secs.unwrap_or(SLOW_DEFAULT_SECS)
        );
        for name in self.names.iter().take(SLOW_MAX_LISTED) {
            out.push('\n');
            out.push_str(name);
        }
        if n > SLOW_MAX_LISTED {
            out.push_str(&format!("\n... and {} more", n - SLOW_MAX_LISTED));
        }
        Some(out)
    }
}

/// Why a gate was stopped by the runner instead of finishing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stop {
    Timeout,
    SlowTests,
}

/// Kills `child` and everything it started. `Child::kill` ends only the process itself, and a test
/// harness starts `klobuk serve` children of its own that would run on for as long as they like.
fn kill_tree(child: &mut Child) {
    let pid = child.id();
    #[cfg(windows)]
    let mut tree = {
        let mut c = Command::new("taskkill");
        c.args(["/PID", &pid.to_string(), "/T", "/F"]);
        c
    };
    // the gate leads its own process group (see `run_gate`), so `-<pid>` addresses the whole tree
    #[cfg(not(windows))]
    let mut tree = {
        let mut c = Command::new("kill");
        c.args(["-KILL", "--", &format!("-{pid}")]);
        c
    };
    let _ = tree
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
    let _ = child.wait();
}

/// What a gate run tells whoever watches it while it goes on (see [`run_gates_watched`]).
#[derive(Debug, Clone, Copy)]
pub enum GateEvent<'a> {
    /// The gate `.0` is about to start.
    Started(&'a str),
    /// The complete output lines the gate in flight wrote since the last event. Sent every poll, also
    /// with no lines at all, so a watcher that writes them down can tell a quiet gate from a dead runner.
    Output(&'a [String]),
    /// The gate is over (a rerun after a linker lock is part of the same gate).
    Finished(&'a GateResult),
}

/// Runs one gate; never panics. After the gate exits or is killed it waits at most [`DRAIN_GRACE`]
/// for its output, so a descendant that holds the pipes open cannot delay the result. The process
/// start itself is not bounded: the timeout clock runs from before it, so a slow start only means
/// the timeout is already due when the gate finally runs.
pub fn run_gate(spec: &GateSpec, cwd: &Path) -> GateResult {
    run_gate_watched(spec, cwd, &mut |_| {})
}

/// [`run_gate`] that tells `watch` the output as it comes ([`GateEvent::Output`] only: the caller of a
/// single gate knows when it starts and ends).
pub fn run_gate_watched(spec: &GateSpec, cwd: &Path, watch: &mut dyn FnMut(GateEvent)) -> GateResult {
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
    let program = cmd.get_program().to_string_lossy().into_owned();
    cmd.current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // own process group, so `kill_tree` can end the gate and everything it started in one go
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return fail(start_error(spec, &program, &e)),
    };
    let t_out = Drain::start(child.stdout.take().expect("piped"));
    let t_err = Drain::start(child.stderr.take().expect("piped"));
    let timeout = Duration::from_secs(spec.timeout_s.max(1));
    let mut stop = None;
    let mut slow = SlowTests::default();
    let (mut seen_out, mut seen_err) = (0, 0);
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if started.elapsed() >= timeout => {
                kill_tree(&mut child);
                stop = Some(Stop::Timeout);
                break None;
            }
            Ok(None) => {
                let mut lines = t_out.new_lines(&mut seen_out);
                lines.extend(t_err.new_lines(&mut seen_err));
                for line in &lines {
                    slow.feed(line);
                }
                watch(GateEvent::Output(&lines));
                if slow.limit_hit(spec) {
                    kill_tree(&mut child);
                    stop = Some(Stop::SlowTests);
                    break None;
                }
                thread::sleep(POLL);
            }
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
    if stop.is_some() {
        // what the poll loop did not get to see before the gate was killed
        combined.lines().for_each(|l| slow.feed(l));
    }
    match stop {
        Some(Stop::Timeout) => combined.push_str(&format!(
            "\n[klobuk] gate `{}` killed: timeout after {}s",
            spec.name, spec.timeout_s
        )),
        Some(Stop::SlowTests) => combined.push_str(&format!(
            "\n[klobuk] gate `{}` killed: stopped early on slow tests ({})",
            spec.name,
            [
                spec.slow_tests.map(|n| format!("slow_tests = {n}")),
                spec.slow_after_s.map(|s| format!("slow_after_s = {s}")),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", ")
        )),
        None => {}
    }
    let exit_code = status.and_then(|s| s.code());
    let ok = stop.is_none() && exit_code == Some(0);
    let mut cut = tail(&combined);
    if !ok {
        if let Some(summary) = failed_summary(&combined, &cut) {
            cut.push('\n');
            cut.push_str(&summary);
        }
        let header = match stop {
            Some(Stop::Timeout) => slow.header(&spec.name, &format!("timed out after {}s", spec.timeout_s)),
            Some(Stop::SlowTests) => slow.header(&spec.name, "stopped"),
            None => None,
        };
        if let Some(header) = header {
            cut = format!("{header}\n{cut}");
        }
    }
    GateResult {
        name: spec.name.clone(),
        ok,
        exit_code,
        duration_ms: started.elapsed().as_millis() as u64,
        tail: cut,
        attempts: 1,
    }
}

/// Quote characters a localized linker may put around the path: ASCII `"` and `'`, Polish „ ”, the
/// English “ ”, French « » and Japanese 「 」.
const QUOTE_CHARS: &[char] = &['"', '\'', '„', '”', '“', '«', '»', '「', '」'];

/// The `.exe` path on one `LNK1104` line, whatever language the linker message is in.
///
/// The path ends after the last `.exe` of the line (any case). It starts at the first Windows drive
/// path (`X:\` or `X:/`) on the line, which skips the sentence before it (the French
/// `d'ouvrir le fichier «...»` has an apostrophe that is not a quote); a path without a drive letter
/// must be quoted and starts after the last quote character before its end. An unquoted relative
/// name carries no reliable start, so it is not taken.
fn exe_path_on_line(line: &str) -> Option<&str> {
    let end = line.to_ascii_lowercase().rfind(".exe")? + ".exe".len();
    let head = &line[..end];
    let bytes = head.as_bytes();
    let drive = (0..bytes.len().saturating_sub(2)).find(|&i| {
        bytes[i].is_ascii_alphabetic()
            && bytes[i + 1] == b':'
            && matches!(bytes[i + 2], b'\\' | b'/')
            && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric())
    });
    let start = match drive {
        Some(i) => i,
        None => {
            let quote = head.rfind(QUOTE_CHARS)?;
            quote + head[quote..].chars().next()?.len_utf8()
        }
    };
    let path = head[start..].trim();
    (!path.is_empty()).then_some(path)
}

/// Files `link.exe` could not open, taken from the path on each `LNK1104` line.
fn locked_files(output: &str) -> Vec<std::path::PathBuf> {
    output
        .lines()
        .filter(|l| l.contains(LINKER_LOCK_MARKER))
        .filter_map(exe_path_on_line)
        .map(std::path::PathBuf::from)
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
fn run_gate_with_retry(
    spec: &GateSpec,
    cwd: &Path,
    pauses: &LinkerLockPauses,
    watch: &mut dyn FnMut(GateEvent),
) -> GateResult {
    let started = Instant::now();
    let mut r = run_gate_watched(spec, cwd, watch);
    // attempts that count against the limit, the first run included
    let mut counted = 1;
    let mut reruns = 0;
    let mut previous: Option<BTreeSet<String>> = None;
    let mut notes: Vec<String> = Vec::new();
    while !r.ok && reruns < LINKER_LOCK_MAX_RERUNS && r.tail.contains(LINKER_LOCK_MARKER) {
        let named = locked_files(&r.tail);
        let now = locked_set(&named);
        // Progress needs names to compare: a message without any path says nothing about it.
        let progress = !now.is_empty() && previous.as_ref().is_some_and(|p| *p != now);
        // The limit only stops attempts without progress: a new set of files after the last counted
        // attempt still earns a free rerun, and only the cap on all reruns stops it then.
        if !progress && counted >= LINKER_LOCK_ATTEMPTS {
            break;
        }
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
            "[klobuk] gate `{}` hit a linker lock ({LINKER_LOCK_MARKER}); {what}; {verdict}",
            spec.name,
        ));
        r = run_gate_watched(spec, cwd, watch);
    }
    if !r.ok && !notes.is_empty() && r.tail.contains(LINKER_LOCK_MARKER) {
        let reason = if reruns >= LINKER_LOCK_MAX_RERUNS {
            format!("the cap of {LINKER_LOCK_MAX_RERUNS} reruns is used up")
        } else {
            format!("{LINKER_LOCK_ATTEMPTS} attempts failed on a linker lock without progress")
        };
        notes.push(format!("[klobuk] gate `{}` gives up: {reason}", spec.name));
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
    run_gates_watched_with_pauses(specs, cwd, pauses, &mut |_| {})
}

/// [`run_gates`] that tells `watch` when each gate starts, the output it writes while it runs and its
/// result, so a caller can show the run before it is over.
pub fn run_gates_watched(
    specs: &[GateSpec],
    cwd: &Path,
    watch: &mut dyn FnMut(GateEvent),
) -> Vec<GateResult> {
    run_gates_watched_with_pauses(specs, cwd, &LinkerLockPauses::DEFAULT, watch)
}

/// [`run_gates_watched`] with the linker-lock pause schedules injected.
pub fn run_gates_watched_with_pauses(
    specs: &[GateSpec],
    cwd: &Path,
    pauses: &LinkerLockPauses,
    watch: &mut dyn FnMut(GateEvent),
) -> Vec<GateResult> {
    let mut results = Vec::new();
    for spec in specs {
        watch(GateEvent::Started(&spec.name));
        let r = run_gate_with_retry(spec, cwd, pauses, watch);
        watch(GateEvent::Finished(&r));
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
            slow_tests: g.slow_tests,
            slow_after_s: g.slow_after_s,
        })
        .collect()
}

/// The test binary a `cargo test` run printed in its `Running ...` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestBinary {
    /// `Running unittests <root> (...<exe>)`: `krate` is the directory name under `crates/` (the exe
    /// name without its hash, `_` read as `-`).
    Unit { krate: String },
    /// `Running tests/<file>.rs (...)`: the crate is not in the line, only the file.
    Integration { file: String },
    /// `Doc-tests <crate>`: a doc test has no module path to follow.
    Doc,
}

/// One failing test of a `cargo test` tail. `binary` is `None` when the tail does not say which
/// binary it ran in (the `Running` line was cut off).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedTest {
    pub name: String,
    pub binary: Option<TestBinary>,
}

/// A failing test that the branch did not touch, with the file it lives in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignTest {
    pub name: String,
    pub file: String,
}

/// The binary named by a `Running ...` or `Doc-tests ...` line. `Some(None)` is a line that starts a
/// run but that this reader cannot make sense of (an older cargo format): the binary is then unknown.
fn binary_of(line: &str) -> Option<Option<TestBinary>> {
    let t = line.trim_start();
    if t.starts_with("Doc-tests ") {
        return Some(Some(TestBinary::Doc));
    }
    let rest = t.strip_prefix("Running ")?;
    let Some((head, exe)) = rest.split_once(" (") else {
        return Some(None);
    };
    let head = head.replace('\\', "/");
    if head.starts_with("unittests ") {
        let exe = exe.trim_end_matches(')').replace('\\', "/");
        let file = exe.rsplit('/').next().unwrap_or("");
        let file = file.strip_suffix(".exe").unwrap_or(file);
        let krate = file.rsplit_once('-').map_or(file, |(name, _hash)| name);
        return Some(Some(TestBinary::Unit {
            krate: krate.replace('_', "-"),
        }));
    }
    Some(Some(TestBinary::Integration {
        file: head.trim().to_string(),
    }))
}

/// The tests a `cargo test` tail reports as failed: the `---- <name> stdout ----` headers and the
/// indented `failures:` list, each name once, in the order first seen.
///
/// A name belongs to the nearest `Running` line above it. The gate output is stdout followed by
/// stderr and cargo prints `Running` on stderr, so a name with none above it takes the last binary
/// of the tail: without `--no-fail-fast` cargo stops at the first failing binary, which is the last
/// one that ran.
pub fn failed_tests(tail: &str) -> Vec<FailedTest> {
    let mut found: Vec<FailedTest> = Vec::new();
    let mut current: Option<TestBinary> = None;
    let mut last: Option<TestBinary> = None;
    let mut in_list = false;
    let mut listed = false;
    fn add(name: &str, binary: &Option<TestBinary>, found: &mut Vec<FailedTest>) {
        if name.is_empty() {
            return;
        }
        match found.iter_mut().find(|f| f.name == name) {
            // seen before its binary was known (stdout comes first): a later sighting names it
            Some(f) => {
                if f.binary.is_none() {
                    f.binary = binary.clone();
                }
            }
            None => found.push(FailedTest {
                name: name.to_string(),
                binary: binary.clone(),
            }),
        }
    }
    for line in tail.lines() {
        if let Some(binary) = binary_of(line) {
            current = binary;
            last = current.clone();
            in_list = false;
            continue;
        }
        let t = line.trim();
        if t == "failures:" {
            (in_list, listed) = (true, false);
            continue;
        }
        if in_list {
            if line.starts_with("    ") && !t.is_empty() {
                add(t, &current, &mut found);
                listed = true;
                continue;
            }
            if t.is_empty() && !listed {
                continue;
            }
            in_list = false;
        }
        if let Some(name) = t
            .strip_prefix("---- ")
            .and_then(|r| r.strip_suffix(" stdout ----"))
        {
            add(name.trim(), &current, &mut found);
        }
    }
    for f in &mut found {
        if f.binary.is_none() {
            f.binary = last.clone();
        }
    }
    found
}

/// The `Running` line that makes [`failed_tests`] read the names below it as belonging to `binary`.
fn running_line(binary: Option<&TestBinary>) -> String {
    match binary {
        Some(TestBinary::Unit { krate }) => {
            format!("     Running unittests src/lib.rs (target/debug/deps/{krate}-summary)")
        }
        Some(TestBinary::Integration { file }) => {
            format!("     Running {file} (target/debug/deps/summary)")
        }
        Some(TestBinary::Doc) => "   Doc-tests summary".to_string(),
        // a line this reader cannot place: the names below it stay without a binary
        None => "     Running unknown".to_string(),
    }
}

/// A short block for the end of a gate tail that lists the failing tests of `full`, when the cut
/// `cut` no longer tells the same story. The tail is the last [`TAIL_LINES`] lines of stdout followed
/// by stderr, and cargo writes a stderr line for every crate it builds and every binary it runs, so in
/// a workspace run the failures list is pushed out by the lines after it. The block is in the format
/// [`failed_tests`] reads: the `Running` line of the binary, `failures:`, the indented names.
///
/// At most [`SUMMARY_MAX_TESTS`] names are listed. A run with more failures than that ends in a line
/// that maps to no file, so it never counts as foreign: that many failures are not a flake.
fn failed_summary(full: &str, cut: &str) -> Option<String> {
    let all = failed_tests(full);
    if all.is_empty() || failed_tests(cut) == all {
        return None;
    }
    let mut out = String::from("[klobuk] failed tests, read from the output before the tail cut it:");
    let mut group: Option<&Option<TestBinary>> = None;
    for t in all.iter().take(SUMMARY_MAX_TESTS) {
        if group != Some(&t.binary) {
            out.push_str(&format!("\n{}\nfailures:", running_line(t.binary.as_ref())));
            group = Some(&t.binary);
        }
        out.push_str(&format!("\n    {}", t.name));
    }
    if all.len() > SUMMARY_MAX_TESTS {
        out.push_str(&format!(
            "\n{}\nfailures:\n    ... and {} more",
            running_line(None),
            all.len() - SUMMARY_MAX_TESTS
        ));
    }
    Some(out)
}

/// The source file (relative to `repo`, `/` separators) that holds `test`, or `None` when it cannot
/// be told.
///
/// A unit test `a::b::tests::name` is looked up as `crates/<crate>/src/a/b/tests.rs`, then
/// `.../a/b/tests/mod.rs`, then the same for `a/b`, then `a`: the longest module prefix that is a
/// file wins, which is how an inline `mod tests` ends up in its parent's file. No such file means
/// no answer (a module declared in the crate root or through `#[path]` is not guessed at). An
/// integration test is `crates/*/tests/<file>`, and only when exactly one crate has that file.
pub fn test_source_file(repo: &Path, test: &FailedTest) -> Option<String> {
    let is_file = |rel: &str| repo.join(rel).is_file();
    match test.binary.as_ref()? {
        TestBinary::Doc => None,
        TestBinary::Unit { krate } => {
            let modules: Vec<&str> = test.name.split("::").collect();
            let modules = &modules[..modules.len() - 1];
            for len in (1..=modules.len()).rev() {
                let module = modules[..len].join("/");
                for candidate in [
                    format!("crates/{krate}/src/{module}.rs"),
                    format!("crates/{krate}/src/{module}/mod.rs"),
                ] {
                    if is_file(&candidate) {
                        return Some(candidate);
                    }
                }
            }
            None
        }
        TestBinary::Integration { file } => {
            let mut hits = std::fs::read_dir(repo.join("crates"))
                .ok()?
                .flatten()
                .filter(|c| c.path().join(file).is_file())
                .map(|c| format!("crates/{}/{file}", c.file_name().to_string_lossy()));
            let first = hits.next()?;
            hits.next().is_none().then_some(first)
        }
    }
}

/// The failing tests of a gate `tail` when every one of them lives in a file outside `changed` (the
/// branch's own diff, repo-relative with `/`). `None` when the tail names no test, or when any test
/// maps to a changed file or to no file at all: what cannot be placed counts as the branch's own.
pub fn foreign_tests(repo: &Path, tail: &str, changed: &[String]) -> Option<Vec<ForeignTest>> {
    let tests = failed_tests(tail);
    if tests.is_empty() {
        return None;
    }
    tests
        .into_iter()
        .map(|t| {
            let file = test_source_file(repo, &t).filter(|f| !changed.contains(f))?;
            Some(ForeignTest { name: t.name, file })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{build_for, locked_files, slow_test_line, start_error, SlowTests, SLOW_MAX_LISTED};
    use klobuk_core::GateSpec;
    use std::path::PathBuf;

    fn shell_spec(shell: Option<&str>, cmd: &[&str]) -> GateSpec {
        GateSpec {
            name: "ps".into(),
            cmd: cmd.iter().map(|s| s.to_string()).collect(),
            timeout_s: 30,
            shell: shell.map(str::to_owned),
            slow_tests: None,
            slow_after_s: None,
        }
    }

    /// The program and arguments `build_for` produces, without starting anything.
    fn program_and_args(spec: &GateSpec, windows: bool) -> (String, Vec<String>) {
        let c = build_for(spec, windows).expect("a command");
        (
            c.get_program().to_string_lossy().into_owned(),
            c.get_args().map(|a| a.to_string_lossy().into_owned()).collect(),
        )
    }

    #[test]
    fn pwsh_and_powershell_start_powershell_on_windows_and_pwsh_elsewhere() {
        for shell in ["pwsh", "powershell"] {
            let spec = shell_spec(Some(shell), &["Write-Output", "hi"]);
            let args = ["-NoProfile", "-NonInteractive", "-Command", "Write-Output hi"];
            assert_eq!(
                program_and_args(&spec, true),
                ("powershell".into(), args.map(String::from).to_vec()),
                "{shell} on Windows"
            );
            assert_eq!(
                program_and_args(&spec, false),
                ("pwsh".into(), args.map(String::from).to_vec()),
                "{shell} elsewhere"
            );
        }
    }

    #[test]
    fn other_shells_and_plain_commands_do_not_depend_on_the_platform() {
        for windows in [true, false] {
            let sh = shell_spec(Some("bash"), &["echo", "hi"]);
            assert_eq!(
                program_and_args(&sh, windows),
                ("sh".into(), vec!["-c".into(), "echo hi".into()])
            );
            let plain = shell_spec(None, &["cargo", "test", "-q"]);
            assert_eq!(
                program_and_args(&plain, windows),
                ("cargo".into(), vec!["test".into(), "-q".into()])
            );
            let unknown = shell_spec(Some("zsh"), &["cargo", "fmt"]);
            assert_eq!(
                program_and_args(&unknown, windows),
                ("cargo".into(), vec!["fmt".into()])
            );
        }
        assert!(
            build_for(&shell_spec(None, &[]), true).is_none(),
            "empty cmd has nothing to start"
        );
    }

    #[test]
    fn a_missing_shell_program_is_named_with_the_setting_that_asked_for_it() {
        let e = std::io::Error::from(std::io::ErrorKind::NotFound);
        let msg = start_error(&shell_spec(Some("pwsh"), &["Write-Output", "hi"]), "pwsh", &e);
        assert!(msg.starts_with("cannot start `pwsh`: "), "{msg}");
        assert!(
            msg.contains("gate `ps` asked for it through `shell = \"pwsh\"`"),
            "{msg}"
        );
        assert!(
            !msg.contains("Write-Output"),
            "the gate command is not what is missing: {msg}"
        );
        // without a shell the message names the gate command, as before
        let msg = start_error(&shell_spec(None, &["python", "-c", "1"]), "python", &e);
        assert!(msg.starts_with("cannot start `python -c 1`: "), "{msg}");
        assert!(!msg.contains("shell ="), "{msg}");
    }

    #[test]
    fn reads_the_name_and_seconds_of_a_libtest_hang_warning() {
        assert_eq!(
            slow_test_line("test serve::loop_ends has been running for over 60 seconds"),
            Some(("serve::loop_ends", 60))
        );
        // Windows line ending and indentation
        assert_eq!(
            slow_test_line("  test a::b has been running for over 120 seconds\r"),
            Some(("a::b", 120))
        );
    }

    #[test]
    fn other_lines_are_not_hang_warnings() {
        for line in [
            "",
            "test a::b ... ok",
            "test a::b ... FAILED",
            "running 3 tests",
            "a::b has been running for over 60 seconds",
            "test  has been running for over 60 seconds",
            "test two words has been running for over 60 seconds",
            "test a::b has been running for over many seconds",
            "test a::b has been running for over 60 seconds, really",
        ] {
            assert_eq!(slow_test_line(line), None, "{line:?}");
        }
    }

    #[test]
    fn the_header_names_each_test_once_one_per_line() {
        let mut s = SlowTests::default();
        assert_eq!(s.header("test", "stopped"), None, "nothing slow, no header");
        for l in [
            "test b::two has been running for over 60 seconds",
            "test a::one has been running for over 60 seconds",
            "test b::two has been running for over 60 seconds",
        ] {
            s.feed(l);
        }
        assert_eq!(
            s.header("test", "stopped").unwrap(),
            "gate `test` stopped: 2 tests ran for more than 60 s:\nb::two\na::one"
        );
    }

    #[test]
    fn a_long_list_is_cut_with_a_count_of_the_rest() {
        let mut s = SlowTests::default();
        for i in 0..SLOW_MAX_LISTED + 7 {
            s.feed(&format!("test t{i} has been running for over 60 seconds"));
        }
        let h = s.header("test", "timed out after 9s").unwrap();
        assert!(h.starts_with(&format!(
            "gate `test` timed out after 9s: {} tests ran",
            SLOW_MAX_LISTED + 7
        )));
        assert_eq!(h.lines().count(), 1 + SLOW_MAX_LISTED + 1);
        assert!(h.ends_with("... and 7 more"), "{h}");
    }

    const EXE: &str = r"D:\Programowanie\auto-software-factory\.worktrees\asf-43\target\debug\deps\klobuk-ce36f34fbf868c1a.exe";

    fn only(line: &str) -> Vec<PathBuf> {
        locked_files(line)
    }

    /// The exact line observed on a Polish Windows (asf-43 merge item, 2026-11-09).
    #[test]
    fn reads_the_path_from_a_polish_linker_line() {
        let line = r"LINK : fatal error LNK1104: nie można otworzyć pliku „D:\Programowanie\auto-software-factory\.worktrees\asf-43\target\debug\deps\klobuk-ce36f34fbf868c1a.exe”";
        assert_eq!(only(line), vec![PathBuf::from(EXE)]);
    }

    #[test]
    fn reads_the_path_from_an_english_linker_line() {
        let line = format!("LINK : fatal error LNK1104: cannot open file \"{EXE}\"");
        assert_eq!(only(&line), vec![PathBuf::from(EXE)]);
    }

    #[test]
    fn reads_the_path_whatever_quotes_the_locale_uses() {
        for (open, close) in [
            ("'", "'"),
            ("“", "”"),
            ("«", "»"),
            ("« ", " »"),
            ("「", "」"),
            ("„", "“"),
        ] {
            let line = format!("LINK : fatal error LNK1104: file {open}{EXE}{close}");
            assert_eq!(only(&line), vec![PathBuf::from(EXE)], "quotes {open}{close}");
        }
    }

    /// French has an apostrophe in the sentence before the path, which must not start the path.
    #[test]
    fn an_apostrophe_in_the_message_text_does_not_start_the_path() {
        let line = format!("LINK : fatal error LNK1104: impossible d'ouvrir le fichier «{EXE}»");
        assert_eq!(only(&line), vec![PathBuf::from(EXE)]);
    }

    #[test]
    fn keeps_spaces_and_apostrophes_inside_the_path() {
        let path = r"C:\Users\Jan O'Brien\My Projects\target\debug\deps\a-1.EXE";
        let line = format!("LINK : fatal error LNK1104: nie można otworzyć pliku „{path}”");
        assert_eq!(only(&line), vec![PathBuf::from(path)]);
    }

    #[test]
    fn forward_slashes_are_accepted() {
        let path = "D:/work/target/debug/deps/a-1.exe";
        let line = format!("LINK : fatal error LNK1104: cannot open file \"{path}\"");
        assert_eq!(only(&line), vec![PathBuf::from(path)]);
    }

    #[test]
    fn collects_one_path_per_linker_line_and_skips_other_lines() {
        let out = format!(
            "   Compiling x\nLINK : fatal error LNK1104: nie można otworzyć pliku „{EXE}”\nerror: other\n\
LINK : fatal error LNK1104: cannot open file \"{}\"\n{} is not an LNK line\n",
            r"D:\b\two.exe", r"D:\b\three.exe"
        );
        assert_eq!(
            only(&out),
            vec![PathBuf::from(EXE), PathBuf::from(r"D:\b\two.exe")]
        );
    }

    #[test]
    fn a_quoted_relative_path_still_counts() {
        let line = r#"LINK : fatal error LNK1104: cannot open file "target\debug\deps\a.exe""#;
        assert_eq!(only(line), vec![PathBuf::from(r"target\debug\deps\a.exe")]);
    }

    #[test]
    fn ignores_lines_without_a_path_to_an_exe() {
        assert!(only("LINK : fatal error LNK1104: cannot open file foo.exe").is_empty());
        assert!(only(r#"LINK : fatal error LNK1104: cannot open file "D:\x\y.rlib""#).is_empty());
        assert!(only("LINK : fatal error LNK1104").is_empty());
        assert!(only("").is_empty());
    }
}
