//! Evidence source registration with validated, provenance-tagged hashes
//! (FOR-04).
//!
//! * `evidence_hash` must be hex of the length its algorithm defines
//!   (md5/sha1/sha256/sha512, optionally written `algo:hex`); it is stored
//!   lowercase with `evidence_hash_algorithm` and `evidence_hash_origin`
//!   (`computed` = digested by FrameTrace, `operator-supplied` = typed in
//!   and NOT verified by FrameTrace).
//! * Re-registering an existing source never re-points it: a source id
//!   bound to one kind/path refuses a different kind/path, and a kind/path
//!   bound to one id refuses a different explicit id.
//! * Custody-field changes on an existing source (hash, write protection,
//!   acquisition tool, notes) are applied only together with a chained
//!   audit entry in `evidence/logs/evidence-source-log.jsonl` naming the
//!   old and new values, and are returned to the caller to print.
//! * A computed digest is never replaced by a different operator-supplied
//!   value.
//! * Tool-generated default notes ("Auto-registered by …", written on
//!   every scan-folder / inspect-e01 / … run) only fill an empty notes
//!   field; they never overwrite what the operator wrote.

use super::*;
use crate::util::now_unix;
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use std::path::Path;

/// R5-P6: every registered evidence source path (`evidence_sources.path`),
/// for path redaction's list of known roots. `Ok(empty)` without a db or
/// table; an unreadable db is an error.
pub fn evidence_source_paths(case_dir: &Path) -> Result<Vec<String>, String> {
    let path = case_db_path(case_dir);
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let conn = open_readonly_case_db(&path)?;
    if !table_exists(&conn, "evidence_sources")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare("SELECT path FROM evidence_sources ORDER BY path")
        .map_err(|err| format!("failed to prepare evidence source query: {err}"))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|err| format!("failed to query evidence sources: {err}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|err| format!("failed to read evidence sources: {err}"))
}

/// Where a recorded evidence hash came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashOrigin {
    /// FrameTrace digested the bytes itself.
    Computed,
    /// Typed in by an operator; FrameTrace has not verified it.
    OperatorSupplied,
}

impl HashOrigin {
    pub fn label(self) -> &'static str {
        match self {
            Self::Computed => "computed",
            Self::OperatorSupplied => "operator-supplied",
        }
    }
}

/// Human-readable description of a stored origin label, for output.
pub fn describe_hash_origin(origin: Option<&str>) -> &'static str {
    match origin {
        Some("computed") => "computed by FrameTrace",
        Some("operator-supplied") => "operator-supplied, NOT verified by FrameTrace",
        _ => "origin not recorded (registered before schema v6), NOT verified by FrameTrace",
    }
}

/// A validated evidence hash: lowercase hex plus its algorithm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceHash {
    pub algorithm: &'static str,
    pub hex: String,
}

/// Validates an evidence hash. Accepts bare hex (algorithm inferred from
/// the length: 32 md5, 40 sha1, 64 sha256, 128 sha512) or `algo:hex`,
/// where the hex length must match the named algorithm.
pub fn parse_evidence_hash(text: &str) -> Result<EvidenceHash, String> {
    let trimmed = text.trim();
    let (declared, hex) = match trimmed.split_once(':') {
        Some((algo, hex)) => (Some(algo.trim().to_ascii_lowercase()), hex.trim()),
        None => (None, trimmed),
    };
    if hex.is_empty() || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!(
            "invalid evidence hash {trimmed:?}: expected hexadecimal digits (optionally prefixed md5:/sha1:/sha256:/sha512:)"
        ));
    }
    let by_length = match hex.len() {
        32 => Some("md5"),
        40 => Some("sha1"),
        64 => Some("sha256"),
        128 => Some("sha512"),
        _ => None,
    };
    let algorithm = match declared.as_deref() {
        None => by_length.ok_or_else(|| {
            format!(
                "invalid evidence hash {trimmed:?}: {} hex digits matches no supported algorithm (md5 32, sha1 40, sha256 64, sha512 128)",
                hex.len()
            )
        })?,
        Some(name) => {
            let canonical = match name.replace('-', "").as_str() {
                "md5" => "md5",
                "sha1" => "sha1",
                "sha256" => "sha256",
                "sha512" => "sha512",
                _ => {
                    return Err(format!(
                        "invalid evidence hash {trimmed:?}: unsupported algorithm {name:?} (md5, sha1, sha256, sha512)"
                    ));
                }
            };
            if by_length != Some(canonical) {
                return Err(format!(
                    "invalid evidence hash {trimmed:?}: {canonical} needs {} hex digits, found {}",
                    match canonical {
                        "md5" => 32,
                        "sha1" => 40,
                        "sha256" => 64,
                        _ => 128,
                    },
                    hex.len()
                ));
            }
            canonical
        }
    };
    Ok(EvidenceHash {
        algorithm,
        hex: hex.to_ascii_lowercase(),
    })
}

