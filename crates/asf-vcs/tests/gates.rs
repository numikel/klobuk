use std::time::Duration;

use asf_core::GateSpec;
use asf_vcs::gates::{run_gates_with_pauses, LinkerLockPauses};
use asf_vcs::run_gates;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// Pauses so long that a test using them by mistake would visibly hang.
const HUGE: [Duration; 4] = [Duration::from_secs(600); 4];

fn spec(name: &str, cmd: &[&str], timeout_s: u64) -> GateSpec {
    GateSpec {
        name: name.into(),
        cmd: cmd.iter().map(|s| s.to_string()).collect(),
        timeout_s,
        shell: None,
    }
}

fn py(code: &str) -> Vec<&str> {
    vec!["python", "-c", code]
}

#[test]
fn runs_in_order_and_stops_at_first_failure() {
    let dir = tempfile::tempdir().unwrap();
    let specs = vec![
        spec("one", &py("print('first ok')"), 30),
        spec("two", &py("import sys; print('boom'); sys.exit(3)"), 30),
        spec("three", &py("print('never')"), 30),
    ];
    let results = run_gates(&specs, dir.path());
    assert_eq!(results.len(), 2, "third gate must not run");
    assert!(results[0].ok);
    assert_eq!(results[0].exit_code, Some(0));
    assert!(results[0].tail.contains("first ok"));
    assert!(!results[1].ok);
    assert_eq!(results[1].exit_code, Some(3));
    assert!(results[1].tail.contains("boom"));
    assert_eq!(results[1].name, "two");
}

#[test]
fn timeout_kills_and_reports() {
    let dir = tempfile::tempdir().unwrap();
    // The gate stamps the moment it begins to run. `run_gates` is timed from there, not from the
    // call: starting a process takes seconds when other tests spawn at the same time (measured up
    // to 6 s), and that wait is not what this test is about.
    let code = "import time; open('started', 'w').write(str(time.time())); time.sleep(30)";
    let specs = vec![spec("slow", &py(code), 1)];
    let results = run_gates(&specs, dir.path());
    let returned = epoch_now();
    // no stamp means the gate was killed before it ran a single line, which is also a kill
    if let Some(started) = epoch_in(&dir.path().join("started")) {
        assert!(
            returned - started < 10.0,
            "killed well before its own 30 s sleep ended, but run_gates came back {:.1} s after the gate began",
            returned - started
        );
    }
    assert!(!results[0].ok);
    assert_eq!(results[0].exit_code, None, "killed => no exit code");
    assert!(results[0].tail.contains("timeout"), "{}", results[0].tail);
}

/// Seconds since the Unix epoch, the same clock as Python's `time.time()` in a gate.
fn epoch_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

/// A timestamp a gate wrote into `path`, if it got as far as writing it.
fn epoch_in(path: &std::path::Path) -> Option<f64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Killing the gate must not wait for its descendants: a grandchild that inherited the output pipe
/// keeps it open long after the gate itself is gone (`sh -c`, `cargo test`, ...). The clock starts
/// when the grandchild runs, not when `run_gates` is called.
///
/// The gate's timeout counts from before the process start, which takes seconds under parallel load
/// (measured up to 6 s). A gate killed before `Popen` ran leaves no grandchild and proves nothing,
/// so the gate stamps `parent` only after `Popen` returned (the grandchild then exists, and killing
/// the gate does not kill it); without the stamp the scenario is run again in a fresh directory.
#[test]
fn returns_promptly_after_a_timeout_even_when_a_descendant_keeps_the_pipe_open() {
    const ATTEMPTS: u32 = 3;
    const GRANDCHILD_SLEEP_S: u32 = 40;
    let code = format!(
        "import subprocess, sys, time\n\
subprocess.Popen([sys.executable, '-c', \"import time; open('grandchild', 'w').write(str(time.time())); time.sleep({GRANDCHILD_SLEEP_S})\"])\n\
open('parent', 'w').write('x')\n\
time.sleep(120)"
    );
    for attempt in 1..=ATTEMPTS {
        let dir = tempfile::tempdir().unwrap();
        let results = run_gates(&[spec("tree", &py(&code), 8)], dir.path());
        let returned = epoch_now();
        assert!(!results[0].ok);
        assert_eq!(results[0].exit_code, None, "killed => no exit code");
        assert!(results[0].tail.contains("timeout"), "{}", results[0].tail);
        if !dir.path().join("parent").exists() {
            eprintln!("attempt {attempt}/{ATTEMPTS}: the gate was killed before it started a grandchild");
            continue;
        }
        // the grandchild exists but may still be starting: wait for its timestamp
        let marker = dir.path().join("grandchild");
        let wait = std::time::Instant::now();
        let grandchild_started = loop {
            if let Some(t) = epoch_in(&marker) {
                break t;
            }
            assert!(
                wait.elapsed().as_secs() < 60,
                "the grandchild never wrote its timestamp"
            );
            std::thread::sleep(ms(50));
        };
        assert!(
            returned - grandchild_started < 25.0,
            "run_gates came back {:.1} s after the grandchild started; it slept {GRANDCHILD_SLEEP_S} s holding the pipe",
            returned - grandchild_started
        );
        return;
    }
    panic!("the gate was killed before it could start a grandchild in all {ATTEMPTS} attempts");
}

