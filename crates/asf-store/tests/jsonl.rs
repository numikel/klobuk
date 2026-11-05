use std::io::Cursor;

use asf_core::*;
use asf_store::SqliteStore;
use tempfile::TempDir;

fn open() -> (TempDir, SqliteStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&dir.path().join("f.db")).unwrap();
    store.init_repo("D:/repo", "asf", "main").unwrap();
    (dir, store)
}

fn export(s: &SqliteStore) -> String {
    let mut buf = Vec::new();
    s.export_jsonl(&mut buf, &ExportOpts::default()).unwrap();
    String::from_utf8(buf).unwrap()
}

/// Drops the `meta` line (its `exported_at` is the only non-deterministic field).
fn body(export: &str) -> String {
    export.lines().skip(1).collect::<Vec<_>>().join("\n")
}

fn seed(s: &SqliteStore) {
    let a = s
        .create_issue(&NewIssue {
            title: "a".into(),
            status: Status::Ready,
            ..NewIssue::default()
        })
        .unwrap();
    let mut n = NewIssue {
        title: "b".into(),
        ..NewIssue::default()
    };
    n.deps = vec![a.id.clone()];
    n.file_scope = vec!["src/**".into()];
    n.labels = vec!["risky".into()];
    let b = s.create_issue(&n).unwrap();
    let c1 = s
        .add_comment(&NewComment {
            issue_id: a.id.clone(),
            author: "human".into(),
            kind: CommentType::Decision,
            to_lane: "all".into(),
            body: "go".into(),
            reply_to: None,
        })
        .unwrap();
    s.add_comment(&NewComment {
        issue_id: a.id.clone(),
        author: "agent:asf-1".into(),
        kind: CommentType::Answer,
        to_lane: "human".into(),
        body: "ok".into(),
        reply_to: Some(c1.id),
    })
    .unwrap();
    s.add_comment(&NewComment {
        issue_id: b.id.clone(),
        author: "human".into(),
        kind: CommentType::Fyi,
        to_lane: "all".into(),
        body: "note".into(),
        reply_to: None,
    })
    .unwrap();
}

