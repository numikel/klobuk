//! Terminal UI (M3): a ratatui app meant to run as a pane in Herdr.
//!
//! Layout of this module:
//! - `theme`: the symbols and every style, `NO_COLOR` included; `chrome`: header, tabs, banner, rules,
//!   key hints and the too-small gate; `bindings`: the one table of key bindings behind the footer and
//!   the help page; `overlays`: the answer overlay, help page, comment modal, confirmation and input.
//! - `snapshot`: what is loaded from the store, and the helpers the views share.
//! - `views/<name>`: one module per view, each with `State`, `render`, `handle_key` and `BINDINGS`.
//! - `app`: `App` (the current view, the seven states, the overlay stack, the status message) and the
//!   dispatch of keys and drawing; this file: the event loop and every action that needs IO.
//!
//! Synchronous loop: keys are polled every 200 ms, the store snapshot is refreshed every second and
//! the agents' live state every two seconds. Quick actions go through the same `Worker` and
//! merge-queue code the CLI uses, so the TUI never has behaviour of its own. The two slow ones, a
//! tick (which can run the gates for minutes) and a worker start (up to 180 s), run as child
//! processes of this very executable and are polled from the loop, so the screen keeps redrawing.

pub mod app;
pub mod bindings;
pub mod chrome;
pub mod overlays;
pub mod snapshot;
pub mod theme;
pub mod views;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use asf_core::*;
use ratatui::crossterm::event::{self, Event as TermEvent, KeyEventKind};

use crate::ctx::{Ctx, CONFIG_FILE, FACTORY_DIR};
use crate::output::AppError;
use crate::worker::Worker;
use app::{Action, App, View};
use snapshot::{config_rows, load_detail, load_snapshot, read_config_text, RuntimeState};
use theme::Theme;

const KEY_POLL: Duration = Duration::from_millis(200);
/// Faster while a background action runs, so the spinner turns smoothly.
const BUSY_POLL: Duration = Duration::from_millis(100);
const STORE_REFRESH: Duration = Duration::from_secs(1);
const AGENT_REFRESH: Duration = Duration::from_secs(2);

/// Applies one Config-view edit and returns the status line for it.
///
/// The file is written only when `config_edit::set_key` accepted the value; a refused key or value
/// (or an unreadable file) leaves it byte-identical and yields an `error: ...` line instead.
pub(crate) fn config_set_status(root: &Path, key: &str, value: &str) -> String {
    config_edit_status(root, key, |text| asf_core::config_edit::set_key(text, key, value))
}

/// Removes an optional config key and returns the status line for it; same file rules as
/// [`config_set_status`] (a refused key leaves the file byte-identical).
pub(crate) fn config_unset_status(root: &Path, key: &str) -> String {
    config_edit_status(root, key, |text| asf_core::config_edit::unset_key(text, key))
}

/// Status line for a sent key sequence: `sent 2 enter to asf-27`.
pub(crate) fn keys_status(id: &IssueId, keys: &[String]) -> String {
    format!("sent {} to {id}", keys.join(" "))
}

/// The last 20 lines of the worker pane for the answer overlay, or a note saying why it is not there.
fn peek_screen(worker: &Worker<'_>, id: &IssueId) -> String {
    let run = match worker.run_for(id) {
        Ok(r) => r,
        Err(e) => return format!("(cannot read {id}: {})", e.message),
    };
    let (Some(pane), Some(name)) = (&run.pane_id, &run.agent_name) else {
        return format!("(run of {id} has no pane)");
    };
    let handle = AgentHandle {
        name: name.clone(),
        pane_id: pane.clone(),
        workspace_id: run.workspace_id.clone().unwrap_or_default(),
    };
    worker
        .rt
        .read(&handle, 20)
        .unwrap_or_else(|e| format!("(cannot read the pane: {e})"))
}

fn config_edit_status(
    root: &Path,
    key: &str,
    edit: impl FnOnce(&str) -> Result<String, asf_core::ConfigError>,
) -> String {
    let path = root.join(FACTORY_DIR).join(CONFIG_FILE);
    let text = match read_config_text(root) {
        Ok(t) => t,
        Err(e) => return format!("error: cannot read {}: {e}", path.display()),
    };
    let edited = match edit(&text) {
        Ok(t) => t,
        Err(e) => return format!("error: {e}"),
    };
    if edited != text {
        if let Err(e) = std::fs::write(&path, edited.as_bytes()) {
            return format!("error: cannot write {}: {e}", path.display());
        }
    }
    let shown = |t: &str| {
        config_rows(t)
            .into_iter()
            .find(|r| r.key == key)
            .map(|r| r.value)
            .unwrap_or_default()
    };
    // The same sentence `asf config set` prints, so the CLI and this view cannot disagree.
    format!(
        "{key}: {} -> {} ({})",
        shown(&text),
        shown(&edited),
        asf_core::config_edit::apply_note(key)
    )
}

