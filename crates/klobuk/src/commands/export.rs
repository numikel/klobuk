//! `klobuk export|import` and the auto-export hook.

use std::borrow::Cow;
use std::ffi::OsString;
use std::fs;
use std::io::{self, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use klobuk_core::{
    redact_home_paths, scan_findings, ExportOpts, ImportMode, ImportReport, IssueStore, Record,
};
use serde::Serialize;

use crate::cli::{Cli, ExportArgs, ImportArgs};
use crate::ctx::{Ctx, FACTORY_DIR};
use crate::output::{AppError, ErrorCode, Rendered};

pub const EXPORT_FILE: &str = "issues.jsonl";

/// One e-mail address or secret-like string in the export. Names the record and the kind, never the value.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ExportWarning {
    pub issue: String,
    /// `n` of the comment in the export; `None` for a field of the issue itself.
    pub comment: Option<i64>,
    pub field: String,
    pub kind: &'static str,
}

impl ExportWarning {
    fn line(&self) -> String {
        let at = self
            .comment
            .map_or_else(|| self.issue.clone(), |n| format!("{} comment {n}", self.issue));
        format!("warning: {at} {}: {}", self.field, self.kind)
    }
}

#[derive(Serialize)]
struct ExportResult {
    path: String,
    records: usize,
    warnings: Vec<ExportWarning>,
}

/// The home directories of the current user, as the environment spells them. They go to the pure
/// redaction in `klobuk-core`, which reads no environment itself.
fn home_dirs() -> Vec<String> {
    let mut homes: Vec<String> = ["USERPROFILE", "HOME"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|h| h.to_string_lossy().into_owned())
        .filter(|h| !h.trim().is_empty())
        .collect();
    homes.dedup();
    homes
}

/// Rewrites home paths in `s` in place; with `warnings`, also records what the (rewritten) text holds.
fn scrub(
    s: &mut String,
    homes: &[&str],
    at: (&str, Option<i64>, &str),
    warnings: Option<&mut Vec<ExportWarning>>,
) {
    let rewritten = match redact_home_paths(s, homes) {
        Cow::Owned(n) => Some(n),
        Cow::Borrowed(_) => None,
    };
    if let Some(n) = rewritten {
        *s = n;
    }
    if let Some(out) = warnings {
        out.extend(scan_findings(s).into_iter().map(|k| ExportWarning {
            issue: at.0.to_string(),
            comment: at.1,
            field: at.2.to_string(),
            kind: k.label(),
        }));
    }
}

