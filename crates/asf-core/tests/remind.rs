use std::collections::BTreeMap;

use asf_core::config::NotifyCfg;
use asf_core::remind::{
    due, first_line, human_age, reminder_text, EventIndex, WaitKind, Waiting, EVENT_REMINDED,
};
use asf_core::{Event, IssueId};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

fn id(n: u32) -> IssueId {
    format!("asf-{n}").parse().unwrap()
}

fn t0() -> OffsetDateTime {
    OffsetDateTime::parse("2026-11-07T10:00:00Z", &Rfc3339).unwrap()
}

fn ts(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap()
}

/// `since` is how many minutes before `t0()` the wait began.
fn waiting(n: u32, kind: WaitKind, minutes_ago: i64, reason: &str) -> Waiting {
    Waiting {
        issue: id(n),
        kind,
        since: ts(t0() - Duration::minutes(minutes_ago)),
        reason: reason.into(),
    }
}

fn cfg(after: u64, every: u64) -> NotifyCfg {
    NotifyCfg {
        remind_after_minutes: after,
        remind_every_minutes: every,
        command: Vec::new(),
    }
}

fn reminded(pairs: &[(u32, i64)]) -> BTreeMap<IssueId, String> {
    pairs
        .iter()
        .map(|(n, ago)| (id(*n), ts(t0() - Duration::minutes(*ago))))
        .collect()
}

fn issues(r: &[asf_core::remind::Reminder]) -> Vec<String> {
    r.iter().map(|r| r.issue.to_string()).collect()
}

#[test]
fn nothing_is_due_before_the_first_delay() {
    let w = [waiting(1, WaitKind::IssueBlocked, 14, "x")];
    assert!(due(&w, &BTreeMap::new(), t0(), &cfg(15, 30)).is_empty());
}

#[test]
fn the_first_reminder_is_due_exactly_at_the_delay() {
    let w = [waiting(1, WaitKind::IssueBlocked, 15, "gate test timed out")];
    let r = due(&w, &BTreeMap::new(), t0(), &cfg(15, 30));
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].issue, id(1));
    assert_eq!(r[0].kind, WaitKind::IssueBlocked);
    assert_eq!(r[0].age_minutes, 15);
    assert_eq!(
        r[0].text,
        "asf-1 has been blocked for 15 min: gate test timed out"
    );
}

#[test]
fn a_reminder_repeats_only_after_the_interval() {
    let w = [waiting(1, WaitKind::IssueBlocked, 60, "")];
    let c = cfg(15, 30);
    assert!(due(&w, &reminded(&[(1, 29)]), t0(), &c).is_empty(), "29 min ago");
    assert_eq!(due(&w, &reminded(&[(1, 30)]), t0(), &c).len(), 1, "30 min ago");
    assert_eq!(due(&w, &reminded(&[(1, 45)]), t0(), &c).len(), 1, "long ago");
}

#[test]
fn zero_delay_switches_reminders_off() {
    let w = [waiting(1, WaitKind::IssueBlocked, 600, "x")];
    assert!(due(&w, &BTreeMap::new(), t0(), &cfg(0, 30)).is_empty());
}

#[test]
fn zero_interval_reminds_once_per_wait() {
    let w = [waiting(1, WaitKind::IssueBlocked, 600, "x")];
    let c = cfg(15, 0);
    assert_eq!(due(&w, &BTreeMap::new(), t0(), &c).len(), 1);
    assert!(due(&w, &reminded(&[(1, 500)]), t0(), &c).is_empty());
}

#[test]
fn a_reminder_from_an_earlier_wait_does_not_count() {
    // reminded 100 min ago, but the current wait began 20 min ago: a new episode
    let w = [waiting(1, WaitKind::IssueBlocked, 20, "x")];
    let r = due(&w, &reminded(&[(1, 100)]), t0(), &cfg(15, 30));
    assert_eq!(
        r.len(),
        1,
        "the old reminder must not postpone the first one of this wait"
    );
    // the same with a reminder made 5 min ago, i.e. inside this wait
    assert!(due(&w, &reminded(&[(1, 5)]), t0(), &cfg(15, 30)).is_empty());
}

