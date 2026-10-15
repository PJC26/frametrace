use crate::util::read_to_string;
use std::path::Path;

pub const SELECTION_SCHEMA_VERSION: u32 = 1;
pub const MARKS_SCHEMA_VERSION: u32 = 1;

pub const MARK_STATUSES: &[&str] = &["reviewed", "important", "needs_verification", "noted"];

#[derive(Debug, Clone, PartialEq)]
pub struct SelectionItem {
    pub selector: String,
    pub kind: Option<String>,
    pub action: Option<String>,
    pub format: Option<String>,
    pub time_seconds: Option<f64>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SelectionFile {
    pub case_id: Option<String>,
    pub items: Vec<SelectionItem>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MarkEntry {
    pub id: String,
    pub status: String,
    pub marked_unix: Option<u64>,
    pub note: Option<String>,
    pub examiner: Option<String>,
    /// Absent for older clients; `{in:null,out:null}` clears a stored range.
    pub range: Option<crate::case_db::RangeSpan>,
}

fn parse_range_field(
    item: &serde_json::Value,
    index: usize,
    path: &Path,
) -> Result<Option<crate::case_db::RangeSpan>, String> {
    let Some(raw) = item.get("range").filter(|raw| !raw.is_null()) else {
        return Ok(None);
    };
    let bad = |what: &str| {
        format!(
            "marks entry {} in {} has invalid range: {what}",
            index + 1,
            path.display()
        )
    };
    let obj = raw.as_object().ok_or_else(|| bad("expected an object"))?;
    // Only an explicit {in, out} object may touch the stored range: an
    // object with other keys (e.g. start/end) must not silently clear it.
    if let Some(unknown) = obj.keys().find(|key| !matches!(key.as_str(), "in" | "out")) {
        return Err(bad(&format!("unknown key {unknown:?} (expected in/out)")));
    }
    if !obj.contains_key("in") && !obj.contains_key("out") {
        return Err(bad("expected explicit in and/or out keys"));
    }
    let point = |key: &str| -> Result<Option<f64>, String> {
        match obj.get(key) {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(value) => value
                .as_f64()
                .filter(|v| v.is_finite() && *v >= 0.0)
                .map(Some)
                .ok_or_else(|| bad(&format!("{key} must be a non-negative number"))),
        }
    };
    let (start, end) = (point("in")?, point("out")?);
    if let (Some(a), Some(b)) = (start, end)
        && b <= a
    {
        return Err(bad("out must be greater than in"));
    }
    Ok(Some(crate::case_db::RangeSpan { start, end }))
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TagEntry {
    pub id: String,
    pub tags: Vec<String>,
}

/// Which save protocol a marks body declared (`protocol` key).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarksProtocol {
    /// No `protocol` key: a plain marks export (full upsert, no deletes).
    Legacy,
    /// `patch-v1`: full marks + tags + `deleted_ids`, last write wins.
    PatchV1,
    /// `patch-v2`: dirty ids only, per-record `base_revisions`.
    PatchV2,
}

impl MarksProtocol {
    /// Wire name (`null` for a body without `protocol`).
    pub fn wire_name(self) -> Option<&'static str> {
        match self {
            MarksProtocol::Legacy => None,
            MarksProtocol::PatchV1 => Some("patch-v1"),
            MarksProtocol::PatchV2 => Some("patch-v2"),
        }
    }
}

/// `batch` of a patch-v2 body: chunk `index` of `total` (0-based) sharing
/// one `id`, so the server can run make-report once per batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarksBatch {
    pub id: String,
    pub index: u64,
    pub total: u64,
}

