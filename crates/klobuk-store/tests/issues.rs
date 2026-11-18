use klobuk_core::*;
use klobuk_store::SqliteStore;
use tempfile::TempDir;

fn open() -> (TempDir, SqliteStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&dir.path().join(".factory").join("factory.db")).unwrap();
    store.init_repo("D:/repo", "asf", "main").unwrap();
    (dir, store)
}

fn new_issue(title: &str) -> NewIssue {
    NewIssue {
        title: title.into(),
        ..NewIssue::default()
    }
}

fn id(s: &str) -> IssueId {
    s.parse().unwrap()
}

#[test]
fn open_creates_file_wal_and_schema_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".factory").join("factory.db");
    let store = SqliteStore::open(&path).unwrap();
    assert!(path.exists());
    assert_eq!(store.schema_version().unwrap(), 2);
    assert_eq!(store.journal_mode().unwrap(), "wal");
    // reopening an existing database must not re-run the migration
    drop(store);
    let again = SqliteStore::open(&path).unwrap();
    assert_eq!(again.schema_version().unwrap(), 2);
}

#[test]
fn init_repo_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&dir.path().join("f.db")).unwrap();
    let info = store.init_repo("D:/repo", "law", "main").unwrap();
    assert_eq!(info.id_prefix, "law");
    assert_eq!(info.next_seq, 1);
    let e = store.init_repo("D:/repo", "law", "main").unwrap_err();
    assert!(matches!(e, StoreError::Conflict(_)), "{e}");
    assert!(matches!(
        store.init_repo("D:/x", "Bad", "main").unwrap_err(),
        StoreError::Validation(_)
    ));
    assert_eq!(store.repo().unwrap().id_prefix, "law");
}

#[test]
fn repo_missing_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&dir.path().join("f.db")).unwrap();
    assert!(matches!(store.repo().unwrap_err(), StoreError::NotFound(_)));
}

#[test]
fn create_allocates_sequential_ids() {
    let (_d, s) = open();
    let a = s.create_issue(&new_issue("a")).unwrap();
    let b = s.create_issue(&new_issue("b")).unwrap();
    assert_eq!(a.id, id("asf-1"));
    assert_eq!(b.id, id("asf-2"));
    assert_eq!(s.repo().unwrap().next_seq, 3);
    assert_eq!(a.status, Status::Backlog);
    assert_eq!(a.uid.len(), 26, "ULID string");
    assert_ne!(a.uid, b.uid);
    assert!(a.created_at.ends_with('Z'));
    assert_eq!(s.get_issue(&id("asf-1")).unwrap().unwrap().title, "a");
    assert!(s.get_issue(&id("asf-9")).unwrap().is_none());
}

#[test]
fn create_with_deps_and_list() {
    let (_d, s) = open();
    let a = s.create_issue(&new_issue("a")).unwrap();
    let mut n = new_issue("b");
    n.deps = vec![a.id.clone()];
    n.file_scope = vec!["src/**".into()];
    n.acceptance = vec![Acceptance {
        text: "works".into(),
        verify: Some("cargo test".into()),
    }];
    n.labels = vec!["risky".into()];
    let b = s.create_issue(&n).unwrap();
    assert_eq!(b.file_scope, vec!["src/**".to_string()]);
    assert_eq!(b.acceptance[0].verify.as_deref(), Some("cargo test"));
    let deps = s.deps().unwrap();
    assert_eq!(
        deps,
        vec![Dep {
            issue_id: b.id.clone(),
            depends_on: a.id.clone()
        }]
    );
    let all = s.list_issues(&IssueFilter::default()).unwrap();
    assert_eq!(
        all.iter().map(|i| i.id.to_string()).collect::<Vec<_>>(),
        ["asf-1", "asf-2"]
    );
}

#[test]
fn create_rejects_unknown_dep_atomically() {
    let (_d, s) = open();
    let mut n = new_issue("b");
    n.deps = vec![id("asf-9")];
    let e = s.create_issue(&n).unwrap_err();
    assert!(matches!(e, StoreError::Validation(_)), "{e}");
    assert!(s.list_issues(&IssueFilter::default()).unwrap().is_empty());
    assert_eq!(
        s.repo().unwrap().next_seq,
        1,
        "sequence must not advance on a failed create"
    );
}

#[test]
fn create_rejects_empty_title_and_bad_glob() {
    let (_d, s) = open();
    assert!(matches!(
        s.create_issue(&new_issue("  ")).unwrap_err(),
        StoreError::Validation(_)
    ));
    let mut n = new_issue("x");
    n.file_scope = vec!["src/[".into()];
    assert!(matches!(
        s.create_issue(&n).unwrap_err(),
        StoreError::Validation(_)
    ));
}

