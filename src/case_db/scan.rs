use super::*;
use crate::model::{ScanResult, VideoRecord};
use crate::util::path_to_file_url;
use rusqlite::{Transaction, params};
use std::path::Path;

pub fn write_scan_index(
    case_dir: &Path,
    result: &ScanResult,
    merged_records: &[IndexedVideoRow],
) -> Result<(), String> {
    let mut conn = open_case_db(case_dir)?;
    init_schema(&conn)?;

    let tx = begin_write(&mut conn)
        .map_err(|err| format!("failed to start SQLite transaction: {err}"))?;
    insert_scan_run(&tx, result)?;
    for record in merged_records {
        upsert_indexed_record(&tx, record, result.scanned_unix)?;
    }
    for record in &result.records {
        upsert_scanned_record(&tx, record, result.scanned_unix)?;
    }
    tx.commit()
        .map_err(|err| format!("failed to commit SQLite index: {err}"))?;
    Ok(())
}

/// Upserts serialized index rows without inserting a scan run — used by
/// merge-cases, which rewrites the JSONL/index without a filesystem scan.
pub fn upsert_indexed_rows(
    case_dir: &Path,
    merged_records: &[IndexedVideoRow],
    indexed_unix: u64,
) -> Result<(), String> {
    let mut conn = open_case_db(case_dir)?;
    init_schema(&conn)?;
    let tx = begin_write(&mut conn)
        .map_err(|err| format!("failed to start SQLite transaction: {err}"))?;
    for record in merged_records {
        upsert_indexed_record(&tx, record, indexed_unix)?;
    }
    tx.commit()
        .map_err(|err| format!("failed to commit SQLite index merge: {err}"))?;
    Ok(())
}

pub fn load_video_record_lines(case_dir: &Path) -> Result<Vec<String>, String> {
    let path = case_db_path(case_dir);
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let conn = open_readonly_case_db(&path)?;
    if !table_exists(&conn, "videos")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare("SELECT record_json FROM videos ORDER BY id")
        .map_err(|err| format!("failed to prepare SQLite records query: {err}"))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|err| format!("failed to query SQLite records: {err}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|err| format!("failed to read SQLite records: {err}"))
}

/// `(matched_ids, indexed_ids)` for `/api/records?q=`: ids whose FTS
/// haystack contains the query, plus every id present in the videos
/// table. The second set distinguishes "indexed but no match" from "not
/// DB-indexed" (carved/fs records), which still deserve the plain
/// substring path.
pub type FtsSearchHit = (
    std::collections::HashSet<String>,
    std::collections::HashSet<String>,
);

/// FTS5 substring lookup for `/api/records?q=`. Returns `None` when the
/// case db or index is unavailable so the caller falls back to a full
/// substring scan without treating it as zero hits.
pub fn fts_search_video_ids(case_dir: &Path, query: &str) -> Result<Option<FtsSearchHit>, String> {
    let path = case_db_path(case_dir);
    if !path.is_file() || query.len() < 3 {
        return Ok(None);
    }
    let conn = open_readonly_case_db(&path)?;
    if !table_exists(&conn, "videos_fts")? || !table_exists(&conn, "videos")? {
        return Ok(None);
    }
    // Trigram MATCH needs the query as one quoted phrase so embedded
    // whitespace/punctuation stays a literal substring, and queries
    // shorter than three bytes can never form a trigram.
    let fts_query = format!("\"{}\"", query.replace('"', "\"\""));
    let mut stmt = conn
        .prepare("SELECT id FROM videos_fts WHERE videos_fts MATCH ?1")
        .map_err(|err| format!("failed to prepare FTS query: {err}"))?;
    let matched = stmt
        .query_map([fts_query], |row| row.get::<_, String>(0))
        .map_err(|err| format!("failed to run FTS query: {err}"))?
        .collect::<Result<std::collections::HashSet<_>, _>>()
        .map_err(|err| format!("failed to read FTS matches: {err}"))?;
    let mut stmt = conn
        .prepare("SELECT id FROM videos")
        .map_err(|err| format!("failed to prepare id listing: {err}"))?;
    let indexed = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|err| format!("failed to list indexed ids: {err}"))?
        .collect::<Result<std::collections::HashSet<_>, _>>()
        .map_err(|err| format!("failed to read indexed ids: {err}"))?;
    Ok(Some((matched, indexed)))
}