#[test]
fn missing_binary_is_a_failed_gate_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let results = run_gates(&[spec("nope", &["definitely-not-a-binary-xyz"], 5)], dir.path());
    assert!(!results[0].ok);
    assert!(results[0].tail.contains("definitely-not-a-binary-xyz"));
}

#[test]
fn tail_keeps_last_lines_only_and_runs_in_cwd() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("marker.txt"), "m").unwrap();
    let code =
        "import os; print('\\n'.join(str(i) for i in range(100))); print(os.path.exists('marker.txt'))";
    let results = run_gates(&[spec("many", &py(code), 30)], dir.path());
    assert!(results[0].ok);
    assert!(
        results[0].tail.lines().count() <= 40,
        "{}",
        results[0].tail.lines().count()
    );
    assert!(
        results[0].tail.contains("True"),
        "ran in cwd: {}",
        results[0].tail
    );
    assert!(!results[0].tail.contains("\n0\n"), "early lines dropped");
}

#[test]
fn shell_pwsh_runs_through_powershell() {
    let dir = tempfile::tempdir().unwrap();
    let s = GateSpec {
        name: "ps".into(),
        cmd: vec!["Write-Output".into(), "hello-from-pwsh".into()],
        timeout_s: 30,
        shell: Some("pwsh".into()),
    };
    let results = run_gates(&[s], dir.path());
    assert!(results[0].ok, "{}", results[0].tail);
    assert!(results[0].tail.contains("hello-from-pwsh"));
}

/// Windows: the AV scanner holds a freshly linked test executable for a moment and `link.exe`
/// reports LNK1104. That is not a code failure, so the gate is rerun a couple of times.
#[test]
fn retries_a_gate_that_failed_on_a_linker_lock() {
    let dir = tempfile::tempdir().unwrap();
    let code = "import os, sys; e = os.path.exists('locked.once'); open('locked.once', 'w').close(); \
print('tests ok' if e else 'LINK : fatal error LNK1104: cannot open file foo.exe'); sys.exit(0 if e else 101)";
    let pauses = LinkerLockPauses {
        moved: &HUGE,
        absent: &[ms(300); 4],
    };
    let start = std::time::Instant::now();
    let results = run_gates_with_pauses(&[spec("test", &py(code), 30)], dir.path(), &pauses);
    assert!(results[0].ok, "{}", results[0].tail);
    assert_eq!(results[0].exit_code, Some(0));
    assert!(results[0].tail.contains("tests ok"), "{}", results[0].tail);
    assert!(
        results[0].tail.contains("LNK1104"),
        "the retry is visible: {}",
        results[0].tail
    );
    assert!(results[0].tail.contains("attempt 2"), "{}", results[0].tail);
    assert!(start.elapsed() >= ms(300), "waits before the rerun");
}

#[test]
fn gives_up_on_a_permanent_linker_lock_after_five_attempts() {
    let dir = tempfile::tempdir().unwrap();
    let code = "import sys\n\
with open('runs.log', 'a') as f: f.write('x')\n\
print('LINK : fatal error LNK1104: cannot open file foo.exe'); sys.exit(101)";
    let pauses = LinkerLockPauses {
        moved: &HUGE,
        absent: &[ms(10); 4],
    };
    let results = run_gates_with_pauses(&[spec("test", &py(code), 30)], dir.path(), &pauses);
    assert!(!results[0].ok);
    assert_eq!(results[0].exit_code, Some(101));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("runs.log")).unwrap(),
        "xxxxx"
    );
    assert!(results[0].tail.contains("attempt 5"), "{}", results[0].tail);
}

#[test]
fn ordinary_failures_are_not_retried() {
    let dir = tempfile::tempdir().unwrap();
    let code = "import sys\n\
with open('runs.log', 'a') as f: f.write('x')\n\
print('assertion failed'); sys.exit(101)";
    let results = run_gates(&[spec("test", &py(code), 30)], dir.path());
    assert!(!results[0].ok);
    assert_eq!(std::fs::read_to_string(dir.path().join("runs.log")).unwrap(), "x");
}

