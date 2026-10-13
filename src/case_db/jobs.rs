use super::*;
use crate::util::now_unix;
use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Advisory lock files held for the lifetime of a running job. A job that
/// dies mid-run (killed CLI, crashed workstation step) leaves its
/// `running` row behind; the lock dies with the process, so the next job
/// start — or status poll — can prove the row is orphaned without parsing
/// PIDs. Entries live until process exit by design.
static JOB_LOCKS: Mutex<Vec<std::fs::File>> = Mutex::new(Vec::new());

fn job_lock_path(case_dir: &Path, job_id: &str) -> PathBuf {
    case_dir.join("db/job-locks").join(format!("{job_id}.lock"))
}

/// Marks `running` jobs as `interrupted` when no live process holds their
/// advisory lock. Best-effort: failures are ignored so telemetry repair
/// can never break evidence processing.
pub fn reap_interrupted_jobs(case_dir: &Path) {
    let Ok(conn) = open_case_db(case_dir) else {
        return;
    };
    if init_schema(&conn).is_err() {
        return;
    }
    let ids: Vec<String> = conn
        .prepare("SELECT job_id FROM jobs WHERE status = 'running'")
        .and_then(|mut stmt| {
            stmt.query_map([], |row| row.get(0))
                .map(|rows| rows.collect::<Result<Vec<_>, _>>())
        })
        .ok()
        .and_then(|rows| rows.ok())
        .unwrap_or_default();
    for job_id in ids {
        if !job_owner_alive(case_dir, &job_id) {
            let now = match now_unix() {
                Ok(now) => now,
                Err(_) => continue,
            };
            if conn
                .execute(
                    "UPDATE jobs SET status = 'interrupted', updated_unix = ?2, completed_unix = ?2, error = COALESCE(error, 'owning process exited without completing') WHERE job_id = ?1 AND status = 'running'",
                    params![job_id.as_str(), u64_to_i64(now)],
                )
                .is_ok()
            {
                let _ = append_job_event(
                    case_dir,
                    &job_id,
                    "interrupted",
                    "owning process exited; marked interrupted",
                    None,
                );
            }
        }
    }
}

/// Whether a live process still holds the advisory lock of `job_id`.
fn job_owner_alive(case_dir: &Path, job_id: &str) -> bool {
    let lock_path = job_lock_path(case_dir, job_id);
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
    {
        Ok(file) => {
            // If we can take the lock, its owner is gone — locks are
            // released by the OS on process exit (LockFileEx on Windows,
            // flock on Unix), even for killed processes.
            match file.try_lock_exclusive() {
                Ok(()) => {
                    let _ = file.unlock();
                    false
                }
                Err(_) => true,
            }
        }
        // No lock file means the job predates the lock mechanism or the
        // lock could not be created — treat as orphaned.
        Err(_) => false,
    }
}

/// R10: the `/api/status` progress read, cheap enough for a 1 s poll
/// while a carve/scan child writes the case DB. Unlike
/// [`latest_running_job`] it never writes — no reaping (an UPDATE that
/// queues behind the writer), no schema init (a BEGIN IMMEDIATE) — opens
/// the DB read-only, and waits at most `busy` for a lock. Rows whose owner
/// died are skipped (same lock-file proof the reaper uses) instead of
/// being rewritten here; the next job start reaps them. An error (DB busy
/// past `busy`, no jobs table yet) is returned for the caller to degrade.
///
/// The wait is a total deadline enforced here (SQLite's own busy handler
/// is re-armed per lock attempt — schema read, then query — and measured
/// ~4x its nominal timeout under an EXCLUSIVE writer on Windows).
pub fn latest_running_job_quick(
    case_dir: &Path,
    busy: std::time::Duration,
) -> Result<Option<JobProgress>, String> {
    let deadline = std::time::Instant::now() + busy;
    loop {
        match read_running_job_once(case_dir) {
            Err((true, _)) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err((_, err)) => return Err(err),
            Ok(job) => return Ok(job),
        }
    }
}

