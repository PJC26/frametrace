use super::*;

#[derive(Debug, Clone)]
pub(super) struct IdRegistry {
    pub(super) ids_by_source: HashMap<String, String>,
    pub(super) next_number: usize,
}

impl IdRegistry {
    pub(super) fn id_for(&mut self, path: &Path) -> String {
        let source_path = source_key(path);
        if let Some(id) = self.ids_by_source.get(&source_path) {
            return id.clone();
        }

        let id = format!("vid_{:06}", self.next_number);
        self.next_number += 1;
        self.ids_by_source.insert(source_path, id.clone());
        id
    }
}

/// Older case indexes stored source paths with the Windows extended-length
/// prefix (`\\?\`), newer ones store clean paths. Matching on the normalized
/// form keeps video ids stable across binary upgrades.
pub(super) fn normalize_source_key(source_path: &str) -> String {
    crate::util::strip_windows_extended_prefix(Path::new(source_path))
        .to_string_lossy()
        .to_string()
}

/// Lossless id key for an OS path. Valid-Unicode paths keep the exact key
/// older binaries used (`normalize_source_key` of the string), so their
/// ids stay stable. A non-Unicode name (`\xff.mp4` on unix, an unpaired
/// surrogate on NTFS) used to collapse through `to_string_lossy` onto the
/// same U+FFFD key as every other such name, so two files shared one id
/// and one record. Those keys now append the raw encoding
/// (`crate::model::path_raw_hex`) after a NUL, which no real path string
/// contains: they never collide with each other or with a UTF-8 path that
/// literally contains U+FFFD.
pub(super) fn source_key(path: &Path) -> String {
    let display = normalize_source_key(&path.to_string_lossy());
    match crate::model::path_raw_hex(path) {
        None => display,
        Some(raw) => format!("{display}\0{raw}"),
    }
}

#[derive(Debug, Clone)]
pub(super) struct IndexedRecordLine {
    pub(super) id: String,
    pub(super) source_path: String,
    /// Field order and spelling must stay identical to the published
    /// JSONL contract, so the serialized form is kept verbatim and the
    /// structured fields are derived through serde.
    pub(super) json_line: String,
    pub(super) record: VideoRecord,
}

impl IndexedRecordLine {
    /// Lossless id key of this record's source (see [`source_key`]).
    pub(super) fn key(&self) -> String {
        source_key(&self.record.source_path)
    }

    pub(super) fn from_record(record: &VideoRecord) -> Self {
        Self {
            id: record.id.clone(),
            source_path: record.source_path.to_string_lossy().to_string(),
            json_line: record.to_json(),
            record: record.clone(),
        }
    }

    pub(super) fn to_tsv_row(&self) -> String {
        let profile = &self.record.source_profile;
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            tsv_escape(&self.id),
            tsv_escape(&self.source_path),
            tsv_escape(&self.record.relative_path),
            tsv_escape(&self.record.extension),
            self.record.size_bytes,
            tsv_escape(self.record.sha256.as_deref().unwrap_or("")),
            tsv_escape(&profile.vendor),
            tsv_escape(&profile.parser),
            tsv_escape(&profile.confidence)
        )
    }

    pub(super) fn to_db_row(&self) -> IndexedVideoRow {
        let probe = &self.record.probe;
        IndexedVideoRow {
            id: self.id.clone(),
            source_path: self.source_path.clone(),
            file_url: path_to_file_url(&self.record.source_path),
            relative_path: self.record.relative_path.clone(),
            extension: self.record.extension.clone(),
            size_bytes: self.record.size_bytes,
            modified_unix: self.record.modified_unix,
            sha256: self.record.sha256.clone(),
            hash_status: clone_or(&self.record.hash_status, "unknown"),
            confidence: clone_or(&self.record.confidence, "unknown"),
            source_profile_json: self.record.source_profile.to_json(),
            duration_seconds: probe.duration_seconds,
            format_name: probe.format_name.clone(),
            video_codec: probe.video_codec.clone(),
            audio_codec: probe.audio_codec.clone(),
            width: probe.width.map(u64::from),
            height: probe.height.map(u64::from),
            ffprobe_ok: probe.ok,
            ffprobe_error: probe.error.clone(),
            ffprobe_json: probe.raw_json.clone(),
            record_json: self.json_line.clone(),
        }
    }
}

