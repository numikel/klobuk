//! Reminders: what `asf serve` nags the human about, and when.
//!
//! Pure: the adapter (`asf serve`) collects the [`Waiting`] things, folds the store's event log into
//! an [`EventIndex`], passes "now" in and acts on the [`Reminder`]s [`due`] returns. Nothing here
//! reads a clock, a file or the store, so every rule is tested with fixed times.
//!
//! Two kinds of nudge share one record (the `attention.reminded` event): the [`Alert`] at the moment
//! something new starts to wait ([`alerts`]), and the [`Reminder`] when it keeps waiting ([`due`]).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::config::NotifyCfg;
use crate::ids::IssueId;
use crate::model::{str_enum, Comment, CommentType, Event, Issue, ParseEnumError, Status};
use crate::run::{MergeItem, MergeState, Run, RunState};

/// Kind of the event that records a sent reminder (`issue_id`, payload `{kind, age_minutes}`).
pub const EVENT_REMINDED: &str = "attention.reminded";

/// Longest reason quoted in a reminder, so a notification stays one short line.
const REASON_MAX_CHARS: usize = 120;

/// What an issue is waiting for the human on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitKind {
    IssueBlocked,
    MergeFailed,
    MergeBlocked,
    RunBlocked,
    RunStalled,
    RunOverBudget,
    RunFailed,
    Question,
    /// A BLOCKER comment addressed to the human. Only ever announced ([`alerts`]): whether it still
    /// blocks is not something the store can say, so it never ages into a reminder.
    Blocker,
}
str_enum!(WaitKind, "wait kind", {
    IssueBlocked => "issue_blocked", MergeFailed => "merge_failed", MergeBlocked => "merge_blocked",
    RunBlocked => "run_blocked", RunStalled => "run_stalled", RunOverBudget => "run_over_budget",
    RunFailed => "run_failed", Question => "question", Blocker => "blocker",
});

impl WaitKind {
    /// What follows `asf <id>` in the title of an [`Alert`]: `asf asf-27 asks`, `... merge failed`.
    fn headline(self) -> &'static str {
        match self {
            WaitKind::Question => "asks",
            WaitKind::Blocker | WaitKind::IssueBlocked | WaitKind::RunBlocked => "is blocked",
            WaitKind::MergeFailed => "merge failed",
            WaitKind::MergeBlocked => "merge blocked",
            WaitKind::RunStalled => "is stalled",
            WaitKind::RunOverBudget => "is over budget",
            WaitKind::RunFailed => "run failed",
        }
    }

    /// The body of an [`Alert`] when the wait carries no text of its own.
    fn fallback(self) -> &'static str {
        match self {
            WaitKind::Question => "asked a question",
            WaitKind::Blocker => "reported a blocker",
            WaitKind::IssueBlocked => "the issue is blocked",
            WaitKind::RunBlocked => "the worker waits for input",
            WaitKind::RunStalled => "the worker stopped making progress",
            WaitKind::RunOverBudget => "the worker went over budget",
            WaitKind::RunFailed => "the run failed",
            WaitKind::MergeFailed => "the merge failed",
            WaitKind::MergeBlocked => "the merge is blocked",
        }
    }

    /// The one order of what waits for the human (lower first); [`waits`] sorts by it, an [`Alert`] is
    /// led by it and the TUI picks the row of an issue by it. A blocked run first (a human must press
    /// a key before the agent goes on), then a QUESTION (the agent is parked but not failing), then a
    /// BLOCKER comment, a failed run (the cause) before its merge item (the consequence), and last
    /// what is stalled, over budget or blocked as a whole issue.
    pub fn urgency(self) -> u8 {
        match self {
            WaitKind::RunBlocked => 0,
            WaitKind::Question => 1,
            WaitKind::Blocker => 2,
            WaitKind::RunFailed => 3,
            WaitKind::MergeFailed => 4,
            WaitKind::MergeBlocked => 5,
            WaitKind::RunStalled => 6,
            WaitKind::RunOverBudget => 7,
            WaitKind::IssueBlocked => 8,
        }
    }

    /// The words that follow the issue id in a reminder: `asf-27 <phrase> for 45 min: …`.
    fn phrase(self) -> &'static str {
        match self {
            WaitKind::IssueBlocked => "has been blocked",
            WaitKind::MergeFailed => "has had a failed merge",
            WaitKind::MergeBlocked => "has had a blocked merge",
            WaitKind::RunBlocked => "has had a worker waiting for input",
            WaitKind::RunStalled => "has had a stalled worker",
            WaitKind::RunOverBudget => "has had a worker over budget",
            WaitKind::RunFailed => "has had a failed run",
            WaitKind::Question => "has had an unanswered question",
            WaitKind::Blocker => "has had a blocker",
        }
    }
}

