use asf_core::*;
use asf_store::SqliteStore;
use tempfile::TempDir;

fn open() -> (TempDir, SqliteStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&dir.path().join("f.db")).unwrap();
    store.init_repo("D:/repo", "asf", "main").unwrap();
    (dir, store)
}

fn ready_issue(s: &SqliteStore, title: &str) -> Issue {
    s.create_issue(&NewIssue {
        title: title.into(),
        status: Status::Ready,
        ..NewIssue::default()
    })
    .unwrap()
}

fn new_run(issue: &IssueId) -> NewRun {
    NewRun {
        issue_id: issue.clone(),
        runtime: "fake".into(),
        worktree: format!(".worktrees/{issue}"),
        branch: format!("feat/{issue}"),
        base_sha: "abc123".into(),
        budget_usd: Some(5.0),
    }
}

#[test]
fn create_run_numbers_attempts_and_defaults() {
    let (_d, s) = open();
    let a = ready_issue(&s, "a");
    let r1 = s.create_run(&new_run(&a.id)).unwrap();
    assert_eq!(r1.attempt, 1);
    assert_eq!(r1.state, RunState::Starting);
    assert_eq!(r1.cost_usd, 0.0);
    assert_eq!(r1.tool_calls, 0);
    assert_eq!(r1.id.len(), 26, "ULID");
    assert!(r1.started_at.ends_with('Z'));
    let r2 = s.create_run(&new_run(&a.id)).unwrap();
    assert_eq!(r2.attempt, 2);
    assert!(matches!(
        s.create_run(&new_run(&"asf-9".parse().unwrap())).unwrap_err(),
        StoreError::NotFound(_)
    ));
}

#[test]
fn update_run_patches_only_given_fields() {
    let (_d, s) = open();
    let a = ready_issue(&s, "a");
    let r = s.create_run(&new_run(&a.id)).unwrap();
    let p = RunPatch {
        state: Some(RunState::Working),
        pane_id: Some("w1:p1".into()),
        workspace_id: Some("w1".into()),
        agent_name: Some(a.id.to_string()),
        cost_usd: Some(0.42),
        tool_calls: Some(3),
        ..RunPatch::default()
    };
    let u = s.update_run(&r.id, &p).unwrap();
    assert_eq!(u.state, RunState::Working);
    assert_eq!(u.pane_id.as_deref(), Some("w1:p1"));
    assert_eq!(u.cost_usd, 0.42);
    assert_eq!(u.tool_calls, 3);
    assert_eq!(u.branch, r.branch, "untouched");
    let u2 = s
        .update_run(
            &r.id,
            &RunPatch {
                exit_reason: Some("done".into()),
                ..RunPatch::default()
            },
        )
        .unwrap();
    assert_eq!(u2.cost_usd, 0.42, "earlier patch kept");
    assert_eq!(u2.exit_reason.as_deref(), Some("done"));
    assert!(matches!(
        s.update_run("nope", &RunPatch::default()).unwrap_err(),
        StoreError::NotFound(_)
    ));
    assert_eq!(s.get_run(&r.id).unwrap().unwrap().state, RunState::Working);
    assert!(s.get_run("nope").unwrap().is_none());
}

#[test]
fn runs_filter_by_issue_and_active() {
    let (_d, s) = open();
    let a = ready_issue(&s, "a");
    let b = ready_issue(&s, "b");
    let ra = s.create_run(&new_run(&a.id)).unwrap();
    let _rb = s.create_run(&new_run(&b.id)).unwrap();
    s.update_run(
        &ra.id,
        &RunPatch {
            state: Some(RunState::Succeeded),
            ..RunPatch::default()
        },
    )
    .unwrap();
    assert_eq!(s.runs(None, false).unwrap().len(), 2);
    assert_eq!(s.runs(None, true).unwrap().len(), 1, "succeeded is terminal");
    assert_eq!(s.runs(Some(&a.id), false).unwrap().len(), 1);
    assert_eq!(s.runs(Some(&a.id), true).unwrap().len(), 0);
    // latest active run for an issue is the last created
    let ra2 = s.create_run(&new_run(&a.id)).unwrap();
    let active_a = s.runs(Some(&a.id), true).unwrap();
    assert_eq!(active_a.len(), 1);
    assert_eq!(active_a[0].id, ra2.id);
}

