//! Reminders: what `asf serve` nags the human about, and when.
//!
//! Pure: the adapter (`asf serve`) collects the [`Waiting`] things, folds the store's event log into
//! an [`EventIndex`], passes "now" in and acts on the [`Reminder`]s [`due`] returns. Nothing here
//! reads a clock, a file or the store, so every rule is tested with fixed times.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::config::NotifyCfg;
use crate::ids::IssueId;
use crate::model::{str_enum, Event, ParseEnumError};

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
}
str_enum!(WaitKind, "wait kind", {
    IssueBlocked => "issue_blocked", MergeFailed => "merge_failed", MergeBlocked => "merge_blocked",
    RunBlocked => "run_blocked", RunStalled => "run_stalled", RunOverBudget => "run_over_budget",
    RunFailed => "run_failed", Question => "question",
});

impl WaitKind {
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

/// What the event log says about reminders and run states, built by feeding events in order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventIndex {
    /// Time (RFC 3339) of the latest `attention.reminded` event per issue.
    pub reminded: BTreeMap<IssueId, String>,
    /// Per run id: the state it last entered and when (from `run.state` events).
    pub run_states: BTreeMap<String, (String, String)>,
}

impl EventIndex {
    /// Folds one event in; events must arrive in `seq` order so the latest one wins.
    pub fn absorb(&mut self, e: &Event) {
        match e.kind.as_str() {
            EVENT_REMINDED => {
                if let Some(issue) = &e.issue_id {
                    self.reminded.insert(issue.clone(), e.ts.clone());
                }
            }
            "run.state" => {
                if let (Some(run), Some(to)) = (&e.run_id, e.payload.get("to").and_then(|v| v.as_str())) {
                    self.run_states
                        .insert(run.clone(), (to.to_string(), e.ts.clone()));
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
}

fn parse_ts(s: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339).ok()
}

/// The reminders due at `now`.
///
/// - `after_minutes == 0` switches everything off.
/// - An issue waits on at most one thing: the oldest of its [`Waiting`] entries (ties: the kind
///   that sorts first), so a failed run and a failed merge item of one issue remind once.
/// - A wait of at least `after_minutes` is due unless the issue was reminded within
///   `every_minutes` (`reminded` is [`EventIndex::reminded`]); `every_minutes == 0`
///   means "once": no repeat while the same wait lasts.
/// - A reminder older than the wait itself belongs to an earlier episode and does not count.
///
/// Oldest wait first.
pub fn due(
    waiting: &[Waiting],
    reminded: &BTreeMap<IssueId, String>,
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
