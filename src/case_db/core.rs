use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) const SCHEMA_VERSION: &str = "7";

pub fn case_db_path(case_dir: &Path) -> PathBuf {
    case_dir.join("db/case.db")
}

pub(crate) fn open_case_db(case_dir: &Path) -> Result<Connection, String> {
    fs::create_dir_all(case_dir.join("db"))
        .map_err(|err| format!("failed to create case db directory: {err}"))?;
    let conn = Connection::open(case_db_path(case_dir))
        .map_err(|err| format!("failed to open SQLite case db: {err}"))?;
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(|err| format!("failed to configure SQLite busy timeout: {err}"))?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|err| format!("failed to enable SQLite foreign keys: {err}"))?;
    Ok(conn)
}

/// Starts a write transaction with `BEGIN IMMEDIATE`.
///
/// A deferred transaction (`conn.transaction()`) begins with no lock and
/// first reads under SHARED; the later upgrade to RESERVED can fail with
/// SQLITE_BUSY *immediately* (lock-upgrade deadlock), bypassing
/// `busy_timeout`. Taking RESERVED up front makes a competing writer wait
/// on the 5 s busy timeout configured by [`open_case_db`] instead, so a
/// mark save racing a scan write does not fail with "database is locked".
pub(crate) fn begin_write(conn: &mut Connection) -> rusqlite::Result<Transaction<'_>> {
    conn.transaction_with_behavior(TransactionBehavior::Immediate)
}

pub(crate) fn open_readonly_case_db(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|err| format!("failed to open SQLite case db {}: {err}", path.display()))?;
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(|err| format!("failed to configure SQLite busy timeout: {err}"))?;
    Ok(conn)
}

pub(crate) fn init_schema(conn: &Connection) -> Result<(), String> {
    // Two connections can reach first initialization at once (e.g. the
    // workstation status poller vs. a pipeline child). Without a write
    // lock up front, a racer that read schema_meta while holding a
    // SHARED lock hits SQLite's lock-upgrade deadlock — BUSY returned
    // immediately, bypassing busy_timeout. BEGIN IMMEDIATE takes the
    // RESERVED lock before ANY write, including the CREATE TABLE batch
    // below, so losers simply wait on busy_timeout.
    //
    // A migration chain (v1..v6 -> v7) is backed up ONCE, before the chain
    // transaction begins, with SQLite's online backup API (a consistent
    // committed snapshot, never a file copy torn by spilled pages of an
    // open transaction), labelled with the version actually on disk:
    // `case.db.backup-v<from>-to-v<SCHEMA_VERSION>-<unix>`.
    let backed_up = backup_before_migration(conn)?;
    conn.execute_batch("BEGIN IMMEDIATE TRANSACTION")
        .map_err(|err| format!("failed to begin SQLite schema transaction: {err}"))?;
    let result = init_schema_inner(conn, backed_up.as_deref());
    match result {
        Ok(()) => conn
            .execute_batch("COMMIT")
            .map_err(|err| format!("failed to commit SQLite schema transaction: {err}")),
        Err(err) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(err)
        }
    }
}

/// Schema versions [`init_schema`] migrates from.
const MIGRATABLE_VERSIONS: [&str; 6] = ["1", "2", "3", "4", "5", "6"];

/// Reads `schema_version` without failing on a database that has no
/// `schema_meta` table yet (fresh file → `None`).
fn read_stored_schema_version(conn: &Connection) -> Result<Option<String>, String> {
    if !table_exists(conn, "schema_meta")? {
        return Ok(None);
    }
    conn.query_row(
        "SELECT value FROM schema_meta WHERE key = 'schema_version'",
        [],
        |row| row.get(0),
    )
    .optional()
    .map_err(|err| format!("failed to read SQLite schema version: {err}"))
}

/// Takes the one pre-migration backup when the stored schema is older than
/// [`SCHEMA_VERSION`]; returns the version it backed up (`None` when no
/// migration is due or the database is in-memory).
fn backup_before_migration(conn: &Connection) -> Result<Option<String>, String> {
    let Some(version) = read_stored_schema_version(conn)? else {
        return Ok(None);
    };
    if !MIGRATABLE_VERSIONS.contains(&version.as_str()) {
        return Ok(None);
    }
    let Some(db_path) = main_database_path(conn)? else {
        return Ok(None);
    };
    if !db_path.is_file() {
        return Ok(None);
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| format!("system time before UNIX epoch: {err}"))?
        .as_secs();
    let backup_name = format!("case.db.backup-v{version}-to-v{SCHEMA_VERSION}-{timestamp}");
    let backup_path = db_path.with_file_name(backup_name);
    conn.backup(rusqlite::MAIN_DB, &backup_path, None)
        .map_err(|err| {
            format!(
                "failed to create SQLite migration backup {}: {err}",
                backup_path.display()
            )
        })?;
    Ok(Some(version))
}

