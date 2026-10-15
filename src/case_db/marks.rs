//! Review mark writes with append-only history (FOR-07, schema v6) and
//! per-record revisions for save protocol v2 (schema v7).
//!
//! Marks, tags and deletions stay mutable in `review_marks` /
//! `schema_meta` (the viewer reads current state from there), but every
//! effective change is also appended to `review_mark_history` inside the
//! SAME transaction, so a mark can never change without its history. The
//! history table rejects UPDATE/DELETE via triggers.
//!
//! `review_annotation_rev` holds one revision per record id covering the
//! mark, note, examiner, IN/OUT range and tags together. A v2 client sends
//! the revision it last saw per id (contract §10.1 of
//! docs/FIX_PLAN_R1_2026-10-13.md): an id whose base differs from the
//! stored revision (older or newer) is NOT applied and is returned as a
//! conflict with the server's current value — unless the same window
//! (`client_id`) wrote the stored revision, or the patch would not change
//! the stored value at all (then it is applied at the current revision).
//! The other ids in the same patch are applied. Legacy bodies (no base)
//! never conflict (last write wins) but still bump the revision so v2
//! windows notice the change.

use super::*;
use crate::util::now_unix;
use rusqlite::{OptionalExtension, Transaction, params};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

/// R5 contract (U1 ↔ P): the base a viewer sends for a draft it made while
/// it could not trust its own view (read-only state, `unsafe_base`) — the
/// wire value `-1`, which the patch-v2 parser maps here. Such a draft is
/// ALWAYS a conflict, even from the window that wrote the stored revision
/// (§10.1.2 does not apply), unless its content is identical to the stored
/// value (§10.1.3: then it changes nothing and is applied at the current
/// revision). No stored revision ever reaches this value.
pub const UNSAFE_BASE_REVISION: u64 = u64::MAX;

/// One patch body as the server applies it. `base_revisions = None` is a
/// legacy (patch-v1 / schema 1·2) body: no conflict check. `Some(map)` is
/// a v2 body; an id missing from the map has base 0, so a v2 body that
/// omits `base_revisions` entirely conflicts with every id the server
/// already revised instead of overwriting it.
#[derive(Debug, Clone, Copy)]
pub struct AnnotationPatch<'a> {
    pub marks: &'a [ReviewMarkRow],
    pub tags: &'a [crate::selection::TagEntry],
    pub deleted_ids: &'a [String],
    pub base_revisions: Option<&'a BTreeMap<String, u64>>,
}

/// The server's current annotation for one record, returned with a
/// conflict so the viewer can show both sides. `status`/`marked_unix` are
/// `None` when the record has tags but no mark.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ServerAnnotation {
    pub status: Option<String>,
    pub marked_unix: Option<u64>,
    pub examiner: Option<String>,
    pub note: Option<String>,
    pub range: Option<RangeSpan>,
    pub tags: Vec<String>,
}

/// An id the patch did not apply because the client's base revision is
/// older than the stored one. `server` is `None` when the server holds
/// neither a mark nor tags for the id any more (deleted — the revision
/// row is a tombstone).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AnnotationConflict {
    pub id: String,
    pub server_revision: u64,
    pub server: Option<ServerAnnotation>,
}

/// Result of one patch: every non-conflicting id with its revision after
/// the patch (unchanged ids keep their revision — resending is
/// idempotent), the conflicts, and how many mark rows were upserted.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PatchOutcome {
    pub applied: Vec<(String, u64)>,
    pub conflicts: Vec<AnnotationConflict>,
    pub stored: usize,
}

impl PatchOutcome {
    /// `applied` as the `{id: revision}` map the API returns.
    pub fn applied_map(&self) -> BTreeMap<String, u64> {
        self.applied.iter().cloned().collect()
    }
}

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

/// Who/when/what wrote a history row: the acting examiner and the v2
/// writer token (`client:<id>`, `None` for legacy/CLI writers).
#[derive(Clone, Copy)]
struct WriteContext<'a> {
    now: i64,
    actor: Option<&'a str>,
    detail: Option<&'a str>,
}

fn insert_mark_history(
    tx: &Transaction<'_>,
    record_id: &str,
    action: &str,
    ctx: WriteContext<'_>,
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
            old_note, new_note, old_range_in, new_range_in, old_range_out, new_range_out,
            detail
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
        "#,
        params![
            record_id,
            action,
            ctx.now,
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
            ctx.detail,
        ],
    )
    .map_err(|err| format!("failed to record review mark history for {record_id}: {err}"))?;
    Ok(())
}

/// `(revision, client_id of its writer)`; `(0, None)` without a row.
fn read_revision_row(
    tx: &Transaction<'_>,
    record_id: &str,
) -> Result<(u64, Option<String>), String> {
    tx.query_row(
        "SELECT revision, client_id FROM review_annotation_rev WHERE record_id = ?1",
        [record_id],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
    )
    .optional()
    .map(|row| {
        row.map(|(rev, client)| (i64_to_u64(rev), client))
            .unwrap_or((0, None))
    })
    .map_err(|err| format!("failed to read annotation revision for {record_id}: {err}"))
}

fn read_stored_tags(tx: &Transaction<'_>, record_id: &str) -> Result<Option<String>, String> {
    tx.query_row(
        "SELECT value FROM schema_meta WHERE key = ?1",
        [format!("review_tags.v1:{record_id}")],
        |row| row.get(0),
    )
    .optional()
    .map_err(|err| format!("failed to read review tags for {record_id}: {err}"))
}

fn read_server_annotation(
    tx: &Transaction<'_>,
    record_id: &str,
) -> Result<Option<ServerAnnotation>, String> {
    let mark = read_stored_mark(tx, record_id)?;
    let tags: Vec<String> = match read_stored_tags(tx, record_id)? {
        Some(json) => serde_json::from_str(&json)
            .map_err(|err| format!("invalid stored tags for {record_id}: {err}"))?,
        None => Vec::new(),
    };
    if mark.is_none() && tags.is_empty() {
        return Ok(None);
    }
    let range = mark.as_ref().and_then(|m| {
        (m.range_in.is_some() || m.range_out.is_some()).then_some(RangeSpan {
            start: m.range_in,
            end: m.range_out,
        })
    });
    Ok(Some(ServerAnnotation {
        status: mark.as_ref().map(|m| m.status.clone()),
        marked_unix: mark.as_ref().map(|m| i64_to_u64(m.marked_unix)),
        examiner: mark.as_ref().and_then(|m| m.examiner.clone()),
        note: mark.as_ref().and_then(|m| m.note.clone()),
        range,
        tags,
    }))
}

