use crate::case_db::{self, IndexedVideoRow};
use crate::checkpoint::{self, ResumeMode, RunCheckpoint};
use crate::detector;
use crate::ffprobe;
use crate::model::{ProbeSummary, ScanOptions, ScanResult, VideoRecord};
use crate::sha256;
use crate::util::{
    canonicalize_display, json_escape, now_unix, path_to_file_url, read_to_string, unique_path,
    write_text_atomic,
};
use std::collections::{HashMap, VecDeque};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, ErrorKind};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

mod candidates;
mod index_merge;
mod index_rows;
mod json_line;
#[cfg(test)]
mod tests;

use candidates::*;
use index_merge::*;
pub use index_rows::{IndexRow, IndexRows, read_index_rows};
pub(crate) use json_line::tsv_escape;
use json_line::*;
pub use json_line::{json_record_lines, set_json_field, try_set_json_field};

const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mov", "m4v", "avi", "mkv", "wmv", "asf", "mpg", "mpeg", "mts", "m2ts", "ts", "3gp",
    "webm", "flv", "dav", "dav_", "nov", "ave", "g64", "g64x", "glv", "blk", "264", "265", "h264",
    "h265", "hevc",
];

/// Called with `(processed, total)` file counts; `None` disables mid-run
/// progress reporting (tests and headless callers). `total` is only known
/// after candidate collection, so it rides along on every tick.
pub type ScanProgress<'a> = dyn Fn(u64, u64) + Send + Sync + 'a;