/// Rerunning alone is not enough: cargo links to the same file name and the scanner keeps holding
/// it. The locked file named in the message is moved aside first, so the linker writes a fresh one.
#[test]
fn moves_the_locked_executable_aside_before_rerunning() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("deps")).unwrap();
    let locked = dir.path().join("deps").join("cli_tui-b05e1b3ffcdc1251.exe");
    std::fs::write(&locked, "old").unwrap();
    // fails with LNK1104 naming the file while it exists; passes once it is gone
    let code = format!(
        "import os, sys; p = {:?}; e = os.path.exists(p); \
print('tests ok' if not e else 'LINK : fatal error LNK1104: cannot open file \"' + p + '\"'); \
sys.exit(0 if not e else 101)",
        locked.to_string_lossy().replace('\\', "/")
    );
    let results = run_gates(&[spec("test", &py(&code), 30)], dir.path());
    assert!(results[0].ok, "{}", results[0].tail);
    assert!(!locked.exists(), "locked file moved aside");
    assert!(
        dir.path()
            .join("deps")
            .join("cli_tui-b05e1b3ffcdc1251.exe.locked.old")
            .exists(),
        "moved to *.locked.old"
    );
    assert!(results[0].tail.contains("moved aside"), "{}", results[0].tail);
    assert!(results[0].tail.contains("attempt 2"), "{}", results[0].tail);
    assert!(
        results[0].duration_ms >= 2000,
        "duration covers every attempt: {}",
        results[0].duration_ms
    );
}

/// A gate that fails with LNK1104 naming `missing` (a file that never exists) until its `pass_on`-th
/// run, counted in `n.txt`. Every run is also appended to `runs.log`.
fn missing_file_gate(missing: &std::path::Path, pass_on: u32) -> String {
    format!(
        "import os, sys\n\
n = int(open('n.txt').read()) + 1 if os.path.exists('n.txt') else 1\n\
open('n.txt', 'w').write(str(n))\n\
with open('runs.log', 'a') as f: f.write('x')\n\
if n >= {pass_on}: print('tests ok'); sys.exit(0)\n\
print('LINK : fatal error LNK1104: cannot open file \"' + {:?} + '\"'); sys.exit(101)",
        missing.to_string_lossy().replace('\\', "/")
    )
}

/// The AV scanner keeps the name of a just-deleted executable, so the linker cannot create it and
/// there is nothing to move aside: the only remedy is time, taken from the longer schedule.
#[test]
fn a_lock_on_a_file_that_does_not_exist_waits_the_longer_schedule_and_reruns() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("deps").join("asf_vcs-8cd7fc69d49f91f7.exe");
    let code = missing_file_gate(&missing, 3);
    let pauses = LinkerLockPauses {
        moved: &HUGE,
        absent: &[ms(150), ms(250), ms(900), ms(900)],
    };
    let start = std::time::Instant::now();
    let results = run_gates_with_pauses(&[spec("test", &py(&code), 30)], dir.path(), &pauses);
    let tail = &results[0].tail;
    assert!(results[0].ok, "{tail}");
    assert!(tail.contains("tests ok"), "{tail}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("runs.log")).unwrap(),
        "xxx",
        "passes on the third run"
    );
    assert!(tail.contains("attempt 3"), "{tail}");
    assert!(start.elapsed() >= ms(400), "waited 150 ms + 250 ms");
    assert!(
        start.elapsed() < Duration::from_secs(30),
        "the huge `moved` schedule was not used"
    );
}

#[test]
fn the_note_says_nothing_could_be_moved_aside_and_how_long_it_waits() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("deps").join("asf_vcs-8cd7fc69d49f91f7.exe");
    let code = missing_file_gate(&missing, 2);
    let pauses = LinkerLockPauses {
        moved: &HUGE,
        absent: &[Duration::from_secs(1); 4],
    };
    let results = run_gates_with_pauses(&[spec("test", &py(&code), 30)], dir.path(), &pauses);
    let tail = &results[0].tail;
    assert!(results[0].ok, "{tail}");
    assert!(
        tail.contains("nothing to move aside (the file does not exist yet); waiting 1 s"),
        "{tail}"
    );
    assert!(!tail.contains("moved aside []"), "{tail}");
    assert!(tail.contains("rerun attempt 2/5"), "{tail}");
}

#[test]
fn a_lock_message_without_a_file_path_is_explained_too() {
    let dir = tempfile::tempdir().unwrap();
    let code = "import os, sys\n\
if os.path.exists('once'): print('tests ok'); sys.exit(0)\n\
open('once', 'w').close()\n\
print('LINK : fatal error LNK1104: cannot open file foo.exe'); sys.exit(101)";
    let pauses = LinkerLockPauses {
        moved: &HUGE,
        absent: &[ms(10); 4],
    };
    let results = run_gates_with_pauses(&[spec("test", &py(code), 30)], dir.path(), &pauses);
    let tail = &results[0].tail;
    assert!(results[0].ok, "{tail}");
    assert!(
        tail.contains("nothing to move aside (no file path in the linker message)"),
        "{tail}"
    );
    assert!(!tail.contains("moved aside []"), "{tail}");
}

