//! `remind::waits`: the one definition of what waits for the human, shared by `klobuk serve` and the TUI.

use klobuk_core::remind::{screen_line, waits, Act, EventIndex, Wait, WaitInput, WaitKind};
use klobuk_core::*;

fn id(n: u32) -> IssueId {
    format!("asf-{n}").parse().unwrap()
}

fn issue(n: u32, status: Status) -> Issue {
    Issue {
        id: id(n),
        uid: format!("u{n}"),
        title: format!("Title of asf-{n}"),
        body: String::new(),
        kind: IssueType::Task,
        status,
        priority: 2,
        parent_id: None,
        file_scope: vec![],
        acceptance: vec![],
        estimate: None,
        labels: vec![],
        source: None,
        created_at: "2026-11-02T10:00:00Z".into(),
        updated_at: "2026-11-02T10:00:00Z".into(),
        closed_at: None,
    }
}

fn run(n: u32, attempt: i64, state: RunState, finished: &str) -> Run {
    Run {
        id: format!("run-{n}-{attempt}"),
        issue_id: id(n),
        attempt,
        runtime: "fake".into(),
        agent_name: None,
        workspace_id: None,
        pane_id: None,
        claude_session_id: None,
        transcript_path: None,
        worktree: format!(".worktrees/asf-{n}"),
        branch: format!("feat/asf-{n}"),
        base_sha: "base".into(),
        state,
        budget_usd: None,
        cost_usd: 0.0,
        input_tokens: 0,
        output_tokens: 0,
        turns: 0,
        tool_calls: 0,
        exit_reason: None,
        report_path: None,
        started_at: "2026-11-06T08:00:00Z".into(),
        last_seen_at: None,
        finished_at: (!finished.is_empty()).then(|| finished.to_string()),
        herdr_revision: None,
    }
}

fn merge(n: i64, issue: u32, state: MergeState, error: Option<&str>, at: &str) -> MergeItem {
    MergeItem {
        id: n,
        issue_id: id(issue),
        run_id: None,
        branch: format!("feat/asf-{issue}"),
        state,
        attempts: 1,
        head_sha: None,
        rebased_onto: None,
        pr_url: None,
        gate_results: vec![],
        last_error: error.map(Into::into),
        enqueued_at: at.into(),
        updated_at: at.into(),
    }
}

fn comment(n: i64, issue: u32, kind: CommentType, body: &str, at: &str) -> Comment {
    Comment {
        id: n,
        issue_id: id(issue),
        author: "agent:x".into(),
        kind,
        to_lane: "human".into(),
        body: body.into(),
        reply_to: None,
        resolved_at: None,
        created_at: at.into(),
    }
}

/// Everything `waits` reads, owned, so a test builds it with struct-update syntax.
#[derive(Default)]
struct World {
    issues: Vec<Issue>,
    runs: Vec<Run>,
    queue: Vec<MergeItem>,
    questions: Vec<Comment>,
    blockers: Vec<Comment>,
    index: EventIndex,
}

impl World {
    fn waits(&self) -> Vec<Wait> {
        waits(&WaitInput {
            issues: &self.issues,
            runs: &self.runs,
            queue: &self.queue,
            questions: &self.questions,
            blockers: &self.blockers,
            index: &self.index,
        })
    }

    fn kinds(&self) -> Vec<(u32, WaitKind)> {
        self.waits()
            .iter()
            .map(|w| {
                let n = w.waiting.issue.to_string()[4..].parse().unwrap();
                (n, w.waiting.kind)
            })
            .collect()
    }
}

#[test]
fn the_order_of_the_kinds_is_decided_once() {
    let mut all = WaitKind::ALL.to_vec();
    all.sort_by_key(|k| k.urgency());
    assert_eq!(
        all,
        [
            WaitKind::RunBlocked,
            WaitKind::Question,
            WaitKind::Blocker,
            WaitKind::RunFailed,
            WaitKind::MergeFailed,
            WaitKind::MergeBlocked,
            WaitKind::RunStalled,
            WaitKind::RunOverBudget,
            WaitKind::IssueBlocked,
        ]
    );
    let mut urgencies: Vec<u8> = WaitKind::ALL.iter().map(|k| k.urgency()).collect();
    urgencies.sort();
    urgencies.dedup();
    assert_eq!(urgencies.len(), WaitKind::ALL.len(), "no two kinds tie");
}

