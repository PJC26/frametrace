use crate::case_db::{self, IndexedVideoRow};
use crate::checkpoint::{self, ResumeMode, RunCheckpoint};
use crate::detector;
use crate::ffprobe;
use crate::model::{ProbeSummary, ScanOptions, ScanResult, VideoRecord};
use crate::sha256;
use crate::util::{
    canonicalize_display, json_escape, now_unix, path_to_file_url, read_to_string, unique_path,
    write_text_atomic,
};
use std::collections::{HashMap, VecDeque};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, ErrorKind};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mov", "m4v", "avi", "mkv", "wmv", "asf", "mpg", "mpeg", "mts", "m2ts", "ts", "3gp",
    "webm", "flv", "dav", "dav_", "nov", "ave", "g64", "g64x", "glv", "blk", "264", "265", "h264",
    "h265", "hevc",
];

/// Called with `(processed, total)` file counts; `None` disables mid-run
/// progress reporting (tests and headless callers). `total` is only known
/// after candidate collection, so it rides along on every tick.
pub type ScanProgress<'a> = dyn Fn(u64, u64) + Send + Sync + 'a;

pub fn scan_folder(
    case_dir: &Path,
    source_dir: &Path,
    options: &ScanOptions,
    resume: ResumeMode,
    progress: Option<&ScanProgress<'_>>,
) -> Result<ScanResult, String> {
    let source_dir = canonicalize_display(source_dir)
        .map_err(|err| format!("failed to canonicalize source: {err}"))?;
    let excluded_dirs = excluded_case_dirs(case_dir, &source_dir)?;
    let collection = collect_video_candidates(&source_dir, options.max_depth, &excluded_dirs)?;
    // An interrupted run checkpoints one record line per processed file
    // under db/scan-progress.jsonl; when the input fingerprint (source +
    // options + candidate list) still matches, those records are replayed
    // verbatim instead of re-hashing and re-probing gigabytes of evidence.
    let mut checkpoint = RunCheckpoint::begin(
        &case_dir.join("db/scan-progress.jsonl"),
        "scan-folder",
        &scan_fingerprint(&source_dir, options, &collection.files),
        resume,
    )?;
    let resumed_records = load_checkpoint_records(&checkpoint)?;
    let mut id_registry = load_existing_video_ids(case_dir)?;
    // The interrupted run already consumed these ids — re-registering them
    // keeps replayed records stable and stops fresh files from colliding.
    for record in resumed_records.values() {
        let key = normalize_source_key(&record.source_path.to_string_lossy());
        id_registry.ids_by_source.insert(key, record.id.clone());
        if let Some(number) = record.id.strip_prefix("vid_").and_then(parse_usize) {
            id_registry.next_number = id_registry.next_number.max(number + 1);
        }
    }
    // Incremental mode: the existing index is the skip oracle. A record is
    // reused verbatim (never re-hashed or re-probed) when its stored
    // size+mtime still match the file on disk; merge_existing_with_scan then
    // carries the untouched record forward and marks missing paths stale.
    let incremental_index: HashMap<String, IndexedRecordLine> = if options.incremental {
        load_existing_record_lines(case_dir)?
            .into_iter()
            .map(|line| (normalize_source_key(&line.source_path), line))
            .collect()
    } else {
        HashMap::new()
    };
    let total_files = collection.files.len() as u64;
    let mut records = Vec::with_capacity(collection.files.len());
    let mut unchanged_files = 0usize;
    let mut resumed_from_checkpoint = 0usize;
    let mut processed_files = 0u64;
    let mut total_bytes = 0u64;
    let mut warnings = collection.warnings;
    if checkpoint.reset_stale() {
        warnings.push(
            "previous scan checkpoint was recorded for different inputs; started fresh".to_string(),
        );
    }

    for path in collection.files {
        processed_files += 1;
        // Report every 64 files (and once at the start) — frequent enough
        // for a live progress bar, rare enough to keep SQLite writes off
        // the hot path on 10k-file scans.
        if let Some(report) = progress
            && (processed_files % 64 == 1 || processed_files == total_files)
        {
            report(processed_files, total_files);
        }
        // One unreadable file (bad sector, permission, vanished) must not
        // abort the whole scan and leave no index: record it and move on.
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(err) => {
                // An already-indexed file whose metadata cannot be read is
                // still present: merge_existing_with_scan carries its record
                // forward unchanged rather than marking it stale.
                let key = normalize_source_key(&path.to_string_lossy());
                if id_registry.ids_by_source.contains_key(&key) {
                    warnings.push(format!(
                        "metadata-error: {} ({err}); existing index record carried forward unchanged",
                        path.display()
                    ));
                } else {
                    warnings.push(format!(
                        "skipped unreadable file {} (metadata error: {err}); not indexed",
                        path.display()
                    ));
                }
                continue;
            }
        };
        total_bytes = total_bytes.saturating_add(metadata.len());
        let modified_unix = modified_unix(&metadata);
        let source_key = normalize_source_key(&path.to_string_lossy());
        // Replay the interrupted run's record verbatim — but only while the
        // file's size+mtime still match it, so a file changed between the
        // crash and the resume is reprocessed rather than trusted stale.
        if let Some(existing) = resumed_records.get(&source_key)
            && existing.size_bytes == metadata.len()
            && existing.modified_unix == modified_unix
        {
            resumed_from_checkpoint += 1;
            records.push(existing.clone());
            continue;
        }
        if let Some(existing) = incremental_index.get(&source_key)
            && record_is_unchanged(existing, metadata.len(), modified_unix, options)
        {
            unchanged_files += 1;
            continue;
        }
        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let relative_path = path
            .strip_prefix(&source_dir)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();

        let (sha256, hash_status) = if options.hash_files {
            let (sha256, status, warning) = hash_with_restat(&path, &metadata);
            if let Some(warning) = warning {
                warnings.push(warning);
            }
            (sha256, status.to_string())
        } else {
            (None, "skipped".to_string())
        };

        let probe = if options.use_ffprobe {
            ffprobe::probe(&path)
        } else {
            ProbeSummary::skipped()
        };

        let confidence = classify_confidence(&extension, &probe);
        let source_profile = detector::detect_source_profile(
            &relative_path,
            &extension,
            probe.format_name.as_deref(),
        );
        let id = id_registry.id_for(&path);
        let record = VideoRecord {
            id,
            source_path: path,
            relative_path,
            extension,
            size_bytes: metadata.len(),
            modified_unix,
            sha256,
            hash_status,
            probe,
            confidence,
            source_profile,
        };
        // Checkpoint BEFORE the record goes into the in-memory list: a
        // crash after this line replays the record, a crash before it
        // simply reprocesses the file.
        checkpoint.append_line(&record.to_json())?;
        records.push(record);
    }

    let result = ScanResult {
        source_path: source_dir,
        scanned_unix: now_unix()?,
        video_count: records.len(),
        total_bytes,
        unchanged_files,
        resumed_from_checkpoint,
        warnings,
        options: options.clone(),
        records,
    };

    write_scan_outputs(case_dir, &result)?;
    if resumed_from_checkpoint > 0 {
        let line = format!(
            "{{\"schema_version\":1,\"event\":\"scan-resume\",\"run_id\":\"{}\",\"source_path\":\"{}\",\"resumed_records\":{}}}",
            json_escape(checkpoint.run_id()),
            json_escape(&result.source_path.to_string_lossy()),
            resumed_from_checkpoint
        );
        crate::audit::append_chained_jsonl(&case_dir.join("evidence/logs/scan-log.jsonl"), &line)?;
    }
    checkpoint.finish()?;
    Ok(result)
}