#[test]
fn a_permanent_lock_on_a_missing_file_uses_all_four_longer_pauses_and_five_attempts() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("deps").join("never.exe");
    let code = missing_file_gate(&missing, u32::MAX);
    let pauses = LinkerLockPauses {
        moved: &HUGE,
        absent: &[ms(100), ms(200), ms(300), ms(400)],
    };
    let start = std::time::Instant::now();
    let results = run_gates_with_pauses(&[spec("test", &py(&code), 30)], dir.path(), &pauses);
    assert!(!results[0].ok);
    assert_eq!(results[0].exit_code, Some(101));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("runs.log")).unwrap(),
        "xxxxx"
    );
    assert!(start.elapsed() >= ms(1000), "100 + 200 + 300 + 400 ms");
    assert!(results[0].tail.contains("attempt 5"), "{}", results[0].tail);
    assert!(results[0].duration_ms >= 1000, "{}", results[0].duration_ms);
}

/// A locked file that exists is handled as before: moved aside after the short pause, the longer
/// schedule is never consulted.
#[test]
fn a_lock_on_an_existing_file_still_moves_it_aside_with_the_short_pause() {
    let dir = tempfile::tempdir().unwrap();
    let locked = dir.path().join("cli_tui-b05e1b3ffcdc1251.exe");
    std::fs::write(&locked, "old").unwrap();
    let code = format!(
        "import os, sys; p = {:?}; e = os.path.exists(p); \
print('tests ok' if not e else 'LINK : fatal error LNK1104: cannot open file \"' + p + '\"'); \
sys.exit(0 if not e else 101)",
        locked.to_string_lossy().replace('\\', "/")
    );
    let pauses = LinkerLockPauses {
        moved: &[ms(50); 4],
        absent: &HUGE,
    };
    let results = run_gates_with_pauses(&[spec("test", &py(&code), 30)], dir.path(), &pauses);
    let tail = &results[0].tail;
    assert!(results[0].ok, "{tail}");
    assert!(!locked.exists());
    assert!(dir
        .path()
        .join("cli_tui-b05e1b3ffcdc1251.exe.locked.old")
        .exists());
    assert!(
        tail.contains("moved aside [") && tail.contains("cli_tui"),
        "{tail}"
    );
    assert!(!tail.contains("nothing to move aside"), "{tail}");
}

/// A gate that fails with one `LNK1104` line per file in the Python list `files` (an expression over
/// `n`, the 1-based run number kept in `n.txt`) until its `pass_on`-th run. None of the named files
/// exists under `missing_dir`, so nothing is moved aside and only the pauses and the counting matter.
/// Every run is also appended to `runs.log`.
fn lock_gate(missing_dir: &std::path::Path, files: &str, pass_on: u32) -> String {
    format!(
        "import os, sys\n\
n = int(open('n.txt').read()) + 1 if os.path.exists('n.txt') else 1\n\
open('n.txt', 'w').write(str(n))\n\
with open('runs.log', 'a') as f: f.write('x')\n\
if n >= {pass_on}: print('tests ok'); sys.exit(0)\n\
base = {:?}\n\
for name in {files}: print('LINK : fatal error LNK1104: cannot open file \"' + base + '/' + name + '\"')\n\
sys.exit(101)",
        missing_dir.to_string_lossy().replace('\\', "/")
    )
}

fn runs_of(dir: &std::path::Path) -> usize {
    std::fs::read_to_string(dir.join("runs.log")).unwrap().len()
}

const QUICK: LinkerLockPauses<'static> = LinkerLockPauses {
    moved: &[Duration::from_millis(5); 4],
    absent: &[Duration::from_millis(5); 4],
};

/// The asf-31 pattern: every attempt links more executables and trips over different ones. Five
/// attempts are not enough there, but each attempt made progress, so the limit is not used up.
#[test]
fn a_rerun_that_hits_different_files_does_not_count_against_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let code = lock_gate(&dir.path().join("deps"), "['f%d.exe' % n]", 9);
    let results = run_gates_with_pauses(&[spec("test", &py(&code), 30)], dir.path(), &QUICK);
    let tail = &results[0].tail;
    assert!(results[0].ok, "{tail}");
    assert_eq!(
        runs_of(dir.path()),
        9,
        "passes on the ninth run, past the old limit of five"
    );
    assert!(tail.contains("progress"), "{tail}");
    assert!(tail.contains("does not count"), "{tail}");
}

