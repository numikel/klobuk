use std::sync::{Arc, Barrier};
use std::thread;

use klobuk_core::*;
use klobuk_store::SqliteStore;
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

fn claim_by(worker: &str) -> NewClaim {
    NewClaim {
        worker: worker.into(),
        ..NewClaim::default()
    }
}

#[test]
fn claim_sets_in_progress_and_records_fields() {
    let (_d, s) = open();
    let a = ready_issue(&s, "a");
    let c = s
        .claim(
            &a.id,
            &NewClaim {
                worker: "asf-1".into(),
                worktree: Some(".worktrees/asf-1".into()),
                branch: Some("feat/asf-1".into()),
                note: Some("first".into()),
                run_id: None,
            },
        )
        .unwrap();
    assert_eq!(c.issue_id, a.id);
    assert_eq!(c.worker, "asf-1");
    assert_eq!(c.branch.as_deref(), Some("feat/asf-1"));
    assert!(c.claimed_at.ends_with('Z'));
    assert_eq!(s.get_issue(&a.id).unwrap().unwrap().status, Status::InProgress);
    let active = s.active_claims().unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].worker, "asf-1");
}

#[test]
fn second_claim_is_already_claimed_with_worker_name() {
    let (_d, s) = open();
    let a = ready_issue(&s, "a");
    s.claim(&a.id, &claim_by("w1")).unwrap();
    let e = s.claim(&a.id, &claim_by("w2")).unwrap_err();
    assert_eq!(e, ClaimError::AlreadyClaimed { by: "w1".into() });
}

#[test]
fn claim_not_ready_for_backlog_or_blocked() {
    let (_d, s) = open();
    let b = s
        .create_issue(&NewIssue {
            title: "b".into(),
            ..NewIssue::default()
        })
        .unwrap();
    let e = s.claim(&b.id, &claim_by("w")).unwrap_err();
    assert_eq!(
        e,
        ClaimError::NotReady {
            status: Status::Backlog
        }
    );
    let e = s.claim(&"asf-9".parse().unwrap(), &claim_by("w")).unwrap_err();
    assert!(matches!(e, ClaimError::Store(StoreError::NotFound(_))));
}

#[test]
fn release_restores_ready_and_deletes_claim() {
    let (_d, s) = open();
    let a = ready_issue(&s, "a");
    s.claim(&a.id, &claim_by("w")).unwrap();
    s.release(&a.id).unwrap();
    assert!(s.active_claims().unwrap().is_empty());
    assert_eq!(s.get_issue(&a.id).unwrap().unwrap().status, Status::Ready);
    assert!(matches!(s.release(&a.id).unwrap_err(), StoreError::NotFound(_)));
    // re-claimable after release
    s.claim(&a.id, &claim_by("w2")).unwrap();
}

#[test]
fn release_keeps_non_in_progress_status() {
    let (_d, s) = open();
    let a = ready_issue(&s, "a");
    s.claim(&a.id, &claim_by("w")).unwrap();
    s.set_status(&a.id, Status::Blocked, "w").unwrap();
    s.release(&a.id).unwrap();
    assert_eq!(s.get_issue(&a.id).unwrap().unwrap().status, Status::Blocked);
}

#[test]
fn concurrent_claims_one_winner() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.db");
    let s = SqliteStore::open(&path).unwrap();
    s.init_repo("D:/repo", "asf", "main").unwrap();
    let a = ready_issue(&s, "a");
    drop(s);

    let n = 8;
    let barrier = Arc::new(Barrier::new(n));
    let handles: Vec<_> = (0..n)
        .map(|i| {
            let path = path.clone();
            let id = a.id.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let store = SqliteStore::open(&path).unwrap();
                barrier.wait();
                store.claim(&id, &claim_by(&format!("w{i}")))
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let wins = results.iter().filter(|r| r.is_ok()).count();
    let losses = results
        .iter()
        .filter(|r| matches!(r, Err(ClaimError::AlreadyClaimed { .. })))
        .count();
    assert_eq!(wins, 1, "{results:?}");
    assert_eq!(losses, n - 1, "{results:?}");
}

#[test]
fn events_appended_for_mutations() {
    let (_d, s) = open();
    let a = ready_issue(&s, "a"); // issue.created
    s.claim(&a.id, &claim_by("w")).unwrap(); // claim.taken
    s.add_comment(&NewComment {
        issue_id: a.id.clone(),
        author: "agent:w".into(),
        kind: CommentType::Status,
        to_lane: "all".into(),
        body: "started".into(),
        reply_to: None,
    })
    .unwrap(); // comment.added
    s.release(&a.id).unwrap(); // claim.released
    let ev = s.events_since(0, 100).unwrap();
    let kinds: Vec<&str> = ev.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(
        kinds,
        ["issue.created", "claim.taken", "comment.added", "claim.released"]
    );
    assert!(ev.iter().all(|e| e.issue_id.as_ref() == Some(&a.id)));
    assert_eq!(s.events_since(ev[1].seq, 100).unwrap().len(), 2);
    assert_eq!(s.events_since(0, 1).unwrap().len(), 1);
}
