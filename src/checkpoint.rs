//! Crash-resume checkpoints for long-running commands (roadmap R-1).
//!
//! A checkpoint is a small JSONL state file under `db/`: one header line
//! carrying `schema_version`, the command kind, a `run_id`, and an input
//! `fingerprint`, followed by one record per completed unit of work. An
//! interrupted run leaves the file behind; the next run reuses it when the
//! fingerprint still matches, so completed items are never redone. The file
//! is deleted on full success.
//!
//! Corruption policy mirrors the audit-chain convention: a truncated FINAL
//! line (the crash tore an in-flight append) is dropped and its item simply
//! runs again, while a corrupt line in the middle of the file — or a header
//! that fails to parse — refuses the resume with a repair message instead
//! of silently trusting possibly-wrong skip state.
//!
//! A checkpoint is operational state, not evidence: it lives inside the
//! case directory and is not hash-chained. The input fingerprint is the
//! guard that keeps a stale checkpoint from silently skipping work.

use crate::sha256;
use crate::util::{json_escape, now_unix, sync_parent_directory};
use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// How a command should treat an existing checkpoint file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeMode {
    /// Resume when a checkpoint exists and its input fingerprint matches;
    /// otherwise start fresh (a stale checkpoint never silently skips work).
    Auto,
    /// `--resume`: resume requires the checkpoint's fingerprint to match —
    /// a mismatch is an error rather than a silent fresh start.
    Force,
    /// `--no-resume`: ignore any checkpoint and process everything.
    Disabled,
}

impl ResumeMode {
    /// Resolves the `--resume`/`--no-resume` flag pair (last flag wins on
    /// the command line) into a mode.
    pub fn from_flags(resume: bool, no_resume: bool) -> Self {
        if no_resume {
            Self::Disabled
        } else if resume {
            Self::Force
        } else {
            Self::Auto
        }
    }
}

/// An open checkpoint file. Run exclusivity (two overlapping runs of the
/// same command must never interleave progress records) is held by an
/// fs2 exclusive lock on the `<path>.lock` sidecar for the whole run; the
/// data file itself is never locked, so the workstation can read the live
/// checkpoint through its own handle while the scan runs.
pub struct RunCheckpoint {
    path: PathBuf,
    run_id: String,
    resumed: bool,
    reset_stale: bool,
    lines: Vec<String>,
    /// Data lines of a prior checkpoint discarded for a fingerprint
    /// mismatch. Never replayed as "done" work, but a caller may salvage
    /// per-item facts from them (scan-folder reuses hashes of files whose
    /// size+mtime still match) so a cancelled heavy run is not redone from
    /// zero just because a lighter pass ran in between.
    stale_lines: Vec<String>,
    file: File,
    lock: RunLock,
}

/// The sidecar lock handle. Unlocks on drop — on success (`finish`) and on
/// the interrupted-run path alike. The sidecar file itself is never
/// deleted: unlinking it after the unlock would race a run that opened the
/// old file in between (it then locks an unlinked inode while a third run
/// creates and locks a fresh `.lock`, so two runs hold "the" lock at
/// once). An empty `.lock` beside the checkpoint is harmless; checkpoints
/// live at the top of `db/`, which packaging copies only by explicit
/// file list, so it never enters a package.
struct RunLock {
    file: Option<File>,
}

impl RunLock {
    fn acquire(data_path: &Path, kind: &str) -> Result<Self, String> {
        let path = lock_path(data_path);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|err| format!("failed to open checkpoint lock {}: {err}", path.display()))?;
        file.try_lock_exclusive().map_err(|err| {
            format!(
                "failed to lock checkpoint {} (is another {kind} run active?): {err}",
                data_path.display()
            )
        })?;
        Ok(Self { file: Some(file) })
    }
}

impl Drop for RunLock {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = file.unlock();
        }
    }
}

