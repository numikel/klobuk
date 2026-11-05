use asf_core::GateSpec;
use asf_vcs::run_gates;

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
    let specs = vec![spec("slow", &py("import time; time.sleep(30)"), 1)];
    let start = std::time::Instant::now();
    let results = run_gates(&specs, dir.path());
    assert!(start.elapsed().as_secs() < 10, "killed well before 30 s");
    assert!(!results[0].ok);
    assert_eq!(results[0].exit_code, None, "killed => no exit code");
    assert!(results[0].tail.contains("timeout"), "{}", results[0].tail);
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
    let start = std::time::Instant::now();
    let results = run_gates(&[spec("test", &py(code), 30)], dir.path());
    assert!(results[0].ok, "{}", results[0].tail);
    assert_eq!(results[0].exit_code, Some(0));
    assert!(results[0].tail.contains("tests ok"), "{}", results[0].tail);
    assert!(
        results[0].tail.contains("LNK1104"),
        "the retry is visible: {}",
        results[0].tail
    );
    assert!(results[0].tail.contains("attempt 2"), "{}", results[0].tail);
    assert!(start.elapsed().as_secs() >= 2, "waits before the rerun");
}

#[test]
fn gives_up_on_a_permanent_linker_lock_after_five_attempts() {
    let dir = tempfile::tempdir().unwrap();
    let code = "import sys\n\
with open('runs.log', 'a') as f: f.write('x')\n\
print('LINK : fatal error LNK1104: cannot open file foo.exe'); sys.exit(101)";
    let results = run_gates(&[spec("test", &py(code), 30)], dir.path());
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
