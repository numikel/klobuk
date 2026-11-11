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
    let ok = !timed_out && exit_code == Some(0);
    let mut cut = tail(&combined);
    if !ok {
        if let Some(summary) = failed_summary(&combined, &cut) {
            cut.push('\n');
            cut.push_str(&summary);
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
fn run_gate_with_retry(spec: &GateSpec, cwd: &Path, pauses: &LinkerLockPauses) -> GateResult {
    let started = Instant::now();
    let mut r = run_gate(spec, cwd);
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
    let mut out = String::from("[asf] failed tests, read from the output before the tail cut it:");
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
    use super::locked_files;
    use std::path::PathBuf;

    const EXE: &str = r"D:\Programowanie\auto-software-factory\.worktrees\asf-43\target\debug\deps\asf-ce36f34fbf868c1a.exe";

    fn only(line: &str) -> Vec<PathBuf> {
        locked_files(line)
    }

    /// The exact line observed on a Polish Windows (asf-43 merge item, 2026-11-09).
    #[test]
    fn reads_the_path_from_a_polish_linker_line() {
        let line = r"LINK : fatal error LNK1104: nie można otworzyć pliku „D:\Programowanie\auto-software-factory\.worktrees\asf-43\target\debug\deps\asf-ce36f34fbf868c1a.exe”";
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