#[test]
fn merge_queue_fifo_and_conflict_on_duplicate() {
    let (_d, s) = open();
    let a = ready_issue(&s, "a");
    let b = ready_issue(&s, "b");
    let ra = s.create_run(&new_run(&a.id)).unwrap();
    let ma = s.enqueue_merge(&a.id, Some(&ra.id), "feat/asf-1").unwrap();
    let mb = s.enqueue_merge(&b.id, None, "feat/asf-2").unwrap();
    assert_eq!(ma.state, MergeState::Queued);
    assert_eq!(ma.attempts, 0);
    assert!(ma.gate_results.is_empty());
    let q = s.merge_queue().unwrap();
    assert_eq!(q.iter().map(|m| m.id).collect::<Vec<_>>(), vec![ma.id, mb.id]);
    assert!(matches!(
        s.enqueue_merge(&a.id, None, "feat/asf-1").unwrap_err(),
        StoreError::Conflict(_)
    ));
    // after a terminal state the issue can be enqueued again
    s.update_merge(
        ma.id,
        &MergePatch {
            state: Some(MergeState::Skipped),
            ..MergePatch::default()
        },
    )
    .unwrap();
    let ma2 = s.enqueue_merge(&a.id, None, "feat/asf-1").unwrap();
    assert!(ma2.id > mb.id);
    assert!(matches!(
        s.enqueue_merge(&"asf-9".parse().unwrap(), None, "x").unwrap_err(),
        StoreError::NotFound(_)
    ));
}

#[test]
fn update_merge_patch_and_gate_results_roundtrip() {
    let (_d, s) = open();
    let a = ready_issue(&s, "a");
    let m = s.enqueue_merge(&a.id, None, "feat/asf-1").unwrap();
    let gr = vec![GateResult {
        name: "fmt".into(),
        ok: true,
        exit_code: Some(0),
        duration_ms: 5,
        tail: String::new(),
    }];
    let u = s
        .update_merge(
            m.id,
            &MergePatch {
                state: Some(MergeState::Gating),
                attempts: Some(1),
                head_sha: Some("h1".into()),
                rebased_onto: Some("m1".into()),
                gate_results: Some(gr.clone()),
                last_error: Some(Some("x".into())),
                ..MergePatch::default()
            },
        )
        .unwrap();
    assert_eq!(u.state, MergeState::Gating);
    assert_eq!(u.gate_results, gr);
    assert_eq!(u.last_error.as_deref(), Some("x"));
    let u2 = s
        .update_merge(
            m.id,
            &MergePatch {
                last_error: Some(None),
                ..MergePatch::default()
            },
        )
        .unwrap();
    assert!(u2.last_error.is_none());
    assert_eq!(u2.gate_results, gr);
    assert!(u2.updated_at >= m.updated_at);
}

#[test]
fn run_and_merge_events() {
    let (_d, s) = open();
    let a = ready_issue(&s, "a");
    let r = s.create_run(&new_run(&a.id)).unwrap();
    s.update_run(
        &r.id,
        &RunPatch {
            state: Some(RunState::Working),
            ..RunPatch::default()
        },
    )
    .unwrap();
    s.update_run(
        &r.id,
        &RunPatch {
            cost_usd: Some(0.1),
            ..RunPatch::default()
        },
    )
    .unwrap(); // no state change: no event
    let m = s.enqueue_merge(&a.id, Some(&r.id), "feat/asf-1").unwrap();
    s.update_merge(
        m.id,
        &MergePatch {
            state: Some(MergeState::Merged),
            ..MergePatch::default()
        },
    )
    .unwrap();
    let kinds: Vec<String> = s
        .events_since(0, 100)
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(
        kinds,
        [
            "issue.created",
            "run.created",
            "run.state",
            "merge.enqueued",
            "merge.state"
        ]
    );
    let ev = s.events_since(0, 100).unwrap();
    assert_eq!(ev[2].run_id.as_deref(), Some(r.id.as_str()));
    assert_eq!(ev[2].payload["to"], "working");
}
