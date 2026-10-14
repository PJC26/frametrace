use super::*;

/// Fingerprint of everything that decides a scan record: source path, the
/// option set, and the collected candidate list. A rerun against different
/// inputs resets the checkpoint instead of silently skipping fresh work.
pub(super) fn scan_fingerprint(
    source_dir: &Path,
    options: &ScanOptions,
    files: &[PathBuf],
) -> String {
    let mut parts = vec![
        "scan-folder".to_string(),
        source_dir.to_string_lossy().to_string(),
        format!("hash_files={}", options.hash_files),
        format!("use_ffprobe={}", options.use_ffprobe),
        format!("max_depth={:?}", options.max_depth),
        format!("incremental={}", options.incremental),
    ];
    // Lossless per-file spelling: two non-Unicode names must not
    // fingerprint as one candidate list (UTF-8 names are unchanged).
    parts.extend(files.iter().map(|path| {
        crate::model::path_raw_hex(path).unwrap_or_else(|| path.to_string_lossy().to_string())
    }));
    checkpoint::fingerprint(&parts.iter().map(String::as_str).collect::<Vec<_>>())
}

/// Decodes a resumed checkpoint's record lines (verbatim `videos.jsonl`
/// records) into the replay map keyed by normalized source path. Later
/// lines win so a reprocessed file's newest record is the one replayed.
pub(super) fn load_checkpoint_records(
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
        records.insert(source_key(&record.source_path), record);
    }
    Ok(records)
}

pub(super) struct CandidateCollection {
    pub(super) files: Vec<PathBuf>,
    pub(super) warnings: Vec<String>,
}

pub(super) fn collect_video_candidates(
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
            if file_type.is_symlink() {
                // Never followed (a link out of the evidence tree would
                // index foreign files, and a junction loop never ends),
                // but never silently dropped either: Windows reports
                // directory junctions as symlinks, and an examiner whose
                // camera folder is one needs to know it was skipped.
                warnings.push(format!(
                    "skipped symbolic link / junction {} (links are not followed; scan its target directly)",
                    path.display()
                ));
                continue;
            }
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

/// `source_dir` is the scan's canonicalized source (`canonicalize_display`
/// in `scan_folder`). The case dir is canonicalized the SAME way: a plain
/// `canonicalize()` keeps the `\\?\` prefix on Windows, so the
/// `starts_with` below was always false there and a case nested inside
/// the source had its own artifacts re-indexed as evidence.
pub(super) fn excluded_case_dirs(
    case_dir: &Path,
    source_dir: &Path,
) -> Result<Vec<PathBuf>, String> {
    let case_dir = canonicalize_display(case_dir).unwrap_or_else(|_| case_dir.to_path_buf());
    // Tolerate a caller that canonicalized the source with the prefix on.
    let source_dir = crate::util::strip_windows_extended_prefix(source_dir);
    if case_dir == source_dir {
        return Err(format!(
            "source directory cannot be the FrameTrace case directory: {}",
            source_dir.display()
        ));
    }
    if case_dir.starts_with(&source_dir) {
        Ok(vec![case_dir])
    } else {
        Ok(Vec::new())
    }
}

pub(super) fn path_is_or_is_under_excluded_dir(path: &Path, excluded_dirs: &[PathBuf]) -> bool {
    excluded_dirs
        .iter()
        .any(|excluded| path == excluded || path.starts_with(excluded))
}

pub(super) fn looks_like_video(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|ext| ext.to_str()) else {
        return has_video_magic(path);
    };
    VIDEO_EXTENSIONS
        .iter()
        .any(|known| extension.eq_ignore_ascii_case(known))
        || has_video_magic(path)
}

