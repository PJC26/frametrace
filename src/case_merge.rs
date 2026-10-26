//! Merge other cases' video indexes into this case's index.
//!
//! Each source case contributes its `db/videos.jsonl` rows as new records:
//! record ids are renumbered onto this case's `vid_NNNNNN` sequence (the
//! original id is kept as `merged_from_id`), and every merged record carries
//! `merged_from` with the source case path for provenance. Records whose
//! recorded sha256 already exists in the target index are still merged —
//! both records stay — but the merged copy is marked `duplicate_of` the
//! earlier record's id. Merged rows are claims copied from another case's
//! index, not re-verified evidence, hence the `candidate-merge` label.
//!
//! The merge rewrites `db/videos.jsonl`, `db/video_index.json`, and
//! `db/video_paths.tsv`, upserts the new rows into `db/case.db`, and appends
//! one chained audit entry per source case to
//! `evidence/logs/case-merge-log.jsonl`.
//!
//! Review annotations (marks, notes, IN/OUT ranges, tags, mark history)
//! follow their records to the renumbered ids
//! ([`crate::case_db::merge_annotations`]): they are written to the target
//! database in one transaction after the index rows and before the audit
//! entries, so a failed annotation write rolls back every annotation and
//! leaves no success audit line. The three index files and the database
//! change as one set (I8): the new index files are staged under
//! `db/.merge-stage-*`, the database is snapshotted with SQLite's backup
//! API and then written, and only after both database steps succeed are
//! the staged files renamed over the originals. A failure at any step
//! restores the database snapshot and any index file already replaced, so
//! the target is left exactly as it was (if a restore itself fails, the
//! error says so and the staging directory with the originals is kept).
//! The audit entries are appended last; a failure there is reported as
//! "merge applied, audit entry not written", never hidden.

use crate::audit;
use crate::case_db::merge_annotations::{
    AnnotationMergeCounts, MergedIdMapping, SourceAnnotationMerge, SourceAnnotations,
    apply_merge_annotations, load_source_annotations,
};
use crate::case_db::{IndexedVideoRow, upsert_indexed_rows};
use crate::scan::{json_record_lines, set_json_field, tsv_escape};
use crate::util::{json_escape, now_unix, read_to_string, write_text_atomic};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Candidate-grade label: merged rows are recorded claims from another
/// case's index, not re-verified content.
pub const LABEL: &str = "candidate-merge";

#[derive(Debug, Clone)]
pub struct MergeSourceSummary {
    pub case_dir: PathBuf,
    pub merged_records: usize,
    pub duplicate_records: usize,
    /// What happened to this source's review annotations.
    pub annotations: AnnotationMergeCounts,
}

#[derive(Debug, Clone)]
pub struct MergeResult {
    pub generated_unix: u64,
    pub total_records: usize,
    pub merged_records: usize,
    pub duplicate_records: usize,
    pub sources: Vec<MergeSourceSummary>,
}

impl MergeResult {
    /// Annotation counts summed over every source.
    pub fn annotation_totals(&self) -> AnnotationMergeCounts {
        let mut total = AnnotationMergeCounts::default();
        for source in &self.sources {
            let counts = &source.annotations;
            total.marks_merged += counts.marks_merged;
            total.tags_merged += counts.tags_merged;
            total.notes_appended += counts.notes_appended;
            total.marks_conflicted += counts.marks_conflicted;
            total.history_rows_copied += counts.history_rows_copied;
            total.annotations_unmapped += counts.annotations_unmapped;
        }
        total
    }
}