/// One non-blocking attempt of [`latest_running_job_quick`]; the error
/// says whether it was a lock conflict worth retrying.
fn read_running_job_once(case_dir: &Path) -> Result<Option<JobProgress>, (bool, String)> {
    let is_busy = |err: &rusqlite::Error| {
        matches!(
            err.sqlite_error_code(),
            Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
        )
    };
    let fail = |err: rusqlite::Error| {
        (
            is_busy(&err),
            format!("failed to read running job progress: {err}"),
        )
    };
    let conn = rusqlite::Connection::open_with_flags(
        case_db_path(case_dir),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(fail)?;
    conn.busy_timeout(std::time::Duration::ZERO).map_err(fail)?;
    let mut stmt = conn
        .prepare(
            r#"
            SELECT job_id, job_type, total_units, completed_units, started_unix, updated_unix
            FROM jobs
            WHERE status = 'running'
            ORDER BY started_unix DESC
            LIMIT 16
            "#,
        )
        .map_err(fail)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(JobProgress {
                job_id: row.get(0)?,
                job_type: row.get(1)?,
                total_units: row.get::<_, Option<i64>>(2)?.map(|v| v.max(0) as u64),
                completed_units: row.get::<_, i64>(3)?.max(0) as u64,
                started_unix: row.get::<_, i64>(4)?.max(0) as u64,
                updated_unix: row.get::<_, i64>(5)?.max(0) as u64,
            })
        })
        .map_err(fail)?;
    for row in rows {
        let job = row.map_err(fail)?;
        if job_owner_alive(case_dir, &job.job_id) {
            return Ok(Some(job));
        }
    }
    Ok(None)
}

pub fn start_job(
    case_dir: &Path,
    job_type: &str,
    subject_path: &Path,
    total_units: Option<u64>,
    options_json: &str,
) -> Result<JobRecord, String> {
    reap_interrupted_jobs(case_dir);
    let conn = open_case_db(case_dir)?;
    init_schema(&conn)?;
    let now = now_unix()?;
    let job_number = count_table_rows(&conn, "jobs")?.saturating_add(1);
    // Sub-second precision keeps concurrent workers in the same second from
    // generating the same primary key.
    let subsec_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| format!("system time before UNIX epoch: {err}"))?
        .subsec_nanos();
    let job_id = format!("job_{now}_{subsec_nanos:09}_{job_number:06}");
    conn.execute(
        r#"
        INSERT INTO jobs (
            job_id,
            job_type,
            status,
            subject_path,
            started_unix,
            updated_unix,
            total_units,
            completed_units,
            options_json
        )
        VALUES (?1, ?2, 'running', ?3, ?4, ?4, ?5, 0, ?6)
        "#,
        params![
            job_id.as_str(),
            job_type,
            subject_path.to_string_lossy().to_string(),
            u64_to_i64(now),
            total_units.map(u64_to_i64),
            options_json,
        ],
    )
    .map_err(|err| format!("failed to start SQLite job: {err}"))?;
    // Hold an exclusive advisory lock for the job's lifetime; if this
    // process dies the lock is released by the OS and the next caller's
    // reaper marks the orphaned row `interrupted`.
    let lock_dir = case_dir.join("db/job-locks");
    if std::fs::create_dir_all(&lock_dir).is_ok()
        && let Ok(file) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(job_lock_path(case_dir, &job_id))
        && file.try_lock_exclusive().is_ok()
        && let Ok(mut locks) = JOB_LOCKS.lock()
    {
        locks.push(file);
    }
    append_job_event(case_dir, &job_id, "started", "job started", Some(0))?;
    Ok(JobRecord {
        job_id,
        job_type: job_type.to_string(),
        status: "running".to_string(),
    })
}

pub fn update_job_progress(
    case_dir: &Path,
    job_id: &str,
    completed_units: u64,
    message: &str,
) -> Result<(), String> {
    let conn = open_case_db(case_dir)?;
    init_schema(&conn)?;
    let now = now_unix()?;
    conn.execute(
        "UPDATE jobs SET completed_units = ?1, updated_unix = ?2 WHERE job_id = ?3",
        params![u64_to_i64(completed_units), u64_to_i64(now), job_id],
    )
    .map_err(|err| format!("failed to update SQLite job progress: {err}"))?;
    append_job_event(case_dir, job_id, "progress", message, Some(completed_units))
}