impl MarksBatch {
    pub fn is_last(&self) -> bool {
        self.index + 1 == self.total
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MarksFile {
    pub case_id: Option<String>,
    pub examiner: Option<String>,
    pub marks: Vec<MarkEntry>,
    pub tags: Vec<TagEntry>,
    pub deleted_ids: Vec<String>,
    pub protocol: MarksProtocol,
    /// `Some` only for patch-v2 (an omitted map is `Some(empty)`: every id
    /// then has base 0 and conflicts with any server revision > 0).
    pub base_revisions: Option<std::collections::BTreeMap<String, u64>>,
    /// patch-v2 writer token (which window wrote; audited in history).
    pub client_id: Option<String>,
    pub batch: Option<MarksBatch>,
}

/// Minimal JSON field/array accessors reused by other modules (e.g. reading
/// the video index for thumbnail generation).
pub(crate) fn json_array_field(text: &str, key: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()?
        .get(key)?
        .as_array()
        .map(|array| serde_json::Value::Array(array.clone()).to_string())
}

pub(crate) fn json_objects_in_array(array_text: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(array_text)
        .ok()
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter(|item| item.is_object())
        .map(|item| item.to_string())
        .collect()
}

pub(crate) fn json_string_field(line: &str, key: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()?
        .get(key)?
        .as_str()
        .map(str::to_string)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportKind {
    Selection,
    Marks,
}

impl ImportKind {
    fn label(self) -> &'static str {
        match self {
            ImportKind::Selection => "selection",
            ImportKind::Marks => "marks",
        }
    }
}

fn parse_case_id_field(
    value: &serde_json::Value,
    path: &Path,
    kind: ImportKind,
) -> Result<Option<String>, String> {
    let label = kind.label();
    let Some(raw) = value.get("case_id") else {
        return Ok(None);
    };
    match raw {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(text) => {
            if text.trim().is_empty() {
                return Err(format!(
                    "{label} file {} has an empty case_id (expected a case id or null)",
                    path.display()
                ));
            }
            Ok(Some(text.clone()))
        }
        other => Err(format!(
            "{label} file {} has a non-string case_id ({other})",
            path.display()
        )),
    }
}

fn parse_schema_version_field(
    value: &serde_json::Value,
    path: &Path,
    kind: ImportKind,
) -> Result<(), String> {
    let Some(raw) = value.get("schema_version") else {
        return Ok(());
    };
    let expected = match kind {
        ImportKind::Selection => SELECTION_SCHEMA_VERSION,
        ImportKind::Marks => MARKS_SCHEMA_VERSION,
    };
    let Some(actual) = raw.as_u64() else {
        return Err(format!(
            "{} file {} has a non-integer schema_version ({raw})",
            kind.label(),
            path.display()
        ));
    };
    if actual != expected as u64 && !(kind == ImportKind::Marks && actual == 2) {
        return Err(format!(
            "{} file {} has unsupported schema_version {} (expected {})",
            kind.label(),
            path.display(),
            actual,
            expected
        ));
    }
    Ok(())
}

pub fn read_case_id(case_dir: &Path) -> Result<String, String> {
    let path = case_dir.join("case.json");
    let text = read_to_string(&path)
        .map_err(|err| format!("failed to read case manifest {}: {err}", path.display()))?;
    let manifest: serde_json::Value = serde_json::from_str(&text)
        .map_err(|err| format!("failed to parse case manifest {}: {err}", path.display()))?;
    manifest
        .get("case_id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            format!(
                "case manifest {} requires a nonempty string case_id",
                path.display()
            )
        })
}

pub fn enforce_case_binding(
    case_dir: &Path,
    case_id: Option<&str>,
    path: &Path,
    kind: ImportKind,
) -> Result<(), String> {
    let current_id = read_case_id(case_dir)?;
    match case_id {
        None => eprintln!(
            "warning: case binding unavailable for {} file {}: missing or null case_id (legacy import)",
            kind.label(),
            path.display()
        ),
        Some(id) if id != current_id => {
            return Err(format!(
                "case_id mismatch: {} file {} is bound to case '{id}' but the target case is '{current_id}'",
                kind.label(),
                path.display()
            ));
        }
        Some(_) => {}
    }
    Ok(())
}