/// Merge every `source_case_dirs` index into `case_dir`'s index. Sources are
/// processed in argument order; the operation fails loudly when a source has
/// no index (a likely operator error) but tolerates unparseable rows by
/// skipping them, matching the read policy of the other index consumers.
pub fn merge_cases(case_dir: &Path, source_case_dirs: &[PathBuf]) -> Result<MergeResult, String> {
    if source_case_dirs.is_empty() {
        return Err("merge-cases requires at least one source case directory".to_string());
    }
    let case_root = case_dir
        .canonicalize()
        .map_err(|err| format!("failed to canonicalize case directory: {err}"))?;
    let mut existing_lines = load_target_lines(case_dir)?;
    let mut positions_by_path: HashMap<String, usize> = existing_lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let value: serde_json::Value =
                serde_json::from_str(line).expect("validated target record");
            // R5-P3: lossless file identity (raw form of a non-Unicode name).
            (crate::model::record_path_key(&value), i)
        })
        .collect();

    // sha256 -> first record id, for duplicate marking. Unhashed records
    // never deduplicate (nothing trustworthy to compare on).
    let mut id_by_sha256: HashMap<String, String> = HashMap::new();
    let mut next_number = 1usize;
    for line in &existing_lines {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let id = string_field(&value, "id");
        if let Some(number) = id
            .strip_prefix("vid_")
            .and_then(|rest| rest.parse::<usize>().ok())
        {
            next_number = next_number.max(number + 1);
        }
        if let Some(sha) = value.get("sha256").and_then(serde_json::Value::as_str) {
            id_by_sha256
                .entry(sha.to_ascii_lowercase())
                .or_insert_with(|| id.clone());
        }
    }

    // Validate every source before mutating anything: a failure here must
    // leave the target index and its audit log untouched.
    let mut sources: Vec<(PathBuf, Vec<String>, SourceAnnotations)> = Vec::new();
    for source_dir in source_case_dirs {
        let source_root = source_dir.canonicalize().map_err(|err| {
            format!(
                "failed to canonicalize source case {}: {err}",
                source_dir.display()
            )
        })?;
        if source_root == case_root {
            return Err(format!(
                "cannot merge a case into itself: {}",
                source_dir.display()
            ));
        }
        let source_lines = read_index_lines(&source_root, true)?;
        // Read annotations up front too: an unreadable source database must
        // fail the merge before anything is written, never drop its marks.
        let annotations = load_source_annotations(&source_root)?;
        // Every recorded spelling of the source (merged_from,
        // merge_provenance, audit line, history detail, note segment) uses
        // the display path without the Windows verbatim prefix that
        // canonicalize adds (contract §10.2). All reads above used the
        // canonical path.
        sources.push((
            crate::util::strip_windows_extended_prefix(&source_root),
            source_lines,
            annotations,
        ));
    }

    let mut summaries = Vec::new();
    let mut mappings: Vec<Vec<MergedIdMapping>> = Vec::new();
    let generated_unix = now_unix()?;

    for (source_root, source_lines, _) in &sources {
        let mut merged = 0usize;
        let mut duplicates = 0usize;
        let mut mapping: Vec<MergedIdMapping> = Vec::new();
        for line in source_lines {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let original_id = string_field(&value, "id");
            let source_path = string_field(&value, "source_path");
            if original_id.is_empty() || source_path.is_empty() {
                continue;
            }
            // R5-P3: two distinct non-Unicode names share one lossy display
            // spelling; only the lossless key tells them apart.
            let path_key = crate::model::record_path_key(&value);
            if let Some(&position) = positions_by_path.get(&path_key) {
                let mut kept: serde_json::Value =
                    serde_json::from_str(&existing_lines[position])
                        .map_err(|err| format!("invalid merged record: {err}"))?;
                let provenance = serde_json::json!({"source_case": source_root, "record": value});
                let entries = kept
                    .as_object_mut()
                    .expect("record object")
                    .entry("merge_provenance")
                    .or_insert_with(|| serde_json::json!([]))
                    .as_array_mut()
                    .ok_or("invalid merge_provenance: expected array")?;
                if !entries.contains(&provenance) {
                    entries.push(provenance);
                }
                mapping.push(MergedIdMapping {
                    original_id: original_id.clone(),
                    new_id: string_field(&kept, "id"),
                    source_path: source_path.clone(),
                });
                existing_lines[position] = kept.to_string();
                duplicates += 1;
                continue;
            }
            let new_id = format!("vid_{next_number:06}");
            next_number += 1;
            let duplicate_of = value
                .get("sha256")
                .and_then(serde_json::Value::as_str)
                .and_then(|sha| id_by_sha256.get(&sha.to_ascii_lowercase()).cloned());
            if duplicate_of.is_some() {
                duplicates += 1;
            }
            if let Some(sha) = value.get("sha256").and_then(serde_json::Value::as_str) {
                id_by_sha256
                    .entry(sha.to_ascii_lowercase())
                    .or_insert_with(|| new_id.clone());
            }

            // Textual field edits keep the copied record's published field
            // order/spelling; only provenance is added or replaced.
            let mut merged_line =
                set_json_field(line, "id", &format!("\"{}\"", json_escape(&new_id)));
            merged_line = set_json_field(
                &merged_line,
                "merged_from",
                &format!("\"{}\"", json_escape(&source_root.to_string_lossy())),
            );
            merged_line = set_json_field(
                &merged_line,
                "merged_from_id",
                &format!("\"{}\"", json_escape(&original_id)),
            );
            if let Some(duplicate_of) = &duplicate_of {
                merged_line = set_json_field(
                    &merged_line,
                    "duplicate_of",
                    &format!("\"{}\"", json_escape(duplicate_of)),
                );
            }
            mapping.push(MergedIdMapping {
                original_id: original_id.clone(),
                new_id: new_id.clone(),
                source_path: source_path.clone(),
            });
            positions_by_path.insert(path_key, existing_lines.len());
            existing_lines.push(merged_line);
            merged += 1;
        }
        summaries.push(MergeSourceSummary {
            case_dir: source_root.clone(),
            merged_records: merged,
            duplicate_records: duplicates,
            annotations: AnnotationMergeCounts::default(),
        });
        mappings.push(mapping);
    }

    // Deterministic ordering: existing records keep their (lower) id order
    // and merged records follow in argument/line order under fresh ids.
    let mut all = existing_lines;
    all.sort_by_key(|line| line_id(line));

    let mut jsonl = String::new();
    let mut tsv = String::from(
        "id\tsource_path\trelative_path\textension\tsize_bytes\tsha256\tvendor\tparser\tparser_confidence\n",
    );
    for line in &all {
        jsonl.push_str(line);
        jsonl.push('\n');
        tsv.push_str(&tsv_row_for_line(line));
    }
    let index_json = merged_index_json_text(case_dir, &all, generated_unix)?;
    let rows = all
        .iter()
        .map(|line| {
            serde_json::from_str(line)
                .map(|value| row_from_value(&value, line))
                .map_err(|err| format!("invalid merged record: {err}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let plans: Vec<SourceAnnotationMerge<'_>> = sources
        .iter()
        .zip(&mappings)
        .map(
            |((source_root, _, annotations), mapping)| SourceAnnotationMerge {
                source_case: source_root.to_string_lossy().into_owned(),
                mapping,
                annotations,
            },
        )
        .collect();
    // I8: the three index files and the database change as one set. The
    // new index files are staged first; the database is snapshotted, then
    // written (rows + annotations); only then are the staged files swapped
    // in. Any failure restores the database snapshot and the index files
    // that were already swapped, so a failed merge leaves every store as
    // it was.
    let staged = StagedFiles::stage(
        case_dir,
        &[
            ("db/videos.jsonl", jsonl.as_str()),
            ("db/video_paths.tsv", tsv.as_str()),
            ("db/video_index.json", index_json.as_str()),
        ],
    )?;
    let snapshot = match DbSnapshot::take(case_dir, staged.dir()) {
        Ok(snapshot) => snapshot,
        Err(err) => {
            staged.discard();
            return Err(err);
        }
    };
    let written = upsert_indexed_rows(case_dir, &rows, generated_unix)
        .and_then(|()| apply_merge_annotations(case_dir, &plans, generated_unix))
        .and_then(|outcome| staged.swap().map(|()| outcome));
    let outcome = match written {
        Ok(outcome) => outcome,
        Err(err) => {
            let message = match snapshot.restore(case_dir) {
                Ok(()) => format!(
                    "merge-cases failed and was rolled back (index files and database unchanged): {err}"
                ),
                Err(restore_err) => format!(
                    "merge-cases failed: {err}; restoring the database snapshot ALSO failed: {restore_err} — the pre-merge database is kept at {}",
                    snapshot
                        .path()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default()
                ),
            };
            // Backups and the snapshot stay on disk unless everything was
            // put back.
            if snapshot.restored_or_absent() && !staged.restore_failed.get() {
                staged.discard();
            }
            return Err(message);
        }
    };
    staged.discard();
    let mut audit_lines = Vec::new();
    for (summary, counts) in summaries.iter_mut().zip(outcome.per_source) {
        audit_lines.push(format!(
            "{{\"schema_version\":1,\"event\":\"merge-cases\",\"generated_unix\":{},\"label\":\"{}\",\"source_case\":\"{}\",\"merged_records\":{},\"duplicate_records\":{},\"marks_merged\":{},\"tags_merged\":{},\"notes_appended\":{},\"marks_conflicted\":{},\"history_rows_copied\":{},\"annotations_unmapped\":{}}}",
            generated_unix,
            LABEL,
            json_escape(&summary.case_dir.to_string_lossy()),
            summary.merged_records,
            summary.duplicate_records,
            counts.marks_merged,
            counts.tags_merged,
            counts.notes_appended,
            counts.marks_conflicted,
            counts.history_rows_copied,
            counts.annotations_unmapped,
        ));
        summary.annotations = counts;
    }
    for line in audit_lines {
        audit::append_chained_jsonl(&case_dir.join("evidence/logs/case-merge-log.jsonl"), &line)
            .map_err(|err| {
                format!(
                    "merge-cases applied the merge (index files and database updated) but could not write its audit entry: {err}"
                )
            })?;
    }

    let duplicate_total = summaries.iter().map(|s| s.duplicate_records).sum();
    let merged_total = summaries.iter().map(|s| s.merged_records).sum();
    Ok(MergeResult {
        generated_unix,
        total_records: all.len(),
        merged_records: merged_total,
        duplicate_records: duplicate_total,
        sources: summaries,
    })
}

/// Lines of a case's `db/videos.jsonl`. `required` distinguishes the target
/// (absent index = empty index) from a source (absent index = operator
/// error, fail loudly rather than merge nothing).
fn read_index_lines(case_dir: &Path, required: bool) -> Result<Vec<String>, String> {
    let path = case_dir.join("db/videos.jsonl");
    let text = match read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound && !required => {
            return Ok(Vec::new());
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "source case has no video index at {} — run scan-folder there first",
                path.display()
            ));
        }
        Err(err) => return Err(format!("failed to read {}: {err}", path.display())),
    };
    Ok(json_record_lines(&text))
}