fn init_schema_inner(conn: &Connection, backed_up: Option<&str>) -> Result<(), String> {
    // Re-read under the write lock: a migration this call did not back up
    // (the version changed between the backup and BEGIN) must not run.
    let stored = read_stored_schema_version(conn)?;
    if let Some(version) = stored.as_deref()
        && MIGRATABLE_VERSIONS.contains(&version)
        && backed_up != Some(version)
        && main_database_path(conn)?.is_some()
    {
        return Err(format!(
            "SQLite schema v{version} changed while preparing its migration backup; retry the operation"
        ));
    }
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS schema_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS scan_runs (
            run_pk INTEGER PRIMARY KEY AUTOINCREMENT,
            source_path TEXT NOT NULL,
            scanned_unix INTEGER NOT NULL,
            hash_files INTEGER NOT NULL,
            use_ffprobe INTEGER NOT NULL,
            max_depth INTEGER,
            video_count INTEGER NOT NULL,
            total_bytes INTEGER NOT NULL,
            warnings_json TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS scan_runs_scanned_unix_idx
            ON scan_runs (scanned_unix);

        CREATE TABLE IF NOT EXISTS videos (
            id TEXT PRIMARY KEY,
            source_path TEXT NOT NULL UNIQUE,
            file_url TEXT NOT NULL,
            relative_path TEXT NOT NULL,
            extension TEXT NOT NULL,
            size_bytes INTEGER NOT NULL,
            modified_unix INTEGER,
            sha256 TEXT,
            hash_status TEXT NOT NULL,
            confidence TEXT NOT NULL,
            source_profile_json TEXT NOT NULL,
            duration_seconds REAL,
            format_name TEXT,
            video_codec TEXT,
            audio_codec TEXT,
            width INTEGER,
            height INTEGER,
            ffprobe_ok INTEGER NOT NULL,
            ffprobe_error TEXT,
            ffprobe_json TEXT,
            first_indexed_unix INTEGER NOT NULL,
            last_indexed_unix INTEGER NOT NULL,
            last_scanned_unix INTEGER,
            record_json TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS videos_sha256_idx
            ON videos (sha256);
        CREATE INDEX IF NOT EXISTS videos_extension_idx
            ON videos (extension);
        CREATE INDEX IF NOT EXISTS videos_last_indexed_idx
            ON videos (last_indexed_unix);

        CREATE TABLE IF NOT EXISTS evidence_sources (
            source_id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            path TEXT NOT NULL,
            registered_unix INTEGER NOT NULL,
            last_seen_unix INTEGER NOT NULL,
            write_protect TEXT,
            acquisition_tool TEXT,
            evidence_hash TEXT,
            notes TEXT,
            metadata_json TEXT NOT NULL
        );

        CREATE UNIQUE INDEX IF NOT EXISTS evidence_sources_kind_path_idx
            ON evidence_sources (kind, path);
        CREATE INDEX IF NOT EXISTS evidence_sources_hash_idx
            ON evidence_sources (evidence_hash);

        CREATE TABLE IF NOT EXISTS jobs (
            job_id TEXT PRIMARY KEY,
            job_type TEXT NOT NULL,
            status TEXT NOT NULL,
            subject_path TEXT NOT NULL,
            started_unix INTEGER NOT NULL,
            updated_unix INTEGER NOT NULL,
            completed_unix INTEGER,
            total_units INTEGER,
            completed_units INTEGER NOT NULL DEFAULT 0,
            options_json TEXT NOT NULL,
            error TEXT
        );

        CREATE INDEX IF NOT EXISTS jobs_status_idx
            ON jobs (status);
        CREATE INDEX IF NOT EXISTS jobs_type_started_idx
            ON jobs (job_type, started_unix);

        CREATE TABLE IF NOT EXISTS job_events (
            event_pk INTEGER PRIMARY KEY AUTOINCREMENT,
            job_id TEXT NOT NULL,
            event_unix INTEGER NOT NULL,
            event_type TEXT NOT NULL,
            message TEXT NOT NULL,
            completed_units INTEGER,
            FOREIGN KEY(job_id) REFERENCES jobs(job_id)
        );

        CREATE INDEX IF NOT EXISTS job_events_job_idx
            ON job_events (job_id, event_unix);
        "#,
    )
    .map_err(|err| format!("failed to initialize SQLite schema: {err}"))?;
    init_schema_locked(conn)
}

fn init_schema_locked(conn: &Connection) -> Result<(), String> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT value FROM schema_meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|err| format!("failed to read SQLite schema version: {err}"))?;
    match stored.as_deref() {
        None => {
            apply_schema_v2_indexes(conn)?;
            apply_schema_v3_tables(conn)?;
            apply_schema_v4_columns(conn)?;
            apply_schema_v5_columns(conn)?;
            apply_schema_v6_history(conn)?;
            apply_schema_v7_rev(conn)?;
            conn.execute(
                "INSERT OR IGNORE INTO schema_meta (key, value) VALUES ('schema_version', ?1)",
                [SCHEMA_VERSION],
            )
            .map_err(|err| format!("failed to store SQLite schema version: {err}"))?;
        }
        Some("1") => {
            migrate_v1_to_v2(conn)?;
            migrate_v2_to_v3(conn)?;
            migrate_v3_to_v4(conn)?;
            migrate_v4_to_v5(conn)?;
            migrate_v5_to_v6(conn)?;
            migrate_v6_to_v7(conn)?;
        }
        Some("2") => {
            migrate_v2_to_v3(conn)?;
            migrate_v3_to_v4(conn)?;
            migrate_v4_to_v5(conn)?;
            migrate_v5_to_v6(conn)?;
            migrate_v6_to_v7(conn)?;
        }
        Some("3") => {
            migrate_v3_to_v4(conn)?;
            migrate_v4_to_v5(conn)?;
            migrate_v5_to_v6(conn)?;
            migrate_v6_to_v7(conn)?;
        }
        Some("4") => {
            migrate_v4_to_v5(conn)?;
            migrate_v5_to_v6(conn)?;
            migrate_v6_to_v7(conn)?;
        }
        Some("5") => {
            migrate_v5_to_v6(conn)?;
            migrate_v6_to_v7(conn)?;
        }
        Some("6") => migrate_v6_to_v7(conn)?,
        Some(SCHEMA_VERSION) => {
            apply_schema_v2_indexes(conn)?;
            apply_schema_v3_tables(conn)?;
            apply_schema_v4_columns(conn)?;
            apply_schema_v5_columns(conn)?;
            apply_schema_v6_history(conn)?;
            apply_schema_v7_rev(conn)?;
        }
        Some(version) => {
            return Err(format!("unsupported SQLite schema version: {version}"));
        }
    }
    apply_schema_fts_index(conn)
}

