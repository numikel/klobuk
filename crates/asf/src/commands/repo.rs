//! `asf repo init|status`.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;

use asf_core::{is_valid_prefix, Config, IssueFilter, IssueStore, Status};
use asf_store::SqliteStore;
use serde::Serialize;

use crate::cli::{Cli, InitArgs, RepoCmd};
use crate::ctx::{self, Ctx, CONFIG_FILE, DB_FILE, FACTORY_DIR};
use crate::git;
use crate::output::{AppError, Rendered};

/// Lines `asf repo init` guarantees in `.gitignore` and `.git/info/exclude`.
pub const IGNORE_LINES: &[&str] = &[
    ".factory/factory.db",
    ".factory/factory.db-wal",
    ".factory/factory.db-shm",
    ".factory/runs/",
    ".worktrees/",
    "CLAUDE.local.md",
    ".claude/settings.local.json",
];

pub fn run(cli: &Cli, cmd: &RepoCmd) -> Result<Rendered, AppError> {
    match cmd {
        RepoCmd::Init(args) => init(cli, args),
        RepoCmd::Status => status(&Ctx::load(cli)?),
    }
}

#[derive(Serialize)]
struct InitResult {
    root: String,
    prefix: String,
    db: String,
    created: bool,
}

fn init(cli: &Cli, args: &InitArgs) -> Result<Rendered, AppError> {
    let prefix = args.prefix.as_deref().unwrap_or("asf");
    if !is_valid_prefix(prefix) {
        return Err(AppError::validation(format!(
            "prefix `{prefix}` must match ^[a-z][a-z0-9]{{0,15}}$"
        )));
    }
    let root = git::locate_root(&ctx::start_dir(cli)?)?;
    if root.to_string_lossy().contains(' ') {
        return Err(AppError::validation(format!(
            "repository path `{}` contains a space; worker arguments are passed unquoted",
            root.display()
        )));
    }
    let factory_dir = root.join(FACTORY_DIR);
    let config_path = factory_dir.join(CONFIG_FILE);
    if config_path.exists() {
        return Err(AppError::conflict(format!(
            "{} already exists",
            config_path.display()
        )));
    }
    fs::create_dir_all(&factory_dir)?;
    write_utf8(&config_path, &Config::default_toml_with_prefix(prefix))?;
    let db_path = factory_dir.join(DB_FILE);
    let store = SqliteStore::open(&db_path)?;
    store.init_repo(&root.to_string_lossy().replace('\\', "/"), prefix, "main")?;
    ensure_lines(&root.join(".gitignore"), IGNORE_LINES)?;
    ensure_lines(
        &git::common_dir(&root)?.join("info").join("exclude"),
        IGNORE_LINES,
    )?;
    let res = InitResult {
        root: root.to_string_lossy().replace('\\', "/"),
        prefix: prefix.to_string(),
        db: db_path.to_string_lossy().replace('\\', "/"),
        created: true,
    };
    let text = format!("initialised {} (prefix {})", res.root, res.prefix);
    Rendered::new(&res, text)
}

fn write_utf8(path: &Path, content: &str) -> Result<(), AppError> {
    let mut f = fs::File::create(path)?;
    f.write_all(content.replace("\r\n", "\n").as_bytes())?;
    Ok(())
}

/// Appends each missing line (exact match) to `path`, creating it if needed. Keeps existing content.
fn ensure_lines(path: &Path, lines: &[&str]) -> Result<(), AppError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let existing = fs::read_to_string(path).unwrap_or_default();
    let have: std::collections::HashSet<&str> = existing.lines().map(str::trim).collect();
    let missing: Vec<&str> = lines.iter().copied().filter(|l| !have.contains(l)).collect();
    if missing.is_empty() {
        return Ok(());
    }
    let mut out = existing.clone();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.is_empty() {
        out.push_str("# asf\n");
    }
    for l in missing {
        out.push_str(l);
        out.push('\n');
    }
    write_utf8(path, &out)
}

#[derive(Serialize)]
struct StatusResult {
    root: String,
    prefix: String,
    next_seq: i64,
    counts: BTreeMap<String, usize>,
    claims: usize,
    lane: String,
}

fn status(ctx: &Ctx) -> Result<Rendered, AppError> {
    let repo = ctx.store.repo()?;
    let issues = ctx.store.list_issues(&IssueFilter::default())?;
    let mut counts: BTreeMap<String, usize> = Status::ALL.iter().map(|s| (s.to_string(), 0)).collect();
    for i in &issues {
        *counts.entry(i.status.to_string()).or_default() += 1;
    }
    let claims = ctx.store.active_claims()?.len();
    let res = StatusResult {
        root: ctx.root.to_string_lossy().replace('\\', "/"),
        prefix: repo.id_prefix.clone(),
        next_seq: repo.next_seq,
        counts,
        claims,
        lane: ctx.lane.clone(),
    };
    let mut text = format!(
        "root: {}\nprefix: {}\nnext_seq: {}\n",
        res.root, res.prefix, res.next_seq
    );
    text.push_str("counts:");
    for s in Status::ALL {
        text.push_str(&format!(" {}={}", s, res.counts[&s.to_string()]));
    }
    text.push_str(&format!("\nclaims: {}\nlane: {}", res.claims, res.lane));
    Rendered::new(&res, text)
}