impl RunCheckpoint {
    /// Opens (or creates) the checkpoint at `path`. When a prior file
    /// exists and `mode` allows it, its data lines are loaded for the
    /// caller to replay; otherwise the file is reset to a fresh header.
    pub fn begin(
        path: &Path,
        kind: &str,
        fingerprint: &str,
        mode: ResumeMode,
    ) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("failed to create checkpoint directory: {err}"))?;
        }
        // The run lock is taken first (sidecar), so a concurrent run is
        // refused before anything touches the data file. Dropping `lock`
        // on any early return below releases it.
        let lock = RunLock::acquire(path, kind)?;
        // Opened without truncate: whether to keep the prior content is
        // decided after parsing it (set_len below performs the reset).
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|err| format!("failed to open checkpoint {}: {err}", path.display()))?;
        let mut text = String::new();
        file.read_to_string(&mut text)
            .map_err(|err| format!("failed to read checkpoint {}: {err}", path.display()))?;

        let mut resumed = false;
        let mut reset_stale = false;
        let mut run_id = new_run_id(kind)?;
        let mut lines = Vec::new();
        let mut stale_lines = Vec::new();

        if mode != ResumeMode::Disabled && !text.trim().is_empty() {
            match parse_checkpoint(&text, path, kind) {
                Ok(Some((prior_run_id, prior_fingerprint, prior_lines))) => {
                    if prior_fingerprint == fingerprint {
                        resumed = true;
                        run_id = prior_run_id;
                        lines = prior_lines;
                    } else if mode == ResumeMode::Force {
                        return Err(format!(
                            "checkpoint {} was recorded for different inputs; rerun without --resume or pass --no-resume to start fresh",
                            path.display()
                        ));
                    } else {
                        reset_stale = true;
                        stale_lines = prior_lines;
                    }
                }
                Ok(None) => {
                    // No complete line at all: the prior run died while
                    // writing its header, so there is nothing to resume.
                }
                Err(err) => {
                    return Err(err);
                }
            }
        }

        if resumed {
            // The file may still carry the torn tail bytes parse_checkpoint
            // dropped; truncate them off before appending so the next
            // record never concatenates onto a half-written line.
            let keep_len = text.rfind('\n').map(|index| index + 1).unwrap_or(0) as u64;
            file.set_len(keep_len)
                .and_then(|_| file.seek(SeekFrom::End(0)))
                .map_err(|err| {
                    format!("failed to position checkpoint {}: {err}", path.display())
                })?;
        } else {
            file.set_len(0)
                .and_then(|_| file.seek(SeekFrom::Start(0)))
                .map_err(|err| format!("failed to reset checkpoint {}: {err}", path.display()))?;
            let header = format!(
                "{{\"schema_version\":1,\"checkpoint\":\"{}\",\"run_id\":\"{}\",\"fingerprint\":\"{}\"}}\n",
                json_escape(kind),
                json_escape(&run_id),
                json_escape(fingerprint)
            );
            file.write_all(header.as_bytes())
                .and_then(|_| file.sync_data())
                .map_err(|err| {
                    format!(
                        "failed to write checkpoint header {}: {err}",
                        path.display()
                    )
                })?;
        }

        Ok(Self {
            path: path.to_path_buf(),
            run_id,
            resumed,
            reset_stale,
            lines,
            stale_lines,
            file,
            lock,
        })
    }

    /// Appends one completed-work record (a single-line JSON object) and
    /// fsyncs it, so a crash loses at most the in-flight line.
    pub fn append_line(&mut self, line: &str) -> Result<(), String> {
        if line.contains('\n') || line.contains('\r') {
            return Err("checkpoint records must be single-line JSON".to_string());
        }
        self.file
            .write_all(line.as_bytes())
            .and_then(|_| self.file.write_all(b"\n"))
            .and_then(|_| self.file.sync_data())
            .map_err(|err| format!("failed to append checkpoint {}: {err}", self.path.display()))
    }

    /// Deletes the checkpoint after the run fully succeeded.
    pub fn finish(self) -> Result<(), String> {
        let RunCheckpoint {
            path, file, lock, ..
        } = self;
        let _ = file.sync_all();
        drop(file);
        let removed = std::fs::remove_file(&path).map_err(|err| {
            format!(
                "failed to remove completed checkpoint {}: {err}",
                path.display()
            )
        });
        // The lock outlives the data-file removal so a run starting in
        // between cannot observe a half-finished state. The sidecar stays
        // on disk (see `RunLock`): deleting it would reopen the unlink race.
        drop(lock);
        removed?;
        sync_parent_directory(&path);
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// True when this run replayed data lines from a prior run's file.
    pub fn resumed(&self) -> bool {
        self.resumed
    }

    /// True when a prior checkpoint existed but was discarded because its
    /// input fingerprint did not match (auto mode only — Force errors).
    pub fn reset_stale(&self) -> bool {
        self.reset_stale
    }

    /// Data lines (everything after the header) loaded from the prior run.
    pub fn data_lines(&self) -> &[String] {
        &self.lines
    }

    /// Data lines of the prior checkpoint discarded by [`Self::reset_stale`]
    /// (empty otherwise). Salvage only — never treat them as completed work.
    pub fn stale_data_lines(&self) -> &[String] {
        &self.stale_lines
    }
}