/// A refused Config edit (`error: ...`) is also kept by the Config view, which draws it under the row
/// of the key until the next key press. Any other result, or any other action, leaves the view alone.
pub(crate) fn note_config_result(app: &mut App, action: &Action, status: &str) {
    if let Action::SetConfig(key, _) | Action::UnsetConfig(key) = action {
        if status.starts_with("error") {
            app.config.fail(key, status);
        }
    }
}

/// A slow action that runs as a child process of this executable.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum BgKind {
    Tick,
    Start(IssueId),
    /// `asf run verify <id>`: the Now view's `v`.
    Verify(IssueId),
    /// `asf merge retry <id>`: the Now view's `r`.
    Retry(IssueId),
}

impl BgKind {
    /// Header label (the spinner and an ellipsis are added around it).
    fn label(&self) -> String {
        match self {
            BgKind::Tick => "tick".into(),
            BgKind::Start(id) => format!("starting {id}"),
            BgKind::Verify(id) => format!("verifying {id}"),
            BgKind::Retry(id) => format!("retrying {id}"),
        }
    }

    /// What the failure messages call it.
    fn noun(&self) -> String {
        match self {
            BgKind::Tick => "tick".into(),
            BgKind::Start(id) => format!("start {id}"),
            BgKind::Verify(id) => format!("verify {id}"),
            BgKind::Retry(id) => format!("retry {id}"),
        }
    }

    fn args(&self, root: &Path) -> Vec<OsString> {
        let mut a: Vec<OsString> = vec!["--repo".into(), root.as_os_str().to_owned()];
        match self {
            BgKind::Tick => a.extend(["serve", "--once", "--json"].map(OsString::from)),
            BgKind::Start(id) | BgKind::Verify(id) | BgKind::Retry(id) => {
                let (group, verb) = match self {
                    BgKind::Verify(_) => ("run", "verify"),
                    BgKind::Retry(_) => ("merge", "retry"),
                    _ => ("run", "start"),
                };
                a.extend([group, verb].map(OsString::from));
                a.push(id.as_str().into());
                a.push("--json".into());
            }
        }
        a
    }
}

fn count_at(v: &serde_json::Value, path: &[&str]) -> usize {
    path.iter()
        .try_fold(v, |node, seg| node.get(*seg))
        .and_then(|n| n.as_array())
        .map_or(0, Vec::len)
}

/// The status message for a finished child: `✓ <summary>` from the JSON envelope, `✗ <its error
/// message>`, or, when the child printed no envelope at all (a crash), what is known about it.
pub(crate) fn bg_outcome(kind: &BgKind, stdout: &str, stderr: &str, code: Option<i32>) -> String {
    let envelope = serde_json::from_str::<serde_json::Value>(stdout.trim())
        .ok()
        .or_else(|| {
            stdout
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .and_then(|l| serde_json::from_str(l).ok())
        })
        .filter(|v| v.get("ok").is_some());
    match envelope {
        Some(v) if v["ok"] == true => {
            let r = &v["result"];
            match kind {
                BgKind::Tick => format!(
                    "✓ tick: {} transition(s), {} merge item(s), {} promoted, {} epic(s) closed",
                    count_at(r, &["watch", "transitions"]),
                    count_at(r, &["merge", "items"]),
                    count_at(r, &["promoted"]),
                    count_at(r, &["closed_epics"]),
                ),
                BgKind::Verify(id) => {
                    let word = |k: &str| r[k].as_str().unwrap_or("?").to_string();
                    let detail = r["detail"]
                        .as_str()
                        .and_then(|d| d.lines().map(str::trim).find(|l| !l.is_empty()))
                        .map(|d| format!(": {d}"))
                        .unwrap_or_default();
                    // The command succeeds even when the verification it ran does not.
                    let mark = if word("to") == "failed" { '✗' } else { '✓' };
                    format!(
                        "{mark} verify {}: {} -> {}{detail}",
                        r["issue_id"].as_str().unwrap_or(id.as_str()),
                        word("from"),
                        word("to")
                    )
                }
                BgKind::Retry(id) => format!(
                    "✓ {} re-queued (#{})",
                    r["issue_id"].as_str().unwrap_or(id.as_str()),
                    r["id"].as_i64().map_or("?".to_string(), |n| n.to_string())
                ),
                BgKind::Start(id) => {
                    let text = |p: &[&str]| {
                        p.iter()
                            .try_fold(r, |n, s| n.get(*s))
                            .and_then(|n| n.as_str())
                            .unwrap_or("?")
                            .to_string()
                    };
                    let issue = r["run"]["issue_id"].as_str().unwrap_or(id.as_str());
                    format!(
                        "✓ started {issue} in {} (pane {})",
                        text(&["workspace_id"]),
                        text(&["pane_id"])
                    )
                }
            }
        }
        Some(v) => format!("✗ {}", v["error"]["message"].as_str().unwrap_or("unknown error")),
        None => match code {
            Some(c) => {
                let first = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
                if first.is_empty() {
                    format!("✗ {} exited with {c}", kind.noun())
                } else {
                    format!("✗ {} exited with {c}: {first}", kind.noun())
                }
            }
            None => format!("✗ {} was terminated", kind.noun()),
        },
    }
}

