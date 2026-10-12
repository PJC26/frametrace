//! Review mark writes with append-only history (FOR-07, schema v6).
//!
//! Marks, tags and deletions stay mutable in `review_marks` /
//! `schema_meta` (the viewer reads current state from there), but every
//! effective change is also appended to `review_mark_history` inside the
//! SAME transaction, so a mark can never change without its history. The
//! history table rejects UPDATE/DELETE via triggers.

use super::*;
use crate::util::now_unix;
use rusqlite::{OptionalExtension, Transaction, params};
use std::path::Path;

/// Stored state of one review mark, as compared for history rows.
#[derive(Debug, Clone, PartialEq)]
struct StoredMark {
    status: String,
    marked_unix: i64,
    record_path: Option<String>,
    examiner: Option<String>,
    note: Option<String>,
    range_in: Option<f64>,
    range_out: Option<f64>,
}

fn read_stored_mark(tx: &Transaction<'_>, record_id: &str) -> Result<Option<StoredMark>, String> {
    tx.query_row(
        "SELECT status, marked_unix, record_path, examiner, note, range_in, range_out FROM review_marks WHERE record_id = ?1",
        [record_id],
        |row| {
            Ok(StoredMark {
                status: row.get(0)?,
                marked_unix: row.get(1)?,
                record_path: row.get(2)?,
                examiner: row.get(3)?,
                note: row.get(4)?,
                range_in: row.get(5)?,
                range_out: row.get(6)?,
            })
        },
    )
    .optional()
    .map_err(|err| format!("failed to read review mark {record_id}: {err}"))
}

