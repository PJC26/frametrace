//! Carrying review annotations across `merge-cases` (I2).
//!
//! A merge renumbers every copied record onto the target's `vid_NNNNNN`
//! sequence, so a source case's marks, notes, IN/OUT ranges, tags and mark
//! history are keyed by ids the target never had. This module re-keys them
//! (`original_id -> new_id`, where a record kept because its path already
//! existed maps to the target's existing id) and writes them into the
//! target database in ONE `BEGIN IMMEDIATE` transaction: either every
//! source's annotations land, or none do.
//!
//! Per-id rules (docs/FIX_PLAN_R1_2026-10-13.md §2.1):
//!
//! | target state | mark (status/time/examiner/range) | note | tags | history |
//! |---|---|---|---|---|
//! | no mark | source mark inserted, `record_path` = record's source path | as-is | union | copied + `merge` row |
//! | has mark | **target wins** (never overwritten) | non-empty differing source note appended as `\n[병합 <case> <orig_id>] <note>` once | union | copied + `merge-skipped` row whose `detail` keeps the full source mark JSON |
//!
//! Copied history rows keep their order, `changed_unix` and columns; only
//! `record_id` changes and `detail` names where they came from. Re-running
//! the same merge is idempotent: copied rows and boundary rows are matched
//! by their `detail`, and a note segment already present is not appended
//! again. Source case databases are opened read-only and never migrated;
//! a source predating schema v6 simply has no history to copy.

use super::*;
use crate::selection::TagEntry;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

/// One verbatim `review_mark_history` row of a source case.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SourceHistoryRow {
    pub history_pk: i64,
    pub record_id: String,
    pub action: String,
    pub changed_unix: i64,
    pub actor: Option<String>,
    pub old_status: Option<String>,
    pub new_status: Option<String>,
    pub old_marked_unix: Option<i64>,
    pub new_marked_unix: Option<i64>,
    pub old_record_path: Option<String>,
    pub new_record_path: Option<String>,
    pub old_examiner: Option<String>,
    pub new_examiner: Option<String>,
    pub old_note: Option<String>,
    pub new_note: Option<String>,
    pub old_range_in: Option<f64>,
    pub new_range_in: Option<f64>,
    pub old_range_out: Option<f64>,
    pub new_range_out: Option<f64>,
    pub old_tags_json: Option<String>,
    pub new_tags_json: Option<String>,
    pub detail: Option<String>,
}

/// Everything a source case knows about its records' review state.
#[derive(Debug, Clone, Default)]
pub struct SourceAnnotations {
    pub marks: Vec<ReviewMarkRow>,
    pub tags: Vec<TagEntry>,
    pub history: Vec<SourceHistoryRow>,
}

impl SourceAnnotations {
    pub fn is_empty(&self) -> bool {
        self.marks.is_empty() && self.tags.is_empty() && self.history.is_empty()
    }
}

/// Reads a source case's annotations without modifying (or migrating) its
/// database. A case with no database has no annotations; an unreadable
/// database is an error, so a merge never silently drops marks.
pub fn load_source_annotations(case_dir: &Path) -> Result<SourceAnnotations, String> {
    let path = case_db_path(case_dir);
    if !path.is_file() {
        return Ok(SourceAnnotations::default());
    }
    let conn = open_readonly_case_db(&path)?;
    let context = |what: &str, err: String| {
        format!(
            "failed to read source {what} from {}: {err}",
            path.display()
        )
    };
    Ok(SourceAnnotations {
        marks: load_review_marks_from(&conn).map_err(|err| context("review marks", err))?,
        tags: load_review_tags_from(&conn).map_err(|err| context("review tags", err))?,
        history: load_source_history(&conn).map_err(|err| context("mark history", err))?,
    })
}