/// A running background child. Its output is drained on two threads so a chatty child can never
/// block on a full pipe; stdin is null, so it cannot touch the terminal the TUI owns.
struct Background {
    child: Child,
    kind: BgKind,
    stdout: Option<JoinHandle<Vec<u8>>>,
    stderr: Option<JoinHandle<Vec<u8>>>,
    /// When the child was first seen to have exited; the pipes get [`DRAIN_GRACE`] after that.
    exited: Option<Instant>,
}

/// How long after the child exits its pipes may stay open (a grandchild that inherited the handle)
/// before the outcome is reported from what was read; the UI never blocks on a join.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

fn drain(mut pipe: impl Read + Send + 'static) -> JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        buf
    })
}

impl Background {
    fn spawn(kind: BgKind, root: &Path) -> Result<Background, AppError> {
        let exe = std::env::current_exe()
            .map_err(|e| AppError::io(format!("cannot find the asf executable: {e}")))?;
        let mut child = Command::new(exe)
            .args(kind.args(root))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| AppError::io(format!("cannot start {}: {e}", kind.noun())))?;
        let stdout = child.stdout.take().map(drain);
        let stderr = child.stderr.take().map(drain);
        Ok(Background {
            child,
            kind,
            stdout,
            stderr,
            exited: None,
        })
    }

    /// `Some(status message)` once the child has exited, `None` while it runs.
    fn poll(&mut self) -> Option<String> {
        let code = match self.child.try_wait() {
            Ok(None) => return None,
            Ok(Some(status)) => status.code(),
            Err(e) => return Some(format!("✗ cannot poll {}: {e}", self.kind.noun())),
        };
        let exited = *self.exited.get_or_insert_with(Instant::now);
        let open = |h: &Option<JoinHandle<Vec<u8>>>| h.as_ref().is_some_and(|h| !h.is_finished());
        if (open(&self.stdout) || open(&self.stderr)) && exited.elapsed() < DRAIN_GRACE {
            return None;
        }
        // Past the grace period an unfinished reader is dropped (detached) instead of joined.
        let text = |h: Option<JoinHandle<Vec<u8>>>| {
            h.filter(|h| h.is_finished())
                .and_then(|h| h.join().ok())
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default()
        };
        let (out, err) = (text(self.stdout.take()), text(self.stderr.take()));
        Some(bg_outcome(&self.kind, &out, &err, code))
    }
}

/// Posts the human's ANSWER to QUESTION comment `question` on `id` and returns the status line. It is
/// the comment `asf issue comment --lane human --type ANSWER --reply-to <q>` writes: the worker finds
/// it in its inbox, and the question stops blocking `asf issue finish`.
///
/// The answer also settles the human's side: the issue's comments to the human are marked read (the
/// BLOCKER that announced the wait, and the answer itself, which the store shows to every lane), so the
/// Inbox does not list the same thing a second time. Like the Inbox's `m`, the cursor is per issue.
pub(crate) fn post_answer(
    store: &impl IssueStore,
    id: &IssueId,
    question: i64,
    text: &str,
) -> Result<String, AppError> {
    store.add_comment(&NewComment {
        issue_id: id.clone(),
        author: LANE_HUMAN.to_string(),
        kind: CommentType::Answer,
        to_lane: id.to_string(),
        body: text.to_string(),
        reply_to: Some(question),
    })?;
    store.mark_read(LANE_HUMAN, id)?;
    Ok(format!("answered #{question} on {id}"))
}