pub fn scan_folder(
    case_dir: &Path,
    source_dir: &Path,
    options: &ScanOptions,
    resume: ResumeMode,
    progress: Option<&ScanProgress<'_>>,
) -> Result<ScanResult, String> {
    let source_dir = canonicalize_display(source_dir)
        .map_err(|err| format!("failed to canonicalize source: {err}"))?;
    let excluded_dirs = excluded_case_dirs(case_dir, &source_dir)?;
    let collection = collect_video_candidates(&source_dir, options.max_depth, &excluded_dirs)?;
    // An interrupted run checkpoints one record line per processed file
    // under db/scan-progress.jsonl; when the input fingerprint (source +
    // options + candidate list) still matches, those records are replayed
    // verbatim instead of re-hashing and re-probing gigabytes of evidence.
    let mut checkpoint = RunCheckpoint::begin(
        &case_dir.join("db/scan-progress.jsonl"),
        "scan-folder",
        &scan_fingerprint(&source_dir, options, &collection.files),
        resume,
    )?;
    let resumed_records = load_checkpoint_records(&checkpoint)?;
    let mut id_registry = load_existing_video_ids(case_dir)?;
    // The interrupted run already consumed these ids — re-registering them
    // keeps replayed records stable and stops fresh files from colliding.
    for record in resumed_records.values() {
        id_registry
            .ids_by_source
            .insert(source_key(&record.source_path), record.id.clone());
        if let Some(number) = record.id.strip_prefix("vid_").and_then(parse_usize) {
            id_registry.next_number = id_registry.next_number.max(number + 1);
        }
    }
    // Incremental mode: the existing index is the skip oracle. A record is
    // reused verbatim (never re-hashed or re-probed) when its stored
    // size+mtime still match the file on disk; merge_existing_with_scan then
    // carries the untouched record forward and marks missing paths stale.
    //
    // The existing index is also loaded outside incremental mode: per-file
    // facts a pass does not compute itself (a SHA-256 when run without
    // --hash, ffprobe metadata when run with --no-ffprobe) are carried from
    // the indexed record whose size+mtime still match, so a lighter rescan
    // never downgrades a hashed record to `hash_status=skipped` (see
    // `carried_facts`).
    let existing_index: HashMap<String, IndexedRecordLine> = load_existing_record_lines(case_dir)?
        .into_iter()
        .map(|line| (line.key(), line))
        .collect();
    // A prior checkpoint discarded for different inputs (e.g. a cancelled
    // `--incremental --hash` enrichment followed by a lighter phase-1 scan)
    // still holds hashes/probes computed moments ago; salvage them per file
    // instead of re-hashing gigabytes of evidence.
    let salvaged_records = salvage_checkpoint_records(&checkpoint);
    let total_files = collection.files.len() as u64;
    let mut records = Vec::with_capacity(collection.files.len());
    let mut unchanged_files = 0usize;
    let mut resumed_from_checkpoint = 0usize;
    let mut processed_files = 0u64;
    let mut total_bytes = 0u64;
    let mut warnings = collection.warnings;
    let mut lossy_display_owner: HashMap<String, String> = HashMap::new();
    if checkpoint.reset_stale() {
        warnings.push(
            "previous scan checkpoint was recorded for different inputs; started fresh".to_string(),
        );
    }

    for path in collection.files {
        processed_files += 1;
        // Report every 64 files (and once at the start) — frequent enough
        // for a live progress bar, rare enough to keep SQLite writes off
        // the hot path on 10k-file scans.
        if let Some(report) = progress
            && (processed_files % 64 == 1 || processed_files == total_files)
        {
            report(processed_files, total_files);
        }
        // One unreadable file (bad sector, permission, vanished) must not
        // abort the whole scan and leave no index: record it and move on.
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(err) => {
                // An already-indexed file whose metadata cannot be read is
                // still present: merge_existing_with_scan carries its record
                // forward unchanged rather than marking it stale.
                let key = source_key(&path);
                if id_registry.ids_by_source.contains_key(&key) {
                    warnings.push(format!(
                        "metadata-error: {} ({err}); existing index record carried forward unchanged",
                        path.display()
                    ));
                } else {
                    warnings.push(format!(
                        "skipped unreadable file {} (metadata error: {err}); not indexed",
                        path.display()
                    ));
                }
                continue;
            }
        };
        total_bytes = total_bytes.saturating_add(metadata.len());
        let modified_unix = modified_unix(&metadata);
        let source_key = source_key(&path);
        // E1: distinct non-Unicode names that render identically keep
        // distinct ids and records, but the examiner is told their
        // displayed paths are ambiguous.
        if path.to_str().is_none() {
            let display = path.to_string_lossy().to_string();
            match lossy_display_owner.get(&display) {
                Some(first) if first != &source_key => warnings.push(format!(
                    "non-unicode-name-collision: {display} names more than one file (raw names differ); each keeps its own id, and source_path_raw_hex in db/videos.jsonl records the exact name"
                )),
                Some(_) => {}
                None => {
                    lossy_display_owner.insert(display, source_key.clone());
                }
            }
        }
        // Replay the interrupted run's record verbatim — but only while the
        // file's size+mtime still match it, so a file changed between the
        // crash and the resume is reprocessed rather than trusted stale.
        if let Some(existing) = resumed_records.get(&source_key)
            && existing.size_bytes == metadata.len()
            && existing.modified_unix == modified_unix
        {
            resumed_from_checkpoint += 1;
            records.push(existing.clone());
            continue;
        }
        let salvaged = salvaged_records
            .get(&source_key)
            .filter(|record| same_file_state(record, metadata.len(), modified_unix));
        // E2: a record marked stale (e.g. its drive was unplugged during an
        // earlier rescan) still donates its facts once the file is back
        // with the same size+mtime — the stale marker says "was absent",
        // not "these bytes are different". Dropping the filter that
        // excluded flagged lines keeps a lighter rescan from turning the
        // recorded SHA-256 into `hash_status=skipped`. (record_is_unchanged
        // still refuses to skip a flagged record, so the marker clears.)
        let indexed = existing_index
            .get(&source_key)
            .map(|line| &line.record)
            .filter(|record| same_file_state(record, metadata.len(), modified_unix));
        if options.incremental
            && let Some(existing) = existing_index.get(&source_key)
            && record_is_unchanged(existing, metadata.len(), modified_unix, options)
            && !salvaged.is_some_and(|donor| adds_facts(donor, &existing.record))
        {
            unchanged_files += 1;
            continue;
        }
        let carried = carried_facts(salvaged, indexed, options);
        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let relative_path = path
            .strip_prefix(&source_dir)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();

        let (sha256, hash_status) = if let Some(donor) = carried.hash {
            (donor.sha256.clone(), donor.hash_status.clone())
        } else if options.hash_files {
            let (sha256, status, warning) = hash_with_restat(&path, &metadata);
            if let Some(warning) = warning {
                warnings.push(warning);
            }
            (sha256, status.to_string())
        } else {
            (None, "skipped".to_string())
        };

        let probe = if let Some(donor) = carried.probe {
            donor.probe.clone()
        } else if options.use_ffprobe {
            ffprobe::probe(&path)
        } else {
            ProbeSummary::skipped()
        };

        let confidence = classify_confidence(&extension, &probe);
        let source_profile = detector::detect_source_profile(
            &relative_path,
            &extension,
            probe.format_name.as_deref(),
        );
        let id = id_registry.id_for(&path);
        let record = VideoRecord {
            id,
            source_path: path,
            relative_path,
            extension,
            size_bytes: metadata.len(),
            modified_unix,
            sha256,
            hash_status,
            probe,
            confidence,
            source_profile,
        };
        // Checkpoint BEFORE the record goes into the in-memory list: a
        // crash after this line replays the record, a crash before it
        // simply reprocesses the file.
        checkpoint.append_line(&record.to_json())?;
        records.push(record);
    }

    let result = ScanResult {
        source_path: source_dir,
        scanned_unix: now_unix()?,
        video_count: records.len(),
        total_bytes,
        unchanged_files,
        resumed_from_checkpoint,
        warnings,
        options: options.clone(),
        records,
    };

    write_scan_outputs(case_dir, &result)?;
    if resumed_from_checkpoint > 0 {
        let line = format!(
            "{{\"schema_version\":1,\"event\":\"scan-resume\",\"run_id\":\"{}\",\"source_path\":\"{}\",\"resumed_records\":{}}}",
            json_escape(checkpoint.run_id()),
            json_escape(&result.source_path.to_string_lossy()),
            resumed_from_checkpoint
        );
        crate::audit::append_chained_jsonl(&case_dir.join("evidence/logs/scan-log.jsonl"), &line)?;
    }
    checkpoint.finish()?;
    Ok(result)
}

