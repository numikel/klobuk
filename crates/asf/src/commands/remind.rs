//! Reminders for `asf serve`: collect what waits for the human, ask `asf_core::remind::due` what is
//! due, notify through the runtime port, run the optional `[notify] command` and record the reminder
//! as an `attention.reminded` event.
//!
//! Only the serve loop calls this. The decision itself is pure and lives in `asf_core::remind`; this
//! module is the IO around it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use asf_core::remind::{self, EventIndex, WaitKind, Waiting, EVENT_REMINDED};
use asf_core::{
    AgentRuntime, Comment, CommentType, Issue, IssueFilter, IssueId, IssueStore, MergeState, NewEvent, Run,
    RunState, Status, LANE_ORCHESTRATOR,
};
use serde::Serialize;
use time::OffsetDateTime;

use crate::ctx::Ctx;
use crate::output::AppError;
use crate::tui::app::unanswered_questions;

/// How long `[notify] command` may run before it is killed.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// Events read from the store per round when catching up.
const EVENT_PAGE: usize = 2000;

/// What `asf serve` remembers between ticks: how far it has read the event log and what that log
/// says. A fresh process starts at the beginning, which is how a restart recovers the times of the
/// reminders already sent.
#[derive(Debug, Default)]
pub struct RemindState {
    cursor: u64,
    index: EventIndex,
}

impl RemindState {
    pub fn new() -> RemindState {
        RemindState::default()
    }

    /// Reads the events appended since the last call.
    fn sync(&mut self, store: &impl IssueStore) -> Result<(), AppError> {
        loop {
            let events = store.events_since(self.cursor, EVENT_PAGE)?;
            let Some(last) = events.last() else {
                return Ok(());
            };
            self.cursor = last.seq;
            for e in &events {
                self.index.absorb(e);
            }
            if events.len() < EVENT_PAGE {
                return Ok(());
            }
        }
    }
}

/// One reminder sent in a tick, as the tick report lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Sent {
    pub issue: IssueId,
    pub kind: WaitKind,
    pub age_minutes: u64,
    pub text: String,
    /// Why the runtime notification failed; `null` when it went out.
    pub notify_error: Option<String>,
    /// Why `[notify] command` failed or timed out; `null` when it succeeded or none is set.
    pub command_error: Option<String>,
}

/// Sends the reminders that are due now. A failing notification or command is recorded in the
/// returned [`Sent`] and never turns into an error; only a store failure does.
pub fn send_due(
    ctx: &Ctx,
    rt: &dyn AgentRuntime,
    state: &mut RemindState,
    now: OffsetDateTime,
) -> Result<Vec<Sent>, AppError> {
    let cfg = &ctx.config.notify;
    if cfg.after_minutes == 0 {
        return Ok(Vec::new());
    }
    state.sync(&ctx.store)?;
    let waiting = collect(ctx, &state.index)?;
    let due = remind::due(&waiting, &state.index.reminded, now, cfg);

    let mut sent = Vec::new();
    for r in due {
        let notify_error = rt
            .notify(&format!("asf {} needs you", r.issue), &r.text, Some("request"))
            .err()
            .map(|e| e.to_string());
        let command_error = if cfg.command.is_empty() {
            None
        } else {
            let env = [
                ("ASF_REMIND_ISSUE", r.issue.to_string()),
                ("ASF_REMIND_KIND", r.kind.to_string()),
                ("ASF_REMIND_AGE_MINUTES", r.age_minutes.to_string()),
                ("ASF_REMIND_TEXT", r.text.clone()),
            ];
            run_command(&cfg.command, &env, &ctx.root, COMMAND_TIMEOUT).err()
        };
        ctx.store.append_event(&NewEvent {
            kind: EVENT_REMINDED.into(),
            issue_id: Some(r.issue.clone()),
            run_id: None,
            actor: Some(LANE_ORCHESTRATOR.into()),
            payload: serde_json::json!({ "kind": r.kind, "age_minutes": r.age_minutes }),
        })?;
        // Counted from now on, whatever timestamp the store gave the event.
        state
            .index
            .reminded
            .insert(r.issue.clone(), asf_core::now_rfc3339());
        sent.push(Sent {
            issue: r.issue,
            kind: r.kind,
            age_minutes: r.age_minutes,
            text: r.text,
            notify_error,
            command_error,
        });
    }
    Ok(sent)
}

