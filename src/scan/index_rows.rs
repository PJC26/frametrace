//! Read-only view of `db/videos.jsonl` for export/compare consumers (E8).
//!
//! Unlike a `filter_map(.. .ok())` reader, nothing is dropped silently:
//! unparseable lines (a torn tail, a hand-edited row) and rows without an
//! id are counted so the caller can say how much of the index it did NOT
//! cover, and rows a rescan marked `index_status: "stale"` (the file was
//! absent at that scan) are flagged rather than presented as present
//! evidence. An mtime of 0 is a "no timestamp" placeholder, never 1970.

use crate::util::read_to_string;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct IndexRow {
    pub id: String,
    /// Display path (lossy for non-Unicode names; see `source_path_raw_hex`).
    pub source_path: String,
    /// Lossless raw encoding when the name is not valid Unicode (E1).
    pub source_path_raw_hex: Option<String>,
    pub size_bytes: Option<u64>,
    /// `None` when absent or recorded as 0 (placeholder, not a real time).
    pub modified_unix: Option<u64>,
    pub sha256: Option<String>,
    /// The record carries `index_status: "stale"` — the file was missing
    /// when a later rescan ran; its facts describe the last time it was seen.
    pub stale: bool,
    pub stale_since_unix: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct IndexRows {
    pub rows: Vec<IndexRow>,
    /// Non-blank lines that are not a JSON object (torn tail, corruption).
    pub unparseable_lines: usize,
    /// JSON objects without a non-empty string `id`.
    pub rows_without_id: usize,
}

impl IndexRows {
    pub fn stale_count(&self) -> usize {
        self.rows.iter().filter(|row| row.stale).count()
    }

    /// Lines of the index that did not become a row.
    pub fn skipped_lines(&self) -> usize {
        self.unparseable_lines + self.rows_without_id
    }

    /// Human-readable coverage warnings (empty when every line was read).
    pub fn warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.unparseable_lines > 0 {
            out.push(format!(
                "{} line(s) of db/videos.jsonl could not be parsed and were not included",
                self.unparseable_lines
            ));
        }
        if self.rows_without_id > 0 {
            out.push(format!(
                "{} db/videos.jsonl record(s) had no id and were not included",
                self.rows_without_id
            ));
        }
        out
    }
}

/// Reads the case index. A missing index is empty; an unreadable one is an
/// error (never "no videos").
pub fn read_index_rows(case_dir: &Path) -> Result<IndexRows, String> {
    let path = case_dir.join("db/videos.jsonl");
    let text = match read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(IndexRows::default()),
        Err(err) => {
            return Err(format!(
                "failed to read indexed videos {}: {err}",
                path.display()
            ));
        }
    };
    Ok(parse_index_rows(&text))
}

pub(crate) fn parse_index_rows(text: &str) -> IndexRows {
    let mut out = IndexRows::default();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(serde_json::Value::Object(value)) = serde_json::from_str::<serde_json::Value>(line)
        else {
            out.unparseable_lines += 1;
            continue;
        };
        let string = |key: &str| {
            value
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        };
        let id = string("id").unwrap_or_default();
        if id.is_empty() {
            out.rows_without_id += 1;
            continue;
        }
        out.rows.push(IndexRow {
            id,
            source_path: string("source_path").unwrap_or_default(),
            source_path_raw_hex: string("source_path_raw_hex"),
            size_bytes: value.get("size_bytes").and_then(serde_json::Value::as_u64),
            modified_unix: value
                .get("modified_unix")
                .and_then(serde_json::Value::as_u64)
                .filter(|mtime| *mtime > 0),
            sha256: string("sha256"),
            stale: value
                .get("index_status")
                .and_then(serde_json::Value::as_str)
                == Some("stale"),
            stale_since_unix: value
                .get("stale_since_unix")
                .and_then(serde_json::Value::as_u64),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::parse_index_rows;

    #[test]
    fn counts_broken_lines_flags_stale_and_drops_zero_mtime() {
        let text = concat!(
            r#"{"id":"vid_1","source_path":"/a.mp4","size_bytes":3,"modified_unix":0,"sha256":"ab"}"#,
            "\n",
            r#"{"id":"vid_2","source_path":"/b.mp4","modified_unix":5,"index_status":"stale","stale_since_unix":9}"#,
            "\n",
            r#"{"source_path":"/no-id.mp4"}"#,
            "\n",
            "\n",
            r#"{"id":"vid_3","source_pa"#,
        );
        let rows = parse_index_rows(text);
        assert_eq!(rows.rows.len(), 2);
        assert_eq!(rows.unparseable_lines, 1);
        assert_eq!(rows.rows_without_id, 1);
        assert_eq!(rows.skipped_lines(), 2);
        assert_eq!(rows.rows[0].modified_unix, None, "mtime 0 is no timestamp");
        assert!(!rows.rows[0].stale);
        assert!(rows.rows[1].stale);
        assert_eq!(rows.rows[1].stale_since_unix, Some(9));
        assert_eq!(rows.stale_count(), 1);
        assert_eq!(rows.warnings().len(), 2);
    }
}
