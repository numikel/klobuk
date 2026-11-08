use std::collections::{BTreeMap, BTreeSet};

use asf_core::config::NotifyCfg;
use asf_core::remind::{
    alerts, due as due_after_alerts, first_line, human_age, reminder_text, EventIndex, Reminder, WaitKind,
    Waiting, EVENT_REMINDED,
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
        key: format!("{kind}:{n}@{minutes_ago}"),
    }
}

fn cfg(after: u64, every: u64) -> NotifyCfg {
    NotifyCfg {
        after_minutes: after,
        every_minutes: every,
        command: Vec::new(),
        on_new: true,
    }
}

fn reminded(pairs: &[(u32, i64)]) -> BTreeMap<IssueId, String> {
    pairs
        .iter()
        .map(|(n, ago)| (id(*n), ts(t0() - Duration::minutes(*ago))))
        .collect()
}

/// `due` for the world without alerts: the old tests say nothing about them.
fn due(
    waiting: &[Waiting],
    reminded: &BTreeMap<IssueId, String>,
    now: OffsetDateTime,
    cfg: &NotifyCfg,
) -> Vec<Reminder> {
    due_after_alerts(waiting, reminded, &BTreeMap::new(), now, cfg)
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
    assert_eq!(WaitKind::ALL.len(), 9);
    assert_eq!(serde_json::to_string(&WaitKind::Blocker).unwrap(), "\"blocker\"");
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

// ---- alerts: telling the human the moment something new waits ----

fn none() -> BTreeSet<(IssueId, String)> {
    BTreeSet::new()
}

fn announced(w: &Waiting) -> BTreeSet<(IssueId, String)> {
    [(w.issue.clone(), w.key.clone())].into_iter().collect()
}

#[test]
fn a_new_wait_gets_one_alert_with_a_title_for_a_human_in_a_hurry() {
    let cases = [
        (WaitKind::Question, "asf asf-7 asks"),
        (WaitKind::Blocker, "asf asf-7 is blocked"),
        (WaitKind::IssueBlocked, "asf asf-7 is blocked"),
        (WaitKind::RunBlocked, "asf asf-7 is blocked"),
        (WaitKind::MergeFailed, "asf asf-7 merge failed"),
        (WaitKind::MergeBlocked, "asf asf-7 merge blocked"),
        (WaitKind::RunStalled, "asf asf-7 is stalled"),
        (WaitKind::RunOverBudget, "asf asf-7 is over budget"),
        (WaitKind::RunFailed, "asf asf-7 run failed"),
    ];
    assert_eq!(cases.len(), WaitKind::ALL.len(), "every kind has a title");
    for (kind, title) in cases {
        let a = alerts(&[waiting(7, kind, 0, "Which branch?")], &none());
        assert_eq!(a.len(), 1, "{kind}");
        assert_eq!(a[0].issue, id(7));
        assert_eq!(a[0].kind, kind);
        assert_eq!(a[0].title, title);
        assert_eq!(a[0].body, "Which branch?");
        assert_eq!(a[0].text, format!("{title}: Which branch?"));
    }
}

#[test]
fn the_body_is_one_short_line_and_never_empty() {
    let long = format!("rebase conflict in a.rs\nstack\n{}", "x".repeat(300));
    let a = alerts(&[waiting(1, WaitKind::MergeFailed, 0, &long)], &none());
    assert_eq!(a[0].body, "rebase conflict in a.rs");

    let a = alerts(&[waiting(1, WaitKind::MergeFailed, 0, &"y".repeat(300))], &none());
    assert_eq!(a[0].body.chars().count(), 120, "cut like a reminder reason");

    for kind in WaitKind::ALL {
        let a = alerts(&[waiting(1, *kind, 0, "  \n ")], &none());
        assert!(!a[0].body.trim().is_empty(), "{kind} has a stock body");
        assert!(!a[0].body.contains('\n'));
    }
}

#[test]
fn a_wait_that_was_announced_is_not_announced_again() {
    let w = waiting(1, WaitKind::Question, 5, "q");
    assert!(alerts(std::slice::from_ref(&w), &announced(&w)).is_empty());

    // the next episode has another key: a new question on the same issue is news again
    let next = waiting(1, WaitKind::Question, 1, "another");
    let a = alerts(&[w.clone(), next.clone()], &announced(&w));
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].body, "another");
    assert_eq!(a[0].keys, [next.key]);
}

#[test]
fn a_key_announced_for_another_issue_does_not_count() {
    let w = waiting(1, WaitKind::Question, 5, "q");
    let mut other = w.clone();
    other.issue = id(2);
    assert_eq!(alerts(&[other], &announced(&w)).len(), 1);
}