#[test]
fn reruns_on_the_same_files_keep_counting_and_stop_at_five_attempts() {
    let dir = tempfile::tempdir().unwrap();
    let code = lock_gate(&dir.path().join("deps"), "['a.exe']", u32::MAX);
    let results = run_gates_with_pauses(&[spec("test", &py(&code), 30)], dir.path(), &QUICK);
    let tail = &results[0].tail;
    assert!(!results[0].ok);
    assert_eq!(results[0].exit_code, Some(101));
    assert_eq!(runs_of(dir.path()), 5);
    assert!(tail.contains("same 1 file(s)"), "{tail}");
    assert!(tail.contains("no progress"), "{tail}");
    assert!(tail.contains("rerun attempt 5/5 (counts)"), "{tail}");
    assert!(!tail.contains("does not count"), "{tail}");
}

/// Progress buys one free rerun, not immunity: once the files repeat, every further rerun counts.
#[test]
fn the_same_files_count_again_after_a_rerun_that_made_progress() {
    let dir = tempfile::tempdir().unwrap();
    let code = lock_gate(
        &dir.path().join("deps"),
        "(['a.exe'] if n < 2 else ['b.exe'])",
        u32::MAX,
    );
    let results = run_gates_with_pauses(&[spec("test", &py(&code), 30)], dir.path(), &QUICK);
    let tail = &results[0].tail;
    assert!(!results[0].ok);
    // run 1 counts (first lock), run 2 is free (a -> b), runs 3..=6 repeat b and count: attempt 5/5
    assert_eq!(runs_of(dir.path()), 6, "{tail}");
    assert_eq!(tail.matches("does not count").count(), 1, "{tail}");
    assert!(tail.contains("rerun attempt 5/5 (counts)"), "{tail}");
}

/// Progress is judged on the attempt that just failed, so it still earns a rerun when the limit of
/// five counted attempts is already used up: only the cap on all reruns stops it then.
#[test]
fn progress_on_the_last_counted_attempt_still_earns_a_free_rerun() {
    let dir = tempfile::tempdir().unwrap();
    // runs 1..=4 trip over a.exe (counted attempts 1..=4, the 4th rerun is the 5th attempt),
    // run 5 trips over a new file: that rerun is free
    let code = lock_gate(
        &dir.path().join("deps"),
        "(['a.exe'] if n < 5 else ['b%d.exe' % n])",
        7,
    );
    let results = run_gates_with_pauses(&[spec("test", &py(&code), 30)], dir.path(), &QUICK);
    let tail = &results[0].tail;
    assert!(results[0].ok, "{tail}");
    assert_eq!(runs_of(dir.path()), 7, "{tail}");
    assert!(!tail.contains("gives up"), "{tail}");
}

#[test]
fn a_new_set_after_repeated_files_is_progress_even_at_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    // a, b, b, b, b, c: the sixth run is the first to differ from its predecessor since the limit
    let code = lock_gate(
        &dir.path().join("deps"),
        "(['a.exe'] if n < 2 else ['b.exe'] if n < 6 else ['c%d.exe' % n])",
        8,
    );
    let results = run_gates_with_pauses(&[spec("test", &py(&code), 30)], dir.path(), &QUICK);
    let tail = &results[0].tail;
    assert!(results[0].ok, "{tail}");
    assert_eq!(runs_of(dir.path()), 8, "{tail}");
}

/// The give-up note must be true: when the last attempt did make progress, the cap is the reason.
#[test]
fn the_give_up_note_names_the_cap_not_the_attempts_when_the_last_attempt_made_progress() {
    let dir = tempfile::tempdir().unwrap();
    let code = lock_gate(
        &dir.path().join("deps"),
        "(['a.exe'] if n < 5 else ['b%d.exe' % n])",
        u32::MAX,
    );
    let results = run_gates_with_pauses(&[spec("test", &py(&code), 30)], dir.path(), &QUICK);
    let tail = &results[0].tail;
    assert!(!results[0].ok);
    assert_eq!(runs_of(dir.path()), 13, "{tail}");
    assert!(tail.contains("cap of 12 reruns"), "{tail}");
    assert!(!tail.contains("without progress"), "{tail}");
}

#[test]
fn the_total_number_of_reruns_is_capped_even_when_every_attempt_reports_different_files() {
    let dir = tempfile::tempdir().unwrap();
    let code = lock_gate(&dir.path().join("deps"), "['f%d.exe' % n]", u32::MAX);
    let results = run_gates_with_pauses(&[spec("test", &py(&code), 30)], dir.path(), &QUICK);
    let tail = &results[0].tail;
    assert!(!results[0].ok);
    assert_eq!(results[0].exit_code, Some(101));
    assert_eq!(
        runs_of(dir.path()),
        13,
        "the first run plus at most 12 reruns: {tail}"
    );
    assert!(tail.contains("12 reruns"), "the stop is explained: {tail}");
}