/// Fingerprint of everything that decides a scan record: source path, the
/// option set, and the collected candidate list. A rerun against different
/// inputs resets the checkpoint instead of silently skipping fresh work.
fn scan_fingerprint(source_dir: &Path, options: &ScanOptions, files: &[PathBuf]) -> String {
    let mut parts = vec![
        "scan-folder".to_string(),
        source_dir.to_string_lossy().to_string(),
        format!("hash_files={}", options.hash_files),
        format!("use_ffprobe={}", options.use_ffprobe),
        format!("max_depth={:?}", options.max_depth),
        format!("incremental={}", options.incremental),
    ];
    parts.extend(files.iter().map(|path| path.to_string_lossy().to_string()));
    checkpoint::fingerprint(&parts.iter().map(String::as_str).collect::<Vec<_>>())
}

/// Decodes a resumed checkpoint's record lines (verbatim `videos.jsonl`
/// records) into the replay map keyed by normalized source path. Later
/// lines win so a reprocessed file's newest record is the one replayed.
fn load_checkpoint_records(
    checkpoint: &RunCheckpoint,
) -> Result<HashMap<String, VideoRecord>, String> {
    let mut records = HashMap::new();
    for line in checkpoint.data_lines() {
        let record: VideoRecord = serde_json::from_str(line).map_err(|err| {
            format!(
                "checkpoint {} holds a line that is not a video record ({err}); delete it or rerun with --no-resume",
                checkpoint.path().display()
            )
        })?;
        records.insert(
            normalize_source_key(&record.source_path.to_string_lossy()),
            record,
        );
    }
    Ok(records)
}

struct CandidateCollection {
    files: Vec<PathBuf>,
    warnings: Vec<String>,
}

fn collect_video_candidates(
    source_dir: &Path,
    max_depth: Option<usize>,
    excluded_dirs: &[PathBuf],
) -> Result<CandidateCollection, String> {
    let mut out = Vec::new();
    let mut warnings = Vec::new();
    let mut queue = VecDeque::from([(source_dir.to_path_buf(), 0usize)]);

    while let Some((dir, depth)) = queue.pop_front() {
        if max_depth.is_some_and(|max| depth > max) {
            continue;
        }
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if depth == 0 => {
                return Err(format!("failed to read directory {}: {err}", dir.display()));
            }
            Err(err) => {
                warnings.push(format!(
                    "skipped unreadable directory {}: {err}",
                    dir.display()
                ));
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    warnings.push(format!("skipped unreadable directory entry: {err}"));
                    continue;
                }
            };
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(err) => {
                    warnings.push(format!(
                        "skipped unreadable file type {}: {err}",
                        path.display()
                    ));
                    continue;
                }
            };
            if file_type.is_dir() {
                if path_is_or_is_under_excluded_dir(&path, excluded_dirs) {
                    warnings.push(format!(
                        "skipped FrameTrace case output directory {}",
                        path.display()
                    ));
                    continue;
                }
                queue.push_back((path, depth + 1));
            } else if file_type.is_file() && looks_like_video(&path) {
                out.push(path);
            }
        }
    }

    out.sort();
    Ok(CandidateCollection {
        files: out,
        warnings,
    })
}

fn excluded_case_dirs(case_dir: &Path, source_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let case_dir = case_dir
        .canonicalize()
        .unwrap_or_else(|_| case_dir.to_path_buf());
    if case_dir == source_dir {
        return Err(format!(
            "source directory cannot be the FrameTrace case directory: {}",
            source_dir.display()
        ));
    }
    if case_dir.starts_with(source_dir) {
        Ok(vec![case_dir])
    } else {
        Ok(Vec::new())
    }
}

fn path_is_or_is_under_excluded_dir(path: &Path, excluded_dirs: &[PathBuf]) -> bool {
    excluded_dirs
        .iter()
        .any(|excluded| path == excluded || path.starts_with(excluded))
}

fn looks_like_video(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|ext| ext.to_str()) else {
        return has_video_magic(path);
    };
    VIDEO_EXTENSIONS
        .iter()
        .any(|known| extension.eq_ignore_ascii_case(known))
        || has_video_magic(path)
}

fn has_video_magic(path: &Path) -> bool {
    let file = File::open(path);
    let Ok(file) = file else {
        return false;
    };
    let mut reader = BufReader::new(file);
    let buffer = reader.fill_buf().unwrap_or(&[]);
    if buffer.len() < 12 {
        return false;
    }

    // `ftyp` must sit in an MP4 box header at the file start (4-byte size then
    // `ftyp`). Scanning the whole buffer would misclassify any file whose text
    // merely contains "ftyp", such as our own JSONL audit logs.
    buffer.len() >= 8 && &buffer[4..8] == b"ftyp"
        || buffer.starts_with(&[0x00, 0x00, 0x00, 0x01])
        || buffer.starts_with(&[0x00, 0x00, 0x01])
        || buffer.starts_with(b"RIFF")
        || buffer.starts_with(b"IMKH")
        || buffer.starts_with(b"DHAV")
}

/// Hashes one file and re-stats it afterwards. Returns
/// `(sha256, hash_status, warning)`:
/// - `complete` — hashed, and size+mtime unchanged across the read;
/// - `error` — open/read failed (the file stays indexed without a hash);
/// - `changed-during-hash` — size or mtime moved while hashing, so the
///   digest may describe neither version and is not recorded.
fn hash_with_restat(
    path: &Path,
    before: &fs::Metadata,
) -> (Option<String>, &'static str, Option<String>) {
    let digest = File::open(path)
        .map_err(|err| format!("open failed: {err}"))
        .and_then(|file| {
            sha256::digest_reader(BufReader::new(file)).map_err(|err| format!("read failed: {err}"))
        });
    let digest = match digest {
        Ok(digest) => digest,
        Err(err) => {
            return (
                None,
                "error",
                Some(format!(
                    "hash error for {} ({err}); indexed with hash_status=error",
                    path.display()
                )),
            );
        }
    };
    let changed = match fs::metadata(path) {
        Ok(after) => after.len() != before.len() || after.modified().ok() != before.modified().ok(),
        Err(_) => true,
    };
    if changed {
        return (
            None,
            "changed-during-hash",
            Some(format!(
                "{} changed while it was being hashed; hash not recorded (hash_status=changed-during-hash)",
                path.display()
            )),
        );
    }
    (Some(digest), "complete", None)
}

fn modified_unix(metadata: &fs::Metadata) -> Option<u64> {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
}