/// Progress report that also fills in `total_units` once the engine knows
/// it (a scan only learns its file count after candidate collection, which
/// happens after `start_job`). Unlike `update_job_progress` this does NOT
/// append a `job_events` row — mid-run progress is polled via the `jobs`
/// row, so per-tick events would flood the event log on 10k-item runs.
pub fn report_job_progress(
    case_dir: &Path,
    job_id: &str,
    total_units: Option<u64>,
    completed_units: u64,
) -> Result<(), String> {
    let conn = open_case_db(case_dir)?;
    init_schema(&conn)?;
    let now = now_unix()?;
    conn.execute(
        r#"
        UPDATE jobs
        SET total_units = COALESCE(?1, total_units),
            completed_units = ?2,
            updated_unix = ?3
        WHERE job_id = ?4
        "#,
        params![
            total_units.map(u64_to_i64),
            u64_to_i64(completed_units),
            u64_to_i64(now),
            job_id,
        ],
    )
    .map_err(|err| format!("failed to report SQLite job progress: {err}"))?;
    Ok(())
}

/// The newest still-running job for a case, used by the examiner
/// workstation's status endpoint to render live progress + ETA. Progress
/// is best-effort: a job that never reports mid-run ticks simply shows no
/// bar, which the UI already handles.
pub fn latest_running_job(case_dir: &Path) -> Result<Option<JobProgress>, String> {
    // Reap first so a job whose owner died does not shadow live progress
    // (or stall the workstation's status display) indefinitely.
    reap_interrupted_jobs(case_dir);
    let conn = open_case_db(case_dir)?;
    init_schema(&conn)?;
    conn.query_row(
        r#"
        SELECT job_id, job_type, total_units, completed_units, started_unix, updated_unix
        FROM jobs
        WHERE status = 'running'
        ORDER BY started_unix DESC
        LIMIT 1
        "#,
        [],
        |row| {
            Ok(JobProgress {
                job_id: row.get(0)?,
                job_type: row.get(1)?,
                total_units: row.get::<_, Option<i64>>(2)?.map(|v| v.max(0) as u64),
                completed_units: row.get::<_, i64>(3)?.max(0) as u64,
                started_unix: row.get::<_, i64>(4)?.max(0) as u64,
                updated_unix: row.get::<_, i64>(5)?.max(0) as u64,
            })
        },
    )
    .optional()
    .map_err(|err| format!("failed to read running job progress: {err}"))
}

pub struct RunningJob {
    pub job_id: String,
    pub job_type: String,
    pub subject_path: String,
    pub started_unix: i64,
}

pub fn latest_running_jobs(case_dir: &Path) -> Result<Vec<RunningJob>, String> {
    reap_interrupted_jobs(case_dir);
    let conn = open_case_db(case_dir)?;
    init_schema(&conn)?;
    let mut stmt = conn
        .prepare(
            "SELECT job_id, job_type, subject_path, started_unix
             FROM jobs
             WHERE status = 'running'
             ORDER BY started_unix DESC",
        )
        .map_err(|err| format!("failed to read running jobs: {err}"))?;
    let mapped = stmt
        .query_map([], |row| {
            Ok(RunningJob {
                job_id: row.get(0)?,
                job_type: row.get(1)?,
                subject_path: row.get(2)?,
                started_unix: row.get(3)?,
            })
        })
        .map_err(|err| format!("failed to read running jobs: {err}"))?;
    let mut rows = Vec::new();
    for row in mapped {
        rows.push(row.map_err(|err| format!("failed to read running jobs: {err}"))?);
    }
    Ok(rows)
}

pub fn complete_job(
    case_dir: &Path,
    job_id: &str,
    completed_units: u64,
    message: &str,
) -> Result<(), String> {
    finish_job(case_dir, job_id, "complete", completed_units, message, None)
}

