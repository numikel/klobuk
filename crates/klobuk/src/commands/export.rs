//! `klobuk export|import` and the auto-export hook.

use std::fs;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};

use klobuk_core::{ExportOpts, ImportMode, ImportReport, IssueStore};
use serde::Serialize;

use crate::cli::{Cli, ExportArgs, ImportArgs};
use crate::ctx::{Ctx, FACTORY_DIR};
use crate::output::{AppError, Rendered};

pub const EXPORT_FILE: &str = "issues.jsonl";

#[derive(Serialize)]
struct ExportResult {
    path: String,
    records: usize,
}

pub fn default_path(ctx: &Ctx) -> PathBuf {
    ctx.root.join(FACTORY_DIR).join(EXPORT_FILE)
}

/// Refuses to touch a path that is a symlink (a cloned repo could plant one to clobber a user file).
fn refuse_symlink(p: &Path) -> Result<(), AppError> {
    if let Ok(meta) = fs::symlink_metadata(p) {
        if meta.file_type().is_symlink() {
            return Err(AppError::validation(format!(
                "{} is a symlink; refusing to write through it",
                p.display()
            )));
        }
    }
    Ok(())
}

/// Writes the export atomically (exclusive temp file + rename) and returns the number of records.
/// Neither the destination nor the temp file may be a symlink; the fallback never writes through
/// the destination path directly.
pub fn write_export(ctx: &Ctx, path: &Path) -> Result<usize, AppError> {
    let mut buf: Vec<u8> = Vec::new();
    ctx.store.export_jsonl(
        &mut buf,
        &ExportOpts {
            include_comments: ctx.config.store.export_comments,
        },
    )?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    refuse_symlink(path)?;
    let tmp = path.with_extension("jsonl.tmp");
    refuse_symlink(&tmp)?;
    if tmp.exists() {
        // leftover regular file from a crashed run
        fs::remove_file(&tmp)?;
    }
    {
        let mut f = fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(&buf)?;
        f.sync_all()?;
    }
    if let Err(first) = fs::rename(&tmp, path) {
        // Windows refuses to replace a file another process holds open: remove the target and retry once.
        refuse_symlink(path)?;
        let _ = fs::remove_file(path);
        if let Err(second) = fs::rename(&tmp, path) {
            let _ = fs::remove_file(&tmp);
            return Err(AppError::io(format!(
                "{}: {first}; retry: {second}",
                path.display()
            )));
        }
    }
    Ok(buf.iter().filter(|b| **b == b'\n').count())
}

/// Called after every mutating command when `[store] auto_export = true`.
pub fn auto(ctx: &Ctx) -> Result<(), AppError> {
    if ctx.config.store.auto_export {
        write_export(ctx, &default_path(ctx))?;
    }
    Ok(())
}

pub fn export(cli: &Cli, a: &ExportArgs) -> Result<Rendered, AppError> {
    let ctx = Ctx::load(cli)?;
    let path = a.out.clone().unwrap_or_else(|| default_path(&ctx));
    let records = write_export(&ctx, &path)?;
    let res = ExportResult {
        path: path.to_string_lossy().replace('\\', "/"),
        records,
    };
    let text = format!("exported {} records to {}", res.records, res.path);
    Rendered::new(&res, text)
}

pub fn import(cli: &Cli, a: &ImportArgs) -> Result<Rendered, AppError> {
    let ctx = Ctx::load(cli)?;
    let file = fs::File::open(&a.file).map_err(|e| AppError::io(format!("{}: {e}", a.file.display())))?;
    let mut reader = BufReader::new(file);
    let report: ImportReport = ctx.store.import_jsonl(&mut reader, a.mode)?;
    auto(&ctx)?;
    let text = format!(
        "imported: {} created, {} updated, {} skipped, {} deps, {} comments ({})",
        report.issues_created,
        report.issues_updated,
        report.skipped,
        report.deps,
        report.comments,
        match a.mode {
            ImportMode::Upsert => "upsert",
            ImportMode::Replace => "replace",
        }
    );
    Rendered::new(&report, text)
}
