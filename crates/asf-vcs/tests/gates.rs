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