/// Additive FTS5 substring index over the videos table — a regular FTS
/// table (not external-content) populated by triggers, because the
/// searchable haystack joins relational columns with fields inside
/// record_json that the content table cannot express. `q` searches
/// still return record_json from the JSON index, so this index is a
/// lookup accelerator only, never a shape authority.
fn apply_schema_fts_index(conn: &Connection) -> Result<(), String> {
    // The trigram tokenizer gives true substring semantics (the same
    // contract the in-JSON `contains` search honors); unicode61 would
    // silently narrow matches to token boundaries.
    conn.execute_batch(
        r#"
        CREATE VIRTUAL TABLE IF NOT EXISTS videos_fts USING fts5(
            haystack,
            id UNINDEXED,
            tokenize='trigram'
        );

        CREATE TRIGGER IF NOT EXISTS videos_fts_ai AFTER INSERT ON videos BEGIN
            INSERT INTO videos_fts(rowid, id, haystack) VALUES (
                new.rowid, new.id,
                coalesce(new.id,'') || ' ' || coalesce(new.source_path,'') || ' ' ||
                coalesce(new.relative_path,'') || ' ' ||
                coalesce(json_extract(new.record_json,'$.name'),'') || ' ' ||
                coalesce(json_extract(new.record_json,'$.original_name'),''));
        END;
        CREATE TRIGGER IF NOT EXISTS videos_fts_ad AFTER DELETE ON videos BEGIN
            DELETE FROM videos_fts WHERE rowid = old.rowid;
        END;
        CREATE TRIGGER IF NOT EXISTS videos_fts_au AFTER UPDATE ON videos BEGIN
            DELETE FROM videos_fts WHERE rowid = old.rowid;
            INSERT INTO videos_fts(rowid, id, haystack) VALUES (
                new.rowid, new.id,
                coalesce(new.id,'') || ' ' || coalesce(new.source_path,'') || ' ' ||
                coalesce(new.relative_path,'') || ' ' ||
                coalesce(json_extract(new.record_json,'$.name'),'') || ' ' ||
                coalesce(json_extract(new.record_json,'$.original_name'),''));
        END;
        "#,
    )
    .map_err(|err| format!("failed to create videos FTS index: {err}"))?;
    // Backfill once for databases that predate the index; the flag keeps
    // a 100k-row rebuild from repeating on every open.
    let built: Option<String> = conn
        .query_row(
            "SELECT value FROM schema_meta WHERE key = 'videos_fts_built'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|err| format!("failed to read FTS backfill flag: {err}"))?;
    if built.is_none() {
        conn.execute_batch(
            r#"
            INSERT INTO videos_fts(rowid, id, haystack)
            SELECT rowid, id,
                coalesce(id,'') || ' ' || coalesce(source_path,'') || ' ' ||
                coalesce(relative_path,'') || ' ' ||
                coalesce(json_extract(record_json,'$.name'),'') || ' ' ||
                coalesce(json_extract(record_json,'$.original_name'),'')
            FROM videos;
            INSERT OR REPLACE INTO schema_meta (key, value)
            VALUES ('videos_fts_built', '1');
            "#,
        )
        .map_err(|err| format!("failed to backfill videos FTS index: {err}"))?;
    }
    Ok(())
}

fn migrate_v3_to_v4(conn: &Connection) -> Result<(), String> {
    apply_schema_v4_columns(conn)?;
    conn.execute(
        "UPDATE schema_meta SET value = ?1 WHERE key = 'schema_version'",
        [SCHEMA_VERSION],
    )
    .map_err(|err| format!("failed to update SQLite schema version: {err}"))?;
    Ok(())
}

fn migrate_v4_to_v5(conn: &Connection) -> Result<(), String> {
    apply_schema_v5_columns(conn)?;
    conn.execute(
        "UPDATE schema_meta SET value = ?1 WHERE key = 'schema_version'",
        [SCHEMA_VERSION],
    )
    .map_err(|err| format!("failed to update SQLite schema version: {err}"))?;
    Ok(())
}

fn migrate_v5_to_v6(conn: &Connection) -> Result<(), String> {
    // Older chains (e.g. a v4 file that never had review_marks) reach v6
    // with every earlier additive step re-applied; all are idempotent.
    apply_schema_v3_tables(conn)?;
    apply_schema_v4_columns(conn)?;
    apply_schema_v5_columns(conn)?;
    apply_schema_v6_history(conn)?;
    // Marks that existed before history was kept get one `baseline` row so
    // the history states where its knowledge starts instead of implying
    // the mark was created at its first post-migration edit.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| format!("system time before UNIX epoch: {err}"))?
        .as_secs();
    conn.execute(
        r#"
        INSERT INTO review_mark_history (
            record_id, action, changed_unix, actor,
            new_status, new_marked_unix, new_record_path, new_examiner, new_note,
            new_range_in, new_range_out, detail
        )
        SELECT record_id, 'baseline', ?1, NULL,
            status, marked_unix, record_path, examiner, note, range_in, range_out,
            'state at schema v6 migration; no earlier history was recorded'
        FROM review_marks
        "#,
        [i64::try_from(now).unwrap_or(i64::MAX)],
    )
    .map_err(|err| format!("failed to record v6 review mark baseline: {err}"))?;
    conn.execute(
        "UPDATE schema_meta SET value = ?1 WHERE key = 'schema_version'",
        [SCHEMA_VERSION],
    )
    .map_err(|err| format!("failed to update SQLite schema version: {err}"))?;
    Ok(())
}

/// v6 → v7: adds the per-record annotation revision table (save protocol
/// v2, optimistic concurrency). Purely additive — `review_marks`, the
/// `review_tags.v1:*` rows and `review_mark_history` are not touched
/// (history is trigger-protected against UPDATE/DELETE anyway). Every id
/// that currently carries a mark or a tag list starts at revision 1.
fn migrate_v6_to_v7(conn: &Connection) -> Result<(), String> {
    // Chains from older versions re-apply every earlier additive step
    // (all idempotent) before the v7 table is created.
    apply_schema_v3_tables(conn)?;
    apply_schema_v4_columns(conn)?;
    apply_schema_v5_columns(conn)?;
    apply_schema_v6_history(conn)?;
    apply_schema_v7_rev(conn)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| format!("system time before UNIX epoch: {err}"))?
        .as_secs();
    conn.execute(
        r#"
        INSERT OR IGNORE INTO review_annotation_rev (record_id, revision, updated_unix, actor, client_id)
        SELECT record_id, 1, marked_unix, examiner, NULL FROM review_marks
        "#,
        [],
    )
    .map_err(|err| format!("failed to seed v7 annotation revisions from marks: {err}"))?;
    conn.execute(
        r#"
        INSERT OR IGNORE INTO review_annotation_rev (record_id, revision, updated_unix, actor, client_id)
        SELECT substr(key, 16), 1, ?1, NULL, NULL FROM schema_meta
        WHERE substr(key, 1, 15) = 'review_tags.v1:'
        "#,
        [i64::try_from(now).unwrap_or(i64::MAX)],
    )
    .map_err(|err| format!("failed to seed v7 annotation revisions from tags: {err}"))?;
    conn.execute(
        "UPDATE schema_meta SET value = ?1 WHERE key = 'schema_version'",
        [SCHEMA_VERSION],
    )
    .map_err(|err| format!("failed to update SQLite schema version: {err}"))?;
    Ok(())
}