/// SHA-256 over the joined input descriptions. Callers fingerprint every
/// input that affects per-item results (source path, option set, item list)
/// so a checkpoint recorded under different inputs never silently skips.
pub fn fingerprint(parts: &[&str]) -> String {
    // Unit-separator joins keep ambiguous concatenations ("ab"|"c" vs
    // "a"|"bc") from colliding.
    sha256::digest_bytes(parts.join("\u{1f}").as_bytes())
}

/// Sidecar that carries the run lock: `<checkpoint>.lock`. The data file
/// itself is never locked, so other readers (the workstation's live grid
/// poll) can open it while a run is active — Windows locks are mandatory
/// and would otherwise fail every such read with ERROR_LOCK_VIOLATION.
fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

fn new_run_id(kind: &str) -> Result<String, String> {
    let now = now_unix()?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| format!("system time before UNIX epoch: {err}"))?
        .subsec_nanos();
    Ok(format!("{kind}-{now}-{nanos:09}"))
}

/// Complete lines only: a trailing partial line (torn append) is excluded.
fn complete_lines(text: &str) -> Vec<&str> {
    match text.rfind('\n') {
        Some(last) => text[..last]
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect(),
        None => Vec::new(),
    }
}

/// Parses a prior checkpoint file. `Ok(None)` means there is no complete
/// header line to resume from; `Err` means the file is corrupt and resume
/// must be refused rather than trusted.
fn parse_checkpoint(
    text: &str,
    path: &Path,
    kind: &str,
) -> Result<Option<(String, String, Vec<String>)>, String> {
    let corrupt = |detail: String| {
        format!(
            "checkpoint {} is corrupt ({detail}); delete it or rerun with --no-resume to start fresh",
            path.display()
        )
    };
    let mut lines = complete_lines(text);
    if lines.is_empty() {
        return Ok(None);
    }
    let header = lines.remove(0);
    let value: serde_json::Value = serde_json::from_str(header)
        .map_err(|err| corrupt(format!("header line does not parse: {err}")))?;
    let header_kind = value
        .get("checkpoint")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| corrupt("header is missing the checkpoint kind".to_string()))?;
    if header_kind != kind {
        return Err(corrupt(format!(
            "checkpoint kind is '{header_kind}', expected '{kind}'"
        )));
    }
    let run_id = value
        .get("run_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| corrupt("header is missing run_id".to_string()))?
        .to_string();
    let fingerprint = value
        .get("fingerprint")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| corrupt("header is missing fingerprint".to_string()))?
        .to_string();

    let mut data = Vec::with_capacity(lines.len());
    for (index, line) in lines.into_iter().enumerate() {
        // A mid-file line that fails to parse is not a torn tail — refuse
        // rather than guess which completed records are trustworthy.
        let parsed: serde_json::Value = serde_json::from_str(line)
            .map_err(|err| corrupt(format!("record line {} does not parse: {err}", index + 2)))?;
        if !parsed.is_object() {
            return Err(corrupt(format!(
                "record line {} is not a JSON object",
                index + 2
            )));
        }
        data.push(line.to_string());
    }
    Ok(Some((run_id, fingerprint, data)))
}