#[test]
fn several_new_waits_of_one_issue_make_one_alert_that_covers_them_all() {
    let w = [
        waiting(1, WaitKind::IssueBlocked, 3, "gate failed"),
        waiting(1, WaitKind::MergeFailed, 3, "rebase conflict"),
        waiting(1, WaitKind::RunFailed, 3, "verification failed"),
        waiting(1, WaitKind::Blocker, 3, "Verification of asf-1 failed"),
        waiting(2, WaitKind::RunStalled, 1, "no output"),
    ];
    let a = alerts(&w, &none());
    assert_eq!(a.len(), 2, "{a:?}");
    assert_eq!(a[0].issue, id(1));
    assert_eq!(a[0].kind, WaitKind::Blocker, "what a person wrote leads");
    assert_eq!(a[0].keys.len(), 4, "all four are announced by the one alert");
    assert_eq!(a[1].issue, id(2));
}

#[test]
fn a_question_leads_and_among_equals_the_newest_wins() {
    let w = [
        waiting(1, WaitKind::RunFailed, 10, "run"),
        waiting(1, WaitKind::Question, 9, "older question"),
        waiting(1, WaitKind::Question, 2, "newer question"),
    ];
    let a = alerts(&w, &none());
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].kind, WaitKind::Question);
    assert_eq!(a[0].body, "newer question");
}

#[test]
fn the_event_index_remembers_which_waits_an_alert_announced() {
    let mut ix = EventIndex::default();
    ix.absorb(&event(
        EVENT_REMINDED,
        Some(1),
        None,
        "2026-11-07T09:00:00Z",
        serde_json::json!({"kind": "question", "age_minutes": 0, "keys": ["question:5", "run:r1:failed@x"], "immediate": true}),
    ));
    // an ordinary reminder carries no keys and announces nothing
    ix.absorb(&event(
        EVENT_REMINDED,
        Some(2),
        None,
        "2026-11-07T09:05:00Z",
        serde_json::json!({"kind": "issue_blocked", "age_minutes": 20}),
    ));
    assert_eq!(ix.announced.len(), 2);
    assert!(ix.announced.contains(&(id(1), "question:5".to_string())));
    assert!(ix.announced.contains(&(id(1), "run:r1:failed@x".to_string())));
    assert_eq!(
        ix.alerted[&id(1)],
        "2026-11-07T09:00:00Z",
        "an alert is kept apart from the reminders"
    );
    assert!(
        !ix.reminded.contains_key(&id(1)),
        "an alert is not a reminder: it must not start the every_minutes countdown"
    );
    assert_eq!(ix.reminded[&id(2)], "2026-11-07T09:05:00Z");
    assert!(!ix.alerted.contains_key(&id(2)));
}

/// An index that saw one alert for issue 1, `minutes_ago` before `t0()`.
fn alerted_ago(minutes_ago: i64) -> EventIndex {
    let mut ix = EventIndex::default();
    ix.absorb(&event(
        EVENT_REMINDED,
        Some(1),
        None,
        &ts(t0() - Duration::minutes(minutes_ago)),
        serde_json::json!({"kind": "issue_blocked", "age_minutes": 0, "keys": ["k"], "immediate": true}),
    ));
    ix
}

fn due_at(w: &[Waiting], ix: &EventIndex, minutes_after_t0: i64, c: &NotifyCfg) -> usize {
    due_after_alerts(
        w,
        &ix.reminded,
        &ix.alerted,
        t0() + Duration::minutes(minutes_after_t0),
        c,
    )
    .len()
}

#[test]
fn the_first_reminder_is_counted_from_the_alert_by_after_minutes() {
    // the wait is 40 min old; the alert went out 10 min ago: the reminder comes after_minutes
    // (15) after the alert, not every_minutes (30)
    let ix = alerted_ago(10);
    let w = [waiting(1, WaitKind::IssueBlocked, 40, "x")];
    let c = cfg(15, 30);
    assert_eq!(due_at(&w, &ix, 0, &c), 0, "10 min after the alert");
    assert_eq!(due_at(&w, &ix, 4, &c), 0, "14 min after the alert");
    assert_eq!(due_at(&w, &ix, 5, &c), 1, "15 min after the alert");
}

#[test]
fn an_alert_does_not_postpone_a_reminder_that_is_due_long_after_it() {
    // alerted 20 min ago: the first reminder is due now, whatever every_minutes says
    let ix = alerted_ago(20);
    let w = [waiting(1, WaitKind::IssueBlocked, 40, "x")];
    assert_eq!(due_at(&w, &ix, 0, &cfg(15, 30)), 1);
    assert_eq!(
        due_at(&w, &ix, 0, &cfg(15, 120)),
        1,
        "every_minutes is for the repeats"
    );
}