/// Everything that waits for the human right now, with the time it began to wait.
fn collect(ctx: &Ctx, index: &EventIndex) -> Result<Vec<Waiting>, AppError> {
    let store = &ctx.store;
    let live: BTreeMap<IssueId, Issue> = store
        .list_issues(&IssueFilter::default())?
        .into_iter()
        .filter(|i| !i.status.is_terminal())
        .map(|i| (i.id.clone(), i))
        .collect();

    let mut comments: BTreeMap<IssueId, Vec<Comment>> = BTreeMap::new();
    let mut out = Vec::new();
    for issue in live.values() {
        let list = store.comments(&issue.id)?;
        let open: BTreeSet<i64> = unanswered_questions(&list).into_iter().collect();
        for q in list.iter().filter(|c| open.contains(&c.id)) {
            out.push(Waiting {
                issue: issue.id.clone(),
                kind: WaitKind::Question,
                since: q.created_at.clone(),
                reason: remind::first_line(&q.body),
            });
        }
        if issue.status == Status::Blocked {
            out.push(Waiting {
                issue: issue.id.clone(),
                kind: WaitKind::IssueBlocked,
                since: issue.updated_at.clone(),
                reason: last_blocker(&list),
            });
        }
        comments.insert(issue.id.clone(), list);
    }

    for m in store.merge_queue()? {
        let kind = match m.state {
            MergeState::Failed => WaitKind::MergeFailed,
            MergeState::Blocked => WaitKind::MergeBlocked,
            _ => continue,
        };
        // A worker that a reviewer sent back is fixing the findings (the item stays failed while its
        // run is active again): nothing waits for the human then. Same guard as `merge retry`.
        if live.contains_key(&m.issue_id) && store.runs(Some(&m.issue_id), true)?.is_empty() {
            out.push(Waiting {
                reason: m
                    .last_error
                    .as_deref()
                    .map(remind::first_line)
                    .unwrap_or_default(),
                issue: m.issue_id,
                kind,
                since: m.updated_at,
            });
        }
    }

    // Only the newest run of an issue counts: an older failed attempt is history once a retry exists.
    let mut newest: BTreeMap<IssueId, Run> = BTreeMap::new();
    for r in store.runs(None, false)? {
        newest.insert(r.issue_id.clone(), r); // ordered by start: the last one wins
    }
    for (issue, run) in newest {
        let Some(kind) = run_kind(run.state) else {
            continue;
        };
        if !live.contains_key(&issue) {
            continue;
        }
        let since = index
            .run_entered(&run.id, run.state.as_str())
            .map(str::to_string)
            .or_else(|| run.finished_at.clone())
            .or_else(|| run.last_seen_at.clone())
            .unwrap_or_else(|| run.started_at.clone());
        let reason = run
            .exit_reason
            .as_deref()
            .map(remind::first_line)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| comments.get(&issue).map(|c| last_blocker(c)).unwrap_or_default());
        out.push(Waiting {
            issue,
            kind,
            since,
            reason,
        });
    }
    Ok(out)
}

fn run_kind(state: RunState) -> Option<WaitKind> {
    match state {
        RunState::Blocked => Some(WaitKind::RunBlocked),
        RunState::Stalled => Some(WaitKind::RunStalled),
        RunState::OverBudget => Some(WaitKind::RunOverBudget),
        RunState::Failed => Some(WaitKind::RunFailed),
        _ => None,
    }
}

/// First line of the newest BLOCKER comment; empty when there is none.
fn last_blocker(comments: &[Comment]) -> String {
    comments
        .iter()
        .filter(|c| c.kind == CommentType::Blocker)
        .max_by_key(|c| c.id)
        .map(|c| remind::first_line(&c.body))
        .unwrap_or_default()
}

