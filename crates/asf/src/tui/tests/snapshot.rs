//! The snapshot: what the loader reads from the store and the pure helpers the views share.

use super::*;
use crate::ctx::Ctx;
use crate::tui::snapshot::{
    fit, load_detail, load_snapshot, move_selection, text_width, trunc, ReviewLink, REVIEW_STARTED,
};

fn new_issue(title: &str, kind: IssueType, parent: Option<&IssueId>, deps: &[&IssueId]) -> NewIssue {
    NewIssue {
        title: title.into(),
        kind,
        parent_id: parent.cloned(),
        deps: deps.iter().map(|d| (*d).clone()).collect(),
        ..NewIssue::default()
    }
}

fn new_run(issue: &IssueId) -> NewRun {
    NewRun {
        issue_id: issue.clone(),
        runtime: "fake".into(),
        worktree: format!(".worktrees/{issue}"),
        branch: format!("feat/{issue}"),
        base_sha: "base".into(),
        budget_usd: Some(10.0),
    }
}

/// An in-memory store with the story of one afternoon: `a` ran, finished and merged today; `b`
/// depends on `a`; `c` failed twice over (its run and its merge); `r` is the reviewer of `a`.
fn story() -> (Ctx, [IssueId; 4]) {
    let store = asf_store::SqliteStore::open_in_memory().unwrap();
    store.init_repo("D:/repo", "asf", "main").unwrap();
    let a = store
        .create_issue(&new_issue("Build it", IssueType::Task, None, &[]))
        .unwrap()
        .id;
    let b = store
        .create_issue(&new_issue("Use it", IssueType::Task, None, &[&a]))
        .unwrap()
        .id;
    let c = store
        .create_issue(&new_issue("Break it", IssueType::Task, None, &[]))
        .unwrap()
        .id;
    let r = store
        .create_issue(&new_issue("Review a", IssueType::Review, Some(&a), &[]))
        .unwrap()
        .id;

    let run_a = store.create_run(&new_run(&a)).unwrap();
    store
        .update_run(
            &run_a.id,
            &RunPatch {
                state: Some(RunState::Succeeded),
                cost_usd: Some(1.5),
                tool_calls: Some(4),
                pane_id: Some("p1".into()),
                finished_at: Some("2026-11-06T10:00:00Z".into()),
                ..RunPatch::default()
            },
        )
        .unwrap();
    let run_c = store.create_run(&new_run(&c)).unwrap();
    store
        .update_run(
            &run_c.id,
            &RunPatch {
                state: Some(RunState::Failed),
                exit_reason: Some("gate failed".into()),
                finished_at: Some("2026-11-06T10:05:00Z".into()),
                ..RunPatch::default()
            },
        )
        .unwrap();

    let merged = store.enqueue_merge(&a, Some(&run_a.id), "feat/asf-1").unwrap();
    store
        .update_merge(
            merged.id,
            &MergePatch {
                state: Some(MergeState::Merged),
                head_sha: Some("abc1234".into()),
                ..MergePatch::default()
            },
        )
        .unwrap();
    let failed = store.enqueue_merge(&c, Some(&run_c.id), "feat/asf-3").unwrap();
    store
        .update_merge(
            failed.id,
            &MergePatch {
                state: Some(MergeState::Failed),
                last_error: Some(Some("rebase conflict".into())),
                ..MergePatch::default()
            },
        )
        .unwrap();
    for round in [1, 2] {
        store
            .append_event(&NewEvent {
                kind: REVIEW_STARTED.into(),
                issue_id: Some(a.clone()),
                run_id: None,
                actor: None,
                payload: serde_json::json!({ "round": round }),
            })
            .unwrap();
    }
    store
        .add_comment(&NewComment {
            issue_id: b.clone(),
            author: "agent:asf-2".into(),
            kind: CommentType::Question,
            to_lane: LANE_HUMAN.into(),
            body: "Which crate?".into(),
            reply_to: None,
        })
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx {
        root: dir.keep(),
        config: Config::from_toml(Config::default_toml()).unwrap(),
        store,
        lane: LANE_HUMAN.into(),
    };
    (ctx, [a, b, c, r])
}

#[test]
fn the_loader_carries_every_run_not_only_the_latest_one_per_issue() {
    let (ctx, [a, _b, c, _r]) = story();
    // a second, failed attempt at `a`: `last_runs` keeps one row for it, `all_runs` both
    let again = ctx.store.create_run(&new_run(&a)).unwrap();
    ctx.store
        .update_run(
            &again.id,
            &RunPatch {
                state: Some(RunState::Failed),
                cost_usd: Some(0.5),
                ..RunPatch::default()
            },
        )
        .unwrap();
    let s = load_snapshot(&ctx, None, &BTreeMap::new()).unwrap();
    assert!(s.runs.is_empty(), "no run is active");
    assert_eq!(s.all_runs.len(), 3);
    let of = |id: &IssueId| s.all_runs.iter().filter(|r| &r.issue_id == id).count();
    assert_eq!((of(&a), of(&c)), (2, 1));
    let spent: f64 = s.all_runs.iter().map(|r| r.cost_usd).sum();
    assert!((spent - 2.0).abs() < 1e-9, "{spent}");
    assert_eq!(s.last_runs.iter().filter(|l| l.issue == a).count(), 1);
}