fn insert_mark_history(
    tx: &Transaction<'_>,
    record_id: &str,
    action: &str,
    changed_unix: i64,
    actor: Option<&str>,
    old: Option<&StoredMark>,
    new: Option<&StoredMark>,
) -> Result<(), String> {
    tx.execute(
        r#"
        INSERT INTO review_mark_history (
            record_id, action, changed_unix, actor,
            old_status, new_status, old_marked_unix, new_marked_unix,
            old_record_path, new_record_path, old_examiner, new_examiner,
            old_note, new_note, old_range_in, new_range_in, old_range_out, new_range_out
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
        "#,
        params![
            record_id,
            action,
            changed_unix,
            actor,
            old.map(|m| m.status.as_str()),
            new.map(|m| m.status.as_str()),
            old.map(|m| m.marked_unix),
            new.map(|m| m.marked_unix),
            old.and_then(|m| m.record_path.as_deref()),
            new.and_then(|m| m.record_path.as_deref()),
            old.and_then(|m| m.examiner.as_deref()),
            new.and_then(|m| m.examiner.as_deref()),
            old.and_then(|m| m.note.as_deref()),
            new.and_then(|m| m.note.as_deref()),
            old.and_then(|m| m.range_in),
            new.and_then(|m| m.range_in),
            old.and_then(|m| m.range_out),
            new.and_then(|m| m.range_out),
        ],
    )
    .map_err(|err| format!("failed to record review mark history for {record_id}: {err}"))?;
    Ok(())
}

/// [`patch_review_annotations`] with the acting examiner recorded on every
/// history row. Deletions and tag edits carry no per-mark examiner, so
/// `actor` is the only "who" they get; mark upserts record the mark's own
/// examiner, falling back to `actor`.
///
/// History rows: `insert`, `update`, `delete` (a tombstone whose `old_*`
/// columns keep the last stored values), `tags`. A re-sent mark that
/// changes nothing writes no row.
pub fn patch_review_annotations_by(
    case_dir: &Path,
    marks: &[ReviewMarkRow],
    tags: &[crate::selection::TagEntry],
    deleted_ids: &[String],
    actor: Option<&str>,
) -> Result<usize, String> {
    let mut conn = open_case_db(case_dir)?;
    init_schema(&conn)?;
    let now = u64_to_i64(now_unix()?);
    let tx = begin_write(&mut conn)
        .map_err(|err| format!("failed to start SQLite marks transaction: {err}"))?;
    for id in deleted_ids {
        let Some(old) = read_stored_mark(&tx, id)? else {
            continue;
        };
        tx.execute("DELETE FROM review_marks WHERE record_id = ?1", [id])
            .map_err(|err| format!("failed to remove review mark: {err}"))?;
        insert_mark_history(&tx, id, "delete", now, actor, Some(&old), None)?;
    }
    for entry in tags {
        let key = format!("review_tags.v1:{}", entry.id);
        let old_tags: Option<String> = tx
            .query_row(
                "SELECT value FROM schema_meta WHERE key = ?1",
                [&key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|err| format!("failed to read review tags: {err}"))?;
        let new_tags = if entry.tags.is_empty() {
            tx.execute("DELETE FROM schema_meta WHERE key = ?1", [&key])
                .map_err(|err| format!("failed to remove review tags: {err}"))?;
            None
        } else {
            let value = serde_json::to_string(&entry.tags).map_err(|err| err.to_string())?;
            tx.execute("INSERT INTO schema_meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", params![key, value])
                .map_err(|err| format!("failed to store review tags: {err}"))?;
            Some(value)
        };
        if old_tags != new_tags {
            tx.execute(
                "INSERT INTO review_mark_history (record_id, action, changed_unix, actor, old_tags_json, new_tags_json) VALUES (?1, 'tags', ?2, ?3, ?4, ?5)",
                params![entry.id, now, actor, old_tags, new_tags],
            )
            .map_err(|err| format!("failed to record review tag history for {}: {err}", entry.id))?;
        }
    }
    for mark in marks {
        let old = read_stored_mark(&tx, &mark.record_id)?;
        tx.execute(
            r#"
            INSERT INTO review_marks (record_id, status, marked_unix, record_path, examiner, note, range_in, range_out)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(record_id) DO UPDATE SET
                status = excluded.status,
                marked_unix = excluded.marked_unix,
                record_path = COALESCE(excluded.record_path, review_marks.record_path),
                examiner = COALESCE(excluded.examiner, review_marks.examiner),
                note = COALESCE(excluded.note, review_marks.note),
                range_in = CASE WHEN ?9 THEN excluded.range_in ELSE review_marks.range_in END,
                range_out = CASE WHEN ?9 THEN excluded.range_out ELSE review_marks.range_out END
            "#,
            params![
                mark.record_id.as_str(),
                mark.status.as_str(),
                u64_to_i64(mark.marked_unix),
                mark.record_path.as_deref(),
                mark.examiner.as_deref(),
                mark.note.as_deref(),
                mark.range.and_then(|range| range.start),
                mark.range.and_then(|range| range.end),
                mark.range.is_some(),
            ],
        )
        .map_err(|err| format!("failed to upsert review mark {}: {err}", mark.record_id))?;
        let new = read_stored_mark(&tx, &mark.record_id)?;
        if old != new {
            let action = if old.is_some() { "update" } else { "insert" };
            insert_mark_history(
                &tx,
                &mark.record_id,
                action,
                now,
                mark.examiner.as_deref().or(actor),
                old.as_ref(),
                new.as_ref(),
            )?;
        }
    }
    tx.commit()
        .map_err(|err| format!("failed to commit review marks: {err}"))?;
    Ok(marks.len())
}

/// One append-only `review_mark_history` row.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ReviewMarkHistoryRow {
    pub history_pk: i64,
    pub record_id: String,
    /// `baseline` (state at the v6 migration), `insert`, `update`,
    /// `delete` (tombstone: `old_*` keep the last values), or `tags`.
    pub action: String,
    pub changed_unix: u64,
    pub actor: Option<String>,
    pub old_status: Option<String>,
    pub new_status: Option<String>,
    pub old_note: Option<String>,
    pub new_note: Option<String>,
    pub old_range_in: Option<f64>,
    pub new_range_in: Option<f64>,
    pub old_range_out: Option<f64>,
    pub new_range_out: Option<f64>,
    pub old_examiner: Option<String>,
    pub new_examiner: Option<String>,
    pub old_tags_json: Option<String>,
    pub new_tags_json: Option<String>,
}

/// Reads review mark history in append order, optionally for one record.
/// A case db that predates v6 has no history table and returns empty.
pub fn load_review_mark_history(
    case_dir: &Path,
    record_id: Option<&str>,
) -> Result<Vec<ReviewMarkHistoryRow>, String> {
    let path = case_db_path(case_dir);
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let conn = open_readonly_case_db(&path)?;
    load_review_mark_history_from(&conn, record_id)
}

pub(crate) fn load_review_mark_history_from(
    conn: &rusqlite::Connection,
    record_id: Option<&str>,
) -> Result<Vec<ReviewMarkHistoryRow>, String> {
    if !table_exists(conn, "review_mark_history")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare(
            "SELECT history_pk, record_id, action, changed_unix, actor, old_status, new_status, old_note, new_note, old_range_in, new_range_in, old_range_out, new_range_out, old_examiner, new_examiner, old_tags_json, new_tags_json FROM review_mark_history WHERE ?1 IS NULL OR record_id = ?1 ORDER BY history_pk",
        )
        .map_err(|err| format!("failed to prepare review mark history query: {err}"))?;
    let rows = stmt
        .query_map([record_id], |row| {
            Ok(ReviewMarkHistoryRow {
                history_pk: row.get(0)?,
                record_id: row.get(1)?,
                action: row.get(2)?,
                changed_unix: i64_to_u64(row.get(3)?),
                actor: row.get(4)?,
                old_status: row.get(5)?,
                new_status: row.get(6)?,
                old_note: row.get(7)?,
                new_note: row.get(8)?,
                old_range_in: row.get(9)?,
                new_range_in: row.get(10)?,
                old_range_out: row.get(11)?,
                new_range_out: row.get(12)?,
                old_examiner: row.get(13)?,
                new_examiner: row.get(14)?,
                old_tags_json: row.get(15)?,
                new_tags_json: row.get(16)?,
            })
        })
        .map_err(|err| format!("failed to query review mark history: {err}"))?;
    rows.map(|row| row.map_err(|err| format!("failed to read review mark history row: {err}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection::TagEntry;
    use rusqlite::Connection;
    use std::fs;

    fn case(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "frametrace-mark-history-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("db")).unwrap();
        dir
    }

    fn mark(status: &str, note: Option<&str>, range: Option<RangeSpan>) -> ReviewMarkRow {
        ReviewMarkRow {
            record_id: "vid_1".into(),
            status: status.into(),
            marked_unix: 7,
            examiner: Some("kim".into()),
            note: note.map(str::to_string),
            range,
            ..Default::default()
        }
    }

    #[test]
    fn every_change_is_appended_and_deletions_leave_tombstones() {
        let dir = case("chain");
        let span = RangeSpan {
            start: Some(1.0),
            end: Some(4.5),
        };
        patch_review_annotations(&dir, &[mark("relevant", Some("first"), None)], &[], &[]).unwrap();
        // Re-sending the identical mark is not a change.
        patch_review_annotations(&dir, &[mark("relevant", Some("first"), None)], &[], &[]).unwrap();
        patch_review_annotations(
            &dir,
            &[mark("important", Some("second"), Some(span))],
            &[],
            &[],
        )
        .unwrap();
        patch_review_annotations_by(
            &dir,
            &[],
            &[TagEntry {
                id: "vid_1".into(),
                tags: vec!["사고".into()],
            }],
            &["vid_1".into()],
            Some("lee"),
        )
        .unwrap();
        assert!(load_review_marks(&dir).unwrap().is_empty());

        let history = load_review_mark_history(&dir, Some("vid_1")).unwrap();
        let actions: Vec<&str> = history.iter().map(|row| row.action.as_str()).collect();
        assert_eq!(actions, ["insert", "update", "delete", "tags"]);
        assert_eq!(history[0].actor.as_deref(), Some("kim"));
        assert_eq!(history[1].old_note.as_deref(), Some("first"));
        assert_eq!(history[1].new_note.as_deref(), Some("second"));
        assert_eq!(history[1].old_range_in, None);
        assert_eq!(history[1].new_range_in, Some(1.0));
        assert_eq!(history[1].new_range_out, Some(4.5));
        // Tombstone keeps the last values and who removed it.
        assert_eq!(history[2].actor.as_deref(), Some("lee"));
        assert_eq!(history[2].old_status.as_deref(), Some("important"));
        assert_eq!(history[2].old_note.as_deref(), Some("second"));
        assert_eq!(history[2].old_range_out, Some(4.5));
        assert_eq!(history[2].new_status, None);
        assert_eq!(history[3].new_tags_json.as_deref(), Some("[\"사고\"]"));

        // Append-only: the table refuses edits and deletes.
        let conn = open_case_db(&dir).unwrap();
        let update = conn.execute("UPDATE review_mark_history SET actor = 'x'", []);
        assert!(update.unwrap_err().to_string().contains("append-only"));
        let delete = conn.execute("DELETE FROM review_mark_history", []);
        assert!(delete.unwrap_err().to_string().contains("append-only"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn v5_case_migrates_with_backup_baseline_and_then_records_patches() {
        let dir = case("v5");
        let seed = Connection::open(case_db_path(&dir)).unwrap();
        seed.execute_batch(
            r#"
            CREATE TABLE schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            INSERT INTO schema_meta (key, value) VALUES ('schema_version', '5');
            CREATE TABLE evidence_sources (
                source_id TEXT PRIMARY KEY, kind TEXT NOT NULL, path TEXT NOT NULL,
                registered_unix INTEGER NOT NULL, last_seen_unix INTEGER NOT NULL,
                write_protect TEXT, acquisition_tool TEXT, evidence_hash TEXT,
                notes TEXT, metadata_json TEXT NOT NULL
            );
            CREATE TABLE review_marks (
                record_id TEXT PRIMARY KEY, status TEXT NOT NULL,
                marked_unix INTEGER NOT NULL, record_path TEXT, examiner TEXT,
                note TEXT, range_in REAL, range_out REAL
            );
            INSERT INTO review_marks (record_id, status, marked_unix, note, range_in, range_out)
            VALUES ('vid_1', 'relevant', 5, 'pre-v6 note', 2.0, 3.0);
            "#,
        )
        .unwrap();
        drop(seed);

        patch_review_annotations(&dir, &[mark("important", None, None)], &[], &[]).unwrap();

        let conn = open_readonly_case_db(&case_db_path(&dir)).unwrap();
        let version: String = conn
            .query_row(
                "SELECT value FROM schema_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, "6");
        assert!(table_has_column(&conn, "evidence_sources", "evidence_hash_origin").unwrap());
        let backup = fs::read_dir(dir.join("db"))
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("case.db.backup-v5-to-v6-")
            });
        assert!(backup, "v5->v6 migration must leave a backup");
        let history = load_review_mark_history(&dir, None).unwrap();
        assert_eq!(history.len(), 2, "{history:?}");
        assert_eq!(history[0].action, "baseline");
        assert_eq!(history[0].new_note.as_deref(), Some("pre-v6 note"));
        assert_eq!(history[1].action, "update");
        assert_eq!(history[1].old_status.as_deref(), Some("relevant"));
        assert_eq!(history[1].new_status.as_deref(), Some("important"));
        // The v5 note and range survive an update that omits them.
        assert_eq!(history[1].new_note.as_deref(), Some("pre-v6 note"));
        assert_eq!(history[1].new_range_in, Some(2.0));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn every_older_schema_reaches_v6() {
        for version in ["1", "2", "3", "4", "5"] {
            let dir = case(&format!("chain-v{version}"));
            let seed = Connection::open(case_db_path(&dir)).unwrap();
            seed.execute_batch(&format!(
                "CREATE TABLE schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO schema_meta (key, value) VALUES ('schema_version', '{version}');"
            ))
            .unwrap();
            drop(seed);
            patch_review_annotations(&dir, &[mark("relevant", None, None)], &[], &[])
                .unwrap_or_else(|err| panic!("v{version}: {err}"));
            let conn = open_readonly_case_db(&case_db_path(&dir)).unwrap();
            let stored: String = conn
                .query_row(
                    "SELECT value FROM schema_meta WHERE key = 'schema_version'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(stored, "6", "v{version}");
            assert_eq!(
                load_review_mark_history(&dir, None).unwrap().len(),
                1,
                "v{version}"
            );
            let _ = fs::remove_dir_all(dir);
        }
    }
}
