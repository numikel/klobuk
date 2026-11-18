//! The snapshot: what the loader reads from the store and the pure helpers the views share.

use super::*;
use crate::ctx::Ctx;
use crate::tui::snapshot::{
    fit, load_detail, load_snapshot, move_selection, text_width, trunc, ReviewLink, RuntimeState,
    REVIEW_STARTED, RUNTIME_DOWN_AFTER,
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
    let store = klobuk_store::SqliteStore::open_in_memory().unwrap();
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
    let s = load_snapshot(&ctx, None, &BTreeMap::new(), &RuntimeState::default()).unwrap();
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
    let s = load_snapshot(&ctx, None, &BTreeMap::new(), &RuntimeState::default()).unwrap();

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
    // `c` also has a failed merge item, but one issue gets one row: the run, which is what to act on first
    assert!(
        !kinds.contains(&(c.clone(), NeedSource::Merge(MergeState::Failed))),
        "{kinds:?}"
    );
    assert!(kinds
        .iter()
        .any(|(i, k)| *i == b && matches!(k, NeedSource::Question(_))));
    assert_eq!(kinds.len(), 2);
    let since: Vec<&str> = s.needs_you.iter().map(|n| n.since.as_str()).collect();
    assert!(since.windows(2).all(|w| w[0] <= w[1]), "oldest first: {since:?}");
    assert!(s.detail.is_none() && s.pane_preview.is_none(), "loaded on demand");
}

/// One issue for each kind of wait, and a failed merge item whose worker is fixing it (which waits
/// for nobody). Every issue has a single wait, so no row is folded into another.
fn every_wait() -> Ctx {
    let store = klobuk_store::SqliteStore::open_in_memory().unwrap();
    store.init_repo("D:/repo", "asf", "main").unwrap();
    let issue = |title: &str| {
        store
            .create_issue(&new_issue(title, IssueType::Task, None, &[]))
            .unwrap()
            .id
    };
    let run_in = |id: &IssueId, state: RunState| {
        let run = store.create_run(&new_run(id)).unwrap();
        let patch = RunPatch {
            state: Some(state),
            exit_reason: Some(format!("{state} on purpose")),
            ..RunPatch::default()
        };
        store.update_run(&run.id, &patch).unwrap();
    };
    let merge_in = |id: &IssueId, state: MergeState| {
        let item = store.enqueue_merge(id, None, "feat/x").unwrap();
        let patch = MergePatch {
            state: Some(state),
            last_error: Some(Some(format!("{state} on purpose"))),
            ..MergePatch::default()
        };
        store.update_merge(item.id, &patch).unwrap();
    };
    run_in(&issue("blocked run"), RunState::Blocked);
    let asked = issue("question");
    store
        .add_comment(&NewComment {
            issue_id: asked,
            author: "agent:asf-2".into(),
            kind: CommentType::Question,
            to_lane: LANE_HUMAN.into(),
            body: "Which crate?".into(),
            reply_to: None,
        })
        .unwrap();
    run_in(&issue("failed run"), RunState::Failed);
    merge_in(&issue("failed merge"), MergeState::Failed);
    run_in(&issue("stalled run"), RunState::Stalled);
    run_in(&issue("over budget run"), RunState::OverBudget);
    merge_in(&issue("blocked merge"), MergeState::Blocked);
    let fixing = issue("fixing after review");
    merge_in(&fixing, MergeState::Failed);
    run_in(&fixing, RunState::Working);
    Ctx {
        root: tempfile::tempdir().unwrap().keep(),
        config: Config::from_toml(Config::default_toml()).unwrap(),
        store,
        lane: LANE_HUMAN.into(),
    }
}