#[test]
fn the_loader_carries_deps_the_latest_run_per_issue_merged_today_review_links_and_needs_you() {
    let (ctx, [a, b, c, r]) = story();
    let s = load_snapshot(&ctx, None, &BTreeMap::new()).unwrap();

    assert_eq!(
        s.deps,
        vec![Dep {
            issue_id: b.clone(),
            depends_on: a.clone()
        }]
    );

    // the latest run of every issue, a finished one included (`runs` holds active runs only)
    assert!(s.runs.is_empty());
    let last = s.last_run(&a).expect("a finished run is listed");
    assert_eq!(last.state, RunState::Succeeded);
    assert_eq!((last.cost, last.budget, last.tool_calls), (1.5, Some(10.0), 4));
    assert_eq!(last.pane.as_deref(), Some("p1"));
    assert_eq!(last.finished_at.as_deref(), Some("2026-11-06T10:00:00Z"));
    assert!(last.started_at.starts_with("20"));
    assert_eq!(last.model, ctx.config.worker.model);
    assert_eq!(s.last_run(&c).unwrap().state, RunState::Failed);
    assert!(s.last_run(&b).is_none(), "an issue that never ran has no run");

    // merged today, with the head sha; the failed item is not merged and not terminal
    assert_eq!(s.merged_today.len(), 1);
    assert_eq!(s.merged_today[0].issue_id, a);
    assert_eq!(s.merged_today[0].head_sha.as_deref(), Some("abc1234"));
    assert_eq!(s.queue.len(), 1, "only the failed item is still in the queue");

    // the reviewer of `a` and the newest round
    assert_eq!(
        s.review_of.get(&r),
        Some(&ReviewLink {
            reviews: a.clone(),
            round: Some(2)
        })
    );
    assert!(!s.review_of.contains_key(&a));

    // one list for everything that needs the human, oldest first
    let kinds: Vec<(IssueId, NeedSource)> = s
        .needs_you
        .iter()
        .map(|n| (n.issue.clone(), n.source.clone()))
        .collect();
    assert!(
        kinds.contains(&(c.clone(), NeedSource::Run(RunState::Failed))),
        "{kinds:?}"
    );
    assert!(
        kinds.contains(&(c.clone(), NeedSource::Merge(MergeState::Failed))),
        "{kinds:?}"
    );
    assert!(kinds
        .iter()
        .any(|(i, k)| *i == b && matches!(k, NeedSource::Question(_))));
    assert_eq!(kinds.len(), 3);
    let since: Vec<&str> = s.needs_you.iter().map(|n| n.since.as_str()).collect();
    assert!(since.windows(2).all(|w| w[0] <= w[1]), "oldest first: {since:?}");
    assert!(s.detail.is_none() && s.pane_preview.is_none(), "loaded on demand");
}

#[test]
fn the_loader_carries_whether_a_review_is_required_and_the_labels_that_ask_for_one() {
    let (mut ctx, _) = story();
    let s = load_snapshot(&ctx, None, &BTreeMap::new()).unwrap();
    assert!(
        !s.review_required && s.review_on_labels.is_empty(),
        "off by default"
    );
    ctx.config.review.required = true;
    ctx.config.review.on_labels = vec!["risky".into()];
    let s = load_snapshot(&ctx, None, &BTreeMap::new()).unwrap();
    assert!(s.review_required);
    assert_eq!(s.review_on_labels, ["risky"]);
}

#[test]
fn the_loader_carries_whether_the_gates_run_on_main_after_a_merge() {
    let (mut ctx, _) = story();
    let s = load_snapshot(&ctx, None, &BTreeMap::new()).unwrap();
    assert!(!s.verify_main_after, "off by default");
    ctx.config.merge.verify_main_after = true;
    let s = load_snapshot(&ctx, None, &BTreeMap::new()).unwrap();
    assert!(s.verify_main_after);
}

#[test]
fn the_detail_of_an_issue_is_loaded_on_demand_with_comments_and_dependency_status() {
    let (ctx, [a, b, ..]) = story();
    let d = load_detail(&ctx, &b).unwrap();
    assert_eq!(d.issue, b);
    assert_eq!(d.comments.len(), 1);
    assert_eq!(d.comments[0].body, "Which crate?");
    assert_eq!(d.deps, vec![(a.clone(), Status::Backlog)]);
    assert!(load_detail(&ctx, &a).unwrap().deps.is_empty());
}