#[test]
fn waits_come_back_in_that_order_whatever_their_age() {
    // the oldest wait is the last kind, the newest the first: the order is not the age
    let w = World {
        issues: (1..=8).map(|n| issue(n, Status::InProgress)).collect(),
        runs: vec![
            run(1, 1, RunState::Blocked, ""),
            run(3, 1, RunState::Failed, "2026-11-06T09:00:00Z"),
            run(5, 1, RunState::Stalled, ""),
            run(6, 1, RunState::OverBudget, ""),
            run(8, 1, RunState::Working, ""),
        ],
        queue: vec![
            merge(
                1,
                4,
                MergeState::Failed,
                Some("rebase conflict"),
                "2026-11-06T09:30:00Z",
            ),
            merge(
                2,
                7,
                MergeState::Blocked,
                Some("main is dirty"),
                "2026-11-06T09:40:00Z",
            ),
            // asf-8 has a working run: the worker is fixing it, nothing waits
            merge(
                3,
                8,
                MergeState::Failed,
                Some("review asks for changes"),
                "2026-11-06T09:00:00Z",
            ),
        ],
        questions: vec![comment(
            1,
            2,
            CommentType::Question,
            "Which crate?",
            "2026-11-06T11:00:00Z",
        )],
        ..World::default()
    };
    let mut w = w;
    w.issues[0].updated_at = "2026-11-06T11:59:00Z".into();
    w.issues.push(issue(9, Status::Blocked));
    w.issues[8].updated_at = "2026-11-01T00:00:00Z".into();
    assert_eq!(
        w.kinds(),
        [
            (1, WaitKind::RunBlocked),
            (2, WaitKind::Question),
            (3, WaitKind::RunFailed),
            (4, WaitKind::MergeFailed),
            (7, WaitKind::MergeBlocked),
            (5, WaitKind::RunStalled),
            (6, WaitKind::RunOverBudget),
            (9, WaitKind::IssueBlocked),
        ]
    );
}

#[test]
fn within_a_kind_the_oldest_wait_comes_first() {
    let w = World {
        issues: (1..=3).map(|n| issue(n, Status::InProgress)).collect(),
        questions: vec![
            comment(7, 3, CommentType::Question, "third", "2026-11-06T11:30:00Z"),
            comment(8, 1, CommentType::Question, "first", "2026-11-06T10:00:00Z"),
            comment(9, 2, CommentType::Question, "second", "2026-11-06T11:00:00Z"),
        ],
        ..World::default()
    };
    let order: Vec<String> = w.waits().iter().map(|w| w.waiting.reason.clone()).collect();
    assert_eq!(order, ["first", "second", "third"]);
}

#[test]
fn a_finished_issue_waits_for_nobody() {
    let w = World {
        issues: vec![issue(1, Status::Done), issue(2, Status::Cancelled)],
        runs: vec![run(1, 1, RunState::Failed, "2026-11-06T09:00:00Z")],
        queue: vec![merge(1, 2, MergeState::Failed, Some("x"), "2026-11-06T09:00:00Z")],
        questions: vec![comment(1, 1, CommentType::Question, "q", "2026-11-06T09:00:00Z")],
        ..World::default()
    };
    assert!(w.waits().is_empty(), "{:?}", w.kinds());
}

