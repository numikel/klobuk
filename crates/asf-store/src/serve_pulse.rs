//! The pulse of `asf serve`: one small JSON file the loop rewrites every tick, so the TUI, `asf repo
//! doctor` and a second `asf serve` can tell whether a loop runs, and why it stopped when it does not.
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
/// ... but never under this many seconds: one tick can block on a worker start (up to 90 s).
pub const MIN_STALE_SECS: u64 = 120;

/// What the loop was started to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServeMode {
    /// `asf serve --dispatch`: also starts workers.
    Dispatch,
    /// `asf serve`: watches, merges and notifies only.
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pulse {
    pub pid: u32,
    pub started_at: String,
    pub last_tick_at: String,
    pub mode: ServeMode,
    pub interval_s: u64,
    pub max_ticks: Option<u64>,
    pub ticks: u64,
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
            ticks: 0,
            stopped_at: None,
            stop_reason: None,
        }
    }

    /// Records a finished tick.
    pub fn tick(&mut self, ticks: u64, now: &str) {
        self.ticks = ticks;
        self.last_tick_at = now.to_string();
    }

    /// Records a clean stop.
    pub fn stop(&mut self, reason: impl Into<String>, now: &str) {
        self.stopped_at = Some(now.to_string());
        self.stop_reason = Some(reason.into());
    }
}

/// What the pulse says about the loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopState {
    /// No pulse was ever written: `asf serve` never ran on this repo (or its pulse is unreadable).
    NeverStarted,
    Running {
        mode: ServeMode,
        /// Seconds since the last tick.
        age_s: i64,
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
        ServeMode::Dispatch => "asf serve --dispatch",
        ServeMode::Observe => "asf serve",
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
    let limit = (p.interval_s.saturating_mul(STALE_AFTER_INTERVALS)).max(MIN_STALE_SECS);
    if age_s as u64 > limit || pid_alive == Some(false) {
        return stopped(None, &p.last_tick_at);
    }
    LoopState::Running { mode: p.mode, age_s }
}

impl LoopState {
    pub fn is_running(&self) -> bool {
        matches!(self, LoopState::Running { .. })
    }

    /// The header segment: `loop ok 12 s`, `loop observe`, `LOOP STOPPED 14 min`, `loop not running`.
    pub fn header(&self) -> String {
        match self {
            LoopState::NeverStarted => "loop not running".into(),
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
                age_s: 12
            }
        );
    }

    #[test]
    fn a_pulse_older_than_three_intervals_but_within_the_floor_is_still_running() {
        let p = pulse(10);
        assert!(judge(Some(&p), at(MIN_STALE_SECS as i64), None).is_running());
        assert!(!judge(Some(&p), at(MIN_STALE_SECS as i64 + 1), None).is_running());
    }

    #[test]
    fn a_long_interval_raises_the_limit_to_three_intervals() {
        let p = pulse(100); // 300 s
        assert!(judge(Some(&p), at(300), None).is_running());
        assert!(!judge(Some(&p), at(301), None).is_running());
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
        let running = |mode, age_s| LoopState::Running { mode, age_s };
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
            "the loop stopped 14 min ago: max_ticks (633) — start it again: asf serve --dispatch"
        );
        assert_eq!(
            LoopState::NeverStarted.banner(ServeMode::Dispatch, None).unwrap(),
            "the loop is not running — start it: asf serve --dispatch"
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
            .ends_with("start it again: asf serve"));
        let running = LoopState::Running {
            mode: ServeMode::Dispatch,
            age_s: 1,
        };
        assert_eq!(running.banner(ServeMode::Dispatch, None), None);
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

    #[test]
    fn this_process_is_alive() {
        assert_eq!(pid_alive(std::process::id()), Some(true));
    }
}