/// The amount typed into the budget input: `14`, `14.5` or `$14.50`.
pub(crate) fn parse_usd(typed: &str) -> Result<f64, AppError> {
    typed
        .trim()
        .trim_start_matches('$')
        .parse()
        .map_err(|_| AppError::usage(format!("`{}` is not a number of USD", typed.trim())))
}

/// Executes one quick action; returns the status line to show. `Tick` and `Start` are slow and are
/// spawned by the event loop instead (see [`Background`]).
fn execute(action: Action, ctx: &Ctx, worker: &Worker<'_>) -> Result<Option<String>, AppError> {
    Ok(match action {
        // Peek is served by the event loop (it needs the app state); nothing to run here.
        Action::None | Action::Quit | Action::Refresh | Action::Peek(_) | Action::LoadDetail(_) => None,
        Action::Tick | Action::Start(_) | Action::Verify(_) | Action::Retry(_) => None,
        // The app consumed these itself; none of them reaches the event loop.
        Action::Status(_)
        | Action::OpenAnswer(_)
        | Action::OpenInput { .. }
        | Action::Confirm { .. }
        | Action::OpenComment(_)
        | Action::OpenGate(_)
        | Action::OpenOnBoard(_) => None,
        Action::Answer(id, question, text) => Some(post_answer(&ctx.store, &id, question, &text)?),
        Action::Focus(id) => {
            let run = worker.run_for(&id)?;
            if let (Some(pane), Some(name)) = (&run.pane_id, &run.agent_name) {
                worker
                    .rt
                    .focus(&AgentHandle {
                        name: name.clone(),
                        pane_id: pane.clone(),
                        workspace_id: run.workspace_id.clone().unwrap_or_default(),
                    })
                    .map_err(|e| AppError::new(crate::output::ErrorCode::Backend, e.to_string()))?;
            }
            Some(format!("focused {id}"))
        }
        Action::Reply(id, text) => {
            worker.reply(&id, &text)?;
            Some(format!("sent to {id}"))
        }
        Action::Keys(id, keys) => {
            worker.keys(&id, &keys)?;
            Some(keys_status(&id, &keys))
        }
        Action::Enqueue(id) => {
            let m = crate::commands::merge::enqueue_checked(ctx, &id, false)?;
            Some(format!("queued #{} {}", m.id, id))
        }
        Action::Abort(id) => {
            worker.abort(&id)?;
            Some(format!("aborted {id}"))
        }
        Action::RaiseBudget(id, typed) => {
            let usd = parse_usd(&typed)?;
            Some(crate::commands::run::raise_budget(ctx, &id, usd)?.text())
        }
        Action::SetConfig(key, value) => Some(config_set_status(&ctx.root, &key, &value)),
        Action::UnsetConfig(key) => Some(config_unset_status(&ctx.root, &key)),
        Action::MarkRead(id) => {
            let n = ctx.store.mark_read(LANE_HUMAN, &id)?;
            Some(format!("{id}: {n} message(s) marked read"))
        }
        Action::MarkAllRead => {
            let n = ctx.store.inbox(LANE_HUMAN, false, false)?.len();
            Some(format!("inbox: {n} item(s) marked read"))
        }
    })
}

/// One frame as text (for `asf tui --once` and debugging).
pub fn run_once(ctx: &Ctx, view: Option<View>) -> Result<String, AppError> {
    let rt = asf_runtime::from_config(&ctx.config.worker.runtime, &ctx.root).ok();
    let snap = load_snapshot(ctx, rt.as_deref(), &BTreeMap::new(), &RuntimeState::default())?;
    let mut app = App::new(snap);
    app.theme = Theme::from_env();
    if let Some(v) = view {
        app.view = v;
    }
    // a still picture has no keys: the first item of the queue shows its steps, so they can be checked
    app.queue.expanded = true;
    Ok(chrome::render_to_text(&app, 120, 30))
}

