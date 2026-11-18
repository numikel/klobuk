//! Notifications and reminders for `klobuk serve`: collect what waits for the human, tell them at once
//! when something new starts to wait (`[notify] on_new`), remind them when it keeps waiting
//! (`after_minutes`, `every_minutes`), and record each as an `attention.reminded` event.
//!
//! Both go through the runtime port's `notify` and the optional `[notify] command`. Only the serve
//! loop calls this. The decisions are pure and live in `klobuk_core::remind`; this module is the IO.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use klobuk_core::remind::{self, EventIndex, WaitInput, WaitKind, Waiting, EVENT_REMINDED};
use klobuk_core::{
    AgentRuntime, Comment, CommentType, IssueFilter, IssueId, IssueStore, NewEvent, LANE_HUMAN,
    LANE_ORCHESTRATOR,
};
use serde::Serialize;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::ctx::Ctx;
use crate::output::AppError;
use crate::tui::app::unanswered_questions;

/// How long `[notify] command` may run before it is killed.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// Events read from the store per round when catching up.
const EVENT_PAGE: usize = 2000;

/// A BLOCKER comment older than this is history, not news: it is not announced. Keeps the first
/// `klobuk serve` after an upgrade (or after a long pause) from toasting blockers nobody waits on.
const BLOCKER_NEWS_HOURS: i64 = 24;

/// Notification sound of the runtime that asks for attention (the "done" one is for good news).
const ATTENTION_SOUND: &str = "request";

/// What `klobuk serve` remembers between ticks: how far it has read the event log and what that log
/// says. A fresh process starts at the beginning, which is how a restart recovers the times of the
/// reminders already sent and the waits already announced.
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

/// One notification sent in a tick, as the tick report lists it: an alert for a new wait or a
/// reminder for one that keeps waiting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Sent {
    pub issue: IssueId,
    pub kind: WaitKind,
    /// How long it had waited; `0` for an alert.
    pub age_minutes: u64,
    pub text: String,
    /// Why the runtime notification failed; `null` when it went out.
    pub notify_error: Option<String>,
    /// Why `[notify] command` failed or timed out; `null` when it succeeded or none is set.
    pub command_error: Option<String>,
}

/// What one tick sent.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Notices {
    /// Alerts: something new started to wait (`[notify] on_new`).
    pub alerts: Vec<Sent>,
    /// Reminders: something kept waiting (`[notify] after_minutes`, `every_minutes`).
    pub reminders: Vec<Sent>,
}

/// The words of one notification: the runtime shows `title` and `body`, the command gets `text`.
struct Words<'a> {
    title: &'a str,
    body: &'a str,
    text: &'a str,
}

/// Sends the alerts and reminders that are due now. A failing notification or command is recorded
/// in the returned [`Sent`] and never turns into an error; only a store failure does.
///
/// Alerts go first. The first reminder of a wait is counted from its alert by `after_minutes`
/// (so a wait is never both alerted and reminded in the same tick); repeats are `every_minutes`
/// apart, and `every_minutes = 0` still allows that one reminder.
pub fn send_due(
    ctx: &Ctx,
    rt: &dyn AgentRuntime,
    state: &mut RemindState,
    now: OffsetDateTime,
) -> Result<Notices, AppError> {
    let cfg = &ctx.config.notify;
    if cfg.after_minutes == 0 && !cfg.on_new {
        return Ok(Notices::default());
    }
    state.sync(&ctx.store)?;
    let waiting = collect(ctx, &state.index, now)?;

    let mut out = Notices::default();
    if cfg.on_new {
        for a in remind::alerts(&waiting, &state.index.announced) {
            let payload = serde_json::json!({
                "kind": a.kind, "age_minutes": 0, "keys": a.keys, "immediate": true,
            });
            let words = Words {
                title: &a.title,
                body: &a.body,
                text: &a.text,
            };
            let sent = deliver(ctx, rt, (&a.issue, a.kind, 0), &words, payload)?;
            // Counted from now on, whatever timestamp the store gave the event: the first
            // reminder of this wait comes `after_minutes` after it.
            state
                .index
                .alerted
                .insert(a.issue.clone(), klobuk_core::now_rfc3339());
            for key in a.keys {
                state.index.announced.insert((a.issue.clone(), key));
            }
            out.alerts.push(sent);
        }
    }
    for r in remind::due(&waiting, &state.index.reminded, &state.index.alerted, now, cfg) {
        let title = format!("klobuk {} needs you", r.issue);
        let words = Words {
            title: &title,
            body: &r.text,
            text: &r.text,
        };
        let payload = serde_json::json!({ "kind": r.kind, "age_minutes": r.age_minutes });
        let sent = deliver(ctx, rt, (&r.issue, r.kind, r.age_minutes), &words, payload)?;
        state
            .index
            .reminded
            .insert(r.issue.clone(), klobuk_core::now_rfc3339());
        out.reminders.push(sent);
    }
    Ok(out)
}

