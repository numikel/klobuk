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

fn comment(
    s: &SqliteStore,
    issue: &IssueId,
    author: &str,
    kind: CommentType,
    to: &str,
    body: &str,
) -> Comment {
    s.add_comment(&NewComment {
        issue_id: issue.clone(),
        author: author.into(),
        kind,
        to_lane: to.into(),
        body: body.into(),
        reply_to: None,
    })
    .unwrap()
}

fn ids(v: &[Comment]) -> Vec<i64> {
    v.iter().map(|c| c.id).collect()
}

#[test]
fn comment_roundtrip_and_order() {
    let (_d, s) = open();
    let a = ready_issue(&s, "a");
    let c1 = comment(&s, &a.id, "human", CommentType::Decision, "all", "use sqlite");
    let c2 = s
        .add_comment(&NewComment {
            issue_id: a.id.clone(),
            author: "agent:asf-1".into(),
            kind: CommentType::Answer,
            to_lane: "human".into(),
            body: "ack".into(),
            reply_to: Some(c1.id),
        })
        .unwrap();
    assert!(c2.id > c1.id);
    assert_eq!(c2.reply_to, Some(c1.id));
    let all = s.comments(&a.id).unwrap();
    assert_eq!(ids(&all), vec![c1.id, c2.id]);
    assert_eq!(all[0].kind, CommentType::Decision);
    assert!(all[0].created_at.ends_with('Z'));
}

#[test]
fn comment_validation() {
    let (_d, s) = open();
    let a = ready_issue(&s, "a");
    let e = s
        .add_comment(&NewComment {
            issue_id: "asf-9".parse().unwrap(),
            author: "human".into(),
            kind: CommentType::Fyi,
            to_lane: "all".into(),
            body: "x".into(),
            reply_to: None,
        })
        .unwrap_err();
    assert!(matches!(e, StoreError::NotFound(_)));
    let e = s
        .add_comment(&NewComment {
            issue_id: a.id.clone(),
            author: "human".into(),
            kind: CommentType::Fyi,
            to_lane: "all".into(),
            body: "   ".into(),
            reply_to: None,
        })
        .unwrap_err();
    assert!(matches!(e, StoreError::Validation(_)));
    let e = s
        .add_comment(&NewComment {
            issue_id: a.id.clone(),
            author: "human".into(),
            kind: CommentType::Fyi,
            to_lane: "all".into(),
            body: "x".into(),
            reply_to: Some(999),
        })
        .unwrap_err();
    assert!(matches!(e, StoreError::NotFound(_)));
}

#[test]
fn inbox_filters_by_relevance() {
    let (_d, s) = open();
    let a = ready_issue(&s, "a"); // claimed by lane asf-1
    let b = ready_issue(&s, "b"); // claimed by lane asf-2
    let c = ready_issue(&s, "c"); // unclaimed
    s.claim(
        &a.id,
        &NewClaim {
            worker: a.id.to_string(),
            ..NewClaim::default()
        },
    )
    .unwrap();
    s.claim(
        &b.id,
        &NewClaim {
            worker: b.id.to_string(),
            ..NewClaim::default()
        },
    )
    .unwrap();

    let own = comment(
        &s,
        &a.id,
        "agent:asf-2",
        CommentType::Question,
        "asf-1",
        "on my issue, addressed to me",
    );
    let addressed = comment(
        &s,
        &c.id,
        "agent:asf-2",
        CommentType::Fyi,
        "asf-1",
        "on other issue, to me",
    );
    let broadcast = comment(&s, &c.id, "agent:asf-2", CommentType::Decision, "all", "to all");
    let human = comment(&s, &c.id, "human", CommentType::Status, "asf-2", "human to asf-2");
    let _peer = comment(
        &s,
        &b.id,
        "agent:asf-2",
        CommentType::Status,
        "asf-2",
        "peer note, not for asf-1",
    );

    let inbox = s.inbox("asf-1", true, false).unwrap();
    assert_eq!(ids(&inbox), vec![own.id, addressed.id, broadcast.id, human.id]);

    let everything = s.inbox("asf-1", true, true).unwrap();
    assert_eq!(everything.len(), 5);
}

#[test]
fn inbox_cursor_advances_unless_peek() {
    let (_d, s) = open();
    let a = ready_issue(&s, "a");
    s.claim(
        &a.id,
        &NewClaim {
            worker: "asf-1".into(),
            ..NewClaim::default()
        },
    )
    .unwrap();
    comment(&s, &a.id, "human", CommentType::Question, "asf-1", "q1");
    assert_eq!(s.inbox("asf-1", true, false).unwrap().len(), 1, "peek");
    assert_eq!(s.inbox("asf-1", true, false).unwrap().len(), 1, "peek again");
    assert_eq!(s.inbox("asf-1", false, false).unwrap().len(), 1, "read");
    assert_eq!(s.inbox("asf-1", false, false).unwrap().len(), 0, "nothing new");
    comment(&s, &a.id, "human", CommentType::Question, "asf-1", "q2");
    let newer = s.inbox("asf-1", false, false).unwrap();
    assert_eq!(newer.len(), 1);
    assert_eq!(newer[0].body, "q2");
    // cursors are per lane: another lane still sees both
    assert_eq!(s.inbox("asf-7", true, true).unwrap().len(), 2);
}

#[test]
fn orchestrator_sees_all_and_own_comments_are_excluded() {
    let (_d, s) = open();
    let a = ready_issue(&s, "a");
    let b = ready_issue(&s, "b");
    comment(&s, &a.id, "agent:asf-1", CommentType::Status, "asf-2", "a to 2");
    comment(&s, &b.id, "agent:asf-2", CommentType::Status, "asf-1", "b to 1");
    comment(
        &s,
        &b.id,
        "orchestrator",
        CommentType::Fyi,
        "all",
        "from orchestrator",
    );
    let inbox = s.inbox(LANE_ORCHESTRATOR, true, false).unwrap();
    assert_eq!(inbox.len(), 2, "orchestrator does not see its own comments");
    assert!(inbox.iter().all(|c| c.author != "orchestrator"));
    // a lane never sees its own comments either
    let mine = s.inbox("asf-1", true, false).unwrap();
    let bodies: Vec<&str> = mine.iter().map(|c| c.body.as_str()).collect();
    assert_eq!(
        bodies,
        ["b to 1", "from orchestrator"],
        "own comment excluded, `to: all` included"
    );
}