#[test]
fn the_note_says_which_case_happened_in_plain_words() {
    let dir = tempfile::tempdir().unwrap();
    let code = lock_gate(
        &dir.path().join("deps"),
        "['f%d_%d.exe' % (n, i) for i in range(3)]",
        3,
    );
    let results = run_gates_with_pauses(&[spec("test", &py(&code), 30)], dir.path(), &QUICK);
    let tail = &results[0].tail;
    assert!(results[0].ok, "{tail}");
    assert!(
        tail.contains("first linker lock; rerun attempt 2/5 (counts)"),
        "{tail}"
    );
    assert!(
        tail.contains("linker lock on 3 other file(s); progress, rerun 2 (does not count)"),
        "{tail}"
    );
}

/// Progress is judged by file names; a message without any gives nothing to compare, so it counts.
#[test]
fn a_lock_without_file_names_never_counts_as_progress() {
    let dir = tempfile::tempdir().unwrap();
    let code = "import sys\n\
with open('runs.log', 'a') as f: f.write('x')\n\
print('LINK : fatal error LNK1104: cannot open file foo.exe'); sys.exit(101)";
    let results = run_gates_with_pauses(&[spec("test", &py(code), 30)], dir.path(), &QUICK);
    assert!(!results[0].ok);
    assert_eq!(runs_of(dir.path()), 5, "{}", results[0].tail);
    assert!(!results[0].tail.contains("does not count"), "{}", results[0].tail);
}

#[test]
fn a_failure_without_lnk1104_is_not_rerun_whatever_the_schedule() {
    let dir = tempfile::tempdir().unwrap();
    let code = "import sys\n\
with open('runs.log', 'a') as f: f.write('x')\n\
print('assertion failed'); sys.exit(101)";
    let pauses = LinkerLockPauses {
        moved: &HUGE,
        absent: &HUGE,
    };
    let results = run_gates_with_pauses(&[spec("test", &py(code), 30)], dir.path(), &pauses);
    assert!(!results[0].ok);
    assert_eq!(std::fs::read_to_string(dir.path().join("runs.log")).unwrap(), "x");
    assert!(!results[0].tail.contains("linker lock"), "{}", results[0].tail);
}

mod stages {
    use asf_core::config::{GateCfg, GateWhen};
    use asf_vcs::gates::stage_specs;

    fn gate(name: &str, when: Option<Vec<GateWhen>>) -> GateCfg {
        let mut g = GateCfg {
            name: name.into(),
            cmd: vec!["true".into()],
            ..GateCfg::default()
        };
        if let Some(w) = when {
            g.when = w;
        }
        g
    }

    fn names(specs: &[asf_core::GateSpec]) -> Vec<&str> {
        specs.iter().map(|s| s.name.as_str()).collect()
    }

    #[test]
    fn keeps_only_gates_that_run_in_the_stage_in_config_order() {
        let gates = vec![
            gate("v", Some(vec![GateWhen::Verify])),
            gate("vm", Some(vec![GateWhen::Verify, GateWhen::Merge])),
            gate("m", Some(vec![GateWhen::Merge])),
            gate("main", Some(vec![GateWhen::Main])),
        ];
        assert_eq!(names(&stage_specs(&gates, GateWhen::Verify)), ["v", "vm"]);
        assert_eq!(names(&stage_specs(&gates, GateWhen::Merge)), ["vm", "m"]);
        assert_eq!(names(&stage_specs(&gates, GateWhen::Main)), ["main"]);
    }

    #[test]
    fn a_gate_without_when_runs_in_every_stage() {
        let gates = vec![gate("all", None)];
        for stage in GateWhen::ALL {
            assert_eq!(names(&stage_specs(&gates, stage)), ["all"], "{stage:?}");
        }
    }

    #[test]
    fn carries_cmd_timeout_and_shell_over() {
        let mut g = gate("x", None);
        g.cmd = vec!["echo".into(), "hi".into()];
        g.timeout_s = 7;
        g.shell = Some("sh".into());
        let specs = stage_specs(&[g], GateWhen::Merge);
        assert_eq!(specs[0].cmd, ["echo", "hi"]);
        assert_eq!(specs[0].timeout_s, 7);
        assert_eq!(specs[0].shell.as_deref(), Some("sh"));
    }
}