#[cfg(test)]
mod tests {
    use super::{ResumeMode, RunCheckpoint, fingerprint, lock_path};
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "frametrace-checkpoint-{name}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn begin_append_finish_round_trip() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("db/progress.jsonl");
        {
            let mut checkpoint =
                RunCheckpoint::begin(&path, "scan-folder", "fp1", ResumeMode::Auto).unwrap();
            assert!(!checkpoint.resumed());
            checkpoint.append_line("{\"key\":\"a\"}").unwrap();
            checkpoint.append_line("{\"key\":\"b\"}").unwrap();
            // Dropping without finish keeps the checkpoint on disk.
        }
        {
            let checkpoint =
                RunCheckpoint::begin(&path, "scan-folder", "fp1", ResumeMode::Auto).unwrap();
            assert!(checkpoint.resumed());
            assert_eq!(
                checkpoint.data_lines(),
                &["{\"key\":\"a\"}".to_string(), "{\"key\":\"b\"}".to_string()]
            );
            checkpoint.finish().unwrap();
        }
        assert!(!path.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn fingerprint_mismatch_resets_in_auto_and_errors_in_force() {
        let dir = temp_dir("fingerprint");
        let path = dir.join("db/progress.jsonl");
        {
            let mut checkpoint =
                RunCheckpoint::begin(&path, "scan-folder", "fp1", ResumeMode::Auto).unwrap();
            checkpoint.append_line("{\"key\":\"a\"}").unwrap();
        }
        {
            let checkpoint =
                RunCheckpoint::begin(&path, "scan-folder", "fp2", ResumeMode::Auto).unwrap();
            assert!(!checkpoint.resumed());
            assert!(checkpoint.reset_stale());
            assert!(checkpoint.data_lines().is_empty());
            checkpoint.finish().unwrap();
        }
        {
            let mut checkpoint =
                RunCheckpoint::begin(&path, "scan-folder", "fp1", ResumeMode::Auto).unwrap();
            checkpoint.append_line("{\"key\":\"a\"}").unwrap();
        }
        let err = RunCheckpoint::begin(&path, "scan-folder", "fp2", ResumeMode::Force)
            .map(|_| ())
            .unwrap_err();
        assert!(err.contains("different inputs"), "{err}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn torn_tail_is_dropped_and_run_resumes_from_complete_lines() {
        let dir = temp_dir("torn");
        let path = dir.join("db/progress.jsonl");
        {
            let mut checkpoint =
                RunCheckpoint::begin(&path, "scan-folder", "fp1", ResumeMode::Auto).unwrap();
            checkpoint.append_line("{\"key\":\"a\"}").unwrap();
        }
        // Simulate a crash mid-append: partial JSON with no newline.
        use std::io::Write;
        let mut handle = fs::OpenOptions::new().append(true).open(&path).unwrap();
        handle.write_all(b"{\"key\":\"b").unwrap();
        drop(handle);

        let checkpoint =
            RunCheckpoint::begin(&path, "scan-folder", "fp1", ResumeMode::Auto).unwrap();
        assert!(checkpoint.resumed());
        assert_eq!(checkpoint.data_lines(), &["{\"key\":\"a\"}".to_string()]);
        checkpoint.finish().unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupt_mid_file_line_refuses_resume() {
        let dir = temp_dir("corrupt");
        let path = dir.join("db/progress.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "{\"schema_version\":1,\"checkpoint\":\"scan-folder\",\"run_id\":\"r\",\"fingerprint\":\"fp1\"}\n{\"key\":\"a\"}\nnot json\n",
            )
        .unwrap();
        let err = RunCheckpoint::begin(&path, "scan-folder", "fp1", ResumeMode::Auto)
            .map(|_| ())
            .unwrap_err();
        assert!(err.contains("corrupt"), "{err}");
        assert!(err.contains("--no-resume"), "{err}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn no_resume_ignores_existing_checkpoint() {
        let dir = temp_dir("disabled");
        let path = dir.join("db/progress.jsonl");
        {
            let mut checkpoint =
                RunCheckpoint::begin(&path, "scan-folder", "fp1", ResumeMode::Auto).unwrap();
            checkpoint.append_line("{\"key\":\"a\"}").unwrap();
        }
        {
            let checkpoint =
                RunCheckpoint::begin(&path, "scan-folder", "fp1", ResumeMode::Disabled).unwrap();
            assert!(!checkpoint.resumed());
            assert!(checkpoint.data_lines().is_empty());
            checkpoint.finish().unwrap();
        }
        let _ = fs::remove_dir_all(dir);
    }

    /// The workstation polls `db/scan-progress.jsonl` through a second
    /// handle while the scan runs (live grid). Windows file locks are
    /// mandatory, so locking the data file itself made every such read
    /// fail with ERROR_LOCK_VIOLATION; the run lock lives in a sidecar.
    #[test]
    fn data_file_stays_readable_while_a_run_holds_the_lock() {
        let dir = temp_dir("readable");
        let path = dir.join("db/progress.jsonl");
        let mut checkpoint =
            RunCheckpoint::begin(&path, "scan-folder", "fp1", ResumeMode::Auto).unwrap();
        checkpoint.append_line("{\"key\":\"a\"}").unwrap();
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("second handle could not read the checkpoint: {err}"));
        assert!(text.contains("\"key\":\"a\""), "{text}");
        // The same works through the server's reader helper.
        let via_util = crate::util::read_to_string(&path).unwrap();
        assert_eq!(via_util, text);
        checkpoint.finish().unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    /// Run exclusivity still holds: a second `begin` on the same path is
    /// refused while the first run is active, and allowed again after it
    /// finished (or was dropped).
    #[test]
    fn second_begin_is_refused_while_a_run_is_active() {
        let dir = temp_dir("exclusive");
        let path = dir.join("db/progress.jsonl");
        let first = RunCheckpoint::begin(&path, "scan-folder", "fp1", ResumeMode::Auto).unwrap();
        let err = RunCheckpoint::begin(&path, "scan-folder", "fp1", ResumeMode::Auto)
            .map(|_| ())
            .unwrap_err();
        assert!(err.contains("another scan-folder run active"), "{err}");
        first.finish().unwrap();
        let again = RunCheckpoint::begin(&path, "scan-folder", "fp1", ResumeMode::Auto).unwrap();
        assert!(!again.resumed());
        drop(again);
        // Dropped without finish (interrupted run): the lock is released.
        let resumed = RunCheckpoint::begin(&path, "scan-folder", "fp1", ResumeMode::Auto).unwrap();
        assert!(resumed.resumed());
        resumed.finish().unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    /// `finish` removes the data file but deliberately leaves the `.lock`
    /// sidecar (deleting it after unlocking races a concurrent `begin`
    /// into holding a different lock file). The leftover sidecar does not
    /// get in the way: the next run locks it again and is exclusive.
    #[test]
    fn finish_leaves_the_lock_sidecar_and_a_new_run_still_locks_it() {
        let dir = temp_dir("sidecar-kept");
        let path = dir.join("db/progress.jsonl");
        let first = RunCheckpoint::begin(&path, "scan-folder", "fp1", ResumeMode::Auto).unwrap();
        first.finish().unwrap();
        assert!(
            !path.exists(),
            "completed checkpoint data file must be removed"
        );
        assert!(
            lock_path(&path).is_file(),
            "lock sidecar must be left in place by finish"
        );
        let second = RunCheckpoint::begin(&path, "scan-folder", "fp1", ResumeMode::Auto).unwrap();
        assert!(!second.resumed());
        let err = RunCheckpoint::begin(&path, "scan-folder", "fp1", ResumeMode::Auto)
            .map(|_| ())
            .unwrap_err();
        assert!(err.contains("another scan-folder run active"), "{err}");
        second.finish().unwrap();
        assert!(lock_path(&path).is_file());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn fingerprints_distinguish_input_lists() {
        assert_eq!(fingerprint(&["a", "bc"]), fingerprint(&["a", "bc"]));
        assert_ne!(fingerprint(&["ab", "c"]), fingerprint(&["a", "bc"]));
    }
}
