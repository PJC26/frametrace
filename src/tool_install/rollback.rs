//! Fix round 13: what an incomplete rollback leaves behind, kept where no
//! sweep removes it and described so a person can finish the job.
//!
//! * `tools/rollback-backup-<hex>/` — an archive tool's previous files
//!   (`0.bak`, `1.bak`, … as `move_all_or_nothing` named them) and
//!   `RESTORE.txt`, one line per file: `<original path> <- <name here>`.
//! * `tools/.staging-<hex>/backup/RESTORE.txt` — the same, when the backup
//!   could not even be moved out of the staging dir (the sweep skips it).
//! * `tools/python/rollback-backup-<hex>/<tool>/` — a pip tool's previous
//!   venv, whole, with `RESTORE.txt` next to it.
//!
//! The first line of `RESTORE.txt` is `tool: <name>`; [`rollback_backups`]
//! lists them for `/api/tools`, `frametrace tools list` and `doctor`.

use super::*;

/// The restore instructions next to the kept previous files.
pub const RESTORE_FILE: &str = "RESTORE.txt";

/// Name prefix of the kept-backup directories (never swept).
pub(crate) const ROLLBACK_BACKUP_PREFIX: &str = "rollback-backup-";

const TOOL_LINE: &str = "tool: ";

/// `RESTORE.txt`: an optional `tool:` line, how to restore, then one line
/// per file `<original> <- <kept copy, relative to folder>` (or a NEW file
/// with no previous copy, to delete).
pub(crate) fn restore_text(
    tool: Option<&str>,
    entries: &[(PathBuf, Option<PathBuf>)],
    folder: &Path,
) -> String {
    let mut text = String::new();
    if let Some(tool) = tool {
        text.push_str(&format!("{TOOL_LINE}{tool}\n"));
    }
    text.push_str(
        "FrameTrace could not finish undoing a failed install; the previous copies are kept in this folder.\n\
         To restore by hand: close FrameTrace (and anything running the tool); for each line below, \
         move the item named after \"<-\" from this folder to the path before it, replacing what is there \
         (a line marked NEW has no previous copy: delete that path); then delete this folder.\n",
    );
    for (original, kept) in entries {
        match kept {
            Some(kept) => {
                let shown = kept.strip_prefix(folder).unwrap_or(kept);
                text.push_str(&format!("{} <- {}\n", original.display(), shown.display()));
            }
            None => text.push_str(&format!(
                "{} <- (none: NEW file with no previous copy — delete it)\n",
                original.display()
            )),
        }
    }
    text
}

/// Puts `tool: <name>` first in `RESTORE.txt` (written by
/// `move_all_or_nothing`, which does not know the tool). Fix round 14 L4:
/// when that best-effort write left no file, it is created here (the
/// `tool:` line and how to restore; the file list is then only in the
/// error, which the install audit log keeps). `Err` when it still cannot
/// be written — the caller says so instead of pointing at the file.
pub(crate) fn name_tool_in_restore_file(path: &Path, tool: &str) -> Result<(), String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) if text.starts_with(TOOL_LINE) => return Ok(()),
        Ok(text) => format!("{TOOL_LINE}{tool}\n{text}"),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let folder = path.parent().unwrap_or(path);
            format!(
                "{}{MISSING_LIST_NOTE}",
                restore_text(Some(tool), &[], folder)
            )
        }
        Err(err) => return Err(err.to_string()),
    };
    std::fs::write(path, text).map_err(|err| err.to_string())
}

/// The last line of a `RESTORE.txt` created without its file list.
pub(crate) const MISSING_LIST_NOTE: &str = "(The list of files could not be written here: the install error — also in tools/install-audit.jsonl — names each file and the name of its previous copy in this folder.)\n";

/// The note added to an incomplete-rollback error when `RESTORE.txt`
/// could not be written (Fix round 14 L4).
pub(crate) fn restore_unwritten_note(err: &str) -> String {
    format!("({RESTORE_FILE} could not be written: {err})")
}

fn restore_file_tool(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let tool = text.lines().next()?.strip_prefix(TOOL_LINE)?.trim();
    (!tool.is_empty()).then(|| tool.to_string())
}

/// `std::fs::write` of a `RESTORE.txt`; a test can make it fail
/// (Fix round 14 L4).
fn write_restore_file(path: &Path, text: &str) -> std::io::Result<()> {
    #[cfg(test)]
    if FAIL_RESTORE_WRITE.with(|fail| fail.get()) {
        return Err(std::io::Error::other("test: RESTORE.txt write fails"));
    }
    std::fs::write(path, text)
}

#[cfg(test)]
thread_local! {
    pub(crate) static FAIL_RESTORE_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// One kept backup of an incomplete rollback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackBackup {
    /// The directory holding the previous copies.
    pub path: PathBuf,
    /// Its restore instructions (may be missing: a round-12 leftover).
    pub restore_file: PathBuf,
    /// The tool it belongs to, when known.
    pub tool: Option<String>,
    pub size_bytes: u64,
}

fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_symlink() => 0,
            Ok(kind) if kind.is_dir() => dir_size(&entry.path()),
            Ok(_) => entry.metadata().map(|meta| meta.len()).unwrap_or(0),
            Err(_) => 0,
        })
        .sum()
}

fn real_dirs(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| {
            entry
                .file_type()
                .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
        })
        .map(|entry| {
            (
                entry.file_name().to_string_lossy().into_owned(),
                entry.path(),
            )
        })
        .collect()
}