#[test]
fn one_reminder_per_issue_for_its_oldest_wait() {
    let w = [
        waiting(1, WaitKind::MergeFailed, 20, "rebase conflict"),
        waiting(1, WaitKind::RunFailed, 50, "verification failed"),
        waiting(1, WaitKind::Question, 30, "which branch?"),
    ];
    let r = due(&w, &BTreeMap::new(), t0(), &cfg(15, 30));
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].kind, WaitKind::RunFailed);
    assert_eq!(r[0].age_minutes, 50);
    assert!(r[0].text.contains("verification failed"), "{}", r[0].text);
}

#[test]
fn equal_ages_pick_the_kind_that_sorts_first() {
    let w = [
        waiting(1, WaitKind::Question, 40, "q"),
        waiting(1, WaitKind::IssueBlocked, 40, "b"),
    ];
    let r = due(&w, &BTreeMap::new(), t0(), &cfg(15, 30));
    assert_eq!(r[0].kind, WaitKind::IssueBlocked);
}

#[test]
fn issues_are_independent_and_come_back_oldest_first() {
    let w = [
        waiting(3, WaitKind::Question, 20, "c"),
        waiting(1, WaitKind::IssueBlocked, 90, "a"),
        waiting(2, WaitKind::RunStalled, 45, "b"),
        waiting(4, WaitKind::IssueBlocked, 5, "too fresh"),
    ];
    let r = due(&w, &reminded(&[(2, 10)]), t0(), &cfg(15, 30));
    assert_eq!(
        issues(&r),
        ["asf-1", "asf-3"],
        "asf-2 was reminded 10 min ago, asf-4 is too fresh"
    );
}

#[test]
fn a_wait_with_an_unreadable_time_is_ignored() {
    let mut w = waiting(1, WaitKind::IssueBlocked, 60, "x");
    w.since = "yesterday".into();
    assert!(due(&[w], &BTreeMap::new(), t0(), &cfg(15, 30)).is_empty());
}

#[test]
fn a_wait_that_starts_in_the_future_is_not_due() {
    let w = [waiting(1, WaitKind::IssueBlocked, -10, "clock skew")];
    assert!(due(&w, &BTreeMap::new(), t0(), &cfg(1, 30)).is_empty());
}

#[test]
fn texts_say_what_waits_for_how_long_and_why() {
    let cases = [
        (WaitKind::IssueBlocked, "asf-7 has been blocked for 45 min: why"),
        (
            WaitKind::MergeFailed,
            "asf-7 has had a failed merge for 45 min: why",
        ),
        (
            WaitKind::MergeBlocked,
            "asf-7 has had a blocked merge for 45 min: why",
        ),
        (
            WaitKind::RunBlocked,
            "asf-7 has had a worker waiting for input for 45 min: why",
        ),
        (
            WaitKind::RunStalled,
            "asf-7 has had a stalled worker for 45 min: why",
        ),
        (
            WaitKind::RunOverBudget,
            "asf-7 has had a worker over budget for 45 min: why",
        ),
        (WaitKind::RunFailed, "asf-7 has had a failed run for 45 min: why"),
        (
            WaitKind::Question,
            "asf-7 has had an unanswered question for 45 min: why",
        ),
    ];
    for (kind, expect) in cases {
        assert_eq!(reminder_text(&id(7), kind, 45, "why"), expect);
    }
    assert_eq!(
        reminder_text(&id(7), WaitKind::IssueBlocked, 45, "  "),
        "asf-7 has been blocked for 45 min",
        "no dangling colon without a reason"
    );
}