/// Whether an incremental rescan may reuse an indexed record verbatim.
/// The oracle is size+mtime equality (cheap, metadata-only); the record must
/// also be unflagged (a stale or otherwise marked record is reprocessed so
/// the marker can clear) and already carry every datum this run's options
/// would produce — `--hash` against an unhashed record, or ffprobe against a
/// never-probed one, reprocesses the file instead of skipping it.
fn record_is_unchanged(
    existing: &IndexedRecordLine,
    size_bytes: u64,
    modified_unix: Option<u64>,
    options: &ScanOptions,
) -> bool {
    // Substring check, not a depth-aware key lookup: a planted nested
    // `index_status` tag inside ffprobe JSON would only force a reprocess,
    // which is the safe direction.
    if existing.json_line.contains("\"index_status\"") {
        return false;
    }
    if existing.record.size_bytes != size_bytes || existing.record.modified_unix != modified_unix {
        return false;
    }
    if options.hash_files && existing.record.sha256.is_none() {
        return false;
    }
    if options.use_ffprobe && existing.record.probe.error.as_deref() == Some("ffprobe skipped") {
        return false;
    }
    true
}

fn classify_confidence(extension: &str, probe: &ProbeSummary) -> String {
    if probe.ok {
        "ffprobe-confirmed".to_string()
    } else if VIDEO_EXTENSIONS.contains(&extension) {
        "extension-candidate".to_string()
    } else {
        "magic-candidate".to_string()
    }
}

fn write_scan_outputs(case_dir: &Path, result: &ScanResult) -> Result<(), String> {
    let run_path = case_dir
        .join("db/scan_runs")
        .join(format!("scan_{}.json", result.scanned_unix));
    let run_path = unique_path(&run_path);
    write_text_atomic(&run_path, &result.to_json())
        .map_err(|err| format!("failed to write scan run snapshot: {err}"))?;

    let merged_records = merge_existing_with_scan(case_dir, result)?;
    let index_json = scan_index_json(result, &merged_records);
    write_text_atomic(&case_dir.join("db/video_index.json"), &index_json)
        .map_err(|err| format!("failed to write video index: {err}"))?;

    let mut jsonl = String::new();
    let mut paths_tsv = String::from(
        "id\tsource_path\trelative_path\textension\tsize_bytes\tsha256\tvendor\tparser\tparser_confidence\n",
    );
    for record in &merged_records {
        jsonl.push_str(&record.json_line);
        jsonl.push('\n');
        paths_tsv.push_str(&record.to_tsv_row());
    }
    write_text_atomic(&case_dir.join("db/videos.jsonl"), &jsonl)
        .map_err(|err| format!("failed to write video jsonl: {err}"))?;
    write_text_atomic(&case_dir.join("db/video_paths.tsv"), &paths_tsv)
        .map_err(|err| format!("failed to write video path index: {err}"))?;

    let db_records = merged_records
        .iter()
        .map(IndexedRecordLine::to_db_row)
        .collect::<Vec<_>>();
    case_db::write_scan_index(case_dir, result, &db_records)?;
    Ok(())
}

#[derive(Debug, Clone)]
struct IdRegistry {
    ids_by_source: HashMap<String, String>,
    next_number: usize,
}