/// Notifies through the runtime, runs `[notify] command` and records the event.
/// `about` is `(issue, kind, age_minutes)`. The caller notes the time in the [`EventIndex`].
fn deliver(
    ctx: &Ctx,
    rt: &dyn AgentRuntime,
    about: (&IssueId, WaitKind, u64),
    words: &Words,
    payload: serde_json::Value,
) -> Result<Sent, AppError> {
    let (issue, kind, age_minutes) = about;
    let notify_error = rt
        .notify(words.title, words.body, Some(ATTENTION_SOUND))
        .err()
        .map(|e| e.to_string());
    let command_error = if ctx.config.notify.command.is_empty() {
        None
    } else {
        let env = [
            ("KLOBUK_REMIND_ISSUE", issue.to_string()),
            ("KLOBUK_REMIND_KIND", kind.to_string()),
            ("KLOBUK_REMIND_AGE_MINUTES", age_minutes.to_string()),
            ("KLOBUK_REMIND_TEXT", words.text.to_string()),
        ];
        run_command(&ctx.config.notify.command, &env, &ctx.root, COMMAND_TIMEOUT).err()
    };
    ctx.store.append_event(&NewEvent {
        kind: EVENT_REMINDED.into(),
        issue_id: Some(issue.clone()),
        run_id: None,
        actor: Some(LANE_ORCHESTRATOR.into()),
        payload,
    })?;
    Ok(Sent {
        issue: issue.clone(),
        kind,
        age_minutes,
        text: words.text.to_string(),
        notify_error,
        command_error,
    })
}

/// Everything that waits for the human right now, with the time it began to wait and a key that
/// names this episode: [`remind::waits`] (the rule shared with the TUI) plus the open BLOCKER
/// comments to the human (announced, never reminded), which the store cannot tell are still open.
pub(crate) fn collect(ctx: &Ctx, index: &EventIndex, now: OffsetDateTime) -> Result<Vec<Waiting>, AppError> {
    let store = &ctx.store;
    let issues = store.list_issues(&IssueFilter::default())?;

    let mut questions: Vec<Comment> = Vec::new();
    let mut blockers: Vec<Comment> = Vec::new();
    let mut news: Vec<Waiting> = Vec::new();
    for issue in issues.iter().filter(|i| !i.status.is_terminal()) {
        let list = store.comments(&issue.id)?;
        let open: BTreeSet<i64> = unanswered_questions(&list).into_iter().collect();
        questions.extend(list.iter().filter(|c| open.contains(&c.id)).cloned());
        blockers.extend(
            list.iter()
                .filter(|c| c.kind == CommentType::Blocker)
                .max_by_key(|c| c.id)
                .cloned(),
        );
        news.extend(news_blockers(&list, now).into_iter().map(|b| Waiting {
            issue: issue.id.clone(),
            kind: WaitKind::Blocker,
            since: b.created_at.clone(),
            reason: remind::first_line(&b.body),
            key: format!("blocker:{}", b.id),
        }));
    }

    let runs = store.runs(None, false)?;
    let queue = store.merge_queue()?;
    let waits = remind::waits(&WaitInput {
        issues: &issues,
        runs: &runs,
        queue: &queue,
        questions: &questions,
        blockers: &blockers,
        index,
    });
    Ok(waits.into_iter().map(|w| w.waiting).chain(news).collect())
}