/// One thing that needs the human, with the moment it started to wait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Waiting {
    pub issue: IssueId,
    pub kind: WaitKind,
    /// RFC 3339 UTC time the wait began; an entry that does not parse is ignored.
    pub since: String,
    /// A few plain words on why (see [`first_line`]); may be empty.
    pub reason: String,
    /// Identity of this episode of waiting (`question:42`, `merge:7:failed@2026-11-07T09:00:00Z`).
    /// Stable across ticks and restarts, and different for the next episode, so [`alerts`] can tell
    /// "already announced" from "new". Reminders ignore it.
    pub key: String,
}

/// The one key that acts on a wait: what the TUI prints at the end of a NEEDS YOU row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    /// `a`: the answer overlay (blocked or stalled worker) or a typed answer (QUESTION comment).
    Answer,
    /// `v`: rerun verification of a failed run.
    Verify,
    /// `r`: re-queue the merge item.
    Retry,
    /// `x`: abort an over-budget worker (after a confirmation).
    Abort,
}

impl Act {
    pub fn key(self) -> char {
        match self {
            Act::Answer => 'a',
            Act::Verify => 'v',
            Act::Retry => 'r',
            Act::Abort => 'x',
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Act::Answer => "answer",
            Act::Verify => "verify",
            Act::Retry => "retry",
            Act::Abort => "abort",
        }
    }

    /// `a answer`: how the key is printed at the end of a row.
    pub fn hint(self) -> String {
        format!("{} {}", self.key(), self.label())
    }
}

/// A [`Waiting`] with what a person can do about it, as [`waits`] returns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wait {
    pub waiting: Waiting,
    /// The key that acts on it; `None` when no key does (a blocked issue).
    pub act: Option<Act>,
    /// The QUESTION comment behind a [`WaitKind::Question`].
    pub comment: Option<i64>,
}

/// What [`waits`] reads. The adapter loads these from the store; nothing here touches it.
#[derive(Debug, Clone, Copy)]
pub struct WaitInput<'a> {
    /// Every issue: a wait of a finished issue (done, cancelled) is not listed.
    pub issues: &'a [Issue],
    /// The runs to judge: the newest of an issue (highest `attempt`) says what it waits on, and any
    /// run of it that has not ended means its worker is busy. Passing every run of every issue is fine.
    pub runs: &'a [Run],
    /// Merge items; those that are neither `failed` nor `blocked` are ignored.
    pub queue: &'a [MergeItem],
    /// Unanswered QUESTION comments to the human (the adapter decides what "unanswered" means).
    pub questions: &'a [Comment],
    /// BLOCKER comments: the newest of an issue lends its text to a wait that has none of its own.
    pub blockers: &'a [Comment],
    /// Supplies the moment a thing entered its state.
    pub index: &'a EventIndex,
}