/// Every kept backup under `tools_dir` (see the module doc).
pub fn rollback_backups(tools_dir: &Path) -> Vec<RollbackBackup> {
    // `tools/` first, then `tools/python/`, each sorted by path.
    let mut top = Vec::new();
    let mut python = Vec::new();
    for (name, path) in real_dirs(tools_dir) {
        let restore_file = if name.starts_with(ROLLBACK_BACKUP_PREFIX) {
            path.join(RESTORE_FILE)
        } else if name.starts_with(".staging-")
            && std::fs::symlink_metadata(path.join("backup").join(RESTORE_FILE)).is_ok()
        {
            path.join("backup").join(RESTORE_FILE)
        } else {
            continue;
        };
        top.push(RollbackBackup {
            tool: restore_file_tool(&restore_file),
            size_bytes: dir_size(&path),
            path,
            restore_file,
        });
    }
    for (name, path) in real_dirs(&tools_dir.join("python")) {
        if !name.starts_with(ROLLBACK_BACKUP_PREFIX) {
            continue;
        }
        let restore_file = path.join(RESTORE_FILE);
        let tool = restore_file_tool(&restore_file)
            .or_else(|| real_dirs(&path).into_iter().next().map(|(name, _)| name));
        python.push(RollbackBackup {
            tool,
            size_bytes: dir_size(&path),
            path,
            restore_file,
        });
    }
    top.sort_by(|a, b| a.path.cmp(&b.path));
    python.sort_by(|a, b| a.path.cmp(&b.path));
    top.extend(python);
    top
}

/// [`rollback_backups`] of this executable's `tools` dir (none when it
/// cannot be found).
pub fn rollback_backups_for_current_exe() -> Vec<RollbackBackup> {
    InstallDirs::for_current_exe()
        .map(|dirs| rollback_backups(&dirs.tools_dir))
        .unwrap_or_default()
}

/// The warning shown for a kept backup (UI, `tools list`, `doctor`).
pub fn rollback_hint(backup: &RollbackBackup) -> String {
    let size = backup.size_bytes as f64 / (1024.0 * 1024.0);
    format!(
        "이전 설치 되돌리기가 끝나지 않아 이전 파일이 {} ({size:.1} MB)에 남아 있습니다 — 도구 파일이 새것·옛것으로 섞였을 수 있습니다. {}의 안내대로 직접 복원하거나 도구를 다시 설치한 뒤 이 폴더를 지우십시오",
        backup.path.display(),
        backup.restore_file.display()
    )
}

/// Fix round 13 M: undoes a failed pip venv rebuild — removes the new,
/// partial `target` and renames `backup` (the previous venv) back, both
/// with the bounded retry. When that cannot be finished the previous
/// venv is moved to `python_root/rollback-backup-<hex>/<tool>` (no sweep
/// removes it) with `RESTORE.txt`, and the returned error starts with
/// [`ROLLBACK_INCOMPLETE_PREFIX`]. When even that move fails the backup
/// stays as `.<tool>.old-<hex>`, which the sweep keeps while `target` is
/// not a working venv.
pub(crate) fn undo_venv(
    err: String,
    tool: &str,
    target: &Path,
    backup: Option<&Path>,
    python_root: &Path,
) -> String {
    let removed = remove_dir_all_with_retry(target);
    let Some(backup) = backup else {
        return match removed {
            Ok(()) => err,
            Err(remove_err) => format!(
                "{err} (the partial venv {} could not be removed: {remove_err}; delete it by hand)",
                target.display()
            ),
        };
    };
    let problem = match removed {
        Err(remove_err) => format!(
            "{} is the NEW, partial venv (cannot remove it: {remove_err})",
            target.display()
        ),
        Ok(()) => match rename_with_retry(backup, target) {
            Ok(()) => return err,
            Err(restore_err) => format!(
                "{} is missing (cannot restore it: {restore_err})",
                target.display()
            ),
        },
    };
    let kept_dir = python_root.join(format!("{ROLLBACK_BACKUP_PREFIX}{}", random_hex()));
    let kept = kept_dir.join(tool);
    let moved = std::fs::create_dir(&kept_dir).and_then(|()| rename_with_retry(backup, &kept));
    let where_kept = match moved {
        Ok(()) => {
            let restore = kept_dir.join(RESTORE_FILE);
            let written = write_restore_file(
                &restore,
                &restore_text(
                    Some(tool),
                    &[(target.to_path_buf(), Some(kept.clone()))],
                    &kept_dir,
                ),
            );
            // Fix round 14 L4: never point at a file that is not there.
            let how = match written {
                Ok(()) => format!("{} says how", restore.display()),
                Err(err) => format!(
                    "move it back to {} {}",
                    target.display(),
                    restore_unwritten_note(&err.to_string())
                ),
            };
            format!(
                "the previous venv is kept at {} (restore it by hand — {how})",
                kept.display(),
            )
        }
        Err(move_err) => {
            let _ = std::fs::remove_dir(&kept_dir);
            format!(
                "the previous venv is kept at {} — could not move it to {}: {move_err}; it is not removed while {} is not a working venv; restore it by hand before installing again",
                backup.display(),
                kept.display(),
                target.display()
            )
        }
    };
    format!(
        "{ROLLBACK_INCOMPLETE_PREFIX}{err}; the rollback could NOT restore the previous venv — {problem}; {where_kept}"
    )
}