pub fn parse_selection_file(path: &Path) -> Result<SelectionFile, String> {
    let text = read_to_string(path)
        .map_err(|err| format!("failed to read selection file {}: {err}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|err| format!("failed to parse selection file {}: {err}", path.display()))?;
    parse_schema_version_field(&value, path, ImportKind::Selection)?;
    let case_id = parse_case_id_field(&value, path, ImportKind::Selection)?;
    let items_value = value
        .get("items")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("selection file {} is missing items", path.display()))?;
    let mut items = Vec::new();
    for (index, item) in items_value.iter().enumerate() {
        let selector = item
            .get("selector")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                format!(
                    "selection item {} in {} is missing selector",
                    index + 1,
                    path.display()
                )
            })?
            .to_string();
        if selector.trim().is_empty() {
            return Err(format!(
                "selection item {} in {} has an empty selector",
                index + 1,
                path.display()
            ));
        }
        let field = |key: &str| {
            item.get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        };
        items.push(SelectionItem {
            selector,
            kind: field("kind"),
            action: field("action"),
            format: field("format"),
            time_seconds: item
                .get("time_seconds")
                .and_then(serde_json::Value::as_f64)
                .filter(|value| value.is_finite()),
            notes: field("notes"),
        });
    }
    if items.is_empty() {
        return Err(format!("selection file {} has no items", path.display()));
    }
    Ok(SelectionFile { case_id, items })
}

pub fn parse_marks_file(path: &Path) -> Result<MarksFile, String> {
    let text = read_to_string(path)
        .map_err(|err| format!("failed to read marks file {}: {err}", path.display()))?;
    parse_marks_text(&text, path)
}