pub(crate) fn load_target_lines(case_dir: &Path) -> Result<Vec<String>, String> {
    let target_lines = read_index_lines(case_dir, false)?;
    let stored_lines = crate::case_db::load_video_record_lines(case_dir)?;
    let mut records_by_path = HashMap::new();
    let mut paths_by_id = HashMap::new();
    let mut lines = Vec::new();
    for line in stored_lines.into_iter().chain(target_lines) {
        let value: serde_json::Value = serde_json::from_str(&line)
            .map_err(|err| format!("invalid target index record: {err}"))?;
        let id = string_field(&value, "id");
        let path = string_field(&value, "source_path");
        if id.is_empty() || path.is_empty() {
            return Err("merge refuses target records without id/source_path".into());
        }
        // R5-P3: records are told apart by their lossless file identity.
        let key = crate::model::record_path_key(&value);
        if let Some(prior) = paths_by_id.insert(id.clone(), key.clone())
            && prior != key
        {
            return Err(format!(
                "merge refuses conflicting target id {id}; repair stores and marks first"
            ));
        }
        if let Some(prior) = records_by_path.get(&key) {
            if string_field(prior, "id") != id {
                return Err(format!(
                    "merge refuses conflicting target path {path}; repair stores and marks first"
                ));
            }
            if prior != &value {
                return Err(format!(
                    "merge refuses conflicting target record {id} at {path}; repair stores first"
                ));
            }
        } else {
            records_by_path.insert(key, value);
            lines.push(line);
        }
    }
    Ok(lines)
}

fn line_id(line: &str) -> String {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .map(|value| string_field(&value, "id"))
        .unwrap_or_default()
}

fn string_field(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn opt_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

/// One `video_paths.tsv` row from a serialized index record.
fn tsv_row_for_line(line: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return String::new();
    };
    let profile = value.get("source_profile").cloned().unwrap_or_default();
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
        tsv_escape(&string_field(&value, "id")),
        tsv_escape(&string_field(&value, "source_path")),
        tsv_escape(&string_field(&value, "relative_path")),
        tsv_escape(&string_field(&value, "extension")),
        value
            .get("size_bytes")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        tsv_escape(&opt_string(&value, "sha256").unwrap_or_default()),
        tsv_escape(&string_field(&profile, "vendor")),
        tsv_escape(&string_field(&profile, "parser")),
        tsv_escape(&string_field(&profile, "confidence")),
    )
}

/// SQLite row for a merged record. `record_json` is the verbatim merged line
/// so `record_json` and the JSONL stay byte-identical.
fn row_from_value(value: &serde_json::Value, record_json: &str) -> IndexedVideoRow {
    let ffprobe_json = match value.get("ffprobe") {
        Some(serde_json::Value::String(text)) => Some(text.clone()),
        Some(other @ (serde_json::Value::Object(_) | serde_json::Value::Array(_))) => {
            Some(other.to_string())
        }
        _ => None,
    };
    IndexedVideoRow {
        id: string_field(value, "id"),
        source_path: string_field(value, "source_path"),
        file_url: string_field(value, "file_url"),
        relative_path: string_field(value, "relative_path"),
        extension: string_field(value, "extension"),
        size_bytes: value
            .get("size_bytes")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        modified_unix: value
            .get("modified_unix")
            .and_then(serde_json::Value::as_u64),
        sha256: opt_string(value, "sha256"),
        hash_status: {
            let status = string_field(value, "hash_status");
            if status.is_empty() {
                "unknown".to_string()
            } else {
                status
            }
        },
        confidence: {
            let confidence = string_field(value, "confidence");
            if confidence.is_empty() {
                "unknown".to_string()
            } else {
                confidence
            }
        },
        source_profile_json: value
            .get("source_profile")
            .map(serde_json::Value::to_string)
            .unwrap_or_else(|| "{}".to_string()),
        duration_seconds: value
            .get("duration_seconds")
            .and_then(serde_json::Value::as_f64),
        format_name: opt_string(value, "format_name"),
        video_codec: opt_string(value, "video_codec"),
        audio_codec: opt_string(value, "audio_codec"),
        width: value.get("width").and_then(serde_json::Value::as_u64),
        height: value.get("height").and_then(serde_json::Value::as_u64),
        ffprobe_ok: value
            .get("ffprobe_ok")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        ffprobe_error: opt_string(value, "ffprobe_error"),
        ffprobe_json,
        record_json: record_json.to_string(),
    }
}

/// Rewrite `db/video_index.json` with the merged record set. Existing
/// top-level fields (source_path, scan options, warnings) are preserved;
/// only `videos`, `video_count`, `total_bytes`, and a `merged_unix` stamp
/// change. When no index exists yet a minimal document is created.
/// I8: new index files written beside the case (`db/.merge-stage-*`)
/// before anything is replaced, and the originals they replace, so the set
/// can be swapped in — or put back — as a unit.
struct StagedFiles {
    dir: PathBuf,
    /// `(staged file, target, backup of the original — None when absent)`.
    entries: Vec<(PathBuf, PathBuf, Option<PathBuf>)>,
    /// Set when a rollback could not put an original back: the staging
    /// directory (holding the backups) is then kept, never deleted.
    restore_failed: std::cell::Cell<bool>,
}

impl StagedFiles {
    fn stage(case_dir: &Path, files: &[(&str, &str)]) -> Result<Self, String> {
        let dir = crate::util::unique_dir(&case_dir.join(format!(
            "db/.merge-stage-{}-{}",
            std::process::id(),
            now_unix()?
        )));
        std::fs::create_dir_all(&dir)
            .map_err(|err| format!("failed to create merge staging directory: {err}"))?;
        let mut staged = Self {
            dir,
            entries: Vec::new(),
            restore_failed: std::cell::Cell::new(false),
        };
        for (index, (rel, text)) in files.iter().enumerate() {
            let target = case_dir.join(rel);
            let staged_path = staged.dir.join(format!("{index}.new"));
            let result = write_text_atomic(&staged_path, text)
                .map_err(|err| format!("failed to stage {rel}: {err}"))
                .and_then(|()| {
                    if target.is_file() {
                        let backup = staged.dir.join(format!("{index}.orig"));
                        std::fs::copy(&target, &backup)
                            .map(|_| Some(backup))
                            .map_err(|err| format!("failed to back up {rel}: {err}"))
                    } else {
                        Ok(None)
                    }
                });
            match result {
                Ok(backup) => staged.entries.push((staged_path, target, backup)),
                Err(err) => {
                    staged.discard();
                    return Err(err);
                }
            }
        }
        Ok(staged)
    }

    fn dir(&self) -> &Path {
        &self.dir
    }

    /// Renames every staged file over its target. If one rename fails, the
    /// targets already replaced are restored from their backups (or
    /// removed when there was no original) before the error is returned.
    fn swap(&self) -> Result<(), String> {
        for (done, (staged, target, _)) in self.entries.iter().enumerate() {
            if let Err(err) = std::fs::rename(staged, target) {
                let mut message = format!("failed to replace {}: {err}", target.display());
                for (_, swapped, backup) in &self.entries[..done] {
                    let restored = match backup {
                        Some(backup) => std::fs::copy(backup, swapped).map(|_| ()),
                        None => std::fs::remove_file(swapped),
                    };
                    if let Err(restore_err) = restored {
                        self.restore_failed.set(true);
                        message.push_str(&format!(
                            "; restoring {} FAILED: {restore_err} (original kept in {})",
                            swapped.display(),
                            self.dir.display()
                        ));
                    }
                }
                return Err(message);
            }
        }
        Ok(())
    }

