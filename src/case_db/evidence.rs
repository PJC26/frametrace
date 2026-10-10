use super::*;
use crate::util::now_unix;
use rusqlite::params;
use std::path::Path;

pub fn register_evidence_source(
    case_dir: &Path,
    input: &EvidenceSourceInput,
) -> Result<EvidenceSourceRow, String> {
    let conn = open_case_db(case_dir)?;
    init_schema(&conn)?;
    let now = now_unix()?;
    let path = input.path.to_string_lossy().to_string();
    let source_id = input
        .source_id
        .clone()
        .unwrap_or_else(|| stable_source_id(&input.kind, &path));
    conn.execute(
        r#"
        INSERT INTO evidence_sources (
            source_id,
            kind,
            path,
            registered_unix,
            last_seen_unix,
            write_protect,
            acquisition_tool,
            evidence_hash,
            notes,
            metadata_json
        )
        VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, ?7, ?8, ?9)
        ON CONFLICT(source_id) DO UPDATE SET
            kind = excluded.kind,
            path = excluded.path,
            last_seen_unix = excluded.last_seen_unix,
            write_protect = COALESCE(excluded.write_protect, evidence_sources.write_protect),
            acquisition_tool = COALESCE(excluded.acquisition_tool, evidence_sources.acquisition_tool),
            evidence_hash = COALESCE(excluded.evidence_hash, evidence_sources.evidence_hash),
            notes = COALESCE(excluded.notes, evidence_sources.notes),
            metadata_json = excluded.metadata_json
        ON CONFLICT(kind, path) DO UPDATE SET
            last_seen_unix = excluded.last_seen_unix,
            write_protect = COALESCE(excluded.write_protect, evidence_sources.write_protect),
            acquisition_tool = COALESCE(excluded.acquisition_tool, evidence_sources.acquisition_tool),
            evidence_hash = COALESCE(excluded.evidence_hash, evidence_sources.evidence_hash),
            notes = COALESCE(excluded.notes, evidence_sources.notes),
            metadata_json = excluded.metadata_json
        "#,
        params![
            source_id.as_str(),
            input.kind.as_str(),
            path.as_str(),
            u64_to_i64(now),
            input.write_protect.as_deref(),
            input.acquisition_tool.as_deref(),
            input.evidence_hash.as_deref(),
            input.notes.as_deref(),
            input.metadata_json.as_deref().unwrap_or("{}"),
        ],
    )
    .map_err(|err| format!("failed to register evidence source: {err}"))?;

    conn.query_row(
        "SELECT source_id, kind, path FROM evidence_sources WHERE kind = ?1 AND path = ?2",
        params![input.kind.as_str(), path.as_str()],
        |row| {
            Ok(EvidenceSourceRow {
                source_id: row.get(0)?,
                kind: row.get(1)?,
                path: row.get(2)?,
            })
        },
    )
    .map_err(|err| format!("failed to read registered evidence source: {err}"))
}

pub(crate) fn stable_source_id(kind: &str, path: &str) -> String {
    let digest = crate::sha256::digest_bytes(format!("{kind}\n{path}").as_bytes());
    format!("src_{}", &digest[..16])
}

/// Examiner IN/OUT points in seconds. Either side may be unset. As a
/// patch value, `{in: null, out: null}` clears a stored range.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct RangeSpan {
    #[serde(rename = "in")]
    pub start: Option<f64>,
    #[serde(rename = "out")]
    pub end: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize)]
pub struct ReviewMarkRow {
    pub record_id: String,
    pub status: String,
    pub marked_unix: u64,
    pub record_path: Option<String>,
    pub examiner: Option<String>,
    pub note: Option<String>,
    /// `None` = leave any stored range untouched (older clients).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<RangeSpan>,
}

/// Upserts examiner review marks imported from the viewer's marks file.
/// Returns the number of stored rows.
pub fn upsert_review_marks(case_dir: &Path, marks: &[ReviewMarkRow]) -> Result<usize, String> {
    patch_review_annotations(case_dir, marks, &[], &[])
}