#[test]
fn the_reminders_and_the_snapshot_agree_on_what_waits_for_the_human() {
    use crate::commands::remind::collect;
    use klobuk_core::remind::{EventIndex, WaitKind};
    use std::collections::BTreeSet;

    let ctx = every_wait();
    let remind_set = |ctx: &Ctx| -> BTreeSet<(String, WaitKind)> {
        let mut index = EventIndex::default();
        for e in ctx.store.events_since(0, usize::MAX).unwrap() {
            index.absorb(&e);
        }
        collect(ctx, &index, time::OffsetDateTime::now_utc())
            .unwrap()
            .into_iter()
            .map(|w| (w.issue.to_string(), w.kind))
            .collect()
    };
    let snapshot_set = |ctx: &Ctx| -> BTreeSet<(String, WaitKind)> {
        let s = load_snapshot(ctx, None, &BTreeMap::new(), &RuntimeState::default()).unwrap();
        s.needs_you
            .iter()
            .map(|n| {
                let kind = match &n.source {
                    NeedSource::BlockedRun => WaitKind::RunBlocked,
                    NeedSource::Question(_) => WaitKind::Question,
                    NeedSource::Run(RunState::Failed) => WaitKind::RunFailed,
                    NeedSource::Run(RunState::Stalled) => WaitKind::RunStalled,
                    NeedSource::Run(_) => WaitKind::RunOverBudget,
                    NeedSource::Merge(MergeState::Failed) => WaitKind::MergeFailed,
                    NeedSource::Merge(_) => WaitKind::MergeBlocked,
                };
                (n.issue.to_string(), kind)
            })
            .collect()
    };

    let (reminders, rows) = (remind_set(&ctx), snapshot_set(&ctx));
    assert_eq!(reminders, rows, "one rule, two readers");
    let kinds: BTreeSet<WaitKind> = rows.iter().map(|(_, k)| *k).collect();
    assert_eq!(
        kinds,
        BTreeSet::from([
            WaitKind::RunBlocked,
            WaitKind::Question,
            WaitKind::RunFailed,
            WaitKind::MergeFailed,
            WaitKind::RunStalled,
            WaitKind::RunOverBudget,
            WaitKind::MergeBlocked,
        ]),
        "every kind a key acts on, and not the merge item its worker is fixing"
    );

    // What only the reminders announce: a blocked issue and a BLOCKER comment have no key to press
    let blocked = ctx
        .store
        .create_issue(&NewIssue {
            title: "blocked".into(),
            status: Status::Blocked,
            ..NewIssue::default()
        })
        .unwrap()
        .id;
    ctx.store
        .add_comment(&NewComment {
            issue_id: blocked.clone(),
            author: "agent:asf-9".into(),
            kind: CommentType::Blocker,
            to_lane: LANE_HUMAN.into(),
            body: "scope too narrow".into(),
            reply_to: None,
        })
        .unwrap();
    assert_eq!(
        snapshot_set(&ctx),
        rows,
        "the snapshot lists only what a key acts on"
    );
    let extra: BTreeSet<_> = remind_set(&ctx).difference(&reminders).cloned().collect();
    assert_eq!(
        extra,
        BTreeSet::from([
            (blocked.to_string(), WaitKind::IssueBlocked),
            (blocked.to_string(), WaitKind::Blocker),
        ])
    );
}

#[test]
fn the_loader_carries_whether_a_review_is_required_and_the_labels_that_ask_for_one() {
    let (mut ctx, _) = story();
    let s = load_snapshot(&ctx, None, &BTreeMap::new(), &RuntimeState::default()).unwrap();
    assert!(
        !s.review_required && s.review_on_labels.is_empty(),
        "off by default"
    );
    ctx.config.review.required = true;
    ctx.config.review.on_labels = vec!["risky".into()];
    let s = load_snapshot(&ctx, None, &BTreeMap::new(), &RuntimeState::default()).unwrap();
    assert!(s.review_required);
    assert_eq!(s.review_on_labels, ["risky"]);
}

#[test]
fn the_loader_carries_whether_the_gates_run_on_main_after_a_merge() {
    let (mut ctx, _) = story();
    let s = load_snapshot(&ctx, None, &BTreeMap::new(), &RuntimeState::default()).unwrap();
    assert!(!s.verify_main_after, "off by default");
    ctx.config.merge.verify_main_after = true;
    let s = load_snapshot(&ctx, None, &BTreeMap::new(), &RuntimeState::default()).unwrap();
    assert!(s.verify_main_after);
}