/// Runs `argv` without a shell in `cwd` with `env` added, and waits at most `timeout`.
/// The error is a short sentence for the tick report.
pub(crate) fn run_command(
    argv: &[String],
    env: &[(&str, String)],
    cwd: &Path,
    timeout: Duration,
) -> Result<(), String> {
    let Some((program, args)) = argv.split_first() else {
        return Err("no command".into());
    };
    let mut child = Command::new(program)
        .args(args)
        .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start `{program}`: {e}"))?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(match status.code() {
                    Some(code) => format!("`{program}` exited with code {code}"),
                    None => format!("`{program}` was terminated by a signal"),
                })
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "`{program}` timed out after {} s",
                    timeout.as_secs_f64().ceil()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => return Err(format!("cannot wait for `{program}`: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The test binary doubles as the command to run: `helper_*` do nothing unless their variable is
    // set, so a normal `cargo test` passes them without effect. That keeps these tests free of a
    // shell and of any platform-specific program.
    const DUMP_FILE: &str = "remind-env.txt";
    const SLEEP: &str = "ASF_TEST_SLEEP_SECS";
    const FAIL: &str = "ASF_TEST_EXIT_NONZERO";

    #[test]
    fn helper_dump_env() {
        // Acts only when run as a reminder command (ASF_REMIND_ISSUE set); writes into the cwd.
        if std::env::var_os("ASF_REMIND_ISSUE").is_some() {
            let get = |k: &str| std::env::var(k).unwrap_or_default();
            let text = format!(
                "{}|{}|{}|{}",
                get("ASF_REMIND_ISSUE"),
                get("ASF_REMIND_KIND"),
                get("ASF_REMIND_AGE_MINUTES"),
                get("ASF_REMIND_TEXT")
            );
            std::fs::write(DUMP_FILE, text).unwrap();
        }
    }

    #[test]
    fn helper_sleep() {
        if let Ok(s) = std::env::var(SLEEP) {
            std::thread::sleep(Duration::from_secs(s.parse().unwrap()));
        }
    }

    #[test]
    fn helper_fail() {
        assert!(std::env::var_os(FAIL).is_none(), "asked to fail");
    }

    /// `[exe, "--exact", "commands::remind::tests::<name>"]`
    fn helper(name: &str) -> Vec<String> {
        let exe = std::env::current_exe().unwrap().to_string_lossy().into_owned();
        vec![exe, "--exact".into(), format!("commands::remind::tests::{name}")]
    }

    #[test]
    fn the_command_gets_the_four_variables_and_no_shell() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join(DUMP_FILE);
        let env = [
            ("ASF_REMIND_ISSUE", "asf-27".to_string()),
            ("ASF_REMIND_KIND", "issue_blocked".to_string()),
            ("ASF_REMIND_AGE_MINUTES", "45".to_string()),
            (
                "ASF_REMIND_TEXT",
                "asf-27 has been blocked for 45 min: a & b | c > d".to_string(),
            ),
        ];
        run_command(
            &helper("helper_dump_env"),
            &env,
            dir.path(),
            Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(out).unwrap(),
            "asf-27|issue_blocked|45|asf-27 has been blocked for 45 min: a & b | c > d",
            "shell metacharacters in the text arrive untouched"
        );
    }

    #[test]
    fn a_missing_program_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let e = run_command(
            &["asf-no-such-program-xyz".to_string()],
            &[],
            dir.path(),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(e.contains("cannot start `asf-no-such-program-xyz`"), "{e}");
    }

    #[test]
    fn a_failing_command_reports_its_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let env = [(FAIL, "1".to_string())];
        let e = run_command(&helper("helper_fail"), &env, dir.path(), Duration::from_secs(30)).unwrap_err();
        assert!(e.contains("exited with code"), "{e}");
    }

    #[test]
    fn a_command_that_overruns_is_killed_at_the_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let env = [(SLEEP, "30".to_string())];
        let started = Instant::now();
        let e = run_command(
            &helper("helper_sleep"),
            &env,
            dir.path(),
            Duration::from_millis(400),
        )
        .unwrap_err();
        assert!(e.contains("timed out"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(20), "killed, not awaited");
    }

    #[test]
    fn an_empty_command_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(run_command(&[], &[], dir.path(), Duration::from_secs(1)).is_err());
    }

    #[test]
    fn only_run_states_that_need_the_human_have_a_kind() {
        for s in RunState::ALL {
            let expect = matches!(
                s,
                RunState::Blocked | RunState::Stalled | RunState::OverBudget | RunState::Failed
            );
            assert_eq!(run_kind(*s).is_some(), expect, "{s}");
        }
    }
}

#[cfg(test)]
mod flow_tests {
    use super::*;
    use asf_core::{Config, MergePatch, NewComment, NewIssue, NewRun, RunPatch};
    use asf_runtime::FakeRuntime;
    use asf_store::SqliteStore;
    use time::Duration as TimeDuration;

    struct Fx {
        _dir: tempfile::TempDir,
        ctx: Ctx,
        rt: FakeRuntime,
    }