/// Everything that waits for the human right now, most urgent first: the one definition behind the
/// reminders of `asf serve` and the NEEDS YOU rows of the TUI.
///
/// **Order.** By [`WaitKind::urgency`], then by the time the wait began (oldest first), then by issue
/// and key. The order of kinds is decided here once:
///
/// 1. a blocked run: a human must press a key before the agent continues;
/// 2. an unanswered QUESTION: the agent is parked but not failing;
/// 3. a BLOCKER comment (added by the adapter, see [`WaitKind::Blocker`]);
/// 4. a failed run, the cause of what follows;
/// 5. a failed or blocked merge item, the consequence;
/// 6. a stalled run, then an over-budget one;
/// 7. a blocked issue as a whole, which no key acts on.
///
/// **What does not wait.** A finished issue waits for nobody. A failed or blocked merge item of an
/// issue whose worker is busy (a run that has not ended) does not wait either: a reviewer or a gate
/// sent the worker back to fix it, and `asf merge retry` refuses the issue until that run ends. Only
/// the newest run of an issue counts; an older failed attempt is history once a retry exists.
///
/// **Reason.** One short line ([`first_line`]); empty when there is none. A question: its text. A
/// merge item: its error. A blocked run: the line of the screen its BLOCKER quotes, else its exit
/// reason. Any other run: its exit reason, else the first line of the newest BLOCKER of its issue.
///
/// A [`WaitKind::Blocker`] is not listed: whether it still blocks is not something the store says.
pub fn waits(input: &WaitInput) -> Vec<Wait> {
    let live: BTreeSet<&IssueId> = input
        .issues
        .iter()
        .filter(|i| !i.status.is_terminal())
        .map(|i| &i.id)
        .collect();
    let blocker_of = |issue: &IssueId| {
        input
            .blockers
            .iter()
            .filter(|c| &c.issue_id == issue && c.kind == CommentType::Blocker)
            .max_by_key(|c| c.id)
    };
    let mut out: Vec<Wait> = Vec::new();

    for issue in input.issues.iter().filter(|i| i.status == Status::Blocked) {
        let status = issue.status.as_str();
        let entered = input
            .index
            .issue_entered(&issue.id, status)
            .unwrap_or(&issue.updated_at);
        out.push(Wait {
            waiting: Waiting {
                issue: issue.id.clone(),
                kind: WaitKind::IssueBlocked,
                since: issue.updated_at.clone(),
                reason: blocker_of(&issue.id)
                    .map(|c| first_line(&c.body))
                    .unwrap_or_default(),
                key: format!("issue:{status}@{entered}"),
            },
            act: None,
            comment: None,
        });
    }

    for q in input.questions.iter().filter(|q| live.contains(&q.issue_id)) {
        out.push(Wait {
            waiting: Waiting {
                issue: q.issue_id.clone(),
                kind: WaitKind::Question,
                since: q.created_at.clone(),
                reason: first_line(&q.body),
                key: format!("question:{}", q.id),
            },
            act: Some(Act::Answer),
            comment: Some(q.id),
        });
    }

    let mut newest: BTreeMap<&IssueId, &Run> = BTreeMap::new();
    for r in input.runs {
        if newest.get(&r.issue_id).is_none_or(|n| r.attempt > n.attempt) {
            newest.insert(&r.issue_id, r);
        }
    }
    let busy: BTreeSet<&IssueId> = input
        .runs
        .iter()
        .filter(|r| !r.state.is_terminal())
        .map(|r| &r.issue_id)
        .collect();

    for (issue, run) in newest {
        let Some((kind, act)) = run_wait(run.state) else {
            continue;
        };
        if !live.contains(issue) {
            continue;
        }
        let since = input
            .index
            .run_entered(&run.id, run.state.as_str())
            .map(str::to_string)
            .or_else(|| run.finished_at.clone())
            .or_else(|| run.last_seen_at.clone())
            .unwrap_or_else(|| run.started_at.clone());
        // A blocked run is about its prompt: the BLOCKER's other lines say nothing of it. Any other
        // run without an exit reason lets its issue's BLOCKER explain.
        let blocker = blocker_of(issue);
        let blocked = run.state == RunState::Blocked;
        let screen = blocker
            .filter(|_| blocked)
            .and_then(|c| screen_line(&c.body))
            .map(|s| first_line(&s));
        let exit = run.exit_reason.as_deref().map(first_line);
        let quoted = blocker.filter(|_| !blocked).map(|c| first_line(&c.body));
        let reason = [screen, exit, quoted]
            .into_iter()
            .flatten()
            .find(|s| !s.is_empty())
            .unwrap_or_default();
        out.push(Wait {
            waiting: Waiting {
                key: format!("run:{}:{}@{since}", run.id, run.state),
                issue: issue.clone(),
                kind,
                since,
                reason,
            },
            act: Some(act),
            comment: None,
        });
    }

    for m in input.queue {
        let kind = match m.state {
            MergeState::Failed => WaitKind::MergeFailed,
            MergeState::Blocked => WaitKind::MergeBlocked,
            _ => continue,
        };
        if !live.contains(&m.issue_id) || busy.contains(&m.issue_id) {
            continue;
        }
        let entered = input
            .index
            .merge_entered(m.id, m.state.as_str())
            .unwrap_or(&m.updated_at);
        out.push(Wait {
            waiting: Waiting {
                issue: m.issue_id.clone(),
                kind,
                since: m.updated_at.clone(),
                reason: m.last_error.as_deref().map(first_line).unwrap_or_default(),
                key: format!("merge:{}:{}@{entered}", m.id, m.state),
            },
            act: Some(Act::Retry),
            comment: None,
        });
    }

    out.sort_by(|a, b| {
        let (a, b) = (&a.waiting, &b.waiting);
        (a.kind.urgency(), &a.since, &a.issue, &a.key).cmp(&(b.kind.urgency(), &b.since, &b.issue, &b.key))
    });
    out
}