#[test]
fn wait_kinds_round_trip_through_their_names() {
    for k in WaitKind::ALL {
        assert_eq!(k.to_string().parse::<WaitKind>().unwrap(), *k);
    }
    assert_eq!(
        serde_json::to_string(&WaitKind::RunOverBudget).unwrap(),
        "\"run_over_budget\""
    );
    assert_eq!(WaitKind::ALL.len(), 8);
}

#[test]
fn ages_read_as_minutes_then_hours() {
    assert_eq!(human_age(0), "0 min");
    assert_eq!(human_age(45), "45 min");
    assert_eq!(human_age(60), "1 h");
    assert_eq!(human_age(125), "2 h 5 min");
}

#[test]
fn first_line_keeps_one_short_line() {
    assert_eq!(
        first_line("\n\n  Verification of asf-27 failed:\nmore"),
        "Verification of asf-27 failed:"
    );
    assert_eq!(first_line("a   b\tc"), "a b c");
    assert_eq!(first_line(""), "");
    let long = "x".repeat(300);
    let cut = first_line(&long);
    assert_eq!(cut.chars().count(), 120);
    assert!(cut.ends_with("..."));
    let polish = "ą".repeat(200);
    assert_eq!(
        first_line(&polish).chars().count(),
        120,
        "cuts on characters, not bytes"
    );
}

fn event(kind: &str, issue: Option<u32>, run: Option<&str>, ts_: &str, payload: serde_json::Value) -> Event {
    Event {
        seq: 1,
        ts: ts_.into(),
        kind: kind.into(),
        issue_id: issue.map(id),
        run_id: run.map(str::to_string),
        actor: None,
        payload,
    }
}

#[test]
fn the_event_index_keeps_the_latest_reminder_and_run_state_change() {
    let mut ix = EventIndex::default();
    let none = serde_json::json!({});
    ix.absorb(&event(
        EVENT_REMINDED,
        Some(1),
        None,
        "2026-11-07T09:00:00Z",
        none.clone(),
    ));
    ix.absorb(&event(
        EVENT_REMINDED,
        Some(1),
        None,
        "2026-11-07T09:30:00Z",
        none.clone(),
    ));
    ix.absorb(&event(
        EVENT_REMINDED,
        Some(2),
        None,
        "2026-11-07T09:10:00Z",
        none.clone(),
    ));
    ix.absorb(&event(
        "run.state",
        Some(1),
        Some("r1"),
        "2026-11-07T08:00:00Z",
        serde_json::json!({"from":"working","to":"blocked"}),
    ));
    ix.absorb(&event(
        "run.state",
        Some(1),
        Some("r1"),
        "2026-11-07T08:20:00Z",
        serde_json::json!({"from":"blocked","to":"stalled"}),
    ));
    ix.absorb(&event(
        "issue.status",
        Some(1),
        None,
        "2026-11-07T09:59:00Z",
        none,
    ));

    assert_eq!(ix.reminded[&id(1)], "2026-11-07T09:30:00Z");
    assert_eq!(ix.reminded[&id(2)], "2026-11-07T09:10:00Z");
    assert_eq!(ix.reminded.len(), 2, "other event kinds are ignored");
    assert_eq!(ix.run_entered("r1", "stalled"), Some("2026-11-07T08:20:00Z"));
    assert_eq!(ix.run_entered("r1", "blocked"), None, "the run left that state");
    assert_eq!(ix.run_entered("r2", "stalled"), None);
}

#[test]
fn the_index_feeds_due_so_a_restart_does_not_repeat_early() {
    let mut ix = EventIndex::default();
    ix.absorb(&event(
        EVENT_REMINDED,
        Some(1),
        None,
        &ts(t0() - Duration::minutes(10)),
        serde_json::json!({}),
    ));
    let w = [waiting(1, WaitKind::IssueBlocked, 40, "x")];
    assert!(due(&w, &ix.reminded, t0(), &cfg(15, 30)).is_empty());
    assert_eq!(
        due(&w, &ix.reminded, t0() + Duration::minutes(20), &cfg(15, 30)).len(),
        1,
        "30 min after the last reminder"
    );
}