#[test]
fn a_failed_or_blocked_merge_item_of_an_issue_whose_worker_is_busy_does_not_wait() {
    // any run that has not ended: the worker was sent back to fix it, or is stuck on a prompt
    for state in [
        RunState::Starting,
        RunState::Working,
        RunState::Finishing,
        RunState::Verifying,
        RunState::Blocked,
        RunState::Stalled,
        RunState::OverBudget,
    ] {
        for item in [MergeState::Failed, MergeState::Blocked] {
            let w = World {
                issues: vec![issue(1, Status::InProgress)],
                runs: vec![run(1, 1, state, "")],
                queue: vec![merge(1, 1, item, Some("boom"), "2026-11-06T09:00:00Z")],
                ..World::default()
            };
            let kinds: Vec<WaitKind> = w.kinds().into_iter().map(|(_, k)| k).collect();
            assert!(
                !kinds.contains(&WaitKind::MergeFailed) && !kinds.contains(&WaitKind::MergeBlocked),
                "{state} / {item}: {kinds:?}"
            );
        }
    }
    // the same items with no run, or only an ended one, wait for the human
    for runs in [
        vec![],
        vec![run(1, 1, RunState::Succeeded, "2026-11-06T09:00:00Z")],
    ] {
        let w = World {
            issues: vec![issue(1, Status::Validating)],
            runs,
            queue: vec![merge(
                1,
                1,
                MergeState::Failed,
                Some("boom"),
                "2026-11-06T09:00:00Z",
            )],
            ..World::default()
        };
        assert_eq!(w.kinds(), [(1, WaitKind::MergeFailed)]);
    }
}

#[test]
fn merge_items_that_are_not_failed_or_blocked_do_not_wait() {
    let w = World {
        issues: vec![issue(1, Status::Validating)],
        queue: [MergeState::Queued, MergeState::Merged, MergeState::Merging]
            .into_iter()
            .enumerate()
            .map(|(n, s)| merge(n as i64 + 1, 1, s, None, "2026-11-06T09:00:00Z"))
            .collect(),
        ..World::default()
    };
    assert!(w.waits().is_empty(), "{:?}", w.kinds());
}

#[test]
fn only_the_newest_run_of_an_issue_counts() {
    let failed_then_retried = World {
        issues: vec![issue(1, Status::InProgress)],
        runs: vec![
            run(1, 1, RunState::Failed, "2026-11-06T09:00:00Z"),
            run(1, 2, RunState::Working, ""),
        ],
        ..World::default()
    };
    assert!(failed_then_retried.waits().is_empty());

    let retried_and_failed_again = World {
        issues: vec![issue(1, Status::InProgress)],
        runs: vec![
            run(1, 2, RunState::Failed, "2026-11-06T10:00:00Z"),
            run(1, 1, RunState::Failed, "2026-11-06T09:00:00Z"),
        ],
        ..World::default()
    };
    let got = retried_and_failed_again.waits();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].waiting.since, "2026-11-06T10:00:00Z", "the second attempt");
}

#[test]
fn the_key_that_acts_is_the_one_the_command_behind_it_accepts() {
    let w = World {
        issues: (1..=7).map(|n| issue(n, Status::InProgress)).collect(),
        runs: vec![
            run(1, 1, RunState::Blocked, ""),
            run(2, 1, RunState::Stalled, ""),
            run(3, 1, RunState::OverBudget, ""),
            run(4, 1, RunState::Failed, "2026-11-06T09:00:00Z"),
        ],
        queue: vec![merge(1, 5, MergeState::Failed, None, "2026-11-06T09:00:00Z")],
        questions: vec![comment(11, 6, CommentType::Question, "q", "2026-11-06T09:00:00Z")],
        ..World::default()
    };
    let mut w = w;
    w.issues[6] = issue(7, Status::Blocked);
    let acts: Vec<(WaitKind, Option<Act>)> = w.waits().into_iter().map(|x| (x.waiting.kind, x.act)).collect();
    assert_eq!(
        acts,
        [
            (WaitKind::RunBlocked, Some(Act::Answer)),
            (WaitKind::Question, Some(Act::Answer)),
            (WaitKind::RunFailed, Some(Act::Verify)),
            (WaitKind::MergeFailed, Some(Act::Retry)),
            (WaitKind::RunStalled, Some(Act::Answer)),
            (WaitKind::RunOverBudget, Some(Act::Abort)),
            (WaitKind::IssueBlocked, None),
        ]
    );
    let keys: Vec<char> = [Act::Answer, Act::Verify, Act::Retry, Act::Abort]
        .iter()
        .map(|a| a.key())
        .collect();
    assert_eq!(keys, ['a', 'v', 'r', 'x']);
    assert_eq!(Act::Verify.hint(), "v verify");
    assert_eq!(w.waits()[1].comment, Some(11), "a question points at its comment");
}