#[test]
fn add_dep_rejects_cycle_and_self() {
    let (_d, s) = open();
    let a = s.create_issue(&new_issue("a")).unwrap();
    let b = s.create_issue(&new_issue("b")).unwrap();
    s.add_dep(&b.id, &a.id).unwrap();
    let e = s.add_dep(&a.id, &b.id).unwrap_err();
    assert!(matches!(e, StoreError::Validation(_)), "{e}");
    assert!(matches!(
        s.add_dep(&a.id, &a.id).unwrap_err(),
        StoreError::Validation(_)
    ));
    assert!(matches!(
        s.add_dep(&a.id, &id("asf-7")).unwrap_err(),
        StoreError::NotFound(_)
    ));
    s.add_dep(&b.id, &a.id).unwrap(); // idempotent
    assert_eq!(s.deps().unwrap().len(), 1);
    s.remove_dep(&b.id, &a.id).unwrap();
    assert!(s.deps().unwrap().is_empty());
}

#[test]
fn update_patch_only_touches_given_fields() {
    let (_d, s) = open();
    let mut n = new_issue("a");
    n.estimate = Some("M".into());
    n.body = "body".into();
    let a = s.create_issue(&n).unwrap();
    let patch = IssuePatch {
        title: Some("renamed".into()),
        estimate: Some(None),
        ..IssuePatch::default()
    };
    let u = s.update_issue(&a.id, &patch).unwrap();
    assert_eq!(u.title, "renamed");
    assert_eq!(u.body, "body");
    assert_eq!(u.estimate, None);
    assert!(u.updated_at >= a.updated_at);
    assert!(matches!(
        s.update_issue(&id("asf-9"), &IssuePatch::default()).unwrap_err(),
        StoreError::NotFound(_)
    ));
}

#[test]
fn set_status_rules() {
    let (_d, s) = open();
    let a = s.create_issue(&new_issue("a")).unwrap();
    let r = s.set_status(&a.id, Status::Ready, "human").unwrap();
    assert_eq!(r.status, Status::Ready);
    assert_eq!(r.closed_at, None);
    let d = s.set_status(&a.id, Status::Done, "human").unwrap();
    assert!(d.closed_at.is_some());
    let e = s.set_status(&a.id, Status::Ready, "human").unwrap_err();
    assert!(matches!(e, StoreError::Conflict(_)), "{e}");
}

#[test]
fn promote_ready_moves_backlog_with_done_deps() {
    let (_d, s) = open();
    let a = s.create_issue(&new_issue("a")).unwrap();
    let mut n = new_issue("b");
    n.deps = vec![a.id.clone()];
    let b = s.create_issue(&n).unwrap();
    let c = s.create_issue(&new_issue("c")).unwrap();
    let first = s.promote_ready().unwrap();
    assert_eq!(first, vec![a.id.clone(), c.id.clone()]);
    s.set_status(&a.id, Status::Done, "human").unwrap();
    let second = s.promote_ready().unwrap();
    assert_eq!(second, vec![b.id.clone()]);
    assert_eq!(s.get_issue(&b.id).unwrap().unwrap().status, Status::Ready);
    assert!(s.promote_ready().unwrap().is_empty());
}

#[test]
fn close_completed_epics_marks_epic_done_once() {
    let (_d, s) = open();
    let mut e = new_issue("epic");
    e.kind = IssueType::Epic;
    let epic = s.create_issue(&e).unwrap();
    let mut c1 = new_issue("c1");
    c1.parent_id = Some(epic.id.clone());
    let c1 = s.create_issue(&c1).unwrap();
    let mut c2 = new_issue("c2");
    c2.parent_id = Some(epic.id.clone());
    let c2 = s.create_issue(&c2).unwrap();

    assert!(s.close_completed_epics().unwrap().is_empty());
    s.set_status(&c1.id, Status::Done, "human").unwrap();
    assert!(s.close_completed_epics().unwrap().is_empty());
    s.set_status(&c2.id, Status::Cancelled, "human").unwrap();
    assert_eq!(s.close_completed_epics().unwrap(), vec![epic.id.clone()]);
    let got = s.get_issue(&epic.id).unwrap().unwrap();
    assert_eq!(got.status, Status::Done);
    assert!(got.closed_at.is_some());
    assert!(s.close_completed_epics().unwrap().is_empty());
    let last = s.events_since(0, 100).unwrap().pop().unwrap();
    assert_eq!(last.kind, "issue.status");
    assert_eq!(last.payload["to"], "done");
}

#[test]
fn list_filter_by_status_type_label_parent() {
    let (_d, s) = open();
    let mut e = new_issue("epic");
    e.kind = IssueType::Epic;
    let epic = s.create_issue(&e).unwrap();
    let mut n = new_issue("t1");
    n.labels = vec!["risky".into()];
    n.parent_id = Some(epic.id.clone());
    n.status = Status::Ready;
    s.create_issue(&n).unwrap();
    s.create_issue(&new_issue("t2")).unwrap();

    let ready = s
        .list_issues(&IssueFilter {
            status: Some(Status::Ready),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(ready.len(), 1);
    let risky = s
        .list_issues(&IssueFilter {
            label: Some("risky".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(risky[0].title, "t1");
    let epics = s
        .list_issues(&IssueFilter {
            kind: Some(IssueType::Epic),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(epics.len(), 1);
    let children = s
        .list_issues(&IssueFilter {
            parent: Some(epic.id.clone()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(children.len(), 1);
}
