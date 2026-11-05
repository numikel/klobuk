//! Terminal UI (M3): a ratatui app meant to run as a pane in Herdr.
//!
//! Synchronous loop: keys are polled every 200 ms, the store snapshot is refreshed every second and
//! the agents' live state every two seconds. Actions go through the same `Worker` and merge-queue
//! code the CLI uses, so the TUI never has behaviour of its own.

pub mod app;
pub mod view;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use asf_core::*;
use ratatui::crossterm::event::{self, Event as TermEvent, KeyEventKind};

use crate::commands::merge::tick_all;
use crate::ctx::{Ctx, CONFIG_FILE, FACTORY_DIR};
use crate::output::AppError;
use crate::worker::Worker;
use app::{config_rows, unanswered_questions, Action, App, Snapshot, View};

const KEY_POLL: Duration = Duration::from_millis(200);
const STORE_REFRESH: Duration = Duration::from_secs(1);
const AGENT_REFRESH: Duration = Duration::from_secs(2);
const LOG_LIMIT: usize = 200;

/// `.factory/config.toml` text with the BOM dropped and CRLF turned into LF (as `asf config` reads it).
fn read_config_text(root: &Path) -> std::io::Result<String> {
    let raw = std::fs::read_to_string(root.join(FACTORY_DIR).join(CONFIG_FILE))?;
    Ok(raw.trim_start_matches('\u{feff}').replace("\r\n", "\n"))
}

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
    format!(
        "{key}: {} -> {} (restart `asf serve` to apply)",
        shown(&text),
        shown(&edited)
    )
}

/// Loads everything the TUI shows. `rt` adds live agent states (skipped when `None`).
pub fn load_snapshot(
    ctx: &Ctx,
    rt: Option<&dyn AgentRuntime>,
    prev_agents: &BTreeMap<IssueId, AgentInfo>,
) -> Result<Snapshot, AppError> {
    let store = &ctx.store;
    let runs = store.runs(None, true)?;
    let mut agents = prev_agents.clone();
    let mut runtime_ok = None;
    if let Some(rt) = rt {
        runtime_ok = Some(rt.health().is_ok());
        for r in &runs {
            if let (Some(pane), Some(name)) = (&r.pane_id, &r.agent_name) {
                let h = AgentHandle {
                    name: name.clone(),
                    pane_id: pane.clone(),
                    workspace_id: r.workspace_id.clone().unwrap_or_default(),
                };
                if let Ok(info) = rt.info(&h) {
                    agents.insert(r.issue_id.clone(), info);
                }
            }
        }
    }
    let mut events = store.events_since(0, usize::MAX)?;
    if events.len() > LOG_LIMIT {
        events = events.split_off(events.len() - LOG_LIMIT);
    }
    events.reverse();
    let issues = store.list_issues(&IssueFilter::default())?;
    // Unanswered questions are read from all comments, not the inbox: marking the inbox read must
    // not hide a question that still blocks `asf issue finish`.
    let mut questions = Vec::new();
    for issue in issues
        .iter()
        .filter(|i| !matches!(i.status, Status::Done | Status::Cancelled))
    {
        let comments = store.comments(&issue.id)?;
        let open = unanswered_questions(&comments);
        questions.extend(comments.into_iter().filter(|c| open.contains(&c.id)));
    }
    questions.sort_by(|a, b| (&a.created_at, a.id).cmp(&(&b.created_at, b.id)));
    Ok(Snapshot {
        repo_name: ctx
            .root
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default(),
        branch: ctx.config.repo.default_branch.clone(),
        runtime_ok,
        max_parallel: ctx.config.scheduler.max_parallel,
        issues,
        claims: store.active_claims()?,
        runs,
        agents,
        inbox: store.inbox(LANE_HUMAN, true, false)?,
        questions,
        queue: store
            .merge_queue()?
            .into_iter()
            .filter(|m| !m.state.is_terminal())
            .collect(),
        events,
        config: config_rows(&read_config_text(&ctx.root).unwrap_or_default()),
    })
}