fn load_source_history(conn: &Connection) -> Result<Vec<SourceHistoryRow>, String> {
    if !table_exists(conn, "review_mark_history")? {
        return Ok(Vec::new());
    }
    const COLUMNS: [&str; 22] = [
        "history_pk",
        "record_id",
        "action",
        "changed_unix",
        "actor",
        "old_status",
        "new_status",
        "old_marked_unix",
        "new_marked_unix",
        "old_record_path",
        "new_record_path",
        "old_examiner",
        "new_examiner",
        "old_note",
        "new_note",
        "old_range_in",
        "new_range_in",
        "old_range_out",
        "new_range_out",
        "old_tags_json",
        "new_tags_json",
        "detail",
    ];
    let mut select = Vec::new();
    for column in COLUMNS {
        if table_has_column(conn, "review_mark_history", column)? {
            select.push(column.to_string());
        } else {
            select.push("NULL".to_string());
        }
    }
    let sql = format!(
        "SELECT {} FROM review_mark_history ORDER BY history_pk",
        select.join(", ")
    );
    let mut stmt = conn.prepare(&sql).map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok(SourceHistoryRow {
                history_pk: row.get(0)?,
                record_id: row.get(1)?,
                action: row.get(2)?,
                changed_unix: row.get(3)?,
                actor: row.get(4)?,
                old_status: row.get(5)?,
                new_status: row.get(6)?,
                old_marked_unix: row.get(7)?,
                new_marked_unix: row.get(8)?,
                old_record_path: row.get(9)?,
                new_record_path: row.get(10)?,
                old_examiner: row.get(11)?,
                new_examiner: row.get(12)?,
                old_note: row.get(13)?,
                new_note: row.get(14)?,
                old_range_in: row.get(15)?,
                new_range_in: row.get(16)?,
                old_range_out: row.get(17)?,
                new_range_out: row.get(18)?,
                old_tags_json: row.get(19)?,
                new_tags_json: row.get(20)?,
                detail: row.get(21)?,
            })
        })
        .map_err(|err| err.to_string())?;
    rows.map(|row| row.map_err(|err| err.to_string())).collect()
}

/// Where one source record landed in the target index.
#[derive(Debug, Clone, PartialEq)]
pub struct MergedIdMapping {
    pub original_id: String,
    /// The renumbered id, or the target's existing id when the record's
    /// path was already indexed (a "kept" record).
    pub new_id: String,
    pub source_path: String,
}

/// One source case's annotations with its id mapping.
pub struct SourceAnnotationMerge<'a> {
    /// Case path as recorded in `merged_from` and the merge audit line.
    pub source_case: String,
    pub mapping: &'a [MergedIdMapping],
    pub annotations: &'a SourceAnnotations,
}

/// Per-source effect of an annotation merge (audit line + CLI summary).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnnotationMergeCounts {
    /// Source marks inserted for ids that had no target mark.
    pub marks_merged: usize,
    /// Ids whose tag set grew by the union.
    pub tags_merged: usize,
    /// Source notes appended to an existing target note.
    pub notes_appended: usize,
    /// Source marks not applied because the target mark differs (target
    /// wins); the source mark is preserved in a `merge-skipped` history row.
    pub marks_conflicted: usize,
    pub history_rows_copied: usize,
    /// Source ids with annotations but no record in the source index (so no
    /// merged record to attach to). Reported, never silently dropped.
    pub annotations_unmapped: usize,
}

#[derive(Debug, Clone, Default)]
pub struct AnnotationMergeOutcome {
    pub per_source: Vec<AnnotationMergeCounts>,
    /// Target ids whose stored annotations changed, with whether the id had
    /// no annotations before (revision 1) — input for the revision table.
    pub changed_ids: Vec<(String, bool)>,
}

#[derive(Debug, Clone, PartialEq)]
struct TargetMark {
    status: String,
    marked_unix: i64,
    record_path: Option<String>,
    examiner: Option<String>,
    note: Option<String>,
    range_in: Option<f64>,
    range_out: Option<f64>,
}