/// Removes the mark row (tags are separate); returns whether a row went.
fn delete_mark(tx: &Transaction<'_>, id: &str, ctx: WriteContext<'_>) -> Result<bool, String> {
    let Some(old) = read_stored_mark(tx, id)? else {
        return Ok(false);
    };
    tx.execute("DELETE FROM review_marks WHERE record_id = ?1", [id])
        .map_err(|err| format!("failed to remove review mark: {err}"))?;
    insert_mark_history(tx, id, "delete", ctx, ctx.actor, Some(&old), None)?;
    Ok(true)
}

/// Stores (or, for an empty list, removes) a record's tag list; returns
/// whether the stored value changed.
fn store_tags(
    tx: &Transaction<'_>,
    entry: &crate::selection::TagEntry,
    ctx: WriteContext<'_>,
) -> Result<bool, String> {
    let key = format!("review_tags.v1:{}", entry.id);
    let old_tags = read_stored_tags(tx, &entry.id)?;
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
    if old_tags == new_tags {
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO review_mark_history (record_id, action, changed_unix, actor, old_tags_json, new_tags_json, detail) VALUES (?1, 'tags', ?2, ?3, ?4, ?5, ?6)",
        params![entry.id, ctx.now, ctx.actor, old_tags, new_tags, ctx.detail],
    )
    .map_err(|err| format!("failed to record review tag history for {}: {err}", entry.id))?;
    Ok(true)
}

/// The row `mark` leaves behind when written over `old` — the same rules
/// the upsert applies: a missing `record_path`/`examiner`/`note` keeps the
/// stored value, `range: None` keeps the stored range, and a `noted` row
/// that stays `noted` keeps its stored `marked_unix` (the viewer stamps a
/// note-only row with "now" on every export, which is not a change;
/// contract §10.1.4).
fn merged_mark(old: Option<&StoredMark>, mark: &ReviewMarkRow) -> StoredMark {
    let keep_time = old.is_some_and(|old| old.status == "noted" && mark.status == "noted");
    StoredMark {
        status: mark.status.clone(),
        marked_unix: match old {
            Some(old) if keep_time => old.marked_unix,
            _ => u64_to_i64(mark.marked_unix),
        },
        record_path: mark
            .record_path
            .clone()
            .or_else(|| old.and_then(|o| o.record_path.clone())),
        examiner: mark
            .examiner
            .clone()
            .or_else(|| old.and_then(|o| o.examiner.clone())),
        note: mark
            .note
            .clone()
            .or_else(|| old.and_then(|o| o.note.clone())),
        range_in: match mark.range {
            Some(range) => range.start,
            None => old.and_then(|o| o.range_in),
        },
        range_out: match mark.range {
            Some(range) => range.end,
            None => old.and_then(|o| o.range_out),
        },
    }
}

/// Upserts one mark; returns whether the stored row changed.
fn upsert_mark(
    tx: &Transaction<'_>,
    mark: &ReviewMarkRow,
    ctx: WriteContext<'_>,
) -> Result<bool, String> {
    let old = read_stored_mark(tx, &mark.record_id)?;
    let new = merged_mark(old.as_ref(), mark);
    if old.as_ref() == Some(&new) {
        return Ok(false);
    }
    tx.execute(
        r#"
        INSERT INTO review_marks (record_id, status, marked_unix, record_path, examiner, note, range_in, range_out)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
        ON CONFLICT(record_id) DO UPDATE SET
            status = excluded.status,
            marked_unix = excluded.marked_unix,
            record_path = excluded.record_path,
            examiner = excluded.examiner,
            note = excluded.note,
            range_in = excluded.range_in,
            range_out = excluded.range_out
        "#,
        params![
            mark.record_id.as_str(),
            new.status.as_str(),
            new.marked_unix,
            new.record_path.as_deref(),
            new.examiner.as_deref(),
            new.note.as_deref(),
            new.range_in,
            new.range_out,
        ],
    )
    .map_err(|err| format!("failed to upsert review mark {}: {err}", mark.record_id))?;
    let action = if old.is_some() { "update" } else { "insert" };
    insert_mark_history(
        tx,
        &mark.record_id,
        action,
        ctx,
        mark.examiner.as_deref().or(ctx.actor),
        old.as_ref(),
        Some(&new),
    )?;
    Ok(true)
}

/// The tag list as stored in `schema_meta` (`None` = no tags).
fn tags_storage_value(tags: &[String]) -> Result<Option<String>, String> {
    if tags.is_empty() {
        Ok(None)
    } else {
        serde_json::to_string(tags)
            .map(Some)
            .map_err(|err| err.to_string())
    }
}

/// True when applying this id's part of the patch would leave the stored
/// annotation exactly as it is (status, note, examiner, IN/OUT range, tags
/// and deletion — contract §10.1.3). `marked_unix` is ignored when the
/// status stays the same (§10.1.4): a re-stamped time alone is no change.
fn patch_is_noop(
    tx: &Transaction<'_>,
    id: &str,
    mark: Option<&ReviewMarkRow>,
    tags: Option<&crate::selection::TagEntry>,
    deleted: bool,
) -> Result<bool, String> {
    if let Some(entry) = tags
        && read_stored_tags(tx, id)? != tags_storage_value(&entry.tags)?
    {
        return Ok(false);
    }
    let old = read_stored_mark(tx, id)?;
    let after_delete = if deleted { None } else { old.clone() };
    let new = match mark {
        Some(mark) => Some(merged_mark(after_delete.as_ref(), mark)),
        None => after_delete,
    };
    Ok(match (&old, &new) {
        (None, None) => true,
        // Statuses must match, so `marked_unix` is deliberately not compared.
        (Some(old), Some(new)) => {
            old.status == new.status
                && old.record_path == new.record_path
                && old.examiner == new.examiner
                && old.note == new.note
                && old.range_in == new.range_in
                && old.range_out == new.range_out
        }
        _ => false,
    })
}