#[test]
fn what_was_loaded_on_demand_survives_a_refresh() {
    let (ctx, [a, ..]) = story();
    let mut app = App::new(load_snapshot(&ctx, None, &BTreeMap::new()).unwrap());
    app.snap.detail = Some(load_detail(&ctx, &a).unwrap());
    app.set_pane_preview(a.clone(), "last lines".into());
    app.replace(load_snapshot(&ctx, None, &BTreeMap::new()).unwrap());
    assert_eq!(app.snap.detail_of(&a).map(|d| d.issue.clone()), Some(a.clone()));
    assert_eq!(app.snap.preview_of(&a), Some("last lines"));
}

#[test]
fn a_refresh_asks_again_for_the_detail_of_the_issue_the_board_has_open() {
    use crate::tui::snapshot::IssueDetail;
    let mut a = app(); // the Board, its panel open on the selected issue
    a.handle_key(key('j')); // off the group header, onto the first issue
    assert!(a.board.detail);
    let open = crate::tui::views::board::selected_issue(&a.snap, &a.board).expect("a selection");
    let detail = |comments: usize| IssueDetail {
        issue: open.clone(),
        comments: (0..comments)
            .map(|n| {
                question(
                    n as i64 + 1,
                    open.as_ref(),
                    "Which crate?",
                    "2026-11-06T10:00:00Z",
                )
            })
            .collect(),
        deps: vec![],
    };
    a.set_detail(detail(0));
    assert_eq!(a.detail_wanted(), None, "fresh: nothing to load");

    // a store refresh: the comments and dependency status may have changed under the selection
    a.replace(snapshot());
    assert_eq!(
        a.snap.detail_of(&open).map(|d| d.comments.len()),
        Some(0),
        "the old detail stays on screen until the new one is there"
    );
    assert_eq!(a.detail_wanted(), Some(open.clone()), "a refresh makes it stale");

    a.set_detail(detail(1));
    assert_eq!(a.detail_wanted(), None, "loaded again");
    assert_eq!(a.snap.detail_of(&open).map(|d| d.comments.len()), Some(1));

    // only the Board shows a detail, so only the Board reloads it
    a.replace(snapshot());
    assert_eq!(a.detail_wanted(), Some(open.clone()));
    a.handle_key(key('3'));
    assert_eq!(a.detail_wanted(), None, "not the Board");
}

#[test]
fn a_snapshot_built_by_hand_gets_its_review_round_and_needs_you_from_derive() {
    let mut s = working_desk();
    s.review_of.clear();
    s.derive();
    assert_eq!(
        s.review_of.get(&id("asf-31")),
        Some(&ReviewLink {
            reviews: id("asf-24"),
            round: Some(1)
        })
    );
    let mut s = busy_desk();
    s.derive();
    assert_eq!(s.needs_you, needs(&s));
}

#[test]
fn truncation_counts_display_columns_and_never_overflows() {
    assert_eq!(trunc("short", 10), "short");
    assert_eq!(trunc("0123456789", 6), "01234…");
    assert_eq!(trunc("first\nsecond", 20), "first", "only the first line");
    // wide characters take two columns each
    assert_eq!(text_width("日本語"), 6);
    let cut = trunc("日本語日本語", 7);
    assert_eq!(cut, "日本語…");
    assert!(text_width(&cut) <= 7);
    assert_eq!(trunc("日本語", 0), "");
    for n in 0..12 {
        assert!(
            text_width(&trunc("zażółć gęślą jaźń 日本語", n)) <= n,
            "width {n}"
        );
        assert_eq!(text_width(&fit("zażółć 日本語", n)), n, "fit to {n}");
    }
}

#[test]
fn list_navigation_clamps_and_survives_an_empty_list() {
    let nav = |k: KeyEvent, sel: usize, rows: usize| move_selection(&k, sel, rows);
    assert_eq!(nav(key('j'), 0, 3), Some(1));
    assert_eq!(nav(key('j'), 2, 3), Some(2));
    assert_eq!(nav(key('k'), 0, 3), Some(0));
    assert_eq!(nav(key('G'), 0, 3), Some(2));
    assert_eq!(nav(key('g'), 2, 3), Some(0));
    assert_eq!(
        nav(code(KeyCode::Down), 9, 3),
        Some(2),
        "a stale index is clamped first"
    );
    assert_eq!(nav(key('j'), 0, 0), Some(0));
    assert_eq!(nav(key('x'), 0, 3), None, "not a navigation key");
}