pub fn fail_job(case_dir: &Path, job_id: &str, error: &str) -> Result<(), String> {
    finish_job(case_dir, job_id, "failed", 0, "job failed", Some(error))
}

pub(crate) fn finish_job(
    case_dir: &Path,
    job_id: &str,
    status: &str,
    completed_units: u64,
    message: &str,
    error: Option<&str>,
) -> Result<(), String> {
    let conn = open_case_db(case_dir)?;
    init_schema(&conn)?;
    let now = now_unix()?;
    conn.execute(
        r#"
        UPDATE jobs
        SET status = ?1,
            updated_unix = ?2,
            completed_unix = ?2,
            completed_units = CASE WHEN ?3 > 0 THEN ?3 ELSE completed_units END,
            error = ?4
        WHERE job_id = ?5
        "#,
        params![
            status,
            u64_to_i64(now),
            u64_to_i64(completed_units),
            error,
            job_id,
        ],
    )
    .map_err(|err| format!("failed to finish SQLite job: {err}"))?;
    append_job_event(case_dir, job_id, status, message, Some(completed_units))
}

pub(crate) fn append_job_event(
    case_dir: &Path,
    job_id: &str,
    event_type: &str,
    message: &str,
    completed_units: Option<u64>,
) -> Result<(), String> {
    let conn = open_case_db(case_dir)?;
    init_schema(&conn)?;
    conn.execute(
        r#"
        INSERT INTO job_events (
            job_id,
            event_unix,
            event_type,
            message,
            completed_units
        )
        VALUES (?1, ?2, ?3, ?4, ?5)
        "#,
        params![
            job_id,
            u64_to_i64(now_unix()?),
            event_type,
            message,
            completed_units.map(u64_to_i64),
        ],
    )
    .map_err(|err| format!("failed to append SQLite job event: {err}"))?;
    Ok(())
}

pub(crate) fn count_jobs_by_status(conn: &Connection, status: &str) -> Result<u64, String> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM jobs WHERE status = ?1",
            [status],
            |row| row.get(0),
        )
        .map_err(|err| format!("failed to count SQLite jobs with status {status}: {err}"))?;
    Ok(count.max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::{latest_running_job, reap_interrupted_jobs, start_job, u64_to_i64};
    use crate::case_db::{init_schema, open_case_db};
    use rusqlite::params;
    use std::fs;
    use std::path::Path;

    #[test]
    fn reaper_marks_orphaned_running_job_interrupted() {
        let case_dir =
            std::env::temp_dir().join(format!("frametrace-job-reap-{}", std::process::id()));
        let _ = fs::remove_dir_all(&case_dir);
        fs::create_dir_all(case_dir.join("db")).unwrap();

        // A live job holds its lock in this process — the reaper must not
        // touch it.
        let live = start_job(&case_dir, "scan-folder", Path::new("/evidence"), None, "{}").unwrap();
        reap_interrupted_jobs(&case_dir);
        assert_eq!(
            latest_running_job(&case_dir).unwrap().unwrap().job_id,
            live.job_id
        );

        // A job row without a held lock (killed process, or pre-lock
        // schema) is orphaned and gets interrupted.
        let conn = open_case_db(&case_dir).unwrap();
        init_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO jobs (job_id, job_type, status, subject_path, started_unix, updated_unix, completed_units, options_json) VALUES ('job_orphan', 'import-e01', 'running', 'x', ?1, ?1, 0, '{}')",
            params![u64_to_i64(1)],
        )
        .unwrap();
        drop(conn);
        reap_interrupted_jobs(&case_dir);
        let conn = open_case_db(&case_dir).unwrap();
        let status: String = conn
            .query_row(
                "SELECT status FROM jobs WHERE job_id = 'job_orphan'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "interrupted");
        // The still-live job is the newest running row, not the orphan.
        assert_eq!(
            latest_running_job(&case_dir).unwrap().unwrap().job_id,
            live.job_id
        );
        let _ = fs::remove_dir_all(case_dir);
    }
}