pub fn load_video_ids(case_dir: &Path) -> Result<Vec<VideoIdRow>, String> {
    let path = case_db_path(case_dir);
    if !path.is_file() {
        return Ok(Vec::new());
    }

    let conn = open_readonly_case_db(&path)?;
    if !table_exists(&conn, "videos")? {
        return Ok(Vec::new());
    }

    let mut stmt = conn
        .prepare("SELECT id, source_path, sha256 FROM videos ORDER BY id")
        .map_err(|err| format!("failed to prepare SQLite video id query: {err}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(VideoIdRow {
                id: row.get(0)?,
                source_path: row.get(1)?,
                sha256: row.get(2)?,
            })
        })
        .map_err(|err| format!("failed to query SQLite video ids: {err}"))?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|err| format!("failed to read SQLite video id row: {err}"))?);
    }
    Ok(out)
}

pub(crate) fn insert_scan_run(tx: &Transaction<'_>, result: &ScanResult) -> Result<(), String> {
    tx.execute(
        r#"
        INSERT INTO scan_runs (
            source_path,
            scanned_unix,
            hash_files,
            use_ffprobe,
            max_depth,
            video_count,
            total_bytes,
            warnings_json
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
        "#,
        params![
            result.source_path.to_string_lossy().to_string(),
            u64_to_i64(result.scanned_unix),
            bool_to_i64(result.options.hash_files),
            bool_to_i64(result.options.use_ffprobe),
            result.options.max_depth.map(usize_to_i64),
            usize_to_i64(result.video_count),
            u64_to_i64(result.total_bytes),
            string_array_json(&result.warnings),
        ],
    )
    .map_err(|err| format!("failed to insert SQLite scan run: {err}"))?;
    Ok(())
}

pub(crate) fn upsert_indexed_record(
    tx: &Transaction<'_>,
    record: &IndexedVideoRow,
    indexed_unix: u64,
) -> Result<(), String> {
    tx.execute(
        r#"
        INSERT INTO videos (
            id,
            source_path,
            file_url,
            relative_path,
            extension,
            size_bytes,
            modified_unix,
            sha256,
            hash_status,
            confidence,
            source_profile_json,
            duration_seconds,
            format_name,
            video_codec,
            audio_codec,
            width,
            height,
            ffprobe_ok,
            ffprobe_error,
            ffprobe_json,
            first_indexed_unix,
            last_indexed_unix,
            last_scanned_unix,
            record_json
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, NULL, ?23)
        ON CONFLICT(id) DO UPDATE SET
            source_path = excluded.source_path,
            file_url = excluded.file_url,
            relative_path = excluded.relative_path,
            extension = excluded.extension,
            size_bytes = excluded.size_bytes,
            modified_unix = excluded.modified_unix,
            sha256 = excluded.sha256,
            hash_status = excluded.hash_status,
            confidence = excluded.confidence,
            source_profile_json = excluded.source_profile_json,
            duration_seconds = excluded.duration_seconds,
            format_name = excluded.format_name,
            video_codec = excluded.video_codec,
            audio_codec = excluded.audio_codec,
            width = excluded.width,
            height = excluded.height,
            ffprobe_ok = excluded.ffprobe_ok,
            ffprobe_error = excluded.ffprobe_error,
            ffprobe_json = excluded.ffprobe_json,
            last_indexed_unix = excluded.last_indexed_unix,
            record_json = excluded.record_json
        ON CONFLICT(source_path) DO UPDATE SET
            id = excluded.id,
            file_url = excluded.file_url,
            relative_path = excluded.relative_path,
            extension = excluded.extension,
            size_bytes = excluded.size_bytes,
            modified_unix = excluded.modified_unix,
            sha256 = excluded.sha256,
            hash_status = excluded.hash_status,
            confidence = excluded.confidence,
            source_profile_json = excluded.source_profile_json,
            duration_seconds = excluded.duration_seconds,
            format_name = excluded.format_name,
            video_codec = excluded.video_codec,
            audio_codec = excluded.audio_codec,
            width = excluded.width,
            height = excluded.height,
            ffprobe_ok = excluded.ffprobe_ok,
            ffprobe_error = excluded.ffprobe_error,
            ffprobe_json = excluded.ffprobe_json,
            last_indexed_unix = excluded.last_indexed_unix,
            record_json = excluded.record_json
        "#,
        params![
            record.id.as_str(),
            record.source_path.as_str(),
            record.file_url.as_str(),
            record.relative_path.as_str(),
            record.extension.as_str(),
            u64_to_i64(record.size_bytes),
            record.modified_unix.map(u64_to_i64),
            record.sha256.as_deref(),
            record.hash_status.as_str(),
            record.confidence.as_str(),
            record.source_profile_json.as_str(),
            record.duration_seconds,
            record.format_name.as_deref(),
            record.video_codec.as_deref(),
            record.audio_codec.as_deref(),
            record.width.map(u64_to_i64),
            record.height.map(u64_to_i64),
            bool_to_i64(record.ffprobe_ok),
            record.ffprobe_error.as_deref(),
            record.ffprobe_json.as_deref(),
            u64_to_i64(indexed_unix),
            u64_to_i64(indexed_unix),
            record.record_json.as_str(),
        ],
    )
    .map_err(|err| format!("failed to upsert SQLite indexed video {}: {err}", record.id))?;
    Ok(())
}

pub(crate) fn upsert_scanned_record(
    tx: &Transaction<'_>,
    record: &VideoRecord,
    scanned_unix: u64,
) -> Result<(), String> {
    tx.execute(
        r#"
        INSERT INTO videos (
            id,
            source_path,
            file_url,
            relative_path,
            extension,
            size_bytes,
            modified_unix,
            sha256,
            hash_status,
            confidence,
            source_profile_json,
            duration_seconds,
            format_name,
            video_codec,
            audio_codec,
            width,
            height,
            ffprobe_ok,
            ffprobe_error,
            ffprobe_json,
            first_indexed_unix,
            last_indexed_unix,
            last_scanned_unix,
            record_json
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)
        ON CONFLICT(id) DO UPDATE SET
            source_path = excluded.source_path,
            file_url = excluded.file_url,
            relative_path = excluded.relative_path,
            extension = excluded.extension,
            size_bytes = excluded.size_bytes,
            modified_unix = excluded.modified_unix,
            sha256 = excluded.sha256,
            hash_status = excluded.hash_status,
            confidence = excluded.confidence,
            source_profile_json = excluded.source_profile_json,
            duration_seconds = excluded.duration_seconds,
            format_name = excluded.format_name,
            video_codec = excluded.video_codec,
            audio_codec = excluded.audio_codec,
            width = excluded.width,
            height = excluded.height,
            ffprobe_ok = excluded.ffprobe_ok,
            ffprobe_error = excluded.ffprobe_error,
            ffprobe_json = excluded.ffprobe_json,
            last_indexed_unix = excluded.last_indexed_unix,
            last_scanned_unix = excluded.last_scanned_unix,
            record_json = excluded.record_json
        ON CONFLICT(source_path) DO UPDATE SET
            id = excluded.id,
            file_url = excluded.file_url,
            relative_path = excluded.relative_path,
            extension = excluded.extension,
            size_bytes = excluded.size_bytes,
            modified_unix = excluded.modified_unix,
            sha256 = excluded.sha256,
            hash_status = excluded.hash_status,
            confidence = excluded.confidence,
            source_profile_json = excluded.source_profile_json,
            duration_seconds = excluded.duration_seconds,
            format_name = excluded.format_name,
            video_codec = excluded.video_codec,
            audio_codec = excluded.audio_codec,
            width = excluded.width,
            height = excluded.height,
            ffprobe_ok = excluded.ffprobe_ok,
            ffprobe_error = excluded.ffprobe_error,
            ffprobe_json = excluded.ffprobe_json,
            last_indexed_unix = excluded.last_indexed_unix,
            last_scanned_unix = excluded.last_scanned_unix,
            record_json = excluded.record_json
        "#,
        params![
            record.id.as_str(),
            record.source_path.to_string_lossy().to_string(),
            path_to_file_url(&record.source_path),
            record.relative_path.as_str(),
            record.extension.as_str(),
            u64_to_i64(record.size_bytes),
            record.modified_unix.map(u64_to_i64),
            record.sha256.as_deref(),
            record.hash_status.as_str(),
            record.confidence.as_str(),
            record.source_profile.to_json(),
            record.probe.duration_seconds,
            record.probe.format_name.as_deref(),
            record.probe.video_codec.as_deref(),
            record.probe.audio_codec.as_deref(),
            record.probe.width.map(u32_to_i64),
            record.probe.height.map(u32_to_i64),
            bool_to_i64(record.probe.ok),
            record.probe.error.as_deref(),
            record.probe.raw_json.as_deref(),
            u64_to_i64(scanned_unix),
            u64_to_i64(scanned_unix),
            u64_to_i64(scanned_unix),
            record.to_json(),
        ],
    )
    .map_err(|err| format!("failed to upsert SQLite scanned video {}: {err}", record.id))?;
    Ok(())
}