pub(crate) fn parse_marks_text(text: &str, path: &Path) -> Result<MarksFile, String> {
    let value: serde_json::Value = serde_json::from_str(text)
        .map_err(|err| format!("failed to parse marks file {}: {err}", path.display()))?;
    parse_schema_version_field(&value, path, ImportKind::Marks)?;
    let case_id = parse_case_id_field(&value, path, ImportKind::Marks)?;
    let examiner = value
        .get("examiner")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .filter(|name| !name.trim().is_empty());
    let marks_value = value
        .get("marks")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("marks file {} is missing marks", path.display()))?;
    let mut marks = Vec::new();
    for (index, item) in marks_value.iter().enumerate() {
        let id = item
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                format!(
                    "marks entry {} in {} is missing id",
                    index + 1,
                    path.display()
                )
            })?
            .to_string();
        if id.trim().is_empty() {
            return Err(format!(
                "marks entry {} in {} has an empty id",
                index + 1,
                path.display()
            ));
        }
        let status = item
            .get("status")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                format!(
                    "marks entry {} in {} is missing status",
                    index + 1,
                    path.display()
                )
            })?
            .to_string();
        if !MARK_STATUSES.contains(&status.as_str()) {
            return Err(format!(
                "marks entry {} in {} has unsupported status '{}' (expected one of {})",
                index + 1,
                path.display(),
                status,
                MARK_STATUSES.join(", ")
            ));
        }
        let range = parse_range_field(item, index, path)?;
        marks.push(MarkEntry {
            id,
            range,
            status,
            examiner: item
                .get("examiner")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            marked_unix: item.get("marked_unix").and_then(serde_json::Value::as_u64),
            note: item
                .get("note")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        });
    }
    let protocol = match value.get("protocol") {
        None => MarksProtocol::Legacy,
        Some(raw) => match raw.as_str() {
            Some("patch-v1") => MarksProtocol::PatchV1,
            Some("patch-v2") => MarksProtocol::PatchV2,
            _ => return Err("unsupported marks protocol".into()),
        },
    };
    let mut tags: Vec<TagEntry> = value
        .get("tags")
        .map(|raw| serde_json::from_value(raw.clone()))
        .transpose()
        .map_err(|err| format!("invalid tags: {err}"))?
        .unwrap_or_default();
    // v2 only: a mark entry may carry its own `tags`; they are folded into
    // the tag list so every id has at most one tag entry. Legacy bodies
    // never read a mark's `tags` key (as before).
    if protocol == MarksProtocol::PatchV2 {
        for (index, item) in marks_value.iter().enumerate() {
            let Some(raw) = item.get("tags") else {
                continue;
            };
            let id = marks[index].id.clone();
            let inline: Vec<String> = serde_json::from_value(raw.clone()).map_err(|err| {
                format!(
                    "marks entry {} in {} has invalid tags: {err}",
                    index + 1,
                    path.display()
                )
            })?;
            if tags.iter().any(|entry| entry.id == id) {
                return Err(format!(
                    "marks entry {} in {} has tags both inline and in the tags list",
                    index + 1,
                    path.display()
                ));
            }
            tags.push(TagEntry { id, tags: inline });
        }
    }
    let deleted_ids: Vec<String> = value
        .get("deleted_ids")
        .map(|raw| serde_json::from_value(raw.clone()))
        .transpose()
        .map_err(|err| format!("invalid deleted_ids: {err}"))?
        .unwrap_or_default();
    if !deleted_ids.is_empty() && protocol == MarksProtocol::Legacy {
        return Err("deleted_ids requires patch-v1 protocol".into());
    }
    let (base_revisions, client_id, batch) = if protocol == MarksProtocol::PatchV2 {
        parse_patch_v2_fields(&value, path)?
    } else {
        (None, None, None)
    };
    let mut ids = std::collections::HashSet::new();
    for id in marks.iter().map(|mark| &mark.id).chain(deleted_ids.iter()) {
        if id.trim().is_empty() || !ids.insert(id) {
            return Err("empty or duplicate mark/deletion id".into());
        }
    }
    let mut tag_ids = std::collections::HashSet::new();
    for tag in &tags {
        if tag.id.trim().is_empty()
            || !tag_ids.insert(&tag.id)
            || tag.tags.iter().any(|text| text.trim().is_empty())
        {
            return Err("empty or duplicate tag id or empty tag".into());
        }
    }
    if marks.is_empty() && tags.is_empty() && deleted_ids.is_empty() {
        return Err(format!("marks file {} has no marks", path.display()));
    }
    Ok(MarksFile {
        case_id,
        examiner,
        marks,
        tags,
        deleted_ids,
        protocol,
        base_revisions,
        client_id,
        batch,
    })
}

type PatchV2Fields = (
    Option<std::collections::BTreeMap<String, u64>>,
    Option<String>,
    Option<MarksBatch>,
);