/// Applies one patch in a single `BEGIN IMMEDIATE` transaction.
///
/// Per id (deletion, tags and mark together): with a v2 base the id is
/// skipped and reported as a conflict when `base != stored revision`, the
/// stored revision was written by another (or an unknown) `client_id`, and
/// the patch would change the stored value (an unchanged one is applied
/// at the current revision); otherwise deletion, tags and mark are
/// applied in that order and the
/// revision is bumped by one iff something effectively changed (so an
/// identical resend is idempotent and writes neither history nor a new
/// revision). A deleted mark keeps its revision row as a tombstone. Every
/// history row written by a v2 client carries `client:<client_id>` in
/// `detail`.
pub fn patch_review_annotations_v2(
    case_dir: &Path,
    patch: &AnnotationPatch<'_>,
    actor: Option<&str>,
    client_id: Option<&str>,
) -> Result<PatchOutcome, String> {
    let mut conn = open_case_db(case_dir)?;
    init_schema(&conn)?;
    let now = u64_to_i64(now_unix()?);
    let tx = begin_write(&mut conn)
        .map_err(|err| format!("failed to start SQLite marks transaction: {err}"))?;
    let detail = client_id
        .filter(|id| !id.trim().is_empty())
        .map(|id| format!("client:{id}"));
    let ctx = WriteContext {
        now,
        actor,
        detail: detail.as_deref(),
    };
    let marks_by_id: HashMap<&str, &ReviewMarkRow> = patch
        .marks
        .iter()
        .map(|mark| (mark.record_id.as_str(), mark))
        .collect();
    let tags_by_id: HashMap<&str, &crate::selection::TagEntry> = patch
        .tags
        .iter()
        .map(|entry| (entry.id.as_str(), entry))
        .collect();
    let deleted: HashSet<&str> = patch.deleted_ids.iter().map(String::as_str).collect();
    let ids: BTreeSet<&str> = marks_by_id
        .keys()
        .chain(tags_by_id.keys())
        .chain(deleted.iter())
        .copied()
        .collect();
    let mut outcome = PatchOutcome::default();
    let request_client = client_id.filter(|id| !id.trim().is_empty());
    for id in ids {
        let (current, last_writer) = read_revision_row(&tx, id)?;
        if let Some(base) = patch.base_revisions {
            let base_revision = base.get(id).copied().unwrap_or(0);
            // R5: an unsafe base (-1) never matches and is never exempt.
            let unsafe_base = base_revision == UNSAFE_BASE_REVISION;
            // §10.1.1: any mismatch (older OR newer base) is a conflict,
            // except §10.1.2 — this same window wrote the current revision
            // (its own consecutive edit, also across a deferred queue chain).
            let same_window = !unsafe_base
                && request_client.is_some()
                && last_writer.as_deref() == request_client;
            if unsafe_base || (base_revision != current && !same_window) {
                // §10.1.3: content identical to the stored value (a resend
                // whose response was lost, a re-imported queue body) is
                // applied at the current revision — no bump, no history.
                if patch_is_noop(
                    &tx,
                    id,
                    marks_by_id.get(id).copied(),
                    tags_by_id.get(id).copied(),
                    deleted.contains(id),
                )? {
                    outcome.applied.push((id.to_string(), current));
                    continue;
                }
                outcome.conflicts.push(AnnotationConflict {
                    id: id.to_string(),
                    server_revision: current,
                    server: read_server_annotation(&tx, id)?,
                });
                continue;
            }
        }
        let mut changed = false;
        if deleted.contains(id) {
            changed |= delete_mark(&tx, id, ctx)?;
        }
        if let Some(entry) = tags_by_id.get(id) {
            changed |= store_tags(&tx, entry, ctx)?;
        }
        if let Some(mark) = marks_by_id.get(id) {
            changed |= upsert_mark(&tx, mark, ctx)?;
            outcome.stored += 1;
        }
        let revision = if changed {
            let next = current + 1;
            tx.execute(
                r#"
                INSERT INTO review_annotation_rev (record_id, revision, updated_unix, actor, client_id)
                VALUES (?1, ?2, ?3, ?4, ?5)
                ON CONFLICT(record_id) DO UPDATE SET
                    revision = excluded.revision,
                    updated_unix = excluded.updated_unix,
                    actor = excluded.actor,
                    client_id = excluded.client_id
                "#,
                params![id, u64_to_i64(next), now, actor, request_client],
            )
            .map_err(|err| format!("failed to store annotation revision for {id}: {err}"))?;
            next
        } else {
            current
        };
        outcome.applied.push((id.to_string(), revision));
    }
    tx.commit()
        .map_err(|err| format!("failed to commit review marks: {err}"))?;
    Ok(outcome)
}

/// Current `{record_id: revision}` map (empty when the case has no db).
pub fn load_review_revisions(case_dir: &Path) -> Result<BTreeMap<String, u64>, String> {
    let path = case_db_path(case_dir);
    if !path.is_file() {
        return Ok(BTreeMap::new());
    }
    let conn = open_readonly_case_db(&path)?;
    load_review_revisions_from(&conn)
}

/// [`load_review_revisions`] over an already-open (possibly read-only,
/// pre-v7) connection: a db without the table yields an empty map.
pub(crate) fn load_review_revisions_from(
    conn: &rusqlite::Connection,
) -> Result<BTreeMap<String, u64>, String> {
    if !table_exists(conn, "review_annotation_rev")? {
        return Ok(BTreeMap::new());
    }
    let mut stmt = conn
        .prepare("SELECT record_id, revision FROM review_annotation_rev ORDER BY record_id")
        .map_err(|err| format!("failed to prepare annotation revision query: {err}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, i64_to_u64(row.get::<_, i64>(1)?)))
        })
        .map_err(|err| format!("failed to query annotation revisions: {err}"))?;
    rows.map(|row| row.map_err(|err| format!("failed to read annotation revision row: {err}")))
        .collect()
}

