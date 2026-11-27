//! The pulse of `klobuk serve`: one small JSON file the loop rewrites every tick, so the TUI, `klobuk repo
//! doctor` and a second `klobuk serve` can tell whether a loop runs, and why it stopped when it does not.
//!
//! The file lives in `.factory/runs/` (already ignored by git, so it never dirties main). A clean stop
//! writes `stopped_at` and the reason; a loop that was killed or crashed leaves a pulse that simply goes
//! stale, which [`judge`] reports as "stopped without a reason".

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// Directory (under `.factory/`) and file name of the pulse.
const RUNS_DIR: &str = "runs";
pub const PULSE_FILE: &str = "serve.json";

/// A pulse older than this many intervals is stale ...
pub const STALE_AFTER_INTERVALS: u64 = 3;
/// ... but never under this many seconds ...
pub const MIN_STALE_SECS: u64 = 120;
/// ... nor under this many start windows (`[worker] start_timeout_ms`): a worker start waits for the pane,
/// may answer the trust dialog and retries once in a fresh pane. The loop also beats around every start,
/// so this only has to cover one start, not a whole batch.
// shortcut: a start that waits longer than 3 windows still reads as stopped; raise it if live runs show that.
pub const START_WAITS: u64 = 3;
/// `[worker] start_timeout_ms` default, in seconds; what a pulse written without the field assumes.
pub const DEFAULT_START_TIMEOUT_S: u64 = 90;

fn default_start_timeout_s() -> u64 {
    DEFAULT_START_TIMEOUT_S
}

/// What the loop was started to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServeMode {
    /// `klobuk serve --dispatch`: also starts workers.
    Dispatch,
    /// `klobuk serve`: watches, merges and notifies only.
    Observe,
}

impl ServeMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ServeMode::Dispatch => "dispatch",
            ServeMode::Observe => "observe",
        }
    }
}

/// One thing the loop waits for the owner on: the issue and a few plain words (`question`,
/// `worker waits for input`). Written only while `klobuk serve --until-done` is idle on the owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PulseWait {
    pub issue: String,
    pub what: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pulse {
    pub pid: u32,
    pub started_at: String,
    pub last_tick_at: String,
    pub mode: ServeMode,
    pub interval_s: u64,
    pub max_ticks: Option<u64>,
    /// `[worker] start_timeout_ms` in seconds (rounded up): how long one worker start may block the loop.
    #[serde(default = "default_start_timeout_s")]
    pub start_timeout_s: u64,
    pub ticks: u64,
    /// What the loop is idle on right now: it is alive, has nothing to run and waits for the owner. Empty
    /// while it works, and after a stop. A pulse written before the field existed reads as empty.
    #[serde(default)]
    pub waiting: Vec<PulseWait>,
    /// Set by a clean stop only.
    pub stopped_at: Option<String>,
    /// `all_done`, `max_ticks`, `stuck`, `interrupted` or `error: <message>`.
    pub stop_reason: Option<String>,
}

impl Pulse {
    pub fn start(mode: ServeMode, interval_s: u64, max_ticks: Option<u64>, now: &str) -> Pulse {
        Pulse {
            pid: std::process::id(),
            started_at: now.to_string(),
            last_tick_at: now.to_string(),
            mode,
            interval_s,
            max_ticks,
            start_timeout_s: DEFAULT_START_TIMEOUT_S,
            ticks: 0,
            waiting: Vec::new(),
            stopped_at: None,
            stop_reason: None,
        }
    }

    /// Records what the idle loop waits for (empty: it is not idle on the owner). Returns whether it changed.
    pub fn set_waiting(&mut self, waiting: Vec<PulseWait>) -> bool {
        let changed = self.waiting != waiting;
        self.waiting = waiting;
        changed
    }

    /// Records a finished tick.
    pub fn tick(&mut self, ticks: u64, now: &str) {
        self.ticks = ticks;
        self.last_tick_at = now.to_string();
    }