/// What a run in `state` waits on and the key that acts on it; `None` for a run that needs nobody.
/// The key is one the command behind it accepts for that state: `asf run verify` refuses a worker
/// that is still active (stalled, over budget), a reply moves a stalled run back to working, and
/// `asf run abort` takes any run that is not finished.
fn run_wait(state: RunState) -> Option<(WaitKind, Act)> {
    match state {
        RunState::Blocked => Some((WaitKind::RunBlocked, Act::Answer)),
        RunState::Stalled => Some((WaitKind::RunStalled, Act::Answer)),
        RunState::OverBudget => Some((WaitKind::RunOverBudget, Act::Abort)),
        RunState::Failed => Some((WaitKind::RunFailed, Act::Verify)),
        _ => None,
    }
}

/// The first non-empty line of the worker screen quoted in a BLOCKER body (the fenced block after
/// `Screen:`), or `None` when the body quotes no screen.
pub fn screen_line(body: &str) -> Option<String> {
    let mut lines = body.lines().skip_while(|l| l.trim() != "Screen:");
    lines.next()?; // the `Screen:` line itself
    let mut fenced = false;
    for line in lines {
        if line.trim_start().starts_with("```") {
            if fenced {
                return None;
            }
            fenced = true;
        } else if fenced && !line.trim().is_empty() {
            return Some(line.trim().to_string());
        }
    }
    None
}

/// A reminder that is due now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Reminder {
    pub issue: IssueId,
    pub kind: WaitKind,
    pub age_minutes: u64,
    /// The sentence for the notification and `ASF_REMIND_TEXT`.
    pub text: String,
}

/// What the event log says about reminders and state changes, built by feeding events in order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventIndex {
    /// Time (RFC 3339) of the latest [`Reminder`] per issue: an `attention.reminded` event that is
    /// not an alert. Only these start the `every_minutes` countdown.
    pub reminded: BTreeMap<IssueId, String>,
    /// Time (RFC 3339) of the latest [`Alert`] per issue: an `attention.reminded` event with
    /// `"immediate": true`. The first reminder is counted from it, by `after_minutes`.
    pub alerted: BTreeMap<IssueId, String>,
    /// Per run id: the state it last entered and when (from `run.state` events).
    pub run_states: BTreeMap<String, (String, String)>,
    /// `(issue, key)` of every wait an [`Alert`] has already announced (the `keys` of the events).
    pub announced: BTreeSet<(IssueId, String)>,
    /// When a thing last entered a state, from `issue.status`, `merge.state` and `run.state` events:
    /// `("issue:asf-3", "blocked")`, `("merge:7", "failed")`, `("run:<id>", "stalled")` -> time.
    entered: BTreeMap<(String, String), String>,
}

