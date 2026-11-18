use assert_cmd::Command;

/// `klobuk <version> (<7+ hex sha> | unknown)`, one line.
fn assert_shape(out: &str) {
    let line = out.trim_end();
    let rest = line
        .strip_prefix(concat!("klobuk ", env!("CARGO_PKG_VERSION"), " ("))
        .and_then(|r| r.strip_suffix(')'))
        .unwrap_or_else(|| panic!("unexpected version line: {line:?}"));
    let sha_ok = rest == "unknown" || (rest.len() >= 7 && rest.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(sha_ok, "bad sha {rest:?} in {line:?}");
    assert_eq!(out.lines().count(), 1, "expected a single line: {out:?}");
}

fn version_output(flag: &str) -> String {
    let out = Command::cargo_bin("klobuk")
        .unwrap()
        .arg(flag)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(out).unwrap()
}

#[test]
fn long_flag_prints_version_and_sha() {
    assert_shape(&version_output("--version"));
}

#[test]
fn short_flag_matches_long_flag() {
    assert_eq!(version_output("-V"), version_output("--version"));
    assert_shape(&version_output("-V"));
}

#[test]
fn shape_check_rejects_malformed_lines() {
    for bad in [
        "klobuk 0.0.0 (abc1234)\n",
        "klobuk (abc1234)\n",
        "klobuk 0.1.0 (abc12)\n",
        "klobuk 0.1.0 (zzzzzzz)\n",
    ] {
        assert!(
            std::panic::catch_unwind(|| assert_shape(bad)).is_err(),
            "{bad:?} passed"
        );
    }
}