pub fn patch_review_annotations(
    case_dir: &Path,
    marks: &[ReviewMarkRow],
    tags: &[crate::selection::TagEntry],
    deleted_ids: &[String],
) -> Result<usize, String> {
    let mut conn = open_case_db(case_dir)?;
    init_schema(&conn)?;
    let tx = conn
        .transaction()
        .map_err(|err| format!("failed to start SQLite marks transaction: {err}"))?;
    for id in deleted_ids {
        tx.execute("DELETE FROM review_marks WHERE record_id = ?1", [id])
            .map_err(|err| format!("failed to remove review mark: {err}"))?;
    }
    for entry in tags {
        let key = format!("review_tags.v1:{}", entry.id);
        if entry.tags.is_empty() {
            tx.execute("DELETE FROM schema_meta WHERE key = ?1", [&key])
                .map_err(|err| format!("failed to remove review tags: {err}"))?;
        } else {
            let value = serde_json::to_string(&entry.tags).map_err(|err| err.to_string())?;
            tx.execute("INSERT INTO schema_meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", params![key, value])
                .map_err(|err| format!("failed to store review tags: {err}"))?;
        }
    }
    for mark in marks {
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
    }
    tx.commit()
        .map_err(|err| format!("failed to commit review marks: {err}"))?;
    Ok(marks.len())
}

pub fn load_review_tags(case_dir: &Path) -> Result<Vec<crate::selection::TagEntry>, String> {
    let path = case_db_path(case_dir);
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let conn = open_readonly_case_db(&path)?;
    if !table_exists(&conn, "schema_meta")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare("SELECT key, value FROM schema_meta WHERE substr(key, 1, 15) = 'review_tags.v1:' ORDER BY key")
        .map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|err| err.to_string())?;
    rows.map(|row| {
        let (key, value) = row.map_err(|err| err.to_string())?;
        Ok(crate::selection::TagEntry {
            id: key[15..].to_string(),
            tags: serde_json::from_str(&value)
                .map_err(|err| format!("invalid stored tags: {err}"))?,
        })
    })
    .collect()
}

pub fn load_review_marks(case_dir: &Path) -> Result<Vec<ReviewMarkRow>, String> {
    let path = case_db_path(case_dir);
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let conn = open_readonly_case_db(&path)?;
    if !table_exists(&conn, "review_marks")? {
        return Ok(Vec::new());
    }
    // Read-only connections skip init_schema, so a pre-v4 case db may not
    // have the note column yet — fall back to NULLs instead of failing.
    let note_col = if table_has_column(&conn, "review_marks", "note")? {
        "note"
    } else {
        "NULL"
    };
    let (in_col, out_col) = if table_has_column(&conn, "review_marks", "range_in")? {
        ("range_in", "range_out")
    } else {
        ("NULL", "NULL")
    };
    let sql = format!(
        "SELECT record_id, status, marked_unix, record_path, examiner, {note_col}, {in_col}, {out_col} FROM review_marks ORDER BY record_id"
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|err| format!("failed to prepare review marks query: {err}"))?;
    let rows = stmt
        .query_map([], |row| {
            let range_in: Option<f64> = row.get(6)?;
            let range_out: Option<f64> = row.get(7)?;
            Ok(ReviewMarkRow {
                record_id: row.get(0)?,
                status: row.get(1)?,
                marked_unix: i64_to_u64(row.get(2)?),
                record_path: row.get(3)?,
                examiner: row.get(4)?,
                note: row.get(5)?,
                range: (range_in.is_some() || range_out.is_some()).then_some(RangeSpan {
                    start: range_in,
                    end: range_out,
                }),
            })
        })
        .map_err(|err| format!("failed to query review marks: {err}"))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|err| format!("failed to read review mark row: {err}"))?);
    }
    Ok(out)
}

#[cfg(test)]
mod range_tests {
    use super::*;

    fn mark(range: Option<RangeSpan>) -> ReviewMarkRow {
        ReviewMarkRow {
            record_id: "vid_1".into(),
            status: "important".into(),
            marked_unix: 7,
            range,
            ..Default::default()
        }
    }

    #[test]
    fn range_roundtrips_and_absent_range_keeps_stored_value() {
        let dir = std::env::temp_dir().join(format!("frametrace-range-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("db")).unwrap();
        let span = RangeSpan {
            start: Some(1.25),
            end: Some(9.5),
        };
        patch_review_annotations(&dir, &[mark(Some(span))], &[], &[]).unwrap();
        assert_eq!(load_review_marks(&dir).unwrap()[0].range, Some(span));
        // Older client: no range key -> stored range untouched.
        patch_review_annotations(&dir, &[mark(None)], &[], &[]).unwrap();
        assert_eq!(load_review_marks(&dir).unwrap()[0].range, Some(span));
        // Explicit empty range clears it.
        let clear = RangeSpan {
            start: None,
            end: None,
        };
        patch_review_annotations(&dir, &[mark(Some(clear))], &[], &[]).unwrap();
        assert_eq!(load_review_marks(&dir).unwrap()[0].range, None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