    fn fixture() -> Fx {
        let dir = tempfile::Builder::new().prefix("asf-remind-").tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let factory = root.join(crate::ctx::FACTORY_DIR);
        std::fs::create_dir_all(&factory).unwrap();
        let store = SqliteStore::open(&factory.join(crate::ctx::DB_FILE)).unwrap();
        store.init_repo("D:/repo", "asf", "main").unwrap();
        let ctx = Ctx {
            root: root.clone(),
            config: Config::default(),
            store,
            lane: LANE_ORCHESTRATOR.into(),
        };
        Fx {
            rt: FakeRuntime::new(factory.join("fake-runtime.json")),
            _dir: dir,
            ctx,
        }
    }

    fn issue(fx: &Fx, title: &str, status: Status) -> IssueId {
        fx.ctx
            .store
            .create_issue(&NewIssue {
                title: title.into(),
                status,
                ..NewIssue::default()
            })
            .unwrap()
            .id
    }

    fn comment(fx: &Fx, id: &IssueId, kind: CommentType, to: &str, body: &str, reply_to: Option<i64>) -> i64 {
        fx.ctx
            .store
            .add_comment(&NewComment {
                issue_id: id.clone(),
                author: "agent:x".into(),
                kind,
                to_lane: to.into(),
                body: body.into(),
                reply_to,
            })
            .unwrap()
            .id
    }

    fn run_in(fx: &Fx, id: &IssueId, state: RunState, reason: Option<&str>) -> String {
        let run = fx
            .ctx
            .store
            .create_run(&NewRun {
                issue_id: id.clone(),
                runtime: "fake".into(),
                worktree: "wt".into(),
                branch: "b".into(),
                base_sha: "0".into(),
                budget_usd: None,
            })
            .unwrap();
        fx.ctx
            .store
            .update_run(
                &run.id,
                &RunPatch {
                    state: Some(state),
                    exit_reason: reason.map(str::to_string),
                    ..RunPatch::default()
                },
            )
            .unwrap();
        run.id
    }

    fn merge_in(fx: &Fx, id: &IssueId, state: MergeState, error: &str) {
        let item = fx.ctx.store.enqueue_merge(id, None, "feat/x").unwrap();
        fx.ctx
            .store
            .update_merge(
                item.id,
                &MergePatch {
                    state: Some(state),
                    last_error: Some(Some(error.into())),
                    ..MergePatch::default()
                },
            )
            .unwrap();
    }

    /// `minutes` after now: everything the fixture creates has waited that long by then.
    fn later(minutes: i64) -> OffsetDateTime {
        OffsetDateTime::now_utc() + TimeDuration::minutes(minutes)
    }

    fn notify_log(fx: &Fx) -> Vec<serde_json::Value> {
        fx.rt
            .log()
            .into_iter()
            .filter(|e| e.op == "notify")
            .map(|e| e.args)
            .collect()
    }

    #[test]
    fn every_kind_of_wait_is_collected_with_its_reason() {
        let fx = fixture();
        let blocked = issue(&fx, "blocked", Status::Blocked);
        comment(
            &fx,
            &blocked,
            CommentType::Blocker,
            "human",
            "gate test timed out\nlong tail",
            None,
        );
        let asked = issue(&fx, "asked", Status::InProgress);
        comment(
            &fx,
            &asked,
            CommentType::Question,
            "human",
            "Which branch?\nmore",
            None,
        );
        let answered = issue(&fx, "answered", Status::InProgress);
        let aq = comment(
            &fx,
            &answered,
            CommentType::Question,
            "human",
            "old question",
            None,
        );
        comment(&fx, &answered, CommentType::Answer, "x", "done", Some(aq));
        let merge = issue(&fx, "merge", Status::Validating);
        merge_in(&fx, &merge, MergeState::Failed, "rebase conflict in a.rs\nstack");
        let stalled = issue(&fx, "stalled", Status::InProgress);
        run_in(&fx, &stalled, RunState::Stalled, Some("no output for 15 min"));
        let working = issue(&fx, "working", Status::InProgress);
        run_in(&fx, &working, RunState::Working, None);

        let mut state = RemindState::new();
        state.sync(&fx.ctx.store).unwrap();
        let mut got: Vec<(String, WaitKind, String)> = collect(&fx.ctx, &state.index)
            .unwrap()
            .into_iter()
            .map(|w| (w.issue.to_string(), w.kind, w.reason))
            .collect();
        got.sort();
        let mut want = vec![
            (
                blocked.to_string(),
                WaitKind::IssueBlocked,
                "gate test timed out".to_string(),
            ),
            (asked.to_string(), WaitKind::Question, "Which branch?".to_string()),
            (
                merge.to_string(),
                WaitKind::MergeFailed,
                "rebase conflict in a.rs".to_string(),
            ),
            (
                stalled.to_string(),
                WaitKind::RunStalled,
                "no output for 15 min".to_string(),
            ),
        ];
        want.sort();
        assert_eq!(
            got, want,
            "an answered question and a working run are not waiting"
        );
    }