pub(super) fn clone_or(value: &str, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

pub(super) fn load_existing_video_ids(case_dir: &Path) -> Result<IdRegistry, String> {
    let existing = load_existing_record_lines(case_dir)?;
    let mut ids_by_source = HashMap::new();
    let mut max_number = 0usize;
    // The JSONL record carries the lossless path (E1); the SQLite row only
    // its lossy display string. An id the JSONL already describes is keyed
    // from the JSONL alone, so a lossy DB spelling can never hand that id
    // to a different file.
    let jsonl_ids = existing
        .iter()
        .map(|record| record.id.clone())
        .collect::<std::collections::HashSet<_>>();

    for record in case_db::load_video_ids(case_dir)? {
        if let Some(number) = record.id.strip_prefix("vid_").and_then(parse_usize) {
            max_number = max_number.max(number);
        }
        if !jsonl_ids.contains(&record.id) {
            ids_by_source.insert(normalize_source_key(&record.source_path), record.id);
        }
    }

    for record in existing {
        if let Some(number) = record.id.strip_prefix("vid_").and_then(parse_usize) {
            max_number = max_number.max(number);
        }
        ids_by_source.insert(record.key(), record.id);
    }

    Ok(IdRegistry {
        ids_by_source,
        next_number: max_number + 1,
    })
}

/// Fresh records replace indexed ones wholesale. That is safe because a
/// fresh record already carries every per-file fact the pass did not
/// compute itself (scan_folder's `carried_facts` rule): a rescan without
/// `--hash` keeps the recorded SHA-256/hash_status and one with
/// `--no-ffprobe` keeps the recorded probe, as long as size+mtime still
/// match. A lighter pass therefore never downgrades evidence facts; only
/// a changed file (or an explicit non-incremental `--hash`/ffprobe pass,
/// which recomputes) replaces them.
pub(super) fn merge_existing_with_scan(
    case_dir: &Path,
    result: &ScanResult,
) -> Result<Vec<IndexedRecordLine>, String> {
    let current_by_source = result
        .records
        .iter()
        .map(IndexedRecordLine::from_record)
        .map(|record| (record.key(), record))
        .collect::<HashMap<_, _>>();

    let mut merged = Vec::new();
    let mut updated_keys = std::collections::HashSet::new();
    let mut stale_keys = std::collections::HashSet::new();
    for existing in load_existing_record_lines(case_dir)? {
        let key = existing.key();
        if let Some(updated) = current_by_source.get(&key) {
            // Legacy and clean spellings of one file may both exist in old
            // indexes; converge to a single refreshed record.
            if updated_keys.insert(key.clone()) {
                merged.push(updated.clone());
            }
        } else if stale_keys.contains(&key) {
            // Duplicate spelling of an already-decided record; skip.
            continue;
        } else if source_presence(&existing.record.source_path) != SourcePresence::Missing {
            // (Checked on the lossless OS path: the lossy display string of
            // a non-Unicode name does not exist on disk.)
            // The file EXISTS on disk — it merely belongs to a different
            // registered source that this scan did not cover. Sequential
            // multi-source scans (README workflow: scan E:\ then
            // E:\BLACKBOX) must not mark each other's live evidence stale.
            // A file whose metadata could not be read (permissions, symlink
            // loop, I/O error) is not proof of absence either: it is carried
            // forward unchanged and the scan warns "metadata-error".
            merged.push(existing);
            stale_keys.insert(key.clone());
        } else if stale_keys.insert(key.clone()) {
            // Genuinely missing from disk: mark stale once per file, even
            // if indexed under both \\?\ and clean spellings.
            merged.push(existing.mark_stale(result.scanned_unix));
        }
    }

    let existing_sources = merged
        .iter()
        .map(IndexedRecordLine::key)
        .collect::<std::collections::HashSet<_>>();
    for record in result.records.iter().map(IndexedRecordLine::from_record) {
        if !existing_sources.contains(&record.key()) {
            merged.push(record);
        }
    }

    merged.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(merged)
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SourcePresence {
    File,
    /// Definitely absent: not found, a path component is not a directory,
    /// or the path resolves to something other than a regular file.
    Missing,
    /// `fs::metadata` failed for a reason other than absence.
    MetadataError,
}

pub(super) fn source_presence(path: &Path) -> SourcePresence {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => SourcePresence::File,
        Ok(_) => SourcePresence::Missing,
        Err(err) if matches!(err.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            SourcePresence::Missing
        }
        Err(_) => SourcePresence::MetadataError,
    }
}

pub(super) fn load_existing_record_lines(
    case_dir: &Path,
) -> Result<Vec<IndexedRecordLine>, String> {
    let path = case_dir.join("db/videos.jsonl");
    let text = match read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(format!("failed to read {}: {err}", path.display())),
    };

    let mut records = Vec::new();
    for (line_index, line) in json_record_lines(&text).into_iter().enumerate() {
        let line = line.trim();
        let record: VideoRecord = serde_json::from_str(line).map_err(|err| {
            format!(
                "failed to parse {} record {}: {err}",
                path.display(),
                line_index + 1
            )
        })?;
        records.push(IndexedRecordLine {
            id: record.id.clone(),
            source_path: record.source_path.to_string_lossy().to_string(),
            json_line: line.to_string(),
            record,
        });
    }
    Ok(records)
}