#[test]
fn the_reason_is_the_one_line_that_says_why() {
    let screen = "Worker for asf-1 is waiting for input.\n\nScreen:\n```\n\n  Allow Bash(rm -rf target)?\n 1. Yes\n```\nAnswer with `klobuk run keys asf-1 y`.";
    let mut blocked = run(1, 1, RunState::Blocked, "");
    let mut failed = run(2, 1, RunState::Failed, "2026-11-06T09:00:00Z");
    let mut failed_with_reason = run(3, 1, RunState::Failed, "2026-11-06T09:00:00Z");
    failed_with_reason.exit_reason = Some("gate `test` failed\nsecond line".into());
    let w = World {
        issues: (1..=5).map(|n| issue(n, Status::InProgress)).collect(),
        runs: vec![
            blocked.clone(),
            failed.clone(),
            failed_with_reason,
            run(4, 1, RunState::Blocked, ""),
            run(5, 1, RunState::Blocked, ""),
        ],
        blockers: vec![
            comment(1, 1, CommentType::Blocker, screen, "2026-11-06T09:00:00Z"),
            comment(
                2,
                2,
                CommentType::Blocker,
                "Verification of asf-2 failed:\nmore",
                "2026-11-06T09:00:00Z",
            ),
            comment(
                3,
                3,
                CommentType::Blocker,
                "ignored: the run has its own reason",
                "2026-11-06T09:00:00Z",
            ),
            // a blocked run is about its prompt: a BLOCKER that quotes no screen says nothing of it
            comment(
                4,
                4,
                CommentType::Blocker,
                "Over budget.\nNo screen here.",
                "2026-11-06T09:00:00Z",
            ),
        ],
        queue: vec![merge(1, 4, MergeState::Failed, None, "2026-11-06T09:00:00Z")],
        questions: vec![comment(
            9,
            5,
            CommentType::Question,
            "\n  Keep   the flag?\nsecond",
            "2026-11-06T09:00:00Z",
        )],
        ..World::default()
    };
    let reason = |n: u32, kind: WaitKind| {
        w.waits()
            .into_iter()
            .find(|x| x.waiting.issue == id(n) && x.waiting.kind == kind)
            .unwrap_or_else(|| panic!("asf-{n} {kind}"))
            .waiting
            .reason
    };
    assert_eq!(reason(1, WaitKind::RunBlocked), "Allow Bash(rm -rf target)?");
    assert_eq!(reason(2, WaitKind::RunFailed), "Verification of asf-2 failed:");
    assert_eq!(reason(3, WaitKind::RunFailed), "gate `test` failed");
    assert_eq!(reason(4, WaitKind::RunBlocked), "");
    assert_eq!(reason(5, WaitKind::Question), "Keep the flag?");

    // a blocked run with an exit reason and no screen uses the reason
    blocked.exit_reason = Some("waiting for a dialog".into());
    failed.exit_reason = Some("own reason".into());
    let w2 = World {
        issues: vec![issue(1, Status::InProgress), issue(2, Status::InProgress)],
        runs: vec![blocked, failed],
        ..World::default()
    };
    let got: Vec<String> = w2.waits().into_iter().map(|x| x.waiting.reason).collect();
    assert_eq!(got, ["waiting for a dialog", "own reason"]);
}