/// v7: one revision counter per annotated record (marks, note, range and
/// tags together). A row outlives its mark (tombstone) so a stale client
/// recreating a deleted mark is caught as a conflict.
pub(crate) fn apply_schema_v7_rev(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS review_annotation_rev (
            record_id    TEXT PRIMARY KEY,
            revision     INTEGER NOT NULL,
            updated_unix INTEGER NOT NULL,
            actor        TEXT,
            client_id    TEXT
        );
        "#,
    )
    .map_err(|err| format!("failed to apply SQLite v7 annotation revision table: {err}"))
}

/// v6: append-only review mark history (FOR-07) and hash provenance for
/// evidence sources (FOR-04).
fn apply_schema_v6_history(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS review_mark_history (
            history_pk INTEGER PRIMARY KEY AUTOINCREMENT,
            record_id TEXT NOT NULL,
            action TEXT NOT NULL,
            changed_unix INTEGER NOT NULL,
            actor TEXT,
            old_status TEXT,
            new_status TEXT,
            old_marked_unix INTEGER,
            new_marked_unix INTEGER,
            old_record_path TEXT,
            new_record_path TEXT,
            old_examiner TEXT,
            new_examiner TEXT,
            old_note TEXT,
            new_note TEXT,
            old_range_in REAL,
            new_range_in REAL,
            old_range_out REAL,
            new_range_out REAL,
            old_tags_json TEXT,
            new_tags_json TEXT,
            detail TEXT
        );

        CREATE INDEX IF NOT EXISTS review_mark_history_record_idx
            ON review_mark_history (record_id, history_pk);

        CREATE TRIGGER IF NOT EXISTS review_mark_history_no_update
            BEFORE UPDATE ON review_mark_history
        BEGIN
            SELECT RAISE(ABORT, 'review_mark_history is append-only');
        END;

        CREATE TRIGGER IF NOT EXISTS review_mark_history_no_delete
            BEFORE DELETE ON review_mark_history
        BEGIN
            SELECT RAISE(ABORT, 'review_mark_history is append-only');
        END;
        "#,
    )
    .map_err(|err| format!("failed to apply SQLite v6 review mark history: {err}"))?;
    for column in ["evidence_hash_algorithm", "evidence_hash_origin"] {
        if !table_has_column(conn, "evidence_sources", column)? {
            conn.execute_batch(&format!(
                "ALTER TABLE evidence_sources ADD COLUMN {column} TEXT"
            ))
            .map_err(|err| format!("failed to apply SQLite v6 {column} column: {err}"))?;
        }
    }
    Ok(())
}