#[test]
fn a_value_changed_in_config_toml_shows_up_in_the_next_snapshot_without_a_restart() {
    let (ctx, _) = story();
    let path = ctx.root.join(".factory/config.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    // the Ctx was loaded with the defaults; the file says otherwise
    std::fs::write(
        &path,
        "[scheduler]\nmax_parallel = 7\n[review]\nrequired = true\nmodel = \"sonnet\"\non_labels = [\"risky\"]\n[merge]\nverify_main_after = true\n[repo]\ndefault_branch = \"trunk\"\n",
    )
    .unwrap();
    let s = load_snapshot(&ctx, None, &BTreeMap::new(), &RuntimeState::default()).unwrap();
    assert_eq!(s.max_parallel, 7, "the header's workers n/m");
    assert!(s.review_required && s.verify_main_after);
    assert_eq!((s.review_model.as_str(), s.branch.as_str()), ("sonnet", "trunk"));
    assert_eq!(s.review_on_labels, ["risky"]);
    let row = |s: &Snapshot, k: &str| s.config.iter().find(|r| r.key == k).unwrap().raw.clone();
    assert_eq!(row(&s, "scheduler.max_parallel"), "7");

    // an edit made while the TUI runs (`klobuk config set`, an editor) is picked up by the next refresh
    std::fs::write(&path, "[scheduler]\nmax_parallel = 2\n").unwrap();
    let s = load_snapshot(&ctx, None, &BTreeMap::new(), &RuntimeState::default()).unwrap();
    assert_eq!(s.max_parallel, 2);
    assert!(!s.review_required && !s.verify_main_after);
    assert_eq!(row(&s, "scheduler.max_parallel"), "2");

    // a file that does not parse keeps the settings the TUI started with and says so in the rows
    std::fs::write(&path, "[scheduler\nmax_parallel = 9\n").unwrap();
    let s = load_snapshot(&ctx, None, &BTreeMap::new(), &RuntimeState::default()).unwrap();
    assert_eq!(s.max_parallel, ctx.config.scheduler.max_parallel);
    assert_eq!(s.config[0].value, "(unreadable)");
}

#[test]
fn the_model_of_a_run_follows_the_config_file_too() {
    let (ctx, [a, _, _, r]) = story();
    ctx.store.create_run(&new_run(&r)).unwrap();
    let path = ctx.root.join(".factory/config.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        "[worker]\nmodel = \"haiku\"\n[review]\nmodel = \"sonnet\"\n",
    )
    .unwrap();
    let s = load_snapshot(&ctx, None, &BTreeMap::new(), &RuntimeState::default()).unwrap();
    assert_eq!(s.last_run(&a).unwrap().model.as_deref(), Some("haiku"));
    assert_eq!(
        s.last_run(&r).and_then(|l| l.model.clone()).as_deref(),
        Some("sonnet")
    );
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
    let mut app = App::new(load_snapshot(&ctx, None, &BTreeMap::new(), &RuntimeState::default()).unwrap());
    app.snap.detail = Some(load_detail(&ctx, &a).unwrap());
    app.set_pane_preview(a.clone(), "last lines".into());
    app.replace(load_snapshot(&ctx, None, &BTreeMap::new(), &RuntimeState::default()).unwrap());
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

/// A runtime whose `health` answers after `delay`, failing when the script says so (an empty script
/// answers). Nothing else is used by the loader's probe, so the rest refuses.
struct ProbeRuntime {
    delay: Duration,
    answers: std::sync::Mutex<std::collections::VecDeque<bool>>,
}

impl ProbeRuntime {
    fn new(delay: Duration, answers: &[bool]) -> Self {
        ProbeRuntime {
            delay,
            answers: std::sync::Mutex::new(answers.iter().copied().collect()),
        }
    }
}

fn unused<T>() -> Result<T, RuntimeError> {
    Err(RuntimeError::Io("unused".into()))
}

impl AgentRuntime for ProbeRuntime {
    fn health(&self) -> Result<RuntimeInfo, RuntimeError> {
        std::thread::sleep(self.delay);
        if self.answers.lock().unwrap().pop_front().unwrap_or(true) {
            Ok(RuntimeInfo {
                name: "probe".into(),
                version: "0".into(),
                protocol: None,
            })
        } else {
            Err(RuntimeError::Io("herdr status exited 1: socket busy".into()))
        }
    }
    fn open_workspace(&self, _: &OpenWorkspace) -> Result<WorkspaceRef, RuntimeError> {
        unused()
    }
    fn start_agent(&self, _: &StartAgent) -> Result<AgentInfo, RuntimeError> {
        unused()
    }
    fn prompt(&self, _: &AgentHandle, _: &str, _: &[AgentState], _: u64) -> Result<AgentInfo, RuntimeError> {
        unused()
    }
    fn wait(&self, _: &AgentHandle, _: &[AgentState], _: u64) -> Result<AgentInfo, RuntimeError> {
        unused()
    }
    fn info(&self, _: &AgentHandle) -> Result<AgentInfo, RuntimeError> {
        unused()
    }
    fn read(&self, _: &AgentHandle, _: u32) -> Result<String, RuntimeError> {
        unused()
    }
    fn send_keys(&self, _: &AgentHandle, _: &[&str]) -> Result<(), RuntimeError> {
        unused()
    }
    fn focus(&self, _: &AgentHandle) -> Result<(), RuntimeError> {
        unused()
    }
    fn report(&self, _: &str, _: Option<&str>, _: &[(String, String)]) -> Result<(), RuntimeError> {
        unused()
    }
    fn close_workspace(&self, _: &str) -> Result<(), RuntimeError> {
        unused()
    }
    fn notify(&self, _: &str, _: &str, _: Option<&str>) -> Result<(), RuntimeError> {
        unused()
    }
}

fn header_text(s: &Snapshot) -> String {
    render_to_text(&App::new(s.clone()), 120, 30)
        .lines()
        .next()
        .unwrap()
        .to_string()
}

#[test]
fn a_slow_runtime_that_answers_is_shown_as_ok() {
    let (ctx, _) = story();
    let rt = ProbeRuntime::new(Duration::from_millis(300), &[]);
    let s = load_snapshot(&ctx, Some(&rt), &BTreeMap::new(), &RuntimeState::default()).unwrap();
    assert_eq!(s.runtime_ok, Some(true));
    assert!(header_text(&s).contains("runtime ok"), "{}", header_text(&s));
}

#[test]
fn a_refresh_that_skips_the_probe_keeps_the_last_known_state() {
    // the event loop reads the store every second but probes the runtime every two
    let (ctx, _) = story();
    let up = RuntimeState {
        ok: Some(true),
        ..RuntimeState::default()
    };
    let s = load_snapshot(&ctx, None, &BTreeMap::new(), &up).unwrap();
    assert_eq!(s.runtime_ok, Some(true));
    assert!(header_text(&s).contains("runtime ok"));
    // nothing probed yet: still unknown
    let s = load_snapshot(&ctx, None, &BTreeMap::new(), &RuntimeState::default()).unwrap();
    assert_eq!(s.runtime_ok, None);
    assert!(header_text(&s).contains("runtime ?"));
}

#[test]
fn the_runtime_is_down_only_after_several_failed_probes_in_a_row_and_the_reason_is_kept() {
    let (ctx, _) = story();
    let mut script = vec![true];
    script.extend(std::iter::repeat_n(false, RUNTIME_DOWN_AFTER as usize));
    script.push(true);
    let rt = ProbeRuntime::new(Duration::ZERO, &script);
    let mut state = RuntimeState::default();
    let mut shown = Vec::new();
    for _ in 0..script.len() {
        let s = load_snapshot(&ctx, Some(&rt), &BTreeMap::new(), &state).unwrap();
        shown.push((s.runtime_ok, s.runtime_failures, s.runtime_reason.clone()));
        state = s.runtime_state();
    }
    let reason = Some("io: herdr status exited 1: socket busy".to_string());
    let mut want = vec![(Some(true), 0, None)];
    for n in 1..RUNTIME_DOWN_AFTER {
        want.push((Some(true), n, reason.clone())); // a miss keeps the last known state
    }
    want.push((Some(false), RUNTIME_DOWN_AFTER, reason));
    want.push((Some(true), 0, None)); // an answer ends the outage
    assert_eq!(shown, want);
}

#[test]
fn a_runtime_that_never_answered_stays_unknown_until_it_has_failed_enough() {
    let s = RuntimeState::default();
    let once = s.observe(Some(Err("x".into())));
    assert_eq!((once.ok, once.failures), (None, 1));
    let down = (1..RUNTIME_DOWN_AFTER).fold(once, |st, _| st.observe(Some(Err("x".into()))));
    assert_eq!(down.ok, Some(false));
    assert_eq!(down.observe(None), down, "a skipped probe changes nothing");
}