/// A Polish linker quotes the path with „ ” instead of ASCII quotes; the recovery must still find the
/// file, move it aside and say so (and not "no file path in the linker message").
#[test]
fn moves_the_locked_executable_aside_when_the_linker_quotes_it_with_polish_quotes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("deps")).unwrap();
    let locked = dir.path().join("deps").join("asf-ce36f34fbf868c1a.exe");
    std::fs::write(&locked, "old").unwrap();
    // The message goes out as UTF-8 bytes whatever the console code page is.
    let code = format!(
        "import os, sys; p = {:?}; e = os.path.exists(p)\n\
msg = 'LINK : fatal error LNK1104: nie mo\\u017cna otworzy\\u0107 pliku \\u201e' + p + '\\u201d\\n'\n\
sys.stdout.buffer.write((msg if e else 'tests ok\\n').encode('utf-8'))\n\
sys.exit(101 if e else 0)",
        locked.to_string_lossy().replace('\\', "/")
    );
    let results = run_gates(&[spec("test", &py(&code), 30)], dir.path());
    assert!(results[0].ok, "{}", results[0].tail);
    assert!(!locked.exists(), "locked file moved aside");
    assert!(
        dir.path()
            .join("deps")
            .join("asf-ce36f34fbf868c1a.exe.locked.old")
            .exists(),
        "moved to *.locked.old"
    );
    assert!(results[0].tail.contains("moved aside"), "{}", results[0].tail);
    assert!(
        !results[0].tail.contains("nothing to move aside"),
        "{}",
        results[0].tail
    );
}

/// Failing tests named in a `cargo test` tail, and the source files they live in.
mod failing_tests {
    use std::path::Path;

    use asf_vcs::gates::{failed_tests, foreign_tests, test_source_file, FailedTest, TestBinary};

    const UNIT_TAIL: &str =
        "     Running unittests src\\main.rs (target\\debug\\deps\\asf-ce36f34fbf868c1a.exe)\n\
\n\
running 2 tests\n\
test commands::remind::flow_tests::finished_issues_and_older_runs_do_not_wait ... FAILED\n\
test commands::remind::flow_tests::other ... ok\n\
\n\
failures:\n\
\n\
---- commands::remind::flow_tests::finished_issues_and_older_runs_do_not_wait stdout ----\n\
thread 'x' panicked at crates\\asf\\src\\commands\\remind.rs:10:5:\n\
assertion `left == right` failed\n  left: 1\n right: 2\n\
\n\
failures:\n    commands::remind::flow_tests::finished_issues_and_older_runs_do_not_wait\n\
\n\
test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.10s\n";

    const NAME: &str = "commands::remind::flow_tests::finished_issues_and_older_runs_do_not_wait";

    fn unit(krate: &str) -> Option<TestBinary> {
        Some(TestBinary::Unit { krate: krate.into() })
    }

