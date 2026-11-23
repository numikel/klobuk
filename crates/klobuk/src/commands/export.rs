//! `klobuk export|import` and the auto-export hook.

use std::ffi::OsString;
use std::fs;
use std::io::{self, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

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

/// A temp file this old was left by a run that died; a live export is over in milliseconds.
const STALE_TMP_AFTER: Duration = Duration::from_secs(3600);

/// Makes the names of one process differ; the process id makes them differ between processes.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// `<path>.<pid>.<n>.<ext>` next to `path` (same directory, so a rename onto `path` stays atomic).
/// Every call gives a new name, so parallel exports never touch each other's file.
fn sibling_of(path: &Path, ext: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map_or_else(|| OsString::from(EXPORT_FILE), OsString::from);
    name.push(format!(
        ".{}.{}.{ext}",
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    path.with_file_name(name)
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Whether `file` is a temp name made by [`sibling_of`] for `name`: `<name>.<pid>.<n>.tmp`.
fn is_numbered_tmp(file: &str, name: &str) -> bool {
    file.strip_prefix(name)
        .and_then(|r| r.strip_prefix('.'))
        .and_then(|r| r.strip_suffix(".tmp"))
        .and_then(|r| r.split_once('.'))
        .is_some_and(|(pid, n)| digits(pid) && digits(n))
}

/// Removes the temp files a crashed run left next to `path`: the fixed `<name>.tmp` of older versions
/// and any `<name>.<pid>.<n>.tmp` older than [`STALE_TMP_AFTER`] (a younger one may belong to a live
/// parallel export). Best effort: an error here never fails the export. Symlinks, directories and
/// files with other names are left alone. A `.old` file is never removed: after a failed install it
/// can hold the only copy of the previous export.
fn sweep_leftovers(path: &Path, now: SystemTime) {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return;
    };
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let name = name.to_string_lossy();
    let legacy = format!("{name}.tmp");
    for entry in entries.flatten() {
        let file = entry.file_name().to_string_lossy().into_owned();
        let is_legacy = file == legacy;
        if !is_legacy && !is_numbered_tmp(&file, &name) {
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        let stale = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age >= STALE_TMP_AFTER);
        if meta.is_file() && (is_legacy || stale) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Puts `tmp` in place of `path`. When the plain rename fails (Windows refuses to replace a file
/// another process holds open) the old export is moved aside, not deleted, so a failure never leaves
/// the destination empty: the old file is put back, and when even that fails the error says where it
/// is. `rename` is a parameter so tests can make each step fail.
fn install(tmp: &Path, path: &Path, rename: &dyn Fn(&Path, &Path) -> io::Result<()>) -> Result<(), AppError> {
    let first = match rename(tmp, path) {
        Ok(()) => return Ok(()),
        Err(e) => e,
    };
    refuse_symlink(path)?;
    let old = sibling_of(path, "old");
    if let Err(aside) = rename(path, &old) {
        return Err(AppError::io(format!(
            "{}: {first}; could not move the previous export aside ({aside}), it was not changed",
            path.display()
        )));
    }
    match rename(tmp, path) {
        Ok(()) => {
            let _ = fs::remove_file(&old);
            Ok(())
        }
        Err(second) => Err(AppError::io(match rename(&old, path) {
            Ok(()) => format!(
                "{}: {first}; retry: {second}; the previous export is still in place",
                path.display()
            ),
            Err(restore) => format!(
                "{}: {first}; retry: {second}; the previous export is NOT at {} any more, it is saved as {} (putting it back failed: {restore})",
                path.display(),
                path.display(),
                old.display()
            ),
        })),
    }
}

/// Writes the export atomically (own temp file + rename) and returns the number of records.
/// Every call writes to a temp file of its own, so parallel exports never share one; whichever
/// rename finishes last wins, and each wrote a complete snapshot, so any winner is a valid file.
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
    sweep_leftovers(path, SystemTime::now());
    let tmp = sibling_of(path, "tmp");
    refuse_symlink(&tmp)?;
    // `create_new` also refuses a file or symlink planted under the name
    let mut f = fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
    let wrote = f.write_all(&buf).and_then(|()| f.sync_all());
    drop(f);
    let installed = match wrote {
        Ok(()) => install(&tmp, path, &|from, to| fs::rename(from, to)),
        Err(e) => Err(e.into()),
    };
    if let Err(e) = installed {
        let _ = fs::remove_file(&tmp);
        return Err(e);
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

#[cfg(test)]
mod tests {
    use super::{install, is_numbered_tmp, sibling_of, sweep_leftovers, STALE_TMP_AFTER};
    use std::cell::Cell;
    use std::io;
    use std::path::Path;
    use std::time::{Duration, SystemTime};

    fn denied() -> io::Error {
        io::Error::new(io::ErrorKind::PermissionDenied, "held open")
    }

    #[test]
    fn every_temp_name_is_new_and_stays_in_the_destination_directory() {
        let path = Path::new("repo/.factory/issues.jsonl");
        let names: Vec<_> = (0..50).map(|_| sibling_of(path, "tmp")).collect();
        for (i, a) in names.iter().enumerate() {
            assert_eq!(a.parent(), path.parent());
            assert!(
                is_numbered_tmp(&a.file_name().unwrap().to_string_lossy(), "issues.jsonl"),
                "{a:?}"
            );
            assert!(names[i + 1..].iter().all(|b| b != a), "{a:?} repeated");
        }
        // a bare file name keeps its (empty) directory
        assert_eq!(
            sibling_of(Path::new("out.jsonl"), "old").parent(),
            Some(Path::new(""))
        );
    }

    #[test]
    fn only_numbered_temp_names_of_this_file_match() {
        assert!(is_numbered_tmp("issues.jsonl.12.0.tmp", "issues.jsonl"));
        for other in [
            "issues.jsonl",
            "issues.jsonl.tmp",
            "issues.jsonl.old",
            "issues.jsonl.12.0.old",
            "issues.jsonl..0.tmp",
            "issues.jsonl.x.0.tmp",
            "issues.jsonl.12.tmp",
            "other.jsonl.12.0.tmp",
        ] {
            assert!(!is_numbered_tmp(other, "issues.jsonl"), "{other}");
        }
    }

    #[test]
    fn sweep_removes_the_legacy_name_and_old_numbered_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("issues.jsonl");
        let put = |name: &str| {
            let p = dir.path().join(name);
            std::fs::write(&p, "x").unwrap();
            p
        };
        let legacy = put("issues.jsonl.tmp");
        let old = put("issues.jsonl.1.0.tmp");
        let young = put("issues.jsonl.2.0.tmp");
        let backup = put("issues.jsonl.1.1.old");
        let other = put("notes.1.0.tmp");
        let dest_file = put("issues.jsonl");
        let long_ago = SystemTime::now() - STALE_TMP_AFTER - Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        sweep_leftovers(&dest, SystemTime::now());
        assert!(!legacy.exists(), "legacy fixed name is always a leftover");
        assert!(!old.exists(), "old numbered temp file is a leftover");
        assert!(young.exists(), "a young one may belong to a live export");
        assert!(
            backup.exists(),
            ".old may hold the only copy of the previous export"
        );
        assert!(other.exists() && dest_file.exists());
    }

    /// The plain rename fails, the old export is moved aside, the new one goes in: no `.old` is left.
    #[test]
    fn held_file_released_after_the_first_try_gets_the_new_export() {
        let dir = tempfile::tempdir().unwrap();
        let (tmp, dest) = (dir.path().join("t.tmp"), dir.path().join("issues.jsonl"));
        std::fs::write(&tmp, "new").unwrap();
        std::fs::write(&dest, "old").unwrap();
        let first = Cell::new(true);
        install(&tmp, &dest, &|a, b| {
            if first.replace(false) {
                return Err(denied());
            }
            std::fs::rename(a, b)
        })
        .unwrap();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "new");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "no .old or .tmp left"
        );
    }

    /// The new export cannot be installed: the old one must be back at the destination.
    #[test]
    fn failed_install_puts_the_previous_export_back() {
        let dir = tempfile::tempdir().unwrap();
        let (tmp, dest) = (dir.path().join("t.tmp"), dir.path().join("issues.jsonl"));
        std::fs::write(&tmp, "new").unwrap();
        std::fs::write(&dest, "old").unwrap();
        let err = install(&tmp, &dest, &|a, b| {
            if a == tmp {
                Err(denied())
            } else {
                std::fs::rename(a, b)
            }
        })
        .unwrap_err();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "old");
        assert!(
            err.to_string().contains("previous export is still in place"),
            "{err}"
        );
    }

    /// Even putting the old export back fails: the error must say where the only copy is.
    #[test]
    fn when_the_old_export_cannot_come_back_the_error_names_where_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let (tmp, dest) = (dir.path().join("t.tmp"), dir.path().join("issues.jsonl"));
        std::fs::write(&tmp, "new").unwrap();
        std::fs::write(&dest, "old").unwrap();
        let err = install(&tmp, &dest, &|a, b| {
            if a == tmp || b == dest && a != dest {
                Err(denied())
            } else {
                std::fs::rename(a, b)
            }
        })
        .unwrap_err()
        .to_string();
        assert!(!dest.exists());
        assert!(err.contains("NOT at") && err.contains("saved as"), "{err}");
        let saved = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .find(|e| e.file_name().to_string_lossy().ends_with(".old"))
            .expect("the old export is kept under an .old name");
        assert!(err.contains(&*saved.file_name().to_string_lossy()), "{err}");
        assert_eq!(std::fs::read_to_string(saved.path()).unwrap(), "old");
    }

    /// The old export cannot even be moved aside: nothing changed, and the error says so.
    #[test]
    fn old_export_that_cannot_be_moved_aside_is_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let (tmp, dest) = (dir.path().join("t.tmp"), dir.path().join("issues.jsonl"));
        std::fs::write(&tmp, "new").unwrap();
        std::fs::write(&dest, "old").unwrap();
        let err = install(&tmp, &dest, &|_, _| Err(denied())).unwrap_err();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "old");
        assert!(err.to_string().contains("was not changed"), "{err}");
    }
}