pub(super) fn has_video_magic(path: &Path) -> bool {
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
pub(super) fn hash_with_restat(
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

pub(super) fn modified_unix(metadata: &fs::Metadata) -> Option<u64> {
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
pub(super) fn record_is_unchanged(
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

/// Decodes the data lines of a checkpoint discarded for a fingerprint
/// mismatch. Best effort: an undecodable line is skipped (it only costs a
/// re-hash), never an error.
pub(super) fn salvage_checkpoint_records(
    checkpoint: &RunCheckpoint,
) -> HashMap<String, VideoRecord> {
    checkpoint
        .stale_data_lines()
        .iter()
        .filter_map(|line| serde_json::from_str::<VideoRecord>(line).ok())
        .map(|record| (source_key(&record.source_path), record))
        .collect()
}

/// Same size and mtime as the file on disk — the oracle incremental scans
/// and checkpoint replay already use to trust a stored per-file record.
pub(super) fn same_file_state(
    record: &VideoRecord,
    size_bytes: u64,
    modified_unix: Option<u64>,
) -> bool {
    record.size_bytes == size_bytes && record.modified_unix == modified_unix
}

fn has_hash(record: &VideoRecord) -> bool {
    record.sha256.is_some()
}

/// A recorded hash attempt that failed (I11) — kept by passes that do not
/// hash, so the failure stays visible.
fn has_hash_failure(record: &VideoRecord) -> bool {
    record.sha256.is_none()
        && matches!(record.hash_status.as_str(), "error" | "changed-during-hash")
}

fn has_probe(record: &VideoRecord) -> bool {
    record.probe.error.as_deref() != Some("ffprobe skipped")
}

/// True when `donor` knows something `existing` does not (a hash or a
/// probe result), so skipping the file would drop that fact.
pub(super) fn adds_facts(donor: &VideoRecord, existing: &VideoRecord) -> bool {
    (has_hash(donor) && !has_hash(existing)) || (has_probe(donor) && !has_probe(existing))
}

/// Which per-file facts a scan reuses instead of computing.
pub(super) struct CarriedFacts<'a> {
    pub(super) hash: Option<&'a VideoRecord>,
    pub(super) probe: Option<&'a VideoRecord>,
}

/// Merge rule for per-file facts (Blocker: a lighter pass must never drop
/// recorded hashes/metadata). Both donors already match the file's current
/// size+mtime.
///
/// * `salvaged` (a discarded checkpoint of an interrupted run) always
///   donates: those facts were computed for this exact file state and
///   redoing them is the work resume exists to avoid.
/// * `indexed` (the current index record) donates a fact the pass does
///   not compute itself (no `--hash` / `--no-ffprobe`), and — in
///   incremental mode — any fact it already has. Only an explicit
///   non-incremental `--hash` / ffprobe pass recomputes what it was asked
///   to compute.
pub(super) fn carried_facts<'a>(
    salvaged: Option<&'a VideoRecord>,
    indexed: Option<&'a VideoRecord>,
    options: &ScanOptions,
) -> CarriedFacts<'a> {
    // I11: a pass that does not hash also keeps a recorded hash FAILURE
    // (`error` / `changed-during-hash`) for the unchanged file — that is a
    // fact about these bytes too, and a lighter rescan must not turn it into
    // `skipped`. A `--hash` pass retries the file instead.
    let hash = salvaged.filter(|r| has_hash(r)).or_else(|| {
        indexed
            .filter(|_| options.incremental || !options.hash_files)
            .filter(|r| has_hash(r) || (!options.hash_files && has_hash_failure(r)))
    });
    let probe = salvaged.filter(|r| has_probe(r)).or_else(|| {
        indexed
            .filter(|_| options.incremental || !options.use_ffprobe)
            .filter(|r| has_probe(r))
    });
    CarriedFacts { hash, probe }
}

pub(super) fn classify_confidence(extension: &str, probe: &ProbeSummary) -> String {
    if probe.ok {
        "ffprobe-confirmed".to_string()
    } else if VIDEO_EXTENSIONS.contains(&extension) {
        "extension-candidate".to_string()
    } else {
        "magic-candidate".to_string()
    }
}