#[test]
fn zero_interval_still_sends_the_one_reminder_after_an_alert() {
    let ix = alerted_ago(10);
    let w = [waiting(1, WaitKind::IssueBlocked, 40, "x")];
    let c = cfg(15, 0);
    assert_eq!(due_at(&w, &ix, 0, &c), 0, "too soon after the alert");
    assert_eq!(
        due_at(&w, &ix, 5, &c),
        1,
        "the once, after_minutes after the alert"
    );

    // once that reminder is out, "once" means once
    let mut ix = ix;
    ix.absorb(&event(
        EVENT_REMINDED,
        Some(1),
        None,
        &ts(t0() + Duration::minutes(5)),
        serde_json::json!({"kind": "issue_blocked", "age_minutes": 45}),
    ));
    assert_eq!(due_at(&w, &ix, 600, &c), 0);
}

#[test]
fn repeats_count_from_the_last_reminder_not_from_the_alert() {
    let mut ix = alerted_ago(60);
    ix.absorb(&event(
        EVENT_REMINDED,
        Some(1),
        None,
        &ts(t0() - Duration::minutes(10)),
        serde_json::json!({"kind": "issue_blocked", "age_minutes": 50}),
    ));
    let w = [waiting(1, WaitKind::IssueBlocked, 70, "x")];
    let c = cfg(15, 30);
    assert_eq!(due_at(&w, &ix, 0, &c), 0, "reminded 10 min ago");
    assert_eq!(due_at(&w, &ix, 19, &c), 0, "29 min after the reminder");
    assert_eq!(due_at(&w, &ix, 20, &c), 1, "30 min after the reminder");
}

#[test]
fn an_alert_from_an_earlier_wait_does_not_delay_a_new_one() {
    // alerted 100 min ago, the current wait began 20 min ago: the alert belongs to another episode
    let ix = alerted_ago(100);
    let w = [waiting(1, WaitKind::IssueBlocked, 20, "x")];
    assert_eq!(due_at(&w, &ix, 0, &cfg(15, 30)), 1);
}

#[test]
fn an_alert_for_a_wait_that_has_not_yet_reached_the_delay_keeps_it_quiet() {
    // alerted the moment the wait began (5 min ago): nothing is due until 15 min in
    let ix = alerted_ago(5);
    let w = [waiting(1, WaitKind::IssueBlocked, 5, "x")];
    assert_eq!(due_at(&w, &ix, 0, &cfg(15, 30)), 0);
    assert_eq!(due_at(&w, &ix, 10, &cfg(15, 30)), 1);
}

#[test]
fn the_index_knows_when_an_issue_or_a_merge_item_entered_a_state() {
    let mut ix = EventIndex::default();
    ix.absorb(&event(
        "issue.status",
        Some(3),
        None,
        "2026-11-07T08:00:00Z",
        serde_json::json!({"from": "in_progress", "to": "blocked"}),
    ));
    ix.absorb(&event(
        "issue.status",
        Some(3),
        None,
        "2026-11-07T08:30:00Z",
        serde_json::json!({"from": "blocked", "to": "in_progress"}),
    ));
    ix.absorb(&event(
        "issue.status",
        Some(3),
        None,
        "2026-11-07T09:00:00Z",
        serde_json::json!({"from": "in_progress", "to": "blocked"}),
    ));
    ix.absorb(&event(
        "merge.state",
        Some(3),
        None,
        "2026-11-07T09:10:00Z",
        serde_json::json!({"merge_id": 12, "from": "queued", "to": "failed"}),
    ));
    ix.absorb(&event(
        "run.state",
        Some(3),
        Some("r9"),
        "2026-11-07T09:20:00Z",
        serde_json::json!({"from": "working", "to": "stalled"}),
    ));
    assert_eq!(
        ix.issue_entered(&id(3), "blocked"),
        Some("2026-11-07T09:00:00Z"),
        "the latest entry"
    );
    assert_eq!(ix.issue_entered(&id(4), "blocked"), None);
    assert_eq!(ix.merge_entered(12, "failed"), Some("2026-11-07T09:10:00Z"));
    assert_eq!(ix.merge_entered(13, "failed"), None);
    assert_eq!(ix.run_entered("r9", "stalled"), Some("2026-11-07T09:20:00Z"));
}

#[test]
fn a_blocker_comment_is_announced_but_never_reminded() {
    let w = [waiting(1, WaitKind::Blocker, 600, "scope too narrow")];
    assert!(due(&w, &BTreeMap::new(), t0(), &cfg(15, 30)).is_empty());
    assert_eq!(alerts(&w, &BTreeSet::new()).len(), 1);

    // and it does not hide the real wait of its issue from the reminders
    let w = [
        waiting(1, WaitKind::Blocker, 600, "scope too narrow"),
        waiting(1, WaitKind::IssueBlocked, 40, "x"),
    ];
    let r = due(&w, &BTreeMap::new(), t0(), &cfg(15, 30));
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].kind, WaitKind::IssueBlocked);
    assert_eq!(r[0].age_minutes, 40);
}