    /// Records that the tick in progress is alive (it did not finish, so `ticks` stays).
    pub fn touch(&mut self, now: &str) {
        self.last_tick_at = now.to_string();
    }

    /// Records a clean stop.
    pub fn stop(&mut self, reason: impl Into<String>, now: &str) {
        self.stopped_at = Some(now.to_string());
        self.stop_reason = Some(reason.into());
        self.waiting.clear();
    }
}

/// What the pulse says about the loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopState {
    /// No pulse was ever written: `klobuk serve` never ran on this repo (or its pulse is unreadable).
    NeverStarted,
    Running {
        mode: ServeMode,
        /// Seconds since the last tick.
        age_s: i64,
        /// What the loop is idle on (see [`Pulse::waiting`]); empty while it works.
        waiting: Vec<PulseWait>,
    },
    Stopped {
        /// `None`: it stopped without saying why (killed, crashed, machine restarted).
        reason: Option<String>,
        /// Seconds since the stop, or since the last tick when there is no recorded stop.
        age_s: i64,
        ticks: u64,
        max_ticks: Option<u64>,
    },
}

/// The command that starts the loop again, in the mode it last ran in.
fn restart_command(mode: ServeMode) -> &'static str {
    match mode {
        ServeMode::Dispatch => "klobuk serve --dispatch",
        ServeMode::Observe => "klobuk serve",
    }
}

/// `12 s`, `14 min`, `3 h`, `2 d`.
pub fn fmt_age(secs: i64) -> String {
    let s = secs.max(0);
    match s {
        0..=59 => format!("{s} s"),
        60..=3599 => format!("{} min", s / 60),
        3600..=86399 => format!("{} h", s / 3600),
        _ => format!("{} d", s / 86400),
    }
}

fn seconds_between(now: OffsetDateTime, then: &str) -> Option<i64> {
    let then = OffsetDateTime::parse(then, &Rfc3339).ok()?;
    Some((now - then).whole_seconds().max(0))
}

/// Seconds of silence after which a pulse without a recorded stop counts as stale. The one figure
/// [`judge`] applies and the refusal of a second loop quotes.
pub fn stale_after_s(p: &Pulse) -> u64 {
    p.interval_s
        .saturating_mul(STALE_AFTER_INTERVALS)
        .max(MIN_STALE_SECS)
        .max(p.start_timeout_s.saturating_mul(START_WAITS))
}

/// Decides the loop state. `pid_alive` is `Some` only when the caller checked the process (the TUI does
/// not: it refreshes every couple of seconds and a process lookup is a child process on Windows).
/// A pulse without `stopped_at` is stopped when it is stale or its process is gone.
pub fn judge(pulse: Option<&Pulse>, now: OffsetDateTime, pid_alive: Option<bool>) -> LoopState {
    let Some(p) = pulse else {
        return LoopState::NeverStarted;
    };
    let stopped = |reason: Option<String>, since: &str| LoopState::Stopped {
        reason,
        age_s: seconds_between(now, since).unwrap_or(0),
        ticks: p.ticks,
        max_ticks: p.max_ticks,
    };
    if let (Some(at), reason) = (&p.stopped_at, &p.stop_reason) {
        return stopped(reason.clone(), at);
    }
    let Some(age_s) = seconds_between(now, &p.last_tick_at) else {
        return stopped(None, &p.last_tick_at);
    };
    if age_s as u64 > stale_after_s(p) || pid_alive == Some(false) {
        return stopped(None, &p.last_tick_at);
    }
    LoopState::Running {
        mode: p.mode,
        age_s,
        waiting: p.waiting.clone(),
    }
}

impl LoopState {
    pub fn is_running(&self) -> bool {
        matches!(self, LoopState::Running { .. })
    }