#[test]
fn export_shape_and_order() {
    let (_d, s) = open();
    seed(&s);
    let out = export(&s);
    let lines: Vec<&str> = out.lines().collect();
    assert!(
        lines[0].starts_with(r#"{"kind":"meta","format":1,"prefix":"asf","next_seq":3,"#),
        "{}",
        lines[0]
    );
    assert!(lines[1].starts_with(r#"{"kind":"issue","id":"asf-1""#));
    assert!(lines[2].starts_with(r#"{"kind":"issue","id":"asf-2""#));
    assert_eq!(lines[3], r#"{"kind":"dep","issue":"asf-2","depends_on":"asf-1"}"#);
    assert!(lines[4].starts_with(r#"{"kind":"comment","issue":"asf-1","n":1,"author":"human""#));
    assert!(lines[5].contains(r#""n":2,"author":"agent:asf-1""#) && lines[5].contains(r#""reply_to_n":1"#));
    assert!(lines[6].starts_with(r#"{"kind":"comment","issue":"asf-2","n":1"#));
    assert_eq!(lines.len(), 7);
    assert!(out.ends_with('\n'));
    assert!(!out.contains('\r'));
}

#[test]
fn export_is_deterministic_except_meta() {
    let (_d, s) = open();
    seed(&s);
    let a = export(&s);
    let b = export(&s);
    assert_eq!(body(&a), body(&b));
    let mut buf = Vec::new();
    s.export_jsonl(
        &mut buf,
        &ExportOpts {
            include_comments: false,
        },
    )
    .unwrap();
    let no_comments = String::from_utf8(buf).unwrap();
    assert!(!no_comments.contains(r#""kind":"comment""#));
    assert_eq!(no_comments.lines().count(), 4);
}

#[test]
fn import_roundtrip_equal_export() {
    let (_d, s) = open();
    seed(&s);
    let dump = export(&s);

    let dir2 = tempfile::tempdir().unwrap();
    let t = SqliteStore::open(&dir2.path().join("f.db")).unwrap();
    t.init_repo("D:/other", "asf", "main").unwrap();
    let report = t
        .import_jsonl(&mut Cursor::new(dump.as_bytes()), ImportMode::Upsert)
        .unwrap();
    assert_eq!(report.issues_created, 2);
    assert_eq!(report.issues_updated, 0);
    assert_eq!(report.deps, 1);
    assert_eq!(report.comments, 3);
    assert_eq!(body(&export(&t)), body(&dump));
    assert_eq!(t.repo().unwrap().next_seq, 3);
    // comments keep their reply chain
    let a: IssueId = "asf-1".parse().unwrap();
    let cs = t.comments(&a).unwrap();
    assert_eq!(cs[1].reply_to, Some(cs[0].id));
}

#[test]
fn validating_status_survives_export_and_import() {
    let (_d, s) = open();
    let issue = s
        .create_issue(&NewIssue {
            title: "under validation".into(),
            status: Status::Ready,
            ..NewIssue::default()
        })
        .unwrap();
    s.set_status(&issue.id, Status::Validating, "human").unwrap();
    let dump = export(&s);
    assert!(dump.contains(r#""status":"validating""#), "{dump}");

    let dir2 = tempfile::tempdir().unwrap();
    let t = SqliteStore::open(&dir2.path().join("f.db")).unwrap();
    t.init_repo("D:/other", "asf", "main").unwrap();
    let report = t
        .import_jsonl(&mut Cursor::new(dump.as_bytes()), ImportMode::Upsert)
        .unwrap();
    assert_eq!(report.issues_created, 1);
    assert_eq!(
        t.get_issue(&issue.id).unwrap().unwrap().status,
        Status::Validating
    );
    assert_eq!(body(&export(&t)), body(&dump));
}

#[test]
fn import_is_idempotent_and_upsert_keeps_newer() {
    let (_d, s) = open();
    seed(&s);
    let dump = export(&s);
    let report = s
        .import_jsonl(&mut Cursor::new(dump.as_bytes()), ImportMode::Upsert)
        .unwrap();
    assert_eq!(report.issues_created, 0);
    assert_eq!(report.issues_updated, 0);
    assert_eq!(report.skipped, 2);
    assert_eq!(report.comments, 0, "existing comments are not duplicated");
    assert_eq!(s.comments(&"asf-1".parse().unwrap()).unwrap().len(), 2);

    // an incoming record with a newer updated_at wins
    let newer = dump
        .replace(r#""title":"a""#, r#""title":"a-renamed""#)
        .replace(r#""updated_at":"20"#, r#""updated_at":"29"#);
    let report = s
        .import_jsonl(&mut Cursor::new(newer.as_bytes()), ImportMode::Upsert)
        .unwrap();
    assert_eq!(report.issues_updated, 2);
    assert_eq!(
        s.get_issue(&"asf-1".parse().unwrap()).unwrap().unwrap().title,
        "a-renamed"
    );

    // an older record is skipped
    let older = dump
        .replace(r#""title":"a""#, r#""title":"stale""#)
        .replace(r#""updated_at":"20"#, r#""updated_at":"19"#);
    s.import_jsonl(&mut Cursor::new(older.as_bytes()), ImportMode::Upsert)
        .unwrap();
    assert_eq!(
        s.get_issue(&"asf-1".parse().unwrap()).unwrap().unwrap().title,
        "a-renamed"
    );
}

#[test]
fn replace_wipes_then_loads() {
    let (_d, s) = open();
    seed(&s);
    let dump = export(&s);
    s.create_issue(&NewIssue {
        title: "extra".into(),
        ..NewIssue::default()
    })
    .unwrap();
    assert_eq!(s.list_issues(&IssueFilter::default()).unwrap().len(), 3);
    let report = s
        .import_jsonl(&mut Cursor::new(dump.as_bytes()), ImportMode::Replace)
        .unwrap();
    assert_eq!(report.issues_created, 2);
    assert_eq!(s.list_issues(&IssueFilter::default()).unwrap().len(), 2);
    assert_eq!(s.repo().unwrap().next_seq, 4, "next_seq never goes backwards");
}

#[test]
fn import_rejects_garbage_atomically() {
    let (_d, s) = open();
    let bad = "{\"kind\":\"meta\",\"format\":1,\"prefix\":\"asf\",\"next_seq\":1,\"exported_at\":\"x\"}\n{not json\n";
    let e = s
        .import_jsonl(&mut Cursor::new(bad.as_bytes()), ImportMode::Upsert)
        .unwrap_err();
    assert!(matches!(e, StoreError::Validation(_)), "{e}");
    assert!(s.list_issues(&IssueFilter::default()).unwrap().is_empty());
    let wrong_format =
        "{\"kind\":\"meta\",\"format\":9,\"prefix\":\"asf\",\"next_seq\":1,\"exported_at\":\"x\"}\n";
    assert!(matches!(
        s.import_jsonl(&mut Cursor::new(wrong_format.as_bytes()), ImportMode::Upsert)
            .unwrap_err(),
        StoreError::Validation(_)
    ));
}
