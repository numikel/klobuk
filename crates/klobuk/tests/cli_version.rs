use assert_cmd::Command;

// The build script itself, so its source picking is tested without building a package.
#[path = "../build.rs"]
#[allow(dead_code)]
mod build_script;
use build_script::{resolve_sha, sha_from_vcs_info};

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

const FULL_SHA: &str = "aac20b6e7e543e6dd4118b246c77225e3a3a1302";
/// Exactly what `cargo package` writes (cargo-package(1), ".cargo_vcs_info.json format").
const VCS_INFO_DOC: &str = "{\n \"git\": {\n   \"sha1\": \"aac20b6e7e543e6dd4118b246c77225e3a3a1302\",\n   \"dirty\": true\n },\n \"path_in_vcs\": \"crates/klobuk\"\n}";

fn no_git() -> Option<String> {
    panic!("git must not be asked when the vcs info settles the commit");
}

#[test]
fn vcs_info_gives_the_short_commit() {
    assert_eq!(sha_from_vcs_info(VCS_INFO_DOC).as_deref(), Some("aac20b6"));
    // Compact form, no dirty flag, other keys before and after.
    let compact = format!(r#"{{"path_in_vcs":"","git":{{"sha1":"{FULL_SHA}"}}}}"#);
    assert_eq!(sha_from_vcs_info(&compact).as_deref(), Some("aac20b6"));
}

#[test]
fn vcs_info_that_does_not_parse_gives_nothing() {
    let bad = [
        "",
        "{",
        "not json at all",
        r#"{"path_in_vcs":""}"#,
        r#"{"git":{}}"#,
        r#"{"git":{"sha1":"#,
        r#"{"git":{"sha1":"aac20b6"#,
        r#"{"git":{"sha1":aac20b6e7e5}}"#,
        r#"{"git":{"sha1":"aac20b"}}"#,
        r#"{"git":{"sha1":"zzzzzzzzzzzz"}}"#,
        r#"{"git":{"sha1":""}}"#,
        r#"{"other":{"sha1":"aac20b6e7e543e6d"}}"#,
        "{\"git\":{\"sha1\":\"\u{142}\u{142}\u{142}\u{142}\"}}",
    ];
    for json in bad {
        assert_eq!(sha_from_vcs_info(json), None, "{json:?}");
    }
}

#[test]
fn packaged_build_uses_the_vcs_info_and_never_asks_git() {
    assert_eq!(resolve_sha(Some(VCS_INFO_DOC), no_git), "aac20b6");
}

#[test]
fn checkout_without_the_file_uses_git() {
    assert_eq!(resolve_sha(None, || Some("1234abc".into())), "1234abc");
}

#[test]
fn unusable_file_falls_back_to_git() {
    assert_eq!(resolve_sha(Some("{broken"), || Some("1234abc".into())), "1234abc");
}

#[test]
fn neither_source_prints_unknown() {
    assert_eq!(resolve_sha(None, || None), "unknown");
    assert_eq!(resolve_sha(Some("{broken"), || None), "unknown");
}

#[test]
fn shape_check_rejects_malformed_lines() {
    // The last two carry the real version, so only the sha part can make them fail.
    let bad_lines = [
        "klobuk 0.0.0 (abc1234)\n".to_string(),
        "klobuk (abc1234)\n".to_string(),
        concat!("klobuk ", env!("CARGO_PKG_VERSION"), " (abc12)\n").to_string(),
        concat!("klobuk ", env!("CARGO_PKG_VERSION"), " (zzzzzzz)\n").to_string(),
    ];
    for bad in &bad_lines {
        assert!(
            std::panic::catch_unwind(|| assert_shape(bad)).is_err(),
            "{bad:?} passed"
        );
    }
}