/// One custody field that changed on re-registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceSourceChange {
    pub field: &'static str,
    pub old: Option<String>,
    pub new: Option<String>,
}

/// Relative path of the chained log recording evidence source changes.
/// Prefix of the default notes FrameTrace's own commands attach when they
/// register a source implicitly.
pub const AUTO_NOTES_PREFIX: &str = "Auto-registered by ";

/// True for a tool-generated default note (see [`AUTO_NOTES_PREFIX`]).
pub fn is_auto_note(notes: &str) -> bool {
    notes.starts_with(AUTO_NOTES_PREFIX)
}

pub const EVIDENCE_SOURCE_LOG_REL_PATH: &str = "evidence/logs/evidence-source-log.jsonl";

#[derive(Debug, Clone)]
struct StoredSource {
    source_id: String,
    kind: String,
    path: String,
    write_protect: Option<String>,
    acquisition_tool: Option<String>,
    evidence_hash: Option<String>,
    hash_algorithm: Option<String>,
    hash_origin: Option<String>,
    notes: Option<String>,
    metadata_json: Option<String>,
}

fn read_source(
    tx: &Transaction<'_>,
    clause: &str,
    args: &[&dyn rusqlite::ToSql],
) -> Result<Option<StoredSource>, String> {
    tx.query_row(
        &format!(
            "SELECT source_id, kind, path, write_protect, acquisition_tool, evidence_hash, evidence_hash_algorithm, evidence_hash_origin, notes, metadata_json FROM evidence_sources WHERE {clause}"
        ),
        args,
        |row| {
            Ok(StoredSource {
                source_id: row.get(0)?,
                kind: row.get(1)?,
                path: row.get(2)?,
                write_protect: row.get(3)?,
                acquisition_tool: row.get(4)?,
                evidence_hash: row.get(5)?,
                hash_algorithm: row.get(6)?,
                hash_origin: row.get(7)?,
                notes: row.get(8)?,
                metadata_json: row.get(9)?,
            })
        },
    )
    .optional()
    .map_err(|err| format!("failed to read evidence source: {err}"))
}

/// Registers an evidence source. A supplied `evidence_hash` is recorded
/// as operator-supplied (unverified); callers that digested the bytes
/// themselves use [`register_evidence_source_with_origin`].
pub fn register_evidence_source(
    case_dir: &Path,
    input: &EvidenceSourceInput,
) -> Result<EvidenceSourceRow, String> {
    register_evidence_source_with_origin(case_dir, input, HashOrigin::OperatorSupplied)
}