impl IdRegistry {
    fn id_for(&mut self, path: &Path) -> String {
        let source_path = normalize_source_key(&path.to_string_lossy());
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
fn normalize_source_key(source_path: &str) -> String {
    crate::util::strip_windows_extended_prefix(Path::new(source_path))
        .to_string_lossy()
        .to_string()
}

#[derive(Debug, Clone)]
struct IndexedRecordLine {
    id: String,
    source_path: String,
    /// Field order and spelling must stay identical to the published
    /// JSONL contract, so the serialized form is kept verbatim and the
    /// structured fields are derived through serde.
    json_line: String,
    record: VideoRecord,
}

impl IndexedRecordLine {
    fn from_record(record: &VideoRecord) -> Self {
        Self {
            id: record.id.clone(),
            source_path: record.source_path.to_string_lossy().to_string(),
            json_line: record.to_json(),
            record: record.clone(),
        }
    }

    fn to_tsv_row(&self) -> String {
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

    fn to_db_row(&self) -> IndexedVideoRow {
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

fn clone_or(value: &str, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

fn load_existing_video_ids(case_dir: &Path) -> Result<IdRegistry, String> {
    let existing = load_existing_record_lines(case_dir)?;
    let mut ids_by_source = HashMap::new();
    let mut max_number = 0usize;

    for record in case_db::load_video_ids(case_dir)? {
        if let Some(number) = record.id.strip_prefix("vid_").and_then(parse_usize) {
            max_number = max_number.max(number);
        }
        ids_by_source.insert(normalize_source_key(&record.source_path), record.id);
    }

    for record in existing {
        if let Some(number) = record.id.strip_prefix("vid_").and_then(parse_usize) {
            max_number = max_number.max(number);
        }
        ids_by_source.insert(normalize_source_key(&record.source_path), record.id);
    }

    Ok(IdRegistry {
        ids_by_source,
        next_number: max_number + 1,
    })
}

fn merge_existing_with_scan(
    case_dir: &Path,
    result: &ScanResult,
) -> Result<Vec<IndexedRecordLine>, String> {
    let current_by_source = result
        .records
        .iter()
        .map(IndexedRecordLine::from_record)
        .map(|record| (normalize_source_key(&record.source_path), record))
        .collect::<HashMap<_, _>>();

    let mut merged = Vec::new();
    let mut updated_keys = std::collections::HashSet::new();
    let mut stale_keys = std::collections::HashSet::new();
    for existing in load_existing_record_lines(case_dir)? {
        let key = normalize_source_key(&existing.source_path);
        if let Some(updated) = current_by_source.get(&key) {
            // Legacy and clean spellings of one file may both exist in old
            // indexes; converge to a single refreshed record.
            if updated_keys.insert(key.clone()) {
                merged.push(updated.clone());
            }
        } else if stale_keys.contains(&key) {
            // Duplicate spelling of an already-decided record; skip.
            continue;
        } else if source_presence(Path::new(&existing.source_path)) != SourcePresence::Missing {
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
        .map(|record| normalize_source_key(&record.source_path))
        .collect::<std::collections::HashSet<_>>();
    for record in result.records.iter().map(IndexedRecordLine::from_record) {
        if !existing_sources.contains(&normalize_source_key(&record.source_path)) {
            merged.push(record);
        }
    }

    merged.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(merged)
}

#[derive(Debug, PartialEq, Eq)]
enum SourcePresence {
    File,
    /// Definitely absent: not found, a path component is not a directory,
    /// or the path resolves to something other than a regular file.
    Missing,
    /// `fs::metadata` failed for a reason other than absence.
    MetadataError,
}

fn source_presence(path: &Path) -> SourcePresence {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => SourcePresence::File,
        Ok(_) => SourcePresence::Missing,
        Err(err) if matches!(err.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            SourcePresence::Missing
        }
        Err(_) => SourcePresence::MetadataError,
    }
}

fn load_existing_record_lines(case_dir: &Path) -> Result<Vec<IndexedRecordLine>, String> {
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
    fn mark_stale(mut self, stale_since_unix: u64) -> Self {
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

/// Appends or replaces one top-level scalar field on a serialized JSON object
/// through raw text edits only, so existing byte-stable records keep their
/// field order and spelling.
///
/// `#[doc(hidden)]`: exposed so the fuzz harness can hit this hand-rolled
/// editor with arbitrary input; not part of the supported API.
#[doc(hidden)]
pub fn set_json_field(line: &str, key: &str, value: &str) -> String {
    if find_top_level_key(line, key).is_some() {
        replace_json_field(line, key, value)
    } else {
        insert_json_field(line, key, value)
    }
}

/// Byte range of `"key":<value>` at JSON depth 1 (top-level object members
/// only). Nested occurrences — e.g. a metadata tag named `index_status`
/// inside the inlined ffprobe object — are deliberately not matched, so
/// stale markers can never rewrite recorded probe evidence.
fn find_top_level_key(line: &str, key: &str) -> Option<std::ops::Range<usize>> {
    let bytes = line.as_bytes();
    let needle = format!("\"{key}\":");
    let needle = needle.as_bytes();
    let mut depth: usize = 0;
    let mut in_string = false;
    let mut escaped = false;
    let mut index = 0usize;

    while index < bytes.len() {
        let ch = bytes[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == b'\\' {
                escaped = true;
            } else if ch == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        match ch {
            b'"' => in_string = true,
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
        // A member key starts at depth 1 right after a `{` or `,`.
        if depth == 1
            && (bytes[..=index].ends_with(b"{") || bytes[..=index].ends_with(b","))
            && bytes[index + 1..].starts_with(needle)
        {
            let value_start = index + 1 + needle.len();
            let value = extract_scalar_value(&line[value_start..])?;
            let range = value_start..value_start + value.len();
            return Some(range);
        }
        index += 1;
    }
    None
}

/// Consumes one scalar JSON value (string, number, bool, null) from the
/// start of `text` and returns its raw serialized form.
fn extract_scalar_value(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    let offset = text.len() - trimmed.len();
    let mut chars = trimmed.chars();
    match chars.next()? {
        '"' => {
            let mut out = String::from("\"");
            let mut escaped = false;
            for ch in trimmed[1..].chars() {
                out.push(ch);
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    return Some(format!("{}{}", &text[..offset], out));
                }
            }
            None
        }
        first if first == '-' || first.is_ascii_digit() => {
            let raw: String = std::iter::once(first)
                .chain(chars.take_while(|ch| {
                    ch.is_ascii_digit() || matches!(ch, '.' | 'e' | 'E' | '+' | '-')
                }))
                .collect();
            (!raw.is_empty()).then(|| format!("{}{}", &text[..offset], raw))
        }
        't' if trimmed.starts_with("true") => Some(format!("{}true", &text[..offset])),
        'f' if trimmed.starts_with("false") => Some(format!("{}false", &text[..offset])),
        'n' if trimmed.starts_with("null") => Some(format!("{}null", &text[..offset])),
        _ => None,
    }
}

fn replace_json_field(line: &str, key: &str, value: &str) -> String {
    let Some(range) = find_top_level_key(line, key) else {
        return insert_json_field(line, key, value);
    };
    format!("{}{}{}", &line[..range.start], value, &line[range.end..])
}

fn insert_json_field(line: &str, key: &str, value: &str) -> String {
    let trimmed = line.trim_end();
    let Some(close_index) = trimmed.rfind('}') else {
        return line.to_string();
    };
    let prefix = &trimmed[..close_index];
    let suffix = &trimmed[close_index..];
    let separator = if prefix.trim_end().ends_with('{') {
        ""
    } else {
        ","
    };
    format!("{prefix}{separator}\"{key}\":{value}{suffix}")
}

/// Splits index text into top-level JSON object records. The splitter is
/// hand-rolled (depth + string/escape tracking), so it is exposed for the
/// fuzz harness; not part of the supported API.
#[doc(hidden)]
pub fn json_record_lines(text: &str) -> Vec<String> {
    let mut records = Vec::new();
    let mut current = String::new();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for ch in text.chars() {
        if depth > 0 {
            current.push(ch);
        } else if ch.is_whitespace() {
            continue;
        } else {
            current.push(ch);
        }

        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 && !current.trim().is_empty() {
                    records.push(current.trim().to_string());
                    current.clear();
                }
            }
            _ => {}
        }
    }

    if !current.trim().is_empty() {
        records.push(current.trim().to_string());
    }
    records
}

fn scan_index_json(result: &ScanResult, records: &[IndexedRecordLine]) -> String {
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

fn parse_usize(value: &str) -> Option<usize> {
    value.parse::<usize>().ok()
}

/// Escapes a value for the tab-separated `db/video_paths.tsv` format.
/// `pub(crate)` so case_merge can emit rows in the same shape.
pub(crate) fn tsv_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    #[test]
    fn hash_with_restat_records_error_instead_of_aborting() {
        let dir = std::env::temp_dir().join(format!(
            "frametrace-scan-hash-restat-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.mp4");
        std::fs::write(&path, b"abc").unwrap();
        let metadata = std::fs::metadata(&path).unwrap();

        let (sha, status, warning) = super::hash_with_restat(&path, &metadata);
        assert_eq!(status, "complete");
        assert_eq!(
            sha.as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        assert!(warning.is_none());

        // Grown after the "before" stat: the digest is not trusted.
        std::fs::write(&path, b"abcd").unwrap();
        let (sha, status, warning) = super::hash_with_restat(&path, &metadata);
        assert_eq!(status, "changed-during-hash");
        assert!(sha.is_none());
        assert!(warning.is_some());

        // Unreadable (vanished) file: recorded as an error, no panic/abort.
        std::fs::remove_file(&path).unwrap();
        let (sha, status, warning) = super::hash_with_restat(&path, &metadata);
        assert_eq!(status, "error");
        assert!(sha.is_none());
        assert!(warning.unwrap().contains("hash_status=error"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    use super::{
        ResumeMode, RunCheckpoint, collect_video_candidates, excluded_case_dirs, looks_like_video,
        merge_existing_with_scan, scan_fingerprint, set_json_field,
    };
    use crate::model::{ProbeSummary, ScanOptions, ScanResult, SourceProfile, VideoRecord};
    use std::fs;
    use std::path::{Path, PathBuf};

    #[test]
    fn set_json_field_appends_without_reordering_existing_keys() {
        // The JSONL contract is byte-stable for already-indexed evidence:
        // stale markers must not rewrite the whole object.
        let line = r#"{"z_last":1,"a_first":"x"}"#;
        let updated = set_json_field(line, "index_status", "\"stale\"");
        let expected = r#"{"z_last":1,"a_first":"x","index_status":"stale"}"#;
        assert_eq!(updated, expected);
    }

    #[test]
    fn set_json_field_replaces_existing_scalar_in_place() {
        let line = r#"{"id":"vid_1","stale_since_unix":5,"ext":"mp4"}"#;
        let updated = set_json_field(line, "stale_since_unix", "9");
        assert_eq!(
            updated,
            r#"{"id":"vid_1","stale_since_unix":9,"ext":"mp4"}"#
        );
    }

    /// A nested key (e.g. an ffprobe metadata tag) sharing the stale
    /// marker's name must never be touched: only depth-1 members match.
    /// Evidence files can carry tags named `index_status` planted via
    /// `ffmpeg -metadata`, so this is an adversarial-input guard.
    #[test]
    fn set_json_field_ignores_nested_keys_with_the_same_name() {
        let line = r#"{"id":"vid_1","ffprobe":{"index_status":"attacker tag","streams":[]},"index_status":null,"ext":"mp4"}"#;
        let updated = set_json_field(line, "index_status", "\"stale\"");
        assert!(
            updated.contains(r#""ffprobe":{"index_status":"attacker tag""#),
            "nested tag must stay byte-identical: {updated}"
        );
        assert!(updated.contains(r#""index_status":"stale""#), "{updated}");
    }

    #[test]
    fn set_json_field_appends_when_top_level_key_missing() {
        let line = r#"{"id":"vid_1","ffprobe":{"index_status":"nested decoy"}}"#;
        let updated = set_json_field(line, "stale_since_unix", "7");
        assert!(updated.ends_with(r#","stale_since_unix":7}"#), "{updated}");
        assert!(updated.contains(r#""ffprobe":{"index_status":"nested decoy"}"#));
    }

    #[test]
    fn recognizes_common_video_extensions() {
        assert!(looks_like_video(&PathBuf::from("a.MP4")));
        assert!(looks_like_video(&PathBuf::from("camera.dav")));
        assert!(looks_like_video(&PathBuf::from("export.g64x")));
        assert!(looks_like_video(&PathBuf::from("phone.glv")));
        assert!(!looks_like_video(&PathBuf::from("notes.txt")));
    }

    #[test]
    fn recognizes_mp4_magic_without_extension() {
        let path = std::env::temp_dir().join(format!(
            "forensic-video-workstation-test-{}",
            std::process::id()
        ));
        fs::write(&path, b"\0\0\0\x18ftypmp42").unwrap();
        assert!(looks_like_video(&path));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_files_that_mention_ftyp_in_text() {
        let path = std::env::temp_dir().join(format!(
            "frametrace-false-magic-test-{}.jsonl",
            std::process::id()
        ));
        fs::write(
            &path,
            r#"{"signature":"mp4-ftyp","note":"audit text mentioning ftyp boxes"}"#,
        )
        .unwrap();
        assert!(!looks_like_video(&path));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_scanning_the_case_directory_as_source() {
        let root = std::env::temp_dir().join(format!(
            "frametrace-case-source-reject-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let canonical = root.canonicalize().unwrap();
        let err = excluded_case_dirs(&canonical, &canonical).unwrap_err();
        assert!(err.contains("source directory cannot be the FrameTrace case directory"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn collect_candidates_skips_excluded_case_output_tree() {
        let root = std::env::temp_dir().join(format!(
            "frametrace-scan-exclusion-test-{}",
            std::process::id()
        ));
        let source_dir = root.join("source");
        let case_dir = source_dir.join("case");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(source_dir.join("camera")).unwrap();
        fs::create_dir_all(case_dir.join("artifacts/clips")).unwrap();
        fs::write(source_dir.join("camera/real.mp4"), b"not actually media").unwrap();
        fs::write(
            case_dir.join("artifacts/clips/derived.mp4"),
            b"not actually media",
        )
        .unwrap();

        let collection =
            collect_video_candidates(&source_dir, None, std::slice::from_ref(&case_dir)).unwrap();
        let names = collection
            .files
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["real.mp4"]);
        assert!(
            collection
                .warnings
                .iter()
                .any(|warning| warning.contains("skipped FrameTrace case output directory"))
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn deserializes_published_jsonl_records_through_serde() {
        let line = r#"{"id":"vid_000001","source_path":"C:\\Evidence\\a\tb.mp4","relative_path":"a\tb.mp4","extension":"mp4","size_bytes":1,"modified_unix":null,"sha256":null,"hash_status":"skipped","confidence":"extension-candidate","source_profile":{"lane":"generic-video","vendor":"Generic media","parser":"generic_media","confidence":"medium","recommended_action":"Use ffprobe/FFmpeg first; preserve original and export derived clips only when requested.","evidence":["extension"]},"duration_seconds":12.5,"format_name":"mov,mp4,m4a","video_codec":"h264","audio_codec":null,"width":1920,"height":1080,"ffprobe_ok":true,"ffprobe_error":null,"ffprobe":null}"#;
        let record: VideoRecord = serde_json::from_str(line).unwrap();
        assert_eq!(record.id, "vid_000001");
        assert_eq!(record.source_path, PathBuf::from("C:\\Evidence\\a\tb.mp4"));
        assert!(record.probe.ok);
        assert_eq!(record.probe.width, Some(1920));
        assert_eq!(record.probe.duration_seconds, Some(12.5));

        // Round-trip keeps the published contract parseable.
        let reparsed: VideoRecord = serde_json::from_str(&record.to_json()).unwrap();
        assert_eq!(reparsed.id, record.id);
        assert_eq!(reparsed.probe.width, record.probe.width);
    }

    /// An indexed file that still exists but whose metadata cannot be read
    /// (here: a self-referential symlink, ELOOP) must be carried forward
    /// unchanged, not marked stale; a genuinely missing one is still stale.
    #[cfg(unix)]
    #[test]
    fn metadata_error_carries_record_forward_instead_of_stale() {
        let case_dir = std::env::temp_dir().join(format!(
            "frametrace-metadata-error-merge-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&case_dir);
        fs::create_dir_all(case_dir.join("db")).unwrap();
        let looped = case_dir.join("loop.mp4");
        std::os::unix::fs::symlink(&looped, &looped).unwrap();
        assert!(fs::metadata(&looped).is_err());
        assert_eq!(
            super::source_presence(&looped),
            super::SourcePresence::MetadataError
        );
        let unreadable = VideoRecord {
            id: "vid_000001".to_string(),
            source_path: looped.clone(),
            relative_path: "loop.mp4".to_string(),
            extension: "mp4".to_string(),
            size_bytes: 1,
            modified_unix: None,
            sha256: None,
            hash_status: "skipped".to_string(),
            probe: ProbeSummary::skipped(),
            confidence: "extension-candidate".to_string(),
            source_profile: SourceProfile::generic_media("test"),
        };
        let gone = VideoRecord {
            id: "vid_000002".to_string(),
            source_path: case_dir.join("gone.mp4"),
            ..unreadable.clone()
        };
        let original_line = unreadable.to_json();
        fs::write(
            case_dir.join("db/videos.jsonl"),
            format!("{}\n{}\n", original_line, gone.to_json()),
        )
        .unwrap();
        let result = ScanResult {
            source_path: case_dir.clone(),
            scanned_unix: 5,
            video_count: 0,
            total_bytes: 0,
            unchanged_files: 0,
            resumed_from_checkpoint: 0,
            warnings: Vec::new(),
            options: ScanOptions::default(),
            records: Vec::new(),
        };
        let merged = merge_existing_with_scan(&case_dir, &result).unwrap();
        assert_eq!(merged.len(), 2, "{merged:?}");
        assert_eq!(merged[0].json_line, original_line.trim());
        assert!(!merged[0].json_line.contains("stale"));
        assert!(merged[1].json_line.contains(r#""index_status":"stale""#));
        let _ = fs::remove_dir_all(case_dir);
    }

    /// A file that vanished while indexed under BOTH the legacy `\\?\` and
    /// clean spellings must converge to one stale record, not two.
    #[test]
    fn stale_marking_dedupes_legacy_and_clean_spellings() {
        let case_dir = std::env::temp_dir().join(format!(
            "frametrace-stale-dedupe-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&case_dir);
        fs::create_dir_all(case_dir.join("db")).unwrap();

        let legacy = VideoRecord {
            id: "vid_000001".to_string(),
            source_path: PathBuf::from(r"\\?\C:\gone\one.mp4"),
            relative_path: "one.mp4".to_string(),
            extension: "mp4".to_string(),
            size_bytes: 1,
            modified_unix: None,
            sha256: None,
            hash_status: "skipped".to_string(),
            probe: ProbeSummary::skipped(),
            confidence: "extension-candidate".to_string(),
            source_profile: SourceProfile::generic_media("test"),
        };
        let clean = VideoRecord {
            id: "vid_000002".to_string(),
            source_path: PathBuf::from(r"C:\gone\one.mp4"),
            ..legacy.clone()
        };
        fs::write(
            case_dir.join("db/videos.jsonl"),
            format!("{}\n{}\n", legacy.to_json(), clean.to_json()),
        )
        .unwrap();

        let result = ScanResult {
            source_path: PathBuf::from(r"C:\elsewhere"),
            scanned_unix: 5,
            video_count: 0,
            total_bytes: 0,
            unchanged_files: 0,
            resumed_from_checkpoint: 0,
            warnings: Vec::new(),
            options: ScanOptions::default(),
            records: Vec::new(),
        };

        let merged = merge_existing_with_scan(&case_dir, &result).unwrap();
        assert_eq!(
            merged.len(),
            1,
            "duplicate spellings must converge: {merged:?}"
        );
        assert!(merged[0].json_line.contains(r#""index_status":"stale""#));
        let _ = fs::remove_dir_all(case_dir);
    }

    #[test]
    fn merges_scan_records_without_dropping_existing_case_index() {
        let case_dir =
            std::env::temp_dir().join(format!("frametrace-merge-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&case_dir);
        fs::create_dir_all(case_dir.join("db")).unwrap();

        let first = VideoRecord {
            id: "vid_000001".to_string(),
            source_path: PathBuf::from("/evidence/one.mp4"),
            relative_path: "one.mp4".to_string(),
            extension: "mp4".to_string(),
            size_bytes: 1,
            modified_unix: None,
            sha256: None,
            hash_status: "skipped".to_string(),
            probe: ProbeSummary::skipped(),
            confidence: "extension-candidate".to_string(),
            source_profile: SourceProfile::generic_media("test"),
        };
        let second = VideoRecord {
            id: "vid_000002".to_string(),
            source_path: PathBuf::from("/evidence/two.mp4"),
            relative_path: "two.mp4".to_string(),
            extension: "mp4".to_string(),
            size_bytes: 2,
            modified_unix: None,
            sha256: None,
            hash_status: "skipped".to_string(),
            probe: ProbeSummary::skipped(),
            confidence: "extension-candidate".to_string(),
            source_profile: SourceProfile::generic_media("test"),
        };
        fs::write(
            case_dir.join("db/videos.jsonl"),
            format!("{}\n{}\n", first.to_json(), second.to_json()),
        )
        .unwrap();

        let rescanned_second = VideoRecord {
            sha256: Some("abc".to_string()),
            hash_status: "complete".to_string(),
            ..second
        };
        let result = ScanResult {
            source_path: PathBuf::from("/evidence"),
            scanned_unix: 1,
            video_count: 1,
            total_bytes: 2,
            unchanged_files: 0,
            resumed_from_checkpoint: 0,
            warnings: Vec::new(),
            options: ScanOptions::default(),
            records: vec![rescanned_second],
        };

        let merged = merge_existing_with_scan(&case_dir, &result).unwrap();
        assert_eq!(merged.len(), 2);
        assert!(merged[0].json_line.contains("\"id\":\"vid_000001\""));
        assert!(merged[0].json_line.contains("\"index_status\":\"stale\""));
        assert!(merged[0].json_line.contains("\"stale_since_unix\":1"));
        assert!(merged[1].json_line.contains("\"id\":\"vid_000002\""));
        assert!(merged[1].json_line.contains("\"sha256\":\"abc\""));

        let _ = fs::remove_dir_all(case_dir);
    }

    #[test]
    fn upgrades_legacy_extended_prefix_paths_without_duplicating_ids() {
        let case_dir = std::env::temp_dir().join(format!(
            "frametrace-legacy-path-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&case_dir);
        fs::create_dir_all(case_dir.join("db")).unwrap();

        let legacy = VideoRecord {
            id: "vid_000001".to_string(),
            source_path: PathBuf::from(r"\\?\C:\evidence\one.mp4"),
            relative_path: "one.mp4".to_string(),
            extension: "mp4".to_string(),
            size_bytes: 1,
            modified_unix: None,
            sha256: None,
            hash_status: "skipped".to_string(),
            probe: ProbeSummary::skipped(),
            confidence: "extension-candidate".to_string(),
            source_profile: SourceProfile::generic_media("test"),
        };
        fs::write(
            case_dir.join("db/videos.jsonl"),
            format!("{}\n", legacy.to_json()),
        )
        .unwrap();

        let rescanned = VideoRecord {
            source_path: PathBuf::from(r"C:\evidence\one.mp4"),
            sha256: Some("abc".to_string()),
            hash_status: "complete".to_string(),
            ..legacy
        };
        let result = ScanResult {
            source_path: PathBuf::from(r"C:\evidence"),
            scanned_unix: 2,
            video_count: 1,
            total_bytes: 1,
            unchanged_files: 0,
            resumed_from_checkpoint: 0,
            warnings: Vec::new(),
            options: ScanOptions::default(),
            records: vec![rescanned],
        };

        let merged = merge_existing_with_scan(&case_dir, &result).unwrap();
        assert_eq!(merged.len(), 1, "same file must not duplicate records");
        assert!(merged[0].json_line.contains("\"id\":\"vid_000001\""));
        assert!(merged[0].json_line.contains(r"C:\\evidence\\one.mp4"));
        assert!(!merged[0].json_line.contains("index_status"));

        let _ = fs::remove_dir_all(case_dir);
    }

    fn incremental_fixture(name: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "frametrace-incremental-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let case_dir = root.join("case");
        let source_dir = root.join("source");
        fs::create_dir_all(case_dir.join("db")).unwrap();
        fs::create_dir_all(&source_dir).unwrap();
        (case_dir, source_dir)
    }

    fn read_jsonl_ids_with_hash(case_dir: &std::path::Path) -> Vec<(String, bool)> {
        let text = fs::read_to_string(case_dir.join("db/videos.jsonl")).unwrap();
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let value: serde_json::Value = serde_json::from_str(line).unwrap();
                (
                    value["id"].as_str().unwrap().to_string(),
                    value["sha256"].is_string(),
                )
            })
            .collect()
    }

    /// Unchanged files (same size+mtime) are reused verbatim: no re-hash,
    /// no re-probe, and the stored JSONL line survives byte-identical.
    #[test]
    fn incremental_rescan_skips_unchanged_files() {
        let (case_dir, source_dir) = incremental_fixture("skip");
        fs::write(source_dir.join("a.mp4"), b"\0\0\0\x18ftypmp42one").unwrap();
        fs::write(source_dir.join("b.mp4"), b"\0\0\0\x18ftypmp42two").unwrap();
        let options = ScanOptions {
            hash_files: false,
            use_ffprobe: false,
            ..ScanOptions::default()
        };
        let first =
            super::scan_folder(&case_dir, &source_dir, &options, ResumeMode::Auto, None).unwrap();
        assert_eq!(first.video_count, 2);
        let before = fs::read_to_string(case_dir.join("db/videos.jsonl")).unwrap();

        let rescan = super::scan_folder(
            &case_dir,
            &source_dir,
            &ScanOptions {
                incremental: true,
                ..options.clone()
            },
            ResumeMode::Auto,
            None,
        )
        .unwrap();
        assert_eq!(rescan.video_count, 0, "nothing was re-processed");
        assert_eq!(rescan.unchanged_files, 2);
        let after = fs::read_to_string(case_dir.join("db/videos.jsonl")).unwrap();
        assert_eq!(before, after, "unchanged records must stay verbatim");

        // ...but --hash against never-hashed records must NOT skip them.
        let hashed = super::scan_folder(
            &case_dir,
            &source_dir,
            &ScanOptions {
                hash_files: true,
                incremental: true,
                ..ScanOptions::default()
            },
            ResumeMode::Auto,
            None,
        )
        .unwrap();
        assert_eq!(hashed.video_count, 2);
        assert_eq!(hashed.unchanged_files, 0);
        let rows = read_jsonl_ids_with_hash(&case_dir);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|(_, hashed)| *hashed));

        let _ = fs::remove_dir_all(case_dir.parent().unwrap());
    }

    /// A file whose size changed is re-indexed; one that disappeared is
    /// marked stale; a brand-new file is indexed with a fresh id.
    #[test]
    fn incremental_rescan_reindexes_changed_and_marks_deleted_stale() {
        let (case_dir, source_dir) = incremental_fixture("change");
        fs::write(source_dir.join("keep.mp4"), b"\0\0\0\x18ftypmp42keep").unwrap();
        fs::write(source_dir.join("change.mp4"), b"\0\0\0\x18ftypmp42v1").unwrap();
        fs::write(source_dir.join("gone.mp4"), b"\0\0\0\x18ftypmp42gone").unwrap();
        let options = ScanOptions {
            hash_files: true,
            use_ffprobe: false,
            ..ScanOptions::default()
        };
        let first =
            super::scan_folder(&case_dir, &source_dir, &options, ResumeMode::Auto, None).unwrap();
        assert_eq!(first.video_count, 3);
        let index_before = fs::read_to_string(case_dir.join("db/videos.jsonl")).unwrap();
        let keep_line = index_before
            .lines()
            .find(|line| line.contains("keep.mp4"))
            .unwrap()
            .to_string();
        let change_sha = {
            let line = index_before
                .lines()
                .find(|line| line.contains("change.mp4"))
                .unwrap();
            serde_json::from_str::<serde_json::Value>(line).unwrap()["sha256"]
                .as_str()
                .unwrap()
                .to_string()
        };

        fs::write(
            source_dir.join("change.mp4"),
            b"\0\0\0\x18ftypmp42v2-longer",
        )
        .unwrap();
        fs::remove_file(source_dir.join("gone.mp4")).unwrap();
        fs::write(source_dir.join("new.mov"), b"\0\0\0\x18ftypmp42new").unwrap();

        let rescan = super::scan_folder(
            &case_dir,
            &source_dir,
            &ScanOptions {
                incremental: true,
                ..options
            },
            ResumeMode::Auto,
            None,
        )
        .unwrap();
        assert_eq!(rescan.video_count, 2, "changed + new files processed");
        assert_eq!(rescan.unchanged_files, 1, "keep.mp4 skipped");

        let index_after = fs::read_to_string(case_dir.join("db/videos.jsonl")).unwrap();
        let lines: Vec<&str> = index_after
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        assert_eq!(lines.len(), 4, "stale record stays in the index");

        let keep_after = lines.iter().find(|line| line.contains("keep.mp4")).unwrap();
        assert_eq!(*keep_after, keep_line, "unchanged record is verbatim");
        let gone = lines.iter().find(|line| line.contains("gone.mp4")).unwrap();
        assert!(gone.contains("\"index_status\":\"stale\""), "{gone}");
        assert!(gone.contains("\"stale_since_unix\""));
        let changed = lines
            .iter()
            .find(|line| line.contains("change.mp4"))
            .unwrap();
        let changed_sha = serde_json::from_str::<serde_json::Value>(changed).unwrap()["sha256"]
            .as_str()
            .unwrap()
            .to_string();
        assert_ne!(changed_sha, change_sha, "changed file re-hashed");
        assert!(!changed.contains("index_status"));
        assert!(lines.iter().any(|line| line.contains("new.mov")));

        let _ = fs::remove_dir_all(case_dir.parent().unwrap());
    }

    /// A record already flagged must not be skipped: reprocessing clears the
    /// marker by regenerating the record.
    #[test]
    fn incremental_rescan_refreshes_flagged_records() {
        let (case_dir, source_dir) = incremental_fixture("flagged");
        fs::write(source_dir.join("a.mp4"), b"\0\0\0\x18ftypmp42one").unwrap();
        let options = ScanOptions {
            use_ffprobe: false,
            ..ScanOptions::default()
        };
        super::scan_folder(&case_dir, &source_dir, &options, ResumeMode::Auto, None).unwrap();
        // Flag the record, then rescan unchanged: the flag forces a fresh
        // record, so the flag does not linger on live evidence.
        let index_path = case_dir.join("db/videos.jsonl");
        let flagged = set_json_field(
            &fs::read_to_string(&index_path).unwrap(),
            "index_status",
            "\"stale\"",
        );
        fs::write(&index_path, flagged).unwrap();
        let rescan = super::scan_folder(
            &case_dir,
            &source_dir,
            &ScanOptions {
                incremental: true,
                ..options
            },
            ResumeMode::Auto,
            None,
        )
        .unwrap();
        assert_eq!(rescan.video_count, 1);
        assert_eq!(rescan.unchanged_files, 0);
        let index_after = fs::read_to_string(&index_path).unwrap();
        assert!(!index_after.contains("index_status"), "{index_after}");
        let _ = fs::remove_dir_all(case_dir.parent().unwrap());
    }

    /// Writes the checkpoint a crashed run would have left: header +
    /// `record_lines` already-completed records.
    fn leave_checkpoint(
        case_dir: &Path,
        source_dir: &Path,
        options: &ScanOptions,
        record_lines: &[String],
    ) {
        // scan_folder fingerprints the CANONICALIZED source path, so the
        // crafted checkpoint must be computed the same way.
        let source_dir = crate::util::canonicalize_display(source_dir).unwrap();
        let candidates = collect_video_candidates(&source_dir, None, &[]).unwrap();
        let fingerprint = scan_fingerprint(&source_dir, options, &candidates.files);
        let mut checkpoint = RunCheckpoint::begin(
            &case_dir.join("db/scan-progress.jsonl"),
            "scan-folder",
            &fingerprint,
            ResumeMode::Auto,
        )
        .unwrap();
        for line in record_lines {
            checkpoint.append_line(line).unwrap();
        }
        // Drop without finish: the interrupted run's state stays on disk.
    }

    /// An interrupted run's checkpoint replays finished records verbatim —
    /// no re-hash, no re-probe — and the rerun still indexes everything.
    #[test]
    fn resume_replays_checkpointed_records() {
        let (case_dir, source_dir) = incremental_fixture("resume");
        fs::write(source_dir.join("a.mp4"), b"\0\0\0\x18ftypmp42one").unwrap();
        fs::write(source_dir.join("b.mp4"), b"\0\0\0\x18ftypmp42two").unwrap();
        fs::write(source_dir.join("c.mp4"), b"\0\0\0\x18ftypmp42three").unwrap();
        let options = ScanOptions {
            hash_files: true,
            use_ffprobe: false,
            ..ScanOptions::default()
        };
        let first =
            super::scan_folder(&case_dir, &source_dir, &options, ResumeMode::Auto, None).unwrap();
        assert_eq!(first.video_count, 3);
        let record_lines: Vec<String> = fs::read_to_string(case_dir.join("db/videos.jsonl"))
            .unwrap()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_string)
            .collect();
        assert_eq!(record_lines.len(), 3);

        // Simulate the crash: index outputs never written, checkpoint with
        // the first two processed files survives.
        for file in [
            "db/videos.jsonl",
            "db/video_index.json",
            "db/video_paths.tsv",
        ] {
            let _ = fs::remove_file(case_dir.join(file));
        }
        leave_checkpoint(&case_dir, &source_dir, &options, &record_lines[..2]);

        let second =
            super::scan_folder(&case_dir, &source_dir, &options, ResumeMode::Auto, None).unwrap();
        assert_eq!(second.resumed_from_checkpoint, 2);
        assert_eq!(second.video_count, 3);
        assert!(
            !case_dir.join("db/scan-progress.jsonl").exists(),
            "checkpoint deleted on full success"
        );
        let index = fs::read_to_string(case_dir.join("db/videos.jsonl")).unwrap();
        for name in ["a.mp4", "b.mp4", "c.mp4"] {
            assert!(index.contains(name), "index missing {name}");
        }
        // The replayed lines are byte-identical to what the crashed run
        // computed — resume does not rewrite prior work.
        for line in index.lines().filter(|line| line.contains(".mp4")) {
            if line.contains("a.mp4") {
                assert_eq!(line, record_lines[0]);
            } else if line.contains("b.mp4") {
                assert_eq!(line, record_lines[1]);
            }
        }
        // The resume is audit-logged.
        let scan_log = fs::read_to_string(case_dir.join("evidence/logs/scan-log.jsonl")).unwrap();
        assert!(scan_log.contains("\"event\":\"scan-resume\""), "{scan_log}");
        assert!(scan_log.contains("\"resumed_records\":2"), "{scan_log}");

        let _ = fs::remove_dir_all(case_dir.parent().unwrap());
    }

    /// A checkpoint whose input fingerprint does not match (e.g. recorded
    /// against different scan options) is discarded, not silently reused.
    #[test]
    fn resume_rejects_stale_fingerprint_and_scans_fresh() {
        let (case_dir, source_dir) = incremental_fixture("stalefp");
        fs::write(source_dir.join("a.mp4"), b"\0\0\0\x18ftypmp42one").unwrap();
        let options = ScanOptions {
            use_ffprobe: false,
            ..ScanOptions::default()
        };
        let checkpoint_path = case_dir.join("db/scan-progress.jsonl");
        {
            let mut checkpoint =
                RunCheckpoint::begin(&checkpoint_path, "scan-folder", "bogus", ResumeMode::Auto)
                    .unwrap();
            checkpoint.append_line("{\"id\":\"vid_000001\"}").unwrap();
        }

        let result =
            super::scan_folder(&case_dir, &source_dir, &options, ResumeMode::Auto, None).unwrap();
        assert_eq!(result.resumed_from_checkpoint, 0);
        assert_eq!(result.video_count, 1);
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("different inputs")),
            "expected a stale-checkpoint warning: {:?}",
            result.warnings
        );
        let _ = fs::remove_dir_all(case_dir.parent().unwrap());
    }

    /// A checkpoint file torn mid-append drops only the incomplete tail:
    /// complete records still replay.
    #[test]
    fn resume_tolerates_torn_checkpoint_tail() {
        let (case_dir, source_dir) = incremental_fixture("torntail");
        fs::write(source_dir.join("a.mp4"), b"\0\0\0\x18ftypmp42one").unwrap();
        fs::write(source_dir.join("b.mp4"), b"\0\0\0\x18ftypmp42two").unwrap();
        let options = ScanOptions {
            use_ffprobe: false,
            ..ScanOptions::default()
        };
        let first =
            super::scan_folder(&case_dir, &source_dir, &options, ResumeMode::Auto, None).unwrap();
        assert_eq!(first.video_count, 2);
        let record_lines: Vec<String> = fs::read_to_string(case_dir.join("db/videos.jsonl"))
            .unwrap()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_string)
            .collect();
        for file in [
            "db/videos.jsonl",
            "db/video_index.json",
            "db/video_paths.tsv",
        ] {
            let _ = fs::remove_file(case_dir.join(file));
        }
        leave_checkpoint(&case_dir, &source_dir, &options, &record_lines[..1]);
        // Torn tail: partial JSON appended without a newline.
        use std::io::Write;
        let mut handle = fs::OpenOptions::new()
            .append(true)
            .open(case_dir.join("db/scan-progress.jsonl"))
            .unwrap();
        handle.write_all(b"{\"id\":\"vid_").unwrap();
        drop(handle);

        let second =
            super::scan_folder(&case_dir, &source_dir, &options, ResumeMode::Auto, None).unwrap();
        assert_eq!(second.resumed_from_checkpoint, 1);
        assert_eq!(second.video_count, 2);
        let _ = fs::remove_dir_all(case_dir.parent().unwrap());
    }
}