fn apply_schema_v5_columns(conn: &Connection) -> Result<(), String> {
    // v5: examiner IN/OUT range (seconds) per review mark.
    if !table_exists(conn, "review_marks")? {
        return Ok(());
    }
    for column in ["range_in", "range_out"] {
        if !table_has_column(conn, "review_marks", column)? {
            conn.execute_batch(&format!(
                "ALTER TABLE review_marks ADD COLUMN {column} REAL"
            ))
            .map_err(|err| format!("failed to apply SQLite v5 {column} column: {err}"))?;
        }
    }
    Ok(())
}

fn apply_schema_v4_columns(conn: &Connection) -> Result<(), String> {
    // v4: free-text examiner note per review mark.
    if table_exists(conn, "review_marks")? && !table_has_column(conn, "review_marks", "note")? {
        conn.execute_batch("ALTER TABLE review_marks ADD COLUMN note TEXT")
            .map_err(|err| format!("failed to apply SQLite v4 note column: {err}"))?;
    }
    Ok(())
}

pub(crate) fn table_has_column(
    conn: &Connection,
    table: &str,
    column: &str,
) -> Result<bool, String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|err| format!("failed to inspect SQLite table {table}: {err}"))?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|err| format!("failed to read SQLite table info for {table}: {err}"))?;
    for name in names {
        if name.map_err(|err| format!("failed to read column name: {err}"))? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn migrate_v2_to_v3(conn: &Connection) -> Result<(), String> {
    apply_schema_v3_tables(conn)?;
    conn.execute(
        "UPDATE schema_meta SET value = ?1 WHERE key = 'schema_version'",
        [SCHEMA_VERSION],
    )
    .map_err(|err| format!("failed to update SQLite schema version: {err}"))?;
    Ok(())
}

fn apply_schema_v3_tables(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS review_marks (
            record_id TEXT PRIMARY KEY,
            status TEXT NOT NULL,
            marked_unix INTEGER NOT NULL,
            record_path TEXT,
            examiner TEXT
        );

        CREATE INDEX IF NOT EXISTS review_marks_status_idx
            ON review_marks (status);
        "#,
    )
    .map_err(|err| format!("failed to apply SQLite v3 review marks schema: {err}"))
}