/// The BLOCKER comments to the human that are still news: not resolved, not followed by a comment
/// from the human on the same issue, and younger than [`BLOCKER_NEWS_HOURS`].
fn news_blockers(comments: &[Comment], now: OffsetDateTime) -> Vec<&Comment> {
    comments
        .iter()
        .filter(|c| c.kind == CommentType::Blocker && c.to_lane == LANE_HUMAN && c.resolved_at.is_none())
        .filter(|c| !comments.iter().any(|a| a.id > c.id && a.author == LANE_HUMAN))
        .filter(|c| {
            OffsetDateTime::parse(&c.created_at, &Rfc3339)
                .is_ok_and(|t| now - t <= time::Duration::hours(BLOCKER_NEWS_HOURS))
        })
        .collect()
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
    const SLEEP: &str = "KLOBUK_TEST_SLEEP_SECS";
    const FAIL: &str = "KLOBUK_TEST_EXIT_NONZERO";

    #[test]
    fn helper_dump_env() {
        // Acts only when run as a reminder command (KLOBUK_REMIND_ISSUE set); writes into the cwd.
        if std::env::var_os("KLOBUK_REMIND_ISSUE").is_some() {
            let get = |k: &str| std::env::var(k).unwrap_or_default();
            let text = format!(
                "{}|{}|{}|{}",
                get("KLOBUK_REMIND_ISSUE"),
                get("KLOBUK_REMIND_KIND"),
                get("KLOBUK_REMIND_AGE_MINUTES"),
                get("KLOBUK_REMIND_TEXT")
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
            ("KLOBUK_REMIND_ISSUE", "asf-27".to_string()),
            ("KLOBUK_REMIND_KIND", "issue_blocked".to_string()),
            ("KLOBUK_REMIND_AGE_MINUTES", "45".to_string()),
            (
                "KLOBUK_REMIND_TEXT",
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
            &["klobuk-no-such-program-xyz".to_string()],
            &[],
            dir.path(),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(e.contains("cannot start `klobuk-no-such-program-xyz`"), "{e}");
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
}

#[cfg(test)]
mod flow_tests {
    use super::*;
    use klobuk_core::{
        Config, MergePatch, MergeState, NewComment, NewIssue, NewRun, RunPatch, RunState, Status,
    };
    use klobuk_runtime::FakeRuntime;
    use klobuk_store::SqliteStore;
    use time::Duration as TimeDuration;

    struct Fx {
        _dir: tempfile::TempDir,
        ctx: Ctx,
        rt: FakeRuntime,
    }

    /// The reminders of one tick. The tests below that call this are about reminders alone, so
    /// the fixture switches the immediate notifications off (`on_new = false`).
    fn send_due(
        ctx: &Ctx,
        rt: &dyn AgentRuntime,
        state: &mut RemindState,
        now: OffsetDateTime,
    ) -> Result<Vec<Sent>, AppError> {
        super::send_due(ctx, rt, state, now).map(|n| n.reminders)
    }

    fn fixture() -> Fx {
        let dir = tempfile::Builder::new()
            .prefix("klobuk-remind-")
            .tempdir()
            .unwrap();
        let root = dir.path().to_path_buf();
        let factory = root.join(crate::ctx::FACTORY_DIR);
        std::fs::create_dir_all(&factory).unwrap();
        let store = SqliteStore::open(&factory.join(crate::ctx::DB_FILE)).unwrap();
        store.init_repo("D:/repo", "asf", "main").unwrap();
        let mut config = Config::default();
        config.notify.on_new = false;
        let ctx = Ctx {
            root: root.clone(),
            config,
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
        let mut got: Vec<(String, WaitKind, String)> = collect(&fx.ctx, &state.index, later(0))
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
            (
                blocked.to_string(),
                WaitKind::Blocker,
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
        assert!(collect(&fx.ctx, &state.index, later(0)).unwrap().is_empty());
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
        let got: Vec<_> = collect(&fx.ctx, &state.index, later(0))
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
        assert_eq!(n["title"], format!("klobuk {id} needs you"));
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

        // `klobuk serve` restarted: a new state reads the reminder back from the store. The reminder
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
        fx.ctx.config.notify.command = vec!["klobuk-no-such-program-xyz".into()];
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

    // ---- alerts: the notification at the moment something new waits ----

    /// The fixture with `on_new = true`, the default a user gets.
    fn alerting() -> Fx {
        let mut fx = fixture();
        fx.ctx.config.notify.on_new = true;
        fx
    }

    fn tick(fx: &Fx, state: &mut RemindState, now: OffsetDateTime) -> Notices {
        super::send_due(&fx.ctx, &fx.rt, state, now).unwrap()
    }

    #[test]
    fn a_new_question_is_announced_once_with_the_attention_sound() {
        let fx = alerting();
        let id = issue(&fx, "asked", Status::InProgress);
        comment(
            &fx,
            &id,
            CommentType::Question,
            "human",
            "Which branch?\nmore text",
            None,
        );

        let mut state = RemindState::new();
        let first = tick(&fx, &mut state, later(0));
        assert_eq!(first.alerts.len(), 1, "{first:?}");
        assert!(first.reminders.is_empty());
        assert_eq!(first.alerts[0].issue, id);
        assert_eq!(first.alerts[0].kind, WaitKind::Question);
        assert_eq!(first.alerts[0].age_minutes, 0);
        assert_eq!(first.alerts[0].text, format!("klobuk {id} asks: Which branch?"));
        let n = notify_log(&fx);
        assert_eq!(n.len(), 1);
        assert_eq!(n[0]["title"], format!("klobuk {id} asks"));
        assert_eq!(n[0]["body"], "Which branch?");
        assert_eq!(n[0]["sound"], "request", "the attention sound, not the done one");

        assert_eq!(
            tick(&fx, &mut state, later(1)),
            Notices::default(),
            "same process"
        );
        let mut restarted = RemindState::new();
        assert_eq!(
            tick(&fx, &mut restarted, later(2)),
            Notices::default(),
            "a restart neither repeats it"
        );
        assert_eq!(notify_log(&fx).len(), 1);

        // a second question is news again, and so is one asked while the process was down
        comment(&fx, &id, CommentType::Question, "human", "And the remote?", None);
        let again = tick(&fx, &mut restarted, later(3));
        assert_eq!(again.alerts.len(), 1);
        assert_eq!(again.alerts[0].text, format!("klobuk {id} asks: And the remote?"));
        assert_eq!(notify_log(&fx).len(), 2);
    }

    #[test]
    fn something_that_starts_waiting_while_serve_is_down_is_announced_after_the_restart() {
        let fx = alerting();
        let mut state = RemindState::new();
        assert_eq!(tick(&fx, &mut state, later(0)), Notices::default());
        drop(state); // serve stops
        let id = issue(&fx, "b", Status::Blocked);
        comment(
            &fx,
            &id,
            CommentType::Blocker,
            "human",
            "gate test timed out",
            None,
        );

        let mut restarted = RemindState::new();
        let got = tick(&fx, &mut restarted, later(0));
        assert_eq!(got.alerts.len(), 1, "{got:?}");
        assert_eq!(got.alerts[0].kind, WaitKind::Blocker);
        assert_eq!(
            got.alerts[0].text,
            format!("klobuk {id} is blocked: gate test timed out")
        );
    }

    #[test]
    fn every_kind_of_new_wait_is_announced_exactly_once() {
        // (what happens to the issue, expected kind, title, body)
        type Setup = fn(&Fx, &IssueId);
        let cases: Vec<(Status, Setup, WaitKind, &str, &str)> = vec![
            (
                Status::InProgress,
                |fx, id| {
                    comment(fx, id, CommentType::Question, "human", "q?", None);
                },
                WaitKind::Question,
                "asks",
                "q?",
            ),
            (
                Status::InProgress,
                |fx, id| {
                    comment(fx, id, CommentType::Blocker, "human", "scope too narrow", None);
                },
                WaitKind::Blocker,
                "is blocked",
                "scope too narrow",
            ),
            (
                Status::Blocked,
                |_, _| {},
                WaitKind::IssueBlocked,
                "is blocked",
                "the issue is blocked",
            ),
            (
                Status::InProgress,
                |fx, id| {
                    run_in(fx, id, RunState::Blocked, Some("waiting for a dialog"));
                },
                WaitKind::RunBlocked,
                "is blocked",
                "waiting for a dialog",
            ),
            (
                Status::InProgress,
                |fx, id| {
                    run_in(fx, id, RunState::Stalled, Some("no output for 15 min"));
                },
                WaitKind::RunStalled,
                "is stalled",
                "no output for 15 min",
            ),
            (
                Status::InProgress,
                |fx, id| {
                    run_in(fx, id, RunState::OverBudget, None);
                },
                WaitKind::RunOverBudget,
                "is over budget",
                "the worker went over budget",
            ),
            (
                Status::InProgress,
                |fx, id| {
                    run_in(fx, id, RunState::Failed, Some("verification failed"));
                },
                WaitKind::RunFailed,
                "run failed",
                "verification failed",
            ),
            (
                Status::Validating,
                |fx, id| merge_in(fx, id, MergeState::Failed, "rebase conflict in a.rs\nstack"),
                WaitKind::MergeFailed,
                "merge failed",
                "rebase conflict in a.rs",
            ),
            (
                Status::Validating,
                |fx, id| merge_in(fx, id, MergeState::Blocked, "main is dirty"),
                WaitKind::MergeBlocked,
                "merge blocked",
                "main is dirty",
            ),
        ];
        for (status, setup, kind, headline, body) in cases {
            let fx = alerting();
            let id = issue(&fx, "x", status);
            setup(&fx, &id);

            let mut state = RemindState::new();
            let got = tick(&fx, &mut state, later(0));
            assert_eq!(got.alerts.len(), 1, "{kind}: {got:?}");
            assert_eq!(got.alerts[0].kind, kind);
            assert_eq!(
                got.alerts[0].text,
                format!("klobuk {id} {headline}: {body}"),
                "{kind}"
            );
            assert!(
                got.reminders.is_empty(),
                "{kind}: an alert is not also a reminder"
            );
            assert_eq!(notify_log(&fx).len(), 1, "{kind}");

            assert_eq!(
                tick(&fx, &mut state, later(1)),
                Notices::default(),
                "{kind}: next tick"
            );
            assert_eq!(
                tick(&fx, &mut RemindState::new(), later(2)),
                Notices::default(),
                "{kind}: after a restart"
            );
            assert_eq!(notify_log(&fx).len(), 1, "{kind}");
        }
    }

    #[test]
    fn a_failed_run_with_its_merge_item_and_blocker_is_one_notification() {
        let fx = alerting();
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
        let got = tick(&fx, &mut state, later(0));
        assert_eq!(got.alerts.len(), 1, "{got:?}");
        assert_eq!(got.alerts[0].kind, WaitKind::Blocker, "what a person wrote leads");
        assert_eq!(notify_log(&fx).len(), 1);
        assert_eq!(
            tick(&fx, &mut state, later(1)),
            Notices::default(),
            "the others were announced by the same notification"
        );
    }

    #[test]
    fn a_blocker_that_the_human_answered_or_that_is_a_day_old_is_not_news() {
        let fx = alerting();
        let answered = issue(&fx, "answered", Status::InProgress);
        comment(&fx, &answered, CommentType::Blocker, "human", "stuck", None);
        fx.ctx
            .store
            .add_comment(&klobuk_core::NewComment {
                issue_id: answered.clone(),
                author: LANE_HUMAN.into(),
                kind: CommentType::Decision,
                to_lane: "all".into(),
                body: "use main".into(),
                reply_to: None,
            })
            .unwrap();
        let to_a_peer = issue(&fx, "peer", Status::InProgress);
        comment(
            &fx,
            &to_a_peer,
            CommentType::Blocker,
            "agent:asf-9",
            "not for the human",
            None,
        );
        let old = issue(&fx, "old", Status::InProgress);
        comment(&fx, &old, CommentType::Blocker, "human", "yesterday's", None);

        let mut state = RemindState::new();
        let got = tick(&fx, &mut state, later(25 * 60));
        assert!(got.alerts.is_empty(), "{got:?}");
        assert!(notify_log(&fx).is_empty());
    }

    #[test]
    fn the_first_reminder_is_counted_from_the_notification() {
        let fx = {
            let mut fx = alerting();
            fx.ctx.config.notify.after_minutes = 15;
            fx.ctx.config.notify.every_minutes = 30;
            fx
        };
        let id = issue(&fx, "b", Status::Blocked);
        let mut state = RemindState::new();
        assert_eq!(tick(&fx, &mut state, later(0)).alerts.len(), 1);

        // the first reminder is `after_minutes` (15) after the notification, not `every_minutes`
        assert_eq!(tick(&fx, &mut state, later(14)), Notices::default());
        let mut restarted = RemindState::new();
        assert_eq!(
            tick(&fx, &mut restarted, later(14)),
            Notices::default(),
            "a restart keeps counting from the notification"
        );
        let due = tick(&fx, &mut restarted, later(16));
        assert!(due.alerts.is_empty());
        assert_eq!(due.reminders.len(), 1, "15 min after the notification");
        assert_eq!(due.reminders[0].issue, id);
        assert_eq!(notify_log(&fx).len(), 2);

        // later ones are `every_minutes` (30) apart. The store stamps events with the real clock
        // while `later` is simulated, so the last reminder counts as made at the start of the
        // test: the exact repeat rule is pinned with fixed times in the core tests.
        assert_eq!(tick(&fx, &mut restarted, later(25)), Notices::default());
        assert_eq!(tick(&fx, &mut restarted, later(31)).reminders.len(), 1);
        assert_eq!(notify_log(&fx).len(), 3);
    }

    #[test]
    fn zero_every_minutes_still_sends_the_one_reminder_after_the_notification() {
        let fx = {
            let mut fx = alerting();
            fx.ctx.config.notify.after_minutes = 15;
            fx.ctx.config.notify.every_minutes = 0;
            fx
        };
        let id = issue(&fx, "b", Status::Blocked);
        let mut state = RemindState::new();
        assert_eq!(tick(&fx, &mut state, later(0)).alerts.len(), 1);
        assert_eq!(tick(&fx, &mut state, later(14)), Notices::default());

        let due = tick(&fx, &mut state, later(16));
        assert_eq!(due.reminders.len(), 1, "on_new does not switch the reminder off");
        assert_eq!(due.reminders[0].issue, id);

        assert_eq!(
            tick(&fx, &mut state, later(600)),
            Notices::default(),
            "0 reminds only once"
        );
        let mut restarted = RemindState::new();
        assert_eq!(tick(&fx, &mut restarted, later(601)), Notices::default());
        assert_eq!(notify_log(&fx).len(), 2);
    }

    #[test]
    fn on_new_false_switches_off_only_the_immediate_notification() {
        let mut fx = fixture(); // on_new = false
        fx.ctx.config.notify.after_minutes = 15;
        let id = issue(&fx, "b", Status::Blocked);
        let mut state = RemindState::new();
        assert_eq!(tick(&fx, &mut state, later(0)), Notices::default());
        assert!(notify_log(&fx).is_empty());
        let later_on = tick(&fx, &mut state, later(20));
        assert_eq!(later_on.reminders.len(), 1, "reminders still work");
        assert_eq!(later_on.reminders[0].issue, id);
    }

    #[test]
    fn zero_after_minutes_does_not_switch_the_immediate_notification_off() {
        let mut fx = alerting();
        fx.ctx.config.notify.after_minutes = 0;
        let id = issue(&fx, "b", Status::Blocked);
        let mut state = RemindState::new();
        let got = tick(&fx, &mut state, later(0));
        assert_eq!(got.alerts.len(), 1, "{got:?}");
        assert_eq!(got.alerts[0].issue, id);
        assert_eq!(
            tick(&fx, &mut state, later(600)),
            Notices::default(),
            "and never a reminder"
        );
        assert_eq!(notify_log(&fx).len(), 1);

        fx.ctx.config.notify.on_new = false;
        issue(&fx, "c", Status::Blocked);
        assert_eq!(
            tick(&fx, &mut state, later(601)),
            Notices::default(),
            "both off: silent"
        );
    }

    #[test]
    fn the_alert_is_recorded_as_a_reminder_that_lists_what_it_announced() {
        let fx = alerting();
        let id = issue(&fx, "b", Status::Blocked);
        comment(&fx, &id, CommentType::Blocker, "human", "why", None);
        let mut state = RemindState::new();
        tick(&fx, &mut state, later(0));

        let events: Vec<_> = fx
            .ctx
            .store
            .events_since(0, 1000)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == EVENT_REMINDED)
            .collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].issue_id.as_ref(), Some(&id));
        assert_eq!(events[0].payload["kind"], "blocker");
        assert_eq!(events[0].payload["age_minutes"], 0);
        assert_eq!(events[0].payload["immediate"], true);
        assert_eq!(
            events[0].payload["keys"].as_array().map(Vec::len),
            Some(2),
            "blocker + blocked issue"
        );
    }

    #[test]
    fn the_command_gets_the_alert_in_the_same_four_variables() {
        let mut fx = alerting();
        let exe = std::env::current_exe().unwrap().to_string_lossy().into_owned();
        fx.ctx.config.notify.command = vec![
            exe,
            "--exact".into(),
            "commands::remind::tests::helper_dump_env".into(),
        ];
        let id = issue(&fx, "asked", Status::InProgress);
        comment(&fx, &id, CommentType::Question, "human", "Which branch?", None);

        let mut state = RemindState::new();
        let got = tick(&fx, &mut state, later(0));
        assert_eq!(got.alerts[0].command_error, None, "{got:?}");
        let written = std::fs::read_to_string(fx.ctx.root.join("remind-env.txt")).unwrap();
        assert_eq!(
            written,
            format!("{id}|question|0|klobuk {id} asks: Which branch?"),
            "kind is the wait kind, age 0, text is title and body"
        );
    }

    #[test]
    fn a_failing_command_or_notification_is_reported_and_the_wait_is_still_marked_announced() {
        let mut fx = alerting();
        fx.ctx.config.notify.command = vec!["klobuk-no-such-program-xyz".into()];
        let id = issue(&fx, "b", Status::Blocked);
        let mut state = RemindState::new();
        let got = tick(&fx, &mut state, later(0));
        assert_eq!(got.alerts.len(), 1);
        assert_eq!(got.alerts[0].issue, id);
        assert!(
            got.alerts[0]
                .command_error
                .as_deref()
                .unwrap_or_default()
                .contains("cannot start"),
            "{got:?}"
        );
        assert_eq!(tick(&fx, &mut state, later(1)), Notices::default());
    }
}
