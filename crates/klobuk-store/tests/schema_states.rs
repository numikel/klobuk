//! `RunState::ALL` and `MergeState::ALL` must list exactly the values of the `CHECK (... IN (...))`
//! constraints in `schema.sql`. The schema belongs to this crate, so the check lives here and
//! `klobuk-core` never reads a file of a sibling crate.

use klobuk_core::{MergeState, RunState};

const SCHEMA: &str = include_str!("../src/schema.sql");

/// Values of the `CHECK (col IN (...))` list that follows the first occurrence of `marker`.
fn check_list(marker: &str) -> Vec<String> {
    let idx = SCHEMA.find(marker).expect("marker in schema");
    let rest = &SCHEMA[idx..];
    let start = rest.find("IN (").expect("CHECK IN") + 4;
    let end = rest[start..].find(')').unwrap() + start;
    rest[start..end]
        .split(',')
        .map(|s| s.trim().trim_matches('\'').to_string())
        .collect()
}

#[test]
fn run_state_strings_match_schema() {
    let ours: Vec<String> = RunState::ALL.iter().map(|s| s.to_string()).collect();
    assert_eq!(ours, check_list("CREATE TABLE runs"));
}

#[test]
fn merge_state_strings_match_schema() {
    let ours: Vec<String> = MergeState::ALL.iter().map(|s| s.to_string()).collect();
    assert_eq!(ours, check_list("CREATE TABLE merge_queue"));
}