pub fn register_evidence_source_with_origin(
    case_dir: &Path,
    input: &EvidenceSourceInput,
    origin: HashOrigin,
) -> Result<EvidenceSourceRow, String> {
    let hash = input
        .evidence_hash
        .as_deref()
        .map(parse_evidence_hash)
        .transpose()?;
    let mut conn = open_case_db(case_dir)?;
    init_schema(&conn)?;
    let now = u64_to_i64(now_unix()?);
    let path = input.path.to_string_lossy().to_string();
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|err| format!("failed to start evidence source transaction: {err}"))?;

    let by_id = match input.source_id.as_deref() {
        Some(id) => read_source(&tx, "source_id = ?1", &[&id])?,
        None => None,
    };
    let by_kind_path = read_source(
        &tx,
        "kind = ?1 AND path = ?2",
        &[&input.kind.as_str(), &path.as_str()],
    )?;
    if let Some(existing) = &by_id
        && (existing.kind != input.kind || existing.path != path)
    {
        return Err(format!(
            "refusing to re-point evidence source {}: it is registered as {} {} and the request names {} {}; nothing was changed (register the other source under a new source id)",
            existing.source_id, existing.kind, existing.path, input.kind, path
        ));
    }
    if let (Some(id), Some(existing)) = (input.source_id.as_deref(), &by_kind_path)
        && existing.source_id != id
    {
        return Err(format!(
            "refusing to register {} {} as {id}: it is already registered as evidence source {}; nothing was changed",
            input.kind, path, existing.source_id
        ));
    }

    let existing = by_id.or(by_kind_path);
    let metadata_json = input.metadata_json.as_deref().unwrap_or("{}");
    let Some(existing) = existing else {
        let source_id = input
            .source_id
            .clone()
            .unwrap_or_else(|| stable_source_id(&input.kind, &path));
        tx.execute(
            r#"
            INSERT INTO evidence_sources (
                source_id, kind, path, registered_unix, last_seen_unix,
                write_protect, acquisition_tool, evidence_hash,
                evidence_hash_algorithm, evidence_hash_origin, notes, metadata_json
            )
            VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            "#,
            params![
                source_id,
                input.kind,
                path,
                now,
                input.write_protect,
                input.acquisition_tool,
                hash.as_ref().map(|h| h.hex.as_str()),
                hash.as_ref().map(|h| h.algorithm),
                hash.as_ref().map(|_| origin.label()),
                input.notes,
                metadata_json,
            ],
        )
        .map_err(|err| format!("failed to register evidence source: {err}"))?;
        tx.commit()
            .map_err(|err| format!("failed to commit evidence source: {err}"))?;
        return Ok(EvidenceSourceRow {
            source_id,
            kind: input.kind.clone(),
            path,
            evidence_hash: hash.as_ref().map(|h| h.hex.clone()),
            hash_algorithm: hash.as_ref().map(|h| h.algorithm.to_string()),
            hash_origin: hash.as_ref().map(|_| origin.label().to_string()),
            newly_registered: true,
            changes: Vec::new(),
        });
    };

    // Existing source: work out which custody fields actually change.
    // An implicit re-registration's default note never replaces notes
    // already on the source (the operator's, or an earlier tool's).
    let notes = input.notes.clone().filter(|new| {
        !(is_auto_note(new)
            && existing
                .notes
                .as_deref()
                .is_some_and(|old| !old.trim().is_empty()))
    });
    let mut changes = Vec::new();
    let mut note_change = |field: &'static str, old: &Option<String>, new: &Option<String>| {
        if new.is_some() && new != old {
            changes.push(EvidenceSourceChange {
                field,
                old: old.clone(),
                new: new.clone(),
            });
        }
    };
    note_change(
        "write_protect",
        &existing.write_protect,
        &input.write_protect,
    );
    note_change(
        "acquisition_tool",
        &existing.acquisition_tool,
        &input.acquisition_tool,
    );
    note_change("notes", &existing.notes, &notes);
    // Absent metadata keeps what is stored; supplied metadata replaces it
    // and is logged like any other custody-field change.
    note_change(
        "metadata_json",
        &existing.metadata_json,
        &input.metadata_json,
    );

    let mut stored_hash = (
        existing.evidence_hash.clone(),
        existing.hash_algorithm.clone(),
        existing.hash_origin.clone(),
    );
    if let Some(hash) = &hash {
        let same_value = existing
            .evidence_hash
            .as_deref()
            .map(str::to_ascii_lowercase)
            == Some(hash.hex.clone());
        let old_computed = existing.hash_origin.as_deref() == Some("computed");
        if !same_value && old_computed && origin == HashOrigin::OperatorSupplied {
            return Err(format!(
                "refusing to replace the FrameTrace-computed {} hash of evidence source {} ({}) with a different operator-supplied value ({}); nothing was changed",
                existing.hash_algorithm.as_deref().unwrap_or("recorded"),
                existing.source_id,
                existing.evidence_hash.as_deref().unwrap_or("-"),
                hash.hex
            ));
        }
        if !same_value {
            changes.push(EvidenceSourceChange {
                field: "evidence_hash",
                old: existing.evidence_hash.clone(),
                new: Some(format!("{}:{}", hash.algorithm, hash.hex)),
            });
            stored_hash = (
                Some(hash.hex.clone()),
                Some(hash.algorithm.to_string()),
                Some(origin.label().to_string()),
            );
        } else if origin == HashOrigin::Computed && !old_computed {
            // The same value, now confirmed by FrameTrace's own digest.
            changes.push(EvidenceSourceChange {
                field: "evidence_hash_origin",
                old: existing.hash_origin.clone(),
                new: Some(origin.label().to_string()),
            });
            stored_hash = (
                Some(hash.hex.clone()),
                Some(hash.algorithm.to_string()),
                Some(origin.label().to_string()),
            );
        }
    }

    if !changes.is_empty() {
        // The chained entry is appended before the commit: if it cannot be
        // written the change is rolled back, so no custody field changes
        // without its audit record.
        let body = serde_json::json!({
            "event": "evidence-source-update",
            "changed_unix": now,
            "source_id": existing.source_id,
            "kind": existing.kind,
            "path": existing.path,
            "hash_origin": stored_hash.2,
            "changes": changes.iter().map(|change| serde_json::json!({
                "field": change.field,
                "old": change.old,
                "new": change.new,
            })).collect::<Vec<_>>(),
        })
        .to_string();
        crate::audit::append_chained_jsonl(&case_dir.join(EVIDENCE_SOURCE_LOG_REL_PATH), &body)
            .map_err(|err| {
                format!("evidence source change not applied: audit entry failed: {err}")
            })?;
    }
    tx.execute(
        r#"
        UPDATE evidence_sources SET
            last_seen_unix = ?2,
            write_protect = COALESCE(?3, write_protect),
            acquisition_tool = COALESCE(?4, acquisition_tool),
            evidence_hash = ?5,
            evidence_hash_algorithm = ?6,
            evidence_hash_origin = ?7,
            notes = COALESCE(?8, notes),
            metadata_json = COALESCE(?9, metadata_json)
        WHERE source_id = ?1
        "#,
        params![
            existing.source_id,
            now,
            input.write_protect,
            input.acquisition_tool,
            stored_hash.0,
            stored_hash.1,
            stored_hash.2,
            notes,
            input.metadata_json.as_deref(),
        ],
    )
    .map_err(|err| format!("failed to update evidence source: {err}"))?;
    tx.commit().map_err(|err| {
        format!(
            "failed to commit evidence source update (an audit entry describing it was already written to {EVIDENCE_SOURCE_LOG_REL_PATH}): {err}"
        )
    })?;
    Ok(EvidenceSourceRow {
        source_id: existing.source_id,
        kind: existing.kind,
        path: existing.path,
        evidence_hash: stored_hash.0,
        hash_algorithm: stored_hash.1,
        hash_origin: stored_hash.2,
        newly_registered: false,
        changes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn case(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("frametrace-sources-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("db")).unwrap();
        dir
    }

    fn input(id: Option<&str>, kind: &str, path: &str, hash: Option<&str>) -> EvidenceSourceInput {
        EvidenceSourceInput {
            kind: kind.into(),
            path: PathBuf::from(path),
            source_id: id.map(str::to_string),
            write_protect: None,
            acquisition_tool: None,
            evidence_hash: hash.map(str::to_string),
            notes: None,
            metadata_json: None,
        }
    }

    #[test]
    fn auto_registration_note_never_overwrites_operator_notes() {
        let dir = case("autonotes");
        let auto = |path: &str| EvidenceSourceInput {
            notes: Some("Auto-registered by scan-folder".into()),
            ..input(None, "folder", path, None)
        };
        // First implicit registration fills the empty field.
        register_evidence_source(&dir, &auto("/e/sd")).unwrap();
        let notes = |dir: &Path| -> Option<String> {
            let conn = open_case_db(dir).unwrap();
            conn.query_row(
                "SELECT notes FROM evidence_sources WHERE path = '/e/sd'",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            notes(&dir).as_deref(),
            Some("Auto-registered by scan-folder")
        );
        // The operator writes a real note (audited change).
        let operator = EvidenceSourceInput {
            notes: Some("SD card from vehicle 12가3456, sealed bag #7".into()),
            ..input(None, "folder", "/e/sd", None)
        };
        let row = register_evidence_source(&dir, &operator).unwrap();
        assert!(row.changes.iter().any(|c| c.field == "notes"));
        // Re-scanning must keep it, and logs no notes change.
        let row = register_evidence_source(&dir, &auto("/e/sd")).unwrap();
        assert!(
            row.changes.iter().all(|c| c.field != "notes"),
            "{:?}",
            row.changes.iter().map(|c| c.field).collect::<Vec<_>>()
        );
        assert_eq!(
            notes(&dir).as_deref(),
            Some("SD card from vehicle 12가3456, sealed bag #7")
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn re_registration_without_metadata_keeps_it_and_changes_are_logged() {
        let dir = case("metakeep");
        let with_meta = EvidenceSourceInput {
            metadata_json: Some("{\"camera\":\"dashcam\"}".into()),
            ..input(None, "folder", "/e/meta", None)
        };
        register_evidence_source(&dir, &with_meta).unwrap();
        let stored = |dir: &Path| -> String {
            open_case_db(dir)
                .unwrap()
                .query_row(
                    "SELECT metadata_json FROM evidence_sources WHERE path = '/e/meta'",
                    [],
                    |row| row.get(0),
                )
                .unwrap()
        };
        let notes_only = EvidenceSourceInput {
            notes: Some("sealed bag #7".into()),
            ..input(None, "folder", "/e/meta", None)
        };
        register_evidence_source(&dir, &notes_only).unwrap();
        assert_eq!(stored(&dir), "{\"camera\":\"dashcam\"}");
        let replaced = EvidenceSourceInput {
            metadata_json: Some("{\"camera\":\"bodycam\"}".into()),
            ..input(None, "folder", "/e/meta", None)
        };
        let row = register_evidence_source(&dir, &replaced).unwrap();
        assert!(row.changes.iter().any(|c| c.field == "metadata_json"));
        assert_eq!(stored(&dir), "{\"camera\":\"bodycam\"}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn hash_validation_checks_hex_and_length_per_algorithm() {
        let sha = "A".repeat(64);
        let parsed = parse_evidence_hash(&sha).unwrap();
        assert_eq!(parsed.algorithm, "sha256");
        assert_eq!(parsed.hex, "a".repeat(64));
        assert_eq!(
            parse_evidence_hash(&format!("MD5:{}", "0".repeat(32)))
                .unwrap()
                .algorithm,
            "md5"
        );
        assert_eq!(
            parse_evidence_hash(&format!("sha-512:{}", "f".repeat(128)))
                .unwrap()
                .algorithm,
            "sha512"
        );
        for bad in [
            "abc".to_string(),
            "not a hash".to_string(),
            format!("{}g", "a".repeat(63)),
            format!("sha256:{}", "a".repeat(40)),
            format!("crc32:{}", "a".repeat(8)),
            String::new(),
        ] {
            assert!(parse_evidence_hash(&bad).is_err(), "{bad:?} accepted");
        }
    }

    #[test]
    fn invalid_hex_is_rejected_before_anything_is_stored() {
        let dir = case("badhex");
        let err = register_evidence_source(
            &dir,
            &input(Some("SD1"), "folder", "/e/sd", Some("zz-not-hex")),
        )
        .unwrap_err();
        assert!(err.contains("invalid evidence hash"), "{err}");
        assert_eq!(
            summarize_case_db(&dir)
                .unwrap()
                .map(|s| s.evidence_source_count)
                .unwrap_or(0),
            0
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn origin_is_recorded_and_reregistering_a_different_path_is_refused() {
        let dir = case("repoint");
        let typed = "b".repeat(64);
        let first =
            register_evidence_source(&dir, &input(Some("SD1"), "folder", "/e/sd", Some(&typed)))
                .unwrap();
        assert!(first.newly_registered);
        assert_eq!(first.hash_origin.as_deref(), Some("operator-supplied"));
        assert_eq!(first.hash_algorithm.as_deref(), Some("sha256"));

        let err = register_evidence_source(&dir, &input(Some("SD1"), "folder", "/e/other", None))
            .unwrap_err();
        assert!(err.contains("refusing to re-point"), "{err}");
        assert!(err.contains("/e/sd") && err.contains("/e/other"), "{err}");
        let err =
            register_evidence_source(&dir, &input(Some("SD1"), "e01", "/e/sd", None)).unwrap_err();
        assert!(err.contains("refusing to re-point"), "{err}");
        // Same path under a different explicit id is refused too.
        let err = register_evidence_source(&dir, &input(Some("SD2"), "folder", "/e/sd", None))
            .unwrap_err();
        assert!(
            err.contains("already registered as evidence source SD1"),
            "{err}"
        );
        // Nothing moved and nothing was logged.
        let conn = open_case_db(&dir).unwrap();
        let path: String = conn
            .query_row(
                "SELECT path FROM evidence_sources WHERE source_id = 'SD1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(path, "/e/sd");
        assert!(!dir.join(EVIDENCE_SOURCE_LOG_REL_PATH).exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn custody_changes_are_audited_and_computed_hashes_are_protected() {
        let dir = case("audited");
        let computed = "c".repeat(64);
        register_evidence_source_with_origin(
            &dir,
            &input(None, "raw-image", "/e/x.raw", Some(&computed)),
            HashOrigin::Computed,
        )
        .unwrap();
        // A different typed value cannot overwrite a computed digest.
        let err = register_evidence_source(
            &dir,
            &input(None, "raw-image", "/e/x.raw", Some(&"d".repeat(64))),
        )
        .unwrap_err();
        assert!(err.contains("refusing to replace"), "{err}");

        let mut change = input(None, "raw-image", "/e/x.raw", None);
        change.notes = Some("moved to locker 4".into());
        let row = register_evidence_source(&dir, &change).unwrap();
        assert!(!row.newly_registered);
        assert_eq!(
            row.changes,
            vec![EvidenceSourceChange {
                field: "notes",
                old: None,
                new: Some("moved to locker 4".into()),
            }]
        );
        assert_eq!(row.hash_origin.as_deref(), Some("computed"));
        // Re-sending the same values is not a change and logs nothing more.
        let again = register_evidence_source(&dir, &change).unwrap();
        assert!(again.changes.is_empty());

        // A recomputed (different) digest is accepted but audited.
        let recomputed = "e".repeat(64);
        let row = register_evidence_source_with_origin(
            &dir,
            &input(None, "raw-image", "/e/x.raw", Some(&recomputed)),
            HashOrigin::Computed,
        )
        .unwrap();
        assert_eq!(row.changes[0].field, "evidence_hash");
        assert_eq!(row.changes[0].old.as_deref(), Some(computed.as_str()));

        let log = dir.join(EVIDENCE_SOURCE_LOG_REL_PATH);
        let verified = crate::audit::verify_chained_jsonl(&log).unwrap();
        assert_eq!(verified.entries, 2);
        let text = fs::read_to_string(&log).unwrap();
        assert!(text.contains("moved to locker 4") && text.contains(&recomputed));
        let _ = fs::remove_dir_all(dir);
    }
}