#[test]
fn a_merge_error_is_the_reason_of_its_item() {
    let w = World {
        issues: vec![issue(1, Status::Validating)],
        queue: vec![merge(
            1,
            1,
            MergeState::Blocked,
            Some("main is dirty\nstack"),
            "2026-11-06T09:00:00Z",
        )],
        ..World::default()
    };
    assert_eq!(w.waits()[0].waiting.reason, "main is dirty");
}

#[test]
fn a_wait_began_when_the_log_says_its_thing_entered_the_state() {
    let mut index = EventIndex::default();
    let event = |kind: &str, run: Option<&str>, issue: u32, to: &str, at: &str, extra: serde_json::Value| {
        let mut payload = serde_json::json!({ "to": to });
        payload
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        Event {
            seq: 1,
            ts: at.into(),
            kind: kind.into(),
            issue_id: Some(id(issue)),
            run_id: run.map(str::to_string),
            actor: None,
            payload,
        }
    };
    let none = serde_json::json!({});
    index.absorb(&event(
        "run.state",
        Some("run-1-1"),
        1,
        "stalled",
        "2026-11-06T10:15:00Z",
        none.clone(),
    ));
    index.absorb(&event(
        "merge.state",
        None,
        2,
        "failed",
        "2026-11-06T10:20:00Z",
        serde_json::json!({ "merge_id": 4 }),
    ));
    index.absorb(&event(
        "issue.status",
        None,
        3,
        "blocked",
        "2026-11-06T10:25:00Z",
        none,
    ));
    let w = World {
        issues: vec![
            issue(1, Status::InProgress),
            issue(2, Status::Validating),
            issue(3, Status::Blocked),
        ],
        runs: vec![run(1, 1, RunState::Stalled, "")],
        queue: vec![merge(4, 2, MergeState::Failed, None, "2026-11-06T10:50:00Z")],
        index,
        ..World::default()
    };
    let got: Vec<(WaitKind, String, String)> = w
        .waits()
        .into_iter()
        .map(|x| (x.waiting.kind, x.waiting.since, x.waiting.key))
        .collect();
    assert_eq!(
        got,
        [
            (
                WaitKind::MergeFailed,
                "2026-11-06T10:50:00Z".to_string(),
                "merge:4:failed@2026-11-06T10:20:00Z".to_string()
            ),
            (
                WaitKind::RunStalled,
                "2026-11-06T10:15:00Z".to_string(),
                "run:run-1-1:stalled@2026-11-06T10:15:00Z".to_string()
            ),
            (
                WaitKind::IssueBlocked,
                "2026-11-02T10:00:00Z".to_string(),
                "issue:blocked@2026-11-06T10:25:00Z".to_string()
            ),
        ]
    );
}

#[test]
fn a_run_without_a_logged_state_change_began_at_its_last_known_moment() {
    let mut stalled = run(1, 1, RunState::Stalled, "");
    stalled.last_seen_at = Some("2026-11-06T08:30:00Z".into());
    let failed = run(2, 1, RunState::Failed, "2026-11-06T09:00:00Z");
    let over = run(3, 1, RunState::OverBudget, "");
    let w = World {
        issues: (1..=3).map(|n| issue(n, Status::InProgress)).collect(),
        runs: vec![stalled, failed, over],
        ..World::default()
    };
    let since: Vec<String> = w.waits().into_iter().map(|x| x.waiting.since).collect();
    assert_eq!(
        since,
        [
            "2026-11-06T09:00:00Z", // failed: finished_at
            "2026-11-06T08:30:00Z", // stalled: last_seen_at
            "2026-11-06T08:00:00Z", // over budget: started_at
        ]
    );
}

#[test]
fn the_screen_is_the_first_line_of_the_fenced_block_after_screen() {
    let body = "Worker is waiting.\n\nScreen:\n```\n\n  Allow it?\n 1. Yes\n```\nmore";
    assert_eq!(screen_line(body).as_deref(), Some("Allow it?"));
    assert_eq!(screen_line("No screen in this one."), None);
    assert_eq!(screen_line("Screen:\n```\n```\n"), None, "an empty fence");
    assert_eq!(screen_line("Screen:\nno fence"), None);
}