/// patch-v2 envelope: `base_revisions` (object of non-negative integers,
/// or -1 for an unsafe base; absent/null = empty), `client_id` (non-empty string when present) and
/// `batch` (`{id, index, total}` with `index < total`).
fn parse_patch_v2_fields(value: &serde_json::Value, path: &Path) -> Result<PatchV2Fields, String> {
    let bad = |what: String| format!("marks file {} (patch-v2): {what}", path.display());
    let mut base = std::collections::BTreeMap::new();
    match value.get("base_revisions") {
        None | Some(serde_json::Value::Null) => {}
        Some(serde_json::Value::Object(map)) => {
            for (id, rev) in map {
                // R5 (U1 ↔ P): -1 marks a draft made while the viewer could
                // not trust its view; it always conflicts (see
                // `case_db::UNSAFE_BASE_REVISION`). Any other negative or
                // non-integer value is still refused.
                let rev = match rev.as_u64() {
                    Some(rev) => rev,
                    None if rev.as_i64() == Some(-1) => crate::case_db::UNSAFE_BASE_REVISION,
                    None => {
                        return Err(bad(format!(
                            "base_revisions[{id:?}] must be a non-negative integer or -1"
                        )));
                    }
                };
                if id.trim().is_empty() {
                    return Err(bad("base_revisions has an empty id".into()));
                }
                base.insert(id.clone(), rev);
            }
        }
        Some(_) => return Err(bad("base_revisions must be an object".into())),
    }
    let client_id = match value.get("client_id") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(text)) if !text.trim().is_empty() => Some(text.clone()),
        Some(_) => return Err(bad("client_id must be a non-empty string".into())),
    };
    let batch = match value.get("batch") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::Object(map)) => {
            let id = map
                .get("id")
                .and_then(serde_json::Value::as_str)
                .filter(|id| !id.trim().is_empty())
                .ok_or_else(|| bad("batch.id must be a non-empty string".into()))?;
            let index = map
                .get("index")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| bad("batch.index must be a non-negative integer".into()))?;
            let total = map
                .get("total")
                .and_then(serde_json::Value::as_u64)
                .filter(|total| *total > 0)
                .ok_or_else(|| bad("batch.total must be a positive integer".into()))?;
            if index >= total {
                return Err(bad(format!("batch.index {index} must be < total {total}")));
            }
            Some(MarksBatch {
                id: id.to_string(),
                index,
                total,
            })
        }
        Some(_) => return Err(bad("batch must be an object".into())),
    };
    Ok((Some(base), client_id, batch))
}

/// Effective action for a selection item when the file omits it.
pub fn effective_action(item: &SelectionItem) -> &'static str {
    match item.action.as_deref() {
        Some("export") => "export",
        Some("proxy") => "proxy",
        Some("thumbnail") => "thumbnail",
        Some("validate") => "validate",
        _ => match item.kind.as_deref() {
            Some("carved") | Some("filesystem") => "validate",
            _ => "export",
        },
    }
}