    fn write(root: &Path, rel: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "// x\n").unwrap();
    }

    fn named(name: &str, binary: Option<TestBinary>) -> FailedTest {
        FailedTest {
            name: name.into(),
            binary,
        }
    }

    #[test]
    fn reads_a_unit_test_failure_and_the_crate_from_the_running_line_above_it() {
        let found = failed_tests(UNIT_TAIL);
        assert_eq!(found, vec![named(NAME, unit("asf"))]);
    }

    #[test]
    fn reads_an_integration_test_by_its_file_in_the_running_line() {
        let tail = "     Running tests\\cli_serve.rs (target\\debug\\deps\\cli_serve-0123456789abcdef.exe)\n\
running 1 test\n\
test serve_stops ... FAILED\n\
\n\
failures:\n\
\n\
---- serve_stops stdout ----\n\
boom\n\
\n\
failures:\n    serve_stops\n\
\n\
test result: FAILED. 0 passed; 1 failed\n";
        assert_eq!(
            failed_tests(tail),
            vec![named(
                "serve_stops",
                Some(TestBinary::Integration {
                    file: "tests/cli_serve.rs".into()
                })
            )]
        );
    }

    /// The gate output is stdout followed by stderr, and cargo prints `Running` on stderr: the line
    /// that names the binary comes after the failures, and the last one is the binary that failed.
    #[test]
    fn a_running_line_after_the_failures_names_their_binary() {
        let tail = "failures:\n    a::b::tests::x\n\ntest result: FAILED. 0 passed; 1 failed\n    Finished `test` profile\n     Running unittests src/lib.rs (target/debug/deps/asf_vcs-0123456789abcdef)\n\
error: test failed, to rerun pass `-p asf-vcs --lib`\n";
        assert_eq!(failed_tests(tail), vec![named("a::b::tests::x", unit("asf-vcs"))]);
    }

    #[test]
    fn lists_each_failed_test_once_in_the_order_seen() {
        let tail = "     Running unittests src/lib.rs (target/debug/deps/asf_core-aa)\n\
---- one stdout ----\nx\n---- two stdout ----\ny\n\nfailures:\n    two\n    one\n\ntest result: FAILED.\n";
        let names: Vec<String> = failed_tests(tail).into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["one", "two"]);
    }

    #[test]
    fn a_doc_test_and_a_tail_without_failures_map_to_nothing() {
        let doc = "   Doc-tests asf_core\n---- src/lib.rs - foo (line 3) stdout ----\nboom\n\nfailures:\n    src/lib.rs - foo (line 3)\n\ntest result: FAILED.\n";
        let found = failed_tests(doc);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].binary, Some(TestBinary::Doc));
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(test_source_file(dir.path(), &found[0]), None);
        assert!(failed_tests("error[E0432]: unresolved import\nerror: could not compile `asf`\n").is_empty());
        assert!(failed_tests("").is_empty());
    }

    #[test]
    fn a_name_without_a_binary_has_no_source_file() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "crates/asf/src/a.rs");
        assert_eq!(test_source_file(dir.path(), &named("a::t", None)), None);
        let tail = "---- a::t stdout ----\nx\n";
        assert_eq!(failed_tests(tail), vec![named("a::t", None)]);
    }

    #[test]
    fn a_module_path_maps_to_the_file_that_holds_it() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "crates/asf/src/commands/remind.rs");
        write(dir.path(), "crates/asf/src/main.rs");
        let t = named(NAME, unit("asf"));
        // `flow_tests` is an inline module of remind.rs: the longest prefix that is a file wins
        assert_eq!(
            test_source_file(dir.path(), &t).as_deref(),
            Some("crates/asf/src/commands/remind.rs")
        );
    }

    #[test]
    fn a_module_directory_maps_to_its_mod_rs() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "crates/asf-core/src/config/mod.rs");
        let t = named("config::tests::parses", unit("asf-core"));
        assert_eq!(
            test_source_file(dir.path(), &t).as_deref(),
            Some("crates/asf-core/src/config/mod.rs")
        );
    }

    #[test]
    fn a_test_in_no_module_file_maps_to_nothing() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "crates/asf-core/src/lib.rs");
        // `tests` is declared in lib.rs itself: the crate root is not guessed at
        let t = named("tests::it_works", unit("asf-core"));
        assert_eq!(test_source_file(dir.path(), &t), None);
        // a crate that is not under crates/ has no file either
        let elsewhere = named("a::t", unit("nope"));
        assert_eq!(test_source_file(dir.path(), &elsewhere), None);
    }

    #[test]
    fn an_integration_test_maps_to_the_one_crate_that_has_the_file() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "crates/asf/tests/cli_serve.rs");
        let t = named(
            "serve_stops",
            Some(TestBinary::Integration {
                file: "tests/cli_serve.rs".into(),
            }),
        );
        assert_eq!(
            test_source_file(dir.path(), &t).as_deref(),
            Some("crates/asf/tests/cli_serve.rs")
        );
        // two crates with the same file name: which one ran is not known, so nothing is claimed
        write(dir.path(), "crates/other/tests/cli_serve.rs");
        assert_eq!(test_source_file(dir.path(), &t), None);
    }

    fn tail_of(names: &[&str], binary: &str) -> String {
        let mut s = format!("     Running {binary}\n");
        for n in names {
            s.push_str(&format!("---- {n} stdout ----\nboom\n"));
        }
        s.push_str("\nfailures:\n");
        for n in names {
            s.push_str(&format!("    {n}\n"));
        }
        s.push_str("\ntest result: FAILED.\n");
        s
    }

    const BIN: &str = "unittests src\\main.rs (target\\debug\\deps\\asf-ce36.exe)";

    #[test]
    fn a_failure_is_foreign_when_no_failing_test_lives_in_a_changed_file() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "crates/asf/src/commands/remind.rs");
        let tail = tail_of(&[NAME], BIN);
        let changed = vec!["crates/asf/src/commands/merge.rs".to_string()];
        let foreign = foreign_tests(dir.path(), &tail, &changed).expect("foreign");
        assert_eq!(foreign.len(), 1);
        assert_eq!(foreign[0].name, NAME);
        assert_eq!(foreign[0].file, "crates/asf/src/commands/remind.rs");
    }

    #[test]
    fn a_failure_is_the_branchs_when_any_failing_test_lives_in_a_changed_file() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "crates/asf/src/commands/remind.rs");
        write(dir.path(), "crates/asf/src/commands/merge.rs");
        let tail = tail_of(&[NAME, "commands::merge::tests::mine"], BIN);
        let changed = vec!["crates/asf/src/commands/merge.rs".to_string()];
        assert_eq!(foreign_tests(dir.path(), &tail, &changed), None);
    }

    #[test]
    fn an_unmapped_name_or_a_tail_without_failures_is_never_foreign() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "crates/asf/src/commands/remind.rs");
        let changed = vec!["README.md".to_string()];
        // the file for the second name does not exist: unknown counts as the branch's
        let tail = tail_of(&[NAME, "ghost::tests::x"], BIN);
        assert_eq!(foreign_tests(dir.path(), &tail, &changed), None);
        assert_eq!(
            foreign_tests(dir.path(), "error: could not compile\n", &changed),
            None
        );
        assert_eq!(foreign_tests(dir.path(), "", &changed), None);
    }
}