    #[test]
    fn finished_issues_and_older_runs_do_not_wait() {
        let fx = fixture();
        let done = issue(&fx, "done", Status::Blocked);
        merge_in(&fx, &done, MergeState::Failed, "x");
        fx.ctx.store.set_status(&done, Status::Done, "t").unwrap();
        let retried = issue(&fx, "retried", Status::InProgress);
        run_in(&fx, &retried, RunState::Failed, Some("first attempt"));
        run_in(&fx, &retried, RunState::Working, None);

        let state = RemindState::new();
        assert!(collect(&fx.ctx, &state.index).unwrap().is_empty());
    }

    #[test]
    fn a_failed_merge_whose_worker_is_fixing_review_findings_does_not_wait() {
        let fx = fixture();
        // sent back by the reviewer: the item stays failed, the run is reopened as working
        let fixing = issue(&fx, "fixing", Status::InProgress);
        run_in(&fx, &fixing, RunState::Working, None);
        merge_in(
            &fx,
            &fixing,
            MergeState::Failed,
            "review asks for changes on feat/x",
        );
        // the same failed item with nobody working on it does wait for the human
        let idle = issue(&fx, "idle", Status::Validating);
        merge_in(&fx, &idle, MergeState::Blocked, "main is dirty");

        let state = RemindState::new();
        let got: Vec<_> = collect(&fx.ctx, &state.index)
            .unwrap()
            .into_iter()
            .map(|w| (w.issue, w.kind))
            .collect();
        assert_eq!(got, vec![(idle, WaitKind::MergeBlocked)]);
    }