/// `{record_id: (revision, writer client_id)}` over an open connection
/// (empty for a pre-v7 db).
pub(crate) fn load_review_revision_rows_from(
    conn: &rusqlite::Connection,
) -> Result<BTreeMap<String, (u64, Option<String>)>, String> {
    if !table_exists(conn, "review_annotation_rev")? {
        return Ok(BTreeMap::new());
    }
    let mut stmt = conn
        .prepare(
            "SELECT record_id, revision, client_id FROM review_annotation_rev ORDER BY record_id",
        )
        .map_err(|err| format!("failed to prepare annotation revision query: {err}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (
                    i64_to_u64(row.get::<_, i64>(1)?),
                    row.get::<_, Option<String>>(2)?,
                ),
            ))
        })
        .map_err(|err| format!("failed to query annotation revisions: {err}"))?;
    rows.map(|row| row.map_err(|err| format!("failed to read annotation revision row: {err}")))
        .collect()
}

/// [`patch_review_annotations`] with the acting examiner recorded on every
/// history row. Deletions and tag edits carry no per-mark examiner, so
/// `actor` is the only "who" they get; mark upserts record the mark's own
/// examiner, falling back to `actor`.
///
/// History rows: `insert`, `update`, `delete` (a tombstone whose `old_*`
/// columns keep the last stored values), `tags`. A re-sent mark that
/// changes nothing writes no row.
///
/// Legacy entry point: no base revisions, so nothing conflicts (last
/// write wins) — see [`patch_review_annotations_v2`].
pub fn patch_review_annotations_by(
    case_dir: &Path,
    marks: &[ReviewMarkRow],
    tags: &[crate::selection::TagEntry],
    deleted_ids: &[String],
    actor: Option<&str>,
) -> Result<usize, String> {
    let patch = AnnotationPatch {
        marks,
        tags,
        deleted_ids,
        base_revisions: None,
    };
    patch_review_annotations_v2(case_dir, &patch, actor, None).map(|outcome| outcome.stored)
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
        assert_eq!(version, "7");
        assert!(table_has_column(&conn, "evidence_sources", "evidence_hash_origin").unwrap());
        let backup = fs::read_dir(dir.join("db"))
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("case.db.backup-v5-to-v7-")
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
    fn every_older_schema_reaches_v7() {
        for version in ["1", "2", "3", "4", "5", "6"] {
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
            assert_eq!(stored, "7", "v{version}");
            assert_eq!(
                load_review_mark_history(&dir, None).unwrap().len(),
                1,
                "v{version}"
            );
            assert_eq!(
                load_review_revisions(&dir).unwrap().get("vid_1"),
                Some(&1),
                "v{version}"
            );
            // Exactly one backup, taken before the chain and labelled with
            // the version that was on disk; it still reads as that version.
            let backups: Vec<std::path::PathBuf> = fs::read_dir(dir.join("db"))
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("case.db.backup-")
                })
                .map(|entry| entry.path())
                .collect();
            assert_eq!(backups.len(), 1, "v{version}: {backups:?}");
            assert!(
                backups[0]
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(&format!("case.db.backup-v{version}-to-v7-")),
                "v{version}: {backups:?}"
            );
            let backup = open_readonly_case_db(&backups[0]).unwrap();
            let backed_up: String = backup
                .query_row(
                    "SELECT value FROM schema_meta WHERE key = 'schema_version'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(backed_up, version, "backup holds the pre-migration db");
            assert!(!table_exists(&backup, "review_annotation_rev").unwrap());
            drop(backup);
            let _ = fs::remove_dir_all(dir);
        }
    }

    /// Exact v6 shape (marks with ranges, two tag lists — one on a record
    /// without a mark — and five history rows). The migration must leave
    /// every loaded value byte-identical, seed revision 1 for marks ∪ tag
    /// ids, and leave a backup.
    #[test]
    fn v6_case_migrates_to_v7_losslessly_with_backup() {
        let dir = case("v6-lossless");
        let seed = Connection::open(case_db_path(&dir)).unwrap();
        seed.execute_batch(
            r#"
            CREATE TABLE schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            INSERT INTO schema_meta (key, value) VALUES ('schema_version', '6');
            CREATE TABLE evidence_sources (
                source_id TEXT PRIMARY KEY, kind TEXT NOT NULL, path TEXT NOT NULL,
                registered_unix INTEGER NOT NULL, last_seen_unix INTEGER NOT NULL,
                write_protect TEXT, acquisition_tool TEXT, evidence_hash TEXT,
                notes TEXT, metadata_json TEXT NOT NULL,
                evidence_hash_algorithm TEXT, evidence_hash_origin TEXT
            );
            CREATE TABLE review_marks (
                record_id TEXT PRIMARY KEY, status TEXT NOT NULL,
                marked_unix INTEGER NOT NULL, record_path TEXT, examiner TEXT,
                note TEXT, range_in REAL, range_out REAL
            );
            INSERT INTO review_marks VALUES ('vid_1', 'important', 100, '/e/a.mp4', 'kim', '메모 "따옴표"', 1.5, 4.0);
            INSERT INTO review_marks VALUES ('vid_2', 'reviewed', 200, NULL, NULL, NULL, NULL, NULL);
            INSERT INTO review_marks VALUES ('vid_3', 'noted', 300, NULL, 'lee', 'n3', 2.0, NULL);
            INSERT INTO schema_meta (key, value) VALUES ('review_tags.v1:vid_1', '["사고","보류"]');
            INSERT INTO schema_meta (key, value) VALUES ('review_tags.v1:vid_9', '["tag-only"]');
            CREATE TABLE review_mark_history (
                history_pk INTEGER PRIMARY KEY AUTOINCREMENT,
                record_id TEXT NOT NULL, action TEXT NOT NULL, changed_unix INTEGER NOT NULL,
                actor TEXT, old_status TEXT, new_status TEXT, old_marked_unix INTEGER,
                new_marked_unix INTEGER, old_record_path TEXT, new_record_path TEXT,
                old_examiner TEXT, new_examiner TEXT, old_note TEXT, new_note TEXT,
                old_range_in REAL, new_range_in REAL, old_range_out REAL, new_range_out REAL,
                old_tags_json TEXT, new_tags_json TEXT, detail TEXT
            );
            CREATE TRIGGER review_mark_history_no_update BEFORE UPDATE ON review_mark_history
            BEGIN SELECT RAISE(ABORT, 'review_mark_history is append-only'); END;
            CREATE TRIGGER review_mark_history_no_delete BEFORE DELETE ON review_mark_history
            BEGIN SELECT RAISE(ABORT, 'review_mark_history is append-only'); END;
            INSERT INTO review_mark_history (record_id, action, changed_unix, new_status) VALUES ('vid_1', 'insert', 10, 'noted');
            INSERT INTO review_mark_history (record_id, action, changed_unix, old_status, new_status) VALUES ('vid_1', 'update', 11, 'noted', 'important');
            INSERT INTO review_mark_history (record_id, action, changed_unix, new_tags_json) VALUES ('vid_1', 'tags', 12, '["사고","보류"]');
            INSERT INTO review_mark_history (record_id, action, changed_unix, new_status) VALUES ('vid_2', 'insert', 13, 'reviewed');
            INSERT INTO review_mark_history (record_id, action, changed_unix, new_tags_json) VALUES ('vid_9', 'tags', 14, '["tag-only"]');
            "#,
        )
        .unwrap();
        drop(seed);

        let before_marks = load_review_marks(&dir).unwrap();
        let before_tags = load_review_tags(&dir).unwrap();
        let before_history = load_review_mark_history(&dir, None).unwrap();
        assert_eq!(before_marks.len(), 3);
        assert_eq!(before_tags.len(), 2);
        assert_eq!(before_history.len(), 5);
        assert!(load_review_revisions(&dir).unwrap().is_empty());

        let conn = open_case_db(&dir).unwrap();
        init_schema(&conn).unwrap();
        drop(conn);

        assert_eq!(load_review_marks(&dir).unwrap(), before_marks);
        assert_eq!(load_review_tags(&dir).unwrap(), before_tags);
        assert_eq!(
            load_review_mark_history(&dir, None).unwrap(),
            before_history
        );
        let revisions = load_review_revisions(&dir).unwrap();
        let expected: BTreeMap<String, u64> = ["vid_1", "vid_2", "vid_3", "vid_9"]
            .iter()
            .map(|id| (id.to_string(), 1))
            .collect();
        assert_eq!(revisions, expected);
        let conn = open_readonly_case_db(&case_db_path(&dir)).unwrap();
        let version: String = conn
            .query_row(
                "SELECT value FROM schema_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, "7");
        let backup = fs::read_dir(dir.join("db"))
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("case.db.backup-v6-to-v7-")
            });
        assert!(backup, "v6->v7 migration must leave a backup");
        // Re-opening at v7 is idempotent (no second seeding, no error).
        drop(conn);
        let conn = open_case_db(&dir).unwrap();
        init_schema(&conn).unwrap();
        assert_eq!(load_review_revisions(&dir).unwrap(), expected);
        let _ = fs::remove_dir_all(dir);
    }

    fn base(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
        pairs
            .iter()
            .map(|(id, rev)| (id.to_string(), *rev))
            .collect()
    }

    fn v2(
        dir: &Path,
        marks: &[ReviewMarkRow],
        tags: &[TagEntry],
        deleted: &[String],
        base_revisions: &BTreeMap<String, u64>,
        client: &str,
    ) -> PatchOutcome {
        patch_review_annotations_v2(
            dir,
            &AnnotationPatch {
                marks,
                tags,
                deleted_ids: deleted,
                base_revisions: Some(base_revisions),
            },
            Some("kim"),
            Some(client),
        )
        .unwrap()
    }

    #[test]
    fn patch_v2_applies_when_base_matches_and_bumps_revision() {
        let dir = case("v2-apply");
        let first = v2(
            &dir,
            &[mark("relevant", Some("a"), None)],
            &[],
            &[],
            &base(&[("vid_1", 0)]),
            "w-a",
        );
        assert_eq!(first.applied, vec![("vid_1".to_string(), 1)]);
        assert!(first.conflicts.is_empty());
        assert_eq!(first.stored, 1);
        assert_eq!(load_review_revisions(&dir).unwrap().get("vid_1"), Some(&1));
        // A base id absent from the map counts as 0; a newer base passes.
        let second = v2(
            &dir,
            &[mark("important", Some("b"), None)],
            &[TagEntry {
                id: "vid_1".into(),
                tags: vec!["사고".into()],
            }],
            &[],
            &base(&[("vid_1", 1)]),
            "w-b",
        );
        assert_eq!(second.applied, vec![("vid_1".to_string(), 2)]);
        let history = load_review_mark_history(&dir, Some("vid_1")).unwrap();
        let actions: Vec<&str> = history.iter().map(|row| row.action.as_str()).collect();
        assert_eq!(actions, ["insert", "tags", "update"]);
        let conn = open_readonly_case_db(&case_db_path(&dir)).unwrap();
        let details: Vec<Option<String>> = conn
            .prepare("SELECT detail FROM review_mark_history ORDER BY history_pk")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            details,
            vec![
                Some("client:w-a".to_string()),
                Some("client:w-b".to_string()),
                Some("client:w-b".to_string())
            ]
        );
        let (actor, client): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT actor, client_id FROM review_annotation_rev WHERE record_id = 'vid_1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(actor.as_deref(), Some("kim"));
        assert_eq!(client.as_deref(), Some("w-b"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn patch_v2_refuses_stale_base_without_touching_row_or_history() {
        let dir = case("v2-stale");
        v2(
            &dir,
            &[mark("relevant", Some("server"), None)],
            &[],
            &[],
            &base(&[]),
            "w-a",
        );
        let history_before = load_review_mark_history(&dir, None).unwrap().len();
        let other = ReviewMarkRow {
            record_id: "vid_2".into(),
            status: "noted".into(),
            marked_unix: 9,
            ..Default::default()
        };
        // Stale window: base 0 for vid_1 (server is at 1), fresh vid_2.
        let outcome = v2(
            &dir,
            &[mark("important", Some("mine"), None), other],
            &[],
            &[],
            &base(&[("vid_1", 0), ("vid_2", 0)]),
            "w-b",
        );
        assert_eq!(outcome.applied, vec![("vid_2".to_string(), 1)]);
        assert_eq!(outcome.stored, 1);
        assert_eq!(outcome.conflicts.len(), 1);
        let conflict = &outcome.conflicts[0];
        assert_eq!(conflict.id, "vid_1");
        assert_eq!(conflict.server_revision, 1);
        let server = conflict.server.as_ref().unwrap();
        assert_eq!(server.status.as_deref(), Some("relevant"));
        assert_eq!(server.note.as_deref(), Some("server"));
        assert_eq!(server.examiner.as_deref(), Some("kim"));
        assert!(server.tags.is_empty());
        let stored = load_review_marks(&dir).unwrap();
        let vid_1 = stored.iter().find(|m| m.record_id == "vid_1").unwrap();
        assert_eq!(vid_1.status, "relevant");
        assert_eq!(vid_1.note.as_deref(), Some("server"));
        assert_eq!(load_review_revisions(&dir).unwrap().get("vid_1"), Some(&1));
        // Exactly one history row was added — the vid_2 insert.
        assert_eq!(
            load_review_mark_history(&dir, None).unwrap().len(),
            history_before + 1
        );
        // A v2 body with no base map at all conflicts on every revised id
        // instead of overwriting (review point 7.1).
        let no_base = v2(
            &dir,
            &[mark("important", Some("mine"), None)],
            &[],
            &[],
            &base(&[]),
            "w-c",
        );
        assert_eq!(no_base.conflicts.len(), 1);
        assert!(no_base.applied.is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn patch_v2_resend_identical_is_idempotent_and_keeps_revision() {
        let dir = case("v2-idempotent");
        let span = RangeSpan {
            start: Some(1.0),
            end: Some(2.5),
        };
        let marks = [mark("important", Some("same"), Some(span))];
        let tags = [TagEntry {
            id: "vid_1".into(),
            tags: vec!["a".into(), "b".into()],
        }];
        let first = v2(&dir, &marks, &tags, &[], &base(&[("vid_1", 0)]), "w-a");
        assert_eq!(first.applied, vec![("vid_1".to_string(), 1)]);
        // Resent with the base it was told (e.g. after a 413 retry).
        let again = v2(&dir, &marks, &tags, &[], &base(&[("vid_1", 1)]), "w-a");
        assert_eq!(again.applied, vec![("vid_1".to_string(), 1)]);
        assert!(again.conflicts.is_empty());
        assert_eq!(load_review_revisions(&dir).unwrap().get("vid_1"), Some(&1));
        let history = load_review_mark_history(&dir, None).unwrap();
        assert_eq!(history.len(), 2, "{history:?}"); // insert + tags, no duplicates
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn patch_v2_delete_keeps_rev_row_so_stale_recreate_conflicts() {
        let dir = case("v2-tombstone");
        v2(
            &dir,
            &[mark("important", None, None)],
            &[],
            &[],
            &base(&[]),
            "w-a",
        );
        let deleted = v2(
            &dir,
            &[],
            &[TagEntry {
                id: "vid_1".into(),
                tags: vec![],
            }],
            &["vid_1".to_string()],
            &base(&[("vid_1", 1)]),
            "w-a",
        );
        assert_eq!(deleted.applied, vec![("vid_1".to_string(), 2)]);
        assert!(load_review_marks(&dir).unwrap().is_empty());
        assert_eq!(load_review_revisions(&dir).unwrap().get("vid_1"), Some(&2));
        // Another window that still believes revision 1 cannot resurrect it.
        let stale = v2(
            &dir,
            &[mark("important", None, None)],
            &[],
            &[],
            &base(&[("vid_1", 1)]),
            "w-b",
        );
        assert!(stale.applied.is_empty());
        assert_eq!(stale.conflicts[0].server_revision, 2);
        assert_eq!(
            stale.conflicts[0].server, None,
            "tombstone: no server value"
        );
        assert!(load_review_marks(&dir).unwrap().is_empty());
        // The deleting window itself may recreate with its own revision.
        let same = v2(
            &dir,
            &[mark("important", None, None)],
            &[],
            &[],
            &base(&[("vid_1", 2)]),
            "w-a",
        );
        assert_eq!(same.applied, vec![("vid_1".to_string(), 3)]);
        assert_eq!(load_review_marks(&dir).unwrap().len(), 1);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn patch_v2_explicit_empty_tags_clears_server_tags_without_mark() {
        let dir = case("v2-tags-only");
        let tagged = v2(
            &dir,
            &[],
            &[TagEntry {
                id: "vid_9".into(),
                tags: vec!["보류".into()],
            }],
            &[],
            &base(&[]),
            "w-a",
        );
        assert_eq!(tagged.applied, vec![("vid_9".to_string(), 1)]);
        assert_eq!(tagged.stored, 0);
        assert_eq!(load_review_tags(&dir).unwrap().len(), 1);
        let cleared = v2(
            &dir,
            &[],
            &[TagEntry {
                id: "vid_9".into(),
                tags: vec![],
            }],
            &[],
            &base(&[("vid_9", 1)]),
            "w-a",
        );
        assert_eq!(cleared.applied, vec![("vid_9".to_string(), 2)]);
        assert!(load_review_tags(&dir).unwrap().is_empty());
        assert!(load_review_marks(&dir).unwrap().is_empty());
        let actions: Vec<String> = load_review_mark_history(&dir, Some("vid_9"))
            .unwrap()
            .into_iter()
            .map(|row| row.action)
            .collect();
        assert_eq!(actions, ["tags", "tags"]);
        // A stale window conflicts and sees "tags but no mark" as the server value.
        v2(
            &dir,
            &[],
            &[TagEntry {
                id: "vid_9".into(),
                tags: vec!["x".into()],
            }],
            &[],
            &base(&[("vid_9", 2)]),
            "w-a",
        );
        let stale = v2(
            &dir,
            &[],
            &[TagEntry {
                id: "vid_9".into(),
                tags: vec!["y".into()],
            }],
            &[],
            &base(&[("vid_9", 2)]),
            "w-b",
        );
        let server = stale.conflicts[0].server.as_ref().unwrap();
        assert_eq!(server.status, None);
        assert_eq!(server.tags, vec!["x".to_string()]);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn patch_v1_legacy_body_still_applies_and_bumps_revision() {
        let dir = case("v1-legacy");
        v2(
            &dir,
            &[mark("relevant", Some("v2"), None)],
            &[],
            &[],
            &base(&[]),
            "w-a",
        );
        // Legacy client: no base, last write wins, revision still moves.
        let stored = patch_review_annotations_by(
            &dir,
            &[mark("important", Some("legacy"), None)],
            &[],
            &[],
            Some("lee"),
        )
        .unwrap();
        assert_eq!(stored, 1);
        let marks = load_review_marks(&dir).unwrap();
        assert_eq!(marks[0].status, "important");
        assert_eq!(load_review_revisions(&dir).unwrap().get("vid_1"), Some(&2));
        let conn = open_readonly_case_db(&case_db_path(&dir)).unwrap();
        let client: Option<String> = conn
            .query_row(
                "SELECT client_id FROM review_annotation_rev WHERE record_id = 'vid_1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(client, None, "legacy writers carry no client id");
        let detail: Option<String> = conn
            .query_row(
                "SELECT detail FROM review_mark_history ORDER BY history_pk DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(detail, None);
        // Now the v2 window that still holds revision 1 is told about it.
        let stale = v2(
            &dir,
            &[mark("noted", None, None)],
            &[],
            &[],
            &base(&[("vid_1", 1)]),
            "w-a",
        );
        assert_eq!(stale.conflicts[0].server_revision, 2);
        assert_eq!(
            stale.conflicts[0].server.as_ref().unwrap().note.as_deref(),
            Some("legacy")
        );
        let _ = fs::remove_dir_all(dir);
    }

    fn rev_writer(dir: &Path, id: &str) -> Option<String> {
        open_readonly_case_db(&case_db_path(dir))
            .unwrap()
            .query_row(
                "SELECT client_id FROM review_annotation_rev WHERE record_id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap()
    }

    /// §10.1.2/§10.1.3: a save whose response was lost is resent with the
    /// same (now old) base. From the same window it applies (nothing
    /// changes); from a reloaded window with a new client id the identical
    /// content is a no-op — applied at the current revision either way, no
    /// history row, no revision bump.
    #[test]
    fn patch_v2_lost_response_resend_is_applied_not_conflicted() {
        let dir = case("v2-lost-response");
        let marks = [mark("important", Some("n"), None)];
        let tags = [TagEntry {
            id: "vid_1".into(),
            tags: vec!["사고".into()],
        }];
        let first = v2(&dir, &marks, &tags, &[], &base(&[("vid_1", 0)]), "w-a");
        assert_eq!(first.applied, vec![("vid_1".to_string(), 1)]);
        let history = load_review_mark_history(&dir, None).unwrap().len();
        for client in ["w-a", "w-reloaded"] {
            let again = v2(&dir, &marks, &tags, &[], &base(&[("vid_1", 0)]), client);
            assert_eq!(again.applied, vec![("vid_1".to_string(), 1)], "{client}");
            assert!(again.conflicts.is_empty(), "{client}");
            assert_eq!(load_review_mark_history(&dir, None).unwrap().len(), history);
            assert_eq!(load_review_revisions(&dir).unwrap().get("vid_1"), Some(&1));
        }
        // The no-op path does not take over authorship of the revision.
        assert_eq!(rev_writer(&dir, "vid_1").as_deref(), Some("w-a"));
        // A delete resent after it landed is a no-op too.
        let delete = [String::from("vid_1")];
        let empty = [TagEntry {
            id: "vid_1".into(),
            tags: vec![],
        }];
        let gone = v2(&dir, &[], &empty, &delete, &base(&[("vid_1", 1)]), "w-a");
        assert_eq!(gone.applied, vec![("vid_1".to_string(), 2)]);
        let resent = v2(&dir, &[], &empty, &delete, &base(&[("vid_1", 1)]), "w-b");
        assert_eq!(resent.applied, vec![("vid_1".to_string(), 2)]);
        assert!(resent.conflicts.is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    /// §10.1.2: two bodies queued by one window, both carrying the base it
    /// knew when queuing, are applied in order at the drain — the second no
    /// longer conflicts with the first — while a different window with the
    /// same stale base still conflicts.
    #[test]
    fn patch_v2_deferred_chain_from_same_window_applies_in_order() {
        let dir = case("v2-chain");
        // Another window created revision 1.
        v2(
            &dir,
            &[mark("reviewed", Some("start"), None)],
            &[],
            &[],
            &base(&[]),
            "w-x",
        );
        let one = v2(
            &dir,
            &[mark("important", Some("first edit"), None)],
            &[],
            &[],
            &base(&[("vid_1", 1)]),
            "w-a",
        );
        assert_eq!(one.applied, vec![("vid_1".to_string(), 2)]);
        let two = v2(
            &dir,
            &[mark("important", Some("second edit"), None)],
            &[],
            &[],
            &base(&[("vid_1", 1)]),
            "w-a",
        );
        assert_eq!(two.applied, vec![("vid_1".to_string(), 3)]);
        assert!(two.conflicts.is_empty());
        assert_eq!(
            load_review_marks(&dir).unwrap()[0].note.as_deref(),
            Some("second edit")
        );
        // A different window holding revision 1 is told about it.
        let other = v2(
            &dir,
            &[mark("important", Some("other window"), None)],
            &[],
            &[],
            &base(&[("vid_1", 1)]),
            "w-b",
        );
        assert!(other.applied.is_empty());
        assert_eq!(other.conflicts[0].server_revision, 3);
        assert_eq!(
            other.conflicts[0].server.as_ref().unwrap().note.as_deref(),
            Some("second edit")
        );
        // Without a client id there is no "same window": a wrong base
        // conflicts even when the last writer also had none.
        crate::case_db::patch_review_annotations_by(
            &dir,
            &[mark("important", Some("legacy"), None)],
            &[],
            &[],
            None,
        )
        .unwrap();
        let anonymous = patch_review_annotations_v2(
            &dir,
            &AnnotationPatch {
                marks: &[mark("important", Some("anon"), None)],
                tags: &[],
                deleted_ids: &[],
                base_revisions: Some(&base(&[("vid_1", 1)])),
            },
            None,
            None,
        )
        .unwrap();
        assert_eq!(anonymous.conflicts.len(), 1);
        let _ = fs::remove_dir_all(dir);
    }

    /// R5 (U1 ↔ P contract): a draft sent with base -1 (made while the
    /// viewer was read-only) always conflicts — even from the window that
    /// wrote the stored revision, and even for an id the server never
    /// revised — unless its content equals the stored value (no-op).
    #[test]
    fn patch_v2_unsafe_base_always_conflicts_unless_identical() {
        let dir = case("v2-unsafe-base");
        let unsafe_base = |id: &str| base(&[(id, UNSAFE_BASE_REVISION)]);
        let first = v2(
            &dir,
            &[mark("reviewed", Some("server"), None)],
            &[],
            &[],
            &base(&[]),
            "w-a",
        );
        assert_eq!(first.applied, vec![("vid_1".to_string(), 1)]);
        // Same window, different content: still a conflict (no §10.1.2).
        let same_window = v2(
            &dir,
            &[mark("important", Some("read-only draft"), None)],
            &[],
            &[],
            &unsafe_base("vid_1"),
            "w-a",
        );
        assert!(same_window.applied.is_empty(), "{same_window:?}");
        assert_eq!(same_window.conflicts.len(), 1);
        assert_eq!(same_window.conflicts[0].server_revision, 1);
        assert_eq!(
            load_review_marks(&dir).unwrap()[0].note.as_deref(),
            Some("server"),
            "the stored mark is untouched"
        );
        // A deletion with an unsafe base never deletes.
        let delete = [String::from("vid_1")];
        let gone = v2(&dir, &[], &[], &delete, &unsafe_base("vid_1"), "w-a");
        assert_eq!(gone.conflicts.len(), 1);
        assert_eq!(load_review_marks(&dir).unwrap().len(), 1);
        // An id without any revision row conflicts too (server: none).
        let mut fresh = mark("reviewed", None, None);
        fresh.record_id = "vid_7".into();
        let new_id = v2(&dir, &[fresh], &[], &[], &unsafe_base("vid_7"), "w-a");
        assert!(new_id.applied.is_empty());
        assert_eq!(new_id.conflicts[0].server, None);
        // Identical content is a no-op: applied at the current revision.
        let identical = v2(
            &dir,
            &[mark("reviewed", Some("server"), None)],
            &[],
            &[],
            &unsafe_base("vid_1"),
            "w-b",
        );
        assert_eq!(identical.applied, vec![("vid_1".to_string(), 1)]);
        assert!(identical.conflicts.is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    /// §10.1.1: a base NEWER than the stored revision (the db was restored
    /// from a backup) is a conflict too, also for an id without a row.
    #[test]
    fn patch_v2_base_newer_than_server_conflicts() {
        let dir = case("v2-newer-base");
        v2(
            &dir,
            &[mark("reviewed", Some("restored"), None)],
            &[],
            &[],
            &base(&[]),
            "w-x",
        );
        let newer = v2(
            &dir,
            &[mark("important", Some("mine"), None)],
            &[],
            &[],
            &base(&[("vid_1", 5)]),
            "w-a",
        );
        assert!(newer.applied.is_empty());
        assert_eq!(newer.conflicts[0].server_revision, 1);
        let unknown = ReviewMarkRow {
            record_id: "vid_2".into(),
            status: "important".into(),
            marked_unix: 3,
            ..Default::default()
        };
        let missing_row = v2(&dir, &[unknown], &[], &[], &base(&[("vid_2", 3)]), "w-a");
        assert!(missing_row.applied.is_empty());
        assert_eq!(missing_row.conflicts[0].server_revision, 0);
        assert_eq!(missing_row.conflicts[0].server, None);
        assert_eq!(load_review_marks(&dir).unwrap().len(), 1);
        let _ = fs::remove_dir_all(dir);
    }

    /// §10.1.3/§10.1.4: with an unchanged status the time stamp alone is not
    /// a change (no-op, not a conflict), and a `noted` row keeps its stored
    /// `marked_unix` when it is rewritten as `noted`. A status change is
    /// always a change.
    #[test]
    fn patch_v2_noop_ignores_time_for_same_status_and_noted_keeps_time() {
        let dir = case("v2-noted-time");
        let mut noted = mark("noted", Some("memo"), None);
        noted.marked_unix = 100;
        v2(&dir, &[noted.clone()], &[], &[], &base(&[]), "w-a");
        // The viewer re-stamps a note-only row with "now" on every export.
        noted.marked_unix = 200;
        let restamped = v2(&dir, &[noted.clone()], &[], &[], &base(&[]), "w-b");
        assert_eq!(restamped.applied, vec![("vid_1".to_string(), 1)]);
        assert!(restamped.conflicts.is_empty());
        assert_eq!(load_review_marks(&dir).unwrap()[0].marked_unix, 100);
        // Same window edits the note: applied, the noted time is kept.
        noted.note = Some("memo 2".into());
        noted.marked_unix = 300;
        let edited = v2(
            &dir,
            &[noted.clone()],
            &[],
            &[],
            &base(&[("vid_1", 1)]),
            "w-a",
        );
        assert_eq!(edited.applied, vec![("vid_1".to_string(), 2)]);
        let stored = &load_review_marks(&dir).unwrap()[0];
        assert_eq!(stored.marked_unix, 100);
        assert_eq!(stored.note.as_deref(), Some("memo 2"));
        let history = load_review_mark_history(&dir, None).unwrap();
        assert_eq!(history.len(), 2, "{history:?}");
        // A re-sent legacy body that only re-stamps a noted row is no change.
        noted.marked_unix = 400;
        patch_review_annotations(&dir, &[noted.clone()], &[], &[]).unwrap();
        assert_eq!(load_review_mark_history(&dir, None).unwrap().len(), 2);
        assert_eq!(load_review_revisions(&dir).unwrap().get("vid_1"), Some(&2));
        // Same status, different time, other window: no-op (time ignored).
        let mut important = mark("important", Some("x"), None);
        important.record_id = "vid_2".into();
        important.marked_unix = 10;
        v2(&dir, &[important.clone()], &[], &[], &base(&[]), "w-a");
        important.marked_unix = 99;
        let same_status = v2(&dir, &[important.clone()], &[], &[], &base(&[]), "w-b");
        assert_eq!(same_status.applied, vec![("vid_2".to_string(), 1)]);
        let vid_2 = load_review_marks(&dir)
            .unwrap()
            .into_iter()
            .find(|m| m.record_id == "vid_2")
            .unwrap();
        assert_eq!(vid_2.marked_unix, 10, "no-op writes nothing");
        // A status change from another window with a stale base conflicts.
        let changed = v2(
            &dir,
            &[ReviewMarkRow {
                status: "reviewed".into(),
                ..important
            }],
            &[],
            &[],
            &base(&[]),
            "w-b",
        );
        assert_eq!(changed.conflicts.len(), 1);
        let _ = fs::remove_dir_all(dir);
    }
}