impl EventIndex {
    /// Folds one event in; events must arrive in `seq` order so the latest one wins.
    pub fn absorb(&mut self, e: &Event) {
        let to = e.payload.get("to").and_then(|v| v.as_str());
        match e.kind.as_str() {
            EVENT_REMINDED => {
                if let Some(issue) = &e.issue_id {
                    let immediate = e.payload.get("immediate").and_then(|v| v.as_bool()) == Some(true);
                    let latest = if immediate {
                        &mut self.alerted
                    } else {
                        &mut self.reminded
                    };
                    latest.insert(issue.clone(), e.ts.clone());
                    let keys = e.payload.get("keys").and_then(|v| v.as_array());
                    for key in keys.into_iter().flatten().filter_map(|k| k.as_str()) {
                        self.announced.insert((issue.clone(), key.to_string()));
                    }
                }
            }
            "run.state" => {
                if let (Some(run), Some(to)) = (&e.run_id, to) {
                    self.run_states
                        .insert(run.clone(), (to.to_string(), e.ts.clone()));
                    self.entered
                        .insert((format!("run:{run}"), to.to_string()), e.ts.clone());
                }
            }
            "issue.status" => {
                if let (Some(issue), Some(to)) = (&e.issue_id, to) {
                    self.entered
                        .insert((format!("issue:{issue}"), to.to_string()), e.ts.clone());
                }
            }
            "merge.state" => {
                if let (Some(id), Some(to)) = (e.payload.get("merge_id").and_then(|v| v.as_i64()), to) {
                    self.entered
                        .insert((format!("merge:{id}"), to.to_string()), e.ts.clone());
                }
            }
            _ => {}
        }
    }

    /// When run `run` entered `state`, if the log says so.
    pub fn run_entered(&self, run: &str, state: &str) -> Option<&str> {
        self.run_states
            .get(run)
            .filter(|(s, _)| s == state)
            .map(|(_, ts)| ts.as_str())
    }

    /// When issue `issue` last got status `status`, if the log says so.
    pub fn issue_entered(&self, issue: &IssueId, status: &str) -> Option<&str> {
        self.entered_at(&format!("issue:{issue}"), status)
    }

    /// When merge item `merge` last got state `state`, if the log says so.
    pub fn merge_entered(&self, merge: i64, state: &str) -> Option<&str> {
        self.entered_at(&format!("merge:{merge}"), state)
    }

    fn entered_at(&self, subject: &str, state: &str) -> Option<&str> {
        self.entered
            .get(&(subject.to_string(), state.to_string()))
            .map(String::as_str)
    }
}

/// A new wait to tell the human about right now: one per issue, however many new things it has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Alert {
    pub issue: IssueId,
    /// The kind of the wait that leads the alert (see [`WaitKind::urgency`]).
    pub kind: WaitKind,
    /// `asf asf-27 asks`, `asf asf-27 is blocked`, `asf asf-27 merge failed`.
    pub title: String,
    /// The first line of the question, blocker or error; a short stock phrase when there is none.
    pub body: String,
    /// `title: body`: the sentence for `ASF_REMIND_TEXT` and the tick report.
    pub text: String,
    /// The [`Waiting::key`] of every new wait of the issue, all announced by this one alert.
    pub keys: Vec<String>,
}

/// The alerts for the waits `announced` does not cover yet, ordered by issue.
///
/// An issue with several new waits at once (a failed run, its failed merge item and the BLOCKER
/// comment that explains them) gets one alert, led by the most telling wait; every one of them is
/// listed in [`Alert::keys`] so the next tick does not announce the leftovers.
pub fn alerts(waiting: &[Waiting], announced: &BTreeSet<(IssueId, String)>) -> Vec<Alert> {
    let mut fresh: BTreeMap<&IssueId, Vec<&Waiting>> = BTreeMap::new();
    for w in waiting {
        if !announced.contains(&(w.issue.clone(), w.key.clone())) {
            fresh.entry(&w.issue).or_default().push(w);
        }
    }
    fresh
        .into_iter()
        .filter_map(|(issue, mut ws)| {
            // The most telling wait first; among equals the newest.
            ws.sort_by(|a, b| {
                (a.kind.urgency(), std::cmp::Reverse(&a.since), &a.key).cmp(&(
                    b.kind.urgency(),
                    std::cmp::Reverse(&b.since),
                    &b.key,
                ))
            });
            let lead = ws.first()?;
            let title = format!("asf {issue} {}", lead.kind.headline());
            let reason = first_line(&lead.reason);
            let body = if reason.is_empty() {
                lead.kind.fallback().to_string()
            } else {
                reason
            };
            let mut keys: Vec<String> = ws.iter().map(|w| w.key.clone()).collect();
            keys.sort();
            keys.dedup();
            Some(Alert {
                issue: issue.clone(),
                kind: lead.kind,
                text: format!("{title}: {body}"),
                title,
                body,
                keys,
            })
        })
        .collect()
}