    #[test]
    fn one_issue_with_a_failed_run_a_failed_merge_and_a_block_reminds_once() {
        let fx = fixture();
        let id = issue(&fx, "bad", Status::Blocked);
        comment(
            &fx,
            &id,
            CommentType::Blocker,
            "human",
            "verification failed",
            None,
        );
        run_in(&fx, &id, RunState::Failed, Some("verification failed"));
        merge_in(&fx, &id, MergeState::Blocked, "main is dirty");

        let mut state = RemindState::new();
        let sent = send_due(&fx.ctx, &fx.rt, &mut state, later(60)).unwrap();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].issue, id);
        assert_eq!(notify_log(&fx).len(), 1);
    }

    #[test]
    fn the_notification_names_the_issue_the_time_and_the_reason() {
        let fx = fixture();
        let id = issue(&fx, "b", Status::Blocked);
        comment(
            &fx,
            &id,
            CommentType::Blocker,
            "human",
            "gate test timed out",
            None,
        );

        let mut state = RemindState::new();
        let sent = send_due(&fx.ctx, &fx.rt, &mut state, later(45)).unwrap();
        assert_eq!(sent.len(), 1);
        let n = &notify_log(&fx)[0];
        assert_eq!(n["title"], format!("asf {id} needs you"));
        let body = n["body"].as_str().unwrap();
        assert!(
            body.starts_with(&format!("{id} has been blocked for 4")),
            "{body}"
        );
        assert!(body.ends_with(" min: gate test timed out"), "{body}");
        assert_eq!(sent[0].text, body);
        assert_eq!(sent[0].notify_error, None);
        assert_eq!(sent[0].command_error, None);
    }

    #[test]
    fn nothing_is_sent_before_the_delay_or_when_reminders_are_off() {
        let mut fx = fixture();
        let id = issue(&fx, "b", Status::Blocked);
        comment(&fx, &id, CommentType::Blocker, "human", "why", None);

        let mut state = RemindState::new();
        assert!(send_due(&fx.ctx, &fx.rt, &mut state, later(14))
            .unwrap()
            .is_empty());
        assert!(notify_log(&fx).is_empty());

        fx.ctx.config.notify.after_minutes = 0;
        assert!(send_due(&fx.ctx, &fx.rt, &mut state, later(600))
            .unwrap()
            .is_empty());
        assert!(notify_log(&fx).is_empty());
    }

    #[test]
    fn reminders_repeat_every_interval_and_survive_a_restart() {
        let fx = fixture();
        let id = issue(&fx, "b", Status::Blocked);
        comment(&fx, &id, CommentType::Blocker, "human", "why", None);

        let mut state = RemindState::new();
        assert_eq!(send_due(&fx.ctx, &fx.rt, &mut state, later(20)).unwrap().len(), 1);
        assert!(
            send_due(&fx.ctx, &fx.rt, &mut state, later(25))
                .unwrap()
                .is_empty(),
            "same process, 5 min later"
        );

        // `asf serve` restarted: a new state reads the reminder back from the store. The reminder
        // was recorded at the real "now", so 25 min later is inside the 30 min interval and 31 min
        // later is past it; a state that forgot would have reminded at once (the wait is 25 min old).
        let mut restarted = RemindState::new();
        assert!(
            send_due(&fx.ctx, &fx.rt, &mut restarted, later(25))
                .unwrap()
                .is_empty(),
            "a restart must not repeat early"
        );
        assert_eq!(
            send_due(&fx.ctx, &fx.rt, &mut restarted, later(31))
                .unwrap()
                .len(),
            1,
            "30 min after the first reminder"
        );
        assert_eq!(notify_log(&fx).len(), 2);

        let events: Vec<_> = fx
            .ctx
            .store
            .events_since(0, 1000)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == EVENT_REMINDED)
            .collect();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].issue_id.as_ref(), Some(&id));
        assert_eq!(events[0].payload["kind"], "issue_blocked");
        assert!(events[0].payload["age_minutes"].as_u64().unwrap() >= 19);
    }

    #[test]
    fn a_wait_that_ends_stops_the_reminders() {
        let fx = fixture();
        let id = issue(&fx, "b", Status::Blocked);
        let mut state = RemindState::new();
        assert_eq!(send_due(&fx.ctx, &fx.rt, &mut state, later(20)).unwrap().len(), 1);
        fx.ctx.store.set_status(&id, Status::InProgress, "t").unwrap();
        assert!(send_due(&fx.ctx, &fx.rt, &mut state, later(120))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_failing_command_is_reported_and_does_not_fail_the_tick() {
        let mut fx = fixture();
        fx.ctx.config.notify.command = vec!["asf-no-such-program-xyz".into()];
        let id = issue(&fx, "b", Status::Blocked);
        comment(&fx, &id, CommentType::Blocker, "human", "why", None);

        let mut state = RemindState::new();
        let sent = send_due(&fx.ctx, &fx.rt, &mut state, later(20)).unwrap();
        assert_eq!(sent.len(), 1);
        assert!(
            sent[0]
                .command_error
                .as_deref()
                .unwrap_or_default()
                .contains("cannot start"),
            "{sent:?}"
        );
        assert_eq!(notify_log(&fx).len(), 1, "the notification went out anyway");
        assert!(
            send_due(&fx.ctx, &fx.rt, &mut state, later(21))
                .unwrap()
                .is_empty(),
            "a failed command still counts as a reminder"
        );
    }

    #[test]
    fn the_command_runs_in_the_repo_root_with_the_reminder_in_its_environment() {
        let mut fx = fixture();
        // This test binary is the command: the `helper_dump_env` test writes its environment to
        // `remind-env.txt` in the working directory, which `send_due` sets to the repo root.
        let exe = std::env::current_exe().unwrap().to_string_lossy().into_owned();
        fx.ctx.config.notify.command = vec![
            exe,
            "--exact".into(),
            "commands::remind::tests::helper_dump_env".into(),
        ];
        let id = issue(&fx, "b", Status::Blocked);
        comment(
            &fx,
            &id,
            CommentType::Blocker,
            "human",
            "gate test timed out",
            None,
        );

        let mut state = RemindState::new();
        let sent = send_due(&fx.ctx, &fx.rt, &mut state, later(45)).unwrap();
        assert_eq!(sent[0].command_error, None, "{sent:?}");
        let written = std::fs::read_to_string(fx.ctx.root.join("remind-env.txt")).unwrap();
        let parts: Vec<&str> = written.splitn(4, '|').collect();
        assert_eq!(parts[0], id.to_string());
        assert_eq!(parts[1], "issue_blocked");
        assert!(parts[2].parse::<u64>().unwrap() >= 44, "{written}");
        assert_eq!(parts[3], sent[0].text);
    }
}