/// Executes one action; returns the status line to show.
fn execute(action: Action, ctx: &Ctx, worker: &Worker<'_>) -> Result<Option<String>, AppError> {
    Ok(match action {
        // Peek is served by the event loop (it needs the app state); nothing to run here.
        Action::None | Action::Quit | Action::Refresh | Action::Peek(_) => None,
        Action::Tick => {
            let w = worker.watch()?;
            let m = tick_all(ctx, false)?;
            let p = ctx.store.promote_ready()?;
            let closed = ctx.store.close_completed_epics()?;
            Some(format!(
                "tick: {} transition(s), {} merge item(s), {} promoted, {} epic(s) closed",
                w.transitions.len(),
                m.items.len(),
                p.len(),
                closed.len()
            ))
        }
        Action::Start(id) => {
            let r = worker.start(&id, None)?;
            Some(format!(
                "started {} in {} (pane {})",
                id, r.workspace_id, r.pane_id
            ))
        }
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
        Action::SetConfig(key, value) => Some(config_set_status(&ctx.root, &key, &value)),
        Action::UnsetConfig(key) => Some(config_unset_status(&ctx.root, &key)),
        Action::MarkRead(_) => {
            let n = ctx.store.inbox(LANE_HUMAN, false, false)?.len();
            Some(format!("inbox: {n} item(s) marked read"))
        }
    })
}

/// One frame as text (for `asf tui --once` and debugging).
pub fn run_once(ctx: &Ctx, view: Option<View>) -> Result<String, AppError> {
    let rt = asf_runtime::from_config(&ctx.config.worker.runtime, &ctx.root).ok();
    let snap = load_snapshot(ctx, rt.as_deref(), &BTreeMap::new())?;
    let mut app = App::new(snap);
    if let Some(v) = view {
        app.view = v;
    }
    Ok(view::render_to_text(&app, 120, 30))
}

pub fn run_interactive(ctx: &Ctx, view: Option<View>) -> Result<(), AppError> {
    let worker = Worker::new(ctx)?;
    let mut app = App::new(load_snapshot(ctx, Some(worker.rt.as_ref()), &BTreeMap::new())?);
    if let Some(v) = view {
        app.view = v;
    }
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app, ctx, &worker);
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    ctx: &Ctx,
    worker: &Worker<'_>,
) -> Result<(), AppError> {
    let mut last_store = Instant::now();
    let mut last_agents = Instant::now() - AGENT_REFRESH;
    loop {
        terminal
            .draw(|f| view::render(f, app))
            .map_err(|e| AppError::io(e.to_string()))?;
        if event::poll(KEY_POLL).map_err(|e| AppError::io(e.to_string()))? {
            if let TermEvent::Key(key) = event::read().map_err(|e| AppError::io(e.to_string()))? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                let action = app.handle_key(key);
                if action == Action::Quit {
                    return Ok(());
                }
                if let Action::Peek(id) = &action {
                    app.set_screen(peek_screen(worker, id));
                }
                let refresh = action != Action::None;
                match execute(action, ctx, worker) {
                    Ok(Some(msg)) => app.status = Some(msg),
                    Ok(None) => {}
                    Err(e) => app.status = Some(format!("error: {}", e.message)),
                }
                if refresh {
                    last_store -= STORE_REFRESH;
                    last_agents -= AGENT_REFRESH;
                }
            }
        }
        if last_store.elapsed() >= STORE_REFRESH {
            let with_agents = last_agents.elapsed() >= AGENT_REFRESH;
            let rt = if with_agents {
                Some(worker.rt.as_ref())
            } else {
                None
            };
            match load_snapshot(ctx, rt, &app.snap.agents) {
                Ok(snap) => app.replace(snap),
                Err(e) => app.status = Some(format!("refresh failed: {}", e.message)),
            }
            last_store = Instant::now();
            if with_agents {
                last_agents = Instant::now();
            }
        }
    }
}