fn parse_ts(s: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339).ok()
}

/// The reminders due at `now`.
///
/// - `after_minutes == 0` switches everything off.
/// - An issue waits on at most one thing: the oldest of its [`Waiting`] entries (ties: the kind
///   that sorts first), so a failed run and a failed merge item of one issue remind once.
/// - The first reminder of a wait comes `after_minutes` after the wait began or after the
///   [`Alert`] that announced it (`alerted` is [`EventIndex::alerted`]), whichever is later.
/// - Later ones come `every_minutes` after the last reminder (`reminded` is
///   [`EventIndex::reminded`]); `every_minutes == 0` means "once": the reminder after the alert
///   is the only one while the same wait lasts.
/// - A reminder or alert older than the wait itself belongs to an earlier episode and does not
///   count.
/// - [`WaitKind::Blocker`] never reminds.
///
/// Oldest wait first.
pub fn due(
    waiting: &[Waiting],
    reminded: &BTreeMap<IssueId, String>,
    alerted: &BTreeMap<IssueId, String>,
    now: OffsetDateTime,
    cfg: &NotifyCfg,
) -> Vec<Reminder> {
    if cfg.after_minutes == 0 {
        return Vec::new();
    }
    let after_secs = secs(cfg.after_minutes);
    let every_secs = secs(cfg.every_minutes);

    let mut oldest: BTreeMap<&IssueId, (OffsetDateTime, &Waiting)> = BTreeMap::new();
    for w in waiting {
        // A blocker comment is announced once ([`alerts`]); it has no end the store could see.
        if w.kind == WaitKind::Blocker {
            continue;
        }
        let Some(since) = parse_ts(&w.since) else {
            continue;
        };
        let better = match oldest.get(&w.issue) {
            None => true,
            Some((cur, cw)) => (since, w.kind) < (*cur, cw.kind),
        };
        if better {
            oldest.insert(&w.issue, (since, w));
        }
    }

    let mut out: Vec<(OffsetDateTime, Reminder)> = Vec::new();
    for (issue, (since, w)) in oldest {
        let age_secs = (now - since).whole_seconds();
        if age_secs < after_secs {
            continue;
        }
        let last = reminded
            .get(issue)
            .and_then(|t| parse_ts(t))
            .filter(|t| *t >= since);
        if let Some(t) = last {
            if every_secs == 0 || (now - t).whole_seconds() < every_secs {
                continue;
            }
        } else if let Some(a) = alerted
            .get(issue)
            .and_then(|t| parse_ts(t))
            .filter(|t| *t >= since)
        {
            // No reminder yet: the delay runs from the alert that told the human about the wait.
            if (now - a).whole_seconds() < after_secs {
                continue;
            }
        }
        let age_minutes = u64::try_from(age_secs / 60).unwrap_or(0);
        out.push((
            since,
            Reminder {
                issue: issue.clone(),
                kind: w.kind,
                age_minutes,
                text: reminder_text(issue, w.kind, age_minutes, &w.reason),
            },
        ));
    }
    out.sort_by(|a, b| (a.0, &a.1.issue).cmp(&(b.0, &b.1.issue)));
    out.into_iter().map(|(_, r)| r).collect()
}

fn secs(minutes: u64) -> i64 {
    i64::try_from(minutes).unwrap_or(i64::MAX / 60).saturating_mul(60)
}

/// `asf-27 has been blocked for 45 min: gate test timed out`.
pub fn reminder_text(issue: &IssueId, kind: WaitKind, age_minutes: u64, reason: &str) -> String {
    let mut s = format!("{issue} {} for {}", kind.phrase(), human_age(age_minutes));
    if !reason.trim().is_empty() {
        s.push_str(": ");
        s.push_str(reason.trim());
    }
    s
}

/// `45 min`, `1 h`, `2 h 5 min`.
pub fn human_age(minutes: u64) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// The first non-empty line of `text`, whitespace squeezed and cut to a notification-sized length.
pub fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default();
    let squeezed = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if squeezed.chars().count() <= REASON_MAX_CHARS {
        return squeezed;
    }
    let cut: String = squeezed.chars().take(REASON_MAX_CHARS - 3).collect();
    format!("{}...", cut.trim_end())
}