    /// Alive, idle, and waiting for the owner (see [`Pulse::waiting`]).
    pub fn is_waiting(&self) -> bool {
        matches!(self, LoopState::Running { waiting, .. } if !waiting.is_empty())
    }

    /// The header segment: `loop ok 12 s`, `loop observe`, `loop waiting for you: asf-7 question (+2)`,
    /// `LOOP STOPPED 14 min`, `loop not running`.
    pub fn header(&self) -> String {
        match self {
            LoopState::NeverStarted => "loop not running".into(),
            LoopState::Running { waiting, .. } if !waiting.is_empty() => {
                let more = match waiting.len() - 1 {
                    0 => String::new(),
                    n => format!(" (+{n})"),
                };
                format!(
                    "loop waiting for you: {} {}{more}",
                    waiting[0].issue, waiting[0].what
                )
            }
            LoopState::Running {
                mode: ServeMode::Observe,
                ..
            } => "loop observe".into(),
            LoopState::Running { age_s, .. } => format!("loop ok {}", fmt_age(*age_s)),
            LoopState::Stopped { age_s, .. } => format!("LOOP STOPPED {}", fmt_age(*age_s)),
        }
    }

    /// One line with the reason and the fix; `None` while the loop runs. `waiting` names what a `stuck`
    /// loop waits for (`asf-7 waits for your answer`), when the caller knows it.
    pub fn banner(&self, mode_hint: ServeMode, waiting: Option<&str>) -> Option<String> {
        match self {
            LoopState::Running { .. } => None,
            LoopState::NeverStarted => Some(format!(
                "the loop is not running — start it: {}",
                restart_command(mode_hint)
            )),
            LoopState::Stopped {
                reason,
                age_s,
                max_ticks,
                ticks,
            } => {
                let fix = restart_command(mode_hint);
                let ago = fmt_age(*age_s);
                Some(match reason.as_deref() {
                    None => format!(
                        "the loop stopped {ago} ago without a reason (killed, crashed or the machine restarted) — start it again: {fix}"
                    ),
                    Some("max_ticks") => format!(
                        "the loop stopped {ago} ago: max_ticks ({}) — start it again: {fix}",
                        max_ticks.unwrap_or(*ticks)
                    ),
                    Some("stuck") => match waiting {
                        Some(w) => format!("the loop stopped: stuck ({w})"),
                        None => format!("the loop stopped {ago} ago: stuck (nothing can start) — start it again: {fix}"),
                    },
                    Some("all_done") => {
                        format!("the loop stopped {ago} ago: all_done — start it again: {fix}")
                    }
                    Some(other) => format!("the loop stopped {ago} ago: {other} — start it again: {fix}"),
                })
            }
        }
    }
}

impl LoopState {
    /// The long form of [`LoopState::banner`] for a window: what the state means and what to do.
    /// `None` while the loop runs.
    pub fn explain(&self, mode_hint: ServeMode, waiting: Option<&str>) -> Option<String> {
        let headline = self.banner(mode_hint, waiting)?;
        let fix = restart_command(mode_hint);
        let meaning = match self {
            LoopState::Running { .. } => return None,
            LoopState::NeverStarted => {
                "No `klobuk serve` has written a pulse on this repo yet, so nothing starts workers, merges queued work or sends notifications."
                    .to_string()
            }
            LoopState::Stopped { reason, ticks, .. } => match reason.as_deref() {
                None => format!(
                    "The pulse went silent after {ticks} tick(s) and no stop reason was written: the process was killed (Ctrl+C), crashed or the machine restarted."
                ),
                Some("max_ticks") => format!(
                    "The loop reached its tick limit ({ticks} tick(s)): `--max-ticks` or `[serve] max_ticks`. Leave the limit out or raise it."
                ),
                Some("stuck") => "Nothing is running and nothing can start. If something waits for you (a question, a blocked issue or run), the loop was started with `--exit-when-waiting`: answer or unblock it, then start the loop. If nothing does, the open issues cannot move on their own (a dependency that can never close, a ready issue without `--dispatch`).".to_string(),
                Some("all_done") => "Every open issue is finished.".to_string(),
                Some(other) => format!("The loop stopped with: {other}."),
            },
        };
        Some(format!("{headline}\n\n{meaning}\n\nStart it again: {fix}"))
    }
}