/// Redacts the text fields of a raw export (title, body, acceptance text and check, comment body).
/// Every line is parsed and written back with the same serializer the store used, so the bytes of a
/// line without a home path do not change. With `scan`, e-mail addresses and secret-like strings are
/// listed too. The store is not touched.
fn redact_export(raw: &[u8], homes: &[&str], scan: bool) -> Result<(Vec<u8>, Vec<ExportWarning>), AppError> {
    let text = std::str::from_utf8(raw)
        .map_err(|e| AppError::new(ErrorCode::Backend, format!("export is not UTF-8: {e}")))?;
    let mut out = Vec::with_capacity(raw.len());
    let mut found: Vec<ExportWarning> = Vec::new();
    for line in text.lines() {
        let mut rec: Record = serde_json::from_str(line)
            .map_err(|e| AppError::new(ErrorCode::Backend, format!("export line: {e}")))?;
        match &mut rec {
            Record::Issue(i) => {
                let id = i.id.to_string();
                scrub(
                    &mut i.title,
                    homes,
                    (&id, None, "title"),
                    scan.then_some(&mut found),
                );
                scrub(
                    &mut i.body,
                    homes,
                    (&id, None, "body"),
                    scan.then_some(&mut found),
                );
                for (k, a) in i.acceptance.iter_mut().enumerate() {
                    scrub(
                        &mut a.text,
                        homes,
                        (&id, None, &format!("acceptance[{k}].text")),
                        scan.then_some(&mut found),
                    );
                    if let Some(v) = a.verify.as_mut() {
                        scrub(
                            v,
                            homes,
                            (&id, None, &format!("acceptance[{k}].verify")),
                            scan.then_some(&mut found),
                        );
                    }
                }
            }
            Record::Comment(c) => {
                let id = c.issue.to_string();
                scrub(
                    &mut c.body,
                    homes,
                    (&id, Some(c.n), "body"),
                    scan.then_some(&mut found),
                );
            }
            Record::Meta { .. } | Record::Dep { .. } => {}
        }
        let line = serde_json::to_string(&rec)
            .map_err(|e| AppError::new(ErrorCode::Backend, format!("export line: {e}")))?;
        out.extend_from_slice(line.as_bytes());
        out.push(b'\n');
    }
    Ok((out, found))
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

/// How many temp names are tried before giving up on leftovers.
const TMP_TRIES: u32 = 32;

/// Creates a new temp file under the next name `next_name` gives. A name that is already taken is
/// skipped: no live process owns a file with this process's pid and a counter it has not handed out
/// yet, so it is a leftover of a run that died (and had the same pid, which is recycled). A symlink
/// under a candidate name is refused, never written through. Errors name the file.
fn open_new_tmp(next_name: &mut dyn FnMut() -> PathBuf) -> Result<(fs::File, PathBuf), AppError> {
    let mut last = PathBuf::new();
    for _ in 0..TMP_TRIES {
        let tmp = next_name();
        refuse_symlink(&tmp)?;
        match fs::OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(f) => return Ok((f, tmp)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last = tmp,
            Err(e) => return Err(AppError::io(format!("{}: {e}", tmp.display()))),
        }
    }
    Err(AppError::io(format!(
        "{}: temp file already exists; no free name after {TMP_TRIES} tries, remove the leftover *.tmp files next to the destination",
        last.display()
    )))
}

/// Writes the export atomically (own temp file + rename) and returns the number of records and, with
/// `scan`, the e-mail and secret-like findings. Home paths are rewritten to `~` in the file, never in
/// the store. Every call writes to a temp file of its own, so parallel exports never share one;
/// whichever rename finishes last wins, and each wrote a complete snapshot, so any winner is a valid
/// file. Neither the destination nor the temp file may be a symlink; the fallback never writes through
/// the destination path directly.
pub fn write_export(ctx: &Ctx, path: &Path, scan: bool) -> Result<(usize, Vec<ExportWarning>), AppError> {
    let mut raw: Vec<u8> = Vec::new();
    ctx.store.export_jsonl(
        &mut raw,
        &ExportOpts {
            include_comments: ctx.config.store.export_comments,
        },
    )?;
    let homes = home_dirs();
    let homes: Vec<&str> = homes.iter().map(String::as_str).collect();
    let (buf, warnings) = redact_export(&raw, &homes, scan)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    refuse_symlink(path)?;
    sweep_leftovers(path, SystemTime::now());
    let (mut f, tmp) = open_new_tmp(&mut || sibling_of(path, "tmp"))?;
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
    Ok((buf.iter().filter(|b| **b == b'\n').count(), warnings))
}

/// Called after every mutating command when `[store] auto_export = true`. Redacts like an explicit
/// export but never scans: a finding must not fail or slow a command that only wrote to the store.
pub fn auto(ctx: &Ctx) -> Result<(), AppError> {
    if ctx.config.store.auto_export {
        write_export(ctx, &default_path(ctx), false)?;
    }
    Ok(())
}

pub fn export(cli: &Cli, a: &ExportArgs) -> Result<Rendered, AppError> {
    let ctx = Ctx::load(cli)?;
    let path = a.out.clone().unwrap_or_else(|| default_path(&ctx));
    let (records, warnings) = write_export(&ctx, &path, true)?;
    let res = ExportResult {
        path: path.to_string_lossy().replace('\\', "/"),
        records,
        warnings,
    };
    let mut text = format!("exported {} records to {}", res.records, res.path);
    if !res.warnings.is_empty() {
        for w in &res.warnings {
            text.push('\n');
            text.push_str(&w.line());
        }
        text.push_str(&format!(
            "\n{} warning(s); the values are not printed. Review the export before committing it",
            res.warnings.len()
        ));
    }
    // The envelope has no room for a warning next to a failure, so --strict carries them in the message.
    if a.strict && !res.warnings.is_empty() {
        return Err(AppError::validation(format!(
            "{text}\n--strict: failing because of the warnings above"
        )));
    }
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
    use super::{install, is_numbered_tmp, open_new_tmp, sibling_of, sweep_leftovers, STALE_TMP_AFTER};
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

    /// A run killed between creating and renaming its temp file leaves `<file>.<pid>.0.tmp`; a later
    /// process that gets the same pid must step over it, not fail after its store write.
    #[test]
    fn a_young_leftover_under_the_next_name_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let leftover = dir.path().join("issues.jsonl.7.0.tmp");
        std::fs::write(&leftover, "crashed").unwrap();
        let fresh = dir.path().join("issues.jsonl.7.1.tmp");
        let mut names = vec![leftover.clone(), fresh.clone()].into_iter();
        let (file, tmp) = open_new_tmp(&mut || names.next().unwrap()).unwrap();
        drop(file);
        assert_eq!(tmp, fresh);
        assert_eq!(
            std::fs::read_to_string(&leftover).unwrap(),
            "crashed",
            "the leftover is not touched"
        );
    }

    #[test]
    fn leftovers_under_every_name_end_in_an_error_that_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let leftover = dir.path().join("issues.jsonl.7.0.tmp");
        std::fs::write(&leftover, "crashed").unwrap();
        let tries = std::cell::Cell::new(0);
        let err = open_new_tmp(&mut || {
            tries.set(tries.get() + 1);
            leftover.clone()
        })
        .map(|_| ())
        .unwrap_err()
        .to_string();
        assert!(
            tries.get() > 1 && tries.get() < 100,
            "bounded retries, got {}",
            tries.get()
        );
        assert!(err.contains("issues.jsonl.7.0.tmp"), "{err}");
    }

    #[test]
    fn a_symlink_under_a_candidate_name_is_refused_not_written_through() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim.txt");
        std::fs::write(&victim, "precious").unwrap();
        let link = dir.path().join("issues.jsonl.7.0.tmp");
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(&victim, &link).is_ok();
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&victim, &link).is_ok();
        if !made {
            eprintln!("skipping: cannot create symlinks on this machine");
            return;
        }
        let err = open_new_tmp(&mut || link.clone()).map(|_| ()).unwrap_err();
        assert!(err.to_string().contains("symlink"), "{err}");
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
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