    fn discard(&self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// I8: an online-backup snapshot of `db/case.db` taken before the merge
/// writes it, restored (backup API, not a file copy) when the merge fails.
struct DbSnapshot {
    /// `None` when there was no database file to snapshot.
    path: Option<PathBuf>,
    restored: std::cell::Cell<bool>,
}

impl DbSnapshot {
    fn take(case_dir: &Path, staging_dir: &Path) -> Result<Self, String> {
        let db = crate::case_db::case_db_path(case_dir);
        if !db.is_file() {
            return Ok(Self {
                path: None,
                restored: std::cell::Cell::new(true),
            });
        }
        let path = staging_dir.join("case.db.snapshot");
        let conn = crate::case_db::open_readonly_case_db(&db)?;
        conn.backup(rusqlite::MAIN_DB, &path, None)
            .map_err(|err| format!("failed to snapshot the case database before merging: {err}"))?;
        Ok(Self {
            path: Some(path),
            restored: std::cell::Cell::new(false),
        })
    }

    fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// True when nothing is left to recover from the staging directory.
    fn restored_or_absent(&self) -> bool {
        self.restored.get()
    }

    fn restore(&self, case_dir: &Path) -> Result<(), String> {
        let db = crate::case_db::case_db_path(case_dir);
        let Some(snapshot) = &self.path else {
            // No database before the merge: remove the one it created.
            for suffix in ["", "-wal", "-shm", "-journal"] {
                let file = PathBuf::from(format!("{}{suffix}", db.display()));
                if file.is_file() {
                    std::fs::remove_file(&file)
                        .map_err(|err| format!("failed to remove {}: {err}", file.display()))?;
                }
            }
            return Ok(());
        };
        let mut conn = rusqlite::Connection::open(&db)
            .map_err(|err| format!("failed to open the case database: {err}"))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|err| format!("failed to configure SQLite busy timeout: {err}"))?;
        conn.restore(
            rusqlite::MAIN_DB,
            snapshot,
            None::<fn(rusqlite::backup::Progress)>,
        )
        .map_err(|err| format!("failed to restore the case database snapshot: {err}"))?;
        self.restored.set(true);
        Ok(())
    }
}

/// The merged `db/video_index.json` text (written by the staged swap).
fn merged_index_json_text(
    case_dir: &Path,
    lines: &[String],
    merged_unix: u64,
) -> Result<String, String> {
    let path = case_dir.join("db/video_index.json");
    let mut index = match read_to_string(&path) {
        Ok(text) => serde_json::from_str::<serde_json::Value>(&text)
            .unwrap_or_else(|_| serde_json::json!({})),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(err) => return Err(format!("failed to read {}: {err}", path.display())),
    };
    if !index.is_object() {
        index = serde_json::json!({});
    }
    let videos: Vec<serde_json::Value> = lines
        .iter()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect();
    let total_bytes: u64 = videos
        .iter()
        .filter_map(|video| video.get("size_bytes").and_then(serde_json::Value::as_u64))
        .sum();
    let object = index.as_object_mut().expect("object ensured above");
    object.insert("schema_version".to_string(), serde_json::json!(1));
    object
        .entry("source_path".to_string())
        .or_insert(serde_json::Value::Null);
    object.insert("videos".to_string(), serde_json::Value::Array(videos));
    object.insert("video_count".to_string(), serde_json::json!(lines.len()));
    object.insert("total_bytes".to_string(), serde_json::json!(total_bytes));
    object.insert("merged_unix".to_string(), serde_json::json!(merged_unix));
    let text = serde_json::to_string_pretty(&index)
        .map_err(|err| format!("failed to serialize merged index: {err}"))?;
    Ok(format!("{text}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_case(name: &str, videos_jsonl: Option<&str>) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "frametrace-merge-cases-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("db")).unwrap();
        fs::write(dir.join("case.json"), "{}").unwrap();
        if let Some(text) = videos_jsonl {
            fs::write(dir.join("db/videos.jsonl"), text).unwrap();
        }
        dir
    }

    fn row(id: &str, path: &str, size: u64, sha: Option<&str>) -> String {
        format!(
            "{{\"id\":\"{id}\",\"source_path\":\"{path}\",\"file_url\":\"file://{path}\",\"relative_path\":\"{id}.mp4\",\"extension\":\"mp4\",\"size_bytes\":{size},\"modified_unix\":100,\"sha256\":{},\"hash_status\":\"complete\",\"confidence\":\"ffprobe-confirmed\",\"source_profile\":{{\"lane\":\"generic-video\",\"vendor\":\"Generic media\",\"parser\":\"generic_media\",\"confidence\":\"medium\",\"recommended_action\":\"x\",\"evidence\":[\"extension\"]}},\"ffprobe_ok\":true,\"ffprobe\":null}}\n",
            sha.map(|s| format!("\"{s}\""))
                .unwrap_or_else(|| "null".into())
        )
    }

    fn jsonl_ids(case_dir: &Path) -> Vec<serde_json::Value> {
        read_to_string(&case_dir.join("db/videos.jsonl"))
            .unwrap()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn merges_two_cases_with_renumbered_ids_and_provenance() {
        let target = temp_case(
            "renum-t",
            Some(&row("vid_000001", "/ev/a.mp4", 10, Some("aa"))),
        );
        let source = temp_case(
            "renum-s",
            Some(&format!(
                "{}{}",
                row("vid_000001", "/ev/b.mp4", 20, Some("bb")),
                row("vid_000002", "/ev/c.mp4", 30, Some("cc"))
            )),
        );
        let result = merge_cases(&target, std::slice::from_ref(&source)).unwrap();
        assert_eq!(result.merged_records, 2);
        assert_eq!(result.duplicate_records, 0);
        assert_eq!(result.total_records, 3);

        let rows = jsonl_ids(&target);
        assert_eq!(rows.len(), 3);
        // Renumbered onto the target's id sequence; original ids preserved.
        let merged: Vec<_> = rows
            .iter()
            .filter(|row| row.get("merged_from").is_some())
            .collect();
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0]["id"].as_str(), Some("vid_000002"));
        assert_eq!(merged[0]["merged_from_id"].as_str(), Some("vid_000001"));
        assert_eq!(
            merged[0]["merged_from"].as_str(),
            Some(
                crate::util::canonicalize_display(&source)
                    .unwrap()
                    .to_string_lossy()
                    .as_ref()
            )
        );

        let index = read_to_string(&target.join("db/video_index.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&index).unwrap();
        assert_eq!(parsed["video_count"].as_u64(), Some(3));
        assert_eq!(parsed["videos"].as_array().unwrap().len(), 3);
        let ids = crate::case_db::load_video_ids(&target).unwrap();
        assert_eq!(ids.len(), 3);
        assert!(ids.iter().any(|row| row.id == "vid_000001"));

        // One audit entry per source, chain intact.
        let verification =
            audit::verify_chained_jsonl(&target.join("evidence/logs/case-merge-log.jsonl"))
                .unwrap();
        assert_eq!(verification.entries, 1);

        let _ = fs::remove_dir_all(&target);
        let _ = fs::remove_dir_all(&source);
    }

    /// R5-P3: two source records whose non-Unicode names share one lossy
    /// display spelling are two files — the merge key is the lossless one.
    #[test]
    fn non_unicode_twins_with_one_display_path_stay_two_records() {
        let with_raw = |id: &str, raw: &str, size: u64| -> String {
            let mut value: serde_json::Value =
                serde_json::from_str(&row(id, "/ev/\u{FFFD}.mp4", size, None)).unwrap();
            value["source_path_raw_hex"] = serde_json::json!(raw);
            format!("{value}\n")
        };
        let target = temp_case(
            "twins-t",
            Some(&row("vid_000001", "/ev/a.mp4", 10, Some("aa"))),
        );
        let source = temp_case(
            "twins-s",
            Some(&format!(
                "{}{}",
                with_raw("vid_000001", "unix:2f65762fff2e6d7034", 20),
                with_raw("vid_000002", "unix:2f65762ffe2e6d7034", 30),
            )),
        );
        let result = merge_cases(&target, std::slice::from_ref(&source)).unwrap();
        assert_eq!(result.merged_records, 2, "{result:?}");
        assert_eq!(result.total_records, 3);
        let rows = jsonl_ids(&target);
        let raws: Vec<_> = rows
            .iter()
            .filter_map(|r| r["source_path_raw_hex"].as_str())
            .collect();
        assert_eq!(raws.len(), 2, "{rows:?}");
        // Merging again recognises both by their lossless key (no growth).
        let again = merge_cases(&target, std::slice::from_ref(&source)).unwrap();
        assert_eq!(again.merged_records, 0);
        assert_eq!(again.total_records, 3);
        let _ = fs::remove_dir_all(&target);
        let _ = fs::remove_dir_all(&source);
    }

    #[test]
    fn overlapping_paths_twice_preserve_ids_marks_and_store_rows() {
        let original = row("vid_000007", "/ev/shared.mp4", 10, Some("aa"));
        let target = temp_case("overlap-t", Some(&original));
        let parsed = serde_json::from_str(&original).unwrap();
        upsert_indexed_rows(&target, &[row_from_value(&parsed, original.trim())], 1).unwrap();
        crate::case_db::upsert_review_marks(
            &target,
            &[crate::case_db::ReviewMarkRow {
                record_id: "vid_000007".into(),
                status: "relevant".into(),
                marked_unix: 1,
                record_path: Some("/ev/shared.mp4".into()),
                examiner: None,
                note: Some("keep".into()),
                range: None,
            }],
        )
        .unwrap();
        let source = temp_case(
            "overlap-s",
            Some(&format!(
                "{}{}",
                row("vid_000001", "/ev/shared.mp4", 20, Some("bb")),
                row("vid_000002", "/ev/new.mp4", 30, Some("cc")),
            )),
        );
        for _ in 0..2 {
            let result = merge_cases(&target, std::slice::from_ref(&source)).unwrap();
            let rows = jsonl_ids(&target);
            let ids = crate::case_db::load_video_ids(&target).unwrap();
            assert_eq!(rows.len(), ids.len());
            assert_eq!(result.total_records, 2);
            for id in &ids {
                let record = rows.iter().find(|r| r["id"] == id.id).unwrap();
                assert_eq!(record["source_path"], id.source_path);
            }
            let shared = rows
                .iter()
                .find(|r| r["source_path"] == "/ev/shared.mp4")
                .unwrap();
            assert_eq!(shared["id"], "vid_000007");
            assert_eq!(shared["sha256"], "aa");
            assert!(
                shared["merge_provenance"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|p| p["record"]["id"] == "vid_000001")
            );
            let marks = crate::case_db::load_review_marks(&target).unwrap();
            assert_eq!(marks[0].record_id, "vid_000007");
            assert_eq!(marks[0].note.as_deref(), Some("keep"));
            assert!(ids.iter().any(|id| id.id == marks[0].record_id));
            crate::qa::consistency_report(&target, &target.join("qa")).unwrap();
            let conn = crate::case_db::open_readonly_case_db(&target.join("db/case.db")).unwrap();
            for record in rows {
                let stored: String = conn
                    .query_row(
                        "SELECT record_json FROM videos WHERE id = ?1",
                        [record["id"].as_str().unwrap()],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&stored).unwrap(),
                    record
                );
            }
            assert_eq!(
                read_to_string(&target.join("db/video_paths.tsv"))
                    .unwrap()
                    .lines()
                    .count(),
                3
            );
        }
        let _ = fs::remove_dir_all(target);
        let _ = fs::remove_dir_all(source);
    }

    fn snapshot(dir: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
        let mut files = std::collections::BTreeMap::new();
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files.insert(path.clone(), Vec::new());
                files.extend(snapshot(&path));
            } else {
                files.insert(path.clone(), fs::read(path).unwrap());
            }
        }
        files
    }

    /// Bytes of the three index files (None = absent).
    fn index_files(case_dir: &Path) -> Vec<Option<Vec<u8>>> {
        [
            "db/videos.jsonl",
            "db/video_paths.tsv",
            "db/video_index.json",
        ]
        .iter()
        .map(|rel| fs::read(case_dir.join(rel)).ok())
        .collect()
    }

    fn no_staging_left(case_dir: &Path) -> bool {
        fs::read_dir(case_dir.join("db")).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".merge-stage-")
        })
    }

    /// I8: a forced database failure (the row upsert aborts) leaves the
    /// three index files byte-for-byte unchanged, the database rows as they
    /// were, no audit entry and no staging directory.
    #[test]
    fn forced_db_failure_leaves_index_files_unchanged() {
        let original = row("vid_000001", "/ev/a.mp4", 10, Some("aa"));
        let target = temp_case("forced-db-t", Some(&original));
        let parsed = serde_json::from_str(&original).unwrap();
        upsert_indexed_rows(&target, &[row_from_value(&parsed, original.trim())], 1).unwrap();
        fs::write(
            target.join("db/video_paths.tsv"),
            b"id\tsource_path\nvid_000001\t/ev/a.mp4\n",
        )
        .unwrap();
        fs::write(target.join("db/video_index.json"), b"{\"videos\":[]}\n").unwrap();
        let conn = rusqlite::Connection::open(target.join("db/case.db")).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER fail_videos BEFORE INSERT ON videos BEGIN SELECT RAISE(ABORT, 'forced db failure'); END;",
        )
        .unwrap();
        drop(conn);
        let source = temp_case(
            "forced-db-s",
            Some(&row("vid_000001", "/ev/new.mp4", 20, Some("bb"))),
        );
        let files_before = index_files(&target);
        let ids_before = crate::case_db::load_video_ids(&target).unwrap();

        let err = merge_cases(&target, std::slice::from_ref(&source)).unwrap_err();
        assert!(err.contains("forced db failure"), "{err}");
        assert_eq!(index_files(&target), files_before);
        let ids_after = crate::case_db::load_video_ids(&target).unwrap();
        assert_eq!(ids_after.len(), ids_before.len());
        assert!(!target.join("evidence/logs/case-merge-log.jsonl").exists());
        assert!(no_staging_left(&target));
        let _ = fs::remove_dir_all(target);
        let _ = fs::remove_dir_all(source);
    }

    /// I8: when one staged file cannot be swapped in, the files already
    /// replaced are put back (or removed when they did not exist).
    #[test]
    fn failed_swap_restores_already_replaced_files() {
        let case_dir = temp_case("swap-rollback", Some("old jsonl\n"));
        fs::create_dir_all(case_dir.join("db/blocked.tsv/inner")).unwrap();
        let staged = StagedFiles::stage(
            &case_dir,
            &[
                ("db/videos.jsonl", "new jsonl\n"),
                ("db/fresh.json", "new\n"),
                ("db/blocked.tsv", "cannot replace a directory\n"),
            ],
        )
        .unwrap();
        let err = staged.swap().unwrap_err();
        assert!(err.contains("blocked.tsv"), "{err}");
        assert!(!staged.restore_failed.get());
        assert_eq!(
            fs::read_to_string(case_dir.join("db/videos.jsonl")).unwrap(),
            "old jsonl\n"
        );
        assert!(!case_dir.join("db/fresh.json").exists());
        staged.discard();
        assert!(no_staging_left(&case_dir));
        let _ = fs::remove_dir_all(case_dir);
    }

    #[test]
    fn conflicting_target_records_leave_everything_unchanged() {
        for field in ["sha256", "source_profile"] {
            let original = row("vid_000007", "/ev/shared.mp4", 10, Some("aa"));
            let target = temp_case(&format!("conflict-{field}-t"), Some(&original));
            let mut parsed: serde_json::Value = serde_json::from_str(&original).unwrap();
            parsed[field] = if field == "sha256" {
                serde_json::json!("bb")
            } else {
                serde_json::json!({"vendor": "different metadata"})
            };
            upsert_indexed_rows(&target, &[row_from_value(&parsed, &parsed.to_string())], 1)
                .unwrap();
            crate::case_db::upsert_review_marks(
                &target,
                &[crate::case_db::ReviewMarkRow {
                    record_id: "vid_000007".into(),
                    status: "relevant".into(),
                    marked_unix: 1,
                    record_path: Some("/ev/shared.mp4".into()),
                    examiner: None,
                    note: Some("keep".into()),
                    range: None,
                }],
            )
            .unwrap();
            fs::write(target.join("db/video_index.json"), b"{\"preserve\":true}").unwrap();
            fs::write(target.join("db/video_paths.tsv"), b"preserve\n").unwrap();
            let source = temp_case(
                &format!("conflict-{field}-s"),
                Some(&row("vid_000001", "/ev/new.mp4", 20, None)),
            );
            let before = snapshot(&target);
            let source_before = snapshot(&source);
            let err = merge_cases(&target, std::slice::from_ref(&source)).unwrap_err();
            assert!(err.contains("conflicting target record"), "{err}");
            assert_eq!(snapshot(&target), before);
            assert_eq!(snapshot(&source), source_before);
            let err = crate::cli::handlers::merge_cases(&target, std::slice::from_ref(&source))
                .unwrap_err();
            assert!(err.contains("conflicting target record"), "{err}");
            assert_eq!(snapshot(&target), before);
            assert_eq!(snapshot(&source), source_before);
            assert!(!target.join("evidence/logs/case-merge-log.jsonl").exists());
            let _ = fs::remove_dir_all(target);
            let _ = fs::remove_dir_all(source);
        }
    }

    #[test]
    fn equivalent_target_records_ignore_object_order() {
        let original = row("vid_000007", "/ev/shared.mp4", 10, Some("aa"));
        let target = temp_case("ordered-t", Some(&original));
        let mut parsed: serde_json::Value = serde_json::from_str(&original).unwrap();
        parsed.as_object_mut().unwrap().sort_keys();
        parsed["source_profile"]
            .as_object_mut()
            .unwrap()
            .sort_keys();
        assert_ne!(original.trim(), parsed.to_string());
        upsert_indexed_rows(&target, &[row_from_value(&parsed, &parsed.to_string())], 1).unwrap();
        let source = temp_case("ordered-s", Some(&original));
        let result = merge_cases(&target, std::slice::from_ref(&source)).unwrap();
        assert_eq!(result.total_records, 1);
        assert_eq!(result.duplicate_records, 1);
        assert_eq!(result.merged_records, 0);
        let _ = fs::remove_dir_all(target);
        let _ = fs::remove_dir_all(source);
    }

    #[test]
    fn shared_sha256_marks_the_merged_copy_as_duplicate() {
        let target = temp_case(
            "dup-t",
            Some(&row("vid_000001", "/ev/a.mp4", 10, Some("aa"))),
        );
        let source = temp_case(
            "dup-s",
            Some(&format!(
                "{}{}",
                row("vid_000009", "/other/a-copy.mp4", 10, Some("aa")),
                row("vid_000010", "/other/b.mp4", 20, Some("bb"))
            )),
        );
        let result = merge_cases(&target, std::slice::from_ref(&source)).unwrap();
        assert_eq!(result.duplicate_records, 1);

        let rows = jsonl_ids(&target);
        assert_eq!(rows.len(), 3, "both records are kept");
        let dup: Vec<_> = rows
            .iter()
            .filter(|row| row.get("duplicate_of").is_some())
            .collect();
        assert_eq!(dup.len(), 1);
        assert_eq!(dup[0]["duplicate_of"].as_str(), Some("vid_000001"));
        assert_eq!(dup[0]["id"].as_str(), Some("vid_000002"));

        let _ = fs::remove_dir_all(&target);
        let _ = fs::remove_dir_all(&source);
    }

    #[test]
    fn failed_store_writes_never_append_merge_success_audit() {
        for blocked in [
            "db/videos.jsonl",
            "db/video_paths.tsv",
            "db/video_index.json",
            "db/case.db",
        ] {
            let name = blocked.replace('/', "-");
            let target = temp_case(&format!("failure-{name}-t"), None);
            let source = temp_case(
                &format!("failure-{name}-s"),
                Some(&row("vid_000001", "/ev/a.mp4", 10, None)),
            );
            fs::create_dir(target.join(blocked)).unwrap();
            let before = snapshot(&target);
            assert!(merge_cases(&target, std::slice::from_ref(&source)).is_err());
            assert!(!target.join("evidence/logs/case-merge-log.jsonl").exists());
            // I8: a failure at any store leaves every store as it was — no
            // half-written index set, no staging leftovers.
            assert_eq!(snapshot(&target), before, "{blocked}");
            let _ = fs::remove_dir_all(target);
            let _ = fs::remove_dir_all(source);
        }
    }

    #[test]
    fn merge_into_an_empty_index_creates_one() {
        let target = temp_case("empty-t", None);
        let source = temp_case(
            "empty-s",
            Some(&row("vid_000004", "/ev/x.mp4", 5, Some("xx"))),
        );
        let result = merge_cases(&target, std::slice::from_ref(&source)).unwrap();
        assert_eq!(result.total_records, 1);
        let rows = jsonl_ids(&target);
        assert_eq!(rows[0]["id"].as_str(), Some("vid_000001"));
        assert_eq!(rows[0]["merged_from_id"].as_str(), Some("vid_000004"));
        let _ = fs::remove_dir_all(&target);
        let _ = fs::remove_dir_all(&source);
    }

    fn annotate(
        case_dir: &Path,
        id: &str,
        status: &str,
        note: Option<&str>,
        range: Option<(f64, f64)>,
        tags: &[&str],
    ) {
        crate::case_db::patch_review_annotations_by(
            case_dir,
            &[crate::case_db::ReviewMarkRow {
                record_id: id.into(),
                status: status.into(),
                marked_unix: 42,
                record_path: None,
                examiner: Some("lee".into()),
                note: note.map(str::to_string),
                range: range.map(|(start, end)| crate::case_db::RangeSpan {
                    start: Some(start),
                    end: Some(end),
                }),
            }],
            &[crate::selection::TagEntry {
                id: id.into(),
                tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
            }],
            &[],
            Some("lee"),
        )
        .unwrap();
    }

    fn tags_of(case_dir: &Path, id: &str) -> Vec<String> {
        crate::case_db::load_review_tags(case_dir)
            .unwrap()
            .into_iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.tags)
            .unwrap_or_default()
    }

    fn mark_of(case_dir: &Path, id: &str) -> Option<crate::case_db::ReviewMarkRow> {
        crate::case_db::load_review_marks(case_dir)
            .unwrap()
            .into_iter()
            .find(|mark| mark.record_id == id)
    }

    fn source_case_string(source: &Path) -> String {
        crate::util::canonicalize_display(source)
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    fn audit_entries(case_dir: &Path) -> Vec<serde_json::Value> {
        read_to_string(&case_dir.join("evidence/logs/case-merge-log.jsonl"))
            .unwrap()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// I2: a merged record's mark, note, range, tags and history follow it
    /// to its renumbered id.
    #[test]
    fn merge_moves_marks_notes_ranges_tags_and_history_under_new_ids() {
        let target = temp_case(
            "ann-move-t",
            Some(&row("vid_000001", "/ev/a.mp4", 10, Some("aa"))),
        );
        let source = temp_case(
            "ann-move-s",
            Some(&row("vid_000001", "/ev/b.mp4", 20, Some("bb"))),
        );
        annotate(
            &source,
            "vid_000001",
            "relevant",
            Some("첫 메모"),
            None,
            &["사고"],
        );
        annotate(
            &source,
            "vid_000001",
            "important",
            Some("둘째 메모"),
            Some((1.5, 4.0)),
            &["사고", "보류"],
        );
        let source_history = crate::case_db::load_review_mark_history(&source, None).unwrap();
        assert_eq!(source_history.len(), 4, "{source_history:?}");

        let result = merge_cases(&target, std::slice::from_ref(&source)).unwrap();
        let counts = &result.sources[0].annotations;
        assert_eq!(counts.marks_merged, 1);
        assert_eq!(counts.tags_merged, 1);
        assert_eq!(counts.history_rows_copied, 4);
        assert_eq!(counts.marks_conflicted, 0);
        assert_eq!(counts.annotations_unmapped, 0);

        // Under the NEW id only; the original id never leaks into the target.
        assert!(mark_of(&target, "vid_000001").is_none());
        assert!(tags_of(&target, "vid_000001").is_empty());
        let merged = mark_of(&target, "vid_000002").unwrap();
        assert_eq!(merged.status, "important");
        assert_eq!(merged.note.as_deref(), Some("둘째 메모"));
        assert_eq!(merged.examiner.as_deref(), Some("lee"));
        assert_eq!(merged.marked_unix, 42);
        assert_eq!(merged.record_path.as_deref(), Some("/ev/b.mp4"));
        assert_eq!(
            merged.range,
            Some(crate::case_db::RangeSpan {
                start: Some(1.5),
                end: Some(4.0)
            })
        );
        assert_eq!(tags_of(&target, "vid_000002"), ["보류", "사고"]);

        let history =
            crate::case_db::load_review_mark_history(&target, Some("vid_000002")).unwrap();
        assert_eq!(history.len(), source_history.len() + 1);
        for (copied, original) in history.iter().zip(&source_history) {
            assert_eq!(copied.action, original.action);
            assert_eq!(copied.changed_unix, original.changed_unix);
            assert_eq!(copied.new_note, original.new_note);
            assert_eq!(copied.new_tags_json, original.new_tags_json);
        }
        assert_eq!(history.last().unwrap().action, "merge");
        let conn = crate::case_db::open_readonly_case_db(&target.join("db/case.db")).unwrap();
        let details: Vec<String> = conn
            .prepare("SELECT detail FROM review_mark_history WHERE record_id = 'vid_000002' ORDER BY history_pk")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let case = source_case_string(&source);
        assert!(details[0].starts_with(&format!(
            "merged-history from {case} record vid_000001 history_pk "
        )));
        assert!(
            details
                .last()
                .unwrap()
                .contains("\"original_id\":\"vid_000001\"")
        );

        // Marks stay a subset of indexed ids; stores agree.
        let ids = crate::case_db::load_video_ids(&target).unwrap();
        for mark in crate::case_db::load_review_marks(&target).unwrap() {
            assert!(ids.iter().any(|id| id.id == mark.record_id));
        }
        assert!(
            crate::qa::consistency_report(&target, &target.join("qa"))
                .unwrap()
                .passed
        );

        let audit = audit_entries(&target);
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0]["marks_merged"], 1);
        assert_eq!(audit[0]["tags_merged"], 1);
        assert_eq!(audit[0]["notes_appended"], 0);
        assert_eq!(audit[0]["marks_conflicted"], 0);
        assert_eq!(audit[0]["history_rows_copied"], 4);
        let _ = fs::remove_dir_all(&target);
        let _ = fs::remove_dir_all(&source);
    }

    fn conflict_cases(name: &str) -> (PathBuf, PathBuf) {
        let target = temp_case(
            &format!("{name}-t"),
            Some(&row("vid_000007", "/ev/shared.mp4", 10, Some("aa"))),
        );
        annotate(
            &target,
            "vid_000007",
            "relevant",
            Some("keep"),
            None,
            &["a"],
        );
        let source = temp_case(
            &format!("{name}-s"),
            Some(&row("vid_000001", "/ev/shared.mp4", 10, Some("aa"))),
        );
        annotate(
            &source,
            "vid_000001",
            "important",
            Some("src note\n둘째 줄"),
            Some((2.0, 3.0)),
            &["b", "a"],
        );
        (target, source)
    }

    /// I2: on an id the target already annotated, the target mark wins; the
    /// source note is appended once, tags are unioned, and the source mark
    /// survives verbatim in a `merge-skipped` history row.
    #[test]
    fn merge_conflict_keeps_target_mark_appends_note_unions_tags_and_keeps_source_in_history() {
        let (target, source) = conflict_cases("ann-conflict");
        let result = merge_cases(&target, std::slice::from_ref(&source)).unwrap();
        let counts = &result.sources[0].annotations;
        assert_eq!(counts.marks_merged, 0);
        assert_eq!(counts.marks_conflicted, 1);
        assert_eq!(counts.notes_appended, 1);
        assert_eq!(counts.tags_merged, 1);
        assert_eq!(counts.history_rows_copied, 2);

        let kept = mark_of(&target, "vid_000007").unwrap();
        assert_eq!(kept.status, "relevant");
        assert_eq!(kept.range, None);
        let case = source_case_string(&source);
        assert_eq!(
            kept.note.as_deref(),
            Some(format!("keep\n[병합 {case} vid_000001] src note\n둘째 줄").as_str())
        );
        assert_eq!(tags_of(&target, "vid_000007"), ["a", "b"]);
        assert!(mark_of(&target, "vid_000001").is_none());

        let history =
            crate::case_db::load_review_mark_history(&target, Some("vid_000007")).unwrap();
        let boundary = history.last().unwrap();
        assert_eq!(boundary.action, "merge-skipped");
        assert_eq!(boundary.old_note.as_deref(), Some("keep"));
        assert_eq!(boundary.new_note, kept.note);
        assert_eq!(boundary.new_status.as_deref(), Some("relevant"));
        assert_eq!(boundary.old_tags_json.as_deref(), Some("[\"a\"]"));
        assert_eq!(boundary.new_tags_json.as_deref(), Some("[\"a\",\"b\"]"));
        let conn = crate::case_db::open_readonly_case_db(&target.join("db/case.db")).unwrap();
        let detail: String = conn
            .query_row(
                "SELECT detail FROM review_mark_history WHERE action = 'merge-skipped'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let detail: serde_json::Value = serde_json::from_str(&detail).unwrap();
        assert_eq!(detail["source_case"], case.as_str());
        assert_eq!(detail["original_id"], "vid_000001");
        assert_eq!(detail["source_mark"]["status"], "important");
        assert_eq!(detail["source_mark"]["note"], "src note\n둘째 줄");
        assert_eq!(detail["source_mark"]["range"]["in"], 2.0);
        assert_eq!(audit_entries(&target)[0]["marks_conflicted"], 1);
        let _ = fs::remove_dir_all(&target);
        let _ = fs::remove_dir_all(&source);
    }

    /// I2 / review point 9: merging the same source again adds no second
    /// note segment, no duplicate history and no tag change.
    #[test]
    fn merge_twice_is_idempotent_for_annotations() {
        let (target, source) = conflict_cases("ann-twice");
        // Plus a record that is new to the target on the first merge and
        // "kept" (same path) on the second.
        let mut lines = read_to_string(&source.join("db/videos.jsonl")).unwrap();
        lines.push_str(&row("vid_000002", "/ev/new.mp4", 30, Some("cc")));
        fs::write(source.join("db/videos.jsonl"), lines).unwrap();
        annotate(
            &source,
            "vid_000002",
            "important",
            Some("new note"),
            Some((1.0, 2.0)),
            &["x"],
        );

        merge_cases(&target, std::slice::from_ref(&source)).unwrap();
        let marks = crate::case_db::load_review_marks(&target).unwrap();
        let tags = crate::case_db::load_review_tags(&target).unwrap();
        let history = crate::case_db::load_review_mark_history(&target, None).unwrap();

        let again = merge_cases(&target, std::slice::from_ref(&source)).unwrap();
        let counts = &again.sources[0].annotations;
        assert_eq!(counts.marks_merged, 0);
        assert_eq!(counts.notes_appended, 0);
        assert_eq!(counts.tags_merged, 0);
        assert_eq!(counts.history_rows_copied, 0);
        assert_eq!(crate::case_db::load_review_marks(&target).unwrap(), marks);
        assert_eq!(crate::case_db::load_review_tags(&target).unwrap(), tags);
        assert_eq!(
            crate::case_db::load_review_mark_history(&target, None).unwrap(),
            history
        );
        let note = mark_of(&target, "vid_000007").unwrap().note.unwrap();
        assert_eq!(note.matches("[병합 ").count(), 1, "{note}");
        let _ = fs::remove_dir_all(&target);
        let _ = fs::remove_dir_all(&source);
    }

    /// I2: one transaction — a failing history write leaves every target
    /// mark and tag as it was and appends no success audit line.
    #[test]
    fn merge_annotation_write_failure_rolls_back_everything() {
        let (target, source) = conflict_cases("ann-rollback");
        let mut lines = read_to_string(&source.join("db/videos.jsonl")).unwrap();
        lines.push_str(&row("vid_000002", "/ev/new.mp4", 30, Some("cc")));
        fs::write(source.join("db/videos.jsonl"), lines).unwrap();
        annotate(&source, "vid_000002", "important", Some("n"), None, &["x"]);
        let marks = crate::case_db::load_review_marks(&target).unwrap();
        let tags = crate::case_db::load_review_tags(&target).unwrap();
        let history = crate::case_db::load_review_mark_history(&target, None).unwrap();
        let conn = rusqlite::Connection::open(target.join("db/case.db")).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER fail_history BEFORE INSERT ON review_mark_history BEGIN SELECT RAISE(ABORT, 'induced history failure'); END;",
        )
        .unwrap();
        drop(conn);

        let index_before = index_files(&target);
        let ids_before = crate::case_db::load_video_ids(&target).unwrap();

        let err = merge_cases(&target, std::slice::from_ref(&source)).unwrap_err();
        assert!(err.contains("induced history failure"), "{err}");
        assert!(err.contains("rolled back"), "{err}");
        // I8: the rows upserted before the annotation step are rolled back
        // with it, and no index file was replaced.
        assert_eq!(index_files(&target), index_before);
        assert_eq!(
            crate::case_db::load_video_ids(&target).unwrap().len(),
            ids_before.len()
        );
        assert!(no_staging_left(&target));
        assert_eq!(crate::case_db::load_review_marks(&target).unwrap(), marks);
        assert_eq!(crate::case_db::load_review_tags(&target).unwrap(), tags);
        assert_eq!(
            crate::case_db::load_review_mark_history(&target, None).unwrap(),
            history
        );
        assert!(!target.join("evidence/logs/case-merge-log.jsonl").exists());
        let _ = fs::remove_dir_all(&target);
        let _ = fs::remove_dir_all(&source);
    }

    /// §10.2: every recorded spelling of the source case is the display
    /// path — never the Windows verbatim (`\\?\`) form canonicalize returns.
    #[test]
    fn merge_records_source_display_path_without_verbatim_prefix() {
        let (target, source) = conflict_cases("ann-display");
        let mut lines = read_to_string(&source.join("db/videos.jsonl")).unwrap();
        lines.push_str(&row("vid_000002", "/ev/new.mp4", 30, Some("cc")));
        fs::write(source.join("db/videos.jsonl"), lines).unwrap();
        let result = merge_cases(&target, std::slice::from_ref(&source)).unwrap();
        let display = source_case_string(&source);
        assert!(!display.starts_with(r"\\?\"), "{display}");
        assert_eq!(result.sources[0].case_dir.to_string_lossy(), display);
        let rows = jsonl_ids(&target);
        let merged = rows.iter().find(|r| r["merged_from"].is_string()).unwrap();
        assert_eq!(merged["merged_from"], display.as_str());
        let kept = rows
            .iter()
            .find(|r| r["source_path"] == "/ev/shared.mp4")
            .unwrap();
        assert_eq!(kept["merge_provenance"][0]["source_case"], display.as_str());
        assert_eq!(audit_entries(&target)[0]["source_case"], display.as_str());
        let note = mark_of(&target, "vid_000007").unwrap().note.unwrap();
        assert!(
            note.contains(&format!("[병합 {display} vid_000001]")),
            "{note}"
        );
        let everything = read_to_string(&target.join("db/videos.jsonl")).unwrap()
            + &read_to_string(&target.join("evidence/logs/case-merge-log.jsonl")).unwrap()
            + &note;
        assert!(!everything.contains(r"\\?\"), "{everything}");
        assert!(!everything.contains(r"\\\\?\\"), "{everything}");
        let _ = fs::remove_dir_all(&target);
        let _ = fs::remove_dir_all(&source);
    }

    /// §10.2: the "already appended" check matches whole lines. A note that
    /// is a prefix of an earlier appended segment is still appended.
    #[test]
    fn merge_note_duplicate_check_matches_whole_lines_only() {
        let (target, source) = conflict_cases("ann-lines");
        let case = source_case_string(&source);
        // The target already carries a LONGER segment for the same source id.
        annotate(
            &target,
            "vid_000007",
            "relevant",
            Some(&format!(
                "keep\n[병합 {case} vid_000001] src note\n둘째 줄 (수정)"
            )),
            None,
            &["a"],
        );
        let result = merge_cases(&target, std::slice::from_ref(&source)).unwrap();
        assert_eq!(result.sources[0].annotations.notes_appended, 1);
        let note = mark_of(&target, "vid_000007").unwrap().note.unwrap();
        assert_eq!(
            note,
            format!(
                "keep\n[병합 {case} vid_000001] src note\n둘째 줄 (수정)\n[병합 {case} vid_000001] src note\n둘째 줄"
            )
        );
        // Now the exact segment is present as whole lines: no second append.
        let again = merge_cases(&target, std::slice::from_ref(&source)).unwrap();
        assert_eq!(again.sources[0].annotations.notes_appended, 0);
        assert_eq!(mark_of(&target, "vid_000007").unwrap().note.unwrap(), note);
        let _ = fs::remove_dir_all(&target);
        let _ = fs::remove_dir_all(&source);
    }

    #[test]
    fn rejects_merging_a_case_into_itself_and_indexless_sources() {
        let target = temp_case(
            "rej-t",
            Some(&row("vid_000001", "/ev/a.mp4", 10, Some("aa"))),
        );
        let err = merge_cases(&target, std::slice::from_ref(&target)).unwrap_err();
        assert!(err.contains("into itself"), "{err}");

        let no_index = temp_case("rej-empty", None);
        let err = merge_cases(&target, std::slice::from_ref(&no_index)).unwrap_err();
        assert!(err.contains("no video index"), "{err}");

        let _ = fs::remove_dir_all(&target);
        let _ = fs::remove_dir_all(&no_index);
    }
}