fn migrate_v1_to_v2(conn: &Connection) -> Result<(), String> {
    apply_schema_v2_indexes(conn)?;
    conn.execute(
        "UPDATE schema_meta SET value = ?1 WHERE key = 'schema_version'",
        [SCHEMA_VERSION],
    )
    .map_err(|err| format!("failed to update SQLite schema version: {err}"))?;
    Ok(())
}

fn apply_schema_v2_indexes(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        r#"
        CREATE INDEX IF NOT EXISTS videos_modified_unix_idx
            ON videos (modified_unix);
        CREATE INDEX IF NOT EXISTS videos_ffprobe_ok_idx
            ON videos (ffprobe_ok);
        CREATE INDEX IF NOT EXISTS videos_confidence_idx
            ON videos (confidence);
        CREATE INDEX IF NOT EXISTS videos_extension_modified_idx
            ON videos (extension, modified_unix);
        CREATE INDEX IF NOT EXISTS videos_last_scanned_idx
            ON videos (last_scanned_unix);
        "#,
    )
    .map_err(|err| format!("failed to apply SQLite v2 indexes: {err}"))
}

fn main_database_path(conn: &Connection) -> Result<Option<PathBuf>, String> {
    let mut stmt = conn
        .prepare("PRAGMA database_list")
        .map_err(|err| format!("failed to inspect SQLite database list: {err}"))?;
    let rows = stmt
        .query_map([], |row| {
            let name: String = row.get(1)?;
            let path: String = row.get(2)?;
            Ok((name, path))
        })
        .map_err(|err| format!("failed to query SQLite database list: {err}"))?;
    for row in rows {
        let (name, path) =
            row.map_err(|err| format!("failed to read SQLite database list row: {err}"))?;
        if name == "main" && !path.is_empty() {
            return Ok(Some(PathBuf::from(path)));
        }
    }
    Ok(None)
}

pub(crate) fn table_exists(conn: &Connection, table_name: &str) -> Result<bool, String> {
    let found: Option<String> = conn
        .query_row(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table_name],
            |row| row.get(0),
        )
        .optional()
        .map_err(|err| format!("failed to inspect SQLite schema: {err}"))?;
    Ok(found.is_some())
}

#[cfg(test)]
mod tests {
    use super::{
        SCHEMA_VERSION, begin_write, case_db_path, init_schema, open_case_db, table_exists,
        table_has_column,
    };
    use rusqlite::{Connection, OptionalExtension};
    use std::fs;

