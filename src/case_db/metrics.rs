use super::*;
use crate::util::{json_escape, now_unix};
use rusqlite::Connection;
use std::fs;
use std::path::Path;
use std::time::Instant;

pub fn benchmark_case_db(output_dir: &Path, rows: usize) -> Result<DbBenchmarkResult, String> {
    fs::create_dir_all(output_dir)
        .map_err(|err| format!("failed to create benchmark directory: {err}"))?;
    let mut conn = open_case_db(output_dir)?;
    init_schema(&conn)?;
    let started = Instant::now();
    let now = now_unix()?;
    let tx = begin_write(&mut conn)
        .map_err(|err| format!("failed to start benchmark transaction: {err}"))?;
    for index in 0..rows {
        let id = format!("bench_{index:08}");
        let source_path = format!("C:/Evidence/bench/clip_{index:08}.mp4");
        let record = IndexedVideoRow {
            id,
            source_path: source_path.clone(),
            file_url: format!("file:///{source_path}"),
            relative_path: format!("clip_{index:08}.mp4"),
            extension: "mp4".to_string(),
            size_bytes: 1024 + index as u64,
            modified_unix: None,
            sha256: None,
            hash_status: "benchmark".to_string(),
            confidence: "benchmark".to_string(),
            source_profile_json: "{\"lane\":\"benchmark\",\"vendor\":\"Benchmark\",\"parser\":\"benchmark\",\"confidence\":\"benchmark\",\"recommended_action\":\"Synthetic row only\",\"evidence\":[]}".to_string(),
            duration_seconds: None,
            format_name: None,
            video_codec: None,
            audio_codec: None,
            width: None,
            height: None,
            ffprobe_ok: false,
            ffprobe_error: None,
            ffprobe_json: None,
            record_json: format!(
                "{{\"id\":\"bench_{index:08}\",\"source_path\":\"{}\",\"relative_path\":\"clip_{index:08}.mp4\",\"extension\":\"mp4\",\"size_bytes\":{},\"source_profile\":{{\"vendor\":\"Benchmark\",\"parser\":\"benchmark\"}}}}",
                json_escape(&source_path),
                1024 + index as u64
            ),
        };
        upsert_indexed_record(&tx, &record, now)?;
    }
    tx.commit()
        .map_err(|err| format!("failed to commit benchmark transaction: {err}"))?;
    Ok(DbBenchmarkResult {
        path: case_db_path(output_dir),
        rows,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

pub fn summarize_case_db(case_dir: &Path) -> Result<Option<CaseDbSummary>, String> {
    let path = case_db_path(case_dir);
    if !path.is_file() {
        return Ok(None);
    }

    let conn = open_readonly_case_db(&path)?;
    if !table_exists(&conn, "videos")? {
        return Ok(Some(CaseDbSummary {
            path,
            video_count: 0,
            scan_run_count: 0,
            evidence_source_count: 0,
            job_count: 0,
            active_job_count: 0,
        }));
    }

    let video_count = count_table_rows(&conn, "videos")?;
    let scan_run_count = if table_exists(&conn, "scan_runs")? {
        count_table_rows(&conn, "scan_runs")?
    } else {
        0
    };
    let evidence_source_count = if table_exists(&conn, "evidence_sources")? {
        count_table_rows(&conn, "evidence_sources")?
    } else {
        0
    };
    let job_count = if table_exists(&conn, "jobs")? {
        count_table_rows(&conn, "jobs")?
    } else {
        0
    };
    let active_job_count = if table_exists(&conn, "jobs")? {
        count_jobs_by_status(&conn, "running")?
    } else {
        0
    };
    Ok(Some(CaseDbSummary {
        path,
        video_count,
        scan_run_count,
        evidence_source_count,
        job_count,
        active_job_count,
    }))
}

pub(crate) fn count_table_rows(conn: &Connection, table_name: &str) -> Result<u64, String> {
    let sql = match table_name {
        "videos" => "SELECT COUNT(*) FROM videos",
        "scan_runs" => "SELECT COUNT(*) FROM scan_runs",
        "evidence_sources" => "SELECT COUNT(*) FROM evidence_sources",
        "jobs" => "SELECT COUNT(*) FROM jobs",
        _ => return Err(format!("unsupported SQLite count table: {table_name}")),
    };
    let count: i64 = conn
        .query_row(sql, [], |row| row.get(0))
        .map_err(|err| format!("failed to count SQLite table {table_name}: {err}"))?;
    Ok(count.max(0) as u64)
}