/// `.factory/runs/serve.json` of the repository at `root`.
pub fn pulse_path(root: &Path) -> PathBuf {
    root.join(".factory").join(RUNS_DIR).join(PULSE_FILE)
}

/// The pulse on disk; `Ok(None)` when there is none.
pub fn read(root: &Path) -> Result<Option<Pulse>, String> {
    let path = pulse_path(root);
    match std::fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(raw.trim_start_matches('\u{feff}'))
            .map(Some)
            .map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Writes the pulse atomically (temp file, then rename), UTF-8 without BOM, LF.
pub fn write(root: &Path, pulse: &Pulse) -> std::io::Result<()> {
    let path = pulse_path(root);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut text = serde_json::to_string_pretty(pulse).map_err(std::io::Error::other)?;
    text.push('\n');
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)
}

/// Whether process `pid` exists. `None` when it cannot be told.
pub fn pid_alive(pid: u32) -> Option<bool> {
    if pid == std::process::id() {
        return Some(true);
    }
    if cfg!(windows) {
        let out = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        Some(text.contains(&format!("\"{pid}\"")))
    } else {
        let status = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .ok()?;
        Some(status.status.success())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_800_000_000 + secs).unwrap()
    }

    fn ts(secs: i64) -> String {
        at(secs).format(&Rfc3339).unwrap()
    }

    fn pulse(interval: u64) -> Pulse {
        Pulse::start(ServeMode::Dispatch, interval, Some(600), &ts(0))
    }

    #[test]
    fn no_pulse_means_never_started() {
        assert_eq!(judge(None, at(0), None), LoopState::NeverStarted);
    }

    #[test]
    fn a_fresh_pulse_is_running_with_its_age() {
        let mut p = pulse(10);
        p.tick(3, &ts(100));
        assert_eq!(
            judge(Some(&p), at(112), None),
            LoopState::Running {
                mode: ServeMode::Dispatch,
                age_s: 12,
                waiting: Vec::new(),
            }
        );
    }

    #[test]
    fn a_pulse_older_than_three_intervals_but_within_the_floor_is_still_running() {
        let mut p = pulse(10);
        p.start_timeout_s = 1; // a short start window leaves only the floor
        assert_eq!(stale_after_s(&p), MIN_STALE_SECS);
        assert!(judge(Some(&p), at(MIN_STALE_SECS as i64), None).is_running());
        assert!(!judge(Some(&p), at(MIN_STALE_SECS as i64 + 1), None).is_running());
    }

    #[test]
    fn a_long_interval_raises_the_limit_to_three_intervals() {
        let mut p = pulse(100); // 300 s
        p.start_timeout_s = 1;
        assert_eq!(stale_after_s(&p), 300);
        assert!(judge(Some(&p), at(300), None).is_running());
        assert!(!judge(Some(&p), at(301), None).is_running());
    }

    #[test]
    fn a_tick_that_blocks_on_slow_worker_starts_is_not_read_as_stopped() {
        // default window: one start waits up to 90 s, a tick starts several workers one after another
        let p = pulse(10);
        assert_eq!(p.start_timeout_s, 90);
        for secs in [150, 200, 260] {
            assert!(
                judge(Some(&p), at(secs), Some(true)).is_running(),
                "{secs} s into a tick that is still starting workers"
            );
        }
        assert!(!judge(Some(&p), at(stale_after_s(&p) as i64 + 1), None).is_running());
    }

    #[test]
    fn the_limit_follows_the_start_window_the_loop_runs_with() {
        let mut p = pulse(10);
        p.start_timeout_s = 200;
        assert_eq!(stale_after_s(&p), START_WAITS * 200);
        p.interval_s = u64::MAX; // a silly config must not overflow
        assert_eq!(stale_after_s(&p), u64::MAX);
    }

    #[test]
    fn a_pulse_file_written_before_the_start_window_was_recorded_still_reads() {
        let mut p = pulse(10);
        p.start_timeout_s = 5;
        let mut v = serde_json::to_value(&p).unwrap();
        v.as_object_mut().unwrap().remove("start_timeout_s");
        let old: Pulse = serde_json::from_value(v).unwrap();
        assert_eq!(old.start_timeout_s, DEFAULT_START_TIMEOUT_S);
    }

    #[test]
    fn a_stale_pulse_stopped_without_a_reason() {
        let p = pulse(10);
        let state = judge(Some(&p), at(900), None);
        assert_eq!(
            state,
            LoopState::Stopped {
                reason: None,
                age_s: 900,
                ticks: 0,
                max_ticks: Some(600)
            }
        );
        assert!(state
            .banner(ServeMode::Dispatch, None)
            .unwrap()
            .contains("without a reason"));
    }

    #[test]
    fn a_dead_pid_stops_a_fresh_pulse_and_a_live_one_does_not() {
        let p = pulse(10);
        assert!(!judge(Some(&p), at(5), Some(false)).is_running());
        assert!(judge(Some(&p), at(5), Some(true)).is_running());
        assert!(judge(Some(&p), at(5), None).is_running());
    }

    #[test]
    fn a_recorded_stop_wins_over_freshness_and_keeps_its_reason() {
        let mut p = pulse(10);
        p.tick(633, &ts(1000));
        p.stop("max_ticks", &ts(1001));
        let state = judge(Some(&p), at(1001 + 14 * 60), Some(true));
        assert_eq!(
            state,
            LoopState::Stopped {
                reason: Some("max_ticks".into()),
                age_s: 14 * 60,
                ticks: 633,
                max_ticks: Some(600)
            }
        );
    }

    #[test]
    fn an_unparseable_time_counts_as_stopped() {
        let mut p = pulse(10);
        p.last_tick_at = "garbage".into();
        assert!(!judge(Some(&p), at(1), None).is_running());
    }

    #[test]
    fn header_texts() {
        let running = |mode, age_s| LoopState::Running {
            mode,
            age_s,
            waiting: Vec::new(),
        };
        assert_eq!(running(ServeMode::Dispatch, 12).header(), "loop ok 12 s");
        assert_eq!(running(ServeMode::Observe, 12).header(), "loop observe");
        assert_eq!(LoopState::NeverStarted.header(), "loop not running");
        let stopped = LoopState::Stopped {
            reason: None,
            age_s: 14 * 60,
            ticks: 1,
            max_ticks: None,
        };
        assert_eq!(stopped.header(), "LOOP STOPPED 14 min");
    }

    #[test]
    fn banner_texts_name_the_reason_and_the_fix() {
        let stopped = |reason: Option<&str>| LoopState::Stopped {
            reason: reason.map(String::from),
            age_s: 14 * 60,
            ticks: 633,
            max_ticks: Some(633),
        };
        assert_eq!(
            stopped(Some("max_ticks"))
                .banner(ServeMode::Dispatch, None)
                .unwrap(),
            "the loop stopped 14 min ago: max_ticks (633) — start it again: klobuk serve --dispatch"
        );
        assert_eq!(
            LoopState::NeverStarted.banner(ServeMode::Dispatch, None).unwrap(),
            "the loop is not running — start it: klobuk serve --dispatch"
        );
        assert_eq!(
            stopped(Some("stuck"))
                .banner(ServeMode::Dispatch, Some("asf-7 waits for your answer"))
                .unwrap(),
            "the loop stopped: stuck (asf-7 waits for your answer)"
        );
        assert!(stopped(Some("error: boom"))
            .banner(ServeMode::Observe, None)
            .unwrap()
            .ends_with("start it again: klobuk serve"));
        let running = LoopState::Running {
            mode: ServeMode::Dispatch,
            age_s: 1,
            waiting: Vec::new(),
        };
        assert_eq!(running.banner(ServeMode::Dispatch, None), None);
    }

    #[test]
    fn the_explanation_adds_the_meaning_and_the_command() {
        let stopped = LoopState::Stopped {
            reason: Some("max_ticks".into()),
            age_s: 60,
            ticks: 633,
            max_ticks: Some(633),
        };
        let text = stopped.explain(ServeMode::Dispatch, None).unwrap();
        assert!(
            text.starts_with("the loop stopped 1 min ago: max_ticks (633)"),
            "{text}"
        );
        assert!(
            text.contains("tick limit") && text.ends_with("Start it again: klobuk serve --dispatch"),
            "{text}"
        );
        let running = LoopState::Running {
            mode: ServeMode::Observe,
            age_s: 1,
            waiting: Vec::new(),
        };
        assert_eq!(running.explain(ServeMode::Observe, None), None);
        let never = LoopState::NeverStarted.explain(ServeMode::Observe, None).unwrap();
        assert!(
            never.starts_with("the loop is not running") && never.ends_with("Start it again: klobuk serve"),
            "{never}"
        );
    }

    #[test]
    fn the_file_round_trips_and_a_missing_one_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(dir.path()), Ok(None));
        let mut p = pulse(10);
        p.tick(2, &ts(20));
        write(dir.path(), &p).unwrap();
        assert_eq!(read(dir.path()), Ok(Some(p.clone())));
        p.stop("all_done", &ts(30));
        write(dir.path(), &p).unwrap();
        assert_eq!(
            read(dir.path()).unwrap().unwrap().stop_reason.as_deref(),
            Some("all_done")
        );
        std::fs::write(pulse_path(dir.path()), "{ not json").unwrap();
        assert!(read(dir.path()).is_err());
    }

    fn wait(issue: &str, what: &str) -> PulseWait {
        PulseWait {
            issue: issue.into(),
            what: what.into(),
        }
    }

    #[test]
    fn an_idle_loop_says_what_it_waits_for_and_is_still_running() {
        let mut p = pulse(10);
        assert!(p.set_waiting(vec![wait("asf-7", "question"), wait("asf-9", "issue blocked")]));
        assert!(!p.set_waiting(p.waiting.clone()), "the same list is no change");
        let state = judge(Some(&p), at(5), Some(true));
        assert!(state.is_running() && state.is_waiting(), "{state:?}");
        assert_eq!(state.header(), "loop waiting for you: asf-7 question (+1)");
        assert_eq!(state.banner(ServeMode::Dispatch, None), None, "alive: no banner");
        p.set_waiting(vec![wait("asf-7", "worker waits for input")]);
        assert_eq!(
            judge(Some(&p), at(5), None).header(),
            "loop waiting for you: asf-7 worker waits for input"
        );
        p.set_waiting(Vec::new());
        let state = judge(Some(&p), at(5), None);
        assert!(!state.is_waiting());
        assert_eq!(state.header(), "loop ok 5 s");
    }

    #[test]
    fn a_stop_clears_what_the_loop_waited_for_and_an_old_pulse_reads_as_not_waiting() {
        let mut p = pulse(10);
        p.set_waiting(vec![wait("asf-7", "question")]);
        p.stop("max_ticks", &ts(9));
        assert!(p.waiting.is_empty());
        let mut v = serde_json::to_value(pulse(10)).unwrap();
        v.as_object_mut().unwrap().remove("waiting");
        let old: Pulse = serde_json::from_value(v).unwrap();
        assert!(old.waiting.is_empty());
    }

    #[test]
    fn this_process_is_alive() {
        assert_eq!(pid_alive(std::process::id()), Some(true));
    }
}