pub fn effective_format(item: &SelectionItem) -> Result<&'static str, String> {
    match item.format.as_deref() {
        None => Ok("mp4"),
        Some(raw) => match raw.to_ascii_lowercase().as_str() {
            "mp4" => Ok("mp4"),
            "avi" => Ok("avi"),
            other => Err(format!(
                "unsupported export format '{other}' (use mp4 or avi)"
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        effective_action, effective_format, parse_marks_file, parse_marks_text,
        parse_selection_file,
    };
    use std::fs;
    use std::path::PathBuf;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "frametrace-selection-{name}-{}",
            std::process::id()
        ))
    }

    #[test]
    fn range_object_with_unknown_keys_is_rejected_not_cleared() {
        for range in [r#"{"start":1.5,"end":4}"#, r#"{}"#, r#"{"in":1,"end":4}"#] {
            let text = format!(
                r#"{{"schema_version":2,"marks":[{{"id":"a","status":"important","range":{range}}}]}}"#
            );
            let err = parse_marks_text(&text, std::path::Path::new("m.json")).unwrap_err();
            assert!(err.contains("invalid range"), "{range}: {err}");
        }
    }

    #[test]
    fn parse_patch_v2_fields_and_rejects_bad_batch_or_base_revisions() {
        use super::{MarksBatch, MarksProtocol};
        let p = std::path::Path::new("m.json");
        let full = r#"{"schema_version":2,"protocol":"patch-v2","case_id":"FT-1","examiner":"kim",
            "client_id":"w-7f3a","exported_unix":1,
            "batch":{"id":"b-01","index":0,"total":3},
            "base_revisions":{"vid_000001":4,"vid_000009":0},
            "marks":[{"id":"vid_000001","status":"important","marked_unix":5,"note":"n","examiner":"kim",
                      "range":{"in":1.5,"out":4.0},"tags":["사고"]}],
            "tags":[{"id":"vid_000009","tags":[]}],
            "deleted_ids":["vid_000009"]}"#;
        let file = parse_marks_text(full, p).unwrap();
        assert_eq!(file.protocol, MarksProtocol::PatchV2);
        assert_eq!(file.client_id.as_deref(), Some("w-7f3a"));
        assert_eq!(
            file.batch,
            Some(MarksBatch {
                id: "b-01".into(),
                index: 0,
                total: 3
            })
        );
        assert!(!file.batch.as_ref().unwrap().is_last());
        let base = file.base_revisions.as_ref().unwrap();
        assert_eq!(base.get("vid_000001"), Some(&4));
        assert_eq!(base.get("vid_000009"), Some(&0));
        // Inline mark tags are folded into one tag entry per id.
        let mut tag_ids: Vec<(&str, usize)> = file
            .tags
            .iter()
            .map(|t| (t.id.as_str(), t.tags.len()))
            .collect();
        tag_ids.sort();
        assert_eq!(tag_ids, [("vid_000001", 1), ("vid_000009", 0)]);
        assert_eq!(file.deleted_ids, ["vid_000009"]);

        // Minimal v2: no base map at all is Some(empty) — every id base 0.
        let minimal = r#"{"protocol":"patch-v2","marks":[{"id":"a","status":"noted"}]}"#;
        let file = parse_marks_text(minimal, p).unwrap();
        assert_eq!(file.base_revisions, Some(Default::default()));
        assert!(file.client_id.is_none() && file.batch.is_none());
        let single = r#"{"protocol":"patch-v2","batch":{"id":"b","index":0,"total":1},"marks":[{"id":"a","status":"noted"}]}"#;
        assert!(
            parse_marks_text(single, p)
                .unwrap()
                .batch
                .unwrap()
                .is_last()
        );

        // Legacy bodies are untouched: v2 envelope fields are ignored and a
        // mark's inline tags are NOT read.
        let legacy = r#"{"schema_version":2,"protocol":"patch-v1","client_id":"","batch":5,"base_revisions":"x",
            "marks":[{"id":"a","status":"noted","tags":["ignored"]}],"deleted_ids":["b"]}"#;
        let file = parse_marks_text(legacy, p).unwrap();
        assert_eq!(file.protocol, MarksProtocol::PatchV1);
        assert!(file.base_revisions.is_none() && file.client_id.is_none() && file.batch.is_none());
        assert!(file.tags.is_empty());
        let plain = r#"{"marks":[{"id":"a","status":"noted"}]}"#;
        assert_eq!(
            parse_marks_text(plain, p).unwrap().protocol,
            MarksProtocol::Legacy
        );

        let body = |extra: &str| {
            format!(r#"{{"protocol":"patch-v2",{extra}"marks":[{{"id":"a","status":"noted"}}]}}"#)
        };
        for (extra, what) in [
            (r#""batch":{"id":"b","index":3,"total":3},"#, "batch.index"),
            (r#""batch":{"id":"b","index":0,"total":0},"#, "batch.total"),
            (r#""batch":{"id":"","index":0,"total":1},"#, "batch.id"),
            (r#""batch":[],"#, "batch must be an object"),
            (r#""base_revisions":{"a":-2},"#, "base_revisions"),
            (r#""base_revisions":{"a":-1.0},"#, "base_revisions"),
            (r#""base_revisions":{"a":1.5},"#, "base_revisions"),
            (r#""base_revisions":{"a":"1"},"#, "base_revisions"),
            (
                r#""base_revisions":[],"#,
                "base_revisions must be an object",
            ),
            (r#""client_id":"","#, "client_id"),
            (r#""client_id":7,"#, "client_id"),
        ] {
            let err = parse_marks_text(&body(extra), p).unwrap_err();
            assert!(err.contains(what), "{extra}: {err}");
        }
        // R5: -1 is the unsafe-base marker (always a conflict server-side).
        let unsafe_body = body(r#""base_revisions":{"a":-1},"#);
        let file = parse_marks_text(&unsafe_body, p).unwrap();
        assert_eq!(
            file.base_revisions.unwrap().get("a"),
            Some(&crate::case_db::UNSAFE_BASE_REVISION)
        );
        // Same id tagged inline and in the list is ambiguous.
        let twice = r#"{"protocol":"patch-v2","marks":[{"id":"a","status":"noted","tags":["x"]}],"tags":[{"id":"a","tags":["y"]}]}"#;
        assert!(
            parse_marks_text(twice, p)
                .unwrap_err()
                .contains("both inline")
        );
        assert!(parse_marks_text(r#"{"protocol":"patch-v3","marks":[]}"#, p).is_err());
    }

    #[test]
    fn parse_marks_text_accepts_and_omits_range() {
        let p = std::path::Path::new("marks.json");
        let text = r#"{"schema_version":2,"marks":[
            {"id":"a","status":"important","range":{"in":1.5,"out":4}},
            {"id":"b","status":"noted"},
            {"id":"c","status":"noted","range":{"in":null,"out":null}},
            {"id":"d","status":"noted","range":{"in":2}}]}"#;
        let file = parse_marks_text(text, p).unwrap();
        let r = |i: usize| file.marks[i].range;
        assert_eq!(r(0).map(|x| (x.start, x.end)), Some((Some(1.5), Some(4.0))));
        assert_eq!(r(1), None);
        assert_eq!(r(2).map(|x| (x.start, x.end)), Some((None, None)));
        assert_eq!(r(3).map(|x| (x.start, x.end)), Some((Some(2.0), None)));
        let bad = r#"{"schema_version":2,"marks":[{"id":"a","status":"important","range":{"in":5,"out":2}}]}"#;
        assert!(parse_marks_text(bad, p).is_err());
    }

    #[test]
    fn parses_selection_items() {
        let path = temp_path("parse");
        fs::write(
            &path,
            r#"{"schema_version":1,"case_id":"FT-1","items":[{"selector":"vid_000001","kind":"video","action":"export","format":"mp4","notes":"a\"b"},{"selector":"carve_000001","kind":"carved","action":"validate"}]}"#,
        )
        .unwrap();
        let selection = parse_selection_file(&path).unwrap();
        assert_eq!(selection.case_id.as_deref(), Some("FT-1"));
        assert_eq!(selection.items.len(), 2);
        assert_eq!(selection.items[0].selector, "vid_000001");
        assert_eq!(selection.items[0].notes.as_deref(), Some("a\"b"));
        assert_eq!(effective_action(&selection.items[0]), "export");
        assert_eq!(effective_action(&selection.items[1]), "validate");
        assert_eq!(effective_format(&selection.items[0]).unwrap(), "mp4");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn defaults_action_from_kind_and_rejects_bad_format() {
        let mut item = super::SelectionItem {
            selector: "vid_000009".to_string(),
            kind: Some("video".to_string()),
            action: None,
            format: None,
            time_seconds: None,
            notes: None,
        };
        assert_eq!(effective_action(&item), "export");
        item.kind = Some("carved".to_string());
        assert_eq!(effective_action(&item), "validate");
        item.format = Some("mkv".to_string());
        assert!(effective_format(&item).is_err());
    }

    #[test]
    fn rejects_selection_without_selector() {
        let path = temp_path("noselector");
        fs::write(&path, r#"{"schema_version":1,"items":[{"kind":"video"}]}"#).unwrap();
        let error = parse_selection_file(&path).unwrap_err();
        assert!(error.contains("missing selector"), "unexpected: {error}");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn parses_marks_and_rejects_unknown_status() {
        let path = temp_path("marks");
        fs::write(
            &path,
            r#"{"schema_version":1,"case_id":"FT-1","marks":[{"id":"vid_000001","status":"important","marked_unix":100},{"id":"carve_000002","status":"reviewed"}]}"#,
        )
        .unwrap();
        let marks = parse_marks_file(&path).unwrap();
        assert_eq!(marks.marks.len(), 2);
        assert_eq!(marks.marks[0].marked_unix, Some(100));

        fs::write(&path, r#"{"marks":[{"id":"x","status":"bogus"}]}"#).unwrap();
        let error = parse_marks_file(&path).unwrap_err();
        assert!(error.contains("unsupported status"), "unexpected: {error}");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn case_binding_rejects_invalid_import_metadata() {
        let path = temp_path("invalid-binding");
        for field in ["schema_version", "case_id"] {
            let invalid = if field == "schema_version" {
                vec![
                    serde_json::json!(0),
                    serde_json::json!(3),
                    serde_json::json!(1.5),
                    serde_json::json!("1"),
                    serde_json::Value::Null,
                    serde_json::json!(true),
                ]
            } else {
                vec![
                    serde_json::json!(""),
                    serde_json::json!(" \t"),
                    serde_json::json!(123),
                    serde_json::json!(true),
                    serde_json::json!([]),
                    serde_json::json!({}),
                ]
            };
            for value in invalid {
                let mut payload = serde_json::json!({
                    "schema_version": 1, "case_id": "FT-1",
                    "items": [{"selector": "vid_000001"}],
                    "marks": [{"id": "vid_000001", "status": "reviewed"}]
                });
                payload[field] = value;
                fs::write(&path, payload.to_string()).unwrap();
                assert!(parse_selection_file(&path).unwrap_err().contains(field));
                assert!(parse_marks_file(&path).unwrap_err().contains(field));
            }
        }
        fs::write(
            &path,
            r#"{"schema_version":2,"items":[{"selector":"vid_1"}]}"#,
        )
        .unwrap();
        assert!(
            parse_selection_file(&path)
                .unwrap_err()
                .contains("schema_version")
        );
        for id in ["", " \t"] {
            fs::write(
                &path,
                serde_json::json!({"marks": [{"id": id, "status": "reviewed"}]}).to_string(),
            )
            .unwrap();
            assert!(parse_marks_file(&path).unwrap_err().contains("empty id"));
        }
        let _ = fs::remove_file(path);
    }

    #[test]
    fn case_binding_accepts_legacy_metadata_and_viewer_marks_v2() {
        let path = temp_path("legacy-binding");
        for version in [None, Some(1), Some(2)] {
            for case_id in [
                None,
                Some(serde_json::Value::Null),
                Some(serde_json::json!("FT-1")),
            ] {
                let mut payload = serde_json::json!({
                    "items": [{"selector": "vid_000001"}],
                    "marks": [{"id": "vid_000001", "status": "noted", "note": "memo"}]
                });
                if let Some(version) = version {
                    payload["schema_version"] = serde_json::json!(version);
                }
                if let Some(case_id) = case_id {
                    payload["case_id"] = case_id;
                }
                fs::write(&path, payload.to_string()).unwrap();
                assert_eq!(
                    parse_marks_file(&path).unwrap().case_id.as_deref(),
                    payload["case_id"].as_str()
                );
                if version != Some(2) {
                    assert_eq!(
                        parse_selection_file(&path).unwrap().case_id.as_deref(),
                        payload["case_id"].as_str()
                    );
                }
            }
        }
        let _ = fs::remove_file(path);
    }

    #[test]
    fn shared_json_accessors_handle_escaped_and_nested_values() {
        let index = r#"{"videos":[{"id":"vid_1","source_path":"C:\\ev\\a.mp4"},{"id":"vid_2","source_path":"C:\\ev\\b.mp4"}]}"#;
        let items = super::json_array_field(index, "videos").unwrap();
        let objects = super::json_objects_in_array(&items);
        assert_eq!(objects.len(), 2);
        assert_eq!(
            super::json_string_field(&objects[0], "source_path").unwrap(),
            "C:\\ev\\a.mp4"
        );
        // A missing array key must be None, not an error.
        assert!(super::json_array_field(index, "missing").is_none());
        assert!(super::json_string_field(&objects[0], "missing").is_none());
    }
}