impl IndexedRecordLine {
    pub(super) fn mark_stale(mut self, stale_since_unix: u64) -> Self {
        // Stale markers are appended to the stored JSONL verbatim line so the
        // published record contract (field order, spelling) stays byte-stable
        // for already-indexed evidence. serde_json re-serialization would
        // reorder fields, so the edit stays textual.
        self.json_line = set_json_field(
            &set_json_field(&self.json_line, "index_status", "\"stale\""),
            "stale_since_unix",
            &stale_since_unix.to_string(),
        );
        self
    }
}

pub(super) fn scan_index_json(result: &ScanResult, records: &[IndexedRecordLine]) -> String {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str("  \"schema_version\": 1,\n");
    out.push_str(&format!(
        "  \"source_path\": \"{}\",\n",
        json_escape(&result.source_path.to_string_lossy())
    ));
    out.push_str(&format!("  \"scanned_unix\": {},\n", result.scanned_unix));
    out.push_str(&format!("  \"video_count\": {},\n", records.len()));
    out.push_str(&format!(
        "  \"total_bytes\": {},\n",
        records
            .iter()
            .map(|record| record.record.size_bytes)
            .sum::<u64>()
    ));
    out.push_str(&format!(
        "  \"unchanged_files\": {},\n",
        result.unchanged_files
    ));
    out.push_str("  \"warnings\": [\n");
    for (index, warning) in result.warnings.iter().enumerate() {
        out.push_str(&format!("    \"{}\"", json_escape(warning)));
        if index + 1 != result.warnings.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ],\n");
    out.push_str("  \"options\": {\n");
    out.push_str(&format!(
        "    \"hash_files\": {},\n",
        result.options.hash_files
    ));
    out.push_str(&format!(
        "    \"use_ffprobe\": {},\n",
        result.options.use_ffprobe
    ));
    out.push_str(&format!(
        "    \"incremental\": {},\n",
        result.options.incremental
    ));
    match result.options.max_depth {
        Some(max_depth) => out.push_str(&format!("    \"max_depth\": {}\n", max_depth)),
        None => out.push_str("    \"max_depth\": null\n"),
    }
    out.push_str("  },\n");
    out.push_str("  \"videos\": [\n");
    for (index, record) in records.iter().enumerate() {
        out.push_str("    ");
        out.push_str(&record.json_line);
        if index + 1 != records.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ]\n");
    out.push_str("}\n");
    out
}