impl TargetMark {
    fn from_source(mark: &ReviewMarkRow, record_path: &str) -> Self {
        let (range_in, range_out) = mark.range.map_or((None, None), |r| (r.start, r.end));
        Self {
            status: mark.status.clone(),
            marked_unix: u64_to_i64(mark.marked_unix),
            record_path: Some(record_path.to_string()),
            examiner: mark.examiner.clone(),
            note: mark.note.clone(),
            range_in,
            range_out,
        }
    }
}

fn read_target_mark(tx: &Transaction<'_>, record_id: &str) -> Result<Option<TargetMark>, String> {
    tx.query_row(
        "SELECT status, marked_unix, record_path, examiner, note, range_in, range_out FROM review_marks WHERE record_id = ?1",
        [record_id],
        |row| {
            Ok(TargetMark {
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
    .map_err(|err| format!("failed to read target review mark {record_id}: {err}"))
}

fn read_target_tags(tx: &Transaction<'_>, record_id: &str) -> Result<Option<String>, String> {
    tx.query_row(
        "SELECT value FROM schema_meta WHERE key = ?1",
        [format!("review_tags.v1:{record_id}")],
        |row| row.get(0),
    )
    .optional()
    .map_err(|err| format!("failed to read target review tags {record_id}: {err}"))
}

fn history_detail_exists(
    tx: &Transaction<'_>,
    record_id: &str,
    action: Option<&str>,
    detail: &str,
    allow_suffix: bool,
) -> Result<bool, String> {
    let prefix = format!("{detail}\n");
    tx.query_row(
        "SELECT 1 FROM review_mark_history WHERE record_id = ?1 AND (?2 IS NULL OR action = ?2) AND (detail = ?3 OR (?4 AND substr(detail, 1, length(?5)) = ?5)) LIMIT 1",
        params![record_id, action, detail, allow_suffix, prefix],
        |_| Ok(()),
    )
    .optional()
    .map(|found| found.is_some())
    .map_err(|err| format!("failed to read target mark history for {record_id}: {err}"))
}

/// The idempotency key of one copied history row.
fn copied_history_key(source_case: &str, original_id: &str, history_pk: i64) -> String {
    format!("merged-history from {source_case} record {original_id} history_pk {history_pk}")
}

/// The note segment appended for a conflicting source note.
pub fn merged_note_segment(source_case: &str, original_id: &str, note: &str) -> String {
    format!("[병합 {source_case} {original_id}] {note}")
}

/// True when `segment` (possibly several lines) already appears in `note`
/// as whole lines — a run of consecutive lines exactly equal to the
/// segment's lines. A plain substring test would treat
/// `[병합 C vid_1] a` as present inside `[병합 C vid_1] ab` and silently
/// drop the second note (contract §10.2).
fn note_has_segment(note: &str, segment: &str) -> bool {
    let wanted: Vec<&str> = segment.split('\n').collect();
    let lines: Vec<&str> = note.split('\n').collect();
    lines.len() >= wanted.len()
        && lines
            .windows(wanted.len())
            .any(|window| window == wanted.as_slice())
}

/// Hook for the per-record revision table (`review_annotation_rev`, schema
/// v7, track A of docs/FIX_PLAN_R1_2026-10-13.md).
///
/// Bumps the record's annotation revision inside the merge transaction so
/// open viewers see the merged value as a server-side change (a stale
/// client save then comes back as a conflict instead of overwriting it).
/// A record with no revision row yet starts at 1; an existing row (live or
/// tombstone) goes up by one. Merges have no client: actor/client_id NULL.
fn bump_annotation_revision(
    tx: &Transaction<'_>,
    record_id: &str,
    _created: bool,
    now: i64,
) -> Result<(), String> {
    tx.execute(
        "INSERT INTO review_annotation_rev (record_id, revision, updated_unix, actor, client_id)
         VALUES (?1, 1, ?2, NULL, NULL)
         ON CONFLICT(record_id) DO UPDATE SET
             revision = review_annotation_rev.revision + 1,
             updated_unix = excluded.updated_unix,
             actor = NULL,
             client_id = NULL",
        rusqlite::params![record_id, now],
    )
    .map_err(|err| format!("failed to bump annotation revision for {record_id}: {err}"))?;
    Ok(())
}

/// Writes every source's annotations into `case_dir`'s database in one
/// transaction. Any failure rolls the whole transaction back and returns
/// the error: no mark, tag, note or history row from any source is kept.
pub fn apply_merge_annotations(
    case_dir: &Path,
    sources: &[SourceAnnotationMerge<'_>],
    now_unix: u64,
) -> Result<AnnotationMergeOutcome, String> {
    if sources.iter().all(|source| source.annotations.is_empty()) {
        return Ok(AnnotationMergeOutcome {
            per_source: vec![AnnotationMergeCounts::default(); sources.len()],
            changed_ids: Vec::new(),
        });
    }
    let mut conn = open_case_db(case_dir)?;
    init_schema(&conn)?;
    let now = u64_to_i64(now_unix);
    let tx = begin_write(&mut conn)
        .map_err(|err| format!("failed to start merge annotation transaction: {err}"))?;
    let mut outcome = AnnotationMergeOutcome::default();
    for source in sources {
        let counts = merge_one_source(&tx, source, now, &mut outcome.changed_ids)?;
        outcome.per_source.push(counts);
    }
    tx.commit()
        .map_err(|err| format!("failed to commit merged annotations: {err}"))?;
    Ok(outcome)
}

fn merge_one_source(
    tx: &Transaction<'_>,
    source: &SourceAnnotationMerge<'_>,
    now: i64,
    changed_ids: &mut Vec<(String, bool)>,
) -> Result<AnnotationMergeCounts, String> {
    let mut counts = AnnotationMergeCounts::default();
    let mut mapping: HashMap<&str, &MergedIdMapping> = HashMap::new();
    for entry in source.mapping {
        mapping.entry(entry.original_id.as_str()).or_insert(entry);
    }
    let marks: HashMap<&str, &ReviewMarkRow> = source
        .annotations
        .marks
        .iter()
        .map(|mark| (mark.record_id.as_str(), mark))
        .collect();
    let mut tags: HashMap<&str, BTreeSet<&str>> = HashMap::new();
    for entry in &source.annotations.tags {
        tags.entry(entry.id.as_str())
            .or_default()
            .extend(entry.tags.iter().map(String::as_str));
    }
    let mut history: HashMap<&str, Vec<&SourceHistoryRow>> = HashMap::new();
    for row in &source.annotations.history {
        history.entry(row.record_id.as_str()).or_default().push(row);
    }
    let ids: BTreeSet<&str> = marks
        .keys()
        .chain(tags.keys())
        .chain(history.keys())
        .copied()
        .collect();

    for original_id in ids {
        let Some(target) = mapping.get(original_id) else {
            counts.annotations_unmapped += 1;
            continue;
        };
        let new_id = target.new_id.as_str();
        let source_mark = marks.get(original_id).copied();
        let source_tags: BTreeSet<&str> = tags.get(original_id).cloned().unwrap_or_default();

        let old_mark = read_target_mark(tx, new_id)?;
        let old_tags_json = read_target_tags(tx, new_id)?;
        let created = old_mark.is_none() && old_tags_json.is_none();
        let mut new_mark = old_mark.clone();
        let mut conflicted = false;
        let mut changed = false;

        // 1. Mark and note: target wins; a new id takes the source mark.
        if let Some(source_mark) = source_mark {
            match &old_mark {
                None => {
                    let inserted = TargetMark::from_source(source_mark, &target.source_path);
                    tx.execute(
                        "INSERT INTO review_marks (record_id, status, marked_unix, record_path, examiner, note, range_in, range_out) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        params![
                            new_id,
                            inserted.status,
                            inserted.marked_unix,
                            inserted.record_path,
                            inserted.examiner,
                            inserted.note,
                            inserted.range_in,
                            inserted.range_out,
                        ],
                    )
                    .map_err(|err| format!("failed to insert merged review mark {new_id}: {err}"))?;
                    new_mark = Some(inserted);
                    counts.marks_merged += 1;
                    changed = true;
                }
                Some(existing) => {
                    let incoming = TargetMark::from_source(source_mark, &target.source_path);
                    let source_note = source_mark
                        .note
                        .as_deref()
                        .filter(|note| !note.trim().is_empty());
                    let note_differs =
                        source_note.is_some_and(|note| existing.note.as_deref() != Some(note));
                    conflicted = existing.status != incoming.status
                        || existing.marked_unix != incoming.marked_unix
                        || existing.examiner != incoming.examiner
                        || existing.range_in != incoming.range_in
                        || existing.range_out != incoming.range_out
                        || note_differs;
                    if let Some(note) = source_note.filter(|_| note_differs) {
                        let segment = merged_note_segment(&source.source_case, original_id, note);
                        let current = existing.note.as_deref().unwrap_or_default();
                        if !note_has_segment(current, &segment) {
                            let combined = if current.trim().is_empty() {
                                segment
                            } else {
                                format!("{current}\n{segment}")
                            };
                            tx.execute(
                                "UPDATE review_marks SET note = ?2 WHERE record_id = ?1",
                                params![new_id, combined],
                            )
                            .map_err(|err| {
                                format!("failed to append merged note to {new_id}: {err}")
                            })?;
                            if let Some(mark) = new_mark.as_mut() {
                                mark.note = Some(combined);
                            }
                            counts.notes_appended += 1;
                            changed = true;
                        }
                    }
                }
            }
        }
        if conflicted {
            counts.marks_conflicted += 1;
        }

        // 2. Tags: union, sorted and de-duplicated.
        let mut new_tags_json = old_tags_json.clone();
        if !source_tags.is_empty() {
            let stored: Vec<String> = match &old_tags_json {
                Some(text) => serde_json::from_str(text)
                    .map_err(|err| format!("invalid stored tags for {new_id}: {err}"))?,
                None => Vec::new(),
            };
            let stored_set: BTreeSet<&str> = stored.iter().map(String::as_str).collect();
            let union: BTreeSet<&str> = stored_set.union(&source_tags).copied().collect();
            if union != stored_set {
                let value = serde_json::to_string(&union.iter().collect::<Vec<_>>())
                    .map_err(|err| err.to_string())?;
                tx.execute(
                    "INSERT INTO schema_meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![format!("review_tags.v1:{new_id}"), value],
                )
                .map_err(|err| format!("failed to store merged tags for {new_id}: {err}"))?;
                new_tags_json = Some(value);
                counts.tags_merged += 1;
                changed = true;
            }
        }

        // 3. History: source rows copied once, in their original order.
        let mut copied = 0usize;
        for row in history
            .get(original_id)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let key = copied_history_key(&source.source_case, original_id, row.history_pk);
            if history_detail_exists(tx, new_id, None, &key, true)? {
                continue;
            }
            let detail = match &row.detail {
                Some(original) => format!("{key}\noriginal detail: {original}"),
                None => key,
            };
            tx.execute(
                r#"
                INSERT INTO review_mark_history (
                    record_id, action, changed_unix, actor,
                    old_status, new_status, old_marked_unix, new_marked_unix,
                    old_record_path, new_record_path, old_examiner, new_examiner,
                    old_note, new_note, old_range_in, new_range_in, old_range_out, new_range_out,
                    old_tags_json, new_tags_json, detail
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
                "#,
                params![
                    new_id,
                    row.action,
                    row.changed_unix,
                    row.actor,
                    row.old_status,
                    row.new_status,
                    row.old_marked_unix,
                    row.new_marked_unix,
                    row.old_record_path,
                    row.new_record_path,
                    row.old_examiner,
                    row.new_examiner,
                    row.old_note,
                    row.new_note,
                    row.old_range_in,
                    row.new_range_in,
                    row.old_range_out,
                    row.new_range_out,
                    row.old_tags_json,
                    row.new_tags_json,
                    detail,
                ],
            )
            .map_err(|err| format!("failed to copy merged mark history to {new_id}: {err}"))?;
            copied += 1;
        }
        counts.history_rows_copied += copied;

        // 4. Boundary row: where the merge happened and, when the target
        //    won, the full source mark it did not apply.
        let action = if conflicted { "merge-skipped" } else { "merge" };
        let detail = serde_json::json!({
            "event": "merge-cases",
            "source_case": source.source_case,
            "original_id": original_id,
            "new_id": new_id,
            "source_mark": source_mark,
            "source_tags": (!source_tags.is_empty()).then_some(&source_tags),
        })
        .to_string();
        let needed = changed
            || copied > 0
            || (conflicted && !history_detail_exists(tx, new_id, Some(action), &detail, false)?);
        if needed {
            let tag_change = (old_tags_json != new_tags_json).then_some(());
            tx.execute(
                r#"
                INSERT INTO review_mark_history (
                    record_id, action, changed_unix, actor,
                    old_status, new_status, old_marked_unix, new_marked_unix,
                    old_record_path, new_record_path, old_examiner, new_examiner,
                    old_note, new_note, old_range_in, new_range_in, old_range_out, new_range_out,
                    old_tags_json, new_tags_json, detail
                ) VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
                "#,
                params![
                    new_id,
                    action,
                    now,
                    old_mark.as_ref().map(|m| m.status.as_str()),
                    new_mark.as_ref().map(|m| m.status.as_str()),
                    old_mark.as_ref().map(|m| m.marked_unix),
                    new_mark.as_ref().map(|m| m.marked_unix),
                    old_mark.as_ref().and_then(|m| m.record_path.as_deref()),
                    new_mark.as_ref().and_then(|m| m.record_path.as_deref()),
                    old_mark.as_ref().and_then(|m| m.examiner.as_deref()),
                    new_mark.as_ref().and_then(|m| m.examiner.as_deref()),
                    old_mark.as_ref().and_then(|m| m.note.as_deref()),
                    new_mark.as_ref().and_then(|m| m.note.as_deref()),
                    old_mark.as_ref().and_then(|m| m.range_in),
                    new_mark.as_ref().and_then(|m| m.range_in),
                    old_mark.as_ref().and_then(|m| m.range_out),
                    new_mark.as_ref().and_then(|m| m.range_out),
                    tag_change.and(old_tags_json.as_deref()),
                    tag_change.and(new_tags_json.as_deref()),
                    detail,
                ],
            )
            .map_err(|err| format!("failed to record merge history for {new_id}: {err}"))?;
        }
        if changed {
            bump_annotation_revision(tx, new_id, created, now)?;
            changed_ids.push((new_id.to_string(), created));
        }
    }
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn case(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "frametrace-merge-annotations-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("db")).unwrap();
        dir
    }

    fn mark(id: &str, status: &str, note: Option<&str>) -> ReviewMarkRow {
        ReviewMarkRow {
            record_id: id.into(),
            status: status.into(),
            marked_unix: 11,
            examiner: Some("kim".into()),
            note: note.map(str::to_string),
            range: Some(RangeSpan {
                start: Some(1.5),
                end: Some(4.0),
            }),
            ..Default::default()
        }
    }

    fn mapping(original: &str, new: &str) -> Vec<MergedIdMapping> {
        vec![MergedIdMapping {
            original_id: original.into(),
            new_id: new.into(),
            source_path: "/ev/a.mp4".into(),
        }]
    }

    #[test]
    fn source_without_database_or_history_table_loads_empty_history() {
        let none = case("no-db");
        assert!(load_source_annotations(&none).unwrap().is_empty());
        // A v5 source (no history table) is read without being migrated.
        let v5 = case("v5-src");
        let seed = Connection::open(case_db_path(&v5)).unwrap();
        seed.execute_batch(
            r#"
            CREATE TABLE schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            INSERT INTO schema_meta (key, value) VALUES ('schema_version', '5');
            INSERT INTO schema_meta (key, value) VALUES ('review_tags.v1:vid_000001', '["사고"]');
            CREATE TABLE review_marks (
                record_id TEXT PRIMARY KEY, status TEXT NOT NULL,
                marked_unix INTEGER NOT NULL, record_path TEXT, examiner TEXT,
                note TEXT, range_in REAL, range_out REAL
            );
            INSERT INTO review_marks (record_id, status, marked_unix, note, range_in)
            VALUES ('vid_000001', 'important', 5, 'v5 note', 2.0);
            "#,
        )
        .unwrap();
        drop(seed);
        let before = fs::read(case_db_path(&v5)).unwrap();
        let loaded = load_source_annotations(&v5).unwrap();
        assert_eq!(loaded.marks.len(), 1);
        assert_eq!(loaded.tags.len(), 1);
        assert!(loaded.history.is_empty());
        assert_eq!(
            fs::read(case_db_path(&v5)).unwrap(),
            before,
            "source untouched"
        );

        let target = case("v5-target");
        let outcome = apply_merge_annotations(
            &target,
            &[SourceAnnotationMerge {
                source_case: "/cases/v5".into(),
                mapping: &mapping("vid_000001", "vid_000003"),
                annotations: &loaded,
            }],
            100,
        )
        .unwrap();
        assert_eq!(outcome.per_source[0].marks_merged, 1);
        assert_eq!(outcome.per_source[0].tags_merged, 1);
        assert_eq!(outcome.per_source[0].history_rows_copied, 0);
        let marks = load_review_marks(&target).unwrap();
        assert_eq!(marks[0].record_id, "vid_000003");
        assert_eq!(marks[0].note.as_deref(), Some("v5 note"));
        assert_eq!(marks[0].record_path.as_deref(), Some("/ev/a.mp4"));
        let history = load_review_mark_history(&target, Some("vid_000003")).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].action, "merge");
        assert_eq!(outcome.changed_ids, vec![("vid_000003".to_string(), true)]);
        // The merge bumps the revision in the same transaction (new id → 1).
        let revisions = load_review_revisions(&target).unwrap();
        assert_eq!(revisions.get("vid_000003").copied(), Some(1));
        for dir in [none, v5, target] {
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn unmapped_source_annotations_are_counted_not_written() {
        let target = case("unmapped");
        let annotations = SourceAnnotations {
            marks: vec![mark("vid_000099", "important", None)],
            ..Default::default()
        };
        let outcome = apply_merge_annotations(
            &target,
            &[SourceAnnotationMerge {
                source_case: "/cases/s".into(),
                mapping: &mapping("vid_000001", "vid_000002"),
                annotations: &annotations,
            }],
            100,
        )
        .unwrap();
        assert_eq!(outcome.per_source[0].annotations_unmapped, 1);
        assert!(load_review_marks(&target).unwrap().is_empty());
        let _ = fs::remove_dir_all(target);
    }

    #[test]
    fn identical_existing_mark_is_not_a_conflict_and_writes_nothing() {
        let target = case("identical");
        let source_mark = mark("vid_000001", "important", Some("same"));
        let annotations = SourceAnnotations {
            marks: vec![source_mark.clone()],
            ..Default::default()
        };
        let plan = [SourceAnnotationMerge {
            source_case: "/cases/s".into(),
            mapping: &mapping("vid_000001", "vid_000002"),
            annotations: &annotations,
        }];
        apply_merge_annotations(&target, &plan, 1).unwrap();
        let rows = load_review_mark_history(&target, None).unwrap().len();
        let again = apply_merge_annotations(&target, &plan, 2).unwrap();
        assert_eq!(again.per_source[0], AnnotationMergeCounts::default());
        assert!(again.changed_ids.is_empty());
        assert_eq!(load_review_mark_history(&target, None).unwrap().len(), rows);
        let _ = fs::remove_dir_all(target);
    }
}