    /// `begin_write` must take the RESERVED lock at BEGIN (IMMEDIATE), so a
    /// second writer is blocked right away (and would wait on busy_timeout)
    /// instead of both proceeding and one failing later on lock upgrade.
    /// Deterministic: no threads, no sleeps — the second connection uses
    /// busy_timeout 0 so BUSY is reported instantly.
    #[test]
    fn begin_write_takes_reserved_lock_up_front() {
        let case_dir = std::env::temp_dir().join(format!(
            "frametrace-begin-write-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&case_dir);
        let mut first = open_case_db(&case_dir).unwrap();
        init_schema(&first).unwrap();

        let second = Connection::open(case_db_path(&case_dir)).unwrap();
        second
            .busy_timeout(std::time::Duration::from_millis(0))
            .unwrap();

        // Control: a deferred transaction holds no lock until it touches
        // the db, so a competing BEGIN IMMEDIATE would still succeed.
        {
            let deferred = first.transaction().unwrap();
            second.execute_batch("BEGIN IMMEDIATE").unwrap();
            second.execute_batch("ROLLBACK").unwrap();
            deferred.rollback().unwrap();
        }

        // With the helper the writer slot is already taken.
        let tx = begin_write(&mut first).unwrap();
        let err = second.execute_batch("BEGIN IMMEDIATE").unwrap_err();
        assert_eq!(
            err.sqlite_error_code(),
            Some(rusqlite::ErrorCode::DatabaseBusy),
            "expected SQLITE_BUSY, got {err}"
        );
        tx.rollback().unwrap();
        // Released on rollback: the second writer can now start.
        second.execute_batch("BEGIN IMMEDIATE").unwrap();
        second.execute_batch("ROLLBACK").unwrap();

        drop(second);
        drop(first);
        let _ = fs::remove_dir_all(case_dir);
    }

    #[test]
    fn initializes_new_databases_at_current_schema_version() {
        let case_dir =
            std::env::temp_dir().join(format!("frametrace-schema-new-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&case_dir);

        let conn = open_case_db(&case_dir).unwrap();
        init_schema(&conn).unwrap();
        assert_eq!(read_schema_version(&conn).as_deref(), Some(SCHEMA_VERSION));
        assert!(index_exists(&conn, "videos_modified_unix_idx"));

        let _ = fs::remove_dir_all(case_dir);
    }

    #[test]
    fn fts_index_backfills_and_tracks_video_rows() {
        let case_dir =
            std::env::temp_dir().join(format!("frametrace-fts-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&case_dir);

        let conn = open_case_db(&case_dir).unwrap();
        init_schema(&conn).unwrap();
        assert!(table_exists(&conn, "videos_fts").unwrap());
        let insert = |id: &str, path: &str, rel: &str, record: &str| {
            conn.execute(
                "INSERT INTO videos (id, source_path, file_url, relative_path, extension,
                     size_bytes, hash_status, confidence, source_profile_json, ffprobe_ok,
                     first_indexed_unix, last_indexed_unix, record_json)
                 VALUES (?1, ?2, '', ?3, 'mp4', 1, 'pending', 'confirmed', '{}', 0, 0, 0, ?4)",
                rusqlite::params![id, path, rel, record],
            )
            .unwrap();
        };
        insert(
            "vid_1",
            "D:\\evidence\\사고 블랙박스 영상.mp4",
            "사고 블랙박스 영상.mp4",
            r#"{"id":"vid_1","name":"accident","original_name":"사고 블랙박스 영상.mp4"}"#,
        );
        insert(
            "vid_2",
            "D:\\evidence\\OTHER.mp4",
            "other.mp4",
            r#"{"id":"vid_2"}"#,
        );
        // Trigram substring semantics: mixed-case ASCII and a Hangul
        // fragment both hit; an absent substring misses.
        let hits = |q: &str| -> Vec<String> {
            conn.prepare("SELECT id FROM videos_fts WHERE videos_fts MATCH ?1")
                .unwrap()
                .query_map([format!("\"{q}\"")], |row| row.get(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        assert_eq!(hits("블랙박스"), vec!["vid_1"]);
        assert_eq!(hits("OTHER"), vec!["vid_2"]);
        assert_eq!(hits("ther.mp4"), vec!["vid_2"]);
        assert!(hits("dashcam").is_empty());
        // The update trigger re-indexes: renaming both paths must move
        // the hit (haystack covers source_path AND relative_path).
        conn.execute(
            "UPDATE videos SET source_path = 'D:\\evidence\\renamed.mp4', relative_path = 'renamed.mp4' WHERE id = 'vid_2'",
            [],
        )
        .unwrap();
        assert!(hits("other.mp4").is_empty());
        assert_eq!(hits("renamed"), vec!["vid_2"]);
        conn.execute("DELETE FROM videos WHERE id = 'vid_1'", [])
            .unwrap();
        assert!(hits("블랙박스").is_empty());
        // The one-shot backfill flag is set so reopening never rebuilds.
        let flag: Option<String> = conn
            .query_row(
                "SELECT value FROM schema_meta WHERE key = 'videos_fts_built'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap();
        assert_eq!(flag.as_deref(), Some("1"));

        let _ = fs::remove_dir_all(case_dir);
    }

    #[test]
    fn migrates_v1_schema_to_current_with_backup() {
        let case_dir = std::env::temp_dir().join(format!(
            "frametrace-schema-migrate-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&case_dir);
        fs::create_dir_all(case_dir.join("db")).unwrap();
        let db_path = case_db_path(&case_dir);
        let seed = Connection::open(&db_path).unwrap();
        seed.execute_batch(
            r#"
            CREATE TABLE schema_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            INSERT INTO schema_meta (key, value) VALUES ('schema_version', '1');
            "#,
        )
        .unwrap();
        drop(seed);

        let conn = open_case_db(&case_dir).unwrap();
        init_schema(&conn).unwrap();
        assert_eq!(read_schema_version(&conn).as_deref(), Some(SCHEMA_VERSION));
        assert!(index_exists(&conn, "videos_extension_modified_idx"));
        assert!(table_exists(&conn, "review_marks").unwrap());
        let backup_exists = fs::read_dir(case_dir.join("db"))
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("case.db.backup-v1-to-v7-")
            });
        assert!(backup_exists);

        let _ = fs::remove_dir_all(case_dir);
    }

    #[test]
    fn migrates_v2_schema_to_v3_with_review_marks() {
        let case_dir = std::env::temp_dir().join(format!(
            "frametrace-schema-v2-migrate-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&case_dir);
        fs::create_dir_all(case_dir.join("db")).unwrap();
        let db_path = case_db_path(&case_dir);
        let seed = Connection::open(&db_path).unwrap();
        seed.execute_batch(
            r#"
            CREATE TABLE schema_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            INSERT INTO schema_meta (key, value) VALUES ('schema_version', '2');
            "#,
        )
        .unwrap();
        drop(seed);

        let conn = open_case_db(&case_dir).unwrap();
        init_schema(&conn).unwrap();
        assert_eq!(read_schema_version(&conn).as_deref(), Some(SCHEMA_VERSION));
        assert!(table_exists(&conn, "review_marks").unwrap());
        let backup_exists = fs::read_dir(case_dir.join("db"))
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("case.db.backup-v2-to-v7-")
            });
        assert!(backup_exists);

        let _ = fs::remove_dir_all(case_dir);
    }

    #[test]
    fn migrates_v4_schema_to_v5_with_backup_and_range_columns() {
        let case_dir = std::env::temp_dir().join(format!(
            "frametrace-schema-v4-v5-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&case_dir);
        fs::create_dir_all(case_dir.join("db")).unwrap();
        let seed = Connection::open(case_db_path(&case_dir)).unwrap();
        seed.execute_batch(
            r#"
            CREATE TABLE schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            INSERT INTO schema_meta (key, value) VALUES ('schema_version', '4');
            CREATE TABLE review_marks (
                record_id TEXT PRIMARY KEY,
                status TEXT NOT NULL,
                marked_unix INTEGER NOT NULL,
                record_path TEXT,
                examiner TEXT,
                note TEXT
            );
            INSERT INTO review_marks (record_id, status, marked_unix, note)
            VALUES ('vid_1', 'relevant', 5, 'keep me');
            "#,
        )
        .unwrap();
        drop(seed);

        let conn = open_case_db(&case_dir).unwrap();
        init_schema(&conn).unwrap();
        assert_eq!(read_schema_version(&conn).as_deref(), Some(SCHEMA_VERSION));
        assert!(table_has_column(&conn, "review_marks", "range_in").unwrap());
        assert!(table_has_column(&conn, "review_marks", "range_out").unwrap());
        let note: String = conn
            .query_row(
                "SELECT note FROM review_marks WHERE record_id = 'vid_1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(note, "keep me");
        let backup_exists = fs::read_dir(case_dir.join("db"))
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("case.db.backup-v4-to-v7-")
            });
        assert!(backup_exists);
        let _ = fs::remove_dir_all(case_dir);
    }

    /// First-initialization racing: the workstation status poller and a
    /// pipeline child can both see an empty schema_meta and race the
    /// version insert. Every racer must finish cleanly.
    #[test]
    fn concurrent_first_init_does_not_fail_on_version_insert() {
        let case_dir = std::env::temp_dir().join(format!(
            "frametrace-schema-race-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&case_dir);
        fs::create_dir_all(case_dir.join("db")).unwrap();

        std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for _ in 0..8 {
                handles.push(scope.spawn(|| {
                    // Retry a few rounds: a racer may briefly see the
                    // table before the winner commits the version row.
                    for _ in 0..3 {
                        let conn = open_case_db(&case_dir).unwrap();
                        init_schema(&conn).unwrap();
                    }
                }));
            }
            for handle in handles {
                handle.join().unwrap();
            }
        });

        let conn = open_case_db(&case_dir).unwrap();
        assert_eq!(read_schema_version(&conn).as_deref(), Some(SCHEMA_VERSION));
        let _ = fs::remove_dir_all(case_dir);
    }

    fn read_schema_version(conn: &Connection) -> Option<String> {
        conn.query_row(
            "SELECT value FROM schema_meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
    }

    fn index_exists(conn: &Connection, index_name: &str) -> bool {
        conn.query_row(
            "SELECT name FROM sqlite_master WHERE type = 'index' AND name = ?1",
            [index_name],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .unwrap()
        .is_some()
    }
}