pub fn run_interactive(ctx: &Ctx, view: Option<View>) -> Result<(), AppError> {
    let worker = Worker::new(ctx)?;
    let mut app = App::new(load_snapshot(
        ctx,
        Some(worker.rt.as_ref()),
        &BTreeMap::new(),
        &RuntimeState::default(),
    )?);
    app.theme = Theme::from_env();
    if let Some(v) = view {
        app.view = v;
    }
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app, ctx, &worker);
    ratatui::restore();
    result
}

/// Loads the Board detail of `id` into the snapshot; a failure leaves the panel without it.
fn load_detail_into(app: &mut App, ctx: &Ctx, id: &IssueId) {
    match load_detail(ctx, id) {
        Ok(detail) => app.set_detail(detail),
        Err(e) => app.detail_failed(e.message),
    }
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    ctx: &Ctx,
    worker: &Worker<'_>,
) -> Result<(), AppError> {
    let started = Instant::now();
    let mut last_store = Instant::now();
    let mut last_agents = Instant::now() - AGENT_REFRESH;
    let mut background: Option<Background> = None;
    loop {
        app.advance(started.elapsed());
        terminal
            .draw(|f| chrome::render(f, app))
            .map_err(|e| AppError::io(e.to_string()))?;
        let wait = if background.is_some() { BUSY_POLL } else { KEY_POLL };
        if event::poll(wait).map_err(|e| AppError::io(e.to_string()))? {
            if let TermEvent::Key(key) = event::read().map_err(|e| AppError::io(e.to_string()))? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                let action = app.handle_key(key);
                if action == Action::Quit {
                    // A running child is left to finish: half a tick or a half-started worker is worse.
                    return Ok(());
                }
                if let Action::Peek(id) = &action {
                    let text = peek_screen(worker, id);
                    app.set_pane_preview(id.clone(), text.clone());
                    app.set_screen(text);
                }
                if let Action::LoadDetail(id) = &action {
                    load_detail_into(app, ctx, id);
                }
                let kind = match &action {
                    Action::Tick => Some(BgKind::Tick),
                    Action::Start(id) => Some(BgKind::Start(id.clone())),
                    Action::Verify(id) => Some(BgKind::Verify(id.clone())),
                    Action::Retry(id) => Some(BgKind::Retry(id.clone())),
                    _ => None,
                };
                if let Some(kind) = kind {
                    let label = kind.label();
                    match Background::spawn(kind, &ctx.root) {
                        Ok(b) => {
                            background = Some(b);
                            app.busy = Some(label);
                        }
                        Err(e) => app.status = Some(format!("✗ {}", e.message)),
                    }
                } else {
                    match execute(action.clone(), ctx, worker) {
                        Ok(Some(msg)) => {
                            note_config_result(app, &action, &msg);
                            app.status = Some(msg);
                        }
                        Ok(None) => {}
                        Err(e) => app.status = Some(format!("error: {}", e.message)),
                    }
                }
                if action != Action::None {
                    last_store -= STORE_REFRESH;
                    last_agents -= AGENT_REFRESH;
                }
            }
        }
        if let Some(msg) = background.as_mut().and_then(Background::poll) {
            background = None;
            app.busy = None;
            app.status = Some(msg);
            last_store -= STORE_REFRESH;
            last_agents -= AGENT_REFRESH;
        }
        if last_store.elapsed() >= STORE_REFRESH {
            let with_agents = last_agents.elapsed() >= AGENT_REFRESH;
            let rt = if with_agents {
                Some(worker.rt.as_ref())
            } else {
                None
            };
            match load_snapshot(ctx, rt, &app.snap.agents, &app.snap.runtime_state()) {
                Ok(snap) => app.replace(snap),
                Err(e) => app.status = Some(format!("refresh failed: {}", e.message)),
            }
            last_store = Instant::now();
            if with_agents {
                last_agents = Instant::now();
            }
        }
        // A view switch leaves the Board without the detail of its selection, and a store refresh
        // leaves it with one that may be out of date (a new QUESTION, a dependency that moved on):
        // load it before the next draw so the panel never shows the old one for a whole interval.
        if let Some(id) = app.detail_wanted() {
            load_detail_into(app, ctx, &id);
        }
        // The Agents view shows the pane of the selected run: read it when the view opens and after
        // every store refresh, not only when a key asks for it.
        if let Some(id) = app.peek_wanted() {
            let text = peek_screen(worker, &id);
            app.set_pane_preview(id, text);
        }
    }
}