fn write_scan_outputs(case_dir: &Path, result: &ScanResult) -> Result<(), String> {
    let run_path = case_dir
        .join("db/scan_runs")
        .join(format!("scan_{}.json", result.scanned_unix));
    let run_path = unique_path(&run_path);
    write_text_atomic(&run_path, &result.to_json())
        .map_err(|err| format!("failed to write scan run snapshot: {err}"))?;

    let merged_records = merge_existing_with_scan(case_dir, result)?;
    let index_json = scan_index_json(result, &merged_records);
    write_text_atomic(&case_dir.join("db/video_index.json"), &index_json)
        .map_err(|err| format!("failed to write video index: {err}"))?;

    let mut jsonl = String::new();
    let mut paths_tsv = String::from(
        "id\tsource_path\trelative_path\textension\tsize_bytes\tsha256\tvendor\tparser\tparser_confidence\n",
    );
    for record in &merged_records {
        jsonl.push_str(&record.json_line);
        jsonl.push('\n');
        paths_tsv.push_str(&record.to_tsv_row());
    }
    write_text_atomic(&case_dir.join("db/videos.jsonl"), &jsonl)
        .map_err(|err| format!("failed to write video jsonl: {err}"))?;
    write_text_atomic(&case_dir.join("db/video_paths.tsv"), &paths_tsv)
        .map_err(|err| format!("failed to write video path index: {err}"))?;

    let db_records = merged_records
        .iter()
        .map(IndexedRecordLine::to_db_row)
        .collect::<Vec<_>>();
    case_db::write_scan_index(case_dir, result, &db_records)?;
    Ok(())
}
